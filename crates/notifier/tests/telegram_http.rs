//! Telegram transport tests against a mock HTTP server.
//!
//! These cover the wire contract (endpoint, payload shape), the response
//! classification, the bounded retry budget, and the guarantee that the bot
//! token never escapes through an error message. No live Telegram account is
//! involved, so they run in normal CI.

use std::time::Duration;

use shopee_hunter_notifier::{Notifier, NotifierError, RetryPolicy, TelegramNotifier};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:SUPER-SECRET-TOKEN";
const CHAT: &str = "555000";

fn fast_retry(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    }
}

fn notifier(server: &MockServer, max_attempts: u32) -> TelegramNotifier {
    TelegramNotifier::new(TOKEN)
        .expect("builds")
        .with_base_url(server.uri())
        .with_retry(fast_retry(max_attempts))
}

fn send_message_path() -> String {
    format!("/bot{TOKEN}/sendMessage")
}

#[tokio::test]
async fn posts_message_to_the_bot_send_message_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .and(body_partial_json(serde_json::json!({
            "chat_id": CHAT,
            "text": "NEW VOUCHER\nFreeship 50k",
            "disable_web_page_preview": true,
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": {}})),
        )
        .expect(1)
        .mount(&server)
        .await;

    notifier(&server, 3)
        .send(CHAT, "NEW VOUCHER\nFreeship 50k")
        .await
        .expect("delivery succeeds");
}

#[tokio::test]
async fn retries_transient_server_error_then_succeeds() {
    let server = MockServer::start().await;
    // First response fails, the mock then stops matching and the success mock
    // takes over (wiremock matches mounted mocks in order).
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    notifier(&server, 3)
        .send(CHAT, "hello")
        .await
        .expect("retry recovers");

    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 2, "one failure plus one success");
}

#[tokio::test]
async fn rate_limit_retries_are_bounded_by_the_policy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(429).set_body_json(
            serde_json::json!({"ok": false, "description": "Too Many Requests: retry later"}),
        ))
        .mount(&server)
        .await;

    let err = notifier(&server, 3)
        .send(CHAT, "hello")
        .await
        .expect_err("never succeeds");

    match err {
        NotifierError::RetriesExhausted { attempts, source } => {
            assert_eq!(attempts, 3);
            assert!(matches!(*source, NotifierError::RateLimited { .. }));
        }
        other => panic!("unexpected error: {other}"),
    }

    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 3, "retry budget must be bounded");
}

#[tokio::test]
async fn unauthorized_is_terminal_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            serde_json::json!({"ok": false, "description": "Unauthorized: bot token invalid"}),
        ))
        .mount(&server)
        .await;

    let err = notifier(&server, 5)
        .send(CHAT, "hello")
        .await
        .expect_err("must fail");

    assert!(matches!(err, NotifierError::Unauthorized { .. }));
    assert!(err.needs_owner_action());
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1, "terminal errors must not be retried");
}

#[tokio::test]
async fn chat_errors_are_classified_as_invalid_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({"ok": false, "description": "Bad Request: chat not found"}),
        ))
        .mount(&server)
        .await;

    let err = notifier(&server, 3)
        .send(CHAT, "hello")
        .await
        .expect_err("must fail");

    assert!(matches!(err, NotifierError::InvalidRequest { .. }));
    assert!(err.to_string().contains("chat not found"));
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
}

#[tokio::test]
async fn upstream_description_reaches_the_error_without_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(send_message_path()))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "ok": false,
            "description": format!("Forbidden: token {TOKEN} was revoked"),
        })))
        .mount(&server)
        .await;

    let err = notifier(&server, 2)
        .send(CHAT, "hello")
        .await
        .expect_err("must fail");

    let rendered = err.to_string();
    assert!(rendered.contains("Forbidden"));
    assert!(
        !rendered.contains("SUPER-SECRET-TOKEN"),
        "token leaked into error: {rendered}"
    );
}

#[tokio::test]
async fn transport_failures_never_leak_the_token_bearing_url() {
    // Port 1 is reserved and refuses connections immediately.
    let notifier = TelegramNotifier::new(TOKEN)
        .expect("builds")
        .with_base_url("http://127.0.0.1:1")
        .with_retry(fast_retry(1));

    let err = notifier.send(CHAT, "hello").await.expect_err("must fail");
    let rendered = format!("{err}");
    let debug = format!("{err:?}");

    assert!(
        !rendered.contains("SUPER-SECRET-TOKEN") && !debug.contains("SUPER-SECRET-TOKEN"),
        "token leaked into transport error: {rendered} / {debug}"
    );
}
