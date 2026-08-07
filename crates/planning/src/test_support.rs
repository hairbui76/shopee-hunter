//! Voucher fixtures for this crate's unit tests.
//!
//! Compiled only under `cfg(test)`: it is test scaffolding, not public API.
//! `Voucher` has ~25 fields, so building one inline in every test would bury
//! the property under construction noise.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::{
    DiscountType, SourceId, Voucher, VoucherCandidate, VoucherScope, VoucherStatus, VoucherType,
};

/// Only the fields a test cares about; everything else takes a neutral value.
pub struct VoucherSpec {
    pub code: Option<&'static str>,
    pub title: &'static str,
    pub voucher_type: VoucherType,
    pub discount_type: Option<DiscountType>,
    pub discount_amount: Option<i64>,
    pub discount_percent: Option<i64>,
    pub max_discount: Option<i64>,
    pub min_spend: Option<i64>,
    pub scope: Option<VoucherScope>,
    pub payment_method: Option<&'static str>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub status: VoucherStatus,
}

impl Default for VoucherSpec {
    fn default() -> Self {
        Self {
            code: None,
            title: "voucher",
            voucher_type: VoucherType::Platform,
            discount_type: None,
            discount_amount: None,
            discount_percent: None,
            max_discount: None,
            min_spend: None,
            scope: None,
            payment_method: None,
            start_at: None,
            end_at: None,
            status: VoucherStatus::Discovered,
        }
    }
}

/// Build a domain [`Voucher`] from a spec.
pub fn voucher(spec: VoucherSpec) -> Voucher {
    let candidate = VoucherCandidate {
        source: SourceId::new("test-feed"),
        source_key: spec.title.to_string(),
        external_id: Some(format!("{}-{}", spec.title, spec.code.unwrap_or("none"))),
        code: spec.code.map(str::to_string),
        promotion_id: None,
        signature: None,
        title: spec.title.to_string(),
        description: None,
        voucher_type: spec.voucher_type,
        discount_type: spec.discount_type,
        discount_amount: spec.discount_amount.map(Decimal::from),
        discount_percent: spec.discount_percent.map(Decimal::from),
        max_discount: spec.max_discount.map(Decimal::from),
        min_spend: spec.min_spend.map(Decimal::from),
        start_at: spec.start_at,
        end_at: spec.end_at,
        scope: spec.scope,
        payment_method: spec.payment_method.map(str::to_string),
        landing_url: None,
        raw_payload: serde_json::Value::Null,
        observed_at: DateTime::<Utc>::from_timestamp(1_760_000_000, 0).unwrap_or_default(),
        parser_version: "test".into(),
    };
    let mut voucher = Voucher::from_candidate(
        &candidate,
        DateTime::<Utc>::from_timestamp(1_760_000_000, 0).unwrap_or_default(),
    );
    voucher.status = spec.status;
    voucher
}
