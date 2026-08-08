//! Accesstrade coupon collector (ROADMAP Phase 6, first real source).
//!
//! Accesstrade is a Vietnamese affiliate network that publishes a coupon API
//! covering Shopee VN (and other merchants). Using it is the practical, ToS-safe
//! way to discover vouchers: the owner registers for an Accesstrade account and
//! supplies an API token — no Shopee reverse-engineering required.
//!
//! Endpoint (UNSTABLE, observed via the `trongthaohub/Bot_Voucher` reference,
//! 2026-08-08):
//!   GET https://api.accesstrade.vn/v1/offers_informations/coupon
//!   Authorization: Token <ACCESSTRADE_TOKEN>
//!   query: limit, merchant, is_next_day_coupon
//! Response: { "data": [ { name, coupons:[{coupon_code}], discount_percentage,
//!   discount_value, max_value, min_spend, start_time, end_time, aff_link,
//!   link, merchant } ] }

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use shopee_hunter_domain::voucher::{DiscountType, VoucherCandidate, VoucherType};
use shopee_hunter_domain::SourceId;

use crate::contract::{
    CollectionContext, CollectionResult, CollectorError, PartialFailure, VoucherCollector,
};

pub const PARSER_VERSION: &str = "accesstrade/1";
pub const DEFAULT_BASE_URL: &str = "https://api.accesstrade.vn";
const COUPON_PATH: &str = "/v1/offers_informations/coupon";

#[derive(Debug, Clone)]
pub struct AccesstradeConfig {
    pub token: String,
    /// Merchant filter, e.g. "shopee". Empty = all merchants.
    pub merchant: String,
    pub limit: u32,
    pub base_url: String,
    pub timeout: Duration,
}

impl AccesstradeConfig {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            merchant: "shopee".to_string(),
            limit: 50,
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CouponResponse {
    #[serde(default)]
    data: Vec<CouponItem>,
}

#[derive(Debug, Deserialize)]
struct CouponItem {
    name: Option<String>,
    #[serde(default)]
    coupons: Vec<Coupon>,
    discount_percentage: Option<f64>,
    discount_value: Option<f64>,
    max_value: Option<f64>,
    min_spend: Option<f64>,
    start_time: Option<String>,
    end_time: Option<String>,
    aff_link: Option<String>,
    link: Option<String>,
    merchant: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Coupon {
    coupon_code: Option<String>,
}

fn dec(v: Option<f64>) -> Option<Decimal> {
    v.filter(|x| *x > 0.0).and_then(Decimal::from_f64_retain)
}

/// Parse an Accesstrade time string. Format is not contractually fixed, so try
/// the common shapes; unparseable times degrade to `None` (still a valid
/// voucher). Naive timestamps are interpreted as Vietnam local time (UTC+7).
fn parse_time(s: &Option<String>) -> Option<DateTime<Utc>> {
    let raw = s.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    // Epoch seconds.
    if let Ok(epoch) = raw.parse::<i64>() {
        return Utc.timestamp_opt(epoch, 0).single();
    }
    // RFC3339 with offset.
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    // Naive "YYYY-MM-DD HH:MM:SS" (or with 'T') → treat as Asia/Ho_Chi_Minh.
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(vn_naive_to_utc(naive));
        }
        if fmt == "%Y-%m-%d" {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, fmt) {
                if let Some(naive) = date.and_hms_opt(0, 0, 0) {
                    return Some(vn_naive_to_utc(naive));
                }
            }
        }
    }
    None
}

/// Interpret a naive datetime as Vietnam local time and convert to UTC.
fn vn_naive_to_utc(naive: NaiveDateTime) -> DateTime<Utc> {
    use chrono_tz::Asia::Ho_Chi_Minh;
    match Ho_Chi_Minh.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
        // Ambiguous/none (never happens for VN, no DST) — fall back to UTC-7 shift.
        _ => Utc.from_utc_datetime(&(naive - chrono::Duration::hours(7))),
    }
}

fn infer_type(name: &str) -> VoucherType {
    let n = name.to_lowercase();
    if n.contains("freeship") || n.contains("free ship") || n.contains("vận chuyển") {
        VoucherType::Freeship
    } else {
        VoucherType::Platform
    }
}

pub struct AccesstradeCollector {
    name: String,
    config: AccesstradeConfig,
    client: Client,
}

impl AccesstradeCollector {
    pub fn new(name: impl Into<String>, config: AccesstradeConfig, client: Client) -> Self {
        Self {
            name: name.into(),
            config,
            client,
        }
    }

    fn to_candidate(
        &self,
        item: CouponItem,
        observed_at: DateTime<Utc>,
    ) -> Result<VoucherCandidate, String> {
        let code = item
            .coupons
            .into_iter()
            .find_map(|c| c.coupon_code)
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .ok_or("no coupon_code")?;
        let title = item
            .name
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(&code)
            .to_string();

        let discount_type = if item.discount_percentage.unwrap_or(0.0) > 0.0 {
            Some(DiscountType::Percentage)
        } else if item.discount_value.unwrap_or(0.0) > 0.0 {
            Some(DiscountType::FixedAmount)
        } else {
            None
        };

        let raw = serde_json::json!({
            "name": item.name,
            "coupon_code": code,
            "merchant": item.merchant,
            "discount_percentage": item.discount_percentage,
            "discount_value": item.discount_value,
        });

        Ok(VoucherCandidate {
            source: self.source_id(),
            source_key: code.clone(),
            external_id: Some(code.clone()),
            code: Some(code),
            promotion_id: None, // Accesstrade does not expose Shopee promotionid
            signature: None,
            title: title.clone(),
            description: None,
            voucher_type: infer_type(&title),
            discount_type,
            discount_amount: dec(item.discount_value),
            discount_percent: dec(item.discount_percentage),
            max_discount: dec(item.max_value),
            min_spend: item.min_spend.and_then(Decimal::from_f64_retain), // 0 is meaningful here
            start_at: parse_time(&item.start_time),
            end_at: parse_time(&item.end_time),
            scope: None,
            payment_method: None,
            landing_url: item.aff_link.or(item.link),
            raw_payload: raw,
            observed_at,
            parser_version: PARSER_VERSION.to_string(),
        })
    }
}

#[async_trait]
impl VoucherCollector for AccesstradeCollector {
    fn name(&self) -> &str {
        &self.name
    }

    fn source_id(&self) -> SourceId {
        SourceId::new(&self.name)
    }

    async fn collect(
        &self,
        context: &CollectionContext,
    ) -> Result<CollectionResult, CollectorError> {
        if self.config.token.trim().is_empty() {
            return Err(CollectorError::Config("ACCESSTRADE_TOKEN is empty".into()));
        }
        let started = std::time::Instant::now();
        let url = format!(
            "{}{}",
            self.config.base_url.trim_end_matches('/'),
            COUPON_PATH
        );

        let mut req = self
            .client
            .get(&url)
            .header("authorization", format!("Token {}", self.config.token))
            .query(&[("limit", self.config.limit.to_string())])
            .timeout(self.config.timeout);
        if !self.config.merchant.trim().is_empty() {
            req = req.query(&[("merchant", self.config.merchant.as_str())]);
        }

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                CollectorError::Timeout
            } else {
                CollectorError::Transient(e.to_string())
            }
        })?;

        let status = response.status();
        if status.as_u16() == 429 {
            return Err(CollectorError::RateLimited("HTTP 429".into()));
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(CollectorError::AuthRequired(format!("HTTP {status}")));
        }
        if status.is_server_error() {
            return Err(CollectorError::Transient(format!("HTTP {status}")));
        }
        if !status.is_success() {
            return Err(CollectorError::Malformed(format!("HTTP {status}")));
        }

        let body = response
            .text()
            .await
            .map_err(|e| CollectorError::Transient(e.to_string()))?;
        let parsed: CouponResponse = serde_json::from_str(&body)
            .map_err(|e| CollectorError::Malformed(format!("accesstrade parse error: {e}")))?;

        let latency = started.elapsed();
        let mut candidates = Vec::with_capacity(parsed.data.len());
        let mut partial_failures = Vec::new();
        for item in parsed.data {
            let key_hint = item.name.clone();
            match self.to_candidate(item, context.now) {
                Ok(c) => candidates.push(c),
                Err(reason) => partial_failures.push(PartialFailure {
                    source_key: key_hint,
                    reason,
                }),
            }
        }

        Ok(CollectionResult {
            candidates,
            fetched_at: Some(context.now),
            source_latency: Some(latency),
            partial_failures,
            rate_limit: None,
        })
    }
}
