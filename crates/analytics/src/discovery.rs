//! First-discovery attribution and lead-time measurement.
//!
//! Answers "which source found this voucher first, and by how much?" — the
//! question that tells the owner whether a source is earning its polling
//! budget (ROADMAP Phase 27).
//!
//! The aggregation is a pure function over `(voucher, source, first observation)`
//! rows so it is fully unit-testable without a database; the repository only
//! supplies the rows.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

/// One source's earliest observation of one voucher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstObservation {
    /// Canonical voucher id, as stored.
    pub voucher_id: String,
    /// Source that made the observation.
    pub source: String,
    /// Earliest time this source saw this voucher.
    pub observed_at: DateTime<Utc>,
}

/// Per-source discovery outcome across a set of vouchers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoverySummary {
    /// Vouchers this source observed earliest of all sources.
    pub first_discovery_wins: i64,
    /// Sum of lead times over the wins that had a runner-up.
    pub total_lead: Duration,
    /// How many wins contributed to `total_lead`.
    ///
    /// Wins where no other source ever observed the voucher are excluded: a
    /// "lead" over nobody is not a measurement, and averaging it in as zero
    /// would drag a genuinely fast source's average down.
    pub lead_sample_size: i64,
}

impl DiscoverySummary {
    /// Mean lead over the sampled wins, or `None` when nothing was comparable.
    pub fn average_lead(&self) -> Option<Duration> {
        if self.lead_sample_size <= 0 {
            return None;
        }
        Some(Duration::seconds(
            self.total_lead.num_seconds() / self.lead_sample_size,
        ))
    }
}

/// Attribute first discovery and measure lead times.
///
/// Rules:
///
/// * Exactly **one** win is awarded per voucher, so wins across all sources sum
///   to the number of vouchers observed. A tie on the timestamp is broken by
///   source name ascending — arbitrary, but deterministic, which matters more
///   than which side of a tie wins.
/// * Lead time is measured against the **runner-up**: the earliest observation
///   by any *other* source. That answers "how much sooner did we know?", which
///   is the operationally useful quantity.
/// * Input order does not affect the result.
pub fn summarize_discovery(
    observations: &[FirstObservation],
) -> BTreeMap<String, DiscoverySummary> {
    // Group by voucher, keeping each source's earliest observation.
    let mut by_voucher: BTreeMap<&str, BTreeMap<&str, DateTime<Utc>>> = BTreeMap::new();
    for observation in observations {
        let per_source = by_voucher.entry(&observation.voucher_id).or_default();
        per_source
            .entry(&observation.source)
            .and_modify(|existing| {
                if observation.observed_at < *existing {
                    *existing = observation.observed_at;
                }
            })
            .or_insert(observation.observed_at);
    }

    let mut summaries: BTreeMap<String, DiscoverySummary> = BTreeMap::new();
    for per_source in by_voucher.values() {
        // `BTreeMap` iterates by source name and `min_by_key` keeps the first
        // minimum, so keying on the timestamp alone already resolves a tie to
        // the alphabetically first source.
        let Some((winner, winner_at)) = per_source
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(source, at)| (*source, *at))
        else {
            continue;
        };

        let runner_up = per_source
            .iter()
            .filter(|(source, _)| **source != winner)
            .map(|(_, at)| *at)
            .min();

        let entry = summaries.entry(winner.to_string()).or_default();
        entry.first_discovery_wins += 1;
        if let Some(runner_up_at) = runner_up {
            entry.total_lead += runner_up_at - winner_at;
            entry.lead_sample_size += 1;
        }
    }

    // Sources that observed vouchers but never won still deserve a row, so a
    // report never silently omits a source that is simply always second.
    for observation in observations {
        summaries.entry(observation.source.clone()).or_default();
    }

    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 8, 12, minute, 0)
            .single()
            .expect("fixed test timestamp is unambiguous")
    }

    fn observation(voucher: &str, source: &str, minute: u32) -> FirstObservation {
        FirstObservation {
            voucher_id: voucher.to_string(),
            source: source.to_string(),
            observed_at: at(minute),
        }
    }

    #[test]
    fn the_earliest_source_wins_and_leads_by_the_runner_up_gap() {
        let summaries = summarize_discovery(&[
            observation("v1", "fast", 0),
            observation("v1", "slow", 10),
            observation("v1", "slowest", 30),
        ]);

        let fast = &summaries["fast"];
        assert_eq!(fast.first_discovery_wins, 1);
        // Lead is over the runner-up (10m), not the last source (30m).
        assert_eq!(fast.average_lead(), Some(Duration::minutes(10)));

        assert_eq!(summaries["slow"].first_discovery_wins, 0);
        assert_eq!(summaries["slow"].average_lead(), None);
    }

    #[test]
    fn wins_sum_to_the_voucher_count() {
        let summaries = summarize_discovery(&[
            observation("v1", "a", 0),
            observation("v1", "b", 5),
            observation("v2", "b", 0),
            observation("v2", "a", 5),
            observation("v3", "a", 1),
        ]);
        let total: i64 = summaries.values().map(|s| s.first_discovery_wins).sum();
        assert_eq!(total, 3, "one win per voucher, no double counting");
        assert_eq!(summaries["a"].first_discovery_wins, 2);
        assert_eq!(summaries["b"].first_discovery_wins, 1);
    }

    #[test]
    fn a_solo_win_contributes_no_lead_sample() {
        let summaries = summarize_discovery(&[observation("v1", "only", 0)]);
        assert_eq!(summaries["only"].first_discovery_wins, 1);
        assert_eq!(summaries["only"].lead_sample_size, 0);
        assert_eq!(
            summaries["only"].average_lead(),
            None,
            "a lead over nobody is not a measurement"
        );
    }

    #[test]
    fn averaging_uses_only_comparable_wins() {
        let summaries = summarize_discovery(&[
            // Comparable win: 20 minutes ahead.
            observation("v1", "a", 0),
            observation("v1", "b", 20),
            // Comparable win: 40 minutes ahead.
            observation("v2", "a", 0),
            observation("v2", "b", 40),
            // Solo win: must not dilute the average with a zero.
            observation("v3", "a", 0),
        ]);
        assert_eq!(summaries["a"].first_discovery_wins, 3);
        assert_eq!(summaries["a"].lead_sample_size, 2);
        assert_eq!(summaries["a"].average_lead(), Some(Duration::minutes(30)));
    }

    #[test]
    fn ties_resolve_deterministically_by_source_name() {
        let forward =
            summarize_discovery(&[observation("v1", "alpha", 0), observation("v1", "beta", 0)]);
        let reversed =
            summarize_discovery(&[observation("v1", "beta", 0), observation("v1", "alpha", 0)]);
        assert_eq!(forward, reversed, "input order must not matter");
        assert_eq!(forward["alpha"].first_discovery_wins, 1);
        assert_eq!(forward["beta"].first_discovery_wins, 0);
        // A zero-length lead is still a measurement: the sources are equally fast.
        assert_eq!(forward["alpha"].average_lead(), Some(Duration::zero()));
    }

    #[test]
    fn repeated_observations_collapse_to_the_earliest() {
        let summaries = summarize_discovery(&[
            observation("v1", "a", 30),
            observation("v1", "a", 5),
            observation("v1", "b", 10),
        ]);
        assert_eq!(summaries["a"].first_discovery_wins, 1);
        assert_eq!(summaries["a"].average_lead(), Some(Duration::minutes(5)));
    }

    #[test]
    fn empty_input_yields_no_rows() {
        assert!(summarize_discovery(&[]).is_empty());
    }
}
