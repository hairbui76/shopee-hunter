//! Voucher usefulness scoring.
//!
//! Scoring answers **"how useful is this voucher to the owner?"** and nothing
//! else. It never decides whether to act — that is [`crate::eligibility`] plus
//! the claim policy engine (ARCHITECTURE.md §11: keep them separate).
//!
//! Two properties are load-bearing:
//!
//! * **Explainable** — `total` is exactly the sum of the emitted components, so
//!   every point on screen traces to a named reason.
//! * **Deterministic** — the same voucher, rules, and `now` always produce the
//!   same breakdown, including component order. No clock is read here; `now` is
//!   injected.
//!
//! All money is [`rust_decimal::Decimal`]; no binary float ever touches a
//! monetary value or a ratio.

use std::cmp::Reverse;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use shopee_hunter_domain::{DiscountType, Voucher, VoucherScope, VoucherType};

use crate::rules::UserRules;

// ---------------------------------------------------------------------------
// Weights
//
// One editable table. Tuning ranking should never require touching control
// flow — change a number here and the explanations follow automatically.
// Ranges are open-ended by design: `total` is a raw sum, not a percentage, so
// adding a component cannot silently rescale existing configured thresholds.
// ---------------------------------------------------------------------------

/// Absolute-discount tiers, in VND, ordered from most to least valuable.
const ABSOLUTE_DISCOUNT_TIERS: &[(i64, i32)] = &[
    (500_000, 30),
    (200_000, 24),
    (100_000, 18),
    (50_000, 12),
    (20_000, 6),
    (1, 2),
];

/// Percentage-discount tiers, in whole percent.
const PERCENT_DISCOUNT_TIERS: &[(i64, i32)] = &[(50, 20), (30, 15), (20, 10), (10, 6), (1, 2)];

/// Min-spend efficiency tiers: discount as a percentage of the required spend.
const EFFICIENCY_TIERS: &[(i64, i32)] = &[(50, 25), (25, 20), (10, 14), (5, 8), (2, 3)];

/// Awarded when a voucher has real value and demands no minimum spend.
const NO_MIN_SPEND_BONUS: i32 = 25;
/// Penalty for a voucher whose discount is negligible against its min spend.
const POOR_EFFICIENCY_PENALTY: i32 = -5;

/// Discount-cap tiers, in VND.
const MAX_DISCOUNT_TIERS: &[(i64, i32)] = &[(100_000, 12), (50_000, 8), (20_000, 4), (1, 1)];
/// A percentage voucher with no published cap could be worth almost nothing.
const UNKNOWN_CAP_PENALTY: i32 = -3;

/// Baseline value of each voucher type.
const fn voucher_type_weight(voucher_type: VoucherType) -> i32 {
    match voucher_type {
        VoucherType::Platform => 12,
        VoucherType::Freeship => 10,
        VoucherType::Payment => 6,
        VoucherType::Shop | VoucherType::Category => 4,
        VoucherType::Live | VoucherType::Video => 2,
        VoucherType::Unknown => 0,
    }
}

/// Extra credit when the type is on the owner's preferred list.
const PREFERRED_TYPE_BONUS: i32 = 10;

/// Activation-proximity tiers, in minutes until start.
const ACTIVATION_TIERS: &[(i64, i32)] = &[(60, 8), (360, 5), (1_440, 3), (10_080, 1)];
const ALREADY_ACTIVE_BONUS: i32 = 10;
const DISTANT_ACTIVATION_PENALTY: i32 = -2;
const UNKNOWN_START_PENALTY: i32 = 2;

/// Remaining-validity tiers, in hours.
const DURATION_TIERS: &[(i64, i32)] = &[(168, 6), (48, 4), (12, 2), (1, 1)];
const EXPIRING_SOON_PENALTY: i32 = -4;
const UNKNOWN_END_PENALTY: i32 = -2;

/// Expiry is a **disqualifier, not a weight**.
///
/// A voucher that can no longer be used has zero usefulness however good its
/// terms were, so [`score`] short-circuits on it rather than letting positive
/// components accumulate — otherwise a large enough discount would let an
/// expired voucher outrank a usable one. The magnitude only has to dominate
/// the most negative reachable score of a still-usable voucher (about -45).
const EXPIRED_DISQUALIFIER: i32 = -1_000;

/// Restriction weights, summed into one "restrictions" component.
const SCOPE_PLATFORM: i32 = 8;
const SCOPE_SHOP: i32 = -4;
const SCOPE_CATEGORY: i32 = -2;
const SCOPE_PAYMENT: i32 = -6;
const SCOPE_OTHER: i32 = -3;
const SCOPE_UNKNOWN: i32 = -1;
const PAYMENT_METHOD_REQUIRED: i32 = -4;

const TRUSTED_SOURCE_BONUS: i32 = 12;

const CLAIM_READY_BONUS: i32 = 10;
const NOT_CLAIMABLE_PENALTY: i32 = -15;

/// Nothing numeric is known about the discount at all.
const UNKNOWN_VALUE_PENALTY: i32 = -6;
/// A free-shipping voucher carries value even without a stated amount.
const FREE_SHIPPING_VALUE: i32 = 8;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A fully explained score.
///
/// `total == components.iter().map(|(delta, _)| *delta as i64).sum()` is an
/// invariant, asserted in tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    /// Sum of every component delta.
    pub total: i64,
    /// `(delta, reason)` pairs ordered by contribution, largest first; ties
    /// keep evaluation order, so the ordering is stable.
    pub components: Vec<(i32, String)>,
}

impl ScoreBreakdown {
    /// Human-readable explanation, e.g.
    ///
    /// ```text
    /// score 82
    /// +30 high max discount
    /// +25 efficient min-spend ratio
    /// +15 platform voucher
    /// +12 trusted source
    /// ```
    pub fn explain(&self) -> String {
        let mut out = format!("score {}", self.total);
        for (delta, reason) in &self.components {
            out.push_str(&format!("\n{delta:+} {reason}"));
        }
        out
    }

    /// Whether any component's reason contains `needle` (case-sensitive).
    /// Useful for assertions and for notifier templates.
    pub fn has_reason(&self, needle: &str) -> bool {
        self.components
            .iter()
            .any(|(_, reason)| reason.contains(needle))
    }
}

/// Accumulates components in evaluation order.
struct Builder {
    components: Vec<(i32, String)>,
}

impl Builder {
    fn new() -> Self {
        Self {
            components: Vec::with_capacity(10),
        }
    }

    /// Record a contribution. Only called when there is something to say, so a
    /// breakdown never contains uninformative `+0` noise.
    fn push(&mut self, delta: i32, reason: impl Into<String>) {
        self.components.push((delta, reason.into()));
    }

    fn finish(mut self) -> ScoreBreakdown {
        let total: i64 = self
            .components
            .iter()
            .map(|(delta, _)| i64::from(*delta))
            .sum();
        // Largest contribution first; `sort_by_key` is stable, so equal deltas
        // keep evaluation order and the result stays deterministic.
        self.components.sort_by_key(|(delta, _)| Reverse(*delta));
        ScoreBreakdown {
            total,
            components: self.components,
        }
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Score a voucher against the owner's preferences at a point in time.
///
/// `now` is injected rather than read from a clock so results are reproducible
/// in tests and identical across workers processing the same batch.
pub fn score(voucher: &Voucher, rules: &UserRules, now: DateTime<Utc>) -> ScoreBreakdown {
    let mut builder = Builder::new();

    // Expiry short-circuits everything else. Were it merely a large penalty, a
    // big enough discount would still let an expired voucher outrank a usable
    // one — and it would absurdly be credited "already active" too.
    if voucher.end_at.is_some_and(|end| end <= now) {
        builder.push(EXPIRED_DISQUALIFIER, "already expired");
        return builder.finish();
    }

    score_claim_readiness(voucher, &mut builder);
    score_voucher_type(voucher, rules, &mut builder);
    score_absolute_discount(voucher, &mut builder);
    score_percent_discount(voucher, &mut builder);
    score_max_discount(voucher, &mut builder);
    score_min_spend_efficiency(voucher, &mut builder);
    score_activation_proximity(voucher, now, &mut builder);
    score_duration(voucher, now, &mut builder);
    score_restrictions(voucher, &mut builder);
    score_source_confidence(voucher, rules, &mut builder);

    builder.finish()
}

/// One voucher paired with its score.
#[derive(Debug, Clone)]
pub struct Ranked<'a> {
    /// The scored voucher.
    pub voucher: &'a Voucher,
    /// Its explained score.
    pub score: ScoreBreakdown,
}

/// Score and order a batch, best first.
///
/// Ties break on the canonical voucher id, giving a **total** order: two runs
/// over the same batch always produce the same sequence, so notification
/// digests and dashboards do not shuffle between refreshes.
pub fn rank_all<'a>(
    vouchers: &'a [Voucher],
    rules: &UserRules,
    now: DateTime<Utc>,
) -> Vec<Ranked<'a>> {
    let mut ranked: Vec<Ranked<'a>> = vouchers
        .iter()
        .map(|voucher| Ranked {
            voucher,
            score: score(voucher, rules, now),
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .total
            .cmp(&a.score.total)
            .then_with(|| a.voucher.id.cmp(&b.voucher.id))
    });
    ranked
}

fn score_claim_readiness(voucher: &Voucher, builder: &mut Builder) {
    if voucher.has_claim_identifiers() {
        builder.push(CLAIM_READY_BONUS, "claim identifiers present");
    } else {
        builder.push(
            NOT_CLAIMABLE_PENALTY,
            "no claim identifiers (cannot be auto-claimed)",
        );
    }
}

fn score_voucher_type(voucher: &Voucher, rules: &UserRules, builder: &mut Builder) {
    let weight = voucher_type_weight(voucher.voucher_type);
    if weight != 0 {
        builder.push(
            weight,
            format!("{} voucher", voucher.voucher_type.as_str().to_lowercase()),
        );
    }
    if rules.prefers_type(voucher.voucher_type) {
        builder.push(PREFERRED_TYPE_BONUS, "preferred voucher type");
    }
}

fn score_absolute_discount(voucher: &Voucher, builder: &mut Builder) {
    let Some(amount) = voucher.discount_amount.filter(|a| a.is_sign_positive()) else {
        // No stated amount: only worth flagging when nothing else quantifies
        // the voucher either.
        if voucher.discount_percent.is_none() && voucher.max_discount.is_none() {
            match voucher.discount_type {
                Some(DiscountType::FreeShipping) => {
                    builder.push(FREE_SHIPPING_VALUE, "free shipping");
                }
                _ => builder.push(UNKNOWN_VALUE_PENALTY, "discount value unknown"),
            }
        }
        return;
    };

    if let Some(delta) = tier_for(ABSOLUTE_DISCOUNT_TIERS, amount) {
        builder.push(delta, format!("discount {}", format_vnd(amount)));
    }
}

fn score_percent_discount(voucher: &Voucher, builder: &mut Builder) {
    let Some(percent) = voucher.discount_percent.filter(|p| p.is_sign_positive()) else {
        return;
    };
    if let Some(delta) = tier_for(PERCENT_DISCOUNT_TIERS, percent) {
        builder.push(delta, format!("{} off", format_percent(percent)));
    }
}

fn score_max_discount(voucher: &Voucher, builder: &mut Builder) {
    match voucher.max_discount.filter(|m| m.is_sign_positive()) {
        Some(cap) => {
            if let Some(delta) = tier_for(MAX_DISCOUNT_TIERS, cap) {
                let label = if delta >= 12 {
                    "high max discount"
                } else {
                    "max discount"
                };
                builder.push(delta, format!("{label} {}", format_vnd(cap)));
            }
        }
        None => {
            // A percentage voucher without a published cap is unquantifiable;
            // treat the uncertainty as a small cost rather than a bonus.
            if voucher.discount_percent.is_some() {
                builder.push(UNKNOWN_CAP_PENALTY, "percentage discount with unknown cap");
            }
        }
    }
}

fn score_min_spend_efficiency(voucher: &Voucher, builder: &mut Builder) {
    let Some(value) = effective_discount(voucher) else {
        return;
    };

    let Some(min_spend) = voucher.min_spend.filter(|s| s.is_sign_positive()) else {
        builder.push(NO_MIN_SPEND_BONUS, "no minimum spend");
        return;
    };

    // Ratio kept in Decimal percent; `checked_div` because a zero denominator
    // must never panic on a data-quality problem.
    let Some(ratio_percent) = (value * Decimal::from(100)).checked_div(min_spend) else {
        return;
    };

    let reason = format!(
        "min-spend ratio {} of {}",
        format_percent(ratio_percent.round_dp(0)),
        format_vnd(min_spend)
    );
    match tier_for(EFFICIENCY_TIERS, ratio_percent) {
        Some(delta) => builder.push(delta, format!("efficient {reason}")),
        None => builder.push(POOR_EFFICIENCY_PENALTY, format!("poor {reason}")),
    }
}

fn score_activation_proximity(voucher: &Voucher, now: DateTime<Utc>, builder: &mut Builder) {
    let Some(start) = voucher.start_at else {
        builder.push(UNKNOWN_START_PENALTY, "start time unknown");
        return;
    };
    if start <= now {
        builder.push(ALREADY_ACTIVE_BONUS, "already active");
        return;
    }

    let minutes = (start - now).num_minutes();
    match ACTIVATION_TIERS
        .iter()
        .find(|(threshold, _)| minutes <= *threshold)
    {
        Some((_, delta)) => builder.push(*delta, format!("activates in {}", format_delay(minutes))),
        None => builder.push(
            DISTANT_ACTIVATION_PENALTY,
            format!("activates in {}", format_delay(minutes)),
        ),
    }
}

fn score_duration(voucher: &Voucher, now: DateTime<Utc>, builder: &mut Builder) {
    // `score` has already returned for an expired voucher, so `end > now` here.
    let Some(end) = voucher.end_at else {
        builder.push(UNKNOWN_END_PENALTY, "end time unknown");
        return;
    };

    // Usable window starts when the voucher does, or now if it is already live.
    let from = voucher.start_at.map_or(now, |start| start.max(now));
    let hours = (end - from).num_hours();
    match DURATION_TIERS
        .iter()
        .find(|(threshold, _)| hours >= *threshold)
    {
        Some((_, delta)) => builder.push(*delta, format!("valid for {}", format_window(hours))),
        None => builder.push(EXPIRING_SOON_PENALTY, "usable window under an hour"),
    }
}

fn score_restrictions(voucher: &Voucher, builder: &mut Builder) {
    let mut delta = 0;
    let mut notes: Vec<String> = Vec::new();

    match &voucher.scope {
        Some(VoucherScope::Platform) => {
            delta += SCOPE_PLATFORM;
            notes.push("platform-wide".to_string());
        }
        Some(VoucherScope::Shop { shop_id }) => {
            delta += SCOPE_SHOP;
            notes.push(format!("shop {shop_id} only"));
        }
        Some(VoucherScope::Category { name }) => {
            delta += SCOPE_CATEGORY;
            notes.push(format!("category {name} only"));
        }
        Some(VoucherScope::Payment { method }) => {
            delta += SCOPE_PAYMENT;
            notes.push(format!("{method} payments only"));
        }
        Some(VoucherScope::Other { detail }) => {
            delta += SCOPE_OTHER;
            notes.push(format!("restricted: {detail}"));
        }
        None => {
            delta += SCOPE_UNKNOWN;
            notes.push("scope unknown".to_string());
        }
    }

    // Only when the scope has not already accounted for it.
    if let Some(method) = &voucher.payment_method {
        if !matches!(voucher.scope, Some(VoucherScope::Payment { .. })) {
            delta += PAYMENT_METHOD_REQUIRED;
            notes.push(format!("requires {method}"));
        }
    }

    builder.push(delta, format!("restrictions: {}", notes.join(", ")));
}

fn score_source_confidence(voucher: &Voucher, rules: &UserRules, builder: &mut Builder) {
    // Absent configuration means "no opinion", never "distrusted".
    if rules.trusts_source(&voucher.source) {
        builder.push(TRUSTED_SOURCE_BONUS, "trusted source");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The realistic ceiling on what this voucher saves.
///
/// A percentage voucher's cap is what the owner actually gets, so the smaller
/// of amount and cap is used when both are known.
pub fn effective_discount(voucher: &Voucher) -> Option<Decimal> {
    match (
        voucher.discount_amount.filter(|d| d.is_sign_positive()),
        voucher.max_discount.filter(|d| d.is_sign_positive()),
    ) {
        (Some(amount), Some(cap)) => Some(amount.min(cap)),
        (Some(amount), None) => Some(amount),
        (None, Some(cap)) => Some(cap),
        (None, None) => None,
    }
}

/// First tier whose threshold `value` reaches. Tables are ordered high to low.
fn tier_for(tiers: &[(i64, i32)], value: Decimal) -> Option<i32> {
    tiers
        .iter()
        .find(|(threshold, _)| value >= Decimal::from(*threshold))
        .map(|(_, delta)| *delta)
}

/// Render VND with Vietnamese thousand separators, e.g. `200.000₫`.
pub fn format_vnd(amount: Decimal) -> String {
    let rounded = amount.round_dp(0);
    let negative = rounded.is_sign_negative();
    let digits = rounded.abs().to_string();

    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(ch);
    }
    let mut out: String = grouped.chars().rev().collect();
    if negative {
        out.insert(0, '-');
    }
    out.push('₫');
    out
}

/// Render a percentage without trailing zeros, e.g. `30%`, `12.5%`.
pub fn format_percent(percent: Decimal) -> String {
    format!("{}%", percent.normalize())
}

fn format_delay(minutes: i64) -> String {
    match minutes {
        m if m < 60 => format!("{m}m"),
        m if m < 1_440 => format!("{}h", m / 60),
        m => format!("{}d", m / 1_440),
    }
}

fn format_window(hours: i64) -> String {
    match hours {
        h if h < 24 => format!("{h}h"),
        h => format!("{}d", h / 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_renders_with_vietnamese_grouping_and_no_floats() {
        assert_eq!(format_vnd(Decimal::from(0)), "0₫");
        assert_eq!(format_vnd(Decimal::from(999)), "999₫");
        assert_eq!(format_vnd(Decimal::from(1_000)), "1.000₫");
        assert_eq!(format_vnd(Decimal::from(200_000)), "200.000₫");
        assert_eq!(format_vnd(Decimal::from(1_234_567)), "1.234.567₫");
        assert_eq!(format_vnd(Decimal::from(-5_000)), "-5.000₫");
        // Fractional dong is rounded away rather than printed as a float.
        assert_eq!(format_vnd(Decimal::new(15_004, 1)), "1.500₫");
    }

    #[test]
    fn percentages_drop_trailing_zeros() {
        assert_eq!(format_percent(Decimal::from(30)), "30%");
        assert_eq!(format_percent(Decimal::new(300, 1)), "30%");
        assert_eq!(format_percent(Decimal::new(125, 1)), "12.5%");
    }

    #[test]
    fn tier_lookup_picks_the_highest_reached_band() {
        let tiers: &[(i64, i32)] = &[(100, 10), (50, 5), (1, 1)];
        assert_eq!(tier_for(tiers, Decimal::from(1_000)), Some(10));
        assert_eq!(tier_for(tiers, Decimal::from(100)), Some(10));
        assert_eq!(tier_for(tiers, Decimal::from(99)), Some(5));
        assert_eq!(tier_for(tiers, Decimal::from(1)), Some(1));
        assert_eq!(tier_for(tiers, Decimal::from(0)), None);
        assert_eq!(tier_for(tiers, Decimal::from(-10)), None);
    }

    #[test]
    fn durations_and_delays_render_in_the_largest_useful_unit() {
        assert_eq!(format_delay(45), "45m");
        assert_eq!(format_delay(90), "1h");
        assert_eq!(format_delay(4_320), "3d");
        assert_eq!(format_window(6), "6h");
        assert_eq!(format_window(168), "7d");
    }

    #[test]
    fn explain_renders_the_roadmap_shape() {
        let breakdown = ScoreBreakdown {
            total: 82,
            components: vec![
                (30, "high max discount".to_string()),
                (25, "efficient min-spend ratio".to_string()),
                (15, "platform voucher".to_string()),
                (12, "trusted source".to_string()),
            ],
        };
        assert_eq!(
            breakdown.explain(),
            "score 82\n+30 high max discount\n+25 efficient min-spend ratio\n\
             +15 platform voucher\n+12 trusted source"
        );
        assert!(breakdown.has_reason("trusted source"));
        assert!(!breakdown.has_reason("freeship"));
    }

    #[test]
    fn negative_components_render_with_their_sign() {
        let breakdown = ScoreBreakdown {
            total: -5,
            components: vec![(-5, "poor min-spend ratio".to_string())],
        };
        assert_eq!(breakdown.explain(), "score -5\n-5 poor min-spend ratio");
    }
}
