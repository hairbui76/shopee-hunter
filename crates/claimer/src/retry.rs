//! Response-aware, bounded retry policy. Never a generic retry: the next step
//! is derived from the classified result and the remaining attempt budget.

use std::time::Duration;

use shopee_hunter_domain::claim::{ClaimResultClass, RetryClass};

/// What the claim service should do after a classified attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryStep {
    /// Done — success or terminal failure. No further attempts.
    Terminal,
    /// Retry after `delay` (bounded backoff / rate-limit wait).
    RetryAfter(Duration),
    /// Reschedule at the voucher start time (not active yet).
    RescheduleAtStart,
    /// Stop and let the session recover (expired / verification).
    PauseForSession,
    /// Unknown response over diagnostic budget — stop and alert.
    ReviewRequired,
}

/// Compute the next step. `attempt_index` is 0-based (the attempt that just
/// completed); `max_attempts` is the total budget.
pub fn next_step(
    class: ClaimResultClass,
    attempt_index: u32,
    max_attempts: u32,
    base_delay: Duration,
    diagnostic_budget: u32,
) -> RetryStep {
    match class.retry_class() {
        RetryClass::NoRetry => RetryStep::Terminal,
        RetryClass::RescheduleAtStart => RetryStep::RescheduleAtStart,
        RetryClass::PauseSession => RetryStep::PauseForSession,
        RetryClass::DiagnosticBudget => {
            if attempt_index + 1 >= diagnostic_budget {
                RetryStep::ReviewRequired
            } else {
                RetryStep::RetryAfter(backoff(base_delay, attempt_index))
            }
        }
        RetryClass::RetryAfterBackoff => {
            if attempt_index + 1 >= max_attempts {
                RetryStep::Terminal
            } else {
                RetryStep::RetryAfter(backoff(base_delay, attempt_index))
            }
        }
        RetryClass::RetryAfterRateLimit => {
            if attempt_index + 1 >= max_attempts {
                RetryStep::Terminal
            } else {
                // Rate limits get a longer floor than transient failures.
                RetryStep::RetryAfter(
                    backoff(base_delay, attempt_index).max(Duration::from_secs(2)),
                )
            }
        }
    }
}

/// Capped exponential backoff with a 60s ceiling.
fn backoff(base: Duration, attempt_index: u32) -> Duration {
    let factor = 2u32.saturating_pow(attempt_index.min(16));
    base.saturating_mul(factor).min(Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_terminal_classes_stop() {
        for class in [
            ClaimResultClass::Success,
            ClaimResultClass::AlreadySaved,
            ClaimResultClass::Exhausted,
            ClaimResultClass::Ineligible,
            ClaimResultClass::Expired,
            ClaimResultClass::InvalidVoucher,
        ] {
            assert_eq!(
                next_step(class, 0, 3, Duration::from_millis(500), 2),
                RetryStep::Terminal
            );
        }
    }

    #[test]
    fn transient_retries_then_terminates_at_budget() {
        let d = Duration::from_millis(500);
        assert!(matches!(
            next_step(ClaimResultClass::TransientFailure, 0, 3, d, 2),
            RetryStep::RetryAfter(_)
        ));
        assert!(matches!(
            next_step(ClaimResultClass::TransientFailure, 1, 3, d, 2),
            RetryStep::RetryAfter(_)
        ));
        // Third attempt (index 2) is the last — no more retries.
        assert_eq!(
            next_step(ClaimResultClass::TransientFailure, 2, 3, d, 2),
            RetryStep::Terminal
        );
    }

    #[test]
    fn rate_limited_has_a_floor() {
        match next_step(
            ClaimResultClass::RateLimited,
            0,
            5,
            Duration::from_millis(10),
            2,
        ) {
            RetryStep::RetryAfter(d) => assert!(d >= Duration::from_secs(2)),
            other => panic!("expected RetryAfter, got {other:?}"),
        }
    }

    #[test]
    fn session_and_not_active_and_unknown_paths() {
        let d = Duration::from_millis(500);
        assert_eq!(
            next_step(ClaimResultClass::SessionExpired, 0, 3, d, 2),
            RetryStep::PauseForSession
        );
        assert_eq!(
            next_step(ClaimResultClass::VerificationRequired, 0, 3, d, 2),
            RetryStep::PauseForSession
        );
        assert_eq!(
            next_step(ClaimResultClass::NotActive, 0, 3, d, 2),
            RetryStep::RescheduleAtStart
        );
        // Unknown: one diagnostic retry, then review.
        assert!(matches!(
            next_step(ClaimResultClass::UnknownResponse, 0, 3, d, 2),
            RetryStep::RetryAfter(_)
        ));
        assert_eq!(
            next_step(ClaimResultClass::UnknownResponse, 1, 3, d, 2),
            RetryStep::ReviewRequired
        );
    }

    #[test]
    fn backoff_is_capped() {
        let d = Duration::from_secs(10);
        assert_eq!(backoff(d, 0), Duration::from_secs(10));
        assert_eq!(backoff(d, 1), Duration::from_secs(20));
        assert_eq!(backoff(d, 10), Duration::from_secs(60)); // capped
    }
}
