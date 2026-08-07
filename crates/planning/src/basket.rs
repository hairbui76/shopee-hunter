//! Basket input (ROADMAP Phase 29, "Basket input").
//!
//! Every number here is **provided by the owner**. This crate never opens a
//! cart page, never logs in, and never touches checkout — it only does
//! arithmetic on a summary the owner typed or pasted.
//!
//! Shipping and payment method are optional on purpose: absent values become
//! [`crate::Uncertainty`] in the plan rather than silent zeros.

use std::collections::BTreeSet;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::PlanningError;
use crate::watchlist::contains_ignore_case;

/// One line in a planned basket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasketItem {
    pub title: String,
    pub product_id: Option<String>,
    pub unit_price: Decimal,
    pub quantity: u32,
}

impl BasketItem {
    pub fn new(title: impl Into<String>, unit_price: Decimal, quantity: u32) -> Self {
        Self {
            title: title.into(),
            product_id: None,
            unit_price,
            quantity,
        }
    }

    pub fn with_product_id(mut self, product_id: impl Into<String>) -> Self {
        self.product_id = Some(product_id.into());
        self
    }

    /// Line total; saturates rather than overflowing on absurd quantities.
    pub fn line_total(&self) -> Decimal {
        self.unit_price
            .checked_mul(Decimal::from(self.quantity))
            .unwrap_or(Decimal::MAX)
    }
}

/// The part of a basket coming from one shop.
///
/// Shop-scoped vouchers are checked against this subtotal, which is why the
/// basket is grouped by shop rather than being one flat list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasketShop {
    pub shop_id: String,
    pub shop_name: Option<String>,
    /// Merchandise subtotal for this shop, before any voucher.
    pub subtotal: Decimal,
    pub items: Vec<BasketItem>,
    /// Owner-assigned category tags for this shop's contents.
    pub category_tags: BTreeSet<String>,
}

impl BasketShop {
    /// A shop summarised by subtotal only (the common manual case).
    pub fn new(shop_id: impl Into<String>, subtotal: Decimal) -> Self {
        Self {
            shop_id: shop_id.into(),
            shop_name: None,
            subtotal,
            items: Vec::new(),
            category_tags: BTreeSet::new(),
        }
    }

    /// A shop whose subtotal is derived from its listed items.
    pub fn from_items(shop_id: impl Into<String>, items: Vec<BasketItem>) -> Self {
        let subtotal = items
            .iter()
            .map(BasketItem::line_total)
            .fold(Decimal::ZERO, |acc, line| acc + line);
        Self {
            shop_id: shop_id.into(),
            shop_name: None,
            subtotal,
            items,
            category_tags: BTreeSet::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.shop_name = Some(name.into());
        self
    }

    pub fn with_category(mut self, tag: impl Into<String>) -> Self {
        self.category_tags.insert(tag.into());
        self
    }

    pub fn matches_category(&self, category: &str) -> bool {
        contains_ignore_case(&self.category_tags, category)
    }

    /// Subtotal implied by the listed items, when any were listed.
    pub fn computed_subtotal(&self) -> Option<Decimal> {
        if self.items.is_empty() {
            return None;
        }
        Some(
            self.items
                .iter()
                .map(BasketItem::line_total)
                .fold(Decimal::ZERO, |acc, line| acc + line),
        )
    }

    fn validate(&self, issues: &mut Vec<PlanningError>) {
        if self.shop_id.trim().is_empty() {
            issues.push(PlanningError::BlankIdentifier { field: "shop_id" });
        }
        if self.subtotal.is_sign_negative() {
            issues.push(PlanningError::NegativeAmount {
                field: "subtotal",
                value: self.subtotal,
            });
        }
        for item in &self.items {
            if item.quantity == 0 {
                issues.push(PlanningError::ZeroQuantity {
                    title: item.title.clone(),
                });
            }
            if item.unit_price.is_sign_negative() {
                issues.push(PlanningError::NegativeAmount {
                    field: "unit_price",
                    value: item.unit_price,
                });
            }
        }
        // A stated subtotal that disagrees with the listed items is reported,
        // never silently corrected: only the owner knows which is right.
        if let Some(computed) = self.computed_subtotal() {
            if computed != self.subtotal {
                issues.push(PlanningError::SubtotalMismatch {
                    shop_id: self.shop_id.clone(),
                    stated: self.subtotal,
                    computed,
                });
            }
        }
    }
}

/// A planned order, as described by the owner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Basket {
    pub shops: Vec<BasketShop>,
    /// Shipping cost estimate. `None` means "unknown", not "free".
    pub shipping_estimate: Option<Decimal>,
    /// Payment method the owner intends to use, if decided.
    pub payment_method: Option<String>,
}

impl Basket {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_shop(mut self, shop: BasketShop) -> Self {
        self.shops.push(shop);
        self
    }

    pub fn with_shipping_estimate(mut self, estimate: Decimal) -> Self {
        self.shipping_estimate = Some(estimate);
        self
    }

    pub fn with_payment_method(mut self, method: impl Into<String>) -> Self {
        self.payment_method = Some(method.into());
        self
    }

    /// Merchandise total across all shops, before vouchers and shipping.
    pub fn merchandise_subtotal(&self) -> Decimal {
        self.shops
            .iter()
            .map(|shop| shop.subtotal)
            .fold(Decimal::ZERO, |acc, subtotal| acc + subtotal)
    }

    /// Merchandise plus shipping when shipping is known.
    pub fn total_before_vouchers(&self) -> Decimal {
        self.merchandise_subtotal() + self.shipping_estimate.unwrap_or(Decimal::ZERO)
    }

    pub fn shop(&self, shop_id: &str) -> Option<&BasketShop> {
        self.shops
            .iter()
            .find(|shop| shop.shop_id.trim().eq_ignore_ascii_case(shop_id.trim()))
    }

    pub fn shop_subtotal(&self, shop_id: &str) -> Option<Decimal> {
        self.shop(shop_id).map(|shop| shop.subtotal)
    }

    /// Combined subtotal of every shop tagged with `category`.
    pub fn category_subtotal(&self, category: &str) -> Decimal {
        self.shops
            .iter()
            .filter(|shop| shop.matches_category(category))
            .map(|shop| shop.subtotal)
            .fold(Decimal::ZERO, |acc, subtotal| acc + subtotal)
    }

    /// Whether the basket's payment preference matches `method`.
    ///
    /// `None` means the owner has not decided, which is an uncertainty rather
    /// than a mismatch.
    pub fn matches_payment_method(&self, method: &str) -> Option<bool> {
        self.payment_method
            .as_deref()
            .map(|preferred| preferred.trim().to_lowercase() == method.trim().to_lowercase())
    }

    /// Report every input problem at once.
    pub fn validate(&self) -> Result<(), Vec<PlanningError>> {
        let mut issues = Vec::new();
        if self.shops.is_empty() {
            issues.push(PlanningError::EmptyBasket);
        }
        if let Some(shipping) = self.shipping_estimate.filter(|s| s.is_sign_negative()) {
            issues.push(PlanningError::NegativeAmount {
                field: "shipping_estimate",
                value: shipping,
            });
        }

        let mut seen: BTreeSet<String> = BTreeSet::new();
        for shop in &self.shops {
            shop.validate(&mut issues);
            let key = shop.shop_id.trim().to_lowercase();
            if !seen.insert(key) {
                issues.push(PlanningError::DuplicateShop {
                    shop_id: shop.shop_id.clone(),
                });
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(value: i64) -> Decimal {
        Decimal::from(value)
    }

    #[test]
    fn subtotals_aggregate_per_shop_and_overall() {
        let basket = Basket::new()
            .with_shop(BasketShop::new("shop-1", dec(300_000)))
            .with_shop(BasketShop::new("shop-2", dec(200_000)))
            .with_shipping_estimate(dec(35_000));

        assert_eq!(basket.merchandise_subtotal(), dec(500_000));
        assert_eq!(basket.total_before_vouchers(), dec(535_000));
        assert_eq!(basket.shop_subtotal("shop-1"), Some(dec(300_000)));
        assert_eq!(basket.shop_subtotal("missing"), None);
        assert!(basket.validate().is_ok());
    }

    #[test]
    fn item_lists_derive_their_own_subtotal() {
        let shop = BasketShop::from_items(
            "shop-1",
            vec![
                BasketItem::new("keyboard", dec(1_200_000), 1),
                BasketItem::new("keycaps", dec(150_000), 2),
            ],
        );
        assert_eq!(shop.subtotal, dec(1_500_000));
        assert_eq!(shop.computed_subtotal(), Some(dec(1_500_000)));
        assert!(Basket::new().with_shop(shop).validate().is_ok());
    }

    #[test]
    fn a_stated_subtotal_that_contradicts_items_is_reported() {
        let mut shop = BasketShop::from_items(
            "shop-1",
            vec![BasketItem::new("keyboard", dec(1_200_000), 1)],
        );
        shop.subtotal = dec(999_000);

        let issues = Basket::new()
            .with_shop(shop)
            .validate()
            .expect_err("must be invalid");
        assert!(issues.iter().any(|issue| matches!(
            issue,
            PlanningError::SubtotalMismatch { stated, computed, .. }
                if *stated == dec(999_000) && *computed == dec(1_200_000)
        )));
    }

    #[test]
    fn category_subtotal_sums_tagged_shops_only() {
        let basket = Basket::new()
            .with_shop(BasketShop::new("shop-1", dec(300_000)).with_category("Điện Tử"))
            .with_shop(BasketShop::new("shop-2", dec(200_000)).with_category("Thời Trang"));

        assert_eq!(basket.category_subtotal("điện tử"), dec(300_000));
        assert_eq!(basket.category_subtotal("Nhà Cửa"), Decimal::ZERO);
    }

    #[test]
    fn payment_preference_distinguishes_mismatch_from_undecided() {
        let undecided = Basket::new().with_shop(BasketShop::new("shop-1", dec(1)));
        assert_eq!(undecided.matches_payment_method("ShopeePay"), None);

        let decided = undecided.clone().with_payment_method("ShopeePay");
        assert_eq!(decided.matches_payment_method("shopeepay"), Some(true));
        assert_eq!(decided.matches_payment_method("MoMo"), Some(false));
    }

    #[test]
    fn validation_catches_empty_duplicate_and_negative_input() {
        let empty = Basket::new().validate().expect_err("must be invalid");
        assert!(empty.contains(&PlanningError::EmptyBasket));

        let issues = Basket::new()
            .with_shop(BasketShop::new("shop-1", dec(100)))
            .with_shop(BasketShop::new("SHOP-1", dec(200)))
            .with_shop(BasketShop::new("  ", dec(-5)))
            .validate()
            .expect_err("must be invalid");

        assert!(issues
            .iter()
            .any(|i| matches!(i, PlanningError::DuplicateShop { .. })));
        assert!(issues
            .iter()
            .any(|i| matches!(i, PlanningError::BlankIdentifier { .. })));
        assert!(issues.iter().any(|i| matches!(
            i,
            PlanningError::NegativeAmount {
                field: "subtotal",
                ..
            }
        )));
    }

    #[test]
    fn zero_quantity_lines_are_rejected() {
        let issues = Basket::new()
            .with_shop(BasketShop::from_items(
                "shop-1",
                vec![BasketItem::new("ghost", dec(1_000), 0)],
            ))
            .validate()
            .expect_err("must be invalid");
        assert!(issues
            .iter()
            .any(|i| matches!(i, PlanningError::ZeroQuantity { .. })));
    }

    #[test]
    fn unknown_shipping_is_not_treated_as_free() {
        let basket = Basket::new().with_shop(BasketShop::new("shop-1", dec(100_000)));
        assert_eq!(basket.shipping_estimate, None);
        // It contributes nothing to the total, and the planner reports the gap.
        assert_eq!(basket.total_before_vouchers(), dec(100_000));
    }
}
