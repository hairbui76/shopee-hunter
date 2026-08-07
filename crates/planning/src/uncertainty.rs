//! Explicit uncertainty.
//!
//! This crate estimates whether a voucher *might* help a planned purchase. It
//! never has Shopee's checkout engine in front of it, so every gap between what
//! the bot knows and what checkout will actually do is named here and carried
//! in the output (ROADMAP Phase 29: "Unknown constraints are surfaced instead
//! of guessed as facts").
//!
//! Two rules keep this honest:
//!
//! * a missing fact becomes an [`Uncertainty`], never a default value that
//!   silently reads as a fact;
//! * [`Uncertainty::CheckoutApplicabilityUnverified`] is attached to *every*
//!   relevance verdict and every plan, because no result from this crate is
//!   ever a promise that a voucher will apply at checkout.

use serde::{Deserialize, Serialize};

/// A named gap in what the planner can actually know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Uncertainty {
    /// Standing caveat on every result: only checkout decides for real.
    CheckoutApplicabilityUnverified,
    /// Shopee vouchers can be restricted to product sets the bot cannot see.
    ProductLevelRestrictionsUnknown,
    /// The voucher states no scope, so what it applies to is a guess.
    ScopeUnknown,
    /// A category voucher was matched using shop-level tags supplied by the
    /// owner, not per-item category data.
    CategoryCoverageAssumed,
    /// The voucher demands a payment method; the bot cannot verify the owner
    /// will actually pay with it.
    PaymentMethodUnverified,
    /// A percentage voucher with no published cap: the real ceiling is unknown.
    DiscountCapUnknown,
    /// Nothing numeric is known about the discount.
    DiscountValueUnknown,
    /// The voucher states no minimum spend; there may still be one.
    MinSpendUnknown,
    /// A shipping voucher cannot be valued without a shipping estimate.
    ShippingEstimateMissing,
    /// Which vouchers may be combined is a configured assumption, not a fact
    /// read from Shopee.
    StackingRulesAssumed,
    /// Discounts are summed as if independent; real checkout may apply them
    /// sequentially to a shrinking subtotal.
    DiscountsAssumedIndependent,
    /// Too many candidates to enumerate exhaustively; the search was capped.
    CandidateSetTruncated,
    /// The voucher has no usable validity window.
    VoucherWindowUnknown,
    /// The watch item states no planned spend, so minimum-spend rules cannot
    /// be checked.
    PlannedSpendUnknown,
}

impl Uncertainty {
    /// Stable machine label.
    pub fn code(&self) -> &'static str {
        match self {
            Self::CheckoutApplicabilityUnverified => "CHECKOUT_APPLICABILITY_UNVERIFIED",
            Self::ProductLevelRestrictionsUnknown => "PRODUCT_LEVEL_RESTRICTIONS_UNKNOWN",
            Self::ScopeUnknown => "SCOPE_UNKNOWN",
            Self::CategoryCoverageAssumed => "CATEGORY_COVERAGE_ASSUMED",
            Self::PaymentMethodUnverified => "PAYMENT_METHOD_UNVERIFIED",
            Self::DiscountCapUnknown => "DISCOUNT_CAP_UNKNOWN",
            Self::DiscountValueUnknown => "DISCOUNT_VALUE_UNKNOWN",
            Self::MinSpendUnknown => "MIN_SPEND_UNKNOWN",
            Self::ShippingEstimateMissing => "SHIPPING_ESTIMATE_MISSING",
            Self::StackingRulesAssumed => "STACKING_RULES_ASSUMED",
            Self::DiscountsAssumedIndependent => "DISCOUNTS_ASSUMED_INDEPENDENT",
            Self::CandidateSetTruncated => "CANDIDATE_SET_TRUNCATED",
            Self::VoucherWindowUnknown => "VOUCHER_WINDOW_UNKNOWN",
            Self::PlannedSpendUnknown => "PLANNED_SPEND_UNKNOWN",
        }
    }

    /// Owner-facing sentence explaining what is not known.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::CheckoutApplicabilityUnverified => {
                "estimate only: applicability is confirmed by Shopee at checkout, not here"
            }
            Self::ProductLevelRestrictionsUnknown => {
                "the voucher may exclude specific products, which the bot cannot see"
            }
            Self::ScopeUnknown => "the voucher does not state what it applies to",
            Self::CategoryCoverageAssumed => {
                "category match uses the tags you assigned to the shop, not per-item categories"
            }
            Self::PaymentMethodUnverified => {
                "the voucher requires a specific payment method that was not verified"
            }
            Self::DiscountCapUnknown => {
                "percentage voucher with no published cap; the real limit is unknown"
            }
            Self::DiscountValueUnknown => "the voucher states no usable discount value",
            Self::MinSpendUnknown => "the voucher states no minimum spend; one may still apply",
            Self::ShippingEstimateMissing => {
                "no shipping estimate was provided, so shipping savings cannot be valued"
            }
            Self::StackingRulesAssumed => {
                "which vouchers may be combined is a configured assumption, not a Shopee fact"
            }
            Self::DiscountsAssumedIndependent => {
                "discounts are summed independently; checkout may apply them in sequence"
            }
            Self::CandidateSetTruncated => {
                "too many applicable vouchers to evaluate every combination; the best-valued ones were used"
            }
            Self::VoucherWindowUnknown => "the voucher has no usable start/end time",
            Self::PlannedSpendUnknown => {
                "no planned spend was set, so minimum-spend rules could not be checked"
            }
        }
    }
}

/// Append `value` if absent, keeping insertion order stable.
///
/// Deterministic output matters more than set semantics here: the same inputs
/// must always produce the same explanation text, in the same order.
pub(crate) fn push_unique(list: &mut Vec<Uncertainty>, value: Uncertainty) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// Merge `extra` into `list` without duplicates, preserving order.
pub(crate) fn extend_unique(list: &mut Vec<Uncertainty>, extra: &[Uncertainty]) {
    for value in extra {
        push_unique(list, *value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_descriptions_are_unique() {
        let all = [
            Uncertainty::CheckoutApplicabilityUnverified,
            Uncertainty::ProductLevelRestrictionsUnknown,
            Uncertainty::ScopeUnknown,
            Uncertainty::CategoryCoverageAssumed,
            Uncertainty::PaymentMethodUnverified,
            Uncertainty::DiscountCapUnknown,
            Uncertainty::DiscountValueUnknown,
            Uncertainty::MinSpendUnknown,
            Uncertainty::ShippingEstimateMissing,
            Uncertainty::StackingRulesAssumed,
            Uncertainty::DiscountsAssumedIndependent,
            Uncertainty::CandidateSetTruncated,
            Uncertainty::VoucherWindowUnknown,
            Uncertainty::PlannedSpendUnknown,
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(|u| u.code()).collect();
        let texts: std::collections::BTreeSet<_> = all.iter().map(|u| u.describe()).collect();
        assert_eq!(codes.len(), all.len());
        assert_eq!(texts.len(), all.len());
        assert!(all.iter().all(|u| !u.describe().is_empty()));
    }

    #[test]
    fn deduplication_preserves_first_seen_order() {
        let mut list = Vec::new();
        push_unique(&mut list, Uncertainty::ScopeUnknown);
        push_unique(&mut list, Uncertainty::MinSpendUnknown);
        push_unique(&mut list, Uncertainty::ScopeUnknown);
        extend_unique(
            &mut list,
            &[
                Uncertainty::MinSpendUnknown,
                Uncertainty::DiscountCapUnknown,
            ],
        );
        assert_eq!(
            list,
            vec![
                Uncertainty::ScopeUnknown,
                Uncertainty::MinSpendUnknown,
                Uncertainty::DiscountCapUnknown
            ]
        );
    }

    #[test]
    fn uncertainty_round_trips_through_serde() {
        let encoded =
            serde_json::to_string(&Uncertainty::StackingRulesAssumed).expect("serializes");
        assert_eq!(encoded, "\"STACKING_RULES_ASSUMED\"");
        let decoded: Uncertainty = serde_json::from_str(&encoded).expect("round trips");
        assert_eq!(decoded, Uncertainty::StackingRulesAssumed);
    }
}
