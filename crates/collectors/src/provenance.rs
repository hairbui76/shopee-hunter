//! Source confidence and deterministic conflict resolution (ROADMAP Phase 7).
//!
//! When the same logical voucher is observed from more than one source, field
//! values may disagree. These rules decide which value wins, deterministically
//! and auditably. Raw observations are always retained separately (storage
//! keeps every observation), so conflicts remain inspectable.

use std::collections::HashMap;

use shopee_hunter_domain::voucher::VoucherCandidate;
use shopee_hunter_domain::SourceId;

/// Confidence tier of a source, used only for merge decisions (not ranking).
/// Shopee-derived data is trusted over community feeds for timing/identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceConfidence {
    /// Manually curated / owner-entered.
    Manual = 3,
    /// Directly from a Shopee resource.
    Shopee = 2,
    /// External community feed.
    External = 1,
    /// Unknown / untrusted.
    Unknown = 0,
}

/// Registry mapping source ids to confidence tiers.
#[derive(Debug, Clone, Default)]
pub struct SourceRegistry {
    tiers: HashMap<String, SourceConfidence>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, source: &str, tier: SourceConfidence) -> Self {
        self.tiers.insert(source.to_string(), tier);
        self
    }

    pub fn confidence(&self, source: &SourceId) -> SourceConfidence {
        self.tiers
            .get(source.as_str())
            .copied()
            .unwrap_or(SourceConfidence::Unknown)
    }
}

/// Which candidate's value to keep for a field when merging two observations
/// of the same logical voucher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldChoice {
    KeepExisting,
    TakeIncoming,
}

/// Resolve one optional field. Rules:
/// 1. never overwrite a present value with an absent one (present wins);
/// 2. otherwise prefer the higher-confidence source;
/// 3. on equal confidence, keep the existing value (stable, no churn).
pub fn resolve_optional<T>(
    existing: &Option<T>,
    incoming: &Option<T>,
    existing_conf: SourceConfidence,
    incoming_conf: SourceConfidence,
) -> FieldChoice {
    match (existing.is_some(), incoming.is_some()) {
        (true, false) => FieldChoice::KeepExisting,
        (false, true) => FieldChoice::TakeIncoming,
        (false, false) => FieldChoice::KeepExisting,
        (true, true) => {
            if incoming_conf > existing_conf {
                FieldChoice::TakeIncoming
            } else {
                FieldChoice::KeepExisting
            }
        }
    }
}

/// Merge `incoming` onto `existing` (same logical voucher) producing a new
/// canonical candidate. Timing and identifiers follow the confidence rules;
/// the merged candidate keeps the higher-confidence source as its origin.
pub fn merge(
    existing: &VoucherCandidate,
    incoming: &VoucherCandidate,
    registry: &SourceRegistry,
) -> VoucherCandidate {
    let ec = registry.confidence(&existing.source);
    let ic = registry.confidence(&incoming.source);

    macro_rules! pick {
        ($field:ident) => {
            match resolve_optional(&existing.$field, &incoming.$field, ec, ic) {
                FieldChoice::KeepExisting => existing.$field.clone(),
                FieldChoice::TakeIncoming => incoming.$field.clone(),
            }
        };
    }

    // Origin source/key follow the winning (higher-or-equal) confidence.
    let (origin_source, origin_key) = if ic > ec {
        (incoming.source.clone(), incoming.source_key.clone())
    } else {
        (existing.source.clone(), existing.source_key.clone())
    };

    VoucherCandidate {
        source: origin_source,
        source_key: origin_key,
        external_id: pick!(external_id),
        code: pick!(code),
        promotion_id: pick!(promotion_id),
        signature: pick!(signature),
        title: if incoming.title.trim().is_empty() {
            existing.title.clone()
        } else if ic > ec {
            incoming.title.clone()
        } else {
            existing.title.clone()
        },
        description: pick!(description),
        voucher_type: if ic > ec {
            incoming.voucher_type
        } else {
            existing.voucher_type
        },
        discount_type: pick!(discount_type),
        discount_amount: pick!(discount_amount),
        discount_percent: pick!(discount_percent),
        max_discount: pick!(max_discount),
        min_spend: pick!(min_spend),
        start_at: pick!(start_at),
        end_at: pick!(end_at),
        scope: pick!(scope),
        payment_method: pick!(payment_method),
        landing_url: pick!(landing_url),
        // Keep the freshest raw payload / observation metadata.
        raw_payload: incoming.raw_payload.clone(),
        observed_at: incoming.observed_at.max(existing.observed_at),
        parser_version: incoming.parser_version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use shopee_hunter_domain::voucher::VoucherType;

    fn candidate(source: &str, title: &str) -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new(source),
            source_key: format!("{source}-k"),
            external_id: None,
            code: Some("SALE".into()),
            promotion_id: Some("promo-1".into()),
            signature: None,
            title: title.into(),
            description: None,
            voucher_type: VoucherType::Platform,
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
            observed_at: Utc::now(),
            parser_version: "t".into(),
        }
    }

    fn registry() -> SourceRegistry {
        SourceRegistry::new()
            .with("shopee", SourceConfidence::Shopee)
            .with("external", SourceConfidence::External)
    }

    #[test]
    fn present_value_beats_absent_regardless_of_confidence() {
        // Low-confidence external has a start time; high-confidence shopee lacks it.
        let mut ext = candidate("external", "E");
        ext.start_at = Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap());
        let shopee = candidate("shopee", "S"); // no start_at

        let merged = merge(&shopee, &ext, &registry());
        assert_eq!(merged.start_at, ext.start_at); // present wins over absent
    }

    #[test]
    fn higher_confidence_wins_on_conflict() {
        let mut ext = candidate("external", "E");
        ext.start_at = Some(Utc.with_ymd_and_hms(2026, 8, 10, 6, 0, 0).unwrap());
        let mut shopee = candidate("shopee", "S");
        shopee.start_at = Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap());

        // Existing = external (low), incoming = shopee (high): shopee's time wins.
        let merged = merge(&ext, &shopee, &registry());
        assert_eq!(merged.start_at, shopee.start_at);
        assert_eq!(merged.source, SourceId::new("shopee"));
    }

    #[test]
    fn equal_confidence_keeps_existing_no_churn() {
        let mut a = candidate("shopee", "A");
        a.min_spend = Some(rust_decimal::Decimal::new(100_000, 0));
        let mut b = candidate("shopee", "B");
        b.min_spend = Some(rust_decimal::Decimal::new(200_000, 0));
        let merged = merge(&a, &b, &registry());
        assert_eq!(merged.min_spend, a.min_spend); // existing kept
    }

    #[test]
    fn resolve_optional_matrix() {
        use SourceConfidence::*;
        assert_eq!(
            resolve_optional(&Some(1), &None::<i32>, External, Shopee),
            FieldChoice::KeepExisting
        );
        assert_eq!(
            resolve_optional(&None::<i32>, &Some(1), Shopee, External),
            FieldChoice::TakeIncoming
        );
        assert_eq!(
            resolve_optional(&Some(1), &Some(2), External, Shopee),
            FieldChoice::TakeIncoming
        );
        assert_eq!(
            resolve_optional(&Some(1), &Some(2), Shopee, External),
            FieldChoice::KeepExisting
        );
    }
}
