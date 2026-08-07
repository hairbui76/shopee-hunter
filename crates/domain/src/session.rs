//! Session health state model. The session manager owns transitions; the
//! claimer consults `blocks_claims`/`allows_claims` as a hard gate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionState {
    Unknown,
    Healthy,
    Degraded,
    Expired,
    LoginRequired,
    VerificationRequired,
    Disabled,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Healthy => "HEALTHY",
            Self::Degraded => "DEGRADED",
            Self::Expired => "EXPIRED",
            Self::LoginRequired => "LOGIN_REQUIRED",
            Self::VerificationRequired => "VERIFICATION_REQUIRED",
            Self::Disabled => "DISABLED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "UNKNOWN" => Self::Unknown,
            "HEALTHY" => Self::Healthy,
            "DEGRADED" => Self::Degraded,
            "EXPIRED" => Self::Expired,
            "LOGIN_REQUIRED" => Self::LoginRequired,
            "VERIFICATION_REQUIRED" => Self::VerificationRequired,
            "DISABLED" => Self::Disabled,
            _ => return None,
        })
    }

    /// Hard refusal states: the claim worker must never send a mutating
    /// request while the session is in one of these.
    pub fn blocks_claims(&self) -> bool {
        matches!(
            self,
            Self::Expired | Self::LoginRequired | Self::VerificationRequired | Self::Disabled
        )
    }

    /// Only a positively verified session allows automatic claiming.
    /// `Degraded`/`Unknown` neither block nor allow — policy defers.
    pub fn allows_claims(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// Whether a manual owner action is needed to recover.
    pub fn needs_manual_action(&self) -> bool {
        matches!(self, Self::LoginRequired | Self::VerificationRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_gates() {
        assert!(SessionState::Healthy.allows_claims());
        for s in [
            SessionState::Expired,
            SessionState::LoginRequired,
            SessionState::VerificationRequired,
            SessionState::Disabled,
        ] {
            assert!(s.blocks_claims());
            assert!(!s.allows_claims());
        }
        // Degraded/Unknown are neither allowed nor hard-blocked: policy defers.
        assert!(!SessionState::Degraded.allows_claims());
        assert!(!SessionState::Degraded.blocks_claims());
        assert!(!SessionState::Unknown.allows_claims());
        assert!(!SessionState::Unknown.blocks_claims());
    }
}
