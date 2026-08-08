//! Accesstrade collector tests against a wiremock fake server.

use std::time::Duration;

use chrono::Utc;
use shopee_hunter_collectors::{
    AccesstradeCollector, AccesstradeConfig, CollectionContext, CollectorError, VoucherCollector,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx() -> CollectionContext {
    let now = Utc::now();
    CollectionContext {
        now,
        deadline: now + chrono::Duration::seconds(30),
    }
}

fn collector(base: String, token: &str) -> AccesstradeCollector {
    let mut cfg = AccesstradeConfig::new(token);
    cfg.base_url = base;
    cfg.timeout = Duration::from_secs(5);
    AccesstradeCollector::new("accesstrade", cfg, reqwest::Client::new())
}

const SAMPLE: &str = r#"{
  "data": [
    {
      "name": "Shopee - Giảm 12% tối đa 100k",
      "merchant": "shopee",
      "coupons": [{"coupon_code": "ATSALE12"}],
      "discount_percentage": 12,
      "discount_value": 0,
      "max_value": 100000,
      "min_spend": 250000,
      "start_time": "2026-08-10 00:00:00",
      "end_time": "2026-08-11 23:59:59",
      "aff_link": "https://shope.ee/aff123"
    },
    {
      "name": "Shopee Freeship 30k",
      "merchant": "shopee",
      "coupons": [{"coupon_code": "ATFREESHIP"}],
      "discount_percentage": 0,
      "discount_value": 30000,
      "max_value": 30000,
      "min_spend": 0,
      "start_time": "2026-08-10 00:00:00",
      "end_time": "2026-08-10 12:00:00",
      "link": "https://shope.ee/aff456"
    },
    { "name": "No code offer", "coupons": [] }
  ]
}"#;

#[tokio::test]
async fn parses_accesstrade_coupons_with_auth_and_merchant() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/offers_informations/coupon"))
        .and(header("authorization", "Token tok-123"))
        .and(query_param("merchant", "shopee"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
        .mount(&server)
        .await;

    let c = collector(server.uri(), "tok-123");
    let result = c.collect(&ctx()).await.unwrap();

    // Two valid coupons; the third (no code) is a per-item failure.
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(result.partial_failures.len(), 1);

    let pct = &result.candidates[0];
    assert_eq!(pct.code.as_deref(), Some("ATSALE12"));
    assert_eq!(pct.external_id.as_deref(), Some("ATSALE12"));
    assert_eq!(
        pct.discount_percent,
        Some(rust_decimal::Decimal::new(12, 0))
    );
    assert_eq!(
        pct.max_discount,
        Some(rust_decimal::Decimal::new(100000, 0))
    );
    assert!(pct.start_at.is_some() && pct.end_at.is_some());
    assert_eq!(pct.parser_version, "accesstrade/1");
    // signature/promotion_id are not provided by Accesstrade.
    assert!(pct.signature.is_none() && pct.promotion_id.is_none());

    let fixed = &result.candidates[1];
    assert_eq!(
        fixed.discount_amount,
        Some(rust_decimal::Decimal::new(30000, 0))
    );
    assert_eq!(fixed.min_spend, Some(rust_decimal::Decimal::ZERO));
}

#[tokio::test]
async fn missing_token_fails_fast_without_request() {
    let c = collector("http://127.0.0.1:1".into(), "");
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::Config(_)));
}

#[tokio::test]
async fn unauthorized_maps_to_auth_required() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let c = collector(server.uri(), "bad");
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::AuthRequired(_)));
}

#[tokio::test]
async fn malformed_body_degrades() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{ nope"))
        .mount(&server)
        .await;
    let c = collector(server.uri(), "tok");
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::Malformed(_)));
}
