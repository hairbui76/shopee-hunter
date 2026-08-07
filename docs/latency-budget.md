# Latency budget and regression procedure

Phase 24 deliverable. Defines what the application is allowed to cost at each
stage, records the measured baseline, and gives a repeatable way to detect a
regression.

## The rule this document exists to protect

Claim latency is dominated by the network, not by CPU. The architecture's
response is not to make the code fast — it is to make sure that **nothing but a
socket write remains at `T=0`** (ARCHITECTURE.md §13, §25). Everything else —
parsing, hashing, persistence, plan construction, DNS/TCP/TLS — happens minutes
earlier, while the process is idle.

So the budget below has two very different halves:

* **Preparation and startup**: generous budgets. Being 3× slower here costs
  nothing, because the work happens off the critical path. These figures exist
  to catch accidental pathologies (an O(n²) normalizer, a missing index), not to
  be optimised for their own sake.
* **`T=0`**: a near-zero budget. Anything that appears here is a design defect.
  A new database read, a JSON re-encode, or a lazily built request on this path
  is a bug to be moved into preparation — not a micro-optimisation opportunity.

## Tools

| Tool | Measures | Command |
|---|---|---|
| `benchmark_latency` | Whole stages in a realistic caller, including I/O | `cargo run --release -p shopee-hunter-tools --bin benchmark_latency` |
| `hot_path` Criterion bench | Isolated pure functions, with statistical comparison | `cargo bench -p shopee-hunter-tools --bench hot_path` |

Always build `--release`. Debug figures are roughly an order of magnitude worse
and mean nothing; the harness prints a warning if it detects a debug build.

### Why the two tools disagree

Criterion reports `json_parse` at ~2.8 µs while the harness reports ~6.6 µs for
the same work. This is expected and not a bug in either:

* Criterion runs batched iterations and amortises its timing overhead, isolating
  the function itself.
* The harness times each call individually (`Instant::now()` on both sides, a
  `black_box` barrier forcing the result through memory, plus the drop of the
  returned value), which is closer to what a caller actually pays.

**Never compare a Criterion number to a harness number.** Compare each tool
against its own previous run on the same machine.

## Measured baseline

Captured on the development container in release mode. **These numbers are
machine-specific.** Re-baseline on the production VPS before using them as
production thresholds — a smaller cloud instance will be slower, and SQLite
figures in particular depend heavily on the disk's fsync behaviour.

### Application stages (`benchmark_latency`)

| Stage | Phase | p50 | p95 | Budget (p95) |
|---|---|---:|---:|---:|
| json parse (collector payload) | prepare | 6.63 µs | 6.77 µs | 50 µs |
| normalize + identity + hashes | prepare | 10.03 µs | 10.12 µs | 50 µs |
| claim plan build (incl. encode) | prepare | 902 ns | 959 ns | 10 µs |
| storage upsert (new voucher) | prepare | 7.28 ms | 7.63 ms | 25 ms |
| storage upsert (already known) | prepare | 6.98 ms | 7.19 ms | 25 ms |
| shopee client construction | startup | 12.01 µs | 16.37 µs | 1 ms |
| **T=0 request body handoff** | **T=0** | **68 ns** | **68 ns** | **10 µs** |

### Pure functions (Criterion)

| Benchmark | Time | Budget |
|---|---:|---:|
| `json_parse/voucher_candidate` | 2.78 µs | 10 µs |
| `identity/compute_identity/promotion_id` | 68.9 ns | 500 ns |
| `identity/compute_identity/fingerprint` | 2.00 µs | 8 µs |
| `hashing/version_hash` | 2.22 µs | 8 µs |
| `hashing/raw_hash` | 1.41 µs | 8 µs |
| `pipeline/parse_and_normalize` | 6.70 µs | 25 µs |

Budgets are set at roughly 3× the observed figure. They are tripwires for
structural regressions, not performance targets — a change that moves a stage
from 6 µs to 9 µs is uninteresting; one that moves it to 600 µs is a bug.

### Scheduler wake accuracy

| Metric | Value |
|---|---:|
| min error | +200 µs |
| p50 error | +1.20 ms |
| p95 error | +1.32 ms |
| max error | +1.33 ms |
| budget (p95) | +5 ms |

Positive means the runner woke **late**; it never woke early.

**This is the single most important number in this document.** At ~1.2 ms of
systematic lateness, the scheduler — not serialization, not parsing — sets the
floor on claim timing precision. It is ~17 000× the 68 ns of application work
left at `T=0`, which is the clearest possible evidence that further CPU
optimisation of the claim path would be wasted effort.

The lag is OS timer granularity plus Tokio's timer-wheel resolution (~1 ms
tick), not application overhead. If claim timing ever proves to be the limiting
factor on outcomes, the lever is to aim the deadline slightly early and busy-wait
the final sub-millisecond — a change that must be justified by evidence that
timing actually costs claims, since it trades CPU burn for precision.

### Network stages — not measured here

DNS, TCP, TLS and TTFB are **not** exercised by either tool. They require a live
Shopee endpoint from the deployment host, and CLAUDE.md keeps live checks opt-in
and out of ordinary CI.

Expect the network to dominate every figure in this document by three to five
orders of magnitude. That asymmetry is the entire justification for the
prepare/execute split and for `warm_connection()` in preflight: moving a TLS
handshake off the hot path is worth more than every microsecond of CPU work
listed above, combined.

## Live network procedure (run from the VPS, opt-in)

Not automated. Requires a healthy session and deliberate operator action.

1. Confirm host clock sync (`timedatectl status` / `chronyc tracking`). A skewed
   clock invalidates every scheduler measurement.
2. From the deployment host, measure a **cold** request: fresh process, no pool.
   Record DNS, connect, TLS and TTFB separately.
3. Measure a **warm** request: same process, pooled connection, immediately
   after `warm_connection()`. Record TTFB.
4. The difference is what preflight warming buys. If it is not large, the
   warming strategy needs re-examination — that would be a genuine finding.
5. Record both in this document with the date and the host/region.

Use the session probe endpoint, never `save_voucher`: measurement must not
mutate account state.

## Regression procedure

Run before merging anything touching the domain, storage, scheduler, or client
hot paths.

1. **Baseline.** On the target machine, on the base commit:
   ```bash
   cargo bench -p shopee-hunter-tools --bench hot_path
   ```
   Criterion stores results in `target/criterion/`. Do not delete it between
   runs — the comparison depends on it.
2. **Candidate.** Apply the change and re-run the same command. Criterion prints
   a change percentage and a significance verdict per benchmark.
3. **Interpret.** Treat `Performance has regressed` as actionable only when the
   change exceeds **20%** *and* Criterion calls it statistically significant.
   Anything smaller is noise on a shared machine.
4. **Stages.** Re-run the harness on both commits and compare p50/p95 per stage:
   ```bash
   cargo run --release -p shopee-hunter-tools --bin benchmark_latency
   ```
   Use p50 for the signal and p95 for tail behaviour. Ignore max: a single
   scheduler preemption dominates it.
5. **Hard failure.** Any of these blocks the merge regardless of percentages:
   * a new stage appears in the `T=0` phase;
   * the `T=0` summary rises above its budget;
   * scheduler p95 wake error exceeds 5 ms;
   * a storage stage exceeds its budget, which usually means a lost index or an
     accidental extra round trip.

### Measurement hygiene

* Same machine, and ideally the same power/thermal state, for baseline and
  candidate. Cross-machine comparisons are meaningless.
* Close other heavy processes; both tools are sensitive to CPU contention.
* The harness writes its SQLite scratch database under the OS temp directory and
  removes it on exit, including on panic. Storage figures reflect that
  filesystem — a container overlay and a VPS SSD will differ substantially.
* Re-baseline after a toolchain bump, a dependency bump touching `serde_json`,
  `sha2`, `sqlx`, or `rustls`, or any change to the release profile.

## Release profile

`Cargo.toml` keeps Cargo's defaults plus `debug = 1` for symbolised profiles.
LTO and `codegen-units` tuning are deliberately **not** enabled: per
ROADMAP Phase 24 they should only be adopted with benchmark evidence of
repeatable benefit, and the measurements above show application CPU is nowhere
near being the limiting factor. Revisit only if a profile shows otherwise.
