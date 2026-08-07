//! Long-running worker supervisor implementing the AGENTS.md loop standard:
//! named identity, startup/shutdown logs, cancellation, failure isolation,
//! bounded backoff, jittered intervals, heartbeat, and metrics.

use std::future::Future;
use std::time::Duration;

use thiserror::Error;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::health::HealthHandle;
use crate::metrics::Metrics;

/// Error returned by one worker iteration. Transient errors degrade health and
/// back off; fatal errors mark the service failed and back off at maximum.
#[derive(Debug, Error)]
pub enum IterationError {
    #[error("transient: {0}")]
    Transient(String),
    #[error("fatal: {0}")]
    Fatal(String),
}

impl IterationError {
    pub fn transient(msg: impl Into<String>) -> Self {
        Self::Transient(msg.into())
    }

    pub fn fatal(msg: impl Into<String>) -> Self {
        Self::Fatal(msg.into())
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub name: String,
    /// Delay between successful iterations.
    pub interval: Duration,
    /// Fraction of the interval used as random jitter (0.0..=1.0).
    pub jitter: f64,
    /// First backoff delay after a failure.
    pub backoff_base: Duration,
    /// Backoff ceiling.
    pub backoff_max: Duration,
}

impl WorkerConfig {
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self {
            name: name.into(),
            interval,
            jitter: 0.1,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(60),
        }
    }

    fn delay_after(&self, consecutive_failures: u32) -> Duration {
        let base = if consecutive_failures == 0 {
            self.interval
        } else {
            let exp = consecutive_failures.saturating_sub(1).min(16);
            self.backoff_base
                .saturating_mul(2u32.saturating_pow(exp))
                .min(self.backoff_max)
        };
        apply_jitter(base, self.jitter)
    }
}

fn apply_jitter(base: Duration, jitter: f64) -> Duration {
    if jitter <= 0.0 {
        return base;
    }
    let spread = base.as_secs_f64() * jitter.clamp(0.0, 1.0);
    let offset = (rand::random::<f64>() * 2.0 - 1.0) * spread;
    Duration::from_secs_f64((base.as_secs_f64() + offset).max(0.0))
}

/// Owns spawned worker tasks and the shared cancellation token.
pub struct WorkerSupervisor {
    cancel: CancellationToken,
    tasks: JoinSet<()>,
    metrics: Metrics,
}

impl WorkerSupervisor {
    pub fn new(cancel: CancellationToken, metrics: Metrics) -> Self {
        Self {
            cancel,
            tasks: JoinSet::new(),
            metrics,
        }
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Spawn a supervised iteration loop. `iteration` is called repeatedly;
    /// its errors are isolated, classified, and backed off. The loop exits
    /// when the shared token is cancelled.
    pub fn supervise<F, Fut>(
        &mut self,
        config: WorkerConfig,
        health: HealthHandle,
        mut iteration: F,
    ) where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), IterationError>> + Send,
    {
        let cancel = self.cancel.clone();
        let metrics = self.metrics.clone();
        self.tasks.spawn(async move {
            let service = config.name.clone();
            tracing::info!(event = "worker_started", service);
            let mut consecutive_failures: u32 = 0;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = iteration() => {
                        health.heartbeat();
                        match result {
                            Ok(()) => {
                                consecutive_failures = 0;
                                health.mark_success();
                                metrics.inc("worker_iterations_total", &[("service", &service), ("result", "ok")]);
                            }
                            Err(err) if err.is_transient() => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                health.mark_degraded(&err.to_string());
                                metrics.inc("worker_iterations_total", &[("service", &service), ("result", "transient_error")]);
                                tracing::warn!(event = "worker_iteration_degraded", service, error = %err);
                            }
                            Err(err) => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                health.mark_failure(&err.to_string());
                                metrics.inc("worker_iterations_total", &[("service", &service), ("result", "fatal_error")]);
                                tracing::error!(event = "worker_iteration_failed", service, error = %err);
                            }
                        }
                    }
                }

                let delay = config.delay_after(consecutive_failures);
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            health.mark_stopped();
            tracing::info!(event = "worker_stopped", service);
        });
    }

    /// Cancel all workers and wait for them to finish, up to `timeout`.
    pub async fn shutdown(mut self, timeout: Duration) {
        self.cancel.cancel();
        let drain = async { while self.tasks.join_next().await.is_some() {} };
        if tokio::time::timeout(timeout, drain).await.is_err() {
            tracing::warn!(
                event = "worker_shutdown_timeout",
                timeout_ms = timeout.as_millis() as u64
            );
            self.tasks.abort_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{HealthRegistry, ServiceState};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn test_config(name: &str) -> WorkerConfig {
        WorkerConfig {
            name: name.into(),
            interval: Duration::from_millis(5),
            jitter: 0.0,
            backoff_base: Duration::from_millis(5),
            backoff_max: Duration::from_millis(20),
        }
    }

    #[tokio::test]
    async fn worker_runs_and_stops_on_cancel() {
        let registry = HealthRegistry::new();
        let metrics = Metrics::new();
        let mut supervisor = WorkerSupervisor::new(CancellationToken::new(), metrics.clone());
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);

        supervisor.supervise(test_config("dummy"), registry.handle("dummy"), move || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        supervisor.shutdown(Duration::from_secs(1)).await;

        assert!(count.load(Ordering::SeqCst) >= 2);
        assert_eq!(registry.snapshot()["dummy"].state, ServiceState::Stopped);
    }

    #[tokio::test]
    async fn failing_worker_is_visible_in_health_and_isolated() {
        let registry = HealthRegistry::new();
        let metrics = Metrics::new();
        let mut supervisor = WorkerSupervisor::new(CancellationToken::new(), metrics.clone());

        supervisor.supervise(test_config("bad"), registry.handle("bad"), move || async {
            Err(IterationError::fatal("boom"))
        });
        supervisor.supervise(
            test_config("good"),
            registry.handle("good"),
            move || async { Ok(()) },
        );

        tokio::time::sleep(Duration::from_millis(60)).await;

        let snap = registry.snapshot();
        assert_eq!(snap["bad"].state, ServiceState::Failed);
        assert!(snap["bad"].consecutive_failures >= 1);
        assert_eq!(snap["good"].state, ServiceState::Healthy);

        supervisor.shutdown(Duration::from_secs(1)).await;
    }

    #[test]
    fn backoff_grows_and_caps() {
        let config = WorkerConfig {
            jitter: 0.0,
            ..test_config("x")
        };
        assert_eq!(config.delay_after(0), Duration::from_millis(5));
        assert_eq!(config.delay_after(1), Duration::from_millis(5));
        assert_eq!(config.delay_after(2), Duration::from_millis(10));
        assert_eq!(config.delay_after(3), Duration::from_millis(20));
        assert_eq!(config.delay_after(10), Duration::from_millis(20));
    }
}
