//! Session state manager: single owner of authentication lifecycle. Publishes
//! a claim gate so the claim worker is paused whenever the session is not
//! positively healthy.

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::Serialize;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::SessionState;
use tokio::sync::watch;

/// Immutable snapshot of session status (ARCHITECTURE.md §7.5). Operational
/// metadata only — never a credential store.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub state: SessionState,
    pub checked_at: Option<DateTime<Utc>>,
    pub last_healthy_at: Option<DateTime<Utc>>,
    pub reason_code: Option<String>,
    pub browser_profile_version: u64,
}

impl SessionSnapshot {
    fn initial() -> Self {
        Self {
            state: SessionState::Unknown,
            checked_at: None,
            last_healthy_at: None,
            reason_code: None,
            browser_profile_version: 0,
        }
    }
}

/// A cheaply-clonable read handle the claim worker consults before mutating.
#[derive(Clone)]
pub struct ClaimGate {
    rx: watch::Receiver<bool>,
}

impl ClaimGate {
    /// Whether claims are currently permitted (session positively healthy).
    pub fn is_open(&self) -> bool {
        *self.rx.borrow()
    }

    /// Await the next change to the gate (for reactive pause/resume).
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.rx.changed().await
    }
}

/// Single owner of session authentication lifecycle.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<RwLock<SessionSnapshot>>,
    gate_tx: Arc<watch::Sender<bool>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        let (gate_tx, _rx) = watch::channel(false);
        Self {
            inner: Arc::new(RwLock::new(SessionSnapshot::initial())),
            gate_tx: Arc::new(gate_tx),
        }
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        self.inner.read().expect("session lock").clone()
    }

    pub fn state(&self) -> SessionState {
        self.inner.read().expect("session lock").state
    }

    /// Whether claims are permitted right now (positively healthy only).
    pub fn allows_claims(&self) -> bool {
        self.state().allows_claims()
    }

    /// A claim gate that reflects the healthy/paused transitions.
    pub fn claim_gate(&self) -> ClaimGate {
        ClaimGate {
            rx: self.gate_tx.subscribe(),
        }
    }

    /// Record a freshly observed state. Returns a `SessionStateChanged` event
    /// when the state actually transitioned (for the outbox), else `None`.
    pub fn observe(
        &self,
        new_state: SessionState,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Option<DomainEvent> {
        let reason = reason.into();
        let mut guard = self.inner.write().expect("session lock");
        let previous = guard.state;
        guard.checked_at = Some(now);
        if new_state == SessionState::Healthy {
            guard.last_healthy_at = Some(now);
        }

        if previous == new_state {
            // Update reason without emitting a duplicate transition event.
            if new_state != SessionState::Healthy {
                guard.reason_code = Some(reason);
            }
            return None;
        }

        guard.state = new_state;
        guard.reason_code = if new_state == SessionState::Healthy {
            None
        } else {
            Some(reason.clone())
        };
        drop(guard);

        // Republish the claim gate. `send_replace` updates the stored value
        // even when no receiver is currently subscribed (gates created later
        // must still observe the latest state).
        let open = new_state.allows_claims();
        self.gate_tx.send_replace(open);

        tracing::info!(
            event = "session_state_changed",
            from = previous.as_str(),
            to = new_state.as_str(),
            claims_open = open,
        );

        Some(DomainEvent::SessionStateChanged {
            from: previous,
            to: new_state,
            reason,
        })
    }

    /// Mark the session disabled (owner action / hard stop).
    pub fn disable(&self, reason: impl Into<String>, now: DateTime<Utc>) -> Option<DomainEvent> {
        self.observe(SessionState::Disabled, reason, now)
    }

    /// Increment the browser profile version after a successful re-bootstrap.
    pub fn bump_profile_version(&self) {
        let mut guard = self.inner.write().expect("session lock");
        guard.browser_profile_version = guard.browser_profile_version.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_opens_gate_and_transitions_emit_events() {
        let mgr = SessionManager::new();
        let gate = mgr.claim_gate();
        assert!(!gate.is_open());
        assert_eq!(mgr.state(), SessionState::Unknown);

        let e = mgr.observe(SessionState::Healthy, "ok", Utc::now());
        assert!(e.is_some());
        assert!(gate.is_open());
        assert!(mgr.allows_claims());
        assert!(mgr.snapshot().last_healthy_at.is_some());

        // Same state again → no event, gate unchanged.
        assert!(mgr
            .observe(SessionState::Healthy, "ok", Utc::now())
            .is_none());
        assert!(gate.is_open());
    }

    #[test]
    fn expiry_pauses_claims_and_records_reason() {
        let mgr = SessionManager::new();
        mgr.observe(SessionState::Healthy, "ok", Utc::now());
        let gate = mgr.claim_gate();
        assert!(gate.is_open());

        let e = mgr.observe(SessionState::Expired, "cookie expired", Utc::now());
        match e {
            Some(DomainEvent::SessionStateChanged { from, to, reason }) => {
                assert_eq!(from, SessionState::Healthy);
                assert_eq!(to, SessionState::Expired);
                assert_eq!(reason, "cookie expired");
            }
            other => panic!("expected transition event, got {other:?}"),
        }
        assert!(!gate.is_open());
        assert!(!mgr.allows_claims());
        assert_eq!(
            mgr.snapshot().reason_code.as_deref(),
            Some("cookie expired")
        );
    }

    #[test]
    fn verification_and_disabled_block_claims() {
        let mgr = SessionManager::new();
        mgr.observe(SessionState::VerificationRequired, "captcha", Utc::now());
        assert!(!mgr.allows_claims());
        assert!(mgr.state().needs_manual_action());

        mgr.disable("owner stop", Utc::now());
        assert_eq!(mgr.state(), SessionState::Disabled);
        assert!(!mgr.claim_gate().is_open());
    }
}
