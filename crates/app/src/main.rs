//! Composition root for the shopee-hunter service.
//!
//! This binary only wires subsystems together: settings, telemetry, storage,
//! workers, and the admin API. Business logic lives in the workspace crates.

use shopee_hunter_app::{api, config, runtime};

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use shopee_hunter_observability::logging;
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

    // Initialize shared services (DB + migrations, HTTP client, metrics, health).
    let services = runtime::Services::initialize(&settings).await?;
    let health = services.health.clone();
    let metrics = services.metrics.clone();

    // Rebuild durable scheduler state before starting workers (restart-safe).
    runtime::reconstruct_scheduler(&settings, &services).await?;

    // Spawn the pipeline workers (collectors → outbox, notifier drain, maintenance).
    let supervisor = runtime::spawn_all(&settings, &services, cancel.clone())?;

    // Admin/health API.
    let admin = if settings.observability.admin_token.is_empty() {
        // Admin read endpoints still work; mutating ones are disabled.
        Some(api::AdminState {
            db: services.db.clone(),
            control: services.control.clone(),
            token: String::new(),
        })
    } else {
        Some(api::AdminState {
            db: services.db.clone(),
            control: services.control.clone(),
            token: settings.observability.admin_token.clone(),
        })
    };
    let api_state = Arc::new(api::ApiState {
        health: health.clone(),
        metrics: metrics.clone(),
        started_at: Utc::now(),
        version: VERSION,
        metrics_enabled: settings.observability.metrics_enabled,
        admin,
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
