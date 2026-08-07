//! Append-oriented claim attempt audit records.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::claim::ClaimResultClass;
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAttemptRecord {
    pub id: Uuid,
    pub voucher_id: Uuid,
    pub schedule_job_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub request_version: Option<String>,
    pub result_class: Option<ClaimResultClass>,
    pub upstream_status: Option<i64>,
    pub latency_ms: Option<i64>,
    pub retry_index: i64,
    pub diagnostic_code: Option<String>,
}

pub struct ClaimRepository<'a> {
    db: &'a Database,
}

impl<'a> ClaimRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Create a durable attempt intent before sending the request.
    pub async fn begin_attempt(
        &self,
        voucher_id: Uuid,
        schedule_job_id: Option<Uuid>,
        retry_index: i64,
        started_at: DateTime<Utc>,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO claim_attempts
                (id, voucher_id, schedule_job_id, started_at, retry_index)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(uuid_to_str(id))
        .bind(uuid_to_str(voucher_id))
        .bind(schedule_job_id.map(uuid_to_str))
        .bind(ts_to_str(started_at))
        .bind(retry_index)
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Record the classified outcome of an attempt.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_attempt(
        &self,
        id: Uuid,
        result_class: ClaimResultClass,
        upstream_status: Option<i64>,
        latency_ms: Option<i64>,
        request_version: Option<&str>,
        diagnostic_code: Option<&str>,
        completed_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE claim_attempts SET completed_at = $1, result_class = $2, upstream_status = $3,
                latency_ms = $4, request_version = $5, diagnostic_code = $6 WHERE id = $7",
        )
        .bind(ts_to_str(completed_at))
        .bind(result_class.as_str())
        .bind(upstream_status)
        .bind(latency_ms)
        .bind(request_version)
        .bind(diagnostic_code)
        .bind(uuid_to_str(id))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Whether a terminal success (SUCCESS/ALREADY_SAVED) already exists for
    /// this voucher — idempotency guard against re-claiming.
    pub async fn has_successful_attempt(&self, voucher_id: Uuid) -> Result<bool, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM claim_attempts
             WHERE voucher_id = $1 AND result_class IN ('SUCCESS','ALREADY_SAVED')",
        )
        .bind(uuid_to_str(voucher_id))
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.get::<i64, _>("n") > 0)
    }

    pub async fn attempts_for(
        &self,
        voucher_id: Uuid,
    ) -> Result<Vec<ClaimAttemptRecord>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM claim_attempts WHERE voucher_id = $1 ORDER BY started_at ASC",
        )
        .bind(uuid_to_str(voucher_id))
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(row_to_attempt).collect()
    }

    /// The most recent claim attempts across all vouchers (admin/observability).
    pub async fn recent(&self, limit: i64) -> Result<Vec<ClaimAttemptRecord>, StorageError> {
        let rows = sqlx::query("SELECT * FROM claim_attempts ORDER BY started_at DESC LIMIT $1")
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(row_to_attempt).collect()
    }
}

fn row_to_attempt(row: &sqlx::any::AnyRow) -> Result<ClaimAttemptRecord, StorageError> {
    let result_class = row
        .try_get::<Option<String>, _>("result_class")
        .ok()
        .flatten()
        .and_then(|s| ClaimResultClass::parse(&s));
    Ok(ClaimAttemptRecord {
        id: str_to_uuid(&row.get::<String, _>("id"), "claim_attempts.id")?,
        voucher_id: str_to_uuid(
            &row.get::<String, _>("voucher_id"),
            "claim_attempts.voucher_id",
        )?,
        schedule_job_id: row
            .try_get::<Option<String>, _>("schedule_job_id")
            .ok()
            .flatten()
            .map(|s| str_to_uuid(&s, "claim_attempts.schedule_job_id"))
            .transpose()?,
        started_at: str_to_ts(
            &row.get::<String, _>("started_at"),
            "claim_attempts.started_at",
        )?,
        completed_at: opt_str_to_ts(
            row.try_get::<Option<String>, _>("completed_at")
                .ok()
                .flatten(),
            "claim_attempts.completed_at",
        )?,
        request_version: row
            .try_get::<Option<String>, _>("request_version")
            .ok()
            .flatten(),
        result_class,
        upstream_status: row
            .try_get::<Option<i64>, _>("upstream_status")
            .ok()
            .flatten(),
        latency_ms: row.try_get::<Option<i64>, _>("latency_ms").ok().flatten(),
        retry_index: row.get::<i64, _>("retry_index"),
        diagnostic_code: row
            .try_get::<Option<String>, _>("diagnostic_code")
            .ok()
            .flatten(),
    })
}
