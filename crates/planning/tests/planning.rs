//! End-to-end planning tests through the public API.
//!
//! The unit tests cover each rule in isolation; these prove the crate is
//! usable from outside as one workflow — watchlist, relevance filtering,
//! basket, combination analysis — and that no result ever promises a voucher
//! will work at checkout.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::{SourceId, Voucher, VoucherCandidate, VoucherScope, VoucherType};
use shopee_hunter_planning::{
    best_relevance, optimize, optimize_with, Basket, BasketItem, BasketShop, NotApplicable,
    RelevanceLevel, StackingPolicy, Uncertainty, WatchItem, Watchlist,
};

fn dec(value: i64) -> Decimal {
    Decimal::from(value)
}

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

/// Minimal voucher builder: the crate consumes domain vouchers, which have far
/// more fields than any single test needs.
fn make_voucher(
    code: &str,
    voucher_type: VoucherType,
    scope: Option<VoucherScope>,
    discount_amount: Option<i64>,
    min_spend: Option<i64>,
) -> Voucher {
    let candidate = VoucherCandidate {
        source: SourceId::new("feed"),
        source_key: code.to_string(),
        external_id: Some(code.to_string()),
        code: Some(code.to_string()),
        promotion_id: None,
        signature: None,
        title: format!("voucher {code}"),
        description: None,
        voucher_type,
        discount_type: None,
        discount_amount: discount_amount.map(Decimal::from),
        discount_percent: None,
        max_discount: None,
        min_spend: min_spend.map(Decimal::from),
        start_at: Some(at(1)),
        end_at: Some(at(20)),
        scope,
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::Value::Null,
        observed_at: at(2),
        parser_version: "test".into(),
    };
    Voucher::from_candidate(&candidate, at(2))
}

#[test]
fn watchlist_filters_vouchers_down_to_the_planned_purchase() {
    let mut watchlist = Watchlist::new();
    watchlist
        .add(
            WatchItem::new("w1", "Mechanical keyboard")
                .with_shop("shop-1")
                .with_product_url("https://shopee.vn/product/1")
                .with_target_price(dec(1_200_000))
                .with_planned_spend(dec(1_500_000))
                .with_planned_purchase_at(at(5)),
        )
        .expect("added");
    watchlist.validate().expect("valid watchlist");

    // Relevant: the shop matches and the minimum is met.
    let relevant = make_voucher(
        "SHOP50",
        VoucherType::Shop,
        Some(VoucherScope::Shop {
            shop_id: "shop-1".into(),
        }),
        Some(50_000),
        Some(1_000_000),
    );
    let (item, verdict) = best_relevance(&relevant, &watchlist).expect("actionable");
    assert_eq!(item.id, "w1");
    assert_eq!(verdict.level, RelevanceLevel::Likely);
    assert_eq!(verdict.estimated_saving, Some(dec(50_000)));

    // Irrelevant: another shop entirely.
    let other_shop = make_voucher(
        "SHOP99",
        VoucherType::Shop,
        Some(VoucherScope::Shop {
            shop_id: "shop-99".into(),
        }),
        Some(90_000),
        None,
    );
    assert!(best_relevance(&other_shop, &watchlist).is_none());

    // Uncertain: applies platform-wide but demands more than the plan.
    let too_expensive = make_voucher(
        "BIG",
        VoucherType::Platform,
        Some(VoucherScope::Platform),
        Some(300_000),
        Some(5_000_000),
    );
    assert!(best_relevance(&too_expensive, &watchlist).is_none());
}

#[test]
fn optimizer_recommends_a_combination_and_explains_the_losers() {
    let basket = Basket::new()
        .with_shop(
            BasketShop::from_items(
                "shop-1",
                vec![
                    BasketItem::new("keyboard", dec(1_200_000), 1).with_product_id("p-1"),
                    BasketItem::new("keycaps", dec(150_000), 2),
                ],
            )
            .with_name("Keyboard Store")
            .with_category("Điện Tử"),
        )
        .with_shop(BasketShop::new("shop-2", dec(200_000)))
        .with_shipping_estimate(dec(35_000));
    basket.validate().expect("valid basket");
    assert_eq!(basket.merchandise_subtotal(), dec(1_700_000));

    let vouchers = vec![
        make_voucher(
            "PLAT100",
            VoucherType::Platform,
            Some(VoucherScope::Platform),
            Some(100_000),
            Some(1_000_000),
        ),
        make_voucher(
            "PLAT40",
            VoucherType::Platform,
            Some(VoucherScope::Platform),
            Some(40_000),
            None,
        ),
        make_voucher(
            "SHOP1_60",
            VoucherType::Shop,
            Some(VoucherScope::Shop {
                shop_id: "shop-1".into(),
            }),
            Some(60_000),
            Some(500_000),
        ),
        make_voucher(
            "FREESHIP",
            VoucherType::Freeship,
            Some(VoucherScope::Platform),
            None,
            None,
        ),
        make_voucher(
            "UNREACHABLE",
            VoucherType::Platform,
            Some(VoucherScope::Platform),
            Some(900_000),
            Some(9_000_000),
        ),
    ];

    let plan = optimize(&basket, &vouchers);

    // Best platform + best shop + shipping, one per slot.
    let labels = plan.selected_labels();
    assert!(labels.contains(&"PLAT100"), "got {labels:?}");
    assert!(labels.contains(&"SHOP1_60"), "got {labels:?}");
    assert!(labels.contains(&"FREESHIP"), "got {labels:?}");
    assert!(!labels.contains(&"PLAT40"), "only one platform slot");
    assert_eq!(plan.estimated_total_discount, dec(195_000));
    assert_eq!(plan.estimated_total_after_discount, dec(1_540_000));

    // The weaker platform voucher must be explained, not silently dropped.
    let alternative = plan
        .alternatives
        .iter()
        .find(|alt| alt.labels.contains(&"PLAT40".to_string()))
        .expect("losing alternative is reported");
    assert!(alternative.why_it_loses.contains("less"));

    // The unaffordable voucher is excluded with both numbers.
    let excluded = plan
        .excluded
        .iter()
        .find(|e| e.voucher.label == "UNREACHABLE")
        .expect("excluded voucher is reported");
    assert!(matches!(
        excluded.reason,
        NotApplicable::MinSpendNotMet { .. }
    ));
    assert!(excluded.reason.to_string().contains("9.000.000₫"));

    let text = plan.explain();
    assert!(text.contains("+ PLAT100"));
    assert!(text.contains("x UNREACHABLE"));
}

#[test]
fn scope_and_minimum_spend_are_enforced_per_shop() {
    let basket = Basket::new()
        .with_shop(BasketShop::new("shop-1", dec(300_000)))
        .with_shop(BasketShop::new("shop-2", dec(100_000)));

    // Qualifies against the basket total but not against shop-2 alone.
    let shop_voucher = make_voucher(
        "SHOP2",
        VoucherType::Shop,
        Some(VoucherScope::Shop {
            shop_id: "shop-2".into(),
        }),
        Some(50_000),
        Some(200_000),
    );

    let plan = optimize(&basket, std::slice::from_ref(&shop_voucher));
    assert!(plan.is_empty());
    assert!(matches!(
        plan.excluded[0].reason,
        NotApplicable::MinSpendNotMet { .. }
    ));

    // The same voucher qualifies once that shop's subtotal is large enough.
    let bigger = Basket::new().with_shop(BasketShop::new("shop-2", dec(250_000)));
    let plan = optimize(&bigger, &[shop_voucher]);
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.estimated_total_discount, dec(50_000));
}

#[test]
fn every_result_surfaces_uncertainty_and_never_promises_checkout() {
    let basket = Basket::new().with_shop(BasketShop::new("shop-1", dec(500_000)));
    let vouchers = vec![make_voucher(
        "PLAT50",
        VoucherType::Platform,
        Some(VoucherScope::Platform),
        Some(50_000),
        None,
    )];

    let plan = optimize(&basket, &vouchers);
    assert!(plan
        .uncertainties
        .contains(&Uncertainty::CheckoutApplicabilityUnverified));
    assert!(plan
        .uncertainties
        .contains(&Uncertainty::StackingRulesAssumed));
    assert!(plan
        .uncertainties
        .contains(&Uncertainty::ProductLevelRestrictionsUnknown));
    assert!(plan.uncertainties.contains(&Uncertainty::MinSpendUnknown));

    // Wording stays advisory: it is an estimate, not a guarantee.
    let text = plan.explain();
    assert!(text.contains("estimated saving"));
    assert!(text.contains("estimate only"));
}

#[test]
fn analysis_is_deterministic_and_policy_driven() {
    let basket = Basket::new()
        .with_shop(BasketShop::new("shop-1", dec(600_000)))
        .with_shipping_estimate(dec(30_000));
    let vouchers = vec![
        make_voucher(
            "A",
            VoucherType::Platform,
            Some(VoucherScope::Platform),
            Some(60_000),
            None,
        ),
        make_voucher(
            "B",
            VoucherType::Platform,
            Some(VoucherScope::Platform),
            Some(45_000),
            None,
        ),
    ];

    // Repeated runs are byte-identical.
    let first = optimize(&basket, &vouchers);
    let second = optimize(&basket, &vouchers);
    assert_eq!(first.explain(), second.explain());
    assert_eq!(first, second);

    // Stacking limits are configuration, not a hard-coded platform fact.
    let permissive = StackingPolicy {
        max_platform: 2,
        ..StackingPolicy::default()
    };
    let stacked = optimize_with(&basket, &vouchers, &permissive);
    assert_eq!(stacked.selected.len(), 2);
    assert_eq!(stacked.estimated_total_discount, dec(105_000));
    assert!(stacked
        .uncertainties
        .contains(&Uncertainty::DiscountsAssumedIndependent));
}
