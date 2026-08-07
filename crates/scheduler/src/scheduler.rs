//! Durable scheduler: persists schedule intent, reconstructs jobs after
//! restart, detects stale/missed jobs, and prevents duplicate execution.

use std::time::Duration;

use chrono::{DateTime, Utc};
use shopee_hunter_domain::clock::Clock;
use shopee_hunter_domain::schedule::ScheduleAction;
use shopee_hunter_storage::{Database, ScheduleJobRecord, ScheduleRepository, StorageError};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Coarse wake interval for the outer scheduling loop.
    pub coarse_tick: Duration,
    /// How long before `execute_at` a job enters the preflight window.
    pub preflight_lead: Duration,
    /// A pending job whose `execute_at` is older than this at startup is stale.
    pub stale_after: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            coarse_tick: Duration::from_secs(15),
            preflight_lead: Duration::from_secs(600),
            stale_after: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconstructReport {
    pub future_jobs: usize,
    pub stale_jobs: u64,
}

/// Durable scheduler over the schedule_jobs table.
pub struct Scheduler {
    db: Database,
    config: SchedulerConfig,
}

impl Scheduler {
    pub fn new(db: Database, config: SchedulerConfig) -> Self {
        Self { db, config }
    }

    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Persist scheduling intent. `preflight_at` defaults to
    /// `execute_at - preflight_lead`. Duplicate (voucher, action) jobs are
    /// prevented by the storage unique constraint (idempotent upsert).
    pub async fn schedule(
        &self,
        voucher_id: Uuid,
        action: ScheduleAction,
        execute_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Uuid, StorageError> {
        let lead = chrono::Duration::from_std(self.config.preflight_lead).unwrap_or_default();
        let preflight_at = execute_at - lead;
        ScheduleRepository::new(&self.db)
            .upsert(voucher_id, action, execute_at, preflight_at, now)
            .await
    }

    /// Rebuild scheduling state from the database after a restart. Jobs whose
    /// execute time is far in the past are marked STALE (never blindly fired);
    /// future jobs are counted for the coarse loop to pick up.
    pub async fn reconstruct(&self, now: DateTime<Utc>) -> Result<ReconstructReport, StorageError> {
        let repo = ScheduleRepository::new(&self.db);
        let stale_cutoff =
            now - chrono::Duration::from_std(self.config.stale_after).unwrap_or_default();
        let stale = repo.mark_stale_before(stale_cutoff, now).await?;
        let open = repo.open_jobs().await?;
        Ok(ReconstructReport {
            future_jobs: open.len(),
            stale_jobs: stale,
        })
    }

    /// Jobs that have entered the preflight window and are ready to run.
    pub async fn due_jobs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduleJobRecord>, StorageError> {
        ScheduleRepository::new(&self.db)
            .due_for_preflight(now)
            .await
    }

    /// Atomically claim a job into RUNNING. Returns true iff this caller won —
    /// prevents duplicate execution across workers/restarts.
    pub async fn claim_job(&self, job_id: Uuid, now: DateTime<Utc>) -> Result<bool, StorageError> {
        ScheduleRepository::new(&self.db)
            .try_claim_running(job_id, now)
            .await
    }

    pub fn repository(&self) -> ScheduleRepository<'_> {
        ScheduleRepository::new(&self.db)
    }
}

/// Compute the preflight instant for a target using an injected clock — pure
/// helper for tests and callers that don't want to touch the DB.
pub fn preflight_at(clock: &dyn Clock, execute_at: DateTime<Utc>, lead: Duration) -> DateTime<Utc> {
    let _ = clock;
    execute_at - chrono::Duration::from_std(lead).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shopee_hunter_domain::schedule::JobStatus;
    use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
    use shopee_hunter_domain::SourceId;
    use shopee_hunter_storage::VoucherRepository;

    async fn temp_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("s.db").display());
        (Database::connect(&url, 4).await.unwrap(), dir)
    }

    fn candidate(key: &str) -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: key.into(),
            external_id: Some(key.into()),
            code: Some("C".into()),
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

    async fn make_voucher(db: &Database, key: &str) -> Uuid {
        VoucherRepository::new(db)
            .upsert_candidate(&candidate(key), Utc::now())
            .await
            .unwrap()
            .voucher_id()
    }

    #[tokio::test]
    async fn schedule_is_idempotent_per_voucher_action() {
        let (db, _d) = temp_db().await;
        let vid = make_voucher(&db, "a").await;
        let sched = Scheduler::new(db.clone(), SchedulerConfig::default());
        let now = Utc::now();
        let exec = now + chrono::Duration::hours(2);

        let j1 = sched
            .schedule(vid, ScheduleAction::ClaimVoucher, exec, now)
            .await
            .unwrap();
        let j2 = sched
            .schedule(vid, ScheduleAction::ClaimVoucher, exec, now)
            .await
            .unwrap();
        assert_eq!(j1, j2);
        assert_eq!(sched.repository().open_jobs().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reconstruct_marks_stale_and_keeps_future() {
        let (db, _d) = temp_db().await;
        let now = Utc::now();
        let sched = Scheduler::new(db.clone(), SchedulerConfig::default());

        let future = make_voucher(&db, "future").await;
        let stale = make_voucher(&db, "stale").await;
        sched
            .schedule(
                future,
                ScheduleAction::ClaimVoucher,
                now + chrono::Duration::hours(1),
                now,
            )
            .await
            .unwrap();
        sched
            .schedule(
                stale,
                ScheduleAction::ClaimVoucher,
                now - chrono::Duration::hours(1),
                now,
            )
            .await
            .unwrap();

        let report = sched.reconstruct(now).await.unwrap();
        assert_eq!(report.stale_jobs, 1);
        // Only the future job remains open.
        let open = sched.repository().open_jobs().await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].voucher_id, future);
        assert_eq!(report.future_jobs, 1);
    }

    #[tokio::test]
    async fn due_jobs_respects_preflight_window_and_single_claim() {
        let (db, _d) = temp_db().await;
        let now = Utc::now();
        // Short preflight lead so the job is due almost immediately.
        let config = SchedulerConfig {
            preflight_lead: Duration::from_secs(3600),
            ..SchedulerConfig::default()
        };
        let sched = Scheduler::new(db.clone(), config);
        let vid = make_voucher(&db, "due").await;
        let job = sched
            .schedule(
                vid,
                ScheduleAction::ClaimVoucher,
                now + chrono::Duration::minutes(30), // within the 1h preflight lead
                now,
            )
            .await
            .unwrap();

        let due = sched.due_jobs(now).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, job);

        // Only one caller claims it.
        assert!(sched.claim_job(job, now).await.unwrap());
        assert!(!sched.claim_job(job, now).await.unwrap());
        let record = sched.repository().get(job).await.unwrap().unwrap();
        assert_eq!(record.status, JobStatus::Running);
    }
}
