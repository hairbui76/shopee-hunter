//! Composition root for the shopee-hunter service.
//!
//! This binary only wires subsystems together: settings, telemetry, storage,
//! workers, and the admin API. Business logic lives in the workspace crates.

use shopee_hunter_app::{api, config};

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use shopee_hunter_observability::{
    logging, HealthRegistry, Metrics, WorkerConfig, WorkerSupervisor,
};
use tokio_util::sync::CancellationToken;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);

fn main() -> anyhow::Result<()> {
    let settings = config::Settings::from_env().context("loading configuration")?;
    logging::init(&settings.app.log_level, settings.app.log_format.parse()?)
        .context("initializing logging")?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building Tokio runtime")?;
    runtime.block_on(run(settings))
}

async fn run(settings: config::Settings) -> anyhow::Result<()> {
    tracing::info!(
        event = "service_starting",
        service = "shopee-hunter",
        version = VERSION,
        env = ?settings.app.env,
    );

    let cancel = CancellationToken::new();
    let health = HealthRegistry::new();
    let metrics = Metrics::new();
    let mut supervisor = WorkerSupervisor::new(cancel.clone(), metrics.clone());

    // Application heartbeat worker: proves supervision/health wiring end to end.
    let heartbeat_health = health.handle("app-heartbeat");
    let heartbeat_metrics = metrics.clone();
    supervisor.supervise(
        WorkerConfig::new("app-heartbeat", Duration::from_secs(30)),
        heartbeat_health,
        move || {
            let metrics = heartbeat_metrics.clone();
            async move {
                metrics.inc("app_heartbeats_total", &[]);
                Ok(())
            }
        },
    );

    // Admin/health API.
    let api_state = Arc::new(api::ApiState {
        health: health.clone(),
        metrics: metrics.clone(),
        started_at: Utc::now(),
        version: VERSION,
        metrics_enabled: settings.observability.metrics_enabled,
    });
    let api_cancel = cancel.clone();
    let api_bind = settings.observability.healthcheck_bind;
    let api_task = tokio::spawn(api::serve(api_bind, api_state, api_cancel));

    // Wait for a shutdown signal.
    shutdown_signal(&cancel).await;
    tracing::info!(event = "service_stopping");

    supervisor.shutdown(SHUTDOWN_GRACE).await;
    match tokio::time::timeout(SHUTDOWN_GRACE, api_task).await {
        Ok(joined) => joined??,
        Err(_) => tracing::warn!(event = "api_shutdown_timeout"),
    }

    tracing::info!(event = "service_stopped");
    Ok(())
}

/// Resolve on SIGINT/SIGTERM (or token cancellation from an internal fatal
/// condition) and cancel the shared token.
async fn shutdown_signal(cancel: &CancellationToken) {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler is a static invariant");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!(event = "signal_received", signal = "SIGINT"),
        _ = sigterm.recv() => tracing::info!(event = "signal_received", signal = "SIGTERM"),
        _ = cancel.cancelled() => {}
    }
    cancel.cancel();
}
