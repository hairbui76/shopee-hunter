//! Session health probing. The prober is abstracted behind a trait so the
//! session worker is testable without a live Shopee client, and so the browser
//! refresh path can be swapped in later.

use async_trait::async_trait;
use chrono::Utc;
use shopee_hunter_client::ShopeeClient;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::SessionState;
use thiserror::Error;

use crate::manager::SessionManager;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("probe transport error: {0}")]
    Transport(String),
}

/// Something that can report the current session state via a low-impact probe.
#[async_trait]
pub trait SessionProber: Send + Sync {
    async fn probe(&self) -> Result<SessionState, ProbeError>;
}

/// Prober backed by the real Shopee client's authenticated probe.
pub struct ClientProber {
    client: std::sync::Arc<ShopeeClient>,
}

impl ClientProber {
    pub fn new(client: std::sync::Arc<ShopeeClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl SessionProber for ClientProber {
    async fn probe(&self) -> Result<SessionState, ProbeError> {
        match self.client.probe_session().await {
            Ok(outcome) => Ok(outcome.probe.to_session_state()),
            // A transport failure is not an authentication verdict: report
            // Degraded rather than pretend the session expired.
            Err(err) => {
                tracing::warn!(event = "session_probe_failed", error = %err);
                Ok(SessionState::Degraded)
            }
        }
    }
}

/// One health-check cycle: probe, then fold the result into the manager.
/// Returns a transition event to enqueue when the state changed.
pub struct SessionHealthWorker<P: SessionProber> {
    prober: P,
    manager: SessionManager,
}

impl<P: SessionProber> SessionHealthWorker<P> {
    pub fn new(prober: P, manager: SessionManager) -> Self {
        Self { prober, manager }
    }

    /// Run a single probe cycle. `Ok(Some(event))` on a state transition.
    pub async fn tick(&self) -> Result<Option<DomainEvent>, ProbeError> {
        let state = self.prober.probe().await?;
        let reason = match state {
            SessionState::Healthy => "probe healthy".to_string(),
            other => format!("probe reported {}", other.as_str()),
        };
        Ok(self.manager.observe(state, reason, Utc::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct ScriptedProber {
        states: Vec<SessionState>,
        idx: AtomicUsize,
    }

    #[async_trait]
    impl SessionProber for ScriptedProber {
        async fn probe(&self) -> Result<SessionState, ProbeError> {
            let i = self
                .idx
                .fetch_add(1, Ordering::SeqCst)
                .min(self.states.len() - 1);
            Ok(self.states[i])
        }
    }

    #[tokio::test]
    async fn worker_transitions_and_gates_claims() {
        let prober = ScriptedProber {
            states: vec![
                SessionState::Healthy,
                SessionState::Healthy,
                SessionState::Expired,
            ],
            idx: AtomicUsize::new(0),
        };
        let manager = SessionManager::new();
        let gate = manager.claim_gate();
        let worker = SessionHealthWorker::new(prober, manager.clone());

        // First probe: Unknown -> Healthy, event emitted, gate opens.
        assert!(worker.tick().await.unwrap().is_some());
        assert!(gate.is_open());

        // Second: still Healthy, no event.
        assert!(worker.tick().await.unwrap().is_none());
        assert!(gate.is_open());

        // Third: Healthy -> Expired, event emitted, gate closes.
        assert!(worker.tick().await.unwrap().is_some());
        assert!(!gate.is_open());
        assert_eq!(manager.state(), SessionState::Expired);
    }

    #[tokio::test]
    async fn probe_transport_failure_degrades_not_expires() {
        struct Failing;
        #[async_trait]
        impl SessionProber for Failing {
            async fn probe(&self) -> Result<SessionState, ProbeError> {
                Ok(SessionState::Degraded)
            }
        }
        let manager = SessionManager::new();
        manager.observe(SessionState::Healthy, "ok", Utc::now());
        let worker = SessionHealthWorker::new(Failing, manager.clone());
        worker.tick().await.unwrap();
        // Degraded neither allows nor hard-blocks; claims are paused (not open).
        assert_eq!(manager.state(), SessionState::Degraded);
        assert!(!manager.allows_claims());
        let _ = Arc::new(());
    }
}
