//! Claim result classification and policy decision types.

use serde::{Deserialize, Serialize};

/// Finite classification of every Shopee save/claim response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimResultClass {
    Success,
    AlreadySaved,
    NotActive,
    Expired,
    Exhausted,
    Ineligible,
    InvalidVoucher,
    SessionExpired,
    VerificationRequired,
    RateLimited,
    TransientFailure,
    UnknownResponse,
}

/// What the claimer may do next for a given result class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RetryClass {
    /// Terminal — never retry.
    NoRetry,
    /// Retry after capped exponential backoff.
    RetryAfterBackoff,
    /// Obey upstream rate-limit hints, then delayed retry.
    RetryAfterRateLimit,
    /// Reschedule at/after the voucher start time if it is trustworthy.
    RescheduleAtStart,
    /// Stop claiming and let the session manager recover first.
    PauseSession,
    /// Small diagnostic budget, then stop and alert.
    DiagnosticBudget,
}

impl ClaimResultClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::AlreadySaved => "ALREADY_SAVED",
            Self::NotActive => "NOT_ACTIVE",
            Self::Expired => "EXPIRED",
            Self::Exhausted => "EXHAUSTED",
            Self::Ineligible => "INELIGIBLE",
            Self::InvalidVoucher => "INVALID_VOUCHER",
            Self::SessionExpired => "SESSION_EXPIRED",
            Self::VerificationRequired => "VERIFICATION_REQUIRED",
            Self::RateLimited => "RATE_LIMITED",
            Self::TransientFailure => "TRANSIENT_FAILURE",
            Self::UnknownResponse => "UNKNOWN_RESPONSE",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "SUCCESS" => Self::Success,
            "ALREADY_SAVED" => Self::AlreadySaved,
            "NOT_ACTIVE" => Self::NotActive,
            "EXPIRED" => Self::Expired,
            "EXHAUSTED" => Self::Exhausted,
            "INELIGIBLE" => Self::Ineligible,
            "INVALID_VOUCHER" => Self::InvalidVoucher,
            "SESSION_EXPIRED" => Self::SessionExpired,
            "VERIFICATION_REQUIRED" => Self::VerificationRequired,
            "RATE_LIMITED" => Self::RateLimited,
            "TRANSIENT_FAILURE" => Self::TransientFailure,
            "UNKNOWN_RESPONSE" => Self::UnknownResponse,
            _ => return None,
        })
    }

    /// Success or success-equivalent (voucher is in the account either way).
    pub fn is_success_equivalent(&self) -> bool {
        matches!(self, Self::Success | Self::AlreadySaved)
    }

    /// Terminal for this voucher/account: no further attempts make sense.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Success
                | Self::AlreadySaved
                | Self::Expired
                | Self::Exhausted
                | Self::Ineligible
                | Self::InvalidVoucher
        )
    }

    pub fn retry_class(&self) -> RetryClass {
        match self {
            Self::Success
            | Self::AlreadySaved
            | Self::Expired
            | Self::Exhausted
            | Self::Ineligible
            | Self::InvalidVoucher => RetryClass::NoRetry,
            Self::NotActive => RetryClass::RescheduleAtStart,
            Self::SessionExpired | Self::VerificationRequired => RetryClass::PauseSession,
            Self::RateLimited => RetryClass::RetryAfterRateLimit,
            Self::TransientFailure => RetryClass::RetryAfterBackoff,
            Self::UnknownResponse => RetryClass::DiagnosticBudget,
        }
    }
}

/// Output of the claim policy engine, always with explainable reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimDecision {
    Allow {
        reasons: Vec<String>,
    },
    Deny {
        reasons: Vec<String>,
    },
    Defer {
        reasons: Vec<String>,
        until_hint: Option<chrono::DateTime<chrono::Utc>>,
    },
    ManualReview {
        reasons: Vec<String>,
    },
}

impl ClaimDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    pub fn reasons(&self) -> &[String] {
        match self {
            Self::Allow { reasons }
            | Self::Deny { reasons }
            | Self::Defer { reasons, .. }
            | Self::ManualReview { reasons } => reasons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminality_and_retry_classes_are_consistent() {
        for class in [
            ClaimResultClass::Success,
            ClaimResultClass::AlreadySaved,
            ClaimResultClass::Expired,
            ClaimResultClass::Exhausted,
            ClaimResultClass::Ineligible,
            ClaimResultClass::InvalidVoucher,
        ] {
            assert!(class.is_terminal());
            assert_eq!(class.retry_class(), RetryClass::NoRetry);
        }
        assert!(!ClaimResultClass::TransientFailure.is_terminal());
        assert_eq!(
            ClaimResultClass::NotActive.retry_class(),
            RetryClass::RescheduleAtStart
        );
        assert_eq!(
            ClaimResultClass::SessionExpired.retry_class(),
            RetryClass::PauseSession
        );
        assert_eq!(
            ClaimResultClass::UnknownResponse.retry_class(),
            RetryClass::DiagnosticBudget
        );
    }

    #[test]
    fn already_saved_is_success_equivalent() {
        assert!(ClaimResultClass::AlreadySaved.is_success_equivalent());
        assert!(!ClaimResultClass::Exhausted.is_success_equivalent());
    }

    #[test]
    fn round_trips_via_strings() {
        for class in [
            ClaimResultClass::Success,
            ClaimResultClass::UnknownResponse,
            ClaimResultClass::VerificationRequired,
        ] {
            assert_eq!(ClaimResultClass::parse(class.as_str()), Some(class));
        }
    }
}
