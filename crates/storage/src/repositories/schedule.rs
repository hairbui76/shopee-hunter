//! Durable schedule job persistence. Database state is authoritative; the
//! scheduler rebuilds in-memory timers from these rows after restart.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::schedule::{JobStatus, ScheduleAction};
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleJobRecord {
    pub id: Uuid,
    pub voucher_id: Uuid,
    pub action: ScheduleAction,
    pub execute_at: DateTime<Utc>,
    pub preflight_at: DateTime<Utc>,
    pub status: JobStatus,
    pub scheduler_version: i64,
    pub attempt_count: i64,
    pub last_result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct ScheduleRepository<'a> {
    db: &'a Database,
}

impl<'a> ScheduleRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert (or, for an existing open (voucher, action) pair, update time)
    /// a scheduled job. Prevents duplicate active jobs for the same action.
    pub async fn upsert(
        &self,
        voucher_id: Uuid,
        action: ScheduleAction,
        execute_at: DateTime<Utc>,
        preflight_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO schedule_jobs
                (id, voucher_id, action, execute_at, preflight_at, status,
                 scheduler_version, attempt_count, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, 'PENDING', 1, 0, $6, $7)
             ON CONFLICT(voucher_id, action) DO UPDATE SET
                execute_at = excluded.execute_at,
                preflight_at = excluded.preflight_at,
                updated_at = excluded.updated_at",
        )
        .bind(uuid_to_str(id))
        .bind(uuid_to_str(voucher_id))
        .bind(action.as_str())
        .bind(ts_to_str(execute_at))
        .bind(ts_to_str(preflight_at))
        .bind(ts_to_str(now))
        .bind(ts_to_str(now))
        .execute(self.db.pool())
        .await?;

        // Return the actual id (existing row keeps its original id).
        let row = sqlx::query("SELECT id FROM schedule_jobs WHERE voucher_id = $1 AND action = $2")
            .bind(uuid_to_str(voucher_id))
            .bind(action.as_str())
            .fetch_one(self.db.pool())
            .await?;
        str_to_uuid(&row.get::<String, _>("id"), "schedule_jobs.id")
    }

    /// All jobs whose status is open (PENDING/READY/RUNNING) — used for
    /// restart reconstruction.
    pub async fn open_jobs(&self) -> Result<Vec<ScheduleJobRecord>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_jobs WHERE status IN ('PENDING','READY','RUNNING')
             ORDER BY execute_at ASC",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    /// Jobs due to enter the preflight window at or before `at`.
    pub async fn due_for_preflight(
        &self,
        at: DateTime<Utc>,
    ) -> Result<Vec<ScheduleJobRecord>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_jobs WHERE status = 'PENDING' AND preflight_at <= $1
             ORDER BY execute_at ASC",
        )
        .bind(ts_to_str(at))
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    pub async fn set_status(
        &self,
        id: Uuid,
        status: JobStatus,
        last_result: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE schedule_jobs SET status = $1, last_result = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(status.as_str())
        .bind(last_result)
        .bind(ts_to_str(now))
        .bind(uuid_to_str(id))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Atomically claim a PENDING/READY job into RUNNING. Returns true if this
    /// caller won the claim (guards against duplicate execution).
    pub async fn try_claim_running(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let res = sqlx::query(
            "UPDATE schedule_jobs SET status = 'RUNNING', attempt_count = attempt_count + 1,
                updated_at = $1
             WHERE id = $2 AND status IN ('PENDING','READY')",
        )
        .bind(ts_to_str(now))
        .bind(uuid_to_str(id))
        .execute(self.db.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Mark jobs stale if their execute time passed by more than `grace`.
    pub async fn mark_stale_before(
        &self,
        cutoff: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        let res = sqlx::query(
            "UPDATE schedule_jobs SET status = 'STALE', updated_at = $1
             WHERE status IN ('PENDING','READY') AND execute_at < $2",
        )
        .bind(ts_to_str(now))
        .bind(ts_to_str(cutoff))
        .execute(self.db.pool())
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<ScheduleJobRecord>, StorageError> {
        let row = sqlx::query("SELECT * FROM schedule_jobs WHERE id = $1")
            .bind(uuid_to_str(id))
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(row_to_job).transpose()
    }
}

fn row_to_job(row: &sqlx::any::AnyRow) -> Result<ScheduleJobRecord, StorageError> {
    Ok(ScheduleJobRecord {
        id: str_to_uuid(&row.get::<String, _>("id"), "schedule_jobs.id")?,
        voucher_id: str_to_uuid(
            &row.get::<String, _>("voucher_id"),
            "schedule_jobs.voucher_id",
        )?,
        action: ScheduleAction::parse(&row.get::<String, _>("action")).ok_or(
            StorageError::Decode {
                field: "schedule_jobs.action",
                reason: "unknown action".into(),
            },
        )?,
        execute_at: str_to_ts(
            &row.get::<String, _>("execute_at"),
            "schedule_jobs.execute_at",
        )?,
        preflight_at: str_to_ts(
            &row.get::<String, _>("preflight_at"),
            "schedule_jobs.preflight_at",
        )?,
        status: JobStatus::parse(&row.get::<String, _>("status")).ok_or(StorageError::Decode {
            field: "schedule_jobs.status",
            reason: "unknown status".into(),
        })?,
        scheduler_version: row.get::<i64, _>("scheduler_version"),
        attempt_count: row.get::<i64, _>("attempt_count"),
        last_result: row
            .try_get::<Option<String>, _>("last_result")
            .ok()
            .flatten(),
        created_at: str_to_ts(
            &row.get::<String, _>("created_at"),
            "schedule_jobs.created_at",
        )?,
        updated_at: str_to_ts(
            &row.get::<String, _>("updated_at"),
            "schedule_jobs.updated_at",
        )?,
    })
}
