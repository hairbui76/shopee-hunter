//! Watchlist model (ROADMAP Phase 28).
//!
//! A [`WatchItem`] is what the owner *plans to buy*: products, shops,
//! categories, a target price, and a planned spend. It is pure configuration —
//! nothing here fetches a page, reads a clock, or scrapes a cart.
//!
//! The watchlist exists so voucher alerts can be filtered down to what is
//! actually useful; it is never an instruction to buy or claim anything.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::PlanningError;

/// One planned purchase.
///
/// `Default` is derived and `#[serde(default)]` applies to every field, so a
/// partial config file stays valid and adding a field later cannot break an
/// existing watchlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchItem {
    /// Stable owner-assigned identifier.
    pub id: String,
    /// Human label used in explanations.
    pub label: String,
    /// Shopee product identifiers, when the owner knows them.
    pub product_ids: BTreeSet<String>,
    /// Product URLs, kept verbatim for the owner's reference.
    pub product_urls: BTreeSet<String>,
    /// Shops the purchase would come from.
    pub shop_ids: BTreeSet<String>,
    /// Owner-assigned category tags (matched case-insensitively).
    pub category_tags: BTreeSet<String>,
    /// Price the owner is waiting for, per item.
    pub target_price: Option<Decimal>,
    /// Total the owner expects to spend on this purchase.
    ///
    /// This is what minimum-spend rules are checked against.
    pub planned_spend: Option<Decimal>,
    /// When the owner intends to buy, used to check voucher windows.
    ///
    /// Beyond the roadmap's field list, but it turns "this voucher expires
    /// Tuesday" into a real relevance signal instead of a guess.
    pub planned_purchase_at: Option<DateTime<Utc>>,
    /// Payment method the owner intends to use, if decided.
    ///
    /// Lets payment-restricted vouchers be judged instead of being reported as
    /// permanently uncertain.
    pub preferred_payment_method: Option<String>,
}

impl WatchItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn with_product_id(mut self, product_id: impl Into<String>) -> Self {
        self.product_ids.insert(product_id.into());
        self
    }

    pub fn with_product_url(mut self, url: impl Into<String>) -> Self {
        self.product_urls.insert(url.into());
        self
    }

    pub fn with_shop(mut self, shop_id: impl Into<String>) -> Self {
        self.shop_ids.insert(shop_id.into());
        self
    }

    pub fn with_category(mut self, tag: impl Into<String>) -> Self {
        self.category_tags.insert(tag.into());
        self
    }

    pub fn with_target_price(mut self, price: Decimal) -> Self {
        self.target_price = Some(price);
        self
    }

    pub fn with_planned_spend(mut self, spend: Decimal) -> Self {
        self.planned_spend = Some(spend);
        self
    }

    pub fn with_planned_purchase_at(mut self, at: DateTime<Utc>) -> Self {
        self.planned_purchase_at = Some(at);
        self
    }

    pub fn with_preferred_payment_method(mut self, method: impl Into<String>) -> Self {
        self.preferred_payment_method = Some(method.into());
        self
    }

    /// Spend to check minimum-spend rules against.
    ///
    /// `planned_spend` is authoritative. `target_price` is only a per-item
    /// price, so it is used as a lower-bound fallback — never added to it.
    pub fn effective_spend(&self) -> Option<Decimal> {
        self.planned_spend.or(self.target_price)
    }

    pub fn matches_shop(&self, shop_id: &str) -> bool {
        contains_ignore_case(&self.shop_ids, shop_id)
    }

    pub fn matches_category(&self, category: &str) -> bool {
        contains_ignore_case(&self.category_tags, category)
    }

    pub fn matches_payment_method(&self, method: &str) -> Option<bool> {
        self.preferred_payment_method
            .as_deref()
            .map(|preferred| preferred.trim().to_lowercase() == method.trim().to_lowercase())
    }

    /// Report every configuration problem at once.
    pub fn validate(&self) -> Result<(), Vec<PlanningError>> {
        let mut issues = Vec::new();
        if self.id.trim().is_empty() {
            issues.push(PlanningError::BlankIdentifier { field: "id" });
        }
        for (field, value) in [
            ("target_price", self.target_price),
            ("planned_spend", self.planned_spend),
        ] {
            if let Some(value) = value.filter(|v| v.is_sign_negative()) {
                issues.push(PlanningError::NegativeAmount { field, value });
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

/// The owner's planned purchases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watchlist {
    items: Vec<WatchItem>,
}

impl Watchlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an item, rejecting duplicate ids.
    pub fn add(&mut self, item: WatchItem) -> Result<(), PlanningError> {
        if self.items.iter().any(|existing| existing.id == item.id) {
            return Err(PlanningError::DuplicateWatchItem { id: item.id });
        }
        self.items.push(item);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&WatchItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn remove(&mut self, id: &str) -> Option<WatchItem> {
        let index = self.items.iter().position(|item| item.id == id)?;
        Some(self.items.remove(index))
    }

    pub fn items(&self) -> &[WatchItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn validate(&self) -> Result<(), Vec<PlanningError>> {
        let mut issues = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for item in &self.items {
            if let Err(mut item_issues) = item.validate() {
                issues.append(&mut item_issues);
            }
            if !seen.insert(item.id.as_str()) {
                issues.push(PlanningError::DuplicateWatchItem {
                    id: item.id.clone(),
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

/// Case- and padding-insensitive membership, so `"Điện Tử"` matches
/// `"điện tử"` the way an owner would expect.
pub(crate) fn contains_ignore_case(set: &BTreeSet<String>, value: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    let needle = value.trim().to_lowercase();
    set.iter()
        .any(|entry| entry.trim().to_lowercase() == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_every_planning_field() {
        let item = WatchItem::new("w1", "Mechanical keyboard")
            .with_product_id("p-1")
            .with_product_url("https://shopee.vn/product/1")
            .with_shop("shop-1")
            .with_category("Điện Tử")
            .with_target_price(Decimal::from(1_200_000))
            .with_planned_spend(Decimal::from(1_500_000))
            .with_preferred_payment_method("ShopeePay");

        assert!(item.matches_shop("shop-1"));
        assert!(item.matches_category("điện tử"));
        assert!(!item.matches_shop("shop-2"));
        assert_eq!(item.effective_spend(), Some(Decimal::from(1_500_000)));
        assert_eq!(item.matches_payment_method("shopeepay"), Some(true));
        assert_eq!(item.matches_payment_method("momo"), Some(false));
        assert!(item.validate().is_ok());
    }

    #[test]
    fn target_price_is_only_a_fallback_for_planned_spend() {
        let item = WatchItem::new("w1", "x").with_target_price(Decimal::from(200_000));
        assert_eq!(item.effective_spend(), Some(Decimal::from(200_000)));

        let item = item.with_planned_spend(Decimal::from(500_000));
        assert_eq!(item.effective_spend(), Some(Decimal::from(500_000)));

        assert_eq!(WatchItem::new("w2", "y").effective_spend(), None);
    }

    #[test]
    fn payment_preference_is_unknown_until_set() {
        let item = WatchItem::new("w1", "x");
        assert_eq!(item.matches_payment_method("momo"), None);
    }

    #[test]
    fn validation_reports_blank_ids_and_negative_money() {
        let item = WatchItem {
            id: "  ".into(),
            planned_spend: Some(Decimal::from(-1)),
            target_price: Some(Decimal::from(-2)),
            ..WatchItem::default()
        };
        let issues = item.validate().expect_err("must be invalid");
        assert_eq!(issues.len(), 3);
        assert!(issues.contains(&PlanningError::BlankIdentifier { field: "id" }));
    }

    #[test]
    fn watchlist_rejects_duplicate_ids() {
        let mut list = Watchlist::new();
        list.add(WatchItem::new("w1", "first")).expect("added");
        let err = list
            .add(WatchItem::new("w1", "duplicate"))
            .expect_err("must reject");
        assert_eq!(err, PlanningError::DuplicateWatchItem { id: "w1".into() });
        assert_eq!(list.len(), 1);
        assert!(list.get("w1").is_some());
        assert!(list.remove("w1").is_some());
        assert!(list.is_empty());
    }

    #[test]
    fn watchlist_round_trips_through_serde() {
        let mut list = Watchlist::new();
        list.add(WatchItem::new("w1", "keyboard").with_shop("shop-1"))
            .expect("added");
        let encoded = serde_json::to_string(&list).expect("serializes");
        let decoded: Watchlist = serde_json::from_str(&encoded).expect("round trips");
        assert_eq!(decoded, list);

        // A partial config is valid: omitted fields fall back to defaults.
        let partial: WatchItem =
            serde_json::from_str(r#"{"id":"w2","label":"mouse"}"#).expect("partial config");
        assert_eq!(partial.id, "w2");
        assert!(partial.shop_ids.is_empty());
        assert_eq!(partial.planned_spend, None);
    }
}
