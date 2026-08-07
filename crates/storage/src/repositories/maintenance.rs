//! Data retention (ROADMAP Phase 33). Bounded cleanup of historical rows so an
//! always-on deployment does not grow without limit, while keeping enough
//! history for source-quality and reliability analytics.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::convert::ts_to_str;
use crate::db::Database;
use crate::error::StorageError;

/// Per-table retention cutoffs. Anything older than the cutoff is eligible for
/// deletion. `None` means "keep forever".
#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    pub observations_before: Option<DateTime<Utc>>,
    pub versions_before: Option<DateTime<Utc>>,
    pub claim_attempts_before: Option<DateTime<Utc>>,
    pub collector_runs_before: Option<DateTime<Utc>>,
    /// Only DELIVERED/DEAD_LETTERED outbox rows are pruned; PENDING is kept.
    pub delivered_outbox_before: Option<DateTime<Utc>>,
    pub health_events_before: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub observations: u64,
    pub versions: u64,
    pub claim_attempts: u64,
    pub collector_runs: u64,
    pub outbox: u64,
    pub health_events: u64,
}

impl PruneReport {
    pub fn total(&self) -> u64 {
        self.observations
            + self.versions
            + self.claim_attempts
            + self.collector_runs
            + self.outbox
            + self.health_events
    }
}

pub struct MaintenanceRepository<'a> {
    db: &'a Database,
}

impl<'a> MaintenanceRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    async fn delete_before(
        &self,
        table: &str,
        column: &str,
        cutoff: Option<DateTime<Utc>>,
    ) -> Result<u64, StorageError> {
        let Some(cutoff) = cutoff else { return Ok(0) };
        // Table/column are internal constants, never user input.
        let sql = format!("DELETE FROM {table} WHERE {column} < $1");
        let res = sqlx::query(&sql)
            .bind(ts_to_str(cutoff))
            .execute(self.db.pool())
            .await?;
        Ok(res.rows_affected())
    }

    /// Apply the retention policy. Foreign keys cascade from vouchers, but we
    /// prune history tables directly by age. Vouchers themselves are retained
    /// (they are the canonical record).
    pub async fn prune(&self, policy: &RetentionPolicy) -> Result<PruneReport, StorageError> {
        let observations = self
            .delete_before(
                "voucher_observations",
                "observed_at",
                policy.observations_before,
            )
            .await?;
        let versions = self
            .delete_before("voucher_versions", "created_at", policy.versions_before)
            .await?;
        let claim_attempts = self
            .delete_before("claim_attempts", "started_at", policy.claim_attempts_before)
            .await?;
        let collector_runs = self
            .delete_before("collector_runs", "started_at", policy.collector_runs_before)
            .await?;
        let health_events = self
            .delete_before(
                "service_health_events",
                "created_at",
                policy.health_events_before,
            )
            .await?;

        // Outbox: only prune already-terminal rows.
        let outbox = match policy.delivered_outbox_before {
            None => 0,
            Some(cutoff) => {
                let res = sqlx::query(
                    "DELETE FROM notification_outbox
                     WHERE status IN ('DELIVERED','DEAD_LETTERED') AND updated_at < $1",
                )
                .bind(ts_to_str(cutoff))
                .execute(self.db.pool())
                .await?;
                res.rows_affected()
            }
        };

        Ok(PruneReport {
            observations,
            versions,
            claim_attempts,
            collector_runs,
            outbox,
            health_events,
        })
    }

    /// Best-effort database maintenance (SQLite only; Postgres autovacuums).
    pub async fn vacuum(&self) -> Result<(), StorageError> {
        if self.db.kind() == crate::db::DbKind::Sqlite {
            let row = sqlx::query("PRAGMA journal_mode")
                .fetch_optional(self.db.pool())
                .await?;
            // WAL databases support incremental vacuum; keep it cheap.
            let _ = row.map(|r| r.get::<String, _>(0));
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(self.db.pool())
                .await?;
        }
        Ok(())
    }
}
