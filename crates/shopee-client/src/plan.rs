//! Prebuilt, immutable claim requests.
//!
//! A [`ClaimPlan`] is constructed **before** the precision execution window
//! (ARCHITECTURE.md §13: prepare / execute split). Everything expensive —
//! identifier validation, JSON building, serialization — happens here, so the
//! `T=0` path only has to attach headers and write already-encoded bytes.
//!
//! A plan is immutable: once validated it cannot drift from what was approved.

use serde_json::{json, Value};
use shopee_hunter_domain::Voucher;

use crate::endpoints::Endpoint;

/// Which identifier pair the request will use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimIdentifier {
    /// `promotion_id` + `signature` — the pair Shopee's own web client sends.
    PromotionSignature,
    /// Voucher `code` only — accepted for user-typed codes, and the only
    /// option when a source never exposed a signature.
    Code,
}

impl ClaimIdentifier {
    /// Stable label for logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromotionSignature => "promotion_signature",
            Self::Code => "code",
        }
    }
}

/// Why a voucher cannot be turned into a claim request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// Neither `promotion_id`+`signature` nor `code` was available.
    #[error("voucher {voucher_id} has no usable claim identifiers")]
    MissingIdentifiers {
        /// Canonical voucher id, for correlation with the attempt record.
        voucher_id: String,
    },
}

/// An immutable, validated, pre-serialized claim request.
#[derive(Debug, Clone)]
pub struct ClaimPlan {
    voucher_id: String,
    promotion_id: Option<String>,
    signature: Option<String>,
    code: Option<String>,
    identifier: ClaimIdentifier,
    endpoint: Endpoint,
    body: Value,
    body_bytes: Vec<u8>,
}

impl ClaimPlan {
    /// Build a plan from a canonical voucher.
    ///
    /// Identifier priority mirrors [`Voucher::has_claim_identifiers`]:
    ///
    /// 1. `promotion_id` **and** `signature` (primary — what the web client sends);
    /// 2. `code` (fallback — request body carries `voucher_code`).
    ///
    /// # Errors
    ///
    /// [`PlanError::MissingIdentifiers`] when neither form is available. The
    /// claimer must treat that as a policy denial, not a transport failure.
    pub fn for_voucher(voucher: &Voucher) -> Result<Self, PlanError> {
        let voucher_id = voucher.id.to_string();

        let promotion_id = non_empty(voucher.promotion_id.as_deref());
        let signature = non_empty(voucher.signature.as_deref());
        let code = non_empty(voucher.code.as_deref());

        let (identifier, body) = match (&promotion_id, &signature, &code) {
            (Some(promotion_id), Some(signature), _) => (
                ClaimIdentifier::PromotionSignature,
                save_voucher_body_by_promotion(promotion_id, signature),
            ),
            (_, _, Some(code)) => (ClaimIdentifier::Code, save_voucher_body_by_code(code)),
            _ => return Err(PlanError::MissingIdentifiers { voucher_id }),
        };

        // Serialize once, off the hot path. `Value` cannot contain a map with
        // non-string keys or a non-finite float here, so this cannot fail; the
        // fallback keeps the constructor total rather than panicking.
        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();

        Ok(Self {
            voucher_id,
            promotion_id,
            signature,
            code,
            identifier,
            endpoint: Endpoint::SaveVoucher,
            body,
            body_bytes,
        })
    }

    /// Canonical voucher id (a UUID rendered once at construction) used to
    /// correlate the request with its attempt record.
    pub fn voucher_id(&self) -> &str {
        &self.voucher_id
    }

    /// Promotion id the request carries, when the primary form was used.
    pub fn promotion_id(&self) -> Option<&str> {
        self.promotion_id.as_deref()
    }

    /// Signature the request carries, when the primary form was used.
    pub fn signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    /// Voucher code, when known.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Which identifier form the request uses.
    pub fn identifier(&self) -> ClaimIdentifier {
        self.identifier
    }

    /// Endpoint key this plan targets.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Prepared request body.
    pub fn body(&self) -> &Value {
        &self.body
    }

    /// Body already serialized to JSON bytes, so `T=0` performs no encoding.
    pub fn body_bytes(&self) -> &[u8] {
        &self.body_bytes
    }

    /// Whitelisted fields safe to attach to a log line or attempt record.
    ///
    /// Deliberately excludes `signature`: it is session-adjacent material with
    /// no diagnostic value.
    pub fn log_fields(&self) -> PlanLogFields<'_> {
        PlanLogFields {
            voucher_id: &self.voucher_id,
            identifier: self.identifier.as_str(),
            endpoint: self.endpoint.as_str(),
            has_promotion_id: self.promotion_id.is_some(),
        }
    }
}

/// Safe-to-log projection of a [`ClaimPlan`].
#[derive(Debug, Clone, Copy)]
pub struct PlanLogFields<'a> {
    /// Canonical voucher id.
    pub voucher_id: &'a str,
    /// Identifier form in use.
    pub identifier: &'static str,
    /// Endpoint short name.
    pub endpoint: &'static str,
    /// Whether a promotion id was present.
    pub has_promotion_id: bool,
}

/// UNSTABLE: `save_voucher` body observed 2026-08-08 in the reference tooling.
/// `voucher_promotionid` is numeric when the id fits an `i64` and a string
/// otherwise, matching what the web client emits for very large ids.
fn save_voucher_body_by_promotion(promotion_id: &str, signature: &str) -> Value {
    let promotion_value = match promotion_id.parse::<i64>() {
        Ok(numeric) => json!(numeric),
        Err(_) => json!(promotion_id),
    };
    json!({
        "voucher_promotionid": promotion_value,
        "signature": signature,
    })
}

/// UNSTABLE: code-only fallback body observed 2026-08-08.
fn save_voucher_body_by_code(code: &str) -> Value {
    json!({ "voucher_code": code })
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
    use shopee_hunter_domain::SourceId;

    fn voucher(promotion_id: Option<&str>, signature: Option<&str>, code: Option<&str>) -> Voucher {
        let candidate = VoucherCandidate {
            source: SourceId::new("test"),
            source_key: "k".into(),
            external_id: None,
            code: code.map(str::to_string),
            promotion_id: promotion_id.map(str::to_string),
            signature: signature.map(str::to_string),
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
            parser_version: "test".into(),
        };
        Voucher::from_candidate(&candidate, chrono::Utc::now())
    }

    #[test]
    fn prefers_promotion_and_signature() {
        let v = voucher(Some("12345"), Some("sig-abc"), Some("FREESHIP"));
        let plan = ClaimPlan::for_voucher(&v).expect("plan builds");
        assert_eq!(plan.identifier(), ClaimIdentifier::PromotionSignature);
        assert_eq!(plan.endpoint(), Endpoint::SaveVoucher);
        assert_eq!(plan.body()["voucher_promotionid"], serde_json::json!(12345));
        assert_eq!(plan.body()["signature"], "sig-abc");
        assert!(plan.body().get("voucher_code").is_none());
        assert_eq!(plan.voucher_id(), v.id.to_string());
    }

    #[test]
    fn oversized_promotion_ids_stay_strings() {
        let v = voucher(Some("99999999999999999999999"), Some("sig"), None);
        let plan = ClaimPlan::for_voucher(&v).expect("plan builds");
        assert_eq!(
            plan.body()["voucher_promotionid"],
            serde_json::json!("99999999999999999999999")
        );
    }

    #[test]
    fn falls_back_to_code_when_signature_is_absent() {
        let v = voucher(Some("12345"), None, Some("FREESHIP"));
        let plan = ClaimPlan::for_voucher(&v).expect("plan builds");
        assert_eq!(plan.identifier(), ClaimIdentifier::Code);
        assert_eq!(plan.body()["voucher_code"], "FREESHIP");
        assert!(plan.body().get("signature").is_none());
    }

    #[test]
    fn rejects_vouchers_without_identifiers() {
        for (promotion_id, signature, code) in [
            (None, None, None),
            (Some("12345"), None, None),
            (None, Some("sig"), None),
            // Blank strings are not identifiers.
            (Some("  "), Some("  "), Some("   ")),
        ] {
            let v = voucher(promotion_id, signature, code);
            let err = ClaimPlan::for_voucher(&v).expect_err("must be rejected");
            assert!(matches!(err, PlanError::MissingIdentifiers { .. }));
        }
    }

    #[test]
    fn body_bytes_match_the_prepared_value() {
        let v = voucher(Some("7"), Some("sig"), None);
        let plan = ClaimPlan::for_voucher(&v).expect("plan builds");
        let reparsed: Value = serde_json::from_slice(plan.body_bytes()).expect("valid json");
        assert_eq!(&reparsed, plan.body());
        assert!(!plan.body_bytes().is_empty());
    }

    #[test]
    fn log_fields_exclude_the_signature() {
        let v = voucher(Some("7"), Some("super-secret-signature"), None);
        let plan = ClaimPlan::for_voucher(&v).expect("plan builds");
        let rendered = format!("{:?}", plan.log_fields());
        assert!(!rendered.contains("super-secret-signature"));
        assert!(rendered.contains("promotion_signature"));
    }
}
