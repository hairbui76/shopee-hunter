//! Collector run history for source health and analytics.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Success,
    Partial,
    Failed,
}

impl RunOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::Partial => "PARTIAL",
            Self::Failed => "FAILED",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CollectorRunRecord {
    pub source: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub latency_ms: Option<i64>,
    pub candidate_count: i64,
    pub new_count: i64,
    pub updated_count: i64,
    pub parse_errors: i64,
    pub detail: Option<String>,
}

pub struct CollectorRunRepository<'a> {
    db: &'a Database,
}

impl<'a> CollectorRunRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub async fn record(
        &self,
        run: &CollectorRunRecord,
        outcome: RunOutcome,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO collector_runs
                (id, source, started_at, finished_at, latency_ms, candidate_count,
                 new_count, updated_count, parse_errors, outcome, detail)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(uuid_to_str(id))
        .bind(&run.source)
        .bind(ts_to_str(run.started_at))
        .bind(opt_ts_to_str(run.finished_at))
        .bind(run.latency_ms)
        .bind(run.candidate_count)
        .bind(run.new_count)
        .bind(run.updated_count)
        .bind(run.parse_errors)
        .bind(outcome.as_str())
        .bind(&run.detail)
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    pub async fn last_success_at(
        &self,
        source: &str,
    ) -> Result<Option<DateTime<Utc>>, StorageError> {
        let row = sqlx::query(
            "SELECT finished_at FROM collector_runs
             WHERE source = $1 AND outcome = 'SUCCESS' AND finished_at IS NOT NULL
             ORDER BY finished_at DESC LIMIT 1",
        )
        .bind(source)
        .fetch_optional(self.db.pool())
        .await?;
        match row {
            None => Ok(None),
            Some(r) => opt_str_to_ts(
                r.try_get::<Option<String>, _>("finished_at").ok().flatten(),
                "collector_runs.finished_at",
            ),
        }
    }

    pub async fn count_for(&self, source: &str) -> Result<i64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM collector_runs WHERE source = $1")
            .bind(source)
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>("n"))
    }
}
