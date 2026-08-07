//! Criterion micro-benchmarks for the hottest pure functions (ROADMAP Phase 24).
//!
//! Scope is deliberately tiny. These are the two functions every discovered
//! voucher passes through, they are pure and allocation-light, and they are the
//! only places where a CPU regression could plausibly show up in discovery
//! latency. Everything else in the pipeline is dominated by I/O and belongs in
//! the `benchmark_latency` harness instead.
//!
//! ```bash
//! cargo bench -p shopee-hunter-domain --bench hot_path
//! ```
//!
//! Kept fast on purpose (small sample count, short measurement window) so it
//! can run on every change rather than being skipped as too slow.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use shopee_hunter_domain::identity::{compute_identity, raw_hash, version_hash};
use shopee_hunter_domain::voucher::VoucherCandidate;

/// Representative collector payload: fully populated, because both hashing
/// functions concatenate every field and their cost tracks how many are present.
const SAMPLE_CANDIDATE_JSON: &str = r#"{
    "source": "external-feed",
    "source_key": "feed-item-8891",
    "external_id": null,
    "code": "FREESHIPXTRA",
    "promotion_id": "220481993",
    "signature": "9f2c4a1b7e3d5086",
    "title": "Giảm 200.000₫ cho đơn từ 400.000₫",
    "description": "Áp dụng cho toàn sàn, số lượng có hạn",
    "voucher_type": "PLATFORM",
    "discount_type": "FIXED_AMOUNT",
    "discount_amount": 200000,
    "discount_percent": null,
    "max_discount": 200000,
    "min_spend": 400000,
    "start_at": "2026-08-10T05:00:00Z",
    "end_at": "2026-08-17T16:59:59Z",
    "scope": {"kind": "PLATFORM"},
    "payment_method": null,
    "landing_url": "https://shopee.vn/voucher/redeem",
    "raw_payload": {"promotionid": 220481993, "signature": "9f2c4a1b7e3d5086", "quota": 5000},
    "observed_at": "2026-08-08T12:00:00Z",
    "parser_version": "external-feed/1"
}"#;

fn sample_candidate() -> VoucherCandidate {
    serde_json::from_str(SAMPLE_CANDIDATE_JSON).expect("sample candidate JSON must deserialize")
}

/// Deserializing a source payload into the canonical candidate.
fn bench_json_parse(c: &mut Criterion) {
    c.bench_function("json_parse/voucher_candidate", |b| {
        b.iter(|| {
            let candidate: VoucherCandidate =
                serde_json::from_str(black_box(SAMPLE_CANDIDATE_JSON))
                    .expect("sample candidate JSON must deserialize");
            black_box(candidate)
        });
    });
}

/// Identity computation across both realistic bases.
///
/// The two are benchmarked separately because their costs differ by an order of
/// magnitude: the `promotion_id` path is a `format!`, while the fingerprint
/// path runs SHA-256 over a joined field set. A source that stops emitting
/// promotion ids therefore shifts real CPU cost, and this bench makes that
/// visible instead of hiding it in an average.
fn bench_identity(c: &mut Criterion) {
    let with_promotion = sample_candidate();
    let mut fingerprinted = sample_candidate();
    fingerprinted.promotion_id = None;
    fingerprinted.external_id = None;

    let mut group = c.benchmark_group("identity");
    group.bench_function("compute_identity/promotion_id", |b| {
        b.iter(|| black_box(compute_identity(black_box(&with_promotion))));
    });
    group.bench_function("compute_identity/fingerprint", |b| {
        b.iter(|| black_box(compute_identity(black_box(&fingerprinted))));
    });
    group.finish();
}

/// Version hashing: runs for every observation of every voucher, so it is the
/// single most frequently executed pure function in the pipeline.
fn bench_version_hash(c: &mut Criterion) {
    let candidate = sample_candidate();

    let mut group = c.benchmark_group("hashing");
    group.bench_function("version_hash", |b| {
        b.iter(|| black_box(version_hash(black_box(&candidate))));
    });
    group.bench_function("raw_hash", |b| {
        b.iter(|| black_box(raw_hash(black_box(&candidate.raw_payload))));
    });
    group.finish();
}

/// The full per-observation CPU cost: parse, then normalize. This is the figure
/// to watch, since the individual benches above only explain where it goes.
fn bench_parse_and_normalize(c: &mut Criterion) {
    c.bench_function("pipeline/parse_and_normalize", |b| {
        b.iter(|| {
            let candidate: VoucherCandidate =
                serde_json::from_str(black_box(SAMPLE_CANDIDATE_JSON))
                    .expect("sample candidate JSON must deserialize");
            let identity = compute_identity(&candidate);
            let version = version_hash(&candidate);
            let raw = raw_hash(&candidate.raw_payload);
            black_box((identity, version, raw))
        });
    });
}

criterion_group! {
    name = hot_path;
    // Trimmed from Criterion's defaults (100 samples / 5s) so the whole suite
    // finishes in a few seconds: these functions are sub-microsecond and their
    // variance is tiny, so a smaller window still separates real regressions
    // from noise.
    config = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = bench_json_parse, bench_identity, bench_version_hash, bench_parse_and_normalize
}
criterion_main!(hot_path);
