//! Voucher constraint model (ROADMAP Phase 29, "Constraint model").
//!
//! [`shopee_hunter_domain::Voucher`] is deliberately permissive: almost every
//! field is optional because sources are heterogeneous. Planning needs the
//! opposite — an explicit statement of *what is known*, *what is capped*, and
//! *what is missing*. [`VoucherConstraint::from_voucher`] performs that
//! translation once, recording every gap as an [`Uncertainty`] instead of
//! defaulting it into a fact.
//!
//! All money is [`Decimal`]; no float ever touches a monetary value.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use shopee_hunter_domain::{DiscountType, Voucher, VoucherScope, VoucherType};
use shopee_hunter_ranking::{effective_discount, format_percent, format_vnd};

use crate::uncertainty::{push_unique, Uncertainty};

/// What a voucher applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeConstraint {
    /// Any shop on the platform.
    Platform,
    Shop {
        shop_id: String,
    },
    Category {
        name: String,
    },
    Payment {
        method: String,
    },
    /// A restriction the parser recognised but cannot model.
    Other {
        detail: String,
    },
    /// The voucher states no scope at all.
    Unknown,
}

impl ScopeConstraint {
    fn from_scope(scope: Option<&VoucherScope>) -> Self {
        match scope {
            None => Self::Unknown,
            Some(VoucherScope::Platform) => Self::Platform,
            Some(VoucherScope::Shop { shop_id }) => Self::Shop {
                shop_id: shop_id.clone(),
            },
            Some(VoucherScope::Category { name }) => Self::Category { name: name.clone() },
            Some(VoucherScope::Payment { method }) => Self::Payment {
                method: method.clone(),
            },
            Some(VoucherScope::Other { detail }) => Self::Other {
                detail: detail.clone(),
            },
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Platform => "platform-wide".to_string(),
            Self::Shop { shop_id } => format!("shop {shop_id}"),
            Self::Category { name } => format!("category {name}"),
            Self::Payment { method } => format!("payment method {method}"),
            Self::Other { detail } => format!("restriction: {detail}"),
            Self::Unknown => "unstated scope".to_string(),
        }
    }
}

/// How a voucher computes its discount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiscountRule {
    /// A flat amount off, already capped by any published maximum.
    Fixed { amount: Decimal },
    /// A percentage, optionally capped.
    Percent {
        percent: Decimal,
        cap: Option<Decimal>,
    },
    /// Shipping cost removed, up to `amount` when published.
    FreeShipping { amount: Option<Decimal> },
    /// Nothing numeric is known.
    Unknown,
}

/// An estimated discount plus the caveats attached to the estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscountEstimate {
    /// Estimated saving in VND, never negative and never above `base`.
    pub amount: Decimal,
    /// How the number was derived, for the explanation.
    pub basis: String,
    pub uncertainties: Vec<Uncertainty>,
}

/// A voucher restated as explicit constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherConstraint {
    /// Short human label: the code when present, otherwise the title.
    pub label: String,
    pub voucher_type: VoucherType,
    pub scope: ScopeConstraint,
    pub min_spend: Option<Decimal>,
    pub discount: DiscountRule,
    /// A payment method the voucher requires, from the field or the scope.
    pub payment_method: Option<String>,
    /// Everything this voucher does not tell us.
    pub unknowns: Vec<Uncertainty>,
}

impl VoucherConstraint {
    /// Translate a domain voucher, recording each missing fact.
    pub fn from_voucher(voucher: &Voucher) -> Self {
        let mut unknowns = Vec::new();

        let scope = ScopeConstraint::from_scope(voucher.scope.as_ref());
        match &scope {
            ScopeConstraint::Unknown => push_unique(&mut unknowns, Uncertainty::ScopeUnknown),
            ScopeConstraint::Other { .. } => {
                push_unique(&mut unknowns, Uncertainty::ProductLevelRestrictionsUnknown)
            }
            ScopeConstraint::Platform => {
                push_unique(&mut unknowns, Uncertainty::ProductLevelRestrictionsUnknown)
            }
            _ => {}
        }

        let discount = discount_rule(voucher, &mut unknowns);

        let min_spend = voucher.min_spend.filter(|v| v.is_sign_positive());
        if voucher.min_spend.is_none() {
            push_unique(&mut unknowns, Uncertainty::MinSpendUnknown);
        }

        let payment_method = voucher.payment_method.clone().or(match &scope {
            ScopeConstraint::Payment { method } => Some(method.clone()),
            _ => None,
        });
        if payment_method.is_some() {
            push_unique(&mut unknowns, Uncertainty::PaymentMethodUnverified);
        }

        if voucher.start_at.is_none() && voucher.end_at.is_none() {
            push_unique(&mut unknowns, Uncertainty::VoucherWindowUnknown);
        }

        Self {
            label: label_for(voucher),
            voucher_type: voucher.voucher_type,
            scope,
            min_spend,
            discount,
            payment_method,
            unknowns,
        }
    }

    /// Whether this voucher discounts shipping rather than merchandise.
    ///
    /// It changes which amount the discount applies to, so it is decided once
    /// here rather than at each call site.
    pub fn targets_shipping(&self) -> bool {
        matches!(self.discount, DiscountRule::FreeShipping { .. })
            || self.voucher_type == VoucherType::Freeship
    }

    /// Estimate the saving against `base` (the merchandise or shipping amount
    /// this voucher applies to).
    ///
    /// The result is clamped to `base`: a voucher cannot save more than the
    /// amount it is applied to.
    pub fn estimate_discount(&self, base: Decimal) -> DiscountEstimate {
        let base = base.max(Decimal::ZERO);
        let mut uncertainties = Vec::new();

        let (amount, basis) = match &self.discount {
            DiscountRule::Fixed { amount } => (
                (*amount).min(base),
                format!("flat {} off", format_vnd(*amount)),
            ),
            DiscountRule::Percent { percent, cap } => {
                let raw = (base * *percent / Decimal::from(100)).round_dp(0);
                match cap {
                    Some(cap) => (
                        raw.min(*cap).min(base),
                        format!(
                            "{} of {} capped at {}",
                            format_percent(*percent),
                            format_vnd(base),
                            format_vnd(*cap)
                        ),
                    ),
                    None => {
                        push_unique(&mut uncertainties, Uncertainty::DiscountCapUnknown);
                        (
                            raw.min(base),
                            format!(
                                "{} of {} with no published cap",
                                format_percent(*percent),
                                format_vnd(base)
                            ),
                        )
                    }
                }
            }
            DiscountRule::FreeShipping { amount } => match amount {
                Some(amount) => (
                    (*amount).min(base),
                    format!("shipping up to {}", format_vnd(*amount)),
                ),
                None => {
                    if base.is_zero() {
                        push_unique(&mut uncertainties, Uncertainty::ShippingEstimateMissing);
                    }
                    (base, "shipping cost removed".to_string())
                }
            },
            DiscountRule::Unknown => {
                push_unique(&mut uncertainties, Uncertainty::DiscountValueUnknown);
                (Decimal::ZERO, "no usable discount value".to_string())
            }
        };

        DiscountEstimate {
            amount: amount.max(Decimal::ZERO),
            basis,
            uncertainties,
        }
    }
}

fn discount_rule(voucher: &Voucher, unknowns: &mut Vec<Uncertainty>) -> DiscountRule {
    let free_shipping = voucher.voucher_type == VoucherType::Freeship
        || voucher.discount_type == Some(DiscountType::FreeShipping);

    if let Some(percent) = voucher.discount_percent.filter(|p| p.is_sign_positive()) {
        let cap = voucher.max_discount.filter(|c| c.is_sign_positive());
        if cap.is_none() {
            push_unique(unknowns, Uncertainty::DiscountCapUnknown);
        }
        return DiscountRule::Percent { percent, cap };
    }

    if free_shipping {
        // `effective_discount` already reconciles amount against any cap.
        return DiscountRule::FreeShipping {
            amount: effective_discount(voucher),
        };
    }

    match effective_discount(voucher) {
        Some(amount) => DiscountRule::Fixed { amount },
        None => {
            push_unique(unknowns, Uncertainty::DiscountValueUnknown);
            DiscountRule::Unknown
        }
    }
}

/// Prefer the code (what the owner types at checkout), fall back to the title.
fn label_for(voucher: &Voucher) -> String {
    let code = voucher.code.as_deref().map(str::trim).unwrap_or_default();
    if !code.is_empty() {
        return code.to_string();
    }
    let title = voucher.title.trim();
    if title.is_empty() {
        return "(untitled voucher)".to_string();
    }
    title.chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{voucher, VoucherSpec};

    #[test]
    fn fixed_amount_is_capped_by_the_base() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            code: Some("SALE50"),
            discount_amount: Some(50_000),
            ..VoucherSpec::default()
        }));
        assert_eq!(constraint.label, "SALE50");
        assert_eq!(
            constraint.discount,
            DiscountRule::Fixed {
                amount: Decimal::from(50_000)
            }
        );
        assert_eq!(
            constraint.estimate_discount(Decimal::from(200_000)).amount,
            Decimal::from(50_000)
        );
        // Cannot save more than the amount it applies to.
        assert_eq!(
            constraint.estimate_discount(Decimal::from(30_000)).amount,
            Decimal::from(30_000)
        );
    }

    #[test]
    fn percentage_respects_a_published_cap() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            discount_percent: Some(20),
            max_discount: Some(50_000),
            ..VoucherSpec::default()
        }));
        let estimate = constraint.estimate_discount(Decimal::from(1_000_000));
        assert_eq!(estimate.amount, Decimal::from(50_000));
        assert!(estimate.basis.contains("capped at"));
        assert!(estimate.uncertainties.is_empty());

        let under_cap = constraint.estimate_discount(Decimal::from(100_000));
        assert_eq!(under_cap.amount, Decimal::from(20_000));
    }

    #[test]
    fn uncapped_percentage_surfaces_the_unknown_cap() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            discount_percent: Some(15),
            ..VoucherSpec::default()
        }));
        assert!(constraint
            .unknowns
            .contains(&Uncertainty::DiscountCapUnknown));
        let estimate = constraint.estimate_discount(Decimal::from(200_000));
        assert_eq!(estimate.amount, Decimal::from(30_000));
        assert!(estimate
            .uncertainties
            .contains(&Uncertainty::DiscountCapUnknown));
    }

    #[test]
    fn missing_discount_value_is_reported_not_guessed() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec::default()));
        assert_eq!(constraint.discount, DiscountRule::Unknown);
        assert!(constraint
            .unknowns
            .contains(&Uncertainty::DiscountValueUnknown));

        let estimate = constraint.estimate_discount(Decimal::from(500_000));
        assert_eq!(estimate.amount, Decimal::ZERO);
        assert!(estimate
            .uncertainties
            .contains(&Uncertainty::DiscountValueUnknown));
    }

    #[test]
    fn free_shipping_without_a_stated_amount_needs_a_shipping_estimate() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            voucher_type: VoucherType::Freeship,
            ..VoucherSpec::default()
        }));
        assert!(constraint.targets_shipping());
        assert_eq!(
            constraint.discount,
            DiscountRule::FreeShipping { amount: None }
        );

        let valued = constraint.estimate_discount(Decimal::from(35_000));
        assert_eq!(valued.amount, Decimal::from(35_000));
        assert!(valued.uncertainties.is_empty());

        let unvalued = constraint.estimate_discount(Decimal::ZERO);
        assert_eq!(unvalued.amount, Decimal::ZERO);
        assert!(unvalued
            .uncertainties
            .contains(&Uncertainty::ShippingEstimateMissing));
    }

    #[test]
    fn missing_facts_become_named_unknowns() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec::default()));
        for expected in [
            Uncertainty::ScopeUnknown,
            Uncertainty::MinSpendUnknown,
            Uncertainty::VoucherWindowUnknown,
            Uncertainty::DiscountValueUnknown,
        ] {
            assert!(
                constraint.unknowns.contains(&expected),
                "missing {expected:?}"
            );
        }
    }

    #[test]
    fn payment_scope_becomes_a_payment_requirement() {
        let constraint = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            scope: Some(VoucherScope::Payment {
                method: "ShopeePay".into(),
            }),
            discount_amount: Some(20_000),
            ..VoucherSpec::default()
        }));
        assert_eq!(constraint.payment_method.as_deref(), Some("ShopeePay"));
        assert!(constraint
            .unknowns
            .contains(&Uncertainty::PaymentMethodUnverified));
    }

    #[test]
    fn label_falls_back_from_code_to_title() {
        let titled = VoucherConstraint::from_voucher(&voucher(VoucherSpec {
            title: "Giam 50k cho don tu 200k",
            ..VoucherSpec::default()
        }));
        assert_eq!(titled.label, "Giam 50k cho don tu 200k");
    }
}
