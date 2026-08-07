//! Scenario 6: a bounded "soak-ish" loop over the replay collector.
//!
//! # This is NOT the real soak test
//!
//! ROADMAP Phase 26 calls for a 24–72h soak with memory and file-descriptor
//! monitoring. That cannot live in `cargo test`: it needs a long-running
//! process, an external RSS/FD sampler, and hours of wall time.
//!
//! What this file *does* verify is the property that a real soak would most
//! likely catch and that is cheap to assert here: **repeatedly ingesting the
//! same data must not grow durable state**. A dedup bug, a broken version hash,
//! or an identity that accidentally varies per run would show up as row counts
//! climbing with every iteration — which is exactly how an unbounded-growth
//! bug would present after 24 hours.
//!
//! Growth that *is* expected (the append-only collector run audit) is asserted
//! to grow exactly linearly, so "bounded" is proven rather than assumed.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{candidate, temp_db};
use shopee_hunter_collectors::{CollectorSupervisor, ReplayCollector, SupervisedSource};
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::{CollectorRunRepository, VoucherRepository};

/// Iterations. Bounded so the suite stays fast; the invariant under test is
/// order-independent and shows up within a handful of cycles.
const ITERATIONS: usize = 50;
/// Distinct vouchers replayed on every cycle.
const FIXTURE_SIZE: usize = 5;

#[tokio::test]
async fn repeated_replay_cycles_do_not_grow_the_voucher_table() {
    let (db, _dir) = temp_db().await;

    let candidates: Vec<_> = (0..FIXTURE_SIZE)
        .map(|i| candidate("replay", &format!("fixture-{i}")))
        .collect();
    let source = SupervisedSource::new(
        Arc::new(ReplayCollector::from_candidates("replay", candidates)),
        Duration::from_secs(5),
    );
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let vouchers = VoucherRepository::new(&db);

    // First cycle discovers everything.
    let first = supervisor.run_once(&source).await.expect("first cycle");
    assert_eq!(first.new_count, FIXTURE_SIZE);
    assert_eq!(first.events.len(), FIXTURE_SIZE, "one event per discovery");
    assert_eq!(
        vouchers.count().await.expect("count") as usize,
        FIXTURE_SIZE
    );

    // Every subsequent cycle must be a complete no-op on durable state.
    for iteration in 2..=ITERATIONS {
        let outcome = supervisor
            .run_once(&source)
            .await
            .unwrap_or_else(|err| panic!("cycle {iteration} failed: {err:?}"));

        assert_eq!(
            outcome.new_count, 0,
            "cycle {iteration}: re-seeing known vouchers must not create rows"
        );
        assert_eq!(
            outcome.updated_count, 0,
            "cycle {iteration}: unchanged data must not register as a version change"
        );
        assert_eq!(outcome.unchanged_count, FIXTURE_SIZE);
        assert!(
            outcome.events.is_empty(),
            "cycle {iteration}: a quiet cycle must emit no notifications — \
             otherwise the owner gets a message every poll"
        );
        assert_eq!(
            vouchers.count().await.expect("count") as usize,
            FIXTURE_SIZE,
            "cycle {iteration}: voucher count must stay flat"
        );
    }

    // The audit trail is the one thing that legitimately grows, and it grows
    // exactly once per cycle — no hidden amplification.
    assert_eq!(
        CollectorRunRepository::new(&db)
            .count_for("replay")
            .await
            .expect("run count") as usize,
        ITERATIONS
    );
    assert_eq!(source.health().last_result_count, FIXTURE_SIZE);
}

/// Health state must not accumulate either: after many cycles it is still a
/// single fixed-size snapshot describing the latest run.
#[tokio::test]
async fn source_health_stays_a_bounded_snapshot_across_many_cycles() {
    let (db, _dir) = temp_db().await;
    let source = SupervisedSource::new(
        Arc::new(ReplayCollector::from_candidates(
            "replay-health",
            vec![candidate("replay-health", "only")],
        )),
        Duration::from_secs(5),
    );
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());

    for _ in 0..ITERATIONS {
        supervisor.run_once(&source).await.expect("cycle");
    }

    let health = source.health();
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.last_result_count, 1);
    assert!(health.last_success.is_some());
    assert!(
        health.detail.is_none(),
        "a healthy source carries no accumulated error detail"
    );
}

/// A source that alternates between working and failing must converge on the
/// latest state each cycle rather than drifting — the pattern a flaky upstream
/// produces over a long soak.
#[tokio::test]
async fn alternating_success_and_failure_does_not_drift() {
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());

    let good = SupervisedSource::new(
        Arc::new(ReplayCollector::from_candidates(
            "flaky",
            vec![candidate("flaky", "v1")],
        )),
        Duration::from_secs(5),
    );
    // A replay collector pointed at a directory that does not exist fails
    // deterministically with a Config error.
    let bad = SupervisedSource::new(
        Arc::new(ReplayCollector::from_dir(
            "flaky",
            "/nonexistent/reliability/fixtures",
        )),
        Duration::from_secs(5),
    );

    for _ in 0..10 {
        supervisor.run_once(&good).await.expect("good cycle");
        assert!(supervisor.run_once(&bad).await.is_err());
    }

    // One voucher, regardless of how many cycles or interleaved failures.
    assert_eq!(
        VoucherRepository::new(&db).count().await.expect("count"),
        1,
        "interleaved failures must not duplicate or lose the known voucher"
    );
}
