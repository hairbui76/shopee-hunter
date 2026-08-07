//! Per-service health registry. Each long-running worker owns a
//! [`HealthHandle`] and reports state transitions; the admin API and readiness
//! checks read consistent snapshots.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServiceState {
    Starting,
    Healthy,
    Degraded,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealth {
    pub state: ServiceState,
    pub detail: Option<String>,
    pub last_heartbeat: DateTime<Utc>,
    pub last_success: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}

impl ServiceHealth {
    fn new() -> Self {
        Self {
            state: ServiceState::Starting,
            detail: None,
            last_heartbeat: Utc::now(),
            last_success: None,
            consecutive_failures: 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HealthRegistry {
    inner: Arc<RwLock<BTreeMap<String, ServiceHealth>>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or re-attach to) a named service.
    pub fn handle(&self, service: &str) -> HealthHandle {
        let mut map = self.inner.write().expect("health registry lock poisoned");
        map.entry(service.to_string())
            .or_insert_with(ServiceHealth::new);
        HealthHandle {
            service: service.to_string(),
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn snapshot(&self) -> BTreeMap<String, ServiceHealth> {
        self.inner
            .read()
            .expect("health registry lock poisoned")
            .clone()
    }

    /// Worst state across all registered services; `Stopped` services are
    /// ignored because a cleanly stopped optional worker is not unhealthy.
    pub fn overall(&self) -> ServiceState {
        self.snapshot()
            .values()
            .filter(|h| h.state != ServiceState::Stopped)
            .map(|h| h.state)
            .max()
            .unwrap_or(ServiceState::Starting)
    }
}

#[derive(Debug, Clone)]
pub struct HealthHandle {
    service: String,
    inner: Arc<RwLock<BTreeMap<String, ServiceHealth>>>,
}

impl HealthHandle {
    pub fn service(&self) -> &str {
        &self.service
    }

    fn update(&self, f: impl FnOnce(&mut ServiceHealth)) {
        let mut map = self.inner.write().expect("health registry lock poisoned");
        let entry = map
            .entry(self.service.clone())
            .or_insert_with(ServiceHealth::new);
        entry.last_heartbeat = Utc::now();
        f(entry);
    }

    pub fn heartbeat(&self) {
        self.update(|_| {});
    }

    pub fn mark_success(&self) {
        self.update(|h| {
            h.state = ServiceState::Healthy;
            h.detail = None;
            h.last_success = Some(Utc::now());
            h.consecutive_failures = 0;
        });
    }

    pub fn mark_degraded(&self, detail: &str) {
        self.update(|h| {
            h.state = ServiceState::Degraded;
            h.detail = Some(detail.to_string());
            h.consecutive_failures = h.consecutive_failures.saturating_add(1);
        });
    }

    pub fn mark_failure(&self, detail: &str) {
        self.update(|h| {
            h.state = ServiceState::Failed;
            h.detail = Some(detail.to_string());
            h.consecutive_failures = h.consecutive_failures.saturating_add(1);
        });
    }

    pub fn mark_stopped(&self) {
        self.update(|h| {
            h.state = ServiceState::Stopped;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_and_overall_state() {
        let registry = HealthRegistry::new();
        let a = registry.handle("collector");
        let b = registry.handle("scheduler");

        assert_eq!(registry.overall(), ServiceState::Starting);

        a.mark_success();
        b.mark_success();
        assert_eq!(registry.overall(), ServiceState::Healthy);

        b.mark_degraded("timeout");
        assert_eq!(registry.overall(), ServiceState::Degraded);
        assert_eq!(registry.snapshot()["scheduler"].consecutive_failures, 1);

        b.mark_failure("boom");
        assert_eq!(registry.overall(), ServiceState::Failed);

        b.mark_stopped();
        assert_eq!(registry.overall(), ServiceState::Healthy);

        a.mark_success();
        assert_eq!(registry.snapshot()["collector"].consecutive_failures, 0);
    }
}
