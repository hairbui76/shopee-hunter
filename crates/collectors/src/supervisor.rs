//! Collector supervisor: schedules each source independently, enforces a
//! per-source timeout, isolates failures so one bad source cannot interrupt
//! another, tracks per-source health, and records collector runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::{CollectorRunRecord, CollectorRunRepository, Database, RunOutcome};

use crate::contract::{
    CollectionContext, CollectorError, SharedSourceHealth, SourceHealth, VoucherCollector,
};
use crate::pipeline::{ingest_candidates, PipelineOutcome};

/// A collector plus its runtime state (health, timeout).
pub struct SupervisedSource {
    pub collector: Arc<dyn VoucherCollector>,
    pub timeout: Duration,
    pub health: SharedSourceHealth,
}

impl SupervisedSource {
    pub fn new(collector: Arc<dyn VoucherCollector>, timeout: Duration) -> Self {
        Self {
            collector,
            timeout,
            health: SharedSourceHealth::default(),
        }
    }

    pub fn health(&self) -> SourceHealth {
        self.health.snapshot()
    }
}

/// Runs one collection cycle for a single source with failure isolation.
pub struct CollectorSupervisor {
    db: Database,
    metrics: Metrics,
}

impl CollectorSupervisor {
    pub fn new(db: Database, metrics: Metrics) -> Self {
        Self { db, metrics }
    }

    /// Execute one collection + ingest cycle for a source. Returns the pipeline
    /// outcome (events to enqueue). Never panics on collector failure; records
    /// health and a collector run either way.
    pub async fn run_once(
        &self,
        source: &SupervisedSource,
    ) -> Result<PipelineOutcome, CollectorError> {
        let name = source.collector.name().to_string();
        let now = Utc::now();
        let started = Instant::now();
        let context = CollectionContext {
            now,
            deadline: now + chrono::Duration::from_std(source.timeout).unwrap_or_default(),
        };

        self.metrics
            .inc("collector_runs_total", &[("source", &name)]);

        let collect_result =
            tokio::time::timeout(source.timeout, source.collector.collect(&context)).await;

        let result = match collect_result {
            Err(_) => Err(CollectorError::Timeout),
            Ok(inner) => inner,
        };

        let latency = started.elapsed();

        match result {
            Ok(collection) => {
                let outcome = match ingest_candidates(&self.db, &collection.candidates, now).await {
                    Ok(outcome) => outcome,
                    Err(e) => {
                        // A persistence failure must mark the source unhealthy —
                        // otherwise a DB outage leaves source health reading
                        // Healthy with a stale last_success (Phase 26 finding).
                        let err = CollectorError::Config(format!("persistence error: {e}"));
                        source.health.record_failure(now, &err);
                        self.metrics
                            .inc("collector_failures_total", &[("source", &name)]);
                        return Err(err);
                    }
                };

                let partial = collection.is_partial() || !outcome.rejected.is_empty();
                source.health.record_success(
                    now,
                    collection.source_latency.or(Some(latency)),
                    collection.candidates.len(),
                    partial,
                );

                self.metrics.add(
                    "collector_candidates_total",
                    &[("source", &name)],
                    collection.candidates.len() as u64,
                );
                self.metrics.add(
                    "collector_new_total",
                    &[("source", &name)],
                    outcome.new_count as u64,
                );
                self.metrics.add(
                    "collector_updated_total",
                    &[("source", &name)],
                    outcome.updated_count as u64,
                );
                self.metrics.observe_ms(
                    "collector_fetch_latency_ms",
                    &[("source", &name)],
                    latency.as_secs_f64() * 1000.0,
                );

                self.record_run(
                    &name,
                    now,
                    latency,
                    &collection,
                    &outcome,
                    if partial {
                        RunOutcome::Partial
                    } else {
                        RunOutcome::Success
                    },
                )
                .await;

                Ok(outcome)
            }
            Err(err) => {
                source.health.record_failure(now, &err);
                self.metrics
                    .inc("collector_failures_total", &[("source", &name)]);
                let mut run = CollectorRunRecord {
                    source: name.clone(),
                    started_at: now,
                    finished_at: Some(Utc::now()),
                    latency_ms: Some(latency.as_millis() as i64),
                    detail: Some(err.to_string()),
                    ..Default::default()
                };
                run.parse_errors = matches!(err, CollectorError::Malformed(_)) as i64;
                let _ = CollectorRunRepository::new(&self.db)
                    .record(&run, RunOutcome::Failed)
                    .await;
                Err(err)
            }
        }
    }

    async fn record_run(
        &self,
        name: &str,
        now: chrono::DateTime<Utc>,
        latency: Duration,
        collection: &crate::contract::CollectionResult,
        outcome: &PipelineOutcome,
        run_outcome: RunOutcome,
    ) {
        let run = CollectorRunRecord {
            source: name.to_string(),
            started_at: now,
            finished_at: Some(Utc::now()),
            latency_ms: Some(latency.as_millis() as i64),
            candidate_count: collection.candidates.len() as i64,
            new_count: outcome.new_count as i64,
            updated_count: outcome.updated_count as i64,
            parse_errors: (collection.partial_failures.len() + outcome.rejected.len()) as i64,
            detail: None,
        };
        let _ = CollectorRunRepository::new(&self.db)
            .record(&run, run_outcome)
            .await;
    }
}
