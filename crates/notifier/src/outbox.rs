//! Outbox delivery worker (ROADMAP Phase 16).
//!
//! Domain events are committed to `notification_outbox` in the same
//! transaction as the state change that produced them. This worker drains that
//! table on its own schedule, so a Telegram outage can never block a collector
//! or claim transaction, and a crash between commit and send cannot lose a
//! notification.
//!
//! Delivery semantics:
//!
//! * **idempotent** — the outbox row is unique per
//!   `DomainEvent::idempotency_key`, and a delivered row is never re-fetched,
//!   so the same logical occurrence yields at most one message;
//! * **bounded** — each failure increments `attempt_count` and pushes
//!   `next_attempt_at` out by exponential backoff until the row dead-letters;
//! * **isolated** — one failing message never aborts the batch, and a storage
//!   failure is surfaced rather than swallowed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::voucher::Voucher;
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::{Database, OutboxRepository, StorageError, VoucherRepository};
use tokio_util::sync::CancellationToken;

use crate::error::NotifierError;
use crate::format::{self, RenderedMessage};
use crate::notifier::Notifier;

/// Worker tuning. All values are configuration, so delivery can be slowed,
/// batched, or disabled without a code change.
#[derive(Debug, Clone)]
pub struct OutboxWorkerConfig {
    /// Telegram chat that receives the messages.
    pub chat_id: String,
    /// Maximum rows drained per iteration.
    pub batch_size: i64,
    /// Attempts before a row is dead-lettered.
    pub max_attempts: i64,
    /// First retry delay; doubles per attempt.
    pub base_backoff: Duration,
    /// Retry delay ceiling.
    pub max_backoff: Duration,
    /// Load voucher details to enrich voucher-related messages.
    pub enrich_from_storage: bool,
    /// Delay between drain iterations in [`OutboxNotifierWorker::run`].
    pub poll_interval: Duration,
}

impl Default for OutboxWorkerConfig {
    fn default() -> Self {
        Self {
            chat_id: String::new(),
            batch_size: 20,
            max_attempts: 5,
            base_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(30 * 60),
            enrich_from_storage: true,
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// Result of one drain iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub fetched: usize,
    pub delivered: usize,
    /// Failed but still within budget: scheduled for a later attempt.
    pub retry_scheduled: usize,
    /// Failed and past the attempt ceiling.
    pub dead_lettered: usize,
}

impl DrainStats {
    pub fn is_idle(&self) -> bool {
        self.fetched == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxWorkerError {
    /// The outbox itself is unreadable/unwritable; the loop must back off.
    #[error("outbox storage failure: {0}")]
    Storage(#[from] StorageError),

    #[error("outbox worker misconfigured: {0}")]
    Config(#[source] NotifierError),
}

/// Drains the notification outbox through a [`Notifier`].
pub struct OutboxNotifierWorker<N: Notifier + ?Sized> {
    db: Database,
    notifier: Arc<N>,
    config: OutboxWorkerConfig,
    metrics: Metrics,
}

impl<N: Notifier + ?Sized> OutboxNotifierWorker<N> {
    /// Validate configuration up front so a misconfigured deployment fails at
    /// startup instead of at the first notification.
    pub fn new(
        db: Database,
        notifier: Arc<N>,
        config: OutboxWorkerConfig,
    ) -> Result<Self, OutboxWorkerError> {
        if config.chat_id.trim().is_empty() {
            return Err(OutboxWorkerError::Config(NotifierError::config(
                "chat_id is empty",
            )));
        }
        if config.batch_size <= 0 {
            return Err(OutboxWorkerError::Config(NotifierError::config(
                "batch_size must be positive",
            )));
        }
        if config.max_attempts <= 0 {
            return Err(OutboxWorkerError::Config(NotifierError::config(
                "max_attempts must be positive",
            )));
        }
        Ok(Self {
            db,
            notifier,
            config,
            metrics: Metrics::new(),
        })
    }

    /// Share the application metrics registry.
    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn config(&self) -> &OutboxWorkerConfig {
        &self.config
    }

    /// Drain one batch. Returns per-iteration counters for the caller's
    /// health/metrics reporting.
    pub async fn drain_once(&self, now: DateTime<Utc>) -> Result<DrainStats, OutboxWorkerError> {
        let outbox = OutboxRepository::new(&self.db);
        let rows = outbox.fetch_ready(now, self.config.batch_size).await?;

        let mut stats = DrainStats {
            fetched: rows.len(),
            ..DrainStats::default()
        };

        for row in rows {
            let message = self.render(&row.event).await;
            let started = Instant::now();
            let result = self
                .notifier
                .send(&self.config.chat_id, &message.text)
                .await;
            self.metrics.observe_ms(
                "notifier_send_latency_ms",
                &[("notifier", self.notifier.name())],
                started.elapsed().as_secs_f64() * 1000.0,
            );

            match result {
                Ok(()) => {
                    outbox.mark_delivered(row.id, now).await?;
                    stats.delivered += 1;
                    self.metrics.inc(
                        "notifier_messages_total",
                        &[
                            ("category", message.category.as_str()),
                            ("result", "delivered"),
                        ],
                    );
                    tracing::info!(
                        event = "notification_delivered",
                        service = "notifier_outbox",
                        category = message.category.as_str(),
                        event_kind = %row.event_kind,
                        idempotency_key = %row.idempotency_key,
                        latency_ms = started.elapsed().as_millis() as u64
                    );
                }
                Err(err) => {
                    let attempts_after = row.attempt_count.saturating_add(1);
                    let dead_lettered = attempts_after >= self.config.max_attempts;
                    let detail = format::scrub(&format!("{}: {err}", err.class()));
                    let next_attempt_at = now + self.backoff_delta(attempts_after);

                    outbox
                        .mark_failed(
                            row.id,
                            &detail,
                            next_attempt_at,
                            self.config.max_attempts,
                            now,
                        )
                        .await?;

                    if dead_lettered {
                        stats.dead_lettered += 1;
                        self.metrics.inc(
                            "notifier_dead_lettered_total",
                            &[("category", message.category.as_str())],
                        );
                        tracing::error!(
                            event = "notification_dead_lettered",
                            service = "notifier_outbox",
                            category = message.category.as_str(),
                            event_kind = %row.event_kind,
                            idempotency_key = %row.idempotency_key,
                            attempts = attempts_after,
                            result_class = err.class(),
                            error = %detail
                        );
                    } else {
                        stats.retry_scheduled += 1;
                        tracing::warn!(
                            event = "notification_delivery_failed",
                            service = "notifier_outbox",
                            category = message.category.as_str(),
                            event_kind = %row.event_kind,
                            attempts = attempts_after,
                            result_class = err.class(),
                            error = %detail
                        );
                    }

                    self.metrics.inc(
                        "notifier_messages_total",
                        &[
                            ("category", message.category.as_str()),
                            ("result", err.class()),
                        ],
                    );
                }
            }
        }

        Ok(stats)
    }

    /// Run until cancelled. Storage failures are isolated and retried on the
    /// next tick rather than terminating the worker.
    pub async fn run(&self, cancel: CancellationToken) {
        tracing::info!(event = "worker_started", service = "notifier_outbox");
        loop {
            if cancel.is_cancelled() {
                break;
            }

            tokio::select! {
                _ = cancel.cancelled() => break,
                result = self.drain_once(Utc::now()) => match result {
                    Ok(stats) if !stats.is_idle() => tracing::debug!(
                        event = "outbox_drained",
                        service = "notifier_outbox",
                        fetched = stats.fetched,
                        delivered = stats.delivered,
                        retry_scheduled = stats.retry_scheduled,
                        dead_lettered = stats.dead_lettered
                    ),
                    Ok(_) => {}
                    Err(err) => tracing::error!(
                        event = "outbox_drain_failed",
                        service = "notifier_outbox",
                        error = %err
                    ),
                },
            }

            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }
        tracing::info!(event = "worker_stopped", service = "notifier_outbox");
    }

    /// Render an event, enriching it with voucher details when possible.
    async fn render(&self, event: &DomainEvent) -> RenderedMessage {
        let voucher = if self.config.enrich_from_storage {
            self.load_voucher(event).await
        } else {
            None
        };
        format::render(event, voucher.as_ref())
    }

    /// Enrichment is best-effort: a missing voucher or a read error degrades
    /// the message instead of blocking the notification.
    async fn load_voucher(&self, event: &DomainEvent) -> Option<Voucher> {
        let voucher_id = format::voucher_id_of(event)?;
        match VoucherRepository::new(&self.db).get(voucher_id).await {
            Ok(voucher) => voucher,
            Err(err) => {
                tracing::warn!(
                    event = "notification_enrichment_failed",
                    service = "notifier_outbox",
                    voucher_id = %voucher_id,
                    error = %err
                );
                None
            }
        }
    }

    /// Exponential backoff after `attempts_made` failures, capped.
    fn backoff(&self, attempts_made: i64) -> Duration {
        let exp = attempts_made.clamp(0, 16) as u32;
        self.config
            .base_backoff
            .saturating_mul(2u32.saturating_pow(exp))
            .min(self.config.max_backoff)
    }

    fn backoff_delta(&self, attempts_made: i64) -> chrono::Duration {
        chrono::Duration::from_std(self.backoff(attempts_made))
            .unwrap_or_else(|_| chrono::Duration::seconds(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubNotifier;

    async fn worker(config: OutboxWorkerConfig) -> OutboxNotifierWorker<StubNotifier> {
        // A pool that is never used: these tests only exercise pure helpers.
        let db = Database::connect("sqlite::memory:", 1)
            .await
            .expect("in-memory sqlite");
        OutboxNotifierWorker::new(db, Arc::new(StubNotifier::new()), config).expect("valid config")
    }

    #[tokio::test]
    async fn rejects_invalid_configuration() {
        let db = Database::connect("sqlite::memory:", 1)
            .await
            .expect("in-memory sqlite");
        let notifier = Arc::new(StubNotifier::new());

        let missing_chat =
            OutboxNotifierWorker::new(db.clone(), notifier.clone(), OutboxWorkerConfig::default());
        assert!(matches!(
            missing_chat,
            Err(OutboxWorkerError::Config(NotifierError::Config { .. }))
        ));

        let bad_batch = OutboxNotifierWorker::new(
            db,
            notifier,
            OutboxWorkerConfig {
                chat_id: "chat".into(),
                batch_size: 0,
                ..OutboxWorkerConfig::default()
            },
        );
        assert!(bad_batch.is_err());
    }

    #[tokio::test]
    async fn backoff_doubles_and_caps() {
        let worker = worker(OutboxWorkerConfig {
            chat_id: "chat".into(),
            base_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(60),
            ..OutboxWorkerConfig::default()
        })
        .await;

        assert_eq!(worker.backoff(0), Duration::from_secs(10));
        assert_eq!(worker.backoff(1), Duration::from_secs(20));
        assert_eq!(worker.backoff(2), Duration::from_secs(40));
        assert_eq!(worker.backoff(3), Duration::from_secs(60));
        assert_eq!(worker.backoff(50), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn run_exits_promptly_on_cancellation() {
        let worker = worker(OutboxWorkerConfig {
            chat_id: "chat".into(),
            poll_interval: Duration::from_secs(3600),
            ..OutboxWorkerConfig::default()
        })
        .await;

        let cancel = CancellationToken::new();
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), worker.run(cancel))
            .await
            .expect("cancelled run returns immediately");
    }
}
