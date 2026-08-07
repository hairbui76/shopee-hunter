//! Operator control plane (ROADMAP Phases 20-21). Holds runtime toggles the
//! admin API and workers share: a claim pause flag and a session-refresh
//! request signal. Mutations are audited.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use shopee_hunter_session::SessionManager;

/// Shared, cheaply-clonable operator controls.
#[derive(Clone)]
pub struct ControlPlane {
    claims_paused: Arc<AtomicBool>,
    refresh_requested: Arc<AtomicBool>,
    session: SessionManager,
}

impl ControlPlane {
    pub fn new(session: SessionManager) -> Self {
        Self {
            claims_paused: Arc::new(AtomicBool::new(false)),
            refresh_requested: Arc::new(AtomicBool::new(false)),
            session,
        }
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    /// Whether claims are administratively paused (independent of session gate).
    pub fn claims_paused(&self) -> bool {
        self.claims_paused.load(Ordering::SeqCst)
    }

    pub fn pause_claims(&self) {
        self.claims_paused.store(true, Ordering::SeqCst);
        tracing::warn!(event = "admin_claims_paused");
    }

    pub fn resume_claims(&self) {
        self.claims_paused.store(false, Ordering::SeqCst);
        tracing::warn!(event = "admin_claims_resumed");
    }

    /// Request a session refresh; the session worker consumes and clears it.
    pub fn request_session_refresh(&self) {
        self.refresh_requested.store(true, Ordering::SeqCst);
        tracing::info!(event = "admin_session_refresh_requested");
    }

    /// Consume a pending refresh request (returns true if one was set).
    pub fn take_refresh_request(&self) -> bool {
        self.refresh_requested.swap(false, Ordering::SeqCst)
    }

    /// Effective claim permission: session healthy AND not admin-paused.
    pub fn claims_allowed(&self) -> bool {
        self.session.allows_claims() && !self.claims_paused()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shopee_hunter_domain::SessionState;

    #[test]
    fn pause_and_resume_gate_claims() {
        let session = SessionManager::new();
        session.observe(SessionState::Healthy, "ok", Utc::now());
        let control = ControlPlane::new(session);
        assert!(control.claims_allowed());

        control.pause_claims();
        assert!(control.claims_paused());
        assert!(!control.claims_allowed());

        control.resume_claims();
        assert!(control.claims_allowed());
    }

    #[test]
    fn refresh_request_is_one_shot() {
        let control = ControlPlane::new(SessionManager::new());
        assert!(!control.take_refresh_request());
        control.request_session_refresh();
        assert!(control.take_refresh_request());
        assert!(!control.take_refresh_request());
    }

    #[test]
    fn unhealthy_session_blocks_even_when_not_paused() {
        let control = ControlPlane::new(SessionManager::new()); // Unknown state
        assert!(!control.claims_allowed());
    }
}
