//! Service health transition audit trail.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

pub struct HealthRepository<'a> {
    db: &'a Database,
}

impl<'a> HealthRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub async fn record_transition(
        &self,
        service: &str,
        from_state: Option<&str>,
        to_state: &str,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO service_health_events (id, service, from_state, to_state, reason, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(uuid_to_str(Uuid::new_v4()))
        .bind(service)
        .bind(from_state)
        .bind(to_state)
        .bind(reason)
        .bind(ts_to_str(now))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn count(&self) -> Result<i64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM service_health_events")
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>("n"))
    }
}
