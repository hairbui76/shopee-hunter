//! Canonical voucher entities. Sources are heterogeneous; everything here is
//! source-independent and shaped for identity, scheduling, and claiming.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::VoucherIdentity;
use crate::ids::SourceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VoucherType {
    Platform,
    Shop,
    Freeship,
    Payment,
    Live,
    Video,
    Category,
    Unknown,
}

impl VoucherType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Platform => "PLATFORM",
            Self::Shop => "SHOP",
            Self::Freeship => "FREESHIP",
            Self::Payment => "PAYMENT",
            Self::Live => "LIVE",
            Self::Video => "VIDEO",
            Self::Category => "CATEGORY",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "PLATFORM" => Self::Platform,
            "SHOP" => Self::Shop,
            "FREESHIP" => Self::Freeship,
            "PAYMENT" => Self::Payment,
            "LIVE" => Self::Live,
            "VIDEO" => Self::Video,
            "CATEGORY" => Self::Category,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiscountType {
    FixedAmount,
    Percentage,
    Coins,
    FreeShipping,
}

/// Applicability scope. Serialized as a tagged value; `canonical_string` feeds
/// identity/version hashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VoucherScope {
    Platform,
    Shop { shop_id: String },
    Category { name: String },
    Payment { method: String },
    Other { detail: String },
}

impl VoucherScope {
    pub fn canonical_string(&self) -> String {
        match self {
            Self::Platform => "platform".to_string(),
            Self::Shop { shop_id } => format!("shop:{shop_id}"),
            Self::Category { name } => format!("category:{}", name.to_lowercase()),
            Self::Payment { method } => format!("payment:{}", method.to_lowercase()),
            Self::Other { detail } => format!("other:{}", detail.to_lowercase()),
        }
    }
}

/// Logical voucher lifecycle status (see ARCHITECTURE.md §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VoucherStatus {
    Discovered,
    Validated,
    Eligible,
    Scheduled,
    Claiming,
    Saved,
    Exhausted,
    Ineligible,
    Expired,
    ReviewRequired,
}

impl VoucherStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Discovered => "DISCOVERED",
            Self::Validated => "VALIDATED",
            Self::Eligible => "ELIGIBLE",
            Self::Scheduled => "SCHEDULED",
            Self::Claiming => "CLAIMING",
            Self::Saved => "SAVED",
            Self::Exhausted => "EXHAUSTED",
            Self::Ineligible => "INELIGIBLE",
            Self::Expired => "EXPIRED",
            Self::ReviewRequired => "REVIEW_REQUIRED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "DISCOVERED" => Self::Discovered,
            "VALIDATED" => Self::Validated,
            "ELIGIBLE" => Self::Eligible,
            "SCHEDULED" => Self::Scheduled,
            "CLAIMING" => Self::Claiming,
            "SAVED" => Self::Saved,
            "EXHAUSTED" => Self::Exhausted,
            "INELIGIBLE" => Self::Ineligible,
            "EXPIRED" => Self::Expired,
            "REVIEW_REQUIRED" => Self::ReviewRequired,
            _ => return None,
        })
    }

    /// Legal state-machine transitions. Terminal claim outcomes may only be
    /// entered from `Claiming`; every state can expire.
    pub fn can_transition(self, to: Self) -> bool {
        use VoucherStatus::*;
        if self == to {
            return false;
        }
        match (self, to) {
            (_, Expired) => !matches!(self, Saved | Exhausted),
            (Discovered, Validated) => true,
            (Validated, Eligible) | (Validated, Ineligible) | (Validated, ReviewRequired) => true,
            (Eligible, Scheduled) | (Eligible, Ineligible) => true,
            (Scheduled, Claiming) | (Scheduled, Eligible) => true,
            (Claiming, Saved)
            | (Claiming, Exhausted)
            | (Claiming, Ineligible)
            | (Claiming, Scheduled)
            | (Claiming, ReviewRequired) => true,
            (ReviewRequired, Validated) | (ReviewRequired, Ineligible) => true,
            _ => false,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Saved | Self::Exhausted | Self::Expired)
    }
}

/// What a collector emits after parsing/normalizing one source item.
/// `raw_payload` must already be redacted by the collector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoucherCandidate {
    pub source: SourceId,
    /// Stable per-source key of this item (external ID or derived).
    pub source_key: String,
    /// Source-provided stable identifier, if any (identity priority 1).
    pub external_id: Option<String>,
    pub code: Option<String>,
    pub promotion_id: Option<String>,
    pub signature: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub voucher_type: VoucherType,
    pub discount_type: Option<DiscountType>,
    pub discount_amount: Option<Decimal>,
    pub discount_percent: Option<Decimal>,
    pub max_discount: Option<Decimal>,
    pub min_spend: Option<Decimal>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub scope: Option<VoucherScope>,
    pub payment_method: Option<String>,
    pub landing_url: Option<String>,
    /// Redacted raw payload preserved for debugging.
    pub raw_payload: serde_json::Value,
    pub observed_at: DateTime<Utc>,
    pub parser_version: String,
}

/// Canonical logical voucher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voucher {
    pub id: Uuid,
    pub identity: VoucherIdentity,
    /// Source that first discovered this voucher.
    pub source: SourceId,
    pub source_key: String,
    pub code: Option<String>,
    pub promotion_id: Option<String>,
    pub signature: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub voucher_type: VoucherType,
    pub discount_type: Option<DiscountType>,
    pub discount_amount: Option<Decimal>,
    pub discount_percent: Option<Decimal>,
    pub max_discount: Option<Decimal>,
    pub min_spend: Option<Decimal>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub scope: Option<VoucherScope>,
    pub payment_method: Option<String>,
    pub landing_url: Option<String>,
    pub status: VoucherStatus,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// Hash of normalized meaningful fields; changes on condition changes.
    pub version_hash: String,
    /// Hash of the latest raw payload as received (post-redaction).
    pub raw_hash: String,
}

impl Voucher {
    /// Whether the voucher carries enough identifiers for a claim request.
    pub fn has_claim_identifiers(&self) -> bool {
        (self.promotion_id.is_some() && self.signature.is_some()) || self.code.is_some()
    }

    /// Build a canonical voucher from a candidate at first discovery.
    pub fn from_candidate(candidate: &VoucherCandidate, now: DateTime<Utc>) -> Self {
        let identity = crate::identity::compute_identity(candidate);
        Self {
            id: Uuid::new_v4(),
            identity,
            source: candidate.source.clone(),
            source_key: candidate.source_key.clone(),
            code: candidate.code.clone(),
            promotion_id: candidate.promotion_id.clone(),
            signature: candidate.signature.clone(),
            title: candidate.title.clone(),
            description: candidate.description.clone(),
            voucher_type: candidate.voucher_type,
            discount_type: candidate.discount_type,
            discount_amount: candidate.discount_amount,
            discount_percent: candidate.discount_percent,
            max_discount: candidate.max_discount,
            min_spend: candidate.min_spend,
            start_at: candidate.start_at,
            end_at: candidate.end_at,
            scope: candidate.scope.clone(),
            payment_method: candidate.payment_method.clone(),
            landing_url: candidate.landing_url.clone(),
            status: VoucherStatus::Discovered,
            first_seen_at: now,
            last_seen_at: now,
            version_hash: crate::identity::version_hash(candidate),
            raw_hash: crate::identity::raw_hash(&candidate.raw_payload),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_allows_documented_paths_only() {
        use VoucherStatus::*;
        assert!(Discovered.can_transition(Validated));
        assert!(Validated.can_transition(Eligible));
        assert!(Eligible.can_transition(Scheduled));
        assert!(Scheduled.can_transition(Claiming));
        assert!(Claiming.can_transition(Saved));
        assert!(Claiming.can_transition(Scheduled)); // retryable
        assert!(Claiming.can_transition(ReviewRequired)); // unknown response
        assert!(Eligible.can_transition(Expired));

        assert!(!Discovered.can_transition(Saved));
        assert!(!Saved.can_transition(Expired)); // saved is terminal
        assert!(!Exhausted.can_transition(Expired));
        assert!(!Saved.can_transition(Claiming));
        assert!(!Expired.can_transition(Claiming));
    }

    #[test]
    fn claim_identifier_presence() {
        use crate::ids::SourceId;
        let candidate = VoucherCandidate {
            source: SourceId::new("s"),
            source_key: "k".into(),
            external_id: None,
            code: None,
            promotion_id: Some("1".into()),
            signature: None,
            title: "t".into(),
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
            observed_at: chrono::Utc::now(),
            parser_version: "p".into(),
        };
        let mut v = Voucher::from_candidate(&candidate, chrono::Utc::now());
        assert!(!v.has_claim_identifiers()); // promotion_id without signature
        v.signature = Some("sig".into());
        assert!(v.has_claim_identifiers());
        v.promotion_id = None;
        v.signature = None;
        v.code = Some("CODE".into());
        assert!(v.has_claim_identifiers());
    }
}
