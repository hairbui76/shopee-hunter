//! Scenario 3: session expiry must pause claiming, not retry into a wall.
//!
//! The dangerous failure mode is a claim worker that keeps firing
//! account-mutating requests at an expired session — the fastest way to get an
//! account flagged. These tests assert the executor stops being called at all
//! once the session degrades.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use common::{claimable_candidate, temp_db};
use shopee_hunter_claimer::service::{
    ClaimExecutor, ClaimService, ClaimServiceConfig, ClaimServiceError,
};
use shopee_hunter_claimer::RetryStep;
use shopee_hunter_domain::claim::ClaimResultClass;
use shopee_hunter_domain::voucher::Voucher;
use shopee_hunter_domain::SessionState;
use shopee_hunter_session::SessionManager;
use shopee_hunter_storage::{ClaimRepository, Database, VoucherRepository};
use uuid::Uuid;

/// Executor that returns a scripted class and counts how many times the
/// account-mutating request was actually issued. The count is the assertion
/// that matters: it is the number of real requests a live Shopee would see.
struct CountingExecutor {
    class: ClaimResultClass,
    calls: Arc<AtomicUsize>,
}

impl CountingExecutor {
    fn new(class: ClaimResultClass) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                class,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

#[async_trait]
impl ClaimExecutor for CountingExecutor {
    async fn execute(&self, _voucher: &Voucher) -> Result<ClaimResultClass, ClaimServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.class)
    }
}

fn enabled_config() -> ClaimServiceConfig {
    ClaimServiceConfig {
        auto_claim_enabled: true,
        max_attempts: 5,
        ..ClaimServiceConfig::default()
    }
}

async fn seed_voucher(db: &Database, key: &str) -> Uuid {
    VoucherRepository::new(db)
        .upsert_candidate(&claimable_candidate("feed", key), Utc::now())
        .await
        .expect("seed voucher")
        .voucher_id()
}

#[tokio::test]
async fn an_expired_session_pauses_claiming_after_exactly_one_request() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_voucher(&db, "expiring").await;

    let session = SessionManager::new();
    session.observe(SessionState::Healthy, "probe ok", Utc::now());
    assert!(
        session.allows_claims(),
        "precondition: claiming is permitted"
    );

    let (executor, calls) = CountingExecutor::new(ClaimResultClass::SessionExpired);
    let service = ClaimService::new(db.clone(), session.clone(), executor, enabled_config());

    // First attempt: policy allows, the request goes out, upstream says the
    // session is gone.
    let first = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("first attempt runs");
    assert!(first.decision.is_allow());
    assert_eq!(first.result_class, Some(ClaimResultClass::SessionExpired));
    assert_eq!(
        first.retry_step,
        Some(RetryStep::PauseForSession),
        "an expired session must pause, never schedule a retry"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The service must have transitioned the session and closed the gate.
    assert_eq!(session.state(), SessionState::Expired);
    assert!(!session.allows_claims());
    assert!(!session.claim_gate().is_open());

    // Every subsequent attempt must be refused before any request is issued.
    for round in 0..5 {
        let outcome = service
            .attempt_claim(voucher_id, None, None, false)
            .await
            .expect("subsequent attempt evaluates");
        assert!(
            !outcome.decision.is_allow(),
            "round {round}: policy must refuse while the session is expired"
        );
        assert_eq!(outcome.result_class, None);
        assert_eq!(outcome.attempt_id, None);
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the claim path must not retry into a wall: exactly one request total"
    );

    // Only the one real attempt is in the audit trail.
    let attempts = ClaimRepository::new(&db)
        .attempts_for(voucher_id)
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert!(!ClaimRepository::new(&db)
        .has_successful_attempt(voucher_id)
        .await
        .expect("success check"));
}

#[tokio::test]
async fn a_verification_challenge_pauses_and_asks_for_a_human() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_voucher(&db, "challenged").await;

    let session = SessionManager::new();
    session.observe(SessionState::Healthy, "probe ok", Utc::now());

    let (executor, calls) = CountingExecutor::new(ClaimResultClass::VerificationRequired);
    let service = ClaimService::new(db.clone(), session.clone(), executor, enabled_config());

    let first = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("first attempt runs");
    assert_eq!(first.retry_step, Some(RetryStep::PauseForSession));
    assert_eq!(session.state(), SessionState::VerificationRequired);
    assert!(
        session.state().needs_manual_action(),
        "a challenge must escalate to the owner, never be worked around"
    );

    let second = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("second attempt evaluates");
    // Manual-action states surface as ManualReview rather than a silent defer.
    assert!(matches!(
        second.decision,
        shopee_hunter_domain::claim::ClaimDecision::ManualReview { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The gate must be shut even if policy somehow raced ahead of the state
/// change: the service re-checks immediately before mutating.
#[tokio::test]
async fn the_hard_gate_refuses_when_the_session_is_not_positively_healthy() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_voucher(&db, "ungated").await;

    for state in [
        SessionState::Unknown,
        SessionState::Degraded,
        SessionState::Expired,
        SessionState::LoginRequired,
        SessionState::VerificationRequired,
        SessionState::Disabled,
    ] {
        let session = SessionManager::new();
        session.observe(state, "injected", Utc::now());

        let (executor, calls) = CountingExecutor::new(ClaimResultClass::Success);
        let service = ClaimService::new(db.clone(), session, executor, enabled_config());

        let outcome = service
            .attempt_claim(voucher_id, None, None, false)
            .await
            .expect("attempt evaluates");
        assert!(
            !outcome.decision.is_allow(),
            "{state:?} must not permit an account-mutating request"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "{state:?} must issue zero requests"
        );
    }
}

/// Recovery path: once the owner re-authenticates, claiming resumes without a
/// restart.
#[tokio::test]
async fn claiming_resumes_after_the_session_recovers() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_voucher(&db, "recovering").await;

    let session = SessionManager::new();
    session.observe(SessionState::Expired, "expired", Utc::now());

    let (executor, calls) = CountingExecutor::new(ClaimResultClass::Success);
    let service = ClaimService::new(db.clone(), session.clone(), executor, enabled_config());

    let blocked = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("blocked attempt evaluates");
    assert!(!blocked.decision.is_allow());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Owner re-logs-in; the health probe observes a healthy session.
    session.observe(SessionState::Healthy, "re-authenticated", Utc::now());
    assert!(session.claim_gate().is_open());

    let allowed = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("attempt runs after recovery");
    assert!(allowed.decision.is_allow());
    assert_eq!(allowed.result_class, Some(ClaimResultClass::Success));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
