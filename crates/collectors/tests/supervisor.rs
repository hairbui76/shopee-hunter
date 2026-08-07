//! Collector framework tests: replay ingestion, failure isolation, and
//! per-source health.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use shopee_hunter_collectors::{
    CollectionContext, CollectionResult, CollectorError, CollectorRegistry, CollectorSupervisor,
    ReplayCollector, SourceHealthState, SupervisedSource, VoucherCollector,
};
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
use shopee_hunter_domain::SourceId;
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::{Database, VoucherRepository};

async fn temp_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("c.db").display());
    (Database::connect(&url, 4).await.unwrap(), dir)
}

fn candidate(source: &str, key: &str) -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new(source),
        source_key: key.into(),
        external_id: Some(key.into()),
        code: Some("SALE".into()),
        promotion_id: None,
        signature: None,
        title: "Voucher".into(),
        description: None,
        voucher_type: VoucherType::Platform,
        discount_type: None,
        discount_amount: Some(Decimal::new(10_000, 0)),
        discount_percent: None,
        max_discount: None,
        min_spend: Some(Decimal::ZERO),
        start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
        end_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 17, 0, 0).unwrap()),
        scope: None,
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::json!({"k": key}),
        observed_at: Utc::now(),
        parser_version: "t1".into(),
    }
}

/// A collector that always fails, to prove failure isolation.
struct FailingCollector;

#[async_trait]
impl VoucherCollector for FailingCollector {
    fn name(&self) -> &str {
        "failing"
    }
    async fn collect(&self, _: &CollectionContext) -> Result<CollectionResult, CollectorError> {
        Err(CollectorError::Transient("upstream down".into()))
    }
}

#[tokio::test]
async fn replay_fixtures_create_real_vouchers() {
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let collector = Arc::new(ReplayCollector::from_candidates(
        "replay",
        vec![candidate("replay", "a"), candidate("replay", "b")],
    ));
    let source = SupervisedSource::new(collector, Duration::from_secs(5));

    let outcome = supervisor.run_once(&source).await.unwrap();
    assert_eq!(outcome.new_count, 2);
    assert_eq!(outcome.events.len(), 2);
    assert_eq!(VoucherRepository::new(&db).count().await.unwrap(), 2);
    assert_eq!(source.health().state, SourceHealthState::Healthy);

    // Re-run: same items dedupe, no new vouchers.
    let again = supervisor.run_once(&source).await.unwrap();
    assert_eq!(again.new_count, 0);
    assert_eq!(again.unchanged_count, 2);
    assert_eq!(VoucherRepository::new(&db).count().await.unwrap(), 2);
}

#[tokio::test]
async fn one_failed_source_does_not_interrupt_another() {
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());

    let good = SupervisedSource::new(
        Arc::new(ReplayCollector::from_candidates(
            "good",
            vec![candidate("good", "x")],
        )),
        Duration::from_secs(5),
    );
    let bad = SupervisedSource::new(Arc::new(FailingCollector), Duration::from_secs(5));

    let bad_result = supervisor.run_once(&bad).await;
    assert!(bad_result.is_err());
    assert_eq!(bad.health().state, SourceHealthState::Degraded);
    assert_eq!(bad.health().consecutive_failures, 1);

    // The good source still works after the bad one failed.
    let good_result = supervisor.run_once(&good).await.unwrap();
    assert_eq!(good_result.new_count, 1);
    assert_eq!(good.health().state, SourceHealthState::Healthy);
    assert_eq!(VoucherRepository::new(&db).count().await.unwrap(), 1);
}

#[tokio::test]
async fn slow_collector_times_out_and_degrades() {
    struct Slow;
    #[async_trait]
    impl VoucherCollector for Slow {
        fn name(&self) -> &str {
            "slow"
        }
        async fn collect(&self, _: &CollectionContext) -> Result<CollectionResult, CollectorError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(CollectionResult::default())
        }
    }
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db, Metrics::new());
    let source = SupervisedSource::new(Arc::new(Slow), Duration::from_millis(50));

    let result = supervisor.run_once(&source).await;
    assert!(matches!(result, Err(CollectorError::Timeout)));
    assert_eq!(source.health().state, SourceHealthState::Degraded);
}

#[tokio::test]
async fn registry_runs_two_collectors_independently() {
    let mut registry = CollectorRegistry::new();
    registry.register(Arc::new(ReplayCollector::from_candidates(
        "src-a",
        vec![candidate("src-a", "1")],
    )));
    registry.register(Arc::new(ReplayCollector::from_candidates(
        "src-b",
        vec![candidate("src-b", "1"), candidate("src-b", "2")],
    )));
    assert_eq!(registry.len(), 2);

    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    for collector in registry.all() {
        let source = SupervisedSource::new(collector, Duration::from_secs(5));
        supervisor.run_once(&source).await.unwrap();
    }
    assert_eq!(VoucherRepository::new(&db).count().await.unwrap(), 3);
}
