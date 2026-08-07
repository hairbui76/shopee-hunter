//! Transport-level integration tests against a local mock server.
//!
//! No live Shopee account is involved, so these run in ordinary CI. They cover
//! what the pure classifier cannot: header policy, body encoding, timeout
//! mapping, rate-limit hints, and latency measurement.

use std::time::Duration;

use shopee_hunter_client::{
    ClaimPlan, ClientError, SecretString, SessionProbe, ShopeeClient, ShopeeClientConfig,
};
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
use shopee_hunter_domain::{ClaimResultClass, SourceId, Voucher};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SAVE_PATH: &str = "/api/v2/voucher_wallet/save_voucher";
const PROBE_PATH: &str = "/api/v4/account/basic/get_account_info";

fn client_for(server: &MockServer) -> ShopeeClient {
    ShopeeClient::new(ShopeeClientConfig {
        base_url: server.uri(),
        request_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        ..ShopeeClientConfig::default()
    })
    .expect("client builds against a loopback base url")
}

fn voucher_with_signature() -> Voucher {
    let candidate = VoucherCandidate {
        source: SourceId::new("test"),
        source_key: "k".into(),
        external_id: None,
        code: None,
        promotion_id: Some("987654".into()),
        signature: Some("test-signature".into()),
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

async fn only_request(server: &MockServer) -> Request {
    let mut requests = server
        .received_requests()
        .await
        .expect("mock server records requests");
    assert_eq!(requests.len(), 1, "expected exactly one request");
    requests.remove(0)
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[tokio::test]
async fn probe_reports_healthy_and_sends_only_safe_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PROBE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"error":0,"data":{"userid":42,"username":"owner"}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let client = client_for(&server);
    client.set_cookie_header(SecretString::new("SPC_EC=abc; SPC_ST=def"));

    let outcome = client.probe_session().await.expect("probe succeeds");
    assert_eq!(outcome.probe, SessionProbe::Healthy);
    assert!(outcome.probe.to_session_state().allows_claims());
    assert!(outcome.latency > Duration::ZERO);

    let request = only_request(&server).await;
    assert_eq!(
        header(&request, "cookie").as_deref(),
        Some("SPC_EC=abc; SPC_ST=def"),
        "session cookie must reach the wire"
    );
    assert_eq!(
        header(&request, "referer").as_deref(),
        Some(server.uri().as_str())
    );
    assert!(header(&request, "user-agent").is_some_and(|ua| ua.contains("Chrome")));

    // Header policy: nothing beyond the documented minimum.
    let allowed = [
        "cookie",
        "referer",
        "user-agent",
        "accept",
        "accept-encoding",
        "content-type",
        "content-length",
        "host",
    ];
    for name in request.headers.keys() {
        assert!(
            allowed.contains(&name.as_str()),
            "unexpected header on the wire: {name}"
        );
    }
}

#[tokio::test]
async fn requests_carry_no_cookie_until_one_is_installed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PROBE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"error":0,"data":{"userid":42}}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    assert!(!client.has_cookie());
    client.probe_session().await.expect("probe succeeds");

    let request = only_request(&server).await;
    assert!(
        header(&request, "cookie").is_none(),
        "no cookie must be sent before a session is installed"
    );
}

#[tokio::test]
async fn probe_maps_a_login_page_to_login_required() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PROBE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<!doctype html><html><body><a href=\"/buyer/login\">login</a></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;

    let outcome = client_for(&server)
        .probe_session()
        .await
        .expect("probe succeeds");
    assert_eq!(outcome.probe, SessionProbe::LoginRequired);
    assert!(outcome.probe.to_session_state().blocks_claims());
}

#[tokio::test]
async fn execute_claim_posts_the_prebuilt_body_and_classifies_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"error":0,"error_msg":""}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    client.set_cookie_header(SecretString::new("SPC_EC=abc"));
    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");

    let before = chrono::Utc::now();
    let outcome = client.execute_claim(&plan).await.expect("claim completes");
    assert_eq!(outcome.classified.class, ClaimResultClass::Success);
    assert_eq!(outcome.classified.diagnostic.http_status, 200);
    assert_eq!(outcome.classified.diagnostic.upstream_code, Some(0));
    assert!(outcome.sent_at >= before);
    assert!(outcome.latency > Duration::ZERO);

    let request = only_request(&server).await;
    assert_eq!(
        header(&request, "content-type").as_deref(),
        Some("application/json")
    );
    let sent: serde_json::Value =
        serde_json::from_slice(&request.body).expect("body is the prepared json");
    assert_eq!(sent, *plan.body());
    assert_eq!(sent["voucher_promotionid"], serde_json::json!(987654));
}

#[tokio::test]
async fn execute_claim_classifies_an_already_saved_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"error":5,"error_msg":"Ban da luu voucher nay"}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");
    let outcome = client_for(&server)
        .execute_claim(&plan)
        .await
        .expect("claim completes");
    assert_eq!(outcome.classified.class, ClaimResultClass::AlreadySaved);
    assert!(outcome.classified.class.is_success_equivalent());
}

#[tokio::test]
async fn execute_claim_maps_a_login_wall_to_session_expired() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<!doctype html><html><body>Vui long dang nhap <a href=\"/buyer/login\">here</a></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;

    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");
    let outcome = client_for(&server)
        .execute_claim(&plan)
        .await
        .expect("claim completes");
    assert_eq!(outcome.classified.class, ClaimResultClass::SessionExpired);
    // Markup must never reach the diagnostic.
    assert_eq!(
        outcome.classified.diagnostic.message_excerpt.as_deref(),
        Some("<html document>")
    );
}

#[tokio::test]
async fn execute_claim_surfaces_a_rate_limit_with_its_retry_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "12")
                .set_body_raw(
                    r#"{"error":1,"error_msg":"too many requests"}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");
    let err = client_for(&server)
        .execute_claim(&plan)
        .await
        .expect_err("429 must not look like a completed claim");

    match &err {
        ClientError::RateLimited { retry_after } => {
            assert_eq!(*retry_after, Some(Duration::from_secs(12)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    assert_eq!(err.as_result_class(), ClaimResultClass::RateLimited);
}

#[tokio::test]
async fn execute_claim_classifies_a_server_error_as_transient() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(ResponseTemplate::new(503).set_body_raw("<html>oops</html>", "text/html"))
        .mount(&server)
        .await;

    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");
    let outcome = client_for(&server)
        .execute_claim(&plan)
        .await
        .expect("claim completes");
    assert_eq!(outcome.classified.class, ClaimResultClass::TransientFailure);
    assert!(!outcome.classified.class.is_terminal());
}

#[tokio::test]
async fn a_slow_upstream_becomes_a_typed_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SAVE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_raw(r#"{"error":0}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let client = ShopeeClient::new(ShopeeClientConfig {
        base_url: server.uri(),
        request_timeout: Duration::from_millis(150),
        connect_timeout: Duration::from_millis(150),
        ..ShopeeClientConfig::default()
    })
    .expect("client builds");

    let plan = ClaimPlan::for_voucher(&voucher_with_signature()).expect("plan builds");
    let err = client
        .execute_claim(&plan)
        .await
        .expect_err("must not report a result it never received");
    assert!(matches!(err, ClientError::Timeout), "got {err:?}");
    assert!(err.is_transient());
    assert_eq!(err.as_result_class(), ClaimResultClass::TransientFailure);
}

#[tokio::test]
async fn warm_connection_reaches_the_origin_without_the_session() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = client_for(&server);
    client.set_cookie_header(SecretString::new("SPC_EC=abc"));

    let elapsed = client.warm_connection().await.expect("warmup succeeds");
    assert!(elapsed > Duration::ZERO);

    let request = only_request(&server).await;
    assert!(
        header(&request, "cookie").is_none(),
        "warmup is unauthenticated and must not expose session material"
    );
}

#[tokio::test]
async fn a_supplied_client_is_reused_rather_than_rebuilt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PROBE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"error":0,"data":{"userid":42}}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let http = reqwest::Client::builder()
        .user_agent("shopee-hunter-test/1.0")
        .build()
        .expect("test client builds");
    let client = ShopeeClient::with_client(
        http,
        ShopeeClientConfig {
            base_url: server.uri(),
            ..ShopeeClientConfig::default()
        },
    )
    .expect("wraps the supplied client");

    // Two calls over one client: the second reuses the pooled connection.
    for _ in 0..2 {
        assert_eq!(
            client.probe_session().await.expect("probe succeeds").probe,
            SessionProbe::Healthy
        );
    }

    let requests = server
        .received_requests()
        .await
        .expect("mock server records requests");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        header(&requests[0], "user-agent").as_deref(),
        Some("shopee-hunter-test/1.0"),
        "the supplied client's configuration must be preserved"
    );
}
