//! External feed collector tests against a wiremock fake server.

use std::time::Duration;

use chrono::Utc;
use shopee_hunter_collectors::{
    CollectionContext, CollectorError, ExternalFeedCollector, VoucherCollector,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx() -> CollectionContext {
    let now = Utc::now();
    CollectionContext {
        now,
        deadline: now + chrono::Duration::seconds(30),
    }
}

fn collector(url: String) -> ExternalFeedCollector {
    ExternalFeedCollector::new(
        "external-feed",
        url,
        reqwest::Client::new(),
        Duration::from_secs(5),
    )
}

#[tokio::test]
async fn parses_feed_and_drops_malformed_items() {
    let server = MockServer::start().await;
    let body = include_str!("../../../tests/fixtures/external_feed/sample_feed.json");
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let c = collector(format!("{}/feed", server.uri()));
    let result = c.collect(&ctx()).await.unwrap();

    // Two valid vouchers; the third (empty title) is a per-item failure.
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(result.partial_failures.len(), 1);
    assert!(result.is_partial());

    let first = &result.candidates[0];
    assert_eq!(first.external_id.as_deref(), Some("ext-1001"));
    assert_eq!(first.promotion_id.as_deref(), Some("778899"));
    assert!(first.start_at.is_some());
    assert_eq!(first.parser_version, "external-feed/1");
    // signature is not present in this feed, and never stored in raw_payload.
    assert!(first.raw_payload.get("signature").is_none());
}

#[tokio::test]
async fn schema_change_degrades_not_crashes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{ not json"))
        .mount(&server)
        .await;

    let c = collector(format!("{}/feed", server.uri()));
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::Malformed(_)));
}

#[tokio::test]
async fn rate_limit_is_classified() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;

    let c = collector(format!("{}/feed", server.uri()));
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::RateLimited(_)));
    assert!(err.is_transient());
}

#[tokio::test]
async fn server_error_is_transient() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let c = collector(format!("{}/feed", server.uri()));
    let err = c.collect(&ctx()).await.unwrap_err();
    assert!(matches!(err, CollectorError::Transient(_)));
}
