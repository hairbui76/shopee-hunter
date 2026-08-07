//! Purchase decision support (ROADMAP Phases 28 & 29).
//!
//! Two questions, both answered locally and both advisory:
//!
//! * **Phase 28 — is this voucher relevant?** [`relevance`] compares a voucher
//!   against a [`WatchItem`] the owner plans to buy and returns a graded
//!   [`RelevanceLevel`] with reasons.
//! * **Phase 29 — which vouchers should I use together?** [`optimize`] takes a
//!   [`Basket`] the owner typed and returns the best-valued compatible
//!   combination, what each voucher contributes, why the alternatives lose,
//!   and which assumptions are shaky.
//!
//! # Not automation
//!
//! This crate performs **no I/O of any kind**: no Shopee requests, no cart
//! reads, no checkout, no payment. It is arithmetic over owner-supplied
//! numbers and voucher metadata the bot already holds. Nothing here should
//! ever be wired to an action that spends money — that is explicitly out of
//! scope for the project (CLAUDE.md "Explicitly out of scope").
//!
//! # Guarantees
//!
//! * **Deterministic** — no clock, no randomness, no I/O; every ordering has an
//!   explicit tie-breaker, so identical inputs give byte-identical output. The
//!   only time input is the owner's own planned purchase date.
//! * **Float-free money** — every amount is a [`rust_decimal::Decimal`].
//! * **Honest about ignorance** — a missing fact becomes an [`Uncertainty`],
//!   never a default that reads as a fact, and no result ever claims a voucher
//!   *will* apply at checkout.
//!
//! # Example
//!
//! ```
//! use rust_decimal::Decimal;
//! use shopee_hunter_planning::{optimize, Basket, BasketShop};
//! # fn demo(vouchers: &[shopee_hunter_domain::Voucher]) {
//! let basket = Basket::new()
//!     .with_shop(BasketShop::new("shop-1", Decimal::from(300_000)))
//!     .with_shipping_estimate(Decimal::from(35_000));
//!
//! let plan = optimize(&basket, vouchers);
//! println!("{}", plan.explain());
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod basket;
pub mod constraint;
pub mod error;
pub mod optimize;
pub mod relevance;
pub mod uncertainty;
pub mod watchlist;

#[cfg(test)]
mod test_support;

pub use basket::{Basket, BasketItem, BasketShop};
pub use constraint::{DiscountEstimate, DiscountRule, ScopeConstraint, VoucherConstraint};
pub use error::PlanningError;
pub use optimize::{
    optimize, optimize_with, Alternative, ExcludedVoucher, NotApplicable, Plan, SelectedVoucher,
    StackGroup, StackingPolicy, VoucherRef,
};
pub use relevance::{
    best_relevance, relevance, relevance_for_watchlist, Relevance, RelevanceLevel,
};
pub use uncertainty::Uncertainty;
pub use watchlist::{WatchItem, Watchlist};
