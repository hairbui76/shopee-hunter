//! Notification outbox. Domain events are enqueued atomically with the state
//! change that produced them; the notifier worker drains them separately so a
//! Telegram outage cannot block core transactions.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::events::DomainEvent;
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    Delivered,
    DeadLettered,
}

impl OutboxStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Delivered => "DELIVERED",
            Self::DeadLettered => "DEAD_LETTERED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: Uuid,
    pub idempotency_key: String,
    pub event_kind: String,
    pub event: DomainEvent,
    pub attempt_count: i64,
}

pub struct OutboxRepository<'a> {
    db: &'a Database,
}

impl<'a> OutboxRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Enqueue an event. Idempotent on the event's idempotency key: a repeated
    /// logical occurrence does not create a duplicate notification.
    pub async fn enqueue(
        &self,
        event: &DomainEvent,
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StorageError> {
        let id = Uuid::new_v4();
        let payload = serde_json::to_string(event).map_err(|e| StorageError::Decode {
            field: "outbox.payload",
            reason: e.to_string(),
        })?;
        let res = sqlx::query(
            "INSERT INTO notification_outbox
                (id, idempotency_key, event_kind, payload, status, attempt_count,
                 created_at, updated_at, next_attempt_at)
             VALUES ($1, $2, $3, $4, 'PENDING', 0, $5, $6, $7)
             ON CONFLICT(idempotency_key) DO NOTHING",
        )
        .bind(uuid_to_str(id))
        .bind(event.idempotency_key())
        .bind(event.kind())
        .bind(payload)
        .bind(ts_to_str(now))
        .bind(ts_to_str(now))
        .bind(ts_to_str(now))
        .execute(self.db.pool())
        .await?;
        Ok((res.rows_affected() == 1).then_some(id))
    }

    /// Fetch a batch of pending events ready to deliver.
    pub async fn fetch_ready(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<OutboxRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM notification_outbox
             WHERE status = 'PENDING' AND next_attempt_at <= $1
             ORDER BY created_at ASC LIMIT $2",
        )
        .bind(ts_to_str(now))
        .bind(limit)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(row_to_outbox).collect()
    }

    pub async fn mark_delivered(&self, id: Uuid, now: DateTime<Utc>) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE notification_outbox SET status = 'DELIVERED', updated_at = $1 WHERE id = $2",
        )
        .bind(ts_to_str(now))
        .bind(uuid_to_str(id))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Record a failed delivery; dead-letter once the attempt ceiling is hit.
    pub async fn mark_failed(
        &self,
        id: Uuid,
        error: &str,
        next_attempt_at: DateTime<Utc>,
        max_attempts: i64,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE notification_outbox SET
                attempt_count = attempt_count + 1,
                status = CASE WHEN attempt_count + 1 >= $1 THEN 'DEAD_LETTERED' ELSE 'PENDING' END,
                last_error = $2, next_attempt_at = $3, updated_at = $4
             WHERE id = $5",
        )
        .bind(max_attempts)
        .bind(error)
        .bind(ts_to_str(next_attempt_at))
        .bind(ts_to_str(now))
        .bind(uuid_to_str(id))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn count_with_status(&self, status: OutboxStatus) -> Result<i64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM notification_outbox WHERE status = $1")
            .bind(status.as_str())
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>("n"))
    }
}

fn row_to_outbox(row: &sqlx::any::AnyRow) -> Result<OutboxRow, StorageError> {
    let payload: String = row.get("payload");
    let event: DomainEvent = serde_json::from_str(&payload).map_err(|e| StorageError::Decode {
        field: "outbox.payload",
        reason: e.to_string(),
    })?;
    Ok(OutboxRow {
        id: str_to_uuid(&row.get::<String, _>("id"), "outbox.id")?,
        idempotency_key: row.get("idempotency_key"),
        event_kind: row.get("event_kind"),
        event,
        attempt_count: row.get::<i64, _>("attempt_count"),
    })
}
