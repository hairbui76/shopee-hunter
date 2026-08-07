//! Analytics integration tests against a temporary SQLite database.
//!
//! Seeds real `collector_runs`, `vouchers`, and `voucher_observations` through
//! the storage repositories — never raw INSERTs — so the tests exercise the
//! same rows production writes.

use chrono::{DateTime, Duration, TimeZone, Utc};
use shopee_hunter_analytics::{
    AnalyticsRepository, AnalyticsWindow, DegradeAction, Ratio, BASELINE_SCORE,
};
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
use shopee_hunter_domain::SourceId;
use shopee_hunter_storage::{
    CollectorRunRecord, CollectorRunRepository, Database, RunOutcome, VoucherRepository,
};

async fn temp_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("analytics.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url, 4).await.expect("connect");
    (db, dir)
}

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
        .single()
        .expect("fixed test timestamp is unambiguous")
}

/// Record one collector run.
#[allow(clippy::too_many_arguments)]
async fn record_run(
    db: &Database,
    source: &str,
    at: DateTime<Utc>,
    outcome: RunOutcome,
    candidates: i64,
    new_count: i64,
    updated: i64,
    parse_errors: i64,
    detail: Option<&str>,
) {
    CollectorRunRepository::new(db)
        .record(
            &CollectorRunRecord {
                source: source.to_string(),
                started_at: at,
                finished_at: Some(at + Duration::seconds(1)),
                latency_ms: Some(120),
                candidate_count: candidates,
                new_count,
                updated_count: updated,
                parse_errors,
                detail: detail.map(str::to_string),
            },
            outcome,
        )
        .await
        .expect("record run");
}

/// A healthy run that found one new voucher.
async fn good_run(db: &Database, source: &str, at: DateTime<Utc>) {
    record_run(db, source, at, RunOutcome::Success, 1, 1, 0, 0, None).await;
}

fn candidate(source: &str, promotion_id: &str, observed_at: DateTime<Utc>) -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new(source),
        source_key: format!("key-{promotion_id}"),
        external_id: None,
        code: Some(format!("CODE{promotion_id}")),
        // `promo:` identity is global across sources, so two sources reporting
        // the same promotion converge on one logical voucher — which is what
        // makes first-discovery attribution meaningful.
        promotion_id: Some(promotion_id.to_string()),
        signature: Some("sig".into()),
        title: format!("Voucher {promotion_id}"),
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
        raw_payload: serde_json::json!({"promo": promotion_id}),
        observed_at,
        parser_version: "test-1".into(),
    }
}

async fn observe(db: &Database, source: &str, promotion_id: &str, at: DateTime<Utc>) {
    VoucherRepository::new(db)
        .upsert_candidate(&candidate(source, promotion_id, at), at)
        .await
        .expect("upsert candidate");
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_empty_database_reports_no_sources() {
    let (db, _dir) = temp_db().await;
    let analytics = AnalyticsRepository::new(&db);

    assert!(analytics
        .source_stats(AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .is_empty());
    assert!(analytics
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report")
        .is_empty());
    assert_eq!(
        analytics
            .source_stats_for("missing", AnalyticsWindow::ALL_TIME)
            .await
            .expect("stats"),
        None
    );
}

#[tokio::test]
async fn counters_and_rates_come_from_the_persisted_runs() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    // 4 runs: 3 succeed, 1 fails. 10 candidates, 4 useful, 10 parse errors.
    record_run(&db, "feed", now, RunOutcome::Success, 4, 2, 1, 0, None).await;
    record_run(
        &db,
        "feed",
        now + Duration::minutes(1),
        RunOutcome::Success,
        4,
        1,
        0,
        0,
        None,
    )
    .await;
    record_run(
        &db,
        "feed",
        now + Duration::minutes(2),
        RunOutcome::Partial,
        2,
        0,
        0,
        10,
        Some("2 items unparsable"),
    )
    .await;
    record_run(
        &db,
        "feed",
        now + Duration::minutes(3),
        RunOutcome::Failed,
        0,
        0,
        0,
        0,
        Some("connection refused"),
    )
    .await;

    let stats = AnalyticsRepository::new(&db)
        .source_stats_for("feed", AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .expect("feed has activity");

    assert_eq!(stats.runs, 4);
    assert_eq!(stats.successful_runs, 2);
    assert_eq!(stats.failed_runs, 1);
    assert_eq!(stats.candidates, 10);
    assert_eq!(stats.new_items, 3);
    assert_eq!(stats.updated_items, 1);
    assert_eq!(stats.parse_errors, 10);

    // 10 errors out of 20 items encountered.
    assert_eq!(
        stats.parse_failure_rate.map(Ratio::percent_string),
        Some("50.00%".to_string())
    );
    // 10 candidates, 4 useful => 6 stale.
    assert_eq!(
        stats.stale_candidate_rate.map(Ratio::percent_string),
        Some("60.00%".to_string())
    );
    // 4 runs / 4 useful discoveries.
    assert_eq!(
        stats
            .requests_per_useful_discovery
            .map(Ratio::decimal_string),
        Some("1.00".to_string())
    );
    assert_eq!(
        stats.failure_rate.map(Ratio::percent_string),
        Some("25.00%".to_string())
    );
    // Last SUCCESS finished a second after it started.
    assert_eq!(
        stats.last_success_at,
        Some(now + Duration::minutes(1) + Duration::seconds(1))
    );
}

#[tokio::test]
async fn first_discovery_wins_are_attributed_across_two_sources() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    // "fast" sees promo-1 ten minutes before "slow"...
    observe(&db, "fast", "promo-1", now).await;
    observe(&db, "slow", "promo-1", now + Duration::minutes(10)).await;
    // ...and promo-2 twenty minutes before it.
    observe(&db, "fast", "promo-2", now).await;
    observe(&db, "slow", "promo-2", now + Duration::minutes(20)).await;
    // "slow" wins promo-3 outright, unopposed.
    observe(&db, "slow", "promo-3", now).await;

    let stats = AnalyticsRepository::new(&db)
        .source_stats(AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats");
    let fast = stats.iter().find(|s| s.source == "fast").expect("fast");
    let slow = stats.iter().find(|s| s.source == "slow").expect("slow");

    assert_eq!(fast.first_discovery_wins, 2);
    assert_eq!(slow.first_discovery_wins, 1);
    // Wins are attributed exactly once per voucher.
    assert_eq!(fast.first_discovery_wins + slow.first_discovery_wins, 3);

    // Mean lead over the runner-up: (10m + 20m) / 2.
    assert_eq!(fast.lead_sample_size, 2);
    assert_eq!(fast.avg_discovery_lead(), Some(Duration::minutes(15)));
    // "slow" only ever won unopposed, so it has no comparable lead.
    assert_eq!(slow.lead_sample_size, 0);
    assert_eq!(slow.avg_discovery_lead(), None);

    // The canonical voucher records the source that created them.
    assert_eq!(fast.new_unique_vouchers, 2);
    assert_eq!(slow.new_unique_vouchers, 1);
}

#[tokio::test]
async fn a_source_that_only_ever_arrives_second_still_appears() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    observe(&db, "fast", "promo-1", now).await;
    observe(&db, "laggard", "promo-1", now + Duration::minutes(5)).await;

    let stats = AnalyticsRepository::new(&db)
        .source_stats(AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats");
    let laggard = stats
        .iter()
        .find(|s| s.source == "laggard")
        .expect("a source with no wins must still be reported");
    assert_eq!(laggard.first_discovery_wins, 0);
    assert_eq!(laggard.new_unique_vouchers, 0);
}

#[tokio::test]
async fn rate_limit_incidents_are_counted_from_run_detail() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    for i in 0..10 {
        good_run(&db, "feed", now + Duration::minutes(i)).await;
    }
    for (i, detail) in [
        "HTTP 429 from upstream",
        "rate limit exceeded",
        "Rate Limit hit",
    ]
    .iter()
    .enumerate()
    {
        record_run(
            &db,
            "feed",
            now + Duration::hours(1) + Duration::minutes(i as i64),
            RunOutcome::Partial,
            0,
            0,
            0,
            0,
            Some(detail),
        )
        .await;
    }
    // Unrelated failure detail must not be miscounted.
    record_run(
        &db,
        "feed",
        now + Duration::hours(2),
        RunOutcome::Failed,
        0,
        0,
        0,
        0,
        Some("connection reset by peer"),
    )
    .await;

    let stats = AnalyticsRepository::new(&db)
        .source_stats_for("feed", AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .expect("feed has activity");

    assert_eq!(
        stats.rate_limit_incidents, 3,
        "case-insensitive marker match"
    );
    assert_eq!(stats.runs, 14);
}

#[tokio::test]
async fn the_window_excludes_older_activity() {
    let (db, _dir) = temp_db().await;
    let now = base_time();
    let long_ago = now - Duration::days(30);

    for i in 0..5 {
        good_run(&db, "feed", long_ago + Duration::minutes(i)).await;
    }
    observe(&db, "feed", "old-promo", long_ago).await;

    for i in 0..2 {
        good_run(&db, "feed", now + Duration::minutes(i)).await;
    }
    observe(&db, "feed", "new-promo", now).await;

    let analytics = AnalyticsRepository::new(&db);

    let lifetime = analytics
        .source_stats_for("feed", AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .expect("activity");
    assert_eq!(lifetime.runs, 7);
    assert_eq!(lifetime.new_unique_vouchers, 2);

    let recent = analytics
        .source_stats_for("feed", AnalyticsWindow::trailing(Duration::days(7), now))
        .await
        .expect("stats")
        .expect("activity");
    assert_eq!(recent.runs, 2, "runs before the window must be excluded");
    assert_eq!(recent.new_unique_vouchers, 1);
    assert_eq!(recent.first_discovery_wins, 1);
}

#[tokio::test]
async fn a_broken_parser_is_recommended_for_disabling() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    // 20 runs where 9 of every 10 items fail to parse.
    for i in 0..20 {
        record_run(
            &db,
            "broken",
            now + Duration::minutes(i),
            RunOutcome::Partial,
            1,
            0,
            0,
            9,
            Some("schema mismatch"),
        )
        .await;
    }

    let report = AnalyticsRepository::new(&db)
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report");
    let broken = report
        .iter()
        .find(|r| r.stats.source == "broken")
        .expect("broken source");

    assert_eq!(
        broken.stats.parse_failure_rate.map(Ratio::percent_string),
        Some("90.00%".to_string())
    );
    match broken.recommendation.as_ref().expect("a recommendation") {
        DegradeAction::Disable { reason } => assert!(reason.contains("schema")),
        other => panic!("expected Disable, got {other:?}"),
    }
    assert!(broken.quality.value < BASELINE_SCORE);
    assert!(broken.quality.has_reason("parse failures"));
}

#[tokio::test]
async fn heavy_rate_limiting_is_recommended_for_slower_polling() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    for i in 0..10 {
        good_run(&db, "hot", now + Duration::minutes(i)).await;
    }
    for i in 0..10 {
        record_run(
            &db,
            "hot",
            now + Duration::hours(1) + Duration::minutes(i),
            RunOutcome::Partial,
            0,
            0,
            0,
            0,
            Some("429 Too Many Requests"),
        )
        .await;
    }

    let stats = AnalyticsRepository::new(&db)
        .source_stats_for("hot", AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .expect("activity");

    match shopee_hunter_analytics::should_degrade(&stats).expect("a recommendation") {
        DegradeAction::ReducePolling { factor, reason } => {
            assert!(factor > 1);
            assert!(reason.contains("rate limited"));
        }
        other => panic!("expected ReducePolling, got {other:?}"),
    }
}

#[tokio::test]
async fn a_healthy_source_is_left_alone_and_scores_the_baseline() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    for i in 0..20 {
        good_run(&db, "healthy", now + Duration::minutes(i)).await;
        observe(
            &db,
            "healthy",
            &format!("promo-{i}"),
            now + Duration::minutes(i),
        )
        .await;
    }

    let report = AnalyticsRepository::new(&db)
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report");
    let healthy = report
        .iter()
        .find(|r| r.stats.source == "healthy")
        .expect("healthy source");

    assert_eq!(healthy.recommendation, None);
    assert_eq!(
        healthy.quality.value,
        BASELINE_SCORE,
        "{}",
        healthy.quality.explain()
    );
    assert_eq!(healthy.stats.new_unique_vouchers, 20);
    assert!(healthy.stats.ever_succeeded());
    assert!(healthy.stats.produced_anything());
}

#[tokio::test]
async fn a_brand_new_source_is_never_degraded_on_thin_evidence() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    // Catastrophic, but only three runs of evidence.
    for i in 0..3 {
        record_run(
            &db,
            "fresh",
            now + Duration::minutes(i),
            RunOutcome::Failed,
            0,
            0,
            0,
            50,
            Some("everything is broken"),
        )
        .await;
    }

    let report = AnalyticsRepository::new(&db)
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report");
    let fresh = report
        .iter()
        .find(|r| r.stats.source == "fresh")
        .expect("fresh source");

    assert_eq!(fresh.recommendation, None, "not enough evidence to judge");
    assert!(!fresh.quality.has_enough_evidence);
    assert_eq!(fresh.quality.value, BASELINE_SCORE);
}

#[tokio::test]
async fn the_report_lists_the_worst_source_first() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    for i in 0..20 {
        good_run(&db, "healthy", now + Duration::minutes(i)).await;
        observe(
            &db,
            "healthy",
            &format!("promo-{i}"),
            now + Duration::minutes(i),
        )
        .await;
        record_run(
            &db,
            "broken",
            now + Duration::minutes(i),
            RunOutcome::Failed,
            0,
            0,
            0,
            10,
            Some("schema mismatch"),
        )
        .await;
    }

    let report = AnalyticsRepository::new(&db)
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report");

    assert_eq!(report.len(), 2);
    assert_eq!(report[0].stats.source, "broken", "worst quality first");
    assert_eq!(report[1].stats.source, "healthy");
    assert!(report[0].quality.value < report[1].quality.value);

    // Deterministic across runs.
    let again = AnalyticsRepository::new(&db)
        .report(AnalyticsWindow::ALL_TIME)
        .await
        .expect("report");
    let order: Vec<&str> = report.iter().map(|r| r.stats.source.as_str()).collect();
    let order_again: Vec<&str> = again.iter().map(|r| r.stats.source.as_str()).collect();
    assert_eq!(order, order_again);
}

#[tokio::test]
async fn sources_are_reported_even_without_a_run_log() {
    let (db, _dir) = temp_db().await;
    let now = base_time();

    // Vouchers discovered but no collector_runs rows (e.g. a manual import).
    observe(&db, "manual", "promo-1", now).await;

    let stats = AnalyticsRepository::new(&db)
        .source_stats_for("manual", AnalyticsWindow::ALL_TIME)
        .await
        .expect("stats")
        .expect("a source known only from vouchers must still be reported");

    assert_eq!(stats.runs, 0);
    assert_eq!(stats.new_unique_vouchers, 1);
    assert_eq!(stats.first_discovery_wins, 1);
    // No runs means no denominators: rates are undefined, not zero.
    assert_eq!(stats.parse_failure_rate, None);
    assert_eq!(stats.requests_per_useful_discovery, None);
}
