//! Voucher applicability hints (ROADMAP Phase 28).
//!
//! Answers one question: *might this voucher help this planned purchase?*
//! The answer is graded, never binary, because the bot only sees voucher
//! metadata — it cannot see the product-level rules Shopee applies at
//! checkout. [`RelevanceLevel::Likely`] means "worth telling the owner about",
//! never "this will work".
//!
//! The verdict comes from a small, explicit rule rather than a heuristic
//! score, so every outcome is reproducible and explainable. Four independent
//! checks run — scope, payment method, minimum spend, purchase timing — and
//! each returns *matches*, *conflicts*, *unknown*, or *not constrained*:
//!
//! | condition | verdict |
//! |---|---|
//! | any check conflicts | [`Unlikely`](RelevanceLevel::Unlikely) |
//! | nothing positively matched | [`Unknown`](RelevanceLevel::Unknown) |
//! | something matched, nothing left unchecked | [`Likely`](RelevanceLevel::Likely) |
//! | something matched, something unchecked | [`Possible`](RelevanceLevel::Possible) |
//!
//! *Not constrained* — a voucher with no minimum spend, say — neither helps
//! nor hurts: an absent restriction is not evidence of a match, but it is not
//! a gap in knowledge either. That distinction is what stops "we know nothing
//! about this voucher" from being reported as [`RelevanceLevel::Possible`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use shopee_hunter_domain::Voucher;
use shopee_hunter_ranking::format_vnd;

use crate::constraint::{ScopeConstraint, VoucherConstraint};
use crate::uncertainty::{extend_unique, push_unique, Uncertainty};
use crate::watchlist::{WatchItem, Watchlist};

/// How likely a voucher is to be useful for a planned purchase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelevanceLevel {
    /// A stated restriction conflicts with the plan.
    Unlikely,
    /// Too little is known to say anything either way.
    Unknown,
    /// Nothing conflicts, but something material is unknown.
    Possible,
    /// Everything the bot can check lines up.
    Likely,
}

impl RelevanceLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unlikely => "UNLIKELY",
            Self::Unknown => "UNKNOWN",
            Self::Possible => "POSSIBLE",
            Self::Likely => "LIKELY",
        }
    }

    /// Whether this verdict is worth showing the owner.
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Likely | Self::Possible)
    }
}

/// A graded, explained applicability estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relevance {
    pub level: RelevanceLevel,
    /// Why, in evaluation order: scope, payment, minimum spend, timing.
    pub reasons: Vec<String>,
    /// What could not be checked.
    pub uncertainties: Vec<Uncertainty>,
    /// Estimated saving against the planned spend, when computable.
    ///
    /// `None` means "cannot be valued", never "zero".
    pub estimated_saving: Option<Decimal>,
}

impl Relevance {
    pub fn is_actionable(&self) -> bool {
        self.level.is_actionable()
    }

    /// Whether any reason contains `needle` (case-sensitive).
    pub fn has_reason(&self, needle: &str) -> bool {
        self.reasons.iter().any(|reason| reason.contains(needle))
    }

    /// Owner-facing explanation, uncertainties included.
    pub fn explain(&self) -> String {
        let mut out = format!("relevance {}", self.level.as_str());
        if let Some(saving) = self.estimated_saving {
            out.push_str(&format!(" (estimated saving {})", format_vnd(saving)));
        }
        for reason in &self.reasons {
            out.push_str(&format!("\n- {reason}"));
        }
        for uncertainty in &self.uncertainties {
            out.push_str(&format!("\n? {}", uncertainty.describe()));
        }
        out
    }
}

/// Result of one applicability check. A missing fact is never a "no", and an
/// absent restriction is never evidence of a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// The plan positively satisfies a stated restriction.
    Matches,
    /// The plan contradicts a stated restriction.
    Conflicts,
    /// A restriction exists but the plan lacks the data to check it.
    Unknown,
    /// The voucher states no such restriction.
    NotConstrained,
}

/// Estimate whether `voucher` might apply to `item`.
///
/// Deterministic: no clock, no randomness, no I/O. The purchase date, when the
/// owner set one, is compared against the voucher window; nothing else reads
/// time.
pub fn relevance(voucher: &Voucher, item: &WatchItem) -> Relevance {
    let constraint = VoucherConstraint::from_voucher(voucher);

    let mut reasons: Vec<String> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut uncertainties = vec![Uncertainty::CheckoutApplicabilityUnverified];
    extend_unique(&mut uncertainties, &constraint.unknowns);

    let checks = [
        check_scope(
            &constraint,
            item,
            &mut reasons,
            &mut blockers,
            &mut uncertainties,
        ),
        check_payment(
            &constraint,
            item,
            &mut reasons,
            &mut blockers,
            &mut uncertainties,
        ),
        check_min_spend(
            &constraint,
            item,
            &mut reasons,
            &mut blockers,
            &mut uncertainties,
        ),
        check_window(voucher, item, &mut reasons, &mut blockers),
    ];

    let matched = checks.iter().filter(|c| **c == Check::Matches).count();
    let gaps = checks.iter().filter(|c| **c == Check::Unknown).count();

    let level = if !blockers.is_empty() {
        RelevanceLevel::Unlikely
    } else if matched == 0 {
        // Nothing positively lines up: say so instead of implying a maybe.
        RelevanceLevel::Unknown
    } else if gaps == 0 {
        RelevanceLevel::Likely
    } else {
        RelevanceLevel::Possible
    };

    // Blockers come first: they are the reason for the verdict.
    let mut ordered = blockers;
    ordered.extend(reasons);

    let estimated_saving = estimate_saving(&constraint, item, level, &mut uncertainties);

    Relevance {
        level,
        reasons: ordered,
        uncertainties,
        estimated_saving,
    }
}

/// Relevance of one voucher against every watch item, best first.
///
/// Ordering is total and deterministic: level, then estimated saving, then
/// item id.
pub fn relevance_for_watchlist<'a>(
    voucher: &Voucher,
    watchlist: &'a Watchlist,
) -> Vec<(&'a WatchItem, Relevance)> {
    let mut scored: Vec<(&WatchItem, Relevance)> = watchlist
        .items()
        .iter()
        .map(|item| (item, relevance(voucher, item)))
        .collect();
    scored.sort_by(|(left_item, left), (right_item, right)| {
        right
            .level
            .cmp(&left.level)
            .then_with(|| {
                right
                    .estimated_saving
                    .unwrap_or(Decimal::ZERO)
                    .cmp(&left.estimated_saving.unwrap_or(Decimal::ZERO))
            })
            .then_with(|| left_item.id.cmp(&right_item.id))
    });
    scored
}

/// The watch item a voucher is most relevant to, if any is actionable.
///
/// Intended for notification filtering: no actionable item means the owner
/// probably does not need the alert.
pub fn best_relevance<'a>(
    voucher: &Voucher,
    watchlist: &'a Watchlist,
) -> Option<(&'a WatchItem, Relevance)> {
    relevance_for_watchlist(voucher, watchlist)
        .into_iter()
        .find(|(_, relevance)| relevance.is_actionable())
}

fn check_scope(
    constraint: &VoucherConstraint,
    item: &WatchItem,
    reasons: &mut Vec<String>,
    blockers: &mut Vec<String>,
    uncertainties: &mut Vec<Uncertainty>,
) -> Check {
    match &constraint.scope {
        ScopeConstraint::Platform => {
            reasons.push("platform-wide voucher, so any shop qualifies".to_string());
            Check::Matches
        }
        ScopeConstraint::Shop { shop_id } => {
            if item.shop_ids.is_empty() {
                reasons.push(format!(
                    "voucher is limited to shop {shop_id}; the watch item lists no shop"
                ));
                Check::Unknown
            } else if item.matches_shop(shop_id) {
                reasons.push(format!("voucher's shop {shop_id} is on the watch item"));
                Check::Matches
            } else {
                blockers.push(format!(
                    "voucher is limited to shop {shop_id}, which this purchase does not use"
                ));
                Check::Conflicts
            }
        }
        ScopeConstraint::Category { name } => {
            if item.category_tags.is_empty() {
                reasons.push(format!(
                    "voucher is limited to category {name}; the watch item has no category tags"
                ));
                Check::Unknown
            } else if item.matches_category(name) {
                reasons.push(format!(
                    "voucher's category {name} is tagged on the watch item"
                ));
                // The tag is the owner's, not Shopee's per-item category.
                push_unique(uncertainties, Uncertainty::CategoryCoverageAssumed);
                Check::Matches
            } else {
                blockers.push(format!(
                    "voucher is limited to category {name}, which this purchase is not tagged with"
                ));
                Check::Conflicts
            }
        }
        // Payment-scoped vouchers are decided by `check_payment`.
        ScopeConstraint::Payment { .. } => Check::Unknown,
        ScopeConstraint::Other { detail } => {
            reasons.push(format!(
                "voucher carries an unmodelled restriction: {detail}"
            ));
            Check::Unknown
        }
        ScopeConstraint::Unknown => {
            reasons.push("voucher does not state what it applies to".to_string());
            Check::Unknown
        }
    }
}

fn check_payment(
    constraint: &VoucherConstraint,
    item: &WatchItem,
    reasons: &mut Vec<String>,
    blockers: &mut Vec<String>,
    uncertainties: &mut Vec<Uncertainty>,
) -> Check {
    let Some(method) = constraint.payment_method.as_deref() else {
        return Check::NotConstrained;
    };
    match item.matches_payment_method(method) {
        Some(true) => {
            reasons.push(format!("voucher requires {method}, which you plan to use"));
            // The owner stated the method, so this is no longer unverified.
            uncertainties.retain(|u| *u != Uncertainty::PaymentMethodUnverified);
            Check::Matches
        }
        Some(false) => {
            blockers.push(format!(
                "voucher requires payment with {method}, but this purchase plans a different method"
            ));
            Check::Conflicts
        }
        None => {
            reasons.push(format!("voucher requires payment with {method}"));
            push_unique(uncertainties, Uncertainty::PaymentMethodUnverified);
            Check::Unknown
        }
    }
}

fn check_min_spend(
    constraint: &VoucherConstraint,
    item: &WatchItem,
    reasons: &mut Vec<String>,
    blockers: &mut Vec<String>,
    uncertainties: &mut Vec<Uncertainty>,
) -> Check {
    let Some(required) = constraint.min_spend else {
        // No stated minimum: nothing blocks the voucher, but an absent
        // restriction is not evidence that it applies either. The gap is
        // already recorded as `MinSpendUnknown`.
        return Check::NotConstrained;
    };
    match item.effective_spend() {
        None => {
            reasons.push(format!(
                "voucher needs a {} minimum; no planned spend is set",
                format_vnd(required)
            ));
            push_unique(uncertainties, Uncertainty::PlannedSpendUnknown);
            Check::Unknown
        }
        Some(spend) if spend >= required => {
            reasons.push(format!(
                "planned spend {} meets the {} minimum",
                format_vnd(spend),
                format_vnd(required)
            ));
            Check::Matches
        }
        Some(spend) => {
            blockers.push(format!(
                "voucher needs {} but the planned spend is {}",
                format_vnd(required),
                format_vnd(spend)
            ));
            Check::Conflicts
        }
    }
}

fn check_window(
    voucher: &Voucher,
    item: &WatchItem,
    reasons: &mut Vec<String>,
    blockers: &mut Vec<String>,
) -> Check {
    let Some(planned_at) = item.planned_purchase_at else {
        // The owner set no date, so timing cannot be judged either way.
        return Check::NotConstrained;
    };
    if let Some(end) = voucher.end_at {
        if end < planned_at {
            blockers.push("voucher ends before the planned purchase date".to_string());
            return Check::Conflicts;
        }
    }
    if let Some(start) = voucher.start_at {
        if start > planned_at {
            blockers.push("voucher starts after the planned purchase date".to_string());
            return Check::Conflicts;
        }
    }
    if voucher.start_at.is_some() || voucher.end_at.is_some() {
        reasons.push("voucher window covers the planned purchase date".to_string());
        return Check::Matches;
    }
    Check::NotConstrained
}

/// Value the voucher against the planned spend, when that is meaningful.
fn estimate_saving(
    constraint: &VoucherConstraint,
    item: &WatchItem,
    level: RelevanceLevel,
    uncertainties: &mut Vec<Uncertainty>,
) -> Option<Decimal> {
    if !level.is_actionable() {
        return None;
    }
    if constraint.targets_shipping() {
        // A watch item has no shipping estimate; use `optimize` with a basket
        // to value shipping vouchers.
        push_unique(uncertainties, Uncertainty::ShippingEstimateMissing);
        return None;
    }
    let spend = item.effective_spend()?;
    let estimate = constraint.estimate_discount(spend);
    extend_unique(uncertainties, &estimate.uncertainties);
    (estimate.amount > Decimal::ZERO).then_some(estimate.amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{voucher, VoucherSpec};
    use chrono::{DateTime, TimeZone, Utc};
    use shopee_hunter_domain::{VoucherScope, VoucherType};

    fn dec(value: i64) -> Decimal {
        Decimal::from(value)
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0)
            .single()
            .unwrap_or_default()
    }

    fn planned_item() -> WatchItem {
        WatchItem::new("w1", "Mechanical keyboard")
            .with_shop("shop-1")
            .with_category("Điện Tử")
            .with_planned_spend(dec(1_500_000))
    }

    #[test]
    fn matching_shop_and_met_minimum_is_likely() {
        let v = voucher(VoucherSpec {
            code: Some("SHOP50"),
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-1".into(),
            }),
            discount_amount: Some(50_000),
            min_spend: Some(200_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());

        assert_eq!(result.level, RelevanceLevel::Likely);
        assert!(result.is_actionable());
        assert!(result.has_reason("shop-1"));
        assert!(result.has_reason("meets the"));
        assert_eq!(result.estimated_saving, Some(dec(50_000)));
        // Even a Likely verdict never promises checkout applicability.
        assert!(result
            .uncertainties
            .contains(&Uncertainty::CheckoutApplicabilityUnverified));
    }

    #[test]
    fn platform_voucher_matches_any_shop() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(30_000),
            min_spend: Some(100_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());
        assert_eq!(result.level, RelevanceLevel::Likely);
        assert!(result
            .uncertainties
            .contains(&Uncertainty::ProductLevelRestrictionsUnknown));
    }

    #[test]
    fn wrong_shop_is_unlikely_and_says_why() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-999".into(),
            }),
            discount_amount: Some(50_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());

        assert_eq!(result.level, RelevanceLevel::Unlikely);
        assert!(!result.is_actionable());
        assert!(result.has_reason("shop-999"));
        assert_eq!(result.estimated_saving, None);
    }

    #[test]
    fn wrong_category_is_unlikely() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Category {
                name: "Thời Trang".into(),
            }),
            discount_amount: Some(20_000),
            ..VoucherSpec::default()
        });
        assert_eq!(
            relevance(&v, &planned_item()).level,
            RelevanceLevel::Unlikely
        );
    }

    #[test]
    fn matching_category_is_case_insensitive() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Category {
                name: "điện tử".into(),
            }),
            discount_amount: Some(20_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());
        assert_eq!(result.level, RelevanceLevel::Likely);
        assert!(result
            .uncertainties
            .contains(&Uncertainty::CategoryCoverageAssumed));
    }

    #[test]
    fn unmet_minimum_spend_is_unlikely_with_both_numbers() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(100_000),
            min_spend: Some(5_000_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());

        assert_eq!(result.level, RelevanceLevel::Unlikely);
        assert!(result.has_reason("5.000.000₫"));
        assert!(result.has_reason("1.500.000₫"));
    }

    #[test]
    fn unknown_planned_spend_downgrades_to_possible() {
        let item = WatchItem::new("w1", "keyboard").with_shop("shop-1");
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-1".into(),
            }),
            discount_amount: Some(50_000),
            min_spend: Some(200_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &item);

        assert_eq!(result.level, RelevanceLevel::Possible);
        assert!(result
            .uncertainties
            .contains(&Uncertainty::PlannedSpendUnknown));
        // No planned spend means no way to value the voucher.
        assert_eq!(result.estimated_saving, None);
    }

    #[test]
    fn unknown_scope_with_met_minimum_is_possible() {
        let v = voucher(VoucherSpec {
            discount_amount: Some(50_000),
            min_spend: Some(200_000),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());

        assert_eq!(result.level, RelevanceLevel::Possible);
        assert!(result.uncertainties.contains(&Uncertainty::ScopeUnknown));
        assert_eq!(result.estimated_saving, Some(dec(50_000)));
    }

    #[test]
    fn nothing_known_is_unknown_not_unlikely() {
        let item = WatchItem::new("w1", "keyboard");
        let v = voucher(VoucherSpec::default());
        let result = relevance(&v, &item);

        assert_eq!(result.level, RelevanceLevel::Unknown);
        assert!(!result.is_actionable());
        assert!(result.uncertainties.contains(&Uncertainty::ScopeUnknown));
        assert!(result
            .uncertainties
            .contains(&Uncertainty::DiscountValueUnknown));
    }

    #[test]
    fn payment_restriction_is_judged_when_the_owner_decided() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            payment_method: Some("ShopeePay"),
            discount_amount: Some(30_000),
            ..VoucherSpec::default()
        });

        let matching = planned_item().with_preferred_payment_method("shopeepay");
        let result = relevance(&v, &matching);
        assert_eq!(result.level, RelevanceLevel::Likely);
        assert!(!result
            .uncertainties
            .contains(&Uncertainty::PaymentMethodUnverified));

        let clashing = planned_item().with_preferred_payment_method("MoMo");
        let result = relevance(&v, &clashing);
        assert_eq!(result.level, RelevanceLevel::Unlikely);
        assert!(result.has_reason("ShopeePay"));

        let undecided = planned_item();
        let result = relevance(&v, &undecided);
        assert!(result
            .uncertainties
            .contains(&Uncertainty::PaymentMethodUnverified));
    }

    #[test]
    fn purchase_date_outside_the_voucher_window_is_unlikely() {
        let expiring = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(50_000),
            start_at: Some(at(1)),
            end_at: Some(at(5)),
            ..VoucherSpec::default()
        });
        let item = planned_item().with_planned_purchase_at(at(9));
        let result = relevance(&expiring, &item);
        assert_eq!(result.level, RelevanceLevel::Unlikely);
        assert!(result.has_reason("ends before"));

        let later = planned_item().with_planned_purchase_at(at(3));
        assert_eq!(relevance(&expiring, &later).level, RelevanceLevel::Likely);

        let future = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(50_000),
            start_at: Some(at(20)),
            ..VoucherSpec::default()
        });
        let result = relevance(&future, &later);
        assert_eq!(result.level, RelevanceLevel::Unlikely);
        assert!(result.has_reason("starts after"));
    }

    #[test]
    fn shipping_vouchers_are_not_valued_without_a_basket() {
        let v = voucher(VoucherSpec {
            voucher_type: VoucherType::Freeship,
            scope: Some(VoucherScope::Platform),
            ..VoucherSpec::default()
        });
        let result = relevance(&v, &planned_item());

        assert!(result.is_actionable());
        assert_eq!(result.estimated_saving, None);
        assert!(result
            .uncertainties
            .contains(&Uncertainty::ShippingEstimateMissing));
    }

    #[test]
    fn watchlist_ranking_is_deterministic_and_prefers_the_best_match() {
        let mut watchlist = Watchlist::new();
        watchlist
            .add(
                WatchItem::new("w1", "keyboard")
                    .with_shop("shop-1")
                    .with_planned_spend(dec(1_000_000)),
            )
            .expect("added");
        watchlist
            .add(
                WatchItem::new("w2", "monitor")
                    .with_shop("shop-2")
                    .with_planned_spend(dec(3_000_000)),
            )
            .expect("added");

        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-2".into(),
            }),
            discount_amount: Some(100_000),
            ..VoucherSpec::default()
        });

        let ranked = relevance_for_watchlist(&v, &watchlist);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0.id, "w2");
        assert_eq!(ranked[0].1.level, RelevanceLevel::Likely);
        assert_eq!(ranked[1].1.level, RelevanceLevel::Unlikely);

        // Stable across repeated calls.
        let again = relevance_for_watchlist(&v, &watchlist);
        assert_eq!(again[0].0.id, ranked[0].0.id);

        let best = best_relevance(&v, &watchlist).expect("an actionable item");
        assert_eq!(best.0.id, "w2");
    }

    #[test]
    fn best_relevance_is_none_when_nothing_is_actionable() {
        let mut watchlist = Watchlist::new();
        watchlist
            .add(WatchItem::new("w1", "keyboard").with_shop("shop-1"))
            .expect("added");
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-999".into(),
            }),
            discount_amount: Some(50_000),
            ..VoucherSpec::default()
        });
        assert!(best_relevance(&v, &watchlist).is_none());
    }

    #[test]
    fn explanations_list_reasons_and_uncertainties() {
        let v = voucher(VoucherSpec {
            scope: Some(VoucherScope::Platform),
            discount_percent: Some(20),
            min_spend: Some(200_000),
            ..VoucherSpec::default()
        });
        let text = relevance(&v, &planned_item()).explain();

        assert!(text.starts_with("relevance LIKELY"));
        assert!(text.contains("estimated saving"));
        assert!(text.contains("- platform-wide"));
        assert!(text.contains("? "));
    }
}
