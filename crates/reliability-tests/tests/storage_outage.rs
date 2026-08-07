//! Database outage during a collection cycle (ROADMAP Phase 26: "DB restart").
//!
//! Injected by closing the pool underneath a running supervisor, which is what
//! a PostgreSQL restart or a connection-limit exhaustion looks like to the
//! application: every subsequent query fails immediately.
//!
//! The safe behaviour asserted here is narrow but important — the failure is
//! surfaced as a typed error, nothing panics, no partial voucher state is
//! written, and the process can keep running.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{candidate, temp_db};
use shopee_hunter_collectors::{
    CollectorError, CollectorSupervisor, ReplayCollector, SourceHealthState, SupervisedSource,
};
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::VoucherRepository;

#[tokio::test]
async fn a_database_outage_surfaces_as_a_typed_error_without_panicking() {
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let source = SupervisedSource::new(
        Arc::new(ReplayCollector::from_candidates(
            "outage",
            vec![candidate("outage", "v1")],
        )),
        Duration::from_secs(5),
    );

    // Healthy baseline.
    let first = supervisor.run_once(&source).await.expect("healthy cycle");
    assert_eq!(first.new_count, 1);
    assert_eq!(source.health().state, SourceHealthState::Healthy);

    // The database goes away mid-operation.
    db.close().await;

    let err = supervisor
        .run_once(&source)
        .await
        .expect_err("a dead database must not be reported as a successful cycle");
    assert!(
        matches!(err, CollectorError::Config(_)),
        "expected a persistence error, got {err:?}"
    );
    assert!(
        !err.is_transient(),
        "a persistence failure must not be retried like a network blip: without \
         durable audit state the watcher has nowhere to record what it did"
    );

    // Repeated cycles keep failing cleanly rather than panicking or hanging.
    for _ in 0..3 {
        assert!(supervisor.run_once(&source).await.is_err());
    }
}

#[tokio::test]
async fn a_read_after_outage_fails_cleanly_rather_than_returning_wrong_data() {
    let (db, _dir) = temp_db().await;
    let repo_db = db.clone();

    VoucherRepository::new(&db)
        .upsert_candidate(&candidate("outage", "before"), chrono::Utc::now())
        .await
        .expect("seed");
    assert_eq!(VoucherRepository::new(&db).count().await.expect("count"), 1);

    db.close().await;

    // The critical property: an outage produces an error, never a plausible
    // wrong answer such as a zero count that would read as "no vouchers known".
    assert!(
        VoucherRepository::new(&repo_db).count().await.is_err(),
        "a closed pool must surface an error, not a silently empty result"
    );
}
