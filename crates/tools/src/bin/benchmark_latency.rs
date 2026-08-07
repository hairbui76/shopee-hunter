//! End-to-end latency harness for the application-owned stages (ROADMAP Phase 24).
//!
//! Measures every stage the application controls and prints them side by side,
//! so optimisation decisions come from measurement rather than intuition. Build
//! in release mode for numbers that mean anything:
//!
//! ```bash
//! cargo run --release -p shopee-hunter-tools --bin benchmark_latency
//! ```
//!
//! # What "hot path" means here
//!
//! The claim path is split into two phases (ARCHITECTURE.md §13). Only the
//! `T=0` column matters for claim latency:
//!
//! * **Preparation** (minutes before the target): parse, normalize, persist,
//!   build the [`ClaimPlan`], warm the connection pool. Cost here is free — it
//!   happens while the system is idle.
//! * **`T=0`**: wake from the monotonic deadline and write an already-encoded
//!   request to an already-open socket. Anything measurable that appears here
//!   is a bug in the design, not a tuning opportunity.
//!
//! # What this harness deliberately does not measure
//!
//! Network time — DNS, TCP, TLS, TTFB — is **not** exercised. It needs a live
//! Shopee endpoint from the deployment host, which is neither available nor
//! appropriate in CI (CLAUDE.md: live checks are opt-in and never in ordinary
//! CI). See `docs/latency-budget.md` for the separate live procedure. The
//! numbers below are therefore the *application's* contribution only; in
//! production, network latency dominates all of them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::Utc;
use shopee_hunter_client::{ClaimPlan, ShopeeClient, ShopeeClientConfig};
use shopee_hunter_domain::identity::{compute_identity, raw_hash, version_hash};
use shopee_hunter_domain::voucher::{Voucher, VoucherCandidate};
use shopee_hunter_domain::SystemClock;
use shopee_hunter_scheduler::precision::PrecisionRunner;
use shopee_hunter_storage::{Database, VoucherRepository};

/// A representative Shopee voucher payload, shaped like what a collector hands
/// to the normalization pipeline. Realistic field population matters: hashing
/// cost scales with how many fields are actually present.
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

/// Iterations discarded before measuring, so caches and branch predictors are
/// warm and the first-call cost does not skew the percentiles.
const WARMUP: usize = 32;
/// Iterations for cheap pure functions.
const CPU_SAMPLES: usize = 2_000;
/// Iterations for anything touching the filesystem.
const IO_SAMPLES: usize = 200;
/// Iterations for the scheduler wait (each one really sleeps).
const TIMER_SAMPLES: usize = 40;
/// How far ahead the precision runner aims during measurement.
const TIMER_TARGET: Duration = Duration::from_millis(25);

/// Where a stage sits relative to the claim deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Runs once at process start.
    Startup,
    /// Runs well before `T=0`; cost is absorbed by idle time.
    Preparation,
    /// Runs at the claim deadline. This is the budget that matters.
    HotPath,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Preparation => "prepare",
            Self::HotPath => "T=0",
        }
    }
}

/// Timing samples for one stage, in nanoseconds.
struct Stage {
    name: &'static str,
    phase: Phase,
    samples: Vec<u128>,
}

impl Stage {
    fn new(name: &'static str, phase: Phase, mut samples: Vec<u128>) -> Self {
        samples.sort_unstable();
        Self {
            name,
            phase,
            samples,
        }
    }

    /// Integer-indexed percentile over the pre-sorted samples. No floating
    /// point: the index is computed with exact integer arithmetic so repeated
    /// runs over identical data report identical figures.
    fn percentile(&self, numerator: usize, denominator: usize) -> u128 {
        if self.samples.is_empty() {
            return 0;
        }
        let last = self.samples.len() - 1;
        let index = (last * numerator) / denominator;
        self.samples[index]
    }

    fn min(&self) -> u128 {
        self.samples.first().copied().unwrap_or(0)
    }

    fn max(&self) -> u128 {
        self.samples.last().copied().unwrap_or(0)
    }
}

/// Render nanoseconds with a fixed three-decimal mantissa, chosen so columns
/// line up and small regressions stay visible.
fn format_ns(nanos: u128) -> String {
    if nanos >= 1_000_000_000 {
        format!(
            "{}.{:03} s",
            nanos / 1_000_000_000,
            (nanos % 1_000_000_000) / 1_000_000
        )
    } else if nanos >= 1_000_000 {
        format!(
            "{}.{:03} ms",
            nanos / 1_000_000,
            (nanos % 1_000_000) / 1_000
        )
    } else if nanos >= 1_000 {
        format!("{}.{:03} us", nanos / 1_000, nanos % 1_000)
    } else {
        format!("{nanos} ns")
    }
}

/// Signed microseconds, for scheduler lag where firing early is meaningful.
fn format_signed_us(nanos: i128) -> String {
    let sign = if nanos < 0 { "-" } else { "+" };
    let abs = nanos.unsigned_abs();
    format!("{sign}{}.{:03} us", abs / 1_000, abs % 1_000)
}

/// Time one closure `count` times after `WARMUP` discarded iterations.
fn measure<F, T>(count: usize, mut body: F) -> Vec<u128>
where
    F: FnMut() -> T,
{
    for _ in 0..WARMUP {
        std::hint::black_box(body());
    }
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        let output = body();
        samples.push(started.elapsed().as_nanos());
        // Keep the optimiser from deleting work whose result is unused.
        std::hint::black_box(output);
    }
    samples
}

fn parse_sample_candidate() -> Result<VoucherCandidate> {
    serde_json::from_str(SAMPLE_CANDIDATE_JSON).context("sample candidate JSON must deserialize")
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

fn stage_json_parse() -> Stage {
    let samples = measure(CPU_SAMPLES, || {
        serde_json::from_str::<VoucherCandidate>(SAMPLE_CANDIDATE_JSON).ok()
    });
    Stage::new(
        "json parse (collector payload)",
        Phase::Preparation,
        samples,
    )
}

fn stage_normalize(candidate: &VoucherCandidate) -> Stage {
    let samples = measure(CPU_SAMPLES, || {
        let identity = compute_identity(candidate);
        let version = version_hash(candidate);
        let raw = raw_hash(&candidate.raw_payload);
        (identity, version, raw)
    });
    Stage::new("normalize + identity + hashes", Phase::Preparation, samples)
}

fn stage_claim_plan(voucher: &Voucher) -> Stage {
    let samples = measure(CPU_SAMPLES, || ClaimPlan::for_voucher(voucher).ok());
    Stage::new(
        "claim plan build (incl. encode)",
        Phase::Preparation,
        samples,
    )
}

/// The only work genuinely left at `T=0`: hand the pre-encoded body to the
/// transport. Measured as the plan lookup plus the body copy, since the send
/// itself needs a live socket.
fn stage_hot_path_handoff(plan: &ClaimPlan) -> Stage {
    let samples = measure(CPU_SAMPLES, || plan.body_bytes().to_vec());
    Stage::new("T=0 request body handoff", Phase::HotPath, samples)
}

fn stage_client_construction() -> Stage {
    let samples = measure(IO_SAMPLES, || {
        ShopeeClient::new(ShopeeClientConfig::default()).ok()
    });
    Stage::new("shopee client construction", Phase::Startup, samples)
}

async fn stage_storage_upsert(db: &Database) -> Result<(Stage, Stage)> {
    let repo = VoucherRepository::new(db);
    let base = parse_sample_candidate()?;

    // Warm the pool and the page cache so the first real insert is not an
    // outlier caused by connection setup.
    for index in 0..WARMUP {
        let mut candidate = base.clone();
        candidate.promotion_id = Some(format!("warmup-{index}"));
        repo.upsert_candidate(&candidate, Utc::now())
            .await
            .context("warmup upsert")?;
    }

    // Insert path: a distinct logical voucher every iteration.
    let mut insert_samples = Vec::with_capacity(IO_SAMPLES);
    for index in 0..IO_SAMPLES {
        let mut candidate = base.clone();
        candidate.promotion_id = Some(format!("bench-insert-{index}"));
        let started = Instant::now();
        repo.upsert_candidate(&candidate, Utc::now())
            .await
            .context("insert-path upsert")?;
        insert_samples.push(started.elapsed().as_nanos());
    }

    // Steady state: the same voucher seen again, which is what a polling
    // collector actually does most of the time.
    let mut repeat = base.clone();
    repeat.promotion_id = Some("bench-steady-state".to_string());
    repo.upsert_candidate(&repeat, Utc::now())
        .await
        .context("seed steady-state voucher")?;

    let mut unchanged_samples = Vec::with_capacity(IO_SAMPLES);
    for _ in 0..IO_SAMPLES {
        let started = Instant::now();
        repo.upsert_candidate(&repeat, Utc::now())
            .await
            .context("unchanged-path upsert")?;
        unchanged_samples.push(started.elapsed().as_nanos());
    }

    Ok((
        Stage::new(
            "storage upsert (new voucher)",
            Phase::Preparation,
            insert_samples,
        ),
        Stage::new(
            "storage upsert (already known)",
            Phase::Preparation,
            unchanged_samples,
        ),
    ))
}

/// Scheduler wake accuracy: how far from the planned instant the precision
/// runner actually resumes. Reported separately from the stage table because
/// the interesting quantity is a *signed* error, not a duration.
async fn measure_scheduler_lag() -> Vec<i128> {
    let runner = PrecisionRunner::new(SystemClock);
    let mut errors = Vec::with_capacity(TIMER_SAMPLES);
    for _ in 0..TIMER_SAMPLES {
        let target = Utc::now()
            + chrono::Duration::from_std(TIMER_TARGET).unwrap_or_else(|_| chrono::Duration::zero());
        let before = Instant::now();
        let report = runner.wait_until(target).await;
        let slept = before.elapsed();
        // Monotonic error: how much longer the wait actually took than asked.
        let error = slept.as_nanos() as i128 - TIMER_TARGET.as_nanos() as i128;
        // `report.lag_ms` is the wall-clock view the scheduler records; keep it
        // referenced so this stays tied to the real API rather than a bare sleep.
        std::hint::black_box(report.lag_ms);
        errors.push(error);
    }
    errors.sort_unstable();
    errors
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn print_table(stages: &[Stage]) {
    println!(
        "{:<34} {:>8} {:>7} {:>12} {:>12} {:>12} {:>12}",
        "STAGE", "PHASE", "N", "MIN", "P50", "P95", "MAX"
    );
    println!("{}", "-".repeat(102));
    for stage in stages {
        println!(
            "{:<34} {:>8} {:>7} {:>12} {:>12} {:>12} {:>12}",
            stage.name,
            stage.phase.label(),
            stage.samples.len(),
            format_ns(stage.min()),
            format_ns(stage.percentile(1, 2)),
            format_ns(stage.percentile(95, 100)),
            format_ns(stage.max()),
        );
    }
}

fn print_scheduler_lag(errors: &[i128]) {
    if errors.is_empty() {
        return;
    }
    let last = errors.len() - 1;
    let pick = |numerator: usize, denominator: usize| errors[(last * numerator) / denominator];
    let sum: i128 = errors.iter().sum();
    let mean = sum / errors.len() as i128;

    println!();
    println!("Scheduler wake error (planned vs actual, target {TIMER_TARGET:?})");
    println!("{}", "-".repeat(102));
    println!(
        "  samples {:<6} min {:>14}  p50 {:>14}  p95 {:>14}  max {:>14}  mean {:>14}",
        errors.len(),
        format_signed_us(errors[0]),
        format_signed_us(pick(1, 2)),
        format_signed_us(pick(95, 100)),
        format_signed_us(errors[last]),
        format_signed_us(mean),
    );
    println!("  Positive means the runner woke late. This is OS timer granularity plus");
    println!("  Tokio wheel resolution; it is the floor on claim timing precision.");
}

fn print_network_note() {
    println!();
    println!("Network stages (NOT measured here)");
    println!("{}", "-".repeat(102));
    println!("  cold DNS + TCP + TLS   : requires a live endpoint from the deployment host");
    println!("  warm pooled TTFB       : requires a live endpoint from the deployment host");
    println!();
    println!("  This harness measures only application-owned time. In production the");
    println!("  network dominates every figure above by orders of magnitude, which is why");
    println!("  preflight warms the connection pool: the point of the design is to move");
    println!("  DNS/TCP/TLS out of the T=0 path entirely, not to shave microseconds off");
    println!("  serialization. Run the live procedure in docs/latency-budget.md from the");
    println!("  VPS to fill these in.");
}

fn print_verdict(stages: &[Stage]) {
    println!();
    println!("Hot-path summary");
    println!("{}", "-".repeat(102));
    let hot_total: u128 = stages
        .iter()
        .filter(|stage| stage.phase == Phase::HotPath)
        .map(|stage| stage.percentile(95, 100))
        .sum();
    println!(
        "  Application work remaining at T=0 (p95, summed): {}",
        format_ns(hot_total)
    );
    println!("  Everything else is preparation and runs while the system is idle.");
}

// ---------------------------------------------------------------------------

/// Scratch database directory, removed on drop so a panic cannot leave a stray
/// SQLite file behind.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn create() -> Result<Self> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "shopee-hunter-bench-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating scratch dir {}", path.display()))?;
        Ok(Self { path })
    }

    fn sqlite_url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.join("bench.db").display())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("shopee-hunter latency harness");
    if cfg!(debug_assertions) {
        println!();
        println!("  !! DEBUG BUILD — these numbers are not meaningful.");
        println!(
            "  !! Re-run with: cargo run --release -p shopee-hunter-tools --bin benchmark_latency"
        );
    }
    println!();

    let candidate = parse_sample_candidate()?;
    let voucher = Voucher::from_candidate(&candidate, Utc::now());
    let plan = ClaimPlan::for_voucher(&voucher)
        .map_err(|err| anyhow::anyhow!("sample voucher must yield a claim plan: {err}"))?;

    let scratch = ScratchDir::create()?;
    let db = Database::connect(&scratch.sqlite_url(), 4)
        .await
        .context("connecting scratch SQLite database")?;

    let (insert_stage, unchanged_stage) = stage_storage_upsert(&db).await?;

    let stages = vec![
        stage_json_parse(),
        stage_normalize(&candidate),
        stage_claim_plan(&voucher),
        insert_stage,
        unchanged_stage,
        stage_client_construction(),
        stage_hot_path_handoff(&plan),
    ];

    print_table(&stages);
    print_scheduler_lag(&measure_scheduler_lag().await);
    print_network_note();
    print_verdict(&stages);

    db.close().await;
    println!();
    println!("Scratch database: {} (removed)", scratch.path().display());
    Ok(())
}
