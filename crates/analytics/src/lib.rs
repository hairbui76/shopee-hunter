//! Source quality analytics (ROADMAP Phase 27).
//!
//! Answers one operational question: **which discovery sources are worth
//! keeping, and which are wasting the polling budget?** It reads the history
//! the watcher has already persisted — `collector_runs`, `vouchers`, and
//! `voucher_observations` — and produces per-source statistics, an operational
//! quality score, and a degradation *recommendation*.
//!
//! # What this crate is not
//!
//! * **Not voucher ranking.** [`SourceQualityScore`] judges collectors, never
//!   vouchers. Feeding it into `shopee-hunter-ranking` would let an
//!   infrastructure problem quietly change what the owner is shown.
//! * **Not a controller.** [`should_degrade`] returns advice. Nothing here
//!   disables a collector, edits configuration, or writes to the database —
//!   every statement it issues is a `SELECT`.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`ratio`] | Exact fixed-point rates; no floating point anywhere. |
//! | [`discovery`] | Pure first-discovery attribution and lead-time measurement. |
//! | [`stats`] | [`SourceStats`] and the pure constructor that derives its rates. |
//! | [`quality`] | The operational score and its documented weights. |
//! | [`degrade`] | Degradation recommendations. |
//! | [`repository`] | Read-only aggregation over the shared `Database`. |
//! | [`error`] | [`AnalyticsError`]. |
//!
//! # Determinism
//!
//! Given the same rows, every function here produces the same output: sources
//! are keyed through `BTreeMap`/`BTreeSet`, first-discovery ties break on
//! source name, and all arithmetic is integer. Reports can therefore be
//! diffed between runs.
//!
//! # Example
//!
//! ```no_run
//! use chrono::{Duration, Utc};
//! use shopee_hunter_analytics::{AnalyticsRepository, AnalyticsWindow};
//! # async fn run(db: &shopee_hunter_storage::Database) -> Result<(), Box<dyn std::error::Error>> {
//! let analytics = AnalyticsRepository::new(db);
//! let window = AnalyticsWindow::trailing(Duration::days(7), Utc::now());
//!
//! for report in analytics.report(window).await? {
//!     println!("{}", report.quality.explain());
//!     if let Some(action) = &report.recommendation {
//!         // A recommendation only — the supervisor decides whether to apply it.
//!         println!("  suggested: {} — {}", action.kind(), action.reason());
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod degrade;
pub mod discovery;
pub mod error;
pub mod quality;
pub mod ratio;
pub mod repository;
pub mod stats;

pub use degrade::{should_degrade, DegradeAction};
pub use discovery::{summarize_discovery, DiscoverySummary, FirstObservation};
pub use error::AnalyticsError;
pub use quality::{quality_score, SourceQualityScore, BASELINE_SCORE, MIN_RUNS_FOR_JUDGEMENT};
pub use ratio::{Ratio, BASIS_POINTS_PER_WHOLE};
pub use repository::{
    AnalyticsRepository, AnalyticsWindow, SourceReport, RATE_LIMIT_DETAIL_MARKERS,
};
pub use stats::{SourceCounters, SourceStats};
