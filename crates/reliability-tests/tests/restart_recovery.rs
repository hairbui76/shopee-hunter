//! Scenario 4: process restart around a scheduled job, and duplicate workers.
//!
//! Two dangerous failure modes are covered:
//!
//! * A restart *losing* a near-future job, so a voucher is silently never
//!   claimed.
//! * A restart *blindly firing* a job whose moment passed hours ago, or two
//!   workers both firing the same job — a duplicate claim storm.
//!
//! The database is authoritative, so "restart" is modelled faithfully by
//! dropping the `Scheduler` and building a new one over the same database.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{candidate, temp_db};
use shopee_hunter_domain::schedule::{JobStatus, ScheduleAction};
use shopee_hunter_scheduler::{Scheduler, SchedulerConfig};
use shopee_hunter_storage::{Database, ScheduleRepository, VoucherRepository};
use uuid::Uuid;

fn config() -> SchedulerConfig {
    SchedulerConfig {
        coarse_tick: Duration::from_secs(15),
        preflight_lead: Duration::from_secs(600),
        stale_after: Duration::from_secs(300),
    }
}

async fn seed_voucher(db: &Database, key: &str) -> Uuid {
    VoucherRepository::new(db)
        .upsert_candidate(&candidate("feed", key), Utc::now())
        .await
        .expect("seed voucher")
        .voucher_id()
}

#[tokio::test]
async fn a_restart_keeps_near_future_jobs_and_stales_long_past_ones() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();

    let soon_voucher = seed_voucher(&db, "soon").await;
    let missed_voucher = seed_voucher(&db, "missed").await;

    // Pre-restart process schedules both.
    let before_restart = Scheduler::new(db.clone(), config());
    let soon_job = before_restart
        .schedule(
            soon_voucher,
            ScheduleAction::ClaimVoucher,
            now + chrono::Duration::minutes(5),
            now,
        )
        .await
        .expect("schedule near-future job");
    let missed_job = before_restart
        .schedule(
            missed_voucher,
            ScheduleAction::ClaimVoucher,
            now - chrono::Duration::hours(6),
            now,
        )
        .await
        .expect("schedule long-past job");

    // --- process restart: everything in memory is gone ---
    drop(before_restart);
    let after_restart = Scheduler::new(db.clone(), config());
    let report = after_restart
        .reconstruct(now)
        .await
        .expect("reconstruct from durable state");

    assert_eq!(
        report.stale_jobs, 1,
        "the job whose moment passed six hours ago must be marked stale, not fired"
    );
    assert_eq!(
        report.future_jobs, 1,
        "the near-future job must survive the restart"
    );

    let repo = ScheduleRepository::new(&db);
    let soon = repo.get(soon_job).await.expect("load").expect("job exists");
    let missed = repo
        .get(missed_job)
        .await
        .expect("load")
        .expect("job exists");

    assert_eq!(soon.status, JobStatus::Pending, "still actionable");
    assert_eq!(
        missed.status,
        JobStatus::Stale,
        "a long-missed job must never be blindly executed on restart"
    );
}

#[tokio::test]
async fn a_job_inside_the_grace_window_is_not_declared_stale() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher = seed_voucher(&db, "recent").await;

    let scheduler = Scheduler::new(db.clone(), config());
    // 30s late, well inside the 300s grace: a brief restart must not discard it.
    let job = scheduler
        .schedule(
            voucher,
            ScheduleAction::ClaimVoucher,
            now - chrono::Duration::seconds(30),
            now,
        )
        .await
        .expect("schedule");

    let report = scheduler.reconstruct(now).await.expect("reconstruct");
    assert_eq!(report.stale_jobs, 0);

    let record = ScheduleRepository::new(&db)
        .get(job)
        .await
        .expect("load")
        .expect("exists");
    assert_eq!(record.status, JobStatus::Pending);
    assert!(
        scheduler
            .due_jobs(now)
            .await
            .expect("due jobs")
            .iter()
            .any(|j| j.id == job),
        "a barely-late job is still due, so a quick restart does not lose it"
    );
}

#[tokio::test]
async fn a_duplicate_worker_cannot_claim_the_same_job() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher = seed_voucher(&db, "contended").await;

    let scheduler = Scheduler::new(db.clone(), config());
    let job = scheduler
        .schedule(
            voucher,
            ScheduleAction::ClaimVoucher,
            now + chrono::Duration::minutes(1),
            now,
        )
        .await
        .expect("schedule");

    assert!(
        scheduler.claim_job(job, now).await.expect("first claim"),
        "the first worker wins"
    );
    for round in 0..3 {
        assert!(
            !scheduler.claim_job(job, now).await.expect("later claim"),
            "round {round}: a second worker must never win the same job"
        );
    }

    let record = ScheduleRepository::new(&db)
        .get(job)
        .await
        .expect("load")
        .expect("exists");
    assert_eq!(record.status, JobStatus::Running);
    assert_eq!(
        record.attempt_count, 1,
        "only the winning claim may increment the attempt counter"
    );
}

#[tokio::test]
async fn concurrent_workers_racing_for_one_job_produce_exactly_one_winner() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher = seed_voucher(&db, "raced").await;

    let scheduler = Arc::new(Scheduler::new(db.clone(), config()));
    let job = scheduler
        .schedule(
            voucher,
            ScheduleAction::ClaimVoucher,
            now + chrono::Duration::minutes(1),
            now,
        )
        .await
        .expect("schedule");

    // Eight workers wake at once, as they would after a restart storm.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let scheduler = Arc::clone(&scheduler);
        handles.push(tokio::spawn(async move {
            scheduler.claim_job(job, Utc::now()).await
        }));
    }

    let mut winners = 0;
    for handle in handles {
        if handle.await.expect("worker task").expect("claim query") {
            winners += 1;
        }
    }
    assert_eq!(
        winners, 1,
        "exactly one worker may execute a job: more than one is a duplicate claim storm"
    );

    let record = ScheduleRepository::new(&db)
        .get(job)
        .await
        .expect("load")
        .expect("exists");
    assert_eq!(record.attempt_count, 1);
}

#[tokio::test]
async fn rescheduling_the_same_voucher_action_never_creates_a_second_job() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher = seed_voucher(&db, "idempotent").await;
    let scheduler = Scheduler::new(db.clone(), config());

    let first = scheduler
        .schedule(
            voucher,
            ScheduleAction::ClaimVoucher,
            now + chrono::Duration::hours(2),
            now,
        )
        .await
        .expect("schedule");

    // A restart replaying its scheduling logic, plus a metadata refresh moving
    // the start time, must both converge on the same job row.
    for offset in [2, 3, 4] {
        let again = scheduler
            .schedule(
                voucher,
                ScheduleAction::ClaimVoucher,
                now + chrono::Duration::hours(offset),
                now,
            )
            .await
            .expect("re-schedule");
        assert_eq!(
            again, first,
            "the same (voucher, action) must reuse its job"
        );
    }

    let open = ScheduleRepository::new(&db)
        .open_jobs()
        .await
        .expect("open jobs");
    assert_eq!(open.len(), 1);
    // Timestamps round-trip through fixed-width RFC3339 microseconds (see
    // storage::convert::ts_to_str, which needs a fixed width so TEXT ordering
    // stays chronological), so sub-microsecond truncation is expected.
    let drift = (open[0].execute_at - (now + chrono::Duration::hours(4)))
        .num_microseconds()
        .unwrap_or(i64::MAX)
        .abs();
    assert!(
        drift <= 1,
        "the latest intent wins, without duplicating the job (drift {drift}us)"
    );
}

/// A restart immediately before a job's moment: the job must still be
/// discoverable as due, and still claimable exactly once.
#[tokio::test]
async fn a_restart_just_before_execution_neither_loses_nor_duplicates_the_job() {
    let (db, _dir) = temp_db().await;
    let now = Utc::now();
    let voucher = seed_voucher(&db, "imminent").await;

    let pre = Scheduler::new(db.clone(), config());
    let job = pre
        .schedule(
            voucher,
            ScheduleAction::ClaimVoucher,
            now + chrono::Duration::seconds(2),
            now,
        )
        .await
        .expect("schedule");
    drop(pre);

    // Restart happens two seconds before the target.
    let post = Scheduler::new(db.clone(), config());
    let report = post.reconstruct(now).await.expect("reconstruct");
    assert_eq!(report.stale_jobs, 0);
    assert_eq!(report.future_jobs, 1);

    let due = post.due_jobs(now).await.expect("due jobs");
    assert!(
        due.iter().any(|j| j.id == job),
        "the job is inside its preflight window and must be picked up"
    );

    assert!(post.claim_job(job, now).await.expect("claim"));
    assert!(
        !post.claim_job(job, now).await.expect("second claim"),
        "the restarted process must not be able to run it twice"
    );
}
