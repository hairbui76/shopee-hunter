//! Operational source quality score.
//!
//! **This is not voucher ranking.** It answers "is this collector worth its
//! polling budget?", never "is this voucher good?" (ROADMAP Phase 27 vs 17).
//! Feeding it into voucher ranking would let an infrastructure problem quietly
//! change which vouchers the owner is shown.
//!
//! The score starts from a perfect source and subtracts documented penalties,
//! each proportional to a measured rate. `value == BASELINE + sum(components)`
//! holds exactly, matching the explainability contract used by the ranking
//! crate.

use serde::{Deserialize, Serialize};

use crate::ratio::Ratio;
use crate::stats::SourceStats;

/// Score of a source with no measured problems.
pub const BASELINE_SCORE: i32 = 100;

// --- penalty budgets -------------------------------------------------------
// Each is the *worst case* deduction, scaled by the relevant measured rate.
// Ordered here by how strongly the signal indicates a genuinely broken source.

/// Parse failures mean the upstream schema moved; the strongest signal.
const PARSE_FAILURE_BUDGET: i32 = 40;
/// Failed runs: the source is unreachable or erroring.
const RUN_FAILURE_BUDGET: i32 = 25;
/// Rate limiting means the polling budget itself is the problem.
const RATE_LIMIT_BUDGET: i32 = 20;
/// Churn: mostly re-reporting known data.
const STALENESS_BUDGET: i32 = 10;

/// Flat penalty when a source has run enough times to judge and never once
/// succeeded.
const NEVER_SUCCEEDED_PENALTY: i32 = -30;
/// Flat penalty when a source runs but has never contributed a voucher.
const NO_DISCOVERIES_PENALTY: i32 = -20;
/// Runs per useful discovery above which the source looks wasteful.
const WASTEFUL_RUNS_PER_DISCOVERY: i64 = 50;
/// Flat penalty for exceeding that.
const WASTEFUL_PENALTY: i32 = -10;

/// Below this many runs there is not enough evidence to judge a source, so no
/// penalty is applied at all — a brand-new collector must not be condemned by
/// its first unlucky run.
pub const MIN_RUNS_FOR_JUDGEMENT: i64 = 10;

/// An explained operational score for one source.
///
/// The value is intentionally **not** clamped, so that
/// `BASELINE_SCORE + sum(components)` holds exactly and the arithmetic can be
/// checked. In practice it lands in roughly `-55..=100`: penalties total at
/// most 155, and a source failing several signals at once (unparsable *and*
/// unreachable *and* unproductive) does go negative. A source that merely never
/// succeeds bottoms out around 25, because rate-based penalties need a non-zero
/// denominator to apply at all. Clamp at the display layer if desired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceQualityScore {
    /// Source identifier.
    pub source: String,
    /// `BASELINE_SCORE` plus every component delta.
    pub value: i32,
    /// Whether enough runs were observed to judge the source at all.
    pub has_enough_evidence: bool,
    /// `(delta, reason)` pairs, most punishing first; ties keep evaluation
    /// order, so the ordering is deterministic.
    pub components: Vec<(i32, String)>,
}

impl SourceQualityScore {
    /// Human-readable explanation.
    pub fn explain(&self) -> String {
        let mut out = format!("{} quality {}", self.source, self.value);
        if !self.has_enough_evidence {
            out.push_str(" (insufficient evidence)");
        }
        for (delta, reason) in &self.components {
            out.push_str(&format!("\n{delta:+} {reason}"));
        }
        out
    }

    /// Whether any component reason contains `needle`.
    pub fn has_reason(&self, needle: &str) -> bool {
        self.components
            .iter()
            .any(|(_, reason)| reason.contains(needle))
    }
}

/// Score a source's operational quality.
pub fn quality_score(stats: &SourceStats) -> SourceQualityScore {
    let mut components: Vec<(i32, String)> = Vec::new();
    let has_enough_evidence = stats.runs >= MIN_RUNS_FOR_JUDGEMENT;

    if has_enough_evidence {
        penalize(
            &mut components,
            stats.parse_failure_rate,
            PARSE_FAILURE_BUDGET,
            "parse failures",
        );
        penalize(
            &mut components,
            stats.failure_rate,
            RUN_FAILURE_BUDGET,
            "failed runs",
        );
        penalize(
            &mut components,
            stats.rate_limit_rate,
            RATE_LIMIT_BUDGET,
            "rate limiting",
        );
        penalize(
            &mut components,
            stats.stale_candidate_rate,
            STALENESS_BUDGET,
            "already-known candidates",
        );

        if !stats.ever_succeeded() {
            components.push((
                NEVER_SUCCEEDED_PENALTY,
                format!("no successful run in {} attempts", stats.runs),
            ));
        }
        if !stats.produced_anything() {
            components.push((
                NO_DISCOVERIES_PENALTY,
                format!("no voucher discovered in {} runs", stats.runs),
            ));
        }
        if stats.requests_per_useful_discovery.is_some_and(|ratio| {
            ratio.basis_points / crate::ratio::BASIS_POINTS_PER_WHOLE >= WASTEFUL_RUNS_PER_DISCOVERY
        }) {
            let ratio = stats
                .requests_per_useful_discovery
                .map(Ratio::decimal_string)
                .unwrap_or_default();
            components.push((WASTEFUL_PENALTY, format!("{ratio} runs per discovery")));
        }
    }

    let value = BASELINE_SCORE + components.iter().map(|(delta, _)| *delta).sum::<i32>();
    // Most punishing first; stable sort keeps evaluation order on ties.
    components.sort_by_key(|(delta, _)| *delta);

    SourceQualityScore {
        source: stats.source.clone(),
        value,
        has_enough_evidence,
        components,
    }
}

/// Scale a penalty budget by a measured rate, skipping zero contributions so
/// the explanation carries no `+0` noise.
fn penalize(components: &mut Vec<(i32, String)>, rate: Option<Ratio>, budget: i32, label: &str) {
    let Some(rate) = rate else {
        return;
    };
    let penalty = rate.scale(budget);
    if penalty > 0 {
        components.push((-penalty, format!("{} {label}", rate.percent_string())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DiscoverySummary;
    use crate::stats::SourceCounters;

    fn stats_from(counters: SourceCounters, unique: i64) -> SourceStats {
        SourceStats::from_parts(counters, &DiscoverySummary::default(), unique)
    }

    fn healthy() -> SourceCounters {
        SourceCounters {
            source: "good".into(),
            runs: 100,
            successful_runs: 100,
            failed_runs: 0,
            candidates: 100,
            new_items: 100,
            updated_items: 0,
            parse_errors: 0,
            rate_limit_incidents: 0,
            last_success_at: None,
        }
    }

    #[test]
    fn a_healthy_source_scores_the_baseline() {
        let score = quality_score(&stats_from(healthy(), 100));
        assert_eq!(score.value, BASELINE_SCORE);
        assert!(score.components.is_empty(), "{}", score.explain());
        assert!(score.has_enough_evidence);
    }

    #[test]
    fn the_value_always_equals_baseline_plus_components() {
        let cases = [
            healthy(),
            SourceCounters {
                parse_errors: 50,
                ..healthy()
            },
            SourceCounters {
                runs: 100,
                successful_runs: 0,
                failed_runs: 100,
                candidates: 0,
                new_items: 0,
                ..healthy()
            },
            SourceCounters {
                rate_limit_incidents: 40,
                ..healthy()
            },
        ];
        for counters in cases {
            let score = quality_score(&stats_from(counters, 0));
            let sum: i32 = score.components.iter().map(|(d, _)| *d).sum();
            assert_eq!(
                score.value,
                BASELINE_SCORE + sum,
                "score must stay arithmetically checkable: {}",
                score.explain()
            );
        }
    }

    #[test]
    fn parse_failures_dominate_the_penalties() {
        // Half of everything encountered fails to parse.
        let score = quality_score(&stats_from(
            SourceCounters {
                parse_errors: 100,
                ..healthy()
            },
            100,
        ));
        assert!(score.has_reason("parse failures"));
        // 50% of a 40-point budget.
        assert_eq!(score.value, BASELINE_SCORE - 20);
    }

    #[test]
    fn a_dead_source_is_penalised_from_several_angles() {
        let score = quality_score(&stats_from(
            SourceCounters {
                source: "dead".into(),
                runs: 50,
                successful_runs: 0,
                failed_runs: 50,
                candidates: 0,
                new_items: 0,
                updated_items: 0,
                parse_errors: 0,
                rate_limit_incidents: 0,
                last_success_at: None,
            },
            0,
        ));
        assert!(score.has_reason("failed runs"));
        assert!(score.has_reason("no successful run"));
        assert!(score.has_reason("no voucher discovered"));
        // It never produced a candidate, so the rate-based parse penalty has no
        // denominator and cannot apply; the flat penalties still put it far
        // below a healthy source.
        assert!(score.value < BASELINE_SCORE / 2, "{}", score.explain());
        // Worst contribution is listed first.
        assert_eq!(
            score.components.first().map(|(d, _)| *d),
            score.components.iter().map(|(d, _)| *d).min()
        );
    }

    #[test]
    fn several_simultaneous_failures_push_the_score_below_zero() {
        // Unparsable *and* unreachable *and* unproductive: this is the case the
        // unclamped range exists for.
        let score = quality_score(&stats_from(
            SourceCounters {
                source: "catastrophe".into(),
                runs: 100,
                successful_runs: 0,
                failed_runs: 100,
                candidates: 0,
                new_items: 0,
                updated_items: 0,
                parse_errors: 500,
                rate_limit_incidents: 0,
                last_success_at: None,
            },
            0,
        ));
        assert!(score.value < 0, "{}", score.explain());
        let sum: i32 = score.components.iter().map(|(d, _)| *d).sum();
        assert_eq!(score.value, BASELINE_SCORE + sum);
    }

    #[test]
    fn a_new_source_is_not_condemned_without_evidence() {
        let score = quality_score(&stats_from(
            SourceCounters {
                source: "fresh".into(),
                runs: 3,
                successful_runs: 0,
                failed_runs: 3,
                parse_errors: 9,
                ..Default::default()
            },
            0,
        ));
        assert!(!score.has_enough_evidence);
        assert_eq!(score.value, BASELINE_SCORE);
        assert!(score.components.is_empty());
        assert!(score.explain().contains("insufficient evidence"));
    }

    #[test]
    fn wasteful_polling_is_called_out() {
        let score = quality_score(&stats_from(
            SourceCounters {
                runs: 1_000,
                successful_runs: 1_000,
                candidates: 1_000,
                new_items: 1,
                updated_items: 0,
                ..healthy()
            },
            1,
        ));
        assert!(
            score.has_reason("runs per discovery"),
            "{}",
            score.explain()
        );
    }
}
