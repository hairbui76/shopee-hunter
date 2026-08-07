//! Read-only aggregation service over the shared [`Database`].
//!
//! Analytics owns its own queries rather than adding methods to the storage
//! crate, so reporting can evolve without touching the write path the watcher
//! depends on. Every statement here is a `SELECT`.
//!
//! # Portability
//!
//! Queries run through `sqlx::Any` against SQLite (dev) and PostgreSQL (prod),
//! so only portable SQL is used: no window functions, no `FILTER`, no
//! `date_trunc`. Timestamps are compared as TEXT, which is sound because
//! `storage::convert::ts_to_str` emits fixed-width RFC3339 microseconds — so
//! lexicographic order equals chronological order.
//!
//! The optional time window is applied by selecting a different SQL string
//! rather than binding a nullable parameter: PostgreSQL cannot infer the type
//! of a bare `$1 IS NULL`, and an untypeable parameter would fail at runtime on
//! production only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use shopee_hunter_storage::convert::{str_to_ts, ts_to_str};
use shopee_hunter_storage::Database;
use sqlx::any::AnyRow;
use sqlx::Row;

use crate::degrade::{should_degrade, DegradeAction};
use crate::discovery::{summarize_discovery, DiscoverySummary, FirstObservation};
use crate::error::AnalyticsError;
use crate::quality::{quality_score, SourceQualityScore};
use crate::stats::{SourceCounters, SourceStats};

/// Substrings that mark a `collector_runs.detail` as a rate-limit incident.
///
/// This is the contract between collectors and analytics: the schema has no
/// dedicated rate-limit column, so a collector signals one by mentioning it in
/// the run detail. Matching is done on a lowercased copy of the field.
pub const RATE_LIMIT_DETAIL_MARKERS: &[&str] = &[
    "%rate limit%",
    "%rate_limit%",
    "%429%",
    "%too many request%",
];

/// Time window an analysis covers.
///
/// The window filters *observations and runs*, so a source can only win a first
/// discovery against competitors that also observed within the window. Widen it
/// for lifetime figures; narrow it to judge recent behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnalyticsWindow {
    /// Inclusive lower bound, or `None` for all recorded history.
    pub since: Option<DateTime<Utc>>,
}

impl AnalyticsWindow {
    /// Everything ever recorded.
    pub const ALL_TIME: Self = Self { since: None };

    /// Everything at or after `since`.
    pub fn since(since: DateTime<Utc>) -> Self {
        Self { since: Some(since) }
    }

    /// The trailing `duration` ending at `now`.
    pub fn trailing(duration: Duration, now: DateTime<Utc>) -> Self {
        Self {
            since: Some(now - duration),
        }
    }

    fn bound(&self) -> Option<String> {
        self.since.map(ts_to_str)
    }
}

/// Stats plus the operational judgement derived from them.
#[derive(Debug, Clone)]
pub struct SourceReport {
    /// Measured statistics.
    pub stats: SourceStats,
    /// Derived operational quality score.
    pub quality: SourceQualityScore,
    /// Recommended action, if the evidence supports one. Advice only.
    pub recommendation: Option<DegradeAction>,
}

/// Read-only analytics over the persisted collector and voucher history.
pub struct AnalyticsRepository<'a> {
    db: &'a Database,
}

impl<'a> AnalyticsRepository<'a> {
    /// Borrow the shared database handle.
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Per-source statistics for every source with any recorded activity.
    ///
    /// Sources are returned in name order. A source that appears in only one of
    /// the underlying tables (ran but discovered nothing, or discovered
    /// something before the run log existed) still gets a row, so a report
    /// never silently omits a source.
    pub async fn source_stats(
        &self,
        window: AnalyticsWindow,
    ) -> Result<Vec<SourceStats>, AnalyticsError> {
        let mut counters = self.run_counters(window).await?;
        let rate_limits = self.rate_limit_incidents(window).await?;
        let unique_vouchers = self.unique_vouchers_by_source(window).await?;
        let discovery = summarize_discovery(&self.first_observations(window).await?);

        for (source, incidents) in &rate_limits {
            counters
                .entry(source.clone())
                .or_insert_with(|| SourceCounters {
                    source: source.clone(),
                    ..Default::default()
                })
                .rate_limit_incidents = *incidents;
        }

        // Union of every source seen anywhere, so nothing is dropped.
        let sources: BTreeSet<String> = counters
            .keys()
            .chain(unique_vouchers.keys())
            .chain(discovery.keys())
            .cloned()
            .collect();

        Ok(sources
            .into_iter()
            .map(|source| {
                let counters = counters.remove(&source).unwrap_or(SourceCounters {
                    source: source.clone(),
                    ..Default::default()
                });
                let discovery = discovery.get(&source).cloned().unwrap_or_default();
                let unique = unique_vouchers.get(&source).copied().unwrap_or(0);
                SourceStats::from_parts(counters, &discovery, unique)
            })
            .collect())
    }

    /// Statistics for a single source, or `None` if it has no recorded activity.
    pub async fn source_stats_for(
        &self,
        source: &str,
        window: AnalyticsWindow,
    ) -> Result<Option<SourceStats>, AnalyticsError> {
        Ok(self
            .source_stats(window)
            .await?
            .into_iter()
            .find(|stats| stats.source == source))
    }

    /// Full operational report: stats, quality score, and recommendation.
    ///
    /// Ordered worst-quality first so the operator sees problems immediately;
    /// ties break on source name, keeping the order deterministic.
    pub async fn report(
        &self,
        window: AnalyticsWindow,
    ) -> Result<Vec<SourceReport>, AnalyticsError> {
        let mut reports: Vec<SourceReport> = self
            .source_stats(window)
            .await?
            .into_iter()
            .map(|stats| {
                let quality = quality_score(&stats);
                let recommendation = should_degrade(&stats);
                SourceReport {
                    stats,
                    quality,
                    recommendation,
                }
            })
            .collect();
        reports.sort_by(|a, b| {
            a.quality
                .value
                .cmp(&b.quality.value)
                .then_with(|| a.stats.source.cmp(&b.stats.source))
        });
        Ok(reports)
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    async fn run_counters(
        &self,
        window: AnalyticsWindow,
    ) -> Result<BTreeMap<String, SourceCounters>, AnalyticsError> {
        const SELECT: &str = "SELECT source,
                    COUNT(*) AS runs,
                    SUM(CASE WHEN outcome = 'SUCCESS' THEN 1 ELSE 0 END) AS successful_runs,
                    SUM(CASE WHEN outcome = 'FAILED' THEN 1 ELSE 0 END) AS failed_runs,
                    SUM(candidate_count) AS candidates,
                    SUM(new_count) AS new_items,
                    SUM(updated_count) AS updated_items,
                    SUM(parse_errors) AS parse_errors,
                    MAX(CASE WHEN outcome = 'SUCCESS' THEN finished_at END) AS last_success_at
             FROM collector_runs";

        let bound = window.bound();
        let sql = match bound {
            Some(_) => format!("{SELECT} WHERE started_at >= $1 GROUP BY source"),
            None => format!("{SELECT} GROUP BY source"),
        };

        let mut query = sqlx::query(&sql);
        if let Some(since) = bound {
            query = query.bind(since);
        }
        let rows = query.fetch_all(self.db.pool()).await?;

        let mut out = BTreeMap::new();
        for row in &rows {
            let source = get_string(row, "source")?;
            let last_success_at = match get_opt_string(row, "last_success_at") {
                Some(raw) => Some(str_to_ts(&raw, "collector_runs.finished_at")?),
                None => None,
            };
            out.insert(
                source.clone(),
                SourceCounters {
                    source,
                    runs: get_i64(row, "runs")?,
                    successful_runs: get_i64(row, "successful_runs")?,
                    failed_runs: get_i64(row, "failed_runs")?,
                    candidates: get_i64(row, "candidates")?,
                    new_items: get_i64(row, "new_items")?,
                    updated_items: get_i64(row, "updated_items")?,
                    parse_errors: get_i64(row, "parse_errors")?,
                    rate_limit_incidents: 0,
                    last_success_at,
                },
            );
        }
        Ok(out)
    }

    /// Count runs whose `detail` reports upstream rate limiting.
    ///
    /// The schema has no rate-limit column, so this matches
    /// [`RATE_LIMIT_DETAIL_MARKERS`] against a lowercased `detail`. `LOWER` on
    /// both sides keeps SQLite (ASCII-insensitive `LIKE`) and PostgreSQL
    /// (case-sensitive `LIKE`) in agreement.
    async fn rate_limit_incidents(
        &self,
        window: AnalyticsWindow,
    ) -> Result<BTreeMap<String, i64>, AnalyticsError> {
        let predicate = (1..=RATE_LIMIT_DETAIL_MARKERS.len())
            .map(|i| format!("LOWER(detail) LIKE ${i}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let next_placeholder = RATE_LIMIT_DETAIL_MARKERS.len() + 1;

        let bound = window.bound();
        let window_clause = match bound {
            Some(_) => format!(" AND started_at >= ${next_placeholder}"),
            None => String::new(),
        };
        let sql = format!(
            "SELECT source, COUNT(*) AS n FROM collector_runs
             WHERE detail IS NOT NULL AND ({predicate}){window_clause}
             GROUP BY source"
        );

        let mut query = sqlx::query(&sql);
        for marker in RATE_LIMIT_DETAIL_MARKERS {
            query = query.bind(*marker);
        }
        if let Some(since) = bound {
            query = query.bind(since);
        }
        let rows = query.fetch_all(self.db.pool()).await?;

        let mut out = BTreeMap::new();
        for row in &rows {
            out.insert(get_string(row, "source")?, get_i64(row, "n")?);
        }
        Ok(out)
    }

    /// Distinct logical vouchers attributed to each source by the canonical
    /// `vouchers.source` column, i.e. whoever won the creating insert.
    async fn unique_vouchers_by_source(
        &self,
        window: AnalyticsWindow,
    ) -> Result<BTreeMap<String, i64>, AnalyticsError> {
        const SELECT: &str = "SELECT source, COUNT(*) AS n FROM vouchers";

        let bound = window.bound();
        let sql = match bound {
            Some(_) => format!("{SELECT} WHERE first_seen_at >= $1 GROUP BY source"),
            None => format!("{SELECT} GROUP BY source"),
        };

        let mut query = sqlx::query(&sql);
        if let Some(since) = bound {
            query = query.bind(since);
        }
        let rows = query.fetch_all(self.db.pool()).await?;

        let mut out = BTreeMap::new();
        for row in &rows {
            out.insert(get_string(row, "source")?, get_i64(row, "n")?);
        }
        Ok(out)
    }

    /// Each source's earliest observation of each voucher.
    ///
    /// Aggregated to one row per `(voucher, source)` in SQL so the result set
    /// stays bounded by that product rather than by total observation volume;
    /// the attribution logic itself runs in Rust, where it is unit-testable.
    async fn first_observations(
        &self,
        window: AnalyticsWindow,
    ) -> Result<Vec<FirstObservation>, AnalyticsError> {
        const SELECT: &str =
            "SELECT voucher_id, source, MIN(observed_at) AS first_at FROM voucher_observations";

        let bound = window.bound();
        let sql = match bound {
            Some(_) => format!("{SELECT} WHERE observed_at >= $1 GROUP BY voucher_id, source"),
            None => format!("{SELECT} GROUP BY voucher_id, source"),
        };

        let mut query = sqlx::query(&sql);
        if let Some(since) = bound {
            query = query.bind(since);
        }
        let rows = query.fetch_all(self.db.pool()).await?;

        rows.iter()
            .map(|row| {
                Ok(FirstObservation {
                    voucher_id: get_string(row, "voucher_id")?,
                    source: get_string(row, "source")?,
                    observed_at: str_to_ts(
                        &get_string(row, "first_at")?,
                        "voucher_observations.observed_at",
                    )?,
                })
            })
            .collect()
    }
}

/// Convenience: summarize discovery for an already-fetched observation set.
///
/// Exposed so a caller holding rows from elsewhere (a replay harness, say) can
/// reuse the attribution logic without a database.
pub fn summarize(observations: &[FirstObservation]) -> BTreeMap<String, DiscoverySummary> {
    summarize_discovery(observations)
}

// ---------------------------------------------------------------------------
// Row helpers — no unwrap/expect on any database path.
// ---------------------------------------------------------------------------

fn get_i64(row: &AnyRow, column: &'static str) -> Result<i64, AnalyticsError> {
    // `Option` because an aggregate over zero rows is NULL on both backends.
    row.try_get::<Option<i64>, _>(column)
        .map(|value| value.unwrap_or(0))
        .map_err(|err| AnalyticsError::Decode {
            field: column,
            reason: err.to_string(),
        })
}

fn get_string(row: &AnyRow, column: &'static str) -> Result<String, AnalyticsError> {
    row.try_get::<String, _>(column)
        .map_err(|err| AnalyticsError::Decode {
            field: column,
            reason: err.to_string(),
        })
}

fn get_opt_string(row: &AnyRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn windows_render_a_bound_only_when_set() {
        assert_eq!(AnalyticsWindow::ALL_TIME.bound(), None);
        assert_eq!(AnalyticsWindow::default(), AnalyticsWindow::ALL_TIME);

        let now = Utc
            .with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
            .single()
            .expect("fixed test timestamp is unambiguous");
        let window = AnalyticsWindow::trailing(Duration::days(7), now);
        assert_eq!(window.since, Some(now - Duration::days(7)));
        // Fixed-width RFC3339 keeps TEXT comparison chronological.
        let bound = window.bound().expect("a bound is set");
        assert!(bound.starts_with("2026-08-01T12:00:00."));
        assert!(bound.ends_with('Z'));
    }

    #[test]
    fn rate_limit_markers_are_like_patterns() {
        assert!(!RATE_LIMIT_DETAIL_MARKERS.is_empty());
        for marker in RATE_LIMIT_DETAIL_MARKERS {
            assert!(marker.starts_with('%') && marker.ends_with('%'), "{marker}");
            assert_eq!(
                *marker,
                marker.to_lowercase(),
                "markers are matched against a lowercased column"
            );
        }
    }
}
