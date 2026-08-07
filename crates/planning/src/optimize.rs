//! Voucher combination analysis (ROADMAP Phase 29).
//!
//! Given a basket the owner typed and the vouchers the bot knows about, this
//! module estimates which *combination* saves the most money, and explains why
//! the alternatives lose.
//!
//! **This is analysis, not automation.** Nothing here talks to Shopee, adds
//! anything to a cart, or starts a checkout. The output is a recommendation
//! for a human.
//!
//! # How the search works
//!
//! 1. every voucher is restated as a [`VoucherConstraint`] and checked against
//!    the basket; anything that cannot apply is moved to [`Plan::excluded`]
//!    with a typed reason;
//! 2. survivors are ranked by estimated value and capped at
//!    [`StackingPolicy::max_candidates`], so the search space is bounded;
//! 3. every subset of the survivors is enumerated, keeping those the stacking
//!    policy allows, and the highest-saving one wins.
//!
//! Exhaustive enumeration over a capped candidate set keeps the result
//! *exactly* optimal for the modelled inputs while staying deterministic and
//! fast (at most `2^max_candidates` cheap checks).
//!
//! # What is assumed, not known
//!
//! Shopee does not publish its stacking rules, so [`StackingPolicy`] is a
//! configured assumption and every plan carries
//! [`Uncertainty::StackingRulesAssumed`]. Discounts are summed as if
//! independent, which is recorded as [`Uncertainty::DiscountsAssumedIndependent`].

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use shopee_hunter_domain::{Voucher, VoucherStatus};
use shopee_hunter_ranking::format_vnd;

use crate::basket::Basket;
use crate::constraint::{ScopeConstraint, VoucherConstraint};
use crate::uncertainty::{extend_unique, push_unique, Uncertainty};

/// Hard ceiling on the exhaustive search, independent of configuration:
/// `2^16` subsets is still trivial, and it bounds worst-case work.
const MAX_SEARCH_CANDIDATES: usize = 16;

/// How many losing combinations to explain.
const MAX_ALTERNATIVES: usize = 5;

/// Which vouchers may be combined.
///
/// Shopee does not publish these rules, so this is an owner-configurable
/// assumption. The defaults match the commonly observed "one platform + one
/// shop + one shipping + one payment voucher" behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackingPolicy {
    pub max_platform: usize,
    /// Limit per individual shop, not across all shops.
    pub max_shop_per_shop: usize,
    pub max_freeship: usize,
    pub max_payment: usize,
    /// Vouchers whose scope could not be classified.
    pub max_other: usize,
    /// Overall ceiling, when the owner wants one.
    pub max_total: Option<usize>,
    /// Most vouchers considered by the exhaustive search.
    pub max_candidates: usize,
}

impl Default for StackingPolicy {
    fn default() -> Self {
        Self {
            max_platform: 1,
            max_shop_per_shop: 1,
            max_freeship: 1,
            max_payment: 1,
            max_other: 1,
            max_total: None,
            max_candidates: 12,
        }
    }
}

/// Which stacking slot a voucher competes for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StackGroup {
    Platform,
    Shop(String),
    Freeship,
    Payment,
    Other,
}

/// Identifies a voucher in the input slice without depending on its id type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherRef {
    /// Position in the slice passed to [`optimize`].
    pub index: usize,
    /// The voucher's id, rendered.
    pub id: String,
    /// Code when present, otherwise a shortened title.
    pub label: String,
}

/// A voucher in the recommended combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedVoucher {
    pub voucher: VoucherRef,
    pub estimated_discount: Decimal,
    /// How the number was derived.
    pub basis: String,
    /// The amount it was applied to.
    pub applies_to: String,
}

/// Why a voucher cannot take part in any combination.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotApplicable {
    #[error("voucher status is {status}")]
    TerminalStatus { status: &'static str },
    #[error("shop {shop_id} is not in the basket")]
    ShopNotInBasket { shop_id: String },
    #[error("no basket shop is tagged with category {category}")]
    CategoryNotInBasket { category: String },
    #[error("needs {} but only {} qualifies", format_vnd(*required), format_vnd(*available))]
    MinSpendNotMet {
        required: Decimal,
        available: Decimal,
    },
    #[error("requires payment with {required}, but {preferred} is planned")]
    PaymentMethodMismatch { required: String, preferred: String },
    #[error("not evaluated: only the {limit} best-valued vouchers were searched")]
    NotEvaluated { limit: usize },
}

/// A voucher that was left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedVoucher {
    pub voucher: VoucherRef,
    pub reason: NotApplicable,
}

/// A combination that lost, and by how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternative {
    pub labels: Vec<String>,
    pub estimated_total_discount: Decimal,
    pub why_it_loses: String,
}

/// The recommended combination plus everything needed to sanity-check it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub merchandise_subtotal: Decimal,
    pub shipping_estimate: Option<Decimal>,
    /// The winning combination; empty when nothing applies.
    pub selected: Vec<SelectedVoucher>,
    pub estimated_total_discount: Decimal,
    /// Basket total minus the estimated discount, never below zero.
    pub estimated_total_after_discount: Decimal,
    /// True when the combined discount was clamped to the basket total.
    pub capped_at_basket_total: bool,
    pub alternatives: Vec<Alternative>,
    pub excluded: Vec<ExcludedVoucher>,
    pub uncertainties: Vec<Uncertainty>,
}

impl Plan {
    /// Whether any voucher was recommended.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Labels of the selected vouchers, in selection order.
    pub fn selected_labels(&self) -> Vec<&str> {
        self.selected
            .iter()
            .map(|choice| choice.voucher.label.as_str())
            .collect()
    }

    /// Owner-facing explanation covering choice, alternatives, and assumptions.
    pub fn explain(&self) -> String {
        let total_before =
            self.merchandise_subtotal + self.shipping_estimate.unwrap_or(Decimal::ZERO);
        let mut out = format!(
            "plan: estimated saving {} on {} ({} after vouchers)",
            format_vnd(self.estimated_total_discount),
            format_vnd(total_before),
            format_vnd(self.estimated_total_after_discount)
        );
        if self.selected.is_empty() {
            out.push_str("\n- no voucher applies to this basket");
        }
        for choice in &self.selected {
            out.push_str(&format!(
                "\n+ {}: {} ({}, on {})",
                choice.voucher.label,
                format_vnd(choice.estimated_discount),
                choice.basis,
                choice.applies_to
            ));
        }
        if self.capped_at_basket_total {
            out.push_str("\n! combined discount was capped at the basket total");
        }
        for alternative in &self.alternatives {
            out.push_str(&format!(
                "\n- {} ({}): {}",
                alternative.labels.join(" + "),
                format_vnd(alternative.estimated_total_discount),
                alternative.why_it_loses
            ));
        }
        for excluded in &self.excluded {
            out.push_str(&format!(
                "\nx {}: {}",
                excluded.voucher.label, excluded.reason
            ));
        }
        for uncertainty in &self.uncertainties {
            out.push_str(&format!("\n? {}", uncertainty.describe()));
        }
        out
    }
}

/// One voucher that can take part in a combination.
#[derive(Debug, Clone)]
struct Candidate {
    reference: VoucherRef,
    group: StackGroup,
    estimate: Decimal,
    basis: String,
    applies_to: String,
    targets_shipping: bool,
    uncertainties: Vec<Uncertainty>,
}

/// Analyse a basket against known vouchers using the default stacking policy.
///
/// Deterministic: identical inputs always produce an identical [`Plan`],
/// including the order of selections, alternatives, and uncertainties.
pub fn optimize(basket: &Basket, vouchers: &[Voucher]) -> Plan {
    optimize_with(basket, vouchers, &StackingPolicy::default())
}

/// [`optimize`] with an explicit stacking policy.
pub fn optimize_with(basket: &Basket, vouchers: &[Voucher], policy: &StackingPolicy) -> Plan {
    let merchandise_subtotal = basket.merchandise_subtotal();
    let shipping_estimate = basket.shipping_estimate;

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut excluded: Vec<ExcludedVoucher> = Vec::new();

    for (index, voucher) in vouchers.iter().enumerate() {
        match evaluate(index, voucher, basket) {
            Ok(candidate) => candidates.push(candidate),
            Err(exclusion) => excluded.push(exclusion),
        }
    }

    // Deterministic order: best value first, then label, then input position.
    candidates.sort_by(|left, right| {
        right
            .estimate
            .cmp(&left.estimate)
            .then_with(|| left.reference.label.cmp(&right.reference.label))
            .then_with(|| left.reference.index.cmp(&right.reference.index))
    });

    let limit = policy.max_candidates.min(MAX_SEARCH_CANDIDATES);
    let mut truncated = false;
    if candidates.len() > limit {
        truncated = true;
        for dropped in candidates.split_off(limit) {
            excluded.push(ExcludedVoucher {
                voucher: dropped.reference,
                reason: NotApplicable::NotEvaluated { limit },
            });
        }
    }

    let combinations =
        rank_combinations(&candidates, policy, merchandise_subtotal, shipping_estimate);
    let (best_mask, best_total, best_capped) =
        combinations
            .first()
            .copied()
            .unwrap_or((0usize, Decimal::ZERO, false));

    let selected: Vec<SelectedVoucher> = candidates
        .iter()
        .enumerate()
        .filter(|(position, _)| best_mask & (1usize << position) != 0)
        .map(|(_, candidate)| SelectedVoucher {
            voucher: candidate.reference.clone(),
            estimated_discount: candidate.estimate,
            basis: candidate.basis.clone(),
            applies_to: candidate.applies_to.clone(),
        })
        .collect();

    let alternatives = build_alternatives(&candidates, &combinations, best_total);

    let mut uncertainties = vec![Uncertainty::CheckoutApplicabilityUnverified];
    if !candidates.is_empty() {
        push_unique(&mut uncertainties, Uncertainty::StackingRulesAssumed);
    }
    if selected.len() > 1 {
        push_unique(&mut uncertainties, Uncertainty::DiscountsAssumedIndependent);
    }
    if truncated {
        push_unique(&mut uncertainties, Uncertainty::CandidateSetTruncated);
    }
    if shipping_estimate.is_none() && candidates.iter().any(|c| c.targets_shipping) {
        push_unique(&mut uncertainties, Uncertainty::ShippingEstimateMissing);
    }
    for (position, candidate) in candidates.iter().enumerate() {
        if best_mask & (1usize << position) != 0 {
            extend_unique(&mut uncertainties, &candidate.uncertainties);
        }
    }

    let total_before = merchandise_subtotal + shipping_estimate.unwrap_or(Decimal::ZERO);
    Plan {
        merchandise_subtotal,
        shipping_estimate,
        selected,
        estimated_total_discount: best_total,
        estimated_total_after_discount: (total_before - best_total).max(Decimal::ZERO),
        capped_at_basket_total: best_capped,
        alternatives,
        excluded,
        uncertainties,
    }
}

/// Check one voucher against the basket.
fn evaluate(
    index: usize,
    voucher: &Voucher,
    basket: &Basket,
) -> Result<Candidate, ExcludedVoucher> {
    let constraint = VoucherConstraint::from_voucher(voucher);
    let reference = VoucherRef {
        index,
        id: voucher.id.to_string(),
        label: constraint.label.clone(),
    };
    let exclude = |reason: NotApplicable| ExcludedVoucher {
        voucher: reference.clone(),
        reason,
    };

    // A voucher that can no longer be used is not a planning option. No clock
    // is read: this is the status the domain already recorded.
    if let Some(status) = terminal_status(voucher.status) {
        return Err(exclude(NotApplicable::TerminalStatus { status }));
    }

    let mut uncertainties = constraint.unknowns.clone();

    if let Some(required) = constraint.payment_method.as_deref() {
        match basket.matches_payment_method(required) {
            Some(true) => uncertainties.retain(|u| *u != Uncertainty::PaymentMethodUnverified),
            Some(false) => {
                return Err(exclude(NotApplicable::PaymentMethodMismatch {
                    required: required.to_string(),
                    preferred: basket
                        .payment_method
                        .clone()
                        .unwrap_or_else(|| "another method".to_string()),
                }))
            }
            None => push_unique(&mut uncertainties, Uncertainty::PaymentMethodUnverified),
        }
    }

    // The amount that must clear the minimum spend, which is not always the
    // amount the discount applies to (shipping vouchers).
    let (qualifying, applies_to) = match &constraint.scope {
        ScopeConstraint::Shop { shop_id } => match basket.shop_subtotal(shop_id) {
            Some(subtotal) => (subtotal, format!("shop {shop_id} subtotal")),
            None => {
                return Err(exclude(NotApplicable::ShopNotInBasket {
                    shop_id: shop_id.clone(),
                }))
            }
        },
        ScopeConstraint::Category { name } => {
            let subtotal = basket.category_subtotal(name);
            if subtotal.is_zero() {
                return Err(exclude(NotApplicable::CategoryNotInBasket {
                    category: name.clone(),
                }));
            }
            push_unique(&mut uncertainties, Uncertainty::CategoryCoverageAssumed);
            (subtotal, format!("category {name} subtotal"))
        }
        _ => (basket.merchandise_subtotal(), "basket subtotal".to_string()),
    };

    if let Some(required) = constraint.min_spend {
        if qualifying < required {
            return Err(exclude(NotApplicable::MinSpendNotMet {
                required,
                available: qualifying,
            }));
        }
    }

    let targets_shipping = constraint.targets_shipping();
    let (base, applies_to) = if targets_shipping {
        (
            basket.shipping_estimate.unwrap_or(Decimal::ZERO),
            "shipping estimate".to_string(),
        )
    } else {
        (qualifying, applies_to)
    };

    let estimate = constraint.estimate_discount(base);
    extend_unique(&mut uncertainties, &estimate.uncertainties);

    Ok(Candidate {
        reference,
        group: stack_group(&constraint, targets_shipping),
        estimate: estimate.amount,
        basis: estimate.basis,
        applies_to: format!("{applies_to} {}", format_vnd(base)),
        targets_shipping,
        uncertainties,
    })
}

fn terminal_status(status: VoucherStatus) -> Option<&'static str> {
    matches!(
        status,
        VoucherStatus::Expired | VoucherStatus::Exhausted | VoucherStatus::Ineligible
    )
    .then(|| status.as_str())
}

/// Which slot a voucher competes for. Shipping wins over scope: a shop
/// freeship voucher consumes the shipping slot, not the shop slot.
fn stack_group(constraint: &VoucherConstraint, targets_shipping: bool) -> StackGroup {
    use shopee_hunter_domain::VoucherType;

    if targets_shipping {
        return StackGroup::Freeship;
    }
    if constraint.payment_method.is_some() || constraint.voucher_type == VoucherType::Payment {
        return StackGroup::Payment;
    }
    match &constraint.scope {
        ScopeConstraint::Shop { shop_id } => StackGroup::Shop(shop_id.clone()),
        ScopeConstraint::Platform => StackGroup::Platform,
        _ => {
            if constraint.voucher_type == VoucherType::Platform {
                StackGroup::Platform
            } else {
                StackGroup::Other
            }
        }
    }
}

/// Enumerate every allowed combination, best first.
///
/// Returns `(bitmask, total discount, capped)` triples sorted by total
/// descending, then by fewest vouchers, then by label order — a total order,
/// so the winner never depends on iteration accidents.
fn rank_combinations(
    candidates: &[Candidate],
    policy: &StackingPolicy,
    merchandise_subtotal: Decimal,
    shipping_estimate: Option<Decimal>,
) -> Vec<(usize, Decimal, bool)> {
    let count = candidates.len().min(MAX_SEARCH_CANDIDATES);
    let mut ranked: Vec<(usize, Decimal, bool, usize, Vec<String>)> = Vec::new();

    for mask in 0..(1usize << count) {
        let combo: Vec<&Candidate> = candidates
            .iter()
            .enumerate()
            .filter(|(position, _)| mask & (1usize << position) != 0)
            .map(|(_, candidate)| candidate)
            .collect();

        if !policy_allows(&combo, policy) {
            continue;
        }

        let (total, capped) = combo_total(&combo, merchandise_subtotal, shipping_estimate);
        let labels: Vec<String> = combo
            .iter()
            .map(|candidate| candidate.reference.label.clone())
            .collect();
        ranked.push((mask, total, capped, combo.len(), labels));
    }

    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.4.cmp(&right.4))
    });

    ranked
        .into_iter()
        .map(|(mask, total, capped, _, _)| (mask, total, capped))
        .collect()
}

fn policy_allows(combo: &[&Candidate], policy: &StackingPolicy) -> bool {
    if let Some(max_total) = policy.max_total {
        if combo.len() > max_total {
            return false;
        }
    }

    let mut platform = 0usize;
    let mut freeship = 0usize;
    let mut payment = 0usize;
    let mut other = 0usize;
    let mut per_shop: BTreeMap<&str, usize> = BTreeMap::new();

    for candidate in combo {
        match &candidate.group {
            StackGroup::Platform => platform += 1,
            StackGroup::Freeship => freeship += 1,
            StackGroup::Payment => payment += 1,
            StackGroup::Other => other += 1,
            StackGroup::Shop(shop_id) => {
                *per_shop.entry(shop_id.as_str()).or_default() += 1;
            }
        }
    }

    platform <= policy.max_platform
        && freeship <= policy.max_freeship
        && payment <= policy.max_payment
        && other <= policy.max_other
        && per_shop
            .values()
            .all(|count| *count <= policy.max_shop_per_shop)
}

/// Sum a combination, clamping merchandise and shipping savings separately.
///
/// Clamping per pool matters: a shipping voucher cannot eat into merchandise,
/// and no combination can save more than the basket is worth.
fn combo_total(
    combo: &[&Candidate],
    merchandise_subtotal: Decimal,
    shipping_estimate: Option<Decimal>,
) -> (Decimal, bool) {
    let mut merchandise = Decimal::ZERO;
    let mut shipping = Decimal::ZERO;
    for candidate in combo {
        if candidate.targets_shipping {
            shipping += candidate.estimate;
        } else {
            merchandise += candidate.estimate;
        }
    }

    let shipping_cap = shipping_estimate.unwrap_or(Decimal::ZERO);
    let capped = merchandise > merchandise_subtotal || shipping > shipping_cap;
    (
        merchandise.min(merchandise_subtotal) + shipping.min(shipping_cap),
        capped,
    )
}

fn build_alternatives(
    candidates: &[Candidate],
    combinations: &[(usize, Decimal, bool)],
    best_total: Decimal,
) -> Vec<Alternative> {
    combinations
        .iter()
        .skip(1)
        .filter(|(mask, _, _)| *mask != 0)
        .take(MAX_ALTERNATIVES)
        .map(|(mask, total, _)| {
            let labels: Vec<String> = candidates
                .iter()
                .enumerate()
                .filter(|(position, _)| mask & (1usize << position) != 0)
                .map(|(_, candidate)| candidate.reference.label.clone())
                .collect();
            let why_it_loses = if *total < best_total {
                format!("saves {} less", format_vnd(best_total - *total))
            } else {
                "same estimated saving, but uses more vouchers".to_string()
            };
            Alternative {
                labels,
                estimated_total_discount: *total,
                why_it_loses,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basket::BasketShop;
    use crate::test_support::{voucher, VoucherSpec};
    use shopee_hunter_domain::{VoucherScope, VoucherType};

    fn dec(value: i64) -> Decimal {
        Decimal::from(value)
    }

    /// Two shops, 500.000₫ of merchandise, 35.000₫ shipping.
    fn basket() -> Basket {
        Basket::new()
            .with_shop(BasketShop::new("shop-1", dec(300_000)).with_category("Điện Tử"))
            .with_shop(BasketShop::new("shop-2", dec(200_000)))
            .with_shipping_estimate(dec(35_000))
    }

    #[test]
    fn picks_the_highest_value_compatible_combination() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("PLAT50"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(50_000),
                min_spend: Some(300_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("SHOP30"),
                scope: Some(VoucherScope::Shop {
                    shop_id: "shop-1".into(),
                }),
                discount_amount: Some(30_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("FREESHIP"),
                voucher_type: VoucherType::Freeship,
                scope: Some(VoucherScope::Platform),
                ..VoucherSpec::default()
            }),
        ];

        let plan = optimize(&basket(), &vouchers);

        // One per slot: platform + shop + shipping all stack.
        assert_eq!(plan.selected.len(), 3);
        assert_eq!(plan.estimated_total_discount, dec(115_000));
        assert_eq!(plan.estimated_total_after_discount, dec(420_000));
        let labels = plan.selected_labels();
        assert!(labels.contains(&"PLAT50"));
        assert!(labels.contains(&"SHOP30"));
        assert!(labels.contains(&"FREESHIP"));
        assert!(!plan.capped_at_basket_total);
    }

    #[test]
    fn competing_vouchers_in_one_slot_pick_the_better_and_explain_the_loser() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("PLAT30"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(30_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("PLAT80"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(80_000),
                ..VoucherSpec::default()
            }),
        ];

        let plan = optimize(&basket(), &vouchers);

        assert_eq!(plan.selected_labels(), vec!["PLAT80"]);
        assert_eq!(plan.estimated_total_discount, dec(80_000));

        let alternative = plan
            .alternatives
            .iter()
            .find(|alt| alt.labels == vec!["PLAT30".to_string()])
            .expect("the losing platform voucher is explained");
        assert_eq!(alternative.why_it_loses, "saves 50.000₫ less");
    }

    #[test]
    fn percentage_and_flat_vouchers_are_compared_on_real_amounts() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("FLAT60"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(60_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("PCT20"),
                scope: Some(VoucherScope::Platform),
                discount_percent: Some(20),
                max_discount: Some(150_000),
                ..VoucherSpec::default()
            }),
        ];

        // 20% of 500.000₫ = 100.000₫ beats the flat 60.000₫.
        let plan = optimize(&basket(), &vouchers);
        assert_eq!(plan.selected_labels(), vec!["PCT20"]);
        assert_eq!(plan.estimated_total_discount, dec(100_000));
    }

    #[test]
    fn minimum_spend_and_scope_violations_are_excluded_with_reasons() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("BIG"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(200_000),
                min_spend: Some(1_000_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("OTHERSHOP"),
                scope: Some(VoucherScope::Shop {
                    shop_id: "shop-999".into(),
                }),
                discount_amount: Some(90_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("SHOPMIN"),
                scope: Some(VoucherScope::Shop {
                    shop_id: "shop-2".into(),
                }),
                discount_amount: Some(90_000),
                // shop-2 only holds 200.000₫, so this cannot qualify even
                // though the basket total would.
                min_spend: Some(250_000),
                ..VoucherSpec::default()
            }),
        ];

        let plan = optimize(&basket(), &vouchers);

        assert!(plan.is_empty());
        assert_eq!(plan.estimated_total_discount, Decimal::ZERO);
        assert_eq!(plan.excluded.len(), 3);

        let reasons: Vec<String> = plan
            .excluded
            .iter()
            .map(|excluded| excluded.reason.to_string())
            .collect();
        assert!(reasons.iter().any(|r| r.contains("1.000.000")));
        assert!(reasons.iter().any(|r| r.contains("shop-999")));
        assert!(reasons.iter().any(|r| r.contains("250.000")));
    }

    #[test]
    fn shop_vouchers_are_valued_against_their_own_shop_only() {
        let vouchers = vec![voucher(VoucherSpec {
            code: Some("SHOP20PCT"),
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-1".into(),
            }),
            discount_percent: Some(20),
            max_discount: Some(500_000),
            ..VoucherSpec::default()
        })];

        // 20% of shop-1's 300.000₫, not of the 500.000₫ basket.
        let plan = optimize(&basket(), &vouchers);
        assert_eq!(plan.estimated_total_discount, dec(60_000));
        assert!(plan.selected[0].applies_to.contains("shop shop-1"));
    }

    #[test]
    fn stacking_policy_limits_are_respected() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("PLAT50"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(50_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("PLAT40"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(40_000),
                ..VoucherSpec::default()
            }),
        ];

        // Default policy: one platform voucher only.
        let plan = optimize(&basket(), &vouchers);
        assert_eq!(plan.selected.len(), 1);

        // If the owner knows two may stack, the optimizer uses both.
        let permissive = StackingPolicy {
            max_platform: 2,
            ..StackingPolicy::default()
        };
        let plan = optimize_with(&basket(), &vouchers, &permissive);
        assert_eq!(plan.selected.len(), 2);
        assert_eq!(plan.estimated_total_discount, dec(90_000));
        assert!(plan
            .uncertainties
            .contains(&Uncertainty::DiscountsAssumedIndependent));
    }

    #[test]
    fn payment_mismatch_excludes_and_match_resolves_the_uncertainty() {
        let payment_voucher = voucher(VoucherSpec {
            code: Some("PAY20"),
            scope: Some(VoucherScope::Platform),
            payment_method: Some("ShopeePay"),
            discount_amount: Some(20_000),
            ..VoucherSpec::default()
        });

        let mismatched = basket().with_payment_method("MoMo");
        let plan = optimize(&mismatched, std::slice::from_ref(&payment_voucher));
        assert!(plan.is_empty());
        assert!(matches!(
            plan.excluded[0].reason,
            NotApplicable::PaymentMethodMismatch { .. }
        ));

        let matched = basket().with_payment_method("shopeepay");
        let plan = optimize(&matched, std::slice::from_ref(&payment_voucher));
        assert_eq!(plan.selected.len(), 1);
        assert!(!plan
            .uncertainties
            .contains(&Uncertainty::PaymentMethodUnverified));

        let undecided = basket();
        let plan = optimize(&undecided, std::slice::from_ref(&payment_voucher));
        assert_eq!(plan.selected.len(), 1);
        assert!(plan
            .uncertainties
            .contains(&Uncertainty::PaymentMethodUnverified));
    }

    #[test]
    fn shipping_voucher_without_an_estimate_is_surfaced_not_guessed() {
        let no_shipping = Basket::new().with_shop(BasketShop::new("shop-1", dec(300_000)));
        let vouchers = vec![voucher(VoucherSpec {
            code: Some("FREESHIP"),
            voucher_type: VoucherType::Freeship,
            scope: Some(VoucherScope::Platform),
            ..VoucherSpec::default()
        })];

        let plan = optimize(&no_shipping, &vouchers);
        assert_eq!(plan.estimated_total_discount, Decimal::ZERO);
        assert!(plan
            .uncertainties
            .contains(&Uncertainty::ShippingEstimateMissing));
        // Nothing is invented: the voucher simply cannot be valued.
        assert!(plan.is_empty());
    }

    #[test]
    fn terminal_vouchers_never_enter_a_plan() {
        let vouchers = vec![voucher(VoucherSpec {
            code: Some("GONE"),
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(100_000),
            status: VoucherStatus::Exhausted,
            ..VoucherSpec::default()
        })];

        let plan = optimize(&basket(), &vouchers);
        assert!(plan.is_empty());
        assert!(matches!(
            plan.excluded[0].reason,
            NotApplicable::TerminalStatus {
                status: "EXHAUSTED"
            }
        ));
    }

    #[test]
    fn discount_cannot_exceed_the_basket_value() {
        let small = Basket::new().with_shop(BasketShop::new("shop-1", dec(50_000)));
        let vouchers = vec![voucher(VoucherSpec {
            code: Some("HUGE"),
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(500_000),
            ..VoucherSpec::default()
        })];

        let plan = optimize(&small, &vouchers);
        assert_eq!(plan.estimated_total_discount, dec(50_000));
        assert_eq!(plan.estimated_total_after_discount, Decimal::ZERO);
    }

    #[test]
    fn candidate_set_is_capped_and_the_cap_is_disclosed() {
        let vouchers: Vec<Voucher> = (0..6)
            .map(|i| {
                voucher(VoucherSpec {
                    title: "platform voucher",
                    scope: Some(VoucherScope::Platform),
                    discount_amount: Some(10_000 * (i + 1)),
                    ..VoucherSpec::default()
                })
            })
            .collect();

        let policy = StackingPolicy {
            max_candidates: 2,
            ..StackingPolicy::default()
        };
        let plan = optimize_with(&basket(), &vouchers, &policy);

        assert!(plan
            .uncertainties
            .contains(&Uncertainty::CandidateSetTruncated));
        assert!(plan
            .excluded
            .iter()
            .any(|e| matches!(e.reason, NotApplicable::NotEvaluated { limit: 2 })));
        // The most valuable voucher still wins.
        assert_eq!(plan.estimated_total_discount, dec(60_000));
    }

    #[test]
    fn empty_inputs_produce_an_empty_but_honest_plan() {
        let plan = optimize(&basket(), &[]);
        assert!(plan.is_empty());
        assert_eq!(plan.estimated_total_discount, Decimal::ZERO);
        assert_eq!(plan.estimated_total_after_discount, dec(535_000));
        assert!(plan.alternatives.is_empty());
        assert!(plan
            .uncertainties
            .contains(&Uncertainty::CheckoutApplicabilityUnverified));
        assert!(plan.explain().contains("no voucher applies"));
    }

    #[test]
    fn results_are_deterministic_across_input_order() {
        let a = voucher(VoucherSpec {
            code: Some("AAA"),
            scope: Some(VoucherScope::Platform),
            discount_amount: Some(50_000),
            ..VoucherSpec::default()
        });
        let b = voucher(VoucherSpec {
            code: Some("BBB"),
            scope: Some(VoucherScope::Shop {
                shop_id: "shop-1".into(),
            }),
            discount_amount: Some(50_000),
            ..VoucherSpec::default()
        });

        let forward = optimize(&basket(), &[a.clone(), b.clone()]);
        let reversed = optimize(&basket(), &[b, a]);

        assert_eq!(
            forward.estimated_total_discount,
            reversed.estimated_total_discount
        );
        let mut forward_labels = forward.selected_labels();
        let mut reversed_labels = reversed.selected_labels();
        forward_labels.sort_unstable();
        reversed_labels.sort_unstable();
        assert_eq!(forward_labels, reversed_labels);

        // Repeated calls are byte-identical.
        assert_eq!(
            optimize(&basket(), &[]).explain(),
            optimize(&basket(), &[]).explain()
        );
    }

    #[test]
    fn explanation_covers_choice_alternatives_and_assumptions() {
        let vouchers = vec![
            voucher(VoucherSpec {
                code: Some("PLAT80"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(80_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("PLAT30"),
                scope: Some(VoucherScope::Platform),
                discount_amount: Some(30_000),
                ..VoucherSpec::default()
            }),
            voucher(VoucherSpec {
                code: Some("NOPE"),
                scope: Some(VoucherScope::Shop {
                    shop_id: "shop-42".into(),
                }),
                discount_amount: Some(90_000),
                ..VoucherSpec::default()
            }),
        ];

        let text = optimize(&basket(), &vouchers).explain();
        assert!(text.starts_with("plan: estimated saving 80.000₫"));
        assert!(text.contains("+ PLAT80"));
        assert!(text.contains("- PLAT30"));
        assert!(text.contains("x NOPE"));
        assert!(text.contains("? which vouchers may be combined"));
    }
}
