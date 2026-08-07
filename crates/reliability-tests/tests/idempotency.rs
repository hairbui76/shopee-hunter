//! Scenario 5: idempotency audit.
//!
//! Three places where a retry, a race, or a restart could duplicate work:
//! voucher ingestion, claim success, and notification delivery. Each must
//! converge on exactly one row.

mod common;

use std::sync::Arc;

use chrono::Utc;
use common::{candidate, claimable_candidate, temp_db};
use shopee_hunter_claimer::service::{
    ClaimExecutor, ClaimService, ClaimServiceConfig, ClaimServiceError,
};
use shopee_hunter_domain::claim::ClaimResultClass;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::voucher::Voucher;
use shopee_hunter_domain::{SessionState, SourceId};
use shopee_hunter_session::SessionManager;
use shopee_hunter_storage::{
    ClaimRepository, Database, OutboxRepository, OutboxStatus, UpsertOutcome, VoucherRepository,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Voucher upsert
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_ingestion_of_one_voucher_yields_exactly_one_row() {
    let (db, _dir) = temp_db().await;
    let shared = Arc::new(db.clone());

    // Eight collectors racing on the same logical voucher — the situation a
    // multi-source discovery cycle creates naturally.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&shared);
        handles.push(tokio::spawn(async move {
            VoucherRepository::new(&db)
                .upsert_candidate(&candidate("feed", "contended"), Utc::now())
                .await
        }));
    }

    let mut ids = Vec::new();
    let mut created = 0;
    for handle in handles {
        let outcome = handle.await.expect("task").expect("upsert");
        if matches!(outcome, UpsertOutcome::Created { .. }) {
            created += 1;
        }
        ids.push(outcome.voucher_id());
    }

    assert_eq!(
        VoucherRepository::new(&db).count().await.expect("count"),
        1,
        "concurrent ingestion must converge on one logical voucher"
    );
    assert_eq!(created, 1, "exactly one racer may report Created");
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "every racer must observe the same voucher id: {ids:?}"
    );
}

#[tokio::test]
async fn re_ingesting_the_same_voucher_is_a_no_op() {
    let (db, _dir) = temp_db().await;
    let repo = VoucherRepository::new(&db);

    let first = repo
        .upsert_candidate(&candidate("feed", "stable"), Utc::now())
        .await
        .expect("first ingest");
    assert!(matches!(first, UpsertOutcome::Created { .. }));

    for _ in 0..10 {
        let again = repo
            .upsert_candidate(&candidate("feed", "stable"), Utc::now())
            .await
            .expect("re-ingest");
        assert!(
            matches!(again, UpsertOutcome::Unchanged { .. }),
            "a repeat observation is not a change and must not emit an event"
        );
        assert_eq!(again.voucher_id(), first.voucher_id());
    }
    assert_eq!(repo.count().await.expect("count"), 1);
}

// ---------------------------------------------------------------------------
// Claim success
// ---------------------------------------------------------------------------

struct AlwaysSucceeds {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl ClaimExecutor for AlwaysSucceeds {
    async fn execute(&self, _voucher: &Voucher) -> Result<ClaimResultClass, ClaimServiceError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ClaimResultClass::Success)
    }
}

async fn seed_claimable(db: &Database, key: &str) -> Uuid {
    VoucherRepository::new(db)
        .upsert_candidate(&claimable_candidate("feed", key), Utc::now())
        .await
        .expect("seed voucher")
        .voucher_id()
}

#[tokio::test]
async fn a_successful_claim_is_never_repeated() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_claimable(&db, "claimed-once").await;

    let session = SessionManager::new();
    session.observe(SessionState::Healthy, "probe ok", Utc::now());

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = ClaimService::new(
        db.clone(),
        session,
        AlwaysSucceeds {
            calls: Arc::clone(&calls),
        },
        ClaimServiceConfig {
            auto_claim_enabled: true,
            max_attempts: 5,
            ..ClaimServiceConfig::default()
        },
    );

    let first = service
        .attempt_claim(voucher_id, None, None, false)
        .await
        .expect("first claim");
    assert_eq!(first.result_class, Some(ClaimResultClass::Success));

    let claims = ClaimRepository::new(&db);
    assert!(claims
        .has_successful_attempt(voucher_id)
        .await
        .expect("success recorded"));

    // A duplicate scheduler tick, a retry, or a restart replaying the job must
    // all be refused before another account-mutating request is issued.
    for round in 0..4 {
        let again = service
            .attempt_claim(voucher_id, None, None, false)
            .await
            .expect("repeat claim evaluates");
        assert!(
            !again.decision.is_allow(),
            "round {round}: an already-claimed voucher must not be claimed again"
        );
        assert_eq!(again.attempt_id, None);
    }

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one account-mutating request may ever be sent for a claimed voucher"
    );
    assert_eq!(
        claims
            .attempts_for(voucher_id)
            .await
            .expect("attempts")
            .len(),
        1,
        "the audit trail records one attempt, not a storm"
    );
}

#[tokio::test]
async fn the_attempt_trail_is_append_only_and_bounded_by_the_retry_budget() {
    let (db, _dir) = temp_db().await;
    let voucher_id = seed_claimable(&db, "budgeted").await;

    struct AlwaysTransient;
    #[async_trait::async_trait]
    impl ClaimExecutor for AlwaysTransient {
        async fn execute(&self, _v: &Voucher) -> Result<ClaimResultClass, ClaimServiceError> {
            Ok(ClaimResultClass::TransientFailure)
        }
    }

    let session = SessionManager::new();
    session.observe(SessionState::Healthy, "probe ok", Utc::now());
    let service = ClaimService::new(
        db.clone(),
        session,
        AlwaysTransient,
        ClaimServiceConfig {
            auto_claim_enabled: true,
            max_attempts: 3,
            ..ClaimServiceConfig::default()
        },
    );

    // Drive well past the budget: the guard must be the budget, not the loop.
    for _ in 0..10 {
        service
            .attempt_claim(voucher_id, None, None, false)
            .await
            .expect("attempt evaluates");
    }

    let attempts = ClaimRepository::new(&db)
        .attempts_for(voucher_id)
        .await
        .expect("attempts");
    assert_eq!(
        attempts.len(),
        3,
        "the retry budget must cap real requests at max_attempts, got {}",
        attempts.len()
    );
    assert!(!ClaimRepository::new(&db)
        .has_successful_attempt(voucher_id)
        .await
        .expect("no success"));
}

// ---------------------------------------------------------------------------
// Notification outbox
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enqueueing_the_same_event_twice_delivers_it_once() {
    let (db, _dir) = temp_db().await;
    let outbox = OutboxRepository::new(&db);
    let now = Utc::now();

    let event = DomainEvent::VoucherDiscovered {
        voucher_id: Uuid::new_v4(),
        source: SourceId::new("feed"),
        version_hash: "hash-1".into(),
    };

    let first = outbox.enqueue(&event, now).await.expect("first enqueue");
    assert!(first.is_some(), "the first enqueue creates a row");

    for round in 0..5 {
        let again = outbox.enqueue(&event, now).await.expect("repeat enqueue");
        assert!(
            again.is_none(),
            "round {round}: a repeated occurrence must not create a second notification"
        );
    }

    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Pending)
            .await
            .expect("count"),
        1
    );
}

#[tokio::test]
async fn distinct_occurrences_are_kept_apart() {
    let (db, _dir) = temp_db().await;
    let outbox = OutboxRepository::new(&db);
    let now = Utc::now();
    let voucher_id = Uuid::new_v4();

    // Same voucher, different version: a genuinely new thing to report.
    let discovered = DomainEvent::VoucherDiscovered {
        voucher_id,
        source: SourceId::new("feed"),
        version_hash: "v1".into(),
    };
    let updated = DomainEvent::VoucherUpdated {
        voucher_id,
        source: SourceId::new("feed"),
        version_hash: "v2".into(),
        changed_fields: vec!["min_spend".into()],
    };

    assert!(outbox
        .enqueue(&discovered, now)
        .await
        .expect("v1")
        .is_some());
    assert!(outbox.enqueue(&updated, now).await.expect("v2").is_some());
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Pending)
            .await
            .expect("count"),
        2,
        "idempotency must deduplicate repeats without collapsing distinct events"
    );
}

#[tokio::test]
async fn a_claim_success_event_is_idempotent_on_its_attempt() {
    let (db, _dir) = temp_db().await;
    let outbox = OutboxRepository::new(&db);
    let now = Utc::now();

    let event = DomainEvent::ClaimSucceeded {
        voucher_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        already_saved: false,
    };

    assert!(outbox.enqueue(&event, now).await.expect("first").is_some());
    // A crash between "claim succeeded" and "notification sent" replays the
    // enqueue on restart; the owner must not get two success messages.
    assert!(outbox.enqueue(&event, now).await.expect("replay").is_none());
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Pending)
            .await
            .expect("count"),
        1
    );
}
