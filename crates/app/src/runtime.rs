//! Composition of the running service. Builds shared state and spawns the
//! supervised workers that make up the discovery → alert → schedule → claim
//! pipeline. Business logic lives in the workspace crates; this module only
//! wires them together.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use shopee_hunter_collectors::{
    CollectorRegistry, CollectorSupervisor, ExternalFeedCollector, ReplayCollector,
    SupervisedSource,
};
use shopee_hunter_notifier::{
    Notifier, OutboxNotifierWorker, OutboxWorkerConfig, StubNotifier, TelegramNotifier,
};
use shopee_hunter_observability::{
    HealthRegistry, IterationError, Metrics, WorkerConfig, WorkerSupervisor,
};
use shopee_hunter_session::SessionManager;
use shopee_hunter_storage::{Database, OutboxRepository};
use tokio_util::sync::CancellationToken;

use crate::config::Settings;
use crate::control::ControlPlane;

/// Shared, cheaply-clonable service state.
#[derive(Clone)]
pub struct Services {
    pub db: Database,
    pub metrics: Metrics,
    pub health: HealthRegistry,
    pub http: reqwest::Client,
    pub session: SessionManager,
    pub control: ControlPlane,
}

impl Services {
    pub async fn initialize(settings: &Settings) -> anyhow::Result<Self> {
        let db =
            Database::connect(&settings.database.url, settings.database.max_connections).await?;
        // One process-long HTTP client, reused by every collector.
        let http = reqwest::Client::builder()
            .connect_timeout(settings.shopee.connect_timeout)
            .pool_idle_timeout(None)
            .build()?;
        let session = SessionManager::new();
        let control = ControlPlane::new(session.clone());
        Ok(Self {
            db,
            metrics: Metrics::new(),
            health: HealthRegistry::new(),
            http,
            session,
            control,
        })
    }
}

/// Build the enabled collector registry from configuration.
pub fn build_collectors(settings: &Settings, http: reqwest::Client) -> CollectorRegistry {
    let mut registry = CollectorRegistry::new();

    if settings.collectors.enable_external_feed {
        if let Some(url) = &settings.collectors.external_feed_url {
            registry.register(Arc::new(ExternalFeedCollector::new(
                "external-feed",
                url.clone(),
                http.clone(),
                settings.collectors.timeout,
            )));
        }
    }
    if settings.collectors.enable_replay {
        registry.register(Arc::new(ReplayCollector::from_dir(
            "replay",
            &settings.collectors.replay_fixture_dir,
        )));
    }
    registry
}

/// Spawn one supervised worker per enabled collector. Each iteration collects,
/// ingests through the shared pipeline, and enqueues the resulting domain
/// events into the notification outbox — the latency-critical discovery→alert
/// path, with no blocking work between ingest and enqueue.
pub fn spawn_collectors(
    supervisor: &mut WorkerSupervisor,
    services: &Services,
    registry: &CollectorRegistry,
    interval: Duration,
    timeout: Duration,
) {
    for collector in registry.all() {
        let name = collector.name().to_string();
        let source = Arc::new(SupervisedSource::new(collector, timeout));
        let db = services.db.clone();
        let metrics = services.metrics.clone();
        let health = services.health.handle(&format!("collector:{name}"));

        supervisor.supervise(
            WorkerConfig::new(format!("collector:{name}"), interval),
            health,
            move || {
                let source = Arc::clone(&source);
                let db = db.clone();
                let metrics = metrics.clone();
                async move {
                    let collector_supervisor = CollectorSupervisor::new(db.clone(), metrics);
                    let outcome = collector_supervisor
                        .run_once(&source)
                        .await
                        .map_err(|e| IterationError::transient(e.to_string()))?;

                    // Enqueue discovery/update events for durable delivery.
                    let outbox = OutboxRepository::new(&db);
                    let now = Utc::now();
                    for event in &outcome.events {
                        outbox
                            .enqueue(event, now)
                            .await
                            .map_err(|e| IterationError::transient(e.to_string()))?;
                    }
                    Ok(())
                }
            },
        );
    }
}

/// Build the notifier (Telegram if enabled, else a Stub for dry-run/dev) and
/// spawn the outbox-drain worker so alerts are delivered durably.
pub fn spawn_notifier(
    supervisor: &mut WorkerSupervisor,
    services: &Services,
    settings: &Settings,
) -> anyhow::Result<()> {
    let notifier: Arc<dyn Notifier> = if settings.telegram.enabled {
        Arc::new(TelegramNotifier::new(settings.telegram.bot_token.clone())?)
    } else {
        tracing::warn!(
            event = "telegram_disabled",
            "using stub notifier (no messages sent)"
        );
        Arc::new(StubNotifier::new())
    };

    let chat_id = if settings.telegram.chat_id.is_empty() {
        "0".to_string() // stub path: chat id is unused but must be non-empty
    } else {
        settings.telegram.chat_id.clone()
    };

    let config = OutboxWorkerConfig {
        chat_id,
        ..OutboxWorkerConfig::default()
    };
    let worker = Arc::new(
        OutboxNotifierWorker::new(services.db.clone(), notifier, config)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    );

    let health = services.health.handle("notifier");
    supervisor.supervise(
        WorkerConfig::new("notifier", Duration::from_secs(5)),
        health,
        move || {
            let worker = Arc::clone(&worker);
            async move {
                worker
                    .drain_once(Utc::now())
                    .await
                    .map(|_| ())
                    .map_err(|e| IterationError::transient(e.to_string()))
            }
        },
    );
    Ok(())
}

/// Spawn all pipeline workers. Returns the supervisor so the caller owns
/// shutdown.
pub fn spawn_all(
    settings: &Settings,
    services: &Services,
    cancel: CancellationToken,
) -> anyhow::Result<WorkerSupervisor> {
    let mut supervisor = WorkerSupervisor::new(cancel, services.metrics.clone());

    let registry = build_collectors(settings, services.http.clone());
    if registry.is_empty() {
        tracing::warn!(event = "no_collectors_enabled");
    }
    spawn_collectors(
        &mut supervisor,
        services,
        &registry,
        settings.collectors.default_interval,
        settings.collectors.timeout,
    );
    spawn_notifier(&mut supervisor, services, settings)?;

    Ok(supervisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use std::collections::HashMap;

    fn settings(map: HashMap<String, String>) -> Settings {
        Settings::from_lookup(&|k| map.get(k).cloned()).unwrap()
    }

    #[tokio::test]
    async fn builds_services_and_enabled_collectors() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("rt.db").display());
        let env = HashMap::from([
            ("DATABASE_URL".to_string(), url),
            (
                "ENABLE_EXTERNAL_FEED_COLLECTOR".to_string(),
                "true".to_string(),
            ),
            (
                "EXTERNAL_FEED_URL".to_string(),
                "https://example.test/feed".to_string(),
            ),
        ]);
        let s = settings(env);
        let services = Services::initialize(&s).await.unwrap();
        let registry = build_collectors(&s, services.http.clone());
        assert_eq!(registry.len(), 1);
        assert!(registry.get("external-feed").is_some());
    }

    #[tokio::test]
    async fn spawn_all_runs_and_shuts_down_with_stub_notifier() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("rt2.db").display());
        let s = settings(HashMap::from([("DATABASE_URL".to_string(), url)]));
        let services = Services::initialize(&s).await.unwrap();
        let cancel = CancellationToken::new();
        let supervisor = spawn_all(&s, &services, cancel.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        supervisor.shutdown(Duration::from_secs(2)).await;
        // Notifier worker registered and reached a terminal (stopped) state.
        assert!(services.health.snapshot().contains_key("notifier"));
    }
}
