//! Deterministic voucher identity and version hashing.
//!
//! Identity priority (see CLAUDE.md "Voucher identity and deduplication"):
//! 1. source-provided stable external ID (scoped to the source);
//! 2. trusted `promotion_id` (global across sources);
//! 3. canonical composite fingerprint (scoped to the source).
//!
//! Separately, `version_hash` detects meaningful condition changes on the
//! same logical voucher without creating a duplicate identity.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::voucher::VoucherCandidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IdentityBasis {
    ExternalId,
    PromotionId,
    Fingerprint,
}

impl IdentityBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExternalId => "EXTERNAL_ID",
            Self::PromotionId => "PROMOTION_ID",
            Self::Fingerprint => "FINGERPRINT",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "EXTERNAL_ID" => Self::ExternalId,
            "PROMOTION_ID" => Self::PromotionId,
            "FINGERPRINT" => Self::Fingerprint,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VoucherIdentity {
    /// Globally unique logical key, e.g. `ext:feed:123`, `promo:456`,
    /// `fp:shopee-page:ab12…`.
    pub key: String,
    pub basis: IdentityBasis,
}

fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Canonical code normalization: trim + uppercase.
pub fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

fn canon_decimal(value: &Option<Decimal>) -> String {
    value.map(|d| d.normalize().to_string()).unwrap_or_default()
}

fn canon_time(value: &Option<chrono::DateTime<chrono::Utc>>) -> String {
    value.map(|t| t.timestamp().to_string()).unwrap_or_default()
}

/// Compute the logical identity of a candidate.
pub fn compute_identity(candidate: &VoucherCandidate) -> VoucherIdentity {
    if let Some(ext) = candidate
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return VoucherIdentity {
            key: format!("ext:{}:{}", candidate.source, ext),
            basis: IdentityBasis::ExternalId,
        };
    }
    if let Some(promo) = candidate
        .promotion_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return VoucherIdentity {
            key: format!("promo:{promo}"),
            basis: IdentityBasis::PromotionId,
        };
    }

    let fingerprint_input = [
        candidate.source.as_str().to_string(),
        candidate
            .code
            .as_deref()
            .map(normalize_code)
            .unwrap_or_default(),
        canon_time(&candidate.start_at),
        canon_time(&candidate.end_at),
        candidate
            .scope
            .as_ref()
            .map(|s| s.canonical_string())
            .unwrap_or_default(),
        candidate.voucher_type.as_str().to_string(),
        canon_decimal(&candidate.min_spend),
        canon_decimal(&candidate.discount_amount),
        canon_decimal(&candidate.discount_percent),
        canon_decimal(&candidate.max_discount),
    ]
    .join("|");

    VoucherIdentity {
        key: format!(
            "fp:{}:{}",
            candidate.source,
            &sha256_hex(&fingerprint_input)[..32]
        ),
        basis: IdentityBasis::Fingerprint,
    }
}

/// Hash of normalized *meaningful* fields. Changes when conditions change
/// (times, spend, discounts, scope, identifiers, title); ignores observation
/// noise (observed_at, raw payload, description, landing URL).
pub fn version_hash(candidate: &VoucherCandidate) -> String {
    let input = [
        candidate
            .code
            .as_deref()
            .map(normalize_code)
            .unwrap_or_default(),
        candidate.promotion_id.clone().unwrap_or_default(),
        candidate.signature.clone().unwrap_or_default(),
        candidate.title.trim().to_string(),
        candidate.voucher_type.as_str().to_string(),
        format!("{:?}", candidate.discount_type),
        canon_decimal(&candidate.discount_amount),
        canon_decimal(&candidate.discount_percent),
        canon_decimal(&candidate.max_discount),
        canon_decimal(&candidate.min_spend),
        canon_time(&candidate.start_at),
        canon_time(&candidate.end_at),
        candidate
            .scope
            .as_ref()
            .map(|s| s.canonical_string())
            .unwrap_or_default(),
        candidate
            .payment_method
            .as_deref()
            .map(|m| m.to_lowercase())
            .unwrap_or_default(),
    ]
    .join("|");
    sha256_hex(&input)
}

/// Hash of the redacted raw payload (stable across key ordering).
pub fn raw_hash(payload: &serde_json::Value) -> String {
    // serde_json::Value objects use BTreeMap-like ordered maps only with the
    // "preserve_order" feature off; default serialization is deterministic
    // for a given Value tree.
    sha256_hex(&payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SourceId;
    use crate::voucher::{VoucherScope, VoucherType};
    use chrono::{TimeZone, Utc};
    use rust_decimal::Decimal;

    fn candidate() -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k1".into(),
            external_id: None,
            code: Some("freeship50".into()),
            promotion_id: None,
            signature: None,
            title: "Freeship 50k".into(),
            description: None,
            voucher_type: VoucherType::Freeship,
            discount_type: None,
            discount_amount: Some(Decimal::new(50_000, 0)),
            discount_percent: None,
            max_discount: None,
            min_spend: Some(Decimal::new(0, 0)),
            start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
            end_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 17, 0, 0).unwrap()),
            scope: Some(VoucherScope::Platform),
            payment_method: None,
            landing_url: None,
            raw_payload: serde_json::json!({"c": "freeship50"}),
            observed_at: Utc::now(),
            parser_version: "test-1".into(),
        }
    }

    #[test]
    fn identity_priority_order() {
        let mut c = candidate();
        c.external_id = Some("abc".into());
        c.promotion_id = Some("999".into());
        let id = compute_identity(&c);
        assert_eq!(id.basis, IdentityBasis::ExternalId);
        assert_eq!(id.key, "ext:feed:abc");

        c.external_id = None;
        let id = compute_identity(&c);
        assert_eq!(id.basis, IdentityBasis::PromotionId);
        assert_eq!(id.key, "promo:999");

        c.promotion_id = None;
        let id = compute_identity(&c);
        assert_eq!(id.basis, IdentityBasis::Fingerprint);
        assert!(id.key.starts_with("fp:feed:"));
    }

    #[test]
    fn blank_identifiers_are_ignored() {
        let mut c = candidate();
        c.external_id = Some("  ".into());
        c.promotion_id = Some("".into());
        assert_eq!(compute_identity(&c).basis, IdentityBasis::Fingerprint);
    }

    #[test]
    fn same_logical_voucher_deduplicates_deterministically() {
        let a = candidate();
        let mut b = candidate();
        b.code = Some("  FREESHIP50 ".into()); // code normalization
        b.observed_at = Utc::now(); // observation noise
        b.raw_payload = serde_json::json!({"different": true});
        assert_eq!(compute_identity(&a), compute_identity(&b));
        assert_eq!(version_hash(&a), version_hash(&b));
    }

    #[test]
    fn meaningful_change_produces_new_version_hash_same_identity_via_promo() {
        let mut a = candidate();
        a.promotion_id = Some("777".into());
        let mut b = a.clone();
        b.min_spend = Some(Decimal::new(100_000, 0));
        assert_eq!(compute_identity(&a), compute_identity(&b));
        assert_ne!(version_hash(&a), version_hash(&b));
    }

    #[test]
    fn decimal_scale_does_not_change_hashes() {
        let a = candidate();
        let mut b = candidate();
        b.discount_amount = Some(Decimal::new(50_000_0, 1)); // 50000.0
        assert_eq!(version_hash(&a), version_hash(&b));
        assert_eq!(compute_identity(&a), compute_identity(&b));
    }

    #[test]
    fn different_sources_fingerprint_differently() {
        let a = candidate();
        let mut b = candidate();
        b.source = SourceId::new("other");
        assert_ne!(compute_identity(&a).key, compute_identity(&b).key);
    }
}
