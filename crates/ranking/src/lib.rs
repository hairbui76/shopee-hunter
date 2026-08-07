//! Voucher ranking and owner-rule filtering (ROADMAP Phase 17).
//!
//! Two questions, deliberately answered by two different functions
//! (ARCHITECTURE.md §11):
//!
//! * [`score`] — *how useful is this voucher to the owner?* A numeric, fully
//!   explained judgement.
//! * [`eligibility`] — *does it pass the owner's hard rules?* A boolean-plus-
//!   reasons filter that never looks at the score.
//!
//! Keeping them apart means the owner can reduce notification noise without
//! deleting collectors, and can retune weights without changing what the
//! system is allowed to act on.
//!
//! # Guarantees
//!
//! * **Deterministic** — no clock, no randomness, no I/O. `now` is injected,
//!   and component ordering is a stable sort, so identical inputs always give
//!   byte-identical output.
//! * **Explainable** — [`ScoreBreakdown::total`] is exactly the sum of its
//!   components, and every component names its reason.
//! * **Float-free money** — every monetary value and ratio is a
//!   [`rust_decimal::Decimal`].
//!
//! # Boundaries
//!
//! This crate depends on `shopee-hunter-domain` and nothing else in the
//! workspace. It performs no I/O, sends no notifications, and makes no claim
//! decisions: clearing [`meets_auto_claim_threshold`] is necessary for an
//! auto-claim but never sufficient — the claim policy engine (Phase 14) still
//! weighs session health, retry budgets, and feature flags.
//!
//! # Example
//!
//! ```
//! use shopee_hunter_ranking::{eligibility, passes_notification_threshold, score, UserRules};
//! # fn demo(voucher: &shopee_hunter_domain::Voucher) {
//! let rules = UserRules::default();
//! let now = chrono::Utc::now();
//!
//! let decision = eligibility(voucher, &rules);
//! if decision.is_allowed() {
//!     let breakdown = score(voucher, &rules, now);
//!     if passes_notification_threshold(breakdown.total, &rules) {
//!         println!("{}", breakdown.explain());
//!     }
//! } else {
//!     println!("skipped: {:?}", decision.reasons());
//! }
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod eligibility;
pub mod rules;
pub mod score;

pub use eligibility::{eligibility, Eligibility};
pub use rules::{
    meets_auto_claim_threshold, passes_notification_threshold, RulesError, UserRules,
    DEFAULT_AUTO_CLAIM_THRESHOLD, DEFAULT_NOTIFICATION_THRESHOLD,
};
pub use score::{
    effective_discount, format_percent, format_vnd, rank_all, score, Ranked, ScoreBreakdown,
};
