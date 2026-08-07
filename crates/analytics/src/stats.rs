//! Per-source statistics.
//!
//! [`SourceStats`] is assembled from three independent inputs — run counters,
//! discovery attribution, and the voucher table — by one pure constructor, so
//! every derived rate is guaranteed consistent with the counters it came from
//! and can be tested without a database.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::discovery::DiscoverySummary;
use crate::ratio::Ratio;

/// Raw per-source sums straight out of `collector_runs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCounters {
    /// Source identifier.
    pub source: String,
    /// Runs recorded in the window.
    pub runs: i64,
    /// Runs whose outcome was `SUCCESS`.
    pub successful_runs: i64,
    /// Runs whose outcome was `FAILED`.
    pub failed_runs: i64,
    /// Sum of `candidate_count`: items that parsed into candidates.
    pub candidates: i64,
    /// Sum of `new_count`: candidates that created a voucher.
    pub new_items: i64,
    /// Sum of `updated_count`: candidates that meaningfully changed one.
    pub updated_items: i64,
    /// Sum of `parse_errors`: items the parser rejected.
    pub parse_errors: i64,
    /// Runs whose `detail` matched a rate-limit marker.
    pub rate_limit_incidents: i64,
    /// Most recent successful run's finish time.
    pub last_success_at: Option<DateTime<Utc>>,
}

/// Everything the operator needs to judge one discovery source.
///
/// Counters are absolute; rates are `Option` because an undefined rate (no
/// denominator) must be distinguishable from a genuine zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStats {
    /// Source identifier.
    pub source: String,

    // --- raw counters -----------------------------------------------------
    /// Collector runs in the window.
    pub runs: i64,
    /// Successful runs in the window.
    pub successful_runs: i64,
    /// Failed runs in the window.
    pub failed_runs: i64,
    /// Candidates emitted by the parser.
    pub candidates: i64,
    /// Candidates that created a voucher, per the run log.
    pub new_items: i64,
    /// Candidates that meaningfully updated a voucher.
    pub updated_items: i64,
    /// Items the parser rejected.
    pub parse_errors: i64,

    // --- discovery quality ------------------------------------------------
    /// Distinct logical vouchers whose canonical record names this source as
    /// the discoverer. Counted from the `vouchers` table, so it is unaffected
    /// by how often the source re-reports the same voucher.
    pub new_unique_vouchers: i64,
    /// Vouchers this source observed earlier than every other source.
    pub first_discovery_wins: i64,
    /// Mean head start over the runner-up source, in seconds.
    ///
    /// Seconds rather than a `Duration` so the struct serializes cleanly; see
    /// [`SourceStats::avg_discovery_lead`] for the typed form.
    pub avg_discovery_lead_seconds: Option<i64>,
    /// How many wins had a runner-up to measure against.
    pub lead_sample_size: i64,

    // --- data quality -----------------------------------------------------
    /// Share of emitted candidates that produced neither a new voucher nor a
    /// meaningful update.
    ///
    /// **Best effort, and a churn indicator rather than a correctness measure.**
    /// The schema records no ground truth about whether a voucher turned out to
    /// be fake, so this cannot detect a source inventing plausible vouchers. A
    /// high value means the source mostly re-reports what is already known,
    /// which is normal for frequent polling and only meaningful next to
    /// `new_unique_vouchers` and `requests_per_useful_discovery`.
    pub stale_candidate_rate: Option<Ratio>,
    /// Share of encountered items the parser could not read, i.e.
    /// `parse_errors / (candidates + parse_errors)`. The clearest signal that
    /// an upstream schema changed.
    pub parse_failure_rate: Option<Ratio>,
    /// Collector runs spent per useful discovery, where useful means a new or
    /// meaningfully updated voucher. `None` when the source produced nothing.
    pub requests_per_useful_discovery: Option<Ratio>,
    /// Runs whose detail reported upstream rate limiting.
    pub rate_limit_incidents: i64,
    /// Share of runs that hit a rate limit.
    pub rate_limit_rate: Option<Ratio>,
    /// Share of runs that failed outright.
    pub failure_rate: Option<Ratio>,
    /// When this source last completed successfully.
    pub last_success_at: Option<DateTime<Utc>>,
}

impl SourceStats {
    /// Assemble stats from the three independently gathered inputs.
    ///
    /// Pure: no clock, no I/O. All rates are derived here so they cannot drift
    /// out of step with their counters.
    pub fn from_parts(
        counters: SourceCounters,
        discovery: &DiscoverySummary,
        new_unique_vouchers: i64,
    ) -> Self {
        // Everything the parser was handed, including what it choked on.
        let items_seen = counters.candidates.saturating_add(counters.parse_errors);
        let useful = counters.new_items.saturating_add(counters.updated_items);
        // Clamped: `new + updated` can exceed `candidate_count` if a collector
        // miscounts, and a negative "stale" count would be nonsense.
        let stale = counters.candidates.saturating_sub(useful).max(0);

        Self {
            source: counters.source,
            runs: counters.runs,
            successful_runs: counters.successful_runs,
            failed_runs: counters.failed_runs,
            candidates: counters.candidates,
            new_items: counters.new_items,
            updated_items: counters.updated_items,
            parse_errors: counters.parse_errors,

            new_unique_vouchers,
            first_discovery_wins: discovery.first_discovery_wins,
            avg_discovery_lead_seconds: discovery.average_lead().map(|d| d.num_seconds()),
            lead_sample_size: discovery.lead_sample_size,

            stale_candidate_rate: Ratio::from_ratio(stale, counters.candidates),
            parse_failure_rate: Ratio::from_ratio(counters.parse_errors, items_seen),
            requests_per_useful_discovery: Ratio::from_ratio(counters.runs, useful),
            rate_limit_incidents: counters.rate_limit_incidents,
            rate_limit_rate: Ratio::from_ratio(counters.rate_limit_incidents, counters.runs),
            failure_rate: Ratio::from_ratio(counters.failed_runs, counters.runs),
            last_success_at: counters.last_success_at,
        }
    }

    /// Mean discovery lead as a typed duration.
    pub fn avg_discovery_lead(&self) -> Option<Duration> {
        self.avg_discovery_lead_seconds.map(Duration::seconds)
    }

    /// Whether the source ever completed a run successfully in the window.
    pub fn ever_succeeded(&self) -> bool {
        self.successful_runs > 0
    }

    /// Whether the source produced anything of value in the window.
    pub fn produced_anything(&self) -> bool {
        self.new_items > 0 || self.updated_items > 0 || self.new_unique_vouchers > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters() -> SourceCounters {
        SourceCounters {
            source: "feed".into(),
            runs: 100,
            successful_runs: 90,
            failed_runs: 10,
            candidates: 400,
            new_items: 30,
            updated_items: 10,
            parse_errors: 100,
            rate_limit_incidents: 5,
            last_success_at: None,
        }
    }

    #[test]
    fn rates_derive_from_their_counters() {
        let stats = SourceStats::from_parts(counters(), &DiscoverySummary::default(), 25);

        // 100 errors out of 500 items encountered = 20%.
        assert_eq!(
            stats.parse_failure_rate.map(Ratio::percent_string),
            Some("20.00%".to_string())
        );
        // 400 candidates, 40 useful => 360 stale = 90%.
        assert_eq!(
            stats.stale_candidate_rate.map(Ratio::percent_string),
            Some("90.00%".to_string())
        );
        // 100 runs / 40 useful discoveries = 2.50 runs each.
        assert_eq!(
            stats
                .requests_per_useful_discovery
                .map(Ratio::decimal_string),
            Some("2.50".to_string())
        );
        assert_eq!(
            stats.rate_limit_rate.map(Ratio::percent_string),
            Some("5.00%".to_string())
        );
        assert_eq!(
            stats.failure_rate.map(Ratio::percent_string),
            Some("10.00%".to_string())
        );
        assert_eq!(stats.new_unique_vouchers, 25);
    }

    #[test]
    fn a_source_that_never_ran_reports_no_data_not_zero() {
        let stats = SourceStats::from_parts(
            SourceCounters {
                source: "idle".into(),
                ..Default::default()
            },
            &DiscoverySummary::default(),
            0,
        );
        assert_eq!(stats.parse_failure_rate, None);
        assert_eq!(stats.stale_candidate_rate, None);
        assert_eq!(stats.requests_per_useful_discovery, None);
        assert_eq!(stats.failure_rate, None);
        assert!(!stats.ever_succeeded());
        assert!(!stats.produced_anything());
    }

    #[test]
    fn a_perfect_source_reports_genuine_zeroes() {
        let stats = SourceStats::from_parts(
            SourceCounters {
                source: "clean".into(),
                runs: 10,
                successful_runs: 10,
                candidates: 10,
                new_items: 10,
                ..Default::default()
            },
            &DiscoverySummary::default(),
            10,
        );
        assert_eq!(stats.parse_failure_rate, Some(Ratio::ZERO));
        assert_eq!(stats.stale_candidate_rate, Some(Ratio::ZERO));
        assert_eq!(
            stats
                .requests_per_useful_discovery
                .map(Ratio::decimal_string),
            Some("1.00".to_string())
        );
        assert!(stats.produced_anything());
    }

    #[test]
    fn miscounted_runs_cannot_produce_a_negative_stale_rate() {
        // A buggy collector reporting more useful items than candidates.
        let stats = SourceStats::from_parts(
            SourceCounters {
                source: "buggy".into(),
                runs: 1,
                candidates: 5,
                new_items: 50,
                ..Default::default()
            },
            &DiscoverySummary::default(),
            0,
        );
        assert_eq!(stats.stale_candidate_rate, Some(Ratio::ZERO));
    }

    #[test]
    fn discovery_summary_flows_through() {
        let discovery = DiscoverySummary {
            first_discovery_wins: 4,
            total_lead: Duration::minutes(40),
            lead_sample_size: 2,
        };
        let stats = SourceStats::from_parts(counters(), &discovery, 4);
        assert_eq!(stats.first_discovery_wins, 4);
        assert_eq!(stats.lead_sample_size, 2);
        assert_eq!(stats.avg_discovery_lead(), Some(Duration::minutes(20)));
    }
}
