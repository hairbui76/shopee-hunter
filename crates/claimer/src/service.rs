//! Claim service: orchestrates the controlled voucher-save flow — load state,
//! evaluate policy, verify session, create a durable attempt, execute, classify,
//! persist, decide retry/terminal, and emit a notification event.

use async_trait::async_trait;
use chrono::Utc;
use shopee_hunter_domain::claim::{ClaimDecision, ClaimResultClass};
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::voucher::{Voucher, VoucherStatus};
use shopee_hunter_domain::SessionState;
use shopee_hunter_session::SessionManager;
use shopee_hunter_storage::{ClaimRepository, Database, StorageError, VoucherRepository};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

use crate::policy::{evaluate, PolicyInputs};
use crate::retry::{next_step, RetryStep};

#[derive(Debug, Error)]
pub enum ClaimServiceError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("voucher not found: {0}")]
    VoucherNotFound(Uuid),
    #[error("execution error: {0}")]
    Execution(String),
}

#[derive(Debug, Clone)]
pub struct ClaimServiceConfig {
    pub auto_claim_enabled: bool,
    pub max_attempts: u32,
    pub retry_base_delay: Duration,
    pub diagnostic_budget: u32,
    pub min_score: i64,
}

impl Default for ClaimServiceConfig {
    fn default() -> Self {
        Self {
            auto_claim_enabled: false,
            max_attempts: 3,
            retry_base_delay: Duration::from_millis(500),
            diagnostic_budget: 2,
            min_score: 0,
        }
    }
}

/// Performs the account-mutating request. Abstracted so the service is testable
/// without a live Shopee client and so the transport can be swapped.
#[async_trait]
pub trait ClaimExecutor: Send + Sync {
    async fn execute(&self, voucher: &Voucher) -> Result<ClaimResultClass, ClaimServiceError>;
}

/// Result of one claim service invocation.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub decision: ClaimDecision,
    pub result_class: Option<ClaimResultClass>,
    pub retry_step: Option<RetryStep>,
    pub event: Option<DomainEvent>,
    pub attempt_id: Option<Uuid>,
}

pub struct ClaimService<E: ClaimExecutor> {
    db: Database,
    session: SessionManager,
    executor: E,
    config: ClaimServiceConfig,
    trusted_source: bool,
}

impl<E: ClaimExecutor> ClaimService<E> {
    pub fn new(
        db: Database,
        session: SessionManager,
        executor: E,
        config: ClaimServiceConfig,
    ) -> Self {
        Self {
            db,
            session,
            executor,
            config,
            trusted_source: true,
        }
    }

    pub fn with_trusted_source(mut self, trusted: bool) -> Self {
        self.trusted_source = trusted;
        self
    }

    /// Attempt to claim a voucher under policy. Sends a request only when
    /// policy returns Allow AND the session gate is open.
    pub async fn attempt_claim(
        &self,
        voucher_id: Uuid,
        schedule_job_id: Option<Uuid>,
        score: Option<i64>,
        excluded: bool,
    ) -> Result<ExecOutcome, ClaimServiceError> {
        let now = Utc::now();
        let voucher = VoucherRepository::new(&self.db)
            .get(voucher_id)
            .await?
            .ok_or(ClaimServiceError::VoucherNotFound(voucher_id))?;

        let claims = ClaimRepository::new(&self.db);
        let already_succeeded = claims.has_successful_attempt(voucher_id).await?;
        let attempts_used = claims.attempts_for(voucher_id).await?.len() as u32;

        let session_state = self.session.state();
        let decision = evaluate(&PolicyInputs {
            voucher: &voucher,
            session_state,
            now,
            already_succeeded,
            attempts_used,
            max_attempts: self.config.max_attempts,
            auto_claim_enabled: self.config.auto_claim_enabled,
            min_score: self.config.min_score,
            score,
            excluded,
            trusted_source: self.trusted_source,
        });

        if !decision.is_allow() {
            return Ok(ExecOutcome {
                decision,
                result_class: None,
                retry_step: None,
                event: None,
                attempt_id: None,
            });
        }

        // Hard gate: refuse to mutate if the session is not positively healthy,
        // even if policy raced ahead of a state change.
        if !self.session.allows_claims() {
            return Ok(ExecOutcome {
                decision: ClaimDecision::Defer {
                    reasons: vec!["session gate closed at execution time".into()],
                    until_hint: None,
                },
                result_class: None,
                retry_step: Some(RetryStep::PauseForSession),
                event: None,
                attempt_id: None,
            });
        }

        // Durable attempt intent before sending.
        let attempt_id = claims
            .begin_attempt(voucher_id, schedule_job_id, attempts_used as i64, now)
            .await?;

        let started = std::time::Instant::now();
        let result_class = match self.executor.execute(&voucher).await {
            Ok(class) => class,
            Err(ClaimServiceError::Execution(_)) => ClaimResultClass::TransientFailure,
            Err(e) => return Err(e),
        };
        let latency_ms = started.elapsed().as_millis() as i64;

        claims
            .complete_attempt(
                attempt_id,
                result_class,
                None,
                Some(latency_ms),
                None,
                None,
                Utc::now(),
            )
            .await?;

        let step = next_step(
            result_class,
            attempts_used,
            self.config.max_attempts,
            self.config.retry_base_delay,
            self.config.diagnostic_budget,
        );

        self.apply_status(&voucher, result_class, &step).await?;

        // Session-affecting results pause the claim worker via the manager.
        if matches!(step, RetryStep::PauseForSession) {
            let new_state = match result_class {
                ClaimResultClass::VerificationRequired => SessionState::VerificationRequired,
                _ => SessionState::Expired,
            };
            self.session.observe(
                new_state,
                "claim response indicated session problem",
                Utc::now(),
            );
        }

        let event = self.build_event(voucher_id, attempt_id, result_class, &step);

        Ok(ExecOutcome {
            decision,
            result_class: Some(result_class),
            retry_step: Some(step),
            event: Some(event),
            attempt_id: Some(attempt_id),
        })
    }

    async fn apply_status(
        &self,
        voucher: &Voucher,
        class: ClaimResultClass,
        step: &RetryStep,
    ) -> Result<(), StorageError> {
        let new_status = if class.is_success_equivalent() {
            Some(VoucherStatus::Saved)
        } else if class == ClaimResultClass::Exhausted {
            Some(VoucherStatus::Exhausted)
        } else if class == ClaimResultClass::Expired {
            Some(VoucherStatus::Expired)
        } else if class == ClaimResultClass::Ineligible {
            Some(VoucherStatus::Ineligible)
        } else if matches!(step, RetryStep::ReviewRequired) {
            Some(VoucherStatus::ReviewRequired)
        } else {
            None
        };
        if let Some(status) = new_status {
            VoucherRepository::new(&self.db)
                .set_status(voucher.id, status)
                .await?;
        }
        Ok(())
    }

    fn build_event(
        &self,
        voucher_id: Uuid,
        attempt_id: Uuid,
        class: ClaimResultClass,
        step: &RetryStep,
    ) -> DomainEvent {
        if class.is_success_equivalent() {
            DomainEvent::ClaimSucceeded {
                voucher_id,
                attempt_id,
                already_saved: class == ClaimResultClass::AlreadySaved,
            }
        } else {
            DomainEvent::ClaimFailed {
                voucher_id,
                attempt_id,
                result_class: class,
                terminal: matches!(step, RetryStep::Terminal | RetryStep::ReviewRequired),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
    use shopee_hunter_domain::SourceId;

    struct ScriptedExecutor(ClaimResultClass);
    #[async_trait]
    impl ClaimExecutor for ScriptedExecutor {
        async fn execute(&self, _v: &Voucher) -> Result<ClaimResultClass, ClaimServiceError> {
            Ok(self.0)
        }
    }

    async fn temp_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("cl.db").display());
        (Database::connect(&url, 4).await.unwrap(), dir)
    }

    fn candidate() -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k".into(),
            external_id: Some("x".into()),
            code: Some("CODE".into()),
            promotion_id: None,
            signature: None,
            title: "V".into(),
            description: None,
            voucher_type: VoucherType::Platform,
            discount_type: None,
            discount_amount: None,
            discount_percent: None,
            max_discount: None,
            min_spend: None,
            start_at: None,
            end_at: None,
            scope: None,
            payment_method: None,
            landing_url: None,
            raw_payload: serde_json::Value::Null,
            observed_at: Utc::now(),
            parser_version: "t".into(),
        }
    }

    async fn setup(
        class: ClaimResultClass,
        auto: bool,
    ) -> (Database, SessionManager, Uuid, ScriptedExecutor) {
        let (db, dir) = temp_db().await;
        std::mem::forget(dir); // keep temp dir alive for the test process
        let vid = VoucherRepository::new(&db)
            .upsert_candidate(&candidate(), Utc::now())
            .await
            .unwrap()
            .voucher_id();
        let session = SessionManager::new();
        if auto {
            session.observe(SessionState::Healthy, "ok", Utc::now());
        }
        (db, session, vid, ScriptedExecutor(class))
    }

    fn config(auto: bool) -> ClaimServiceConfig {
        ClaimServiceConfig {
            auto_claim_enabled: auto,
            ..ClaimServiceConfig::default()
        }
    }

    #[tokio::test]
    async fn successful_claim_marks_saved_and_emits_event() {
        let (db, session, vid, exec) = setup(ClaimResultClass::Success, true).await;
        let svc = ClaimService::new(db.clone(), session, exec, config(true));
        let out = svc.attempt_claim(vid, None, Some(50), false).await.unwrap();

        assert!(out.decision.is_allow());
        assert_eq!(out.result_class, Some(ClaimResultClass::Success));
        assert_eq!(out.retry_step, Some(RetryStep::Terminal));
        assert!(matches!(
            out.event,
            Some(DomainEvent::ClaimSucceeded { .. })
        ));
        let v = VoucherRepository::new(&db).get(vid).await.unwrap().unwrap();
        assert_eq!(v.status, VoucherStatus::Saved);
        // Idempotency: a second attempt is denied (already succeeded).
        let again = svc.attempt_claim(vid, None, Some(50), false).await.unwrap();
        assert!(!again.decision.is_allow());
    }

    #[tokio::test]
    async fn disabled_auto_claim_never_sends_request() {
        let (db, session, vid, exec) = setup(ClaimResultClass::Success, true).await;
        let svc = ClaimService::new(db.clone(), session, exec, config(false));
        let out = svc.attempt_claim(vid, None, None, false).await.unwrap();
        assert!(!out.decision.is_allow());
        assert!(out.attempt_id.is_none());
        assert!(ClaimRepository::new(&db)
            .attempts_for(vid)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn unhealthy_session_blocks_execution() {
        let (db, session, vid, exec) = setup(ClaimResultClass::Success, false).await;
        // session left Unknown (not healthy)
        let svc = ClaimService::new(db, session, exec, config(true));
        let out = svc.attempt_claim(vid, None, None, false).await.unwrap();
        assert!(matches!(out.decision, ClaimDecision::Defer { .. }));
        assert!(out.result_class.is_none());
    }

    #[tokio::test]
    async fn session_expired_response_pauses_claims() {
        let (db, session, vid, exec) = setup(ClaimResultClass::SessionExpired, true).await;
        let svc = ClaimService::new(db, session.clone(), exec, config(true));
        let out = svc.attempt_claim(vid, None, None, false).await.unwrap();
        assert_eq!(out.retry_step, Some(RetryStep::PauseForSession));
        // Session manager transitioned to a claim-blocking state.
        assert!(!session.allows_claims());
    }

    #[tokio::test]
    async fn already_saved_is_success_equivalent() {
        let (db, session, vid, exec) = setup(ClaimResultClass::AlreadySaved, true).await;
        let svc = ClaimService::new(db.clone(), session, exec, config(true));
        let out = svc.attempt_claim(vid, None, None, false).await.unwrap();
        assert!(matches!(
            out.event,
            Some(DomainEvent::ClaimSucceeded {
                already_saved: true,
                ..
            })
        ));
        let v = VoucherRepository::new(&db).get(vid).await.unwrap().unwrap();
        assert_eq!(v.status, VoucherStatus::Saved);
    }
}
