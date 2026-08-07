//! Integration tests for the storage layer against a temporary SQLite DB.
//! (PostgreSQL is validated in CI via the same portable SQL + Any driver.)

use chrono::{Duration, TimeZone, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::claim::ClaimResultClass;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::schedule::{JobStatus, ScheduleAction};
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherScope, VoucherType};
use shopee_hunter_domain::SourceId;
use shopee_hunter_storage::{
    ClaimRepository, Database, OutboxRepository, OutboxStatus, ScheduleRepository, UpsertOutcome,
    VoucherRepository,
};

async fn temp_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url, 4).await.expect("connect");
    (db, dir)
}

fn candidate(code: &str) -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new("feed"),
        source_key: format!("key-{code}"),
        external_id: None,
        code: Some(code.into()),
        promotion_id: Some("promo-1".into()),
        signature: Some("sig-1".into()),
        title: "Freeship 50k".into(),
        description: Some("desc".into()),
        voucher_type: VoucherType::Freeship,
        discount_type: None,
        discount_amount: Some(Decimal::new(50_000, 0)),
        discount_percent: None,
        max_discount: None,
        min_spend: Some(Decimal::new(0, 0)),
        start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
        end_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 17, 0, 0).unwrap()),
        scope: Some(VoucherScope::Platform),
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::json!({"code": code}),
        observed_at: Utc::now(),
        parser_version: "test-1".into(),
    }
}

#[tokio::test]
async fn upsert_dedupes_and_versions() {
    let (db, _dir) = temp_db().await;
    let repo = VoucherRepository::new(&db);
    let now = Utc::now();

    let first = repo
        .upsert_candidate(&candidate("SALE"), now)
        .await
        .unwrap();
    assert!(matches!(first, UpsertOutcome::Created { .. }));

    // Same logical voucher (promotion_id identity) → unchanged.
    let again = repo
        .upsert_candidate(&candidate("SALE"), now + Duration::seconds(1))
        .await
        .unwrap();
    assert!(matches!(again, UpsertOutcome::Unchanged { .. }));
    assert_eq!(again.voucher_id(), first.voucher_id());
    assert_eq!(repo.count().await.unwrap(), 1);

    // Meaningful change (min_spend) → updated with changed field.
    let mut changed = candidate("SALE");
    changed.min_spend = Some(Decimal::new(100_000, 0));
    let upd = repo
        .upsert_candidate(&changed, now + Duration::seconds(2))
        .await
        .unwrap();
    match upd {
        UpsertOutcome::Updated { changed_fields, .. } => {
            assert!(changed_fields.contains(&"min_spend".to_string()));
        }
        other => panic!("expected Updated, got {other:?}"),
    }
    assert_eq!(repo.count().await.unwrap(), 1);

    // Round-trips.
    let v = repo.get(first.voucher_id()).await.unwrap().unwrap();
    assert_eq!(v.min_spend, Some(Decimal::new(100_000, 0)));
    assert_eq!(v.scope, Some(VoucherScope::Platform));
    assert!(v.has_claim_identifiers());
}

#[tokio::test]
async fn state_survives_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("persist.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let vid;
    {
        let db = Database::connect(&url, 2).await.unwrap();
        let repo = VoucherRepository::new(&db);
        vid = repo
            .upsert_candidate(&candidate("KEEP"), Utc::now())
            .await
            .unwrap()
            .voucher_id();
        db.close().await;
    }
    // Reconnect: data must still be present (restart safety).
    let db = Database::connect(&url, 2).await.unwrap();
    let repo = VoucherRepository::new(&db);
    assert!(repo.get(vid).await.unwrap().is_some());
    assert_eq!(repo.count().await.unwrap(), 1);
}

#[tokio::test]
async fn schedule_prevents_duplicates_and_claims_once() {
    let (db, _dir) = temp_db().await;
    let vid = VoucherRepository::new(&db)
        .upsert_candidate(&candidate("SCHED"), Utc::now())
        .await
        .unwrap()
        .voucher_id();
    let sched = ScheduleRepository::new(&db);
    let now = Utc::now();
    let exec = now + Duration::hours(1);
    let pre = now + Duration::minutes(50);

    let job1 = sched
        .upsert(vid, ScheduleAction::ClaimVoucher, exec, pre, now)
        .await
        .unwrap();
    // Re-scheduling the same (voucher, action) reuses the row.
    let job2 = sched
        .upsert(vid, ScheduleAction::ClaimVoucher, exec, pre, now)
        .await
        .unwrap();
    assert_eq!(job1, job2);
    assert_eq!(sched.open_jobs().await.unwrap().len(), 1);

    // Only one caller can claim the job into RUNNING.
    assert!(sched.try_claim_running(job1, now).await.unwrap());
    assert!(!sched.try_claim_running(job1, now).await.unwrap());

    sched
        .set_status(job1, JobStatus::Succeeded, Some("SUCCESS"), now)
        .await
        .unwrap();
    assert!(sched.open_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn claim_idempotency_and_audit() {
    let (db, _dir) = temp_db().await;
    let vid = VoucherRepository::new(&db)
        .upsert_candidate(&candidate("CLAIM"), Utc::now())
        .await
        .unwrap()
        .voucher_id();
    let claims = ClaimRepository::new(&db);
    let now = Utc::now();

    assert!(!claims.has_successful_attempt(vid).await.unwrap());
    let attempt = claims.begin_attempt(vid, None, 0, now).await.unwrap();
    claims
        .complete_attempt(
            attempt,
            ClaimResultClass::Success,
            Some(200),
            Some(42),
            Some("v1"),
            None,
            now + Duration::milliseconds(42),
        )
        .await
        .unwrap();
    assert!(claims.has_successful_attempt(vid).await.unwrap());

    let history = claims.attempts_for(vid).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].result_class, Some(ClaimResultClass::Success));
    assert_eq!(history[0].latency_ms, Some(42));
}

#[tokio::test]
async fn outbox_is_idempotent_and_drains() {
    let (db, _dir) = temp_db().await;
    let vid = VoucherRepository::new(&db)
        .upsert_candidate(&candidate("OUT"), Utc::now())
        .await
        .unwrap()
        .voucher_id();
    let outbox = OutboxRepository::new(&db);
    let now = Utc::now();
    let event = DomainEvent::VoucherDiscovered {
        voucher_id: vid,
        source: SourceId::new("feed"),
        version_hash: "v1".into(),
    };

    let first = outbox.enqueue(&event, now).await.unwrap();
    assert!(first.is_some());
    // Same idempotency key → no duplicate.
    let dup = outbox.enqueue(&event, now).await.unwrap();
    assert!(dup.is_none());

    let ready = outbox.fetch_ready(now, 10).await.unwrap();
    assert_eq!(ready.len(), 1);
    outbox.mark_delivered(ready[0].id, now).await.unwrap();
    assert!(outbox.fetch_ready(now, 10).await.unwrap().is_empty());
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Delivered)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn outbox_dead_letters_after_max_attempts() {
    let (db, _dir) = temp_db().await;
    let vid = VoucherRepository::new(&db)
        .upsert_candidate(&candidate("DL"), Utc::now())
        .await
        .unwrap()
        .voucher_id();
    let outbox = OutboxRepository::new(&db);
    let now = Utc::now();
    let event = DomainEvent::VoucherUpcoming {
        voucher_id: vid,
        starts_at: now + Duration::hours(1),
    };
    let id = outbox.enqueue(&event, now).await.unwrap().unwrap();

    for _ in 0..3 {
        outbox
            .mark_failed(id, "telegram down", now, 3, now)
            .await
            .unwrap();
    }
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::DeadLettered)
            .await
            .unwrap(),
        1
    );
}
