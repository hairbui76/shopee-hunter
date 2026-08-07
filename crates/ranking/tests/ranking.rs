//! Ranking behaviour against a representative voucher set.
//!
//! Assertions target *relative ordering*, *explanation content*, and the
//! structural invariants — not exact totals, so retuning a weight does not
//! invalidate the suite while a broken model still fails it.

use chrono::{DateTime, Duration, TimeZone, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::voucher::VoucherCandidate;
use shopee_hunter_domain::{SourceId, Voucher, VoucherScope, VoucherStatus, VoucherType};
use shopee_hunter_ranking::{
    eligibility, meets_auto_claim_threshold, passes_notification_threshold, rank_all, score,
    ScoreBreakdown, UserRules,
};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
        .single()
        .expect("fixed test timestamp is unambiguous")
}

fn vnd(amount: i64) -> Decimal {
    Decimal::from(amount)
}

/// Minimal candidate; individual tests override only what they exercise.
fn candidate(source: &str, key: &str) -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new(source),
        source_key: key.to_string(),
        external_id: None,
        code: None,
        promotion_id: None,
        signature: None,
        title: key.to_string(),
        description: None,
        voucher_type: VoucherType::Unknown,
        discount_type: None,
        discount_amount: None,
        discount_percent: None,
        max_discount: None,
        min_spend: None,
        start_at: None,
        end_at: None,
        scope: None,
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::Value::Null,
        observed_at: now(),
        parser_version: "test".into(),
    }
}

fn build(candidate: VoucherCandidate) -> Voucher {
    Voucher::from_candidate(&candidate, now())
}

/// Platform voucher, big discount, efficient, live now, trusted source, ready
/// to claim. The best case the system can see.
fn platform_jackpot() -> Voucher {
    build(VoucherCandidate {
        voucher_type: VoucherType::Platform,
        promotion_id: Some("123".into()),
        signature: Some("sig".into()),
        discount_amount: Some(vnd(200_000)),
        max_discount: Some(vnd(200_000)),
        min_spend: Some(vnd(400_000)),
        start_at: Some(now() - Duration::hours(1)),
        end_at: Some(now() + Duration::days(7)),
        scope: Some(VoucherScope::Platform),
        ..candidate("shopee-page", "jackpot")
    })
}

/// Small shop voucher, poor ratio, days away, code-only, untrusted source.
fn shop_marginal() -> Voucher {
    build(VoucherCandidate {
        voucher_type: VoucherType::Shop,
        code: Some("SHOP10K".into()),
        discount_amount: Some(vnd(10_000)),
        min_spend: Some(vnd(500_000)),
        start_at: Some(now() + Duration::days(3)),
        end_at: Some(now() + Duration::days(5)),
        scope: Some(VoucherScope::Shop {
            shop_id: "12345".into(),
        }),
        ..candidate("external-feed", "marginal")
    })
}

/// The jackpot, but with no usable claim identifiers.
fn jackpot_without_identifiers() -> Voucher {
    build(VoucherCandidate {
        voucher_type: VoucherType::Platform,
        discount_amount: Some(vnd(200_000)),
        max_discount: Some(vnd(200_000)),
        min_spend: Some(vnd(400_000)),
        start_at: Some(now() - Duration::hours(1)),
        end_at: Some(now() + Duration::days(7)),
        scope: Some(VoucherScope::Platform),
        ..candidate("shopee-page", "no-identifiers")
    })
}

/// Good voucher that activates a month out.
fn distant_activation() -> Voucher {
    build(VoucherCandidate {
        voucher_type: VoucherType::Platform,
        promotion_id: Some("456".into()),
        signature: Some("sig".into()),
        discount_amount: Some(vnd(200_000)),
        max_discount: Some(vnd(200_000)),
        min_spend: Some(vnd(400_000)),
        start_at: Some(now() + Duration::days(30)),
        end_at: Some(now() + Duration::days(37)),
        scope: Some(VoucherScope::Platform),
        ..candidate("shopee-page", "distant")
    })
}

/// Already past its end time.
fn expired() -> Voucher {
    build(VoucherCandidate {
        voucher_type: VoucherType::Platform,
        promotion_id: Some("789".into()),
        signature: Some("sig".into()),
        discount_amount: Some(vnd(200_000)),
        max_discount: Some(vnd(200_000)),
        min_spend: Some(vnd(400_000)),
        start_at: Some(now() - Duration::days(10)),
        end_at: Some(now() - Duration::days(1)),
        scope: Some(VoucherScope::Platform),
        ..candidate("shopee-page", "expired")
    })
}

fn trusting_rules() -> UserRules {
    UserRules {
        trusted_sources: [SourceId::new("shopee-page")].into_iter().collect(),
        ..UserRules::default()
    }
}

fn total(voucher: &Voucher, rules: &UserRules) -> i64 {
    score(voucher, rules, now()).total
}

fn assert_sums(breakdown: &ScoreBreakdown) {
    let sum: i64 = breakdown
        .components
        .iter()
        .map(|(delta, _)| i64::from(*delta))
        .sum();
    assert_eq!(
        breakdown.total,
        sum,
        "total must equal the sum of its components: {}",
        breakdown.explain()
    );
}

#[test]
fn representative_set_orders_by_usefulness() {
    let rules = trusting_rules();

    let jackpot = total(&platform_jackpot(), &rules);
    let no_ids = total(&jackpot_without_identifiers(), &rules);
    let distant = total(&distant_activation(), &rules);
    let marginal = total(&shop_marginal(), &rules);
    let gone = total(&expired(), &rules);

    assert!(
        jackpot > no_ids,
        "missing claim identifiers must cost: {jackpot} vs {no_ids}"
    );
    assert!(
        jackpot > distant,
        "a live voucher must beat an identical one a month out: {jackpot} vs {distant}"
    );
    assert!(
        distant > marginal,
        "a big distant discount must still beat a tiny near one: {distant} vs {marginal}"
    );
    assert!(
        marginal > gone,
        "anything usable must beat an expired voucher: {marginal} vs {gone}"
    );
    assert!(gone < 0, "an expired voucher must score negative: {gone}");
}

#[test]
fn every_score_is_fully_explained() {
    let rules = trusting_rules();
    for voucher in [
        platform_jackpot(),
        shop_marginal(),
        jackpot_without_identifiers(),
        distant_activation(),
        expired(),
    ] {
        let breakdown = score(&voucher, &rules, now());
        assert_sums(&breakdown);
        assert!(
            !breakdown.components.is_empty(),
            "a score with no explanation is not explainable"
        );
        for (_, reason) in &breakdown.components {
            assert!(!reason.trim().is_empty(), "every component needs a reason");
        }
    }
}

#[test]
fn explanations_name_the_drivers() {
    let breakdown = score(&platform_jackpot(), &trusting_rules(), now());

    assert!(breakdown.has_reason("platform voucher"));
    assert!(breakdown.has_reason("trusted source"));
    assert!(breakdown.has_reason("high max discount"));
    assert!(breakdown.has_reason("efficient min-spend ratio"));
    assert!(breakdown.has_reason("claim identifiers present"));
    assert!(breakdown.has_reason("already active"));

    // Money renders in Vietnamese grouping, never as a float.
    assert!(breakdown.has_reason("200.000₫"), "{}", breakdown.explain());
    assert!(
        !breakdown.explain().contains("200000.0"),
        "money must never render as a float"
    );

    // The rendered form matches the roadmap's explanation shape.
    let rendered = breakdown.explain();
    assert!(rendered.starts_with(&format!("score {}", breakdown.total)));
    assert!(rendered.contains("\n+"));
}

#[test]
fn components_are_ordered_by_contribution() {
    let breakdown = score(&platform_jackpot(), &trusting_rules(), now());
    let deltas: Vec<i32> = breakdown.components.iter().map(|(d, _)| *d).collect();
    let mut sorted = deltas.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(deltas, sorted, "largest contribution must come first");
}

#[test]
fn missing_identifiers_are_called_out_explicitly() {
    let rules = trusting_rules();
    let ready = score(&platform_jackpot(), &rules, now());
    let unready = score(&jackpot_without_identifiers(), &rules, now());

    assert!(ready.has_reason("claim identifiers present"));
    assert!(unready.has_reason("no claim identifiers"));
    assert!(unready.total < ready.total);
}

#[test]
fn preferred_types_and_trusted_sources_add_score_without_filtering() {
    let neutral = UserRules::default();
    let preferring = UserRules {
        preferred_voucher_types: vec![VoucherType::Platform],
        trusted_sources: [SourceId::new("shopee-page")].into_iter().collect(),
        ..UserRules::default()
    };

    let voucher = platform_jackpot();
    let plain = score(&voucher, &neutral, now());
    let boosted = score(&voucher, &preferring, now());

    assert!(boosted.total > plain.total);
    assert!(boosted.has_reason("preferred voucher type"));
    assert!(boosted.has_reason("trusted source"));
    assert!(
        !plain.has_reason("trusted source"),
        "absent config is not distrust"
    );

    // A non-preferred type is never denied, only unboosted.
    let shop = shop_marginal();
    assert!(eligibility(&shop, &preferring).is_allowed());
    assert!(!score(&shop, &preferring, now()).has_reason("preferred voucher type"));
}

#[test]
fn scoring_is_deterministic() {
    let rules = trusting_rules();
    let voucher = platform_jackpot();
    let first = score(&voucher, &rules, now());
    for _ in 0..5 {
        assert_eq!(score(&voucher, &rules, now()), first);
    }
}

#[test]
fn rank_all_is_a_stable_total_order() {
    let rules = trusting_rules();
    let vouchers = vec![
        shop_marginal(),
        platform_jackpot(),
        expired(),
        distant_activation(),
        jackpot_without_identifiers(),
    ];

    let ranked = rank_all(&vouchers, &rules, now());
    assert_eq!(ranked.len(), vouchers.len());
    assert_eq!(ranked[0].voucher.source_key, "jackpot");
    assert_eq!(ranked[ranked.len() - 1].voucher.source_key, "expired");

    let totals: Vec<i64> = ranked.iter().map(|r| r.score.total).collect();
    let mut descending = totals.clone();
    descending.sort_by(|a, b| b.cmp(a));
    assert_eq!(totals, descending);

    // Repeated runs over the same batch must not shuffle.
    let again = rank_all(&vouchers, &rules, now());
    let keys: Vec<&str> = ranked
        .iter()
        .map(|r| r.voucher.source_key.as_str())
        .collect();
    let keys_again: Vec<&str> = again
        .iter()
        .map(|r| r.voucher.source_key.as_str())
        .collect();
    assert_eq!(keys, keys_again);
}

// ---------------------------------------------------------------------------
// Eligibility — independent of the numeric score
// ---------------------------------------------------------------------------

#[test]
fn excluded_shop_is_denied_however_good_the_score() {
    let rules = UserRules {
        excluded_shops: ["12345"].iter().map(|s| s.to_string()).collect(),
        ..UserRules::default()
    };
    let voucher = build(VoucherCandidate {
        voucher_type: VoucherType::Shop,
        promotion_id: Some("1".into()),
        signature: Some("s".into()),
        discount_amount: Some(vnd(500_000)),
        max_discount: Some(vnd(500_000)),
        start_at: Some(now() - Duration::hours(1)),
        end_at: Some(now() + Duration::days(7)),
        scope: Some(VoucherScope::Shop {
            shop_id: "12345".into(),
        }),
        ..candidate("shopee-page", "excluded-shop")
    });

    let decision = eligibility(&voucher, &rules);
    assert!(!decision.is_allowed());
    assert!(decision.has_reason("shop 12345 is excluded"));

    // Scoring is untouched by exclusion: the two concerns stay separate.
    assert!(score(&voucher, &rules, now()).total > 0);
}

#[test]
fn excluded_category_and_payment_method_are_denied() {
    let rules = UserRules {
        excluded_categories: ["Điện Tử"].iter().map(|s| s.to_string()).collect(),
        excluded_payment_methods: ["ShopeePay"].iter().map(|s| s.to_string()).collect(),
        ..UserRules::default()
    };

    let category = build(VoucherCandidate {
        scope: Some(VoucherScope::Category {
            name: "điện tử".into(),
        }),
        ..candidate("shopee-page", "cat")
    });
    assert!(eligibility(&category, &rules).has_reason("is excluded"));

    // Excluded via the dedicated field...
    let by_field = build(VoucherCandidate {
        payment_method: Some("shopeepay".into()),
        ..candidate("shopee-page", "pay-field")
    });
    assert!(!eligibility(&by_field, &rules).is_allowed());

    // ...and via the scope, so the rule cannot be bypassed by shape.
    let by_scope = build(VoucherCandidate {
        scope: Some(VoucherScope::Payment {
            method: "SHOPEEPAY".into(),
        }),
        ..candidate("shopee-page", "pay-scope")
    });
    assert!(!eligibility(&by_scope, &rules).is_allowed());

    let other_method = build(VoucherCandidate {
        payment_method: Some("momo".into()),
        ..candidate("shopee-page", "pay-ok")
    });
    assert!(eligibility(&other_method, &rules).is_allowed());
}

#[test]
fn threshold_rules_filter_on_value_not_score() {
    let rules = UserRules {
        min_discount: Some(vnd(50_000)),
        max_required_spend: Some(vnd(300_000)),
        ..UserRules::default()
    };

    let too_small = build(VoucherCandidate {
        discount_amount: Some(vnd(10_000)),
        ..candidate("shopee-page", "small")
    });
    assert!(eligibility(&too_small, &rules).has_reason("is below the"));

    let too_expensive = build(VoucherCandidate {
        discount_amount: Some(vnd(100_000)),
        min_spend: Some(vnd(2_000_000)),
        ..candidate("shopee-page", "expensive")
    });
    assert!(eligibility(&too_expensive, &rules).has_reason("exceeds the"));

    let good = build(VoucherCandidate {
        discount_amount: Some(vnd(100_000)),
        min_spend: Some(vnd(200_000)),
        ..candidate("shopee-page", "good")
    });
    let decision = eligibility(&good, &rules);
    assert!(decision.is_allowed());
    assert!(decision.has_reason("meets the"));
    assert!(decision.has_reason("within the"));
}

#[test]
fn an_unprovable_discount_is_denied_when_a_minimum_is_configured() {
    let rules = UserRules {
        min_discount: Some(vnd(50_000)),
        ..UserRules::default()
    };
    // Percentage-only voucher with no published cap: value cannot be bounded.
    let unquantifiable = build(VoucherCandidate {
        discount_percent: Some(vnd(20)),
        ..candidate("shopee-page", "uncapped")
    });
    let decision = eligibility(&unquantifiable, &rules);
    assert!(!decision.is_allowed());
    assert!(decision.has_reason("unknown"));

    // Without a configured minimum the same voucher passes.
    assert!(eligibility(&unquantifiable, &UserRules::default()).is_allowed());
}

#[test]
fn terminal_statuses_are_denied() {
    for status in [
        VoucherStatus::Saved,
        VoucherStatus::Exhausted,
        VoucherStatus::Expired,
        VoucherStatus::Ineligible,
    ] {
        let mut voucher = platform_jackpot();
        voucher.status = status;
        let decision = eligibility(&voucher, &UserRules::default());
        assert!(!decision.is_allowed(), "{status:?} must be denied");
        assert!(decision.has_reason("terminal"));
    }

    let mut active = platform_jackpot();
    active.status = VoucherStatus::Eligible;
    assert!(eligibility(&active, &UserRules::default()).is_allowed());
}

#[test]
fn denials_report_every_failing_rule() {
    let rules = UserRules {
        min_discount: Some(vnd(500_000)),
        max_required_spend: Some(vnd(1_000)),
        excluded_shops: ["12345"].iter().map(|s| s.to_string()).collect(),
        ..UserRules::default()
    };
    let voucher = build(VoucherCandidate {
        discount_amount: Some(vnd(10_000)),
        min_spend: Some(vnd(900_000)),
        scope: Some(VoucherScope::Shop {
            shop_id: "12345".into(),
        }),
        ..candidate("external-feed", "multi-fail")
    });

    let decision = eligibility(&voucher, &rules);
    assert!(!decision.is_allowed());
    assert_eq!(
        decision.reasons().len(),
        3,
        "all failing rules must be listed: {:?}",
        decision.reasons()
    );
}

#[test]
fn an_allowed_voucher_always_explains_itself() {
    let decision = eligibility(&platform_jackpot(), &UserRules::default());
    assert!(decision.is_allowed());
    assert!(
        !decision.reasons().is_empty(),
        "an allow decision must still be auditable"
    );
}

// ---------------------------------------------------------------------------
// Score-to-action thresholds
// ---------------------------------------------------------------------------

#[test]
fn thresholds_gate_notification_and_auto_claim_independently() {
    let rules = UserRules {
        notification_threshold: 40,
        auto_claim_threshold: 90,
        trusted_sources: [SourceId::new("shopee-page")].into_iter().collect(),
        ..UserRules::default()
    };

    let jackpot = total(&platform_jackpot(), &rules);
    assert!(passes_notification_threshold(jackpot, &rules));
    assert!(meets_auto_claim_threshold(jackpot, &rules));

    let marginal = total(&shop_marginal(), &rules);
    assert!(!passes_notification_threshold(marginal, &rules));
    assert!(!meets_auto_claim_threshold(marginal, &rules));

    // Raising the bar reduces noise without changing any score.
    let strict = UserRules {
        notification_threshold: 1_000,
        auto_claim_threshold: 1_000,
        ..rules.clone()
    };
    assert_eq!(total(&platform_jackpot(), &strict), jackpot);
    assert!(!passes_notification_threshold(jackpot, &strict));
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn sparse_and_hostile_vouchers_do_not_panic() {
    let rules = trusting_rules();

    // Nothing known at all.
    let empty = build(candidate("unknown", "empty"));
    let breakdown = score(&empty, &rules, now());
    assert_sums(&breakdown);
    assert!(breakdown.has_reason("discount value unknown"));
    assert!(breakdown.has_reason("start time unknown"));
    assert!(breakdown.has_reason("end time unknown"));
    assert!(breakdown.has_reason("scope unknown"));

    // Zero and negative money must not divide by zero or invert the model.
    for (amount, spend) in [(0, 0), (0, 100), (100, 0), (-5, -5)] {
        let odd = build(VoucherCandidate {
            discount_amount: Some(vnd(amount)),
            min_spend: Some(vnd(spend)),
            ..candidate("unknown", "odd")
        });
        assert_sums(&score(&odd, &rules, now()));
    }

    // A repeating ratio (1/3) must stay exact-decimal, never a float.
    let repeating = build(VoucherCandidate {
        discount_amount: Some(vnd(1)),
        min_spend: Some(vnd(3)),
        ..candidate("unknown", "repeating")
    });
    let breakdown = score(&repeating, &rules, now());
    assert_sums(&breakdown);
    assert!(
        breakdown.has_reason("min-spend ratio 33%"),
        "{}",
        breakdown.explain()
    );

    // End before start is bad upstream data, not a panic.
    let inverted = build(VoucherCandidate {
        start_at: Some(now() + Duration::days(2)),
        end_at: Some(now() + Duration::days(1)),
        ..candidate("unknown", "inverted")
    });
    assert_sums(&score(&inverted, &rules, now()));
}

#[test]
fn free_shipping_without_a_stated_amount_still_carries_value() {
    let freeship = build(VoucherCandidate {
        voucher_type: VoucherType::Freeship,
        discount_type: Some(shopee_hunter_domain::DiscountType::FreeShipping),
        code: Some("FREESHIP".into()),
        ..candidate("shopee-page", "freeship")
    });
    let breakdown = score(&freeship, &UserRules::default(), now());
    assert!(breakdown.has_reason("free shipping"));
    assert!(!breakdown.has_reason("discount value unknown"));
}
