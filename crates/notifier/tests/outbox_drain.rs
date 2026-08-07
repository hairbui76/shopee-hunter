//! Outbox worker integration tests against a temporary SQLite database.
//!
//! They exercise the Phase 16 guarantees end to end: durable events are
//! delivered exactly once, failures are retried on a schedule and eventually
//! dead-lettered, and voucher messages are enriched from storage.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::ids::SourceId;
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherScope, VoucherType};
use shopee_hunter_domain::SessionState;
use shopee_hunter_notifier::{OutboxNotifierWorker, OutboxWorkerConfig, StubNotifier};
use shopee_hunter_storage::{Database, OutboxRepository, OutboxStatus, VoucherRepository};
use uuid::Uuid;

async fn temp_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notifier.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url, 4).await.expect("connect");
    (db, dir)
}

fn config(chat: &str) -> OutboxWorkerConfig {
    OutboxWorkerConfig {
        chat_id: chat.to_string(),
        batch_size: 10,
        max_attempts: 2,
        base_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(10),
        enrich_from_storage: true,
        poll_interval: Duration::from_millis(10),
    }
}

fn worker(
    db: &Database,
    notifier: &StubNotifier,
    config: OutboxWorkerConfig,
) -> OutboxNotifierWorker<StubNotifier> {
    OutboxNotifierWorker::new(db.clone(), Arc::new(notifier.clone()), config).expect("valid config")
}

fn candidate() -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new("feed"),
        source_key: "k1".into(),
        external_id: Some("ext-1".into()),
        code: Some("FREESHIP50".into()),
        promotion_id: Some("promo-1".into()),
        signature: Some("super-secret-signature".into()),
        title: "Freeship 50k toan san".into(),
        description: None,
        voucher_type: VoucherType::Freeship,
        discount_type: None,
        discount_amount: Some(Decimal::new(50_000, 0)),
        discount_percent: None,
        max_discount: None,
        min_spend: Some(Decimal::new(200_000, 0)),
        start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
        end_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 17, 0, 0).unwrap()),
        scope: Some(VoucherScope::Platform),
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::json!({"code": "FREESHIP50"}),
        observed_at: Utc::now(),
        parser_version: "test-1".into(),
    }
}

#[tokio::test]
async fn drains_pending_events_and_marks_them_delivered() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let outbox = OutboxRepository::new(&db);

    outbox
        .enqueue(
            &DomainEvent::ServiceUnhealthy {
                service: "collector".into(),
                detail: "loop stalled".into(),
            },
            now,
        )
        .await
        .expect("enqueue");
    outbox
        .enqueue(
            &DomainEvent::SessionStateChanged {
                from: SessionState::Healthy,
                to: SessionState::Expired,
                reason: "cookie rejected".into(),
            },
            now,
        )
        .await
        .expect("enqueue");

    let stub = StubNotifier::new();
    let worker = worker(&db, &stub, config("chat-1"));

    let stats = worker.drain_once(now).await.expect("drain");
    assert_eq!(stats.fetched, 2);
    assert_eq!(stats.delivered, 2);
    assert_eq!(stats.dead_lettered, 0);

    assert_eq!(stub.delivered(), 2);
    assert!(stub.messages().iter().all(|m| m.chat_id == "chat-1"));
    assert!(stub.messages()[0].text.starts_with("SERVICE UNHEALTHY"));
    assert!(stub.messages()[1].text.starts_with("SESSION EXPIRED"));

    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Delivered)
            .await
            .expect("count"),
        2
    );
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Pending)
            .await
            .expect("count"),
        0
    );

    // Delivered rows are never re-fetched: the second drain is a no-op.
    let second = worker.drain_once(now).await.expect("drain");
    assert!(second.is_idle());
    assert_eq!(stub.delivered(), 2);
}

#[tokio::test]
async fn duplicate_events_produce_a_single_message() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let outbox = OutboxRepository::new(&db);
    let event = DomainEvent::CollectorDegraded {
        source: SourceId::new("feed"),
        detail: "parse failures".into(),
    };

    let first = outbox.enqueue(&event, now).await.expect("enqueue");
    let second = outbox.enqueue(&event, now).await.expect("enqueue");
    assert!(first.is_some(), "first enqueue creates the row");
    assert!(second.is_none(), "same idempotency key is not duplicated");

    let stub = StubNotifier::new();
    let stats = worker(&db, &stub, config("chat-1"))
        .drain_once(now)
        .await
        .expect("drain");

    assert_eq!(stats.delivered, 1);
    assert_eq!(stub.delivered(), 1);
}

#[tokio::test]
async fn failed_delivery_is_rescheduled_then_dead_lettered() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let outbox = OutboxRepository::new(&db);
    outbox
        .enqueue(
            &DomainEvent::ServiceUnhealthy {
                service: "claimer".into(),
                detail: "down".into(),
            },
            now,
        )
        .await
        .expect("enqueue");

    let stub = StubNotifier::always_failing();
    // max_attempts = 2, base backoff = 1s.
    let worker = worker(&db, &stub, config("chat-1"));

    let first = worker.drain_once(now).await.expect("drain");
    assert_eq!(first.retry_scheduled, 1);
    assert_eq!(first.dead_lettered, 0);
    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::Pending)
            .await
            .expect("count"),
        1
    );

    // Still inside the backoff window: nothing is fetched, so a failing
    // notifier cannot be hammered.
    let during_backoff = worker.drain_once(now).await.expect("drain");
    assert!(during_backoff.is_idle());
    assert_eq!(stub.attempts(), 1);

    let after_backoff = worker
        .drain_once(now + chrono::Duration::seconds(5))
        .await
        .expect("drain");
    assert_eq!(after_backoff.dead_lettered, 1);
    assert_eq!(stub.attempts(), 2);
    assert_eq!(stub.delivered(), 0);

    assert_eq!(
        outbox
            .count_with_status(OutboxStatus::DeadLettered)
            .await
            .expect("count"),
        1
    );
    // A dead-lettered row is never retried again.
    let after_dead_letter = worker
        .drain_once(now + chrono::Duration::seconds(600))
        .await
        .expect("drain");
    assert!(after_dead_letter.is_idle());
}

#[tokio::test]
async fn voucher_messages_are_enriched_from_storage() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher_id = VoucherRepository::new(&db)
        .upsert_candidate(&candidate(), now)
        .await
        .expect("upsert")
        .voucher_id();

    let event = DomainEvent::VoucherDiscovered {
        voucher_id,
        source: SourceId::new("feed"),
        version_hash: "abc123def456".into(),
    };
    OutboxRepository::new(&db)
        .enqueue(&event, now)
        .await
        .expect("enqueue");

    let stub = StubNotifier::new();
    worker(&db, &stub, config("chat-1"))
        .drain_once(now)
        .await
        .expect("drain");

    let text = stub.last_text().expect("one message");
    assert!(text.starts_with("NEW VOUCHER"));
    assert!(text.contains("Freeship 50k toan san"));
    assert!(text.contains("Code: FREESHIP50"));
    assert!(text.contains("Discount: 50.000₫"));
    assert!(text.contains("Min spend: 200.000₫"));
    assert!(text.contains("Window: 10/08 12:00 -> 11/08 00:00 (GMT+7)"));
    // The claim signature must never reach a notification.
    assert!(!text.contains("super-secret-signature"));
}

#[tokio::test]
async fn enrichment_is_optional_and_missing_vouchers_still_notify() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let event = DomainEvent::VoucherDiscovered {
        // No such voucher row: enrichment must degrade, not fail.
        voucher_id: Uuid::new_v4(),
        source: SourceId::new("feed"),
        version_hash: "abc123def456".into(),
    };
    OutboxRepository::new(&db)
        .enqueue(&event, now)
        .await
        .expect("enqueue");

    let stub = StubNotifier::new();
    let stats = worker(&db, &stub, config("chat-1"))
        .drain_once(now)
        .await
        .expect("drain");

    assert_eq!(stats.delivered, 1);
    let text = stub.last_text().expect("one message");
    assert!(text.starts_with("NEW VOUCHER"));
    assert!(text.contains("Voucher: "));
    assert!(text.contains("Source: feed"));
}

#[tokio::test]
async fn enrichment_can_be_disabled_by_configuration() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher_id = VoucherRepository::new(&db)
        .upsert_candidate(&candidate(), now)
        .await
        .expect("upsert")
        .voucher_id();

    OutboxRepository::new(&db)
        .enqueue(
            &DomainEvent::VoucherDiscovered {
                voucher_id,
                source: SourceId::new("feed"),
                version_hash: "abc123def456".into(),
            },
            now,
        )
        .await
        .expect("enqueue");

    let stub = StubNotifier::new();
    let config = OutboxWorkerConfig {
        enrich_from_storage: false,
        ..config("chat-1")
    };
    worker(&db, &stub, config)
        .drain_once(now)
        .await
        .expect("drain");

    let text = stub.last_text().expect("one message");
    assert!(!text.contains("Freeship 50k toan san"));
    assert!(text.contains("Voucher: "));
}

#[tokio::test]
async fn state_survives_a_reconnect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("restart.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let now = Utc::now();

    {
        let db = Database::connect(&url, 2).await.expect("connect");
        OutboxRepository::new(&db)
            .enqueue(
                &DomainEvent::ServiceUnhealthy {
                    service: "app".into(),
                    detail: "restart pending".into(),
                },
                now,
            )
            .await
            .expect("enqueue");
        db.close().await;
    }

    // A crash between commit and send must not lose the notification.
    let db = Database::connect(&url, 2).await.expect("reconnect");
    let stub = StubNotifier::new();
    let stats = worker(&db, &stub, config("chat-1"))
        .drain_once(now)
        .await
        .expect("drain");

    assert_eq!(stats.delivered, 1);
    assert!(stub
        .last_text()
        .expect("message")
        .contains("restart pending"));
}
