//! Automatic degradation *recommendations*.
//!
//! [`should_degrade`] is a pure function that returns advice and nothing else.
//! It never writes configuration, never disables a collector, and never touches
//! the database — the operator or the supervisor decides what to do with the
//! recommendation. That separation is deliberate: a metrics blip must not be
//! able to silently switch off voucher discovery.
//!
//! Every rule requires [`crate::quality::MIN_RUNS_FOR_JUDGEMENT`] runs of
//! evidence first.

use serde::{Deserialize, Serialize};

use crate::quality::MIN_RUNS_FOR_JUDGEMENT;
use crate::ratio::Ratio;
use crate::stats::SourceStats;

/// Parse-failure share at which the parser is considered broken outright.
const BROKEN_PARSER_BP: i64 = 5_000; // 50%
/// Parse-failure share worth a human look before it gets worse.
const DEGRADED_PARSER_BP: i64 = 2_000; // 20%
/// Run-failure share at which the source is treated as unreachable.
const UNREACHABLE_BP: i64 = 8_000; // 80%
/// Rate-limit share at which polling is clearly too aggressive.
const RATE_LIMITED_BP: i64 = 2_000; // 20%
/// Runs per useful discovery above which polling is not paying for itself.
const WASTEFUL_RUNS_PER_DISCOVERY: i64 = 100;

/// How much to slow polling when a source is rate limited.
const RATE_LIMIT_BACKOFF: u32 = 4;
/// How much to slow polling when a source is merely unproductive.
const UNPRODUCTIVE_BACKOFF: u32 = 2;

/// What the analytics layer suggests doing about a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DegradeAction {
    /// Poll this source `factor` times less often, temporarily.
    ReducePolling {
        /// Divisor to apply to the configured interval frequency.
        factor: u32,
        /// Why.
        reason: String,
    },
    /// Stop polling this source until someone looks at it.
    ///
    /// Still only a recommendation; the collector supervisor owns the switch.
    Disable {
        /// Why.
        reason: String,
    },
    /// Keep polling, but the owner should investigate.
    ManualReview {
        /// Why.
        reason: String,
    },
}

impl DegradeAction {
    /// Short stable label for logs and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ReducePolling { .. } => "REDUCE_POLLING",
            Self::Disable { .. } => "DISABLE",
            Self::ManualReview { .. } => "MANUAL_REVIEW",
        }
    }

    /// The explanation carried by the recommendation.
    pub fn reason(&self) -> &str {
        match self {
            Self::ReducePolling { reason, .. }
            | Self::Disable { reason }
            | Self::ManualReview { reason } => reason,
        }
    }
}

/// Recommend a degradation, if the evidence supports one.
///
/// Rules are evaluated most-severe-first and the first match wins:
///
/// 1. **Disable** — parser broken (≥50% parse failures).
/// 2. **Disable** — never succeeded, or effectively unreachable (≥80% failures).
/// 3. **ReducePolling ×4** — rate limited on ≥20% of runs.
/// 4. **ManualReview** — parse failures ≥20%, i.e. drifting upstream schema.
/// 5. **ReducePolling ×4** — nothing discovered at all despite many runs.
/// 6. **ReducePolling ×2** — more than 100 runs per useful discovery.
///
/// Returns `None` when the source looks fine, or when there is not yet enough
/// evidence to judge it.
pub fn should_degrade(stats: &SourceStats) -> Option<DegradeAction> {
    if stats.runs < MIN_RUNS_FOR_JUDGEMENT {
        return None;
    }

    if let Some(rate) = stats.parse_failure_rate {
        if rate.basis_points >= BROKEN_PARSER_BP {
            return Some(DegradeAction::Disable {
                reason: format!(
                    "{} of items fail to parse: the upstream schema has almost certainly changed",
                    rate.percent_string()
                ),
            });
        }
    }

    if !stats.ever_succeeded() {
        return Some(DegradeAction::Disable {
            reason: format!("no successful run in the last {} attempts", stats.runs),
        });
    }

    if stats
        .failure_rate
        .is_some_and(|rate| rate.basis_points >= UNREACHABLE_BP)
    {
        let rate = stats
            .failure_rate
            .map(Ratio::percent_string)
            .unwrap_or_default();
        return Some(DegradeAction::Disable {
            reason: format!("{rate} of runs fail: the source is effectively unreachable"),
        });
    }

    if stats
        .rate_limit_rate
        .is_some_and(|rate| rate.basis_points >= RATE_LIMITED_BP)
    {
        let rate = stats
            .rate_limit_rate
            .map(Ratio::percent_string)
            .unwrap_or_default();
        return Some(DegradeAction::ReducePolling {
            factor: RATE_LIMIT_BACKOFF,
            reason: format!("{rate} of runs are rate limited: polling is too aggressive"),
        });
    }

    if let Some(rate) = stats.parse_failure_rate {
        if rate.basis_points >= DEGRADED_PARSER_BP {
            return Some(DegradeAction::ManualReview {
                reason: format!(
                    "{} of items fail to parse: the parser may be drifting out of date",
                    rate.percent_string()
                ),
            });
        }
    }

    if !stats.produced_anything() {
        return Some(DegradeAction::ReducePolling {
            factor: RATE_LIMIT_BACKOFF,
            reason: format!(
                "no voucher discovered across {} runs: the polling budget is being wasted",
                stats.runs
            ),
        });
    }

    if let Some(ratio) = stats.requests_per_useful_discovery {
        if ratio.basis_points / crate::ratio::BASIS_POINTS_PER_WHOLE >= WASTEFUL_RUNS_PER_DISCOVERY
        {
            return Some(DegradeAction::ReducePolling {
                factor: UNPRODUCTIVE_BACKOFF,
                reason: format!(
                    "{} runs per useful discovery: poll less often for the same result",
                    ratio.decimal_string()
                ),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DiscoverySummary;
    use crate::stats::SourceCounters;

    fn stats(counters: SourceCounters) -> SourceStats {
        SourceStats::from_parts(counters, &DiscoverySummary::default(), 0)
    }

    fn healthy() -> SourceCounters {
        SourceCounters {
            source: "good".into(),
            runs: 100,
            successful_runs: 100,
            failed_runs: 0,
            candidates: 100,
            new_items: 50,
            updated_items: 50,
            parse_errors: 0,
            rate_limit_incidents: 0,
            last_success_at: None,
        }
    }

    #[test]
    fn a_healthy_source_is_left_alone() {
        assert_eq!(should_degrade(&stats(healthy())), None);
    }

    #[test]
    fn insufficient_evidence_never_degrades() {
        // Catastrophic numbers, but only a handful of runs.
        let barely_run = SourceCounters {
            runs: MIN_RUNS_FOR_JUDGEMENT - 1,
            successful_runs: 0,
            failed_runs: MIN_RUNS_FOR_JUDGEMENT - 1,
            candidates: 0,
            new_items: 0,
            updated_items: 0,
            parse_errors: 100,
            ..healthy()
        };
        assert_eq!(should_degrade(&stats(barely_run)), None);
    }

    #[test]
    fn a_broken_parser_is_disabled() {
        let action = should_degrade(&stats(SourceCounters {
            candidates: 10,
            parse_errors: 90,
            ..healthy()
        }))
        .expect("must recommend something");
        assert_eq!(action.kind(), "DISABLE");
        assert!(action.reason().contains("schema"));
    }

    #[test]
    fn a_source_that_never_succeeds_is_disabled() {
        let action = should_degrade(&stats(SourceCounters {
            successful_runs: 0,
            failed_runs: 100,
            ..healthy()
        }))
        .expect("must recommend something");
        assert_eq!(action.kind(), "DISABLE");
        assert!(action.reason().contains("no successful run"));
    }

    #[test]
    fn rate_limiting_reduces_polling_rather_than_disabling() {
        let action = should_degrade(&stats(SourceCounters {
            rate_limit_incidents: 30,
            ..healthy()
        }))
        .expect("must recommend something");
        match action {
            DegradeAction::ReducePolling { factor, ref reason } => {
                assert_eq!(factor, RATE_LIMIT_BACKOFF);
                assert!(reason.contains("rate limited"));
            }
            other => panic!("expected reduced polling, got {other:?}"),
        }
    }

    #[test]
    fn a_drifting_parser_asks_for_a_human() {
        let action = should_degrade(&stats(SourceCounters {
            candidates: 70,
            parse_errors: 30,
            ..healthy()
        }))
        .expect("must recommend something");
        assert_eq!(action.kind(), "MANUAL_REVIEW");
    }

    #[test]
    fn an_unproductive_source_is_slowed_down() {
        let action = should_degrade(&stats(SourceCounters {
            candidates: 500,
            new_items: 0,
            updated_items: 0,
            ..healthy()
        }))
        .expect("must recommend something");
        assert_eq!(action.kind(), "REDUCE_POLLING");
        assert!(action.reason().contains("no voucher discovered"));
    }

    #[test]
    fn poor_yield_is_slowed_down_more_gently() {
        let action = should_degrade(&stats(SourceCounters {
            runs: 1_000,
            successful_runs: 1_000,
            candidates: 1_000,
            new_items: 1,
            updated_items: 0,
            ..healthy()
        }))
        .expect("must recommend something");
        match action {
            DegradeAction::ReducePolling { factor, ref reason } => {
                assert_eq!(factor, UNPRODUCTIVE_BACKOFF);
                assert!(reason.contains("per useful discovery"));
            }
            other => panic!("expected reduced polling, got {other:?}"),
        }
    }

    #[test]
    fn severity_order_puts_disable_ahead_of_slow_down() {
        // Both a broken parser and rate limiting: the parser must win.
        let action = should_degrade(&stats(SourceCounters {
            candidates: 10,
            parse_errors: 90,
            rate_limit_incidents: 90,
            ..healthy()
        }))
        .expect("must recommend something");
        assert_eq!(action.kind(), "DISABLE");
    }
}
