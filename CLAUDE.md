# CLAUDE.md

## Purpose

This file is the operating guide for Claude Code when working in the `shopee-hunter` repository.

`shopee-hunter` is a personal-use, 24/7 Shopee Vietnam voucher monitoring and claiming assistant. The system is intended for a single buyer account and is designed to:

- discover voucher opportunities from approved/known sources;
- normalize and deduplicate voucher metadata;
- keep a Shopee authenticated session healthy;
- schedule time-sensitive actions accurately;
- attempt a controlled voucher-save/claim action when explicitly configured;
- notify the owner through Telegram or another notifier;
- run continuously on a VPS with strong observability and recovery behavior.

This repository is **not** intended to implement account farming, CAPTCHA bypass, stealth fingerprinting, IP rotation to defeat platform controls, multi-account abuse, automatic checkout, payment automation, or other mechanisms intended to circumvent platform protections.

---

## Product principles

When making implementation decisions, optimize for the following order:

1. **Correctness** — never claim that a voucher was saved unless Shopee's response is positively interpreted as success.
2. **Low latency** — minimize discovery delay, scheduler wake-up error, connection setup, and claim-path overhead; latency is a first-class product requirement.
3. **Account/session safety** — gain speed through architecture and warm state, not uncontrolled request volume or bypass behavior.
4. **Observability** — every important state transition and latency stage must be inspectable.
5. **Recoverability** — a crashed watcher must not corrupt scheduler state or lose known vouchers.
6. **Maintainability** — Shopee private endpoints and page structures can change; adapters must be easy to replace.
7. **Source isolation** — no individual discovery source should be able to take down the system.

---

## Project scope

### In scope

- Shopee Vietnam (`shopee.vn`) only.
- One primary buyer account per deployment initially.
- 24/7 server-side deployment.
- Authenticated session/cookie management.
- Browser-backed session refresh when necessary.
- HTTP-based fast path for supported Shopee requests.
- Voucher discovery from modular collectors.
- Voucher normalization and deduplication.
- Voucher ranking and filtering.
- Time-aware scheduling in `Asia/Ho_Chi_Minh`.
- Telegram notifications.
- Local persistence and production database support.
- Health checks, metrics, logging, and alerting.
- Replayable captured fixtures for parser tests.
- Safe manual intervention when authentication or verification is required.

### Explicitly out of scope

Do not implement the following unless the repository owner changes the project charter:

- CAPTCHA solving or bypass.
- Device-fingerprint spoofing intended to evade detection.
- Proxy rotation intended to bypass rate limits or bans.
- Multi-account voucher farming.
- Automatic order placement or checkout.
- Payment automation.
- Botting flash-sale inventory purchases.
- Credential theft or session extraction from other users.
- Techniques intended to defeat platform security controls.

If a task appears to require one of these, stop implementation and explain the conflict in the task notes.

---

## Reference repositories

The following repositories may exist locally under a `research/` or sibling directory. They are **references**, not source dependencies.

### `trongthaohub/Bot_Voucher`

Use it to study:

- voucher feed ingestion patterns;
- scheduling patterns;
- SQLite-based deduplication;
- Telegram notification flow;
- handling of voucher start times.

Do not blindly copy:

- hard-coded credentials;
- stale API schemas;
- database files;
- environment-specific configuration;
- untested polling intervals.

### `vinh781/shopee-voucher-tool`

Use it to study:

- voucher claim/save payload shapes;
- voucher metadata fields such as `promotion_id`, `signature`, and voucher code;
- response classifications around save/claim operations.

Treat all private Shopee endpoints discovered here as unstable. Wrap them behind adapters and fixture-based tests.

### `NgVB1408/shop-watcher`

Use it to study:

- watcher lifecycle;
- retry/backoff patterns;
- polling design;
- SQLite WAL usage;
- Telegram integration;
- long-running process reliability.

Do not copy private endpoint assumptions without verification.

---

## Target repository structure

The preferred repository layout is:

```text
shopee-hunter/
├── Cargo.toml                  # workspace manifest
├── Cargo.lock                  # committed for reproducible application builds
├── rust-toolchain.toml         # pinned stable toolchain/channel policy
├── crates/
│   ├── domain/                 # pure domain types and state machines
│   │   └── src/
│   ├── collectors/             # discovery adapters + supervisor
│   │   └── src/
│   ├── shopee-client/          # authenticated HTTP transport + classifiers
│   │   └── src/
│   ├── session/                # cookie jar, health, browser/CDP bridge
│   │   └── src/
│   ├── scheduler/              # durable + precision scheduling
│   │   └── src/
│   ├── claimer/                # claim policy + execution
│   │   └── src/
│   ├── ranking/                # scoring and eligibility rules
│   │   └── src/
│   ├── notifier/               # Telegram + formatting + outbox delivery
│   │   └── src/
│   ├── storage/                # SQLx repositories + migrations
│   │   ├── src/
│   │   └── migrations/
│   ├── observability/          # tracing, metrics, health state
│   │   └── src/
│   ├── app/                    # composition root and long-running binary
│   │   └── src/
│   │       ├── api.rs
│   │       ├── config.rs
│   │       └── main.rs
│   └── tools/                  # developer/owner utilities, still Rust
│       └── src/bin/
│           ├── login_session.rs
│           ├── inspect_fixture.rs
│           └── benchmark_latency.rs
├── tests/
│   ├── integration/
│   ├── fixtures/
│   └── contract/
├── benches/
├── research/
│   └── README.md
├── docker/
├── .env.example
├── docker-compose.yml
├── deny.toml
├── CLAUDE.md
├── AGENTS.md
├── ARCHITECTURE.md
└── ROADMAP.md
```

Do not create a monolithic `main.rs` containing collectors, database logic, session logic, and claim logic together. `crates/app` is a composition root, not a business-logic crate.

---

## Technical defaults

Unless an accepted architecture decision record says otherwise, use:

- stable Rust, pinned by `rust-toolchain.toml`; no Python runtime in the production project
- Tokio multi-thread runtime for all asynchronous services, timers, signals, and task supervision
- `reqwest` with a single long-lived pooled `Client` for the normal HTTP fast path; use `rustls` unless an integration proves another TLS backend is required
- `serde` / `serde_json` for boundary serialization and schema-tolerant parsing
- `config` or an equivalent typed configuration layer plus explicit validation at startup
- SQLx for SQLite/PostgreSQL, compile-time checked queries where practical, and SQLx migrations
- SQLite for local development and replay tests
- PostgreSQL for production
- `chromiumoxide` or another Rust-native CDP client only for browser-backed session bootstrap/refresh or diagnostic workflows; keep it outside the request hot path
- `tokio::time` for timers; durable schedule intent lives in the database, and final execution uses `tokio::time::Instant` / `sleep_until`
- `tracing` + `tracing-subscriber` for structured logs and spans
- `thiserror` for library/domain errors; `anyhow` only at executable/composition boundaries where typed recovery is no longer useful
- `rust_decimal` for money/percentage values that must not use binary floating point
- `uuid` for identifiers
- `chrono` + `chrono-tz` (or a single accepted equivalent chosen by ADR) for UTC persistence and Vietnam display timezone handling
- `cargo test`; `cargo nextest` may be added when the test suite benefits from it
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo deny` / `cargo audit` in CI for dependency/security policy
- Criterion or a similarly explicit benchmark harness for measured hot-path work
- Docker Compose for deployment

Avoid adding infrastructure such as Kafka, Kubernetes, or Redis until an explicit need exists.

### Performance-first rule

"Fast" means minimizing measured end-to-end latency without turning the bot into a request flood. Optimize in this order:

1. discover vouchers earlier;
2. keep the Shopee session healthy before the execution window;
3. keep DNS/TCP/TLS/HTTP connections warm and reusable;
4. avoid browser and database work on the final claim path;
5. minimize scheduler wake-up error with Tokio monotonic deadlines;
6. only then optimize serialization, allocations, copies, and CPU work shown by profiles/benchmarks.

Rust is the only production implementation language for this repository. Do not add Python helper services or scripts as shortcuts; developer utilities belong in the Rust `tools` crate or in portable shell where appropriate.

---

## Coding rules

### General

- Prefer small modules with explicit responsibilities.
- Keep Shopee-specific parsing outside generic domain code.
- Keep external source data in raw form for debugging, but never log secrets.
- Make public APIs strongly typed; avoid stringly typed state and unvalidated JSON leaking into domain crates.
- Prefer immutable domain values where practical.
- Avoid hidden global state.
- Use constructor-based dependency injection and traits at real external boundaries; avoid trait proliferation inside simple pure modules.
- Keep time access behind a clock abstraction in scheduling-sensitive code.

### Async and concurrency rules

- Tokio owns the async runtime. Do not introduce a second async executor.
- Do not execute blocking filesystem, browser-driver, compression, or CPU-heavy work directly on Tokio worker threads; use an isolated process, `spawn_blocking`, or a bounded dedicated thread when required.
- Put explicit connect/read/overall deadlines on external I/O.
- Bound concurrency with semaphores or bounded channels; never create unbounded task fan-out.
- Prefer structured task ownership with `JoinSet` / supervised task handles and a shared cancellation token.
- Every long-running worker must respond to cancellation and shut down gracefully.
- Avoid holding `Mutex`/`RwLock` guards across `.await` unless the lock type and critical section are deliberately designed for it.
- Prefer message passing or immutable snapshots for hot read paths.
- Reuse HTTP clients and connections; never construct a new client per request.
- The claim-time hot path must avoid avoidable allocation, DNS resolution, TLS setup, database round trips, and browser calls.

### Error handling

Never use a broad exception handler that silently discards failures.

Classify errors into at least:

- transient network error;
- upstream rate limit;
- authentication/session expired;
- malformed upstream payload;
- voucher no longer valid;
- voucher not yet active;
- voucher already saved;
- voucher exhausted;
- account not eligible;
- platform verification required;
- unknown upstream response.

Unknown responses must be stored with a redacted diagnostic payload and surfaced through an alert or metric.

---

## Domain model requirements

The canonical voucher model must not be shaped around a single source.

Recommended fields:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Voucher {
    pub id: uuid::Uuid,
    pub source: SourceId,
    pub source_key: String,
    pub code: Option<String>,
    pub promotion_id: Option<String>,
    pub signature: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub voucher_type: VoucherType,
    pub discount_type: Option<DiscountType>,
    pub discount_amount: Option<rust_decimal::Decimal>,
    pub discount_percent: Option<rust_decimal::Decimal>,
    pub max_discount: Option<rust_decimal::Decimal>,
    pub min_spend: Option<rust_decimal::Decimal>,
    pub start_at: Option<chrono::DateTime<chrono::Utc>>,
    pub end_at: Option<chrono::DateTime<chrono::Utc>>,
    pub scope: Option<VoucherScope>,
    pub payment_method: Option<String>,
    pub landing_url: Option<String>,
    pub first_seen_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    pub raw_hash: String,
}
```

Store times internally in UTC. Convert for display using `Asia/Ho_Chi_Minh`.

---

## Voucher identity and deduplication

Never deduplicate only by voucher code.

Preferred identity strategy:

1. use a stable external ID if the source provides one;
2. otherwise use `promotion_id` if trustworthy;
3. otherwise derive a fingerprint from a normalized set such as:
   - code;
   - start time;
   - end time;
   - scope;
   - discount properties;
   - source.

Maintain both:

- logical voucher identity;
- version hash of the latest raw payload.

This lets the system detect changed conditions without creating a duplicate logical voucher.

---

## Session handling rules

Shopee authenticated state is sensitive.

### Never

- commit cookies;
- commit browser profiles;
- print full `Cookie` headers;
- include cookies in exception traces;
- send cookies through Telegram;
- place cookies directly in Docker images;
- expose browser debugging ports publicly.

### Prefer

- encrypted or permission-restricted mounted storage;
- persistent browser profile stored outside the repository;
- cookie values redacted from logs;
- a dedicated session manager;
- explicit session health state.

Session states should include:

```text
UNKNOWN
HEALTHY
DEGRADED
EXPIRED
LOGIN_REQUIRED
VERIFICATION_REQUIRED
DISABLED
```

The claim worker must refuse to send claim requests when session state is `EXPIRED`, `LOGIN_REQUIRED`, `VERIFICATION_REQUIRED`, or `DISABLED`.

---

## Browser usage rules

Browser automation is a fallback and session-management mechanism, not the default transport for every request.

Use the Rust-native Chromium/CDP session adapter for:

- initial manual login bootstrap;
- session refresh when browser context is required;
- inspecting network calls during development;
- reproducing frontend-only flows;
- confirming behavior when HTTP responses become ambiguous.

Do not continuously render Chromium pages when a stable HTTP collector exists. Browser automation must not sit on the hot claim path.

Do not add CAPTCHA-solving, stealth, fingerprint-spoofing, or control-bypass packages.

---

## Collector contract

Every collector must implement a common contract similar to:

```rust
#[async_trait::async_trait]
pub trait VoucherCollector: Send + Sync {
    fn name(&self) -> &str;
    async fn collect(&self, context: &CollectionContext) -> Result<CollectionResult, CollectorError>;
}
```

A `CollectionResult` should include:

- normalized candidate vouchers;
- raw-source metadata;
- fetch timestamp;
- source latency if known;
- partial failure information;
- rate-limit hints if available.

Collectors must not write directly to Telegram or claim vouchers.

---

## Scheduling rules

Scheduling must be deterministic and restart-safe.

Persist scheduled intent in the database before execution.

At minimum track:

- voucher ID;
- planned action;
- planned timestamp;
- scheduler version;
- execution status;
- attempt count;
- last result;
- timestamps.

When the process restarts, rebuild future jobs from persistent state.

Do not rely solely on in-memory Tokio tasks/timers. Database state is authoritative; Tokio timers are execution mechanisms.

Use a monotonic clock for relative waits and wall-clock UTC for persisted timestamps.

---

## Claim policy

The claimer is controlled by policy, not direct collector events.

A voucher may be claimed only if all required policy checks pass, for example:

- account session is healthy;
- voucher is within configured time window;
- voucher has enough identifying information;
- voucher is not already successfully claimed;
- voucher is not explicitly excluded;
- retry budget has not been exhausted;
- no verification state is active.

Retries must be response-aware.

Never implement an unbounded loop such as:

```rust
loop {
    claim().await?;
}
```

Use a small configured retry budget and backoff appropriate to the response class.

---

## Notification requirements

Telegram notifications should distinguish:

- discovery;
- upcoming voucher;
- claim success;
- claim failure;
- exhausted voucher;
- account ineligible;
- session expired;
- verification required;
- source degraded;
- service unhealthy.

Do not include raw credentials, full cookies, sensitive headers, or full browser storage dumps.

Notifications must be idempotent where possible.

---

## Logging and observability

Every event should include a correlation-friendly set of fields:

```text
event
service
voucher_id
source
source_key
attempt_id
session_state
latency_ms
result_class
timestamp
```

Do not log full raw payloads at INFO level.

Recommended metrics:

- collector requests total;
- collector failures total;
- discovery latency;
- newly discovered vouchers;
- duplicate vouchers;
- scheduled jobs;
- claim attempts;
- claim successes;
- claim failures by class;
- session health transitions;
- request latency to Shopee;
- HTTP status counts;
- rate-limit events;
- notifier failures;
- worker loop lag;
- database errors.

---

## Tests required for every integration

### Unit tests

Required for:

- parsers;
- deduplication;
- scoring;
- scheduler time calculations;
- response classification;
- retry policy;
- formatting.

### Fixture tests

When integrating an unstable/private response:

1. save a redacted JSON fixture;
2. add parser/classifier tests against it;
3. document fixture source/date in comments or fixture metadata;
4. never include cookies or access tokens.

### Integration tests

Use local fakes or recorded redacted fixtures when possible.

Do not make normal CI dependent on a live Shopee account.

### Live smoke tests

Live tests must be opt-in and clearly marked. They must not run in normal CI.

---

## Database rules

- All schema changes require migrations once PostgreSQL is introduced.
- Do not rely on ORM auto-create in production.
- Add unique constraints matching logical identity decisions.
- Store raw external payloads separately from normalized data if payload size becomes large.
- Use an outbox pattern if notification delivery consistency becomes important.
- Keep claim attempts append-only where practical for auditability.

---

## Configuration rules

Configuration must come from environment variables or mounted secret files.

Use a typed settings object.

Expected configuration categories:

```text
APP_ENV
LOG_LEVEL
DATABASE_URL
SHOPEE_PROFILE_PATH
SHOPEE_BASE_URL
TELEGRAM_BOT_TOKEN
TELEGRAM_CHAT_ID
COLLECTOR_* settings
CLAIM_* settings
SCHEDULER_* settings
HEALTHCHECK_* settings
```

Provide `.env.example` containing names and safe examples only.

---

## Security requirements

Before merging code that touches session data, confirm:

- no secret is written to logs;
- files containing session material are excluded by `.gitignore`;
- Docker volume permissions are documented;
- diagnostics redact sensitive headers;
- admin HTTP endpoints are not publicly exposed without authentication;
- browser debugging interfaces bind to localhost only;
- backups do not accidentally include unencrypted session secrets unless explicitly intended.

---

## Git and change discipline

Claude should make focused changes.

For each task:

1. inspect relevant files;
2. explain the intended change briefly;
3. implement the smallest coherent patch;
4. add/update tests;
5. run formatting and tests;
6. summarize changed behavior and remaining risks.

Avoid mixing unrelated refactors into feature work.

Do not rewrite architecture documents simply to match a temporary implementation shortcut. If the architecture must change, update the architecture explicitly and explain why.

---

## Commands

Preferred commands once the project is initialized:

```bash
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

Development service:

```bash
cargo run -p shopee-hunter-app
```

Docker:

```bash
docker compose up -d
docker compose logs -f watcher
```

Session bootstrap:

```bash
cargo run -p shopee-hunter-tools --bin login_session
```

Never invent commands that are not present in `Cargo.toml`, `Makefile`/`justfile`, or repository scripts. If the repository differs from this guide, inspect actual configuration first.

---

## Definition of done

A task is not complete merely because the code runs once.

A feature is done when:

- implementation matches the domain boundary;
- failure behavior is defined;
- tests cover normal and important failure paths;
- logs/metrics are sufficient to diagnose it;
- secrets are handled safely;
- documentation is updated if behavior or architecture changed;
- restart behavior has been considered;
- no new uncontrolled polling or retry loop was introduced;
- the feature can be disabled through configuration when appropriate.

---

## Decision priority when uncertain

When requirements are ambiguous, prefer:

1. passive observation over active mutation;
2. fewer requests over more requests;
3. explicit user intervention over bypassing verification;
4. persistent state over ephemeral state;
5. adapters over hard-coded upstream assumptions;
6. recorded fixtures over undocumented reverse-engineered behavior;
7. clear failure states over silent retries.

