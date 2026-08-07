//! Scenarios 1 & 2: network and upstream failures must degrade a source, never
//! the service.
//!
//! Every assertion here is about *safe* behaviour: the error is classified into
//! a bounded set, per-source health reflects it, the run is recorded for audit,
//! and no other source is affected. A collector that panicked, hung, or
//! retried in a tight loop would fail these.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{feed_body, temp_db};
use shopee_hunter_collectors::contract::{CollectionContext, VoucherCollector};
use shopee_hunter_collectors::{
    CollectorError, CollectorSupervisor, ExternalFeedCollector, SourceHealthState, SupervisedSource,
};
use shopee_hunter_observability::Metrics;
use shopee_hunter_storage::{CollectorRunRepository, VoucherRepository};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn feed_collector(name: &str, url: String, timeout: Duration) -> Arc<ExternalFeedCollector> {
    Arc::new(ExternalFeedCollector::new(
        name,
        url,
        reqwest::Client::new(),
        timeout,
    ))
}

fn context() -> CollectionContext {
    let now = Utc::now();
    CollectionContext {
        now,
        deadline: now + chrono::Duration::seconds(30),
    }
}

/// Mount a feed endpoint returning `template`, and return the collector URL.
async fn mount_feed(server: &MockServer, template: ResponseTemplate) -> String {
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(template)
        .mount(server)
        .await;
    format!("{}/feed", server.uri())
}

// ---------------------------------------------------------------------------
// 1. Network timeout and DNS failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_slow_upstream_becomes_a_bounded_timeout_not_a_hang() {
    let server = MockServer::start().await;
    let url = mount_feed(
        &server,
        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(30))
            .set_body_string(feed_body(&["a"])),
    )
    .await;

    let collector = feed_collector("slow-feed", url, Duration::from_millis(200));
    let started = std::time::Instant::now();
    let err = collector
        .collect(&context())
        .await
        .expect_err("a 30s response against a 200ms budget must fail");

    assert!(
        matches!(err, CollectorError::Timeout),
        "expected Timeout, got {err:?}"
    );
    assert!(err.is_transient(), "a timeout is retryable, not terminal");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the collector must give up on its own budget, not wait for the server"
    );
}

#[tokio::test]
async fn an_unresolvable_host_degrades_the_source_rather_than_failing_it() {
    // `.invalid` is reserved by RFC 2606 and can never resolve. Depending on
    // the resolver this surfaces as a connect error or, if DNS black-holes,
    // as the collector's own timeout — both are transient, which is the
    // property that matters: the supervisor may retry later with backoff.
    let collector = feed_collector(
        "dns-feed",
        "http://shopee-hunter-does-not-exist.invalid/feed".to_string(),
        Duration::from_secs(2),
    );

    let err = collector
        .collect(&context())
        .await
        .expect_err("an unresolvable host must fail");
    assert!(
        err.is_transient(),
        "DNS failure must be transient, got {err:?}"
    );

    let (db, _dir) = temp_db().await;
    let source = SupervisedSource::new(collector, Duration::from_secs(5));
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let _ = supervisor.run_once(&source).await;

    let health = source.health();
    assert_eq!(health.state, SourceHealthState::Degraded);
    assert_eq!(health.consecutive_failures, 1);
    assert!(health.last_failure.is_some());
    assert!(health.last_success.is_none());
}

#[tokio::test]
async fn a_refused_connection_is_classified_transient() {
    // Port 1 on loopback is reliably closed: a deterministic connect failure
    // with no DNS involvement.
    let collector = feed_collector(
        "refused-feed",
        "http://127.0.0.1:1/feed".to_string(),
        Duration::from_secs(2),
    );
    let err = collector
        .collect(&context())
        .await
        .expect_err("a closed port must fail");
    assert!(
        matches!(err, CollectorError::Transient(_)),
        "expected Transient, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Rate limiting and malformed payloads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_429_is_classified_rate_limited_and_marks_the_source() {
    let server = MockServer::start().await;
    let url = mount_feed(
        &server,
        ResponseTemplate::new(429)
            .insert_header("retry-after", "30")
            .set_body_string("slow down"),
    )
    .await;

    let collector = feed_collector("limited-feed", url, Duration::from_secs(5));
    let err = collector
        .collect(&context())
        .await
        .expect_err("429 must not look like a successful collection");

    match &err {
        CollectorError::RateLimited(detail) => {
            assert!(detail.contains("429"));
            assert!(
                detail.contains("30"),
                "the Retry-After hint must survive classification: {detail}"
            );
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }

    let (db, _dir) = temp_db().await;
    let source = SupervisedSource::new(collector, Duration::from_secs(10));
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let _ = supervisor.run_once(&source).await;

    assert_eq!(
        source.health().state,
        SourceHealthState::RateLimited,
        "rate limiting is its own health state so polling can back off"
    );
}

#[tokio::test]
async fn a_malformed_body_degrades_the_source_and_is_not_retried_blindly() {
    let server = MockServer::start().await;
    let url = mount_feed(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>definitely not the feed</html>"),
    )
    .await;

    let (db, _dir) = temp_db().await;
    let collector = feed_collector("broken-feed", url, Duration::from_secs(5));
    let source = SupervisedSource::new(collector, Duration::from_secs(10));
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());

    let err = supervisor
        .run_once(&source)
        .await
        .expect_err("a malformed feed must not be reported as a successful run");

    assert!(
        matches!(err, CollectorError::Malformed(_)),
        "expected Malformed, got {err:?}"
    );
    assert!(
        !err.is_transient(),
        "a malformed payload must NOT be retried like a network blip: \
         retrying a schema change just burns the request budget"
    );

    let health = source.health();
    assert_eq!(health.state, SourceHealthState::Failed);
    assert!(health.detail.is_some(), "the failure must be diagnosable");

    // The failed run is still recorded, so Phase 27 analytics can see it.
    let runs = CollectorRunRepository::new(&db);
    assert_eq!(runs.count_for("broken-feed").await.expect("count"), 1);
    assert_eq!(
        runs.last_success_at("broken-feed").await.expect("last"),
        None
    );

    // Nothing bogus reached the canonical store.
    assert_eq!(
        VoucherRepository::new(&db).count().await.expect("count"),
        0,
        "a malformed response must never create vouchers"
    );
}

#[tokio::test]
async fn a_server_error_is_transient_but_a_client_error_is_not() {
    for (status, expect_transient) in [(500_u16, true), (503, true), (404, false), (400, false)] {
        let server = MockServer::start().await;
        let url = mount_feed(&server, ResponseTemplate::new(status)).await;
        let collector = feed_collector("status-feed", url, Duration::from_secs(5));

        let err = match collector.collect(&context()).await {
            Err(err) => err,
            Ok(_) => panic!("HTTP {status} must not be reported as a successful collection"),
        };
        assert_eq!(
            err.is_transient(),
            expect_transient,
            "HTTP {status} classified as {err:?}: a 5xx is worth retrying, a 4xx is a \
             contract problem that retrying cannot fix"
        );
    }
}

// ---------------------------------------------------------------------------
// Supervisor isolation: one bad source must not affect another
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_failing_source_does_not_disturb_a_healthy_one() {
    let server = MockServer::start().await;
    let good_url = mount_feed(
        &server,
        ResponseTemplate::new(200).set_body_string(feed_body(&["v1", "v2"])),
    )
    .await;

    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());

    let healthy = SupervisedSource::new(
        feed_collector("healthy-feed", good_url, Duration::from_secs(5)),
        Duration::from_secs(10),
    );
    let broken = SupervisedSource::new(
        feed_collector(
            "dead-feed",
            "http://127.0.0.1:1/feed".to_string(),
            Duration::from_secs(2),
        ),
        Duration::from_secs(5),
    );

    // Run the broken source FIRST: a failure must not poison what follows.
    assert!(supervisor.run_once(&broken).await.is_err());
    let outcome = supervisor
        .run_once(&healthy)
        .await
        .expect("the healthy source must still collect");

    assert_eq!(outcome.new_count, 2);
    assert_eq!(healthy.health().state, SourceHealthState::Healthy);
    assert_eq!(broken.health().state, SourceHealthState::Degraded);

    // And again, interleaved, to show the isolation is not order-dependent.
    assert!(supervisor.run_once(&broken).await.is_err());
    let second = supervisor
        .run_once(&healthy)
        .await
        .expect("still healthy after a second neighbouring failure");
    assert_eq!(second.new_count, 0, "already known, so nothing new");
    assert_eq!(second.unchanged_count, 2);

    assert_eq!(
        VoucherRepository::new(&db).count().await.expect("count"),
        2,
        "the healthy source's vouchers persisted despite its neighbour failing"
    );
    assert_eq!(broken.health().consecutive_failures, 2);
    assert_eq!(healthy.health().consecutive_failures, 0);
}

/// Failures must not accumulate unbounded state in the supervisor: health is a
/// fixed-size snapshot regardless of how many times a source fails.
#[tokio::test]
async fn repeated_failures_only_advance_a_counter() {
    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let broken = SupervisedSource::new(
        feed_collector(
            "flapping-feed",
            "http://127.0.0.1:1/feed".to_string(),
            Duration::from_millis(500),
        ),
        Duration::from_secs(2),
    );

    for expected in 1..=5_u32 {
        assert!(supervisor.run_once(&broken).await.is_err());
        assert_eq!(broken.health().consecutive_failures, expected);
    }

    assert_eq!(broken.health().state, SourceHealthState::Degraded);
    // Every attempt is auditable, and that is the only thing that grew.
    assert_eq!(
        CollectorRunRepository::new(&db)
            .count_for("flapping-feed")
            .await
            .expect("count"),
        5
    );
}

/// A source that recovers must return to Healthy: degradation is not sticky.
#[tokio::test]
async fn a_recovered_source_returns_to_healthy() {
    let server = MockServer::start().await;
    // First request fails, subsequent ones succeed.
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(feed_body(&["r1"])))
        .mount(&server)
        .await;

    let (db, _dir) = temp_db().await;
    let supervisor = CollectorSupervisor::new(db.clone(), Metrics::new());
    let source = SupervisedSource::new(
        feed_collector(
            "recovering-feed",
            format!("{}/feed", server.uri()),
            Duration::from_secs(5),
        ),
        Duration::from_secs(10),
    );

    assert!(supervisor.run_once(&source).await.is_err());
    assert_eq!(source.health().state, SourceHealthState::Degraded);

    supervisor.run_once(&source).await.expect("recovered");
    let health = source.health();
    assert_eq!(health.state, SourceHealthState::Healthy);
    assert_eq!(
        health.consecutive_failures, 0,
        "the failure counter resets on success, so backoff does not persist forever"
    );
    assert_eq!(VoucherRepository::new(&db).count().await.expect("count"), 1);
}
