//! External voucher-feed collector (ROADMAP Phase 6). Fetches a JSON feed of
//! Shopee Vietnam voucher candidates over HTTP and normalizes it through the
//! shared pipeline. The exact upstream schema is unstable, so parsing is
//! schema-tolerant and versioned, and malformed items are dropped per-item
//! rather than failing the whole run.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherScope, VoucherType};
use shopee_hunter_domain::SourceId;

use crate::contract::{
    CollectionContext, CollectionResult, CollectorError, PartialFailure, RateLimitHint,
    VoucherCollector,
};

pub const PARSER_VERSION: &str = "external-feed/1";

/// Boundary DTO for the feed. Deliberately lenient: unknown fields ignored,
/// wrong-typed optionals degrade to `None`.
#[derive(Debug, Deserialize)]
struct FeedResponse {
    #[serde(default)]
    vouchers: Vec<FeedVoucher>,
}

#[derive(Debug, Deserialize)]
struct FeedVoucher {
    id: Option<String>,
    code: Option<String>,
    promotion_id: Option<String>,
    signature: Option<String>,
    title: Option<String>,
    description: Option<String>,
    #[serde(rename = "type")]
    voucher_type: Option<String>,
    discount_amount: Option<f64>,
    discount_percent: Option<f64>,
    max_discount: Option<f64>,
    min_spend: Option<f64>,
    /// Epoch seconds.
    start_time: Option<i64>,
    end_time: Option<i64>,
    scope: Option<String>,
    payment_method: Option<String>,
    landing_url: Option<String>,
}

fn dec(v: Option<f64>) -> Option<Decimal> {
    v.and_then(Decimal::from_f64_retain)
}

fn epoch(v: Option<i64>) -> Option<DateTime<Utc>> {
    v.and_then(|s| Utc.timestamp_opt(s, 0).single())
}

fn scope_from(s: Option<&str>) -> Option<VoucherScope> {
    match s.map(|x| x.to_ascii_lowercase()) {
        Some(ref x) if x == "platform" => Some(VoucherScope::Platform),
        Some(x) if x.starts_with("shop:") => Some(VoucherScope::Shop {
            shop_id: x.trim_start_matches("shop:").to_string(),
        }),
        Some(x) if x.starts_with("category:") => Some(VoucherScope::Category {
            name: x.trim_start_matches("category:").to_string(),
        }),
        Some(x) if x.starts_with("payment:") => Some(VoucherScope::Payment {
            method: x.trim_start_matches("payment:").to_string(),
        }),
        Some(x) if !x.is_empty() => Some(VoucherScope::Other { detail: x }),
        _ => None,
    }
}

/// Collector that reads a configured external voucher feed URL.
pub struct ExternalFeedCollector {
    name: String,
    url: String,
    client: Client,
    timeout: Duration,
}

impl ExternalFeedCollector {
    /// Build with a shared HTTP client (reuse the process-long client).
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        client: Client,
        timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            client,
            timeout,
        }
    }

    fn to_candidate(
        &self,
        item: FeedVoucher,
        observed_at: DateTime<Utc>,
    ) -> Result<VoucherCandidate, String> {
        let title = item
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("missing title")?
            .to_string();
        // Need at least one stable key to dedupe on.
        let source_key = item
            .id
            .clone()
            .or_else(|| item.promotion_id.clone())
            .or_else(|| item.code.clone())
            .ok_or("no id/promotion_id/code to key on")?;

        let raw = serde_json::json!({
            "id": item.id,
            "code": item.code,
            "promotion_id": item.promotion_id,
            // signature intentionally omitted from the stored raw payload
            "title": item.title,
            "type": item.voucher_type,
            "min_spend": item.min_spend,
        });

        Ok(VoucherCandidate {
            source: self.source_id(),
            source_key,
            external_id: item.id,
            code: item.code,
            promotion_id: item.promotion_id,
            signature: item.signature,
            title,
            description: item.description,
            voucher_type: item
                .voucher_type
                .as_deref()
                .map(VoucherType::parse)
                .unwrap_or(VoucherType::Unknown),
            discount_type: None,
            discount_amount: dec(item.discount_amount),
            discount_percent: dec(item.discount_percent),
            max_discount: dec(item.max_discount),
            min_spend: dec(item.min_spend),
            start_at: epoch(item.start_time),
            end_at: epoch(item.end_time),
            scope: scope_from(item.scope.as_deref()),
            payment_method: item.payment_method,
            landing_url: item.landing_url,
            raw_payload: raw,
            observed_at,
            parser_version: PARSER_VERSION.to_string(),
        })
    }
}

#[async_trait]
impl VoucherCollector for ExternalFeedCollector {
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
        let started = std::time::Instant::now();
        let response = self
            .client
            .get(&self.url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    CollectorError::Timeout
                } else {
                    CollectorError::Transient(e.to_string())
                }
            })?;

        let status = response.status();
        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(CollectorError::RateLimited(format!(
                "HTTP 429{}",
                retry_after
                    .map(|d| format!(" retry-after {}s", d.as_secs()))
                    .unwrap_or_default()
            )));
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
        let feed: FeedResponse = serde_json::from_str(&body)
            .map_err(|e| CollectorError::Malformed(format!("feed parse error: {e}")))?;

        let source_latency = started.elapsed();
        let mut candidates = Vec::with_capacity(feed.vouchers.len());
        let mut partial_failures = Vec::new();
        for item in feed.vouchers {
            let key_hint = item.id.clone().or_else(|| item.code.clone());
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
            source_latency: Some(source_latency),
            partial_failures,
            rate_limit: None::<RateLimitHint>,
        })
    }
}
