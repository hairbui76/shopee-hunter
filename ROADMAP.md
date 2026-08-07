# ROADMAP.md

## Purpose

This roadmap defines the **full development path** for `shopee-hunter`, from initial research through a mature, continuously operating personal Shopee Vietnam voucher-hunting system.

This is not an MVP-only roadmap. It includes the complete intended product lifecycle:

- research and reverse-engineering discipline;
- production-grade repository foundations;
- voucher discovery;
- normalization and persistence;
- authenticated session management;
- scheduler precision;
- controlled claim execution;
- Telegram UX;
- reliability and recovery;
- security hardening;
- observability;
- deployment;
- ranking and decision support;
- operational tooling;
- long-term maintenance and adaptation to upstream change.

Phases are ordered by dependency. Some later phases can overlap, but a phase should not be considered complete until its exit criteria are satisfied.

---

# Phase 0 — Project charter and research workspace

## Objective

Create a clean engineering starting point and separate research/reference code from production code.

## Deliverables

### Repository initialization

Create:

```text
shopee-hunter/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── crates/
│   ├── domain/
│   ├── collectors/
│   ├── shopee-client/
│   ├── session/
│   ├── scheduler/
│   ├── claimer/
│   ├── notifier/
│   ├── storage/
│   ├── observability/
│   ├── app/
│   └── tools/
├── tests/
├── benches/
├── research/
├── docs/
├── CLAUDE.md
├── AGENTS.md
├── ARCHITECTURE.md
├── ROADMAP.md
├── .gitignore
└── .env.example
```

### Reference repository workspace

Keep the three main references outside production imports:

```text
research/reference-repos/
├── Bot_Voucher/
├── shopee-voucher-tool/
└── shop-watcher/
```

Document:

- repository URL;
- commit hash inspected;
- useful files/functions;
- observed assumptions;
- anything obviously stale or unsafe;
- endpoint names seen;
- fields needed by later research.

### Project charter

Write a concise statement defining:

- Shopee Vietnam only;
- one personal buyer account initially;
- 24/7 VPS deployment;
- Rust-only production implementation;
- voucher monitoring and controlled voucher saving;
- no checkout/payment automation;
- no CAPTCHA/security-control bypass;
- no account farming.

## Tasks

- [ ] Initialize Git repository.
- [ ] Initialize a Cargo workspace and pin the stable Rust toolchain policy in `rust-toolchain.toml`.
- [ ] Commit `Cargo.lock` because this repository builds an application.
- [ ] Add `.gitignore` for `.env`, cookies, profiles, DB files, logs, test secrets.
- [ ] Add pre-commit or equivalent formatting workflow.
- [ ] Add a research README.
- [ ] Record reference repo commit hashes.
- [ ] Create `docs/adr/`.
- [ ] Add ADR 0001 for modular-monolith direction.

## Exit criteria

- Production repository imports no code from reference repos.
- Sensitive files are ignored.
- The project can be installed in a clean environment.
- Scope/non-goals are explicitly documented.

---

# Phase 1 — Engineering foundation

## Objective

Build a production-quality application skeleton before integrating unstable Shopee behavior.

## Deliverables

### Dependency and quality tooling

Use or establish:

- Cargo workspace + committed `Cargo.lock`;
- Tokio as the sole async runtime;
- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --workspace --all-features` and optional `cargo nextest` when useful;
- `tracing` / `tracing-subscriber` structured logging;
- typed configuration deserialization plus explicit startup validation;
- `cargo deny` / `cargo audit` security policy;
- Criterion or an equivalent benchmark harness for measured hot paths.

### Application lifecycle

Implement:

```text
shopee-hunter-app
├── typed settings load
├── logging setup
├── service initialization
├── worker startup
├── signal handling
└── graceful shutdown
```

### Configuration

Typed settings sections:

```text
AppSettings
DatabaseSettings
ShopeeSettings
SessionSettings
CollectorSettings
SchedulerSettings
ClaimSettings
TelegramSettings
ObservabilitySettings
```

### Rust runtime and hot-path foundation

Establish the runtime/performance shape before unstable integrations arrive:

- one Tokio multi-thread runtime;
- a shared cancellation primitive for coordinated shutdown;
- `JoinSet` or an explicit task supervisor;
- bounded Tokio channels for queued work;
- one process-long shared `reqwest::Client`;
- bounded SQLx pools;
- monotonic clock abstraction around `tokio::time::Instant` for timing-sensitive tests;
- a release-mode latency benchmark binary that separates application overhead from DNS/connect/TLS/TTFB.

Do not introduce direct Hyper/socket code yet. The baseline must first prove where latency comes from.

### Base worker abstraction

Provide common behavior for:

- startup;
- heartbeat;
- cancellation;
- top-level failure isolation;
- controlled retry delay.

## Tasks

- [ ] Add Cargo workspace crate structure and dependency boundaries.
- [ ] Add typed configuration.
- [ ] Add structured logging.
- [ ] Add redaction helpers.
- [ ] Add graceful shutdown handling.
- [ ] Add common worker base/supervisor.
- [ ] Add unit test setup.
- [ ] Add CI for lint/type/test.
- [ ] Add Dockerfile build.

## Exit criteria

- Empty service starts and shuts down cleanly.
- CI passes.
- A failed dummy worker is visible in logs/health.
- Secrets are redacted by shared logging helpers.

---

# Phase 2 — Canonical voucher domain

## Objective

Define stable internal concepts before integrating external schemas.

## Deliverables

### Domain entities

Implement source-independent types:

```text
Voucher
VoucherCandidate
VoucherObservation
VoucherVersion
VoucherType
VoucherScope
DiscountType
VoucherStatus
```

### Identity engine

Implement deterministic voucher identity.

Identity priority:

1. source stable ID;
2. trusted promotion ID;
3. canonical composite fingerprint.

### Normalized hashing

Generate:

- identity fingerprint;
- normalized version hash;
- optional raw payload hash.

### Validation

Validate:

- timezone awareness;
- discount values;
- minimum spend;
- start/end ordering;
- malformed code/identifier fields;
- source metadata.

## Tasks

- [ ] Define enums/value objects.
- [ ] Define canonical strongly typed Rust `Voucher` struct and supporting enums/newtypes.
- [ ] Implement identity function.
- [ ] Implement normalized hash.
- [ ] Add edge-case tests.
- [ ] Document identity rules.

## Exit criteria

- The same logical voucher deduplicates deterministically.
- Meaningful condition changes produce a new version hash.
- Domain layer has no dependency on Shopee HTTP/browser/database libraries.

---

# Phase 3 — Persistence layer

## Objective

Make all important state durable and restart-safe.

## Deliverables

### Development storage

Support SQLite for local work.

### Production storage

Support PostgreSQL using the same repository interfaces.

### Schema

Initial tables:

```text
vouchers
voucher_observations
voucher_versions
collector_runs
schedule_jobs
claim_attempts
service_health_events
```

Later add:

```text
notification_outbox
user_rules
source_configs
```

### Repository layer

Implement repositories such as:

```text
VoucherRepository
ObservationRepository
ScheduleRepository
ClaimRepository
HealthRepository
```

### Migrations

Use SQLx migrations from the beginning of production DB use. Keep migration files in the storage crate and validate forward application in CI.

## Tasks

- [ ] SQLx storage setup with explicit SQLite and PostgreSQL feature selection.
- [ ] SQLite local config.
- [ ] PostgreSQL config.
- [ ] SQLx migration directory and migration runner initialization.
- [ ] Initial schema migration.
- [ ] Unique constraints for identity.
- [ ] Transaction tests.
- [ ] Restart persistence tests.

## Exit criteria

- Voucher and observation state survives restart.
- Duplicate voucher ingestion is transaction-safe.
- Schema can be created from migrations alone.
- PostgreSQL is validated in integration tests.

---

# Phase 4 — Collector framework

## Objective

Create a modular source ingestion system independent of any single feed.

## Deliverables

### Collector protocol

```rust
#[async_trait::async_trait]
pub trait VoucherCollector: Send + Sync {
    fn name(&self) -> &str;
    async fn collect(&self, context: &CollectionContext) -> Result<CollectionResult, CollectorError>;
}
```

### Collector supervisor

Responsibilities:

- schedule collectors independently;
- enforce per-source timeout;
- per-source backoff;
- track source health;
- isolate failures;
- record collector runs.

### Collection pipeline

```text
fetch
→ validate
→ parse
→ normalize
→ identity
→ upsert
→ domain events
```

### Replay collector

Read test fixtures and feed them through the real normalization pipeline.

## Tasks

- [ ] Implement collector contract.
- [ ] Implement registry.
- [ ] Implement supervisor.
- [ ] Implement source health state.
- [ ] Implement replay collector.
- [ ] Add collector metrics.
- [ ] Add failure isolation tests.

## Exit criteria

- Two fake collectors can run at different intervals.
- One failed source does not interrupt another.
- Replay fixtures create real voucher records.

---

# Phase 5 — Reference-repo and Shopee behavior research

## Objective

Systematically understand the voucher discovery and voucher-save behavior needed by later integrations.

## Deliverables

### Research notes for each reference repo

For every useful file/function, document:

- behavior;
- request path;
- required inputs;
- response examples;
- session requirements;
- signs of outdated assumptions;
- whether behavior is discovery, metadata enrichment, or mutation.

### Sanitized fixtures

Capture redacted examples of relevant voucher data and response structures.

Fixture metadata should include:

```text
source
capture_date
purpose
parser_version
redactions_applied
```

### Request field inventory

Maintain a document/table of observed fields such as:

```text
voucher code
promotion ID
signature
start/end time
minimum spend
discount amount/percentage
scope
status/usage indicators
```

Do not assume all fields are always available.

## Tasks

- [ ] Inspect `Bot_Voucher` source flow.
- [ ] Inspect `shopee-voucher-tool` claim flow.
- [ ] Inspect `shop-watcher` reliability patterns.
- [ ] Record relevant request/response examples.
- [ ] Sanitize all fixtures.
- [ ] Add contract tests around any adopted schema.

## Exit criteria

- The production implementation can be built from documented behavior rather than copy-pasted scripts.
- No secrets exist in fixtures.
- Known unstable assumptions are explicitly listed.

---

# Phase 6 — First real voucher discovery source

## Objective

Integrate the first useful real-world source through the collector framework.

## Deliverables

### Source adapter

Implement one source that reliably provides Shopee Vietnam voucher candidates.

The exact source may be a Shopee resource or an external voucher feed, but it must be isolated behind the collector interface.

### Parser

- Serde DTO parsing plus explicit boundary/domain validation;
- canonical normalization;
- malformed-item handling;
- parser versioning.

### Source-specific health

Track:

- last success;
- last failure;
- fetch latency;
- result count;
- parse errors;
- rate-limit state.

## Tasks

- [ ] Implement source client.
- [ ] Implement parser.
- [ ] Add fixtures.
- [ ] Add integration tests with fake server.
- [ ] Add adaptive polling configuration.
- [ ] Add collector health reporting.

## Exit criteria

- New vouchers reach the canonical database.
- Duplicates are handled correctly.
- Schema changes result in a degraded source, not a crashed service.

---

# Phase 7 — Multi-source discovery and source confidence

## Objective

Increase coverage while preventing inconsistent sources from corrupting canonical state.

## Deliverables

### Additional collectors

Add sources one at a time.

Examples:

- configured Shopee voucher/campaign resources;
- external feed(s);
- manual imported vouchers;
- local curated source.

### Source confidence model

Each field can have provenance.

Potential rules:

- prefer Shopee-derived timing over community timing;
- prefer a source with a stable external ID for identity;
- do not overwrite a known precise start time with a lower-confidence approximate time;
- retain contradictory observations for debugging.

### Conflict resolution

Implement deterministic merge rules.

## Tasks

- [ ] Add second source.
- [ ] Add provenance metadata.
- [ ] Add merge policy.
- [ ] Add conflicting-source tests.
- [ ] Add source enable/disable config.

## Exit criteria

- Same voucher observed from two sources produces one canonical voucher.
- Conflicting data is resolved predictably and auditably.
- Individual collectors remain independently disableable.

---

# Phase 8 — Telegram notification system

## Objective

Make discoveries and operational state visible without logging into the VPS.

## Deliverables

### Telegram notifier

Message categories:

```text
NEW_VOUCHER
VOUCHER_UPDATED
UPCOMING
CLAIM_SUCCESS
CLAIM_FAILURE
SESSION_EXPIRED
VERIFICATION_REQUIRED
SOURCE_DEGRADED
SERVICE_UNHEALTHY
```

### Message formatting

Voucher message should show only relevant fields, for example:

```text
voucher type
code if present
discount
minimum spend
maximum discount
start/end
source confidence
claim readiness
```

### Notification idempotency

Avoid duplicate messages for the same event/version.

### Outbox preparation

Design notifier so a DB outbox can be inserted later without rewriting business logic.

## Tasks

- [ ] Telegram adapter.
- [ ] Formatting module.
- [ ] Notification event model.
- [ ] Idempotency key.
- [ ] Retry/backoff.
- [ ] Redaction tests.

## Exit criteria

- Discovery messages are reliable and non-duplicative.
- Operational errors produce actionable notifications.
- No secret/session material appears in messages.

---

# Phase 9 — Shopee session bootstrap

## Objective

Create a secure owner-controlled authenticated session lifecycle.

## Deliverables

### Persistent Chromium/CDP session context

Script:

```text
`cargo run -p shopee-hunter-tools --bin login_session`
```

Responsibilities:

- open persistent profile;
- allow manual login;
- close cleanly;
- never print cookies.

### Profile storage

Document deployment location and permissions.

### Session manager skeleton

States:

```text
UNKNOWN
HEALTHY
DEGRADED
EXPIRED
LOGIN_REQUIRED
VERIFICATION_REQUIRED
DISABLED
```

## Tasks

- [ ] Add a Rust-native Chromium/CDP adapter behind an optional/session-only crate boundary (for example `chromiumoxide`), with no browser dependency in the claim fast path.
- [ ] Add persistent profile config.
- [ ] Add manual bootstrap script.
- [ ] Add filesystem permission documentation.
- [ ] Add session state model.
- [ ] Add logging redaction around browser/session code.

## Exit criteria

- Owner can bootstrap a Shopee session without placing cookies in source files.
- Profile persists across process restart.
- Profile directory is ignored by Git and mounted securely.

---

# Phase 10 — Session health and HTTP session bridge

## Objective

Use the persistent login safely in long-running HTTP operations and detect session failure early.

## Deliverables

### Session health check

Implement a low-impact authenticated health request or equivalent session validation mechanism.

Classify:

```text
healthy
expired
login required
verification required
transient failure
unknown
```

### HTTP cookie/session bridge

Provide session credentials to a single owned HTTP client without global state.

### Claim pause gate

Claim service must be disabled whenever session is not healthy.

### Session refresh workflow

If the browser session can recover authentication without bypassing verification, synchronize updated state. Otherwise notify owner for manual action.

## Tasks

- [ ] Implement health probe abstraction.
- [ ] Implement Shopee auth adapter.
- [ ] Implement session state transitions.
- [ ] Implement HTTP cookie jar synchronization.
- [ ] Add state-transition metrics.
- [ ] Add integration tests with fake responses.

## Exit criteria

- Claim path can prove session health before mutating requests.
- Session expiry does not trigger repeated failed claims.
- Manual re-login can restore the service without rebuilding the deployment.

---

# Phase 11 — Shopee client and response-classification layer

## Objective

Centralize all Shopee transport assumptions in one anti-corruption layer.

## Deliverables

### Shopee HTTP client

Capabilities:

- connection pooling;
- timeouts;
- safe headers;
- session application;
- latency measurement;
- error normalization.

### Endpoint registry

All Shopee paths/config in one module.

### Response classifiers

At minimum:

```text
SUCCESS
ALREADY_SAVED
NOT_ACTIVE
EXPIRED
EXHAUSTED
INELIGIBLE
INVALID_VOUCHER
SESSION_EXPIRED
VERIFICATION_REQUIRED
RATE_LIMITED
TRANSIENT_FAILURE
UNKNOWN_RESPONSE
```

### Fixture suite

Every known response class should have a sanitized fixture.

## Tasks

- [ ] HTTP client.
- [ ] Error hierarchy.
- [ ] Response schema models.
- [ ] Response classifier.
- [ ] Redacted diagnostics.
- [ ] Contract tests.

## Exit criteria

- No claim code parses raw Shopee JSON directly.
- Unknown responses are explicit and observable.
- Client can be completely replaced without changing domain policy.

---

# Phase 12 — Durable scheduler

## Objective

Create restart-safe scheduling for voucher activation times.

## Deliverables

### Schedule entity/repository

Persist:

- action type;
- target timestamp;
- preflight timestamp;
- state;
- attempt count;
- last result.

### Scheduler service

Responsibilities:

- create/update/cancel jobs;
- reconstruct jobs after restart;
- detect stale/missed jobs;
- avoid duplicate execution.

### Fake clock

Provide deterministic tests.

## Tasks

- [ ] Schedule DB model.
- [ ] Scheduler service.
- [ ] Startup reconstruction.
- [ ] Duplicate prevention.
- [ ] Stale job policy.
- [ ] Timing tests.

## Exit criteria

- Restarting the service does not lose future voucher actions.
- Duplicate workers cannot execute the same job accidentally under normal deployment assumptions.
- Scheduler lag is measured.

---

# Phase 13 — Precision execution window

## Objective

Improve timing around voucher activation without introducing uncontrolled request spam.

## Deliverables

### Preflight window

Configurable events such as:

```text
T-10m  metadata check
T-1m   session health check
T-10s  client/precondition check
T-2s   enter Tokio precision task with prebuilt claim plan
T=0    primary action
```

Exact timings are configuration values, not constants.

### Monotonic target wait

Convert target wall-clock time into a `tokio::time::Instant` deadline during the final execution window and wait with `sleep_until`. Recompute the target if a material wall-clock correction is detected before entering the precision window.

### Clock quality check

Monitor or document host NTP synchronization.

### Scheduler lag metrics

Measure:

```text
actual_execution_time - planned_execution_time
```

## Tasks

- [ ] Preflight job implementation.
- [ ] Precision runner.
- [ ] Monotonic wait abstraction.
- [ ] Clock-skew operational check.
- [ ] Timing benchmark script.

## Exit criteria

- Timing behavior is reproducible in tests.
- Service can quantify its own execution lag.
- No high-frequency polling loop is required solely to wait for activation time.

---

# Phase 14 — Claim policy engine

## Objective

Make the decision to claim explicit, testable, and configurable.

## Deliverables

### Policy inputs

- voucher state;
- source confidence;
- session state;
- known start/end time;
- claim identifiers;
- prior attempts;
- user inclusion/exclusion rules;
- feature flags.

### Policy output

```text
ALLOW
DENY
DEFER
MANUAL_REVIEW
```

with structured reasons.

### Rules

Examples:

- no auto-claim if session unhealthy;
- no auto-claim with missing required identifiers;
- no retry after terminal result;
- configurable minimum voucher value;
- configurable excluded voucher types;
- configurable trusted sources.

## Tasks

- [ ] Policy interface.
- [ ] Core rules.
- [ ] Explainable decision output.
- [ ] Unit test matrix.
- [ ] Config binding.

## Exit criteria

- Every automatic claim has an auditable policy decision.
- Policy can be changed without editing the Shopee client.

---

# Phase 15 — Controlled voucher-save/claim execution

## Objective

Implement the actual account-mutating voucher-save operation through the Shopee client.

## Deliverables

### Claim service

Flow:

```text
receive scheduled action
→ load latest voucher state
→ load claim history
→ evaluate policy
→ verify session
→ create attempt record
→ send request
→ classify result
→ persist result
→ decide retry/terminal
→ emit notification event
```

### Bounded retry policy

Retry only response classes that justify it.

### Idempotency

Prevent duplicate simultaneous claims for the same voucher/account.

### Audit history

Every attempt persists:

- attempt time;
- response class;
- latency;
- retry index;
- diagnostic code.

## Tasks

- [ ] Claim service.
- [ ] Claim lock/idempotency mechanism.
- [ ] Retry policy.
- [ ] Attempt persistence.
- [ ] Success/failure notifications.
- [ ] Live smoke procedure documented but not automated in CI.

## Exit criteria

- One controlled claim can be executed and classified.
- Already-saved state is treated correctly.
- Session expiry pauses further claims.
- Unknown responses do not cause infinite retries.

---

# Phase 16 — Notification outbox and event-driven internal flow

## Objective

Remove fragile direct notifier calls and make important notifications durable.

## Deliverables

### Domain/application event model

Events for voucher/session/claim changes.

### Outbox table

Persist pending notifications atomically with important state changes.

### Notification worker

- fetch pending events;
- send;
- mark delivered;
- retry separately;
- dead-letter after configured threshold.

## Tasks

- [ ] Outbox schema.
- [ ] Event serialization.
- [ ] Worker.
- [ ] Retry/dead-letter policy.
- [ ] Cleanup/retention.

## Exit criteria

- A process crash after a successful claim cannot silently lose the success notification.
- Telegram outages do not block collector or claimer transactions.

---

# Phase 17 — Voucher ranking and personal usefulness scoring

## Objective

Prioritize vouchers that are likely to save the owner meaningful money.

## Deliverables

### Base scoring model

Potential components:

```text
absolute discount
percentage discount
minimum spend efficiency
maximum discount cap
voucher type
activation proximity
duration
source confidence
restriction complexity
claim readiness
```

### User rules

Configurable preferences:

- minimum discount;
- maximum required spend;
- preferred voucher types;
- excluded payment methods;
- excluded shops/categories;
- notification threshold;
- auto-claim threshold.

### Explanation

Every score should be explainable.

Example:

```text
score 82
+30 high max discount
+25 efficient min-spend ratio
+15 platform voucher
+12 trusted source
```

## Tasks

- [ ] Ranking model.
- [ ] Configurable rules.
- [ ] Explanation output.
- [ ] Tests against representative voucher sets.
- [ ] Telegram formatting.

## Exit criteria

- Owner can reduce noise without deleting collectors.
- Ranking decisions are deterministic and inspectable.

---

# Phase 18 — Voucher lifecycle intelligence

## Objective

Track more than "new voucher" and build useful historical behavior.

## Deliverables

### Version history

Record changes in:

- time window;
- minimum spend;
- discount amount/cap;
- scope;
- identifier completeness;
- availability state.

### Lifecycle events

```text
first seen
changed
upcoming
active
saved
exhausted
expired
ineligible
```

### Source timing analytics

Measure:

```text
first seen by source
first seen globally
source update timestamps
```

Use this to identify which sources consistently surface vouchers earlier.

## Tasks

- [ ] Version diff service.
- [ ] Lifecycle transition rules.
- [ ] Source-latency metrics.
- [ ] Historical reports.

## Exit criteria

- System can explain when a voucher first appeared and which source found it first.
- Noisy sources can be evaluated using data rather than intuition.

---

# Phase 19 — Adaptive polling and source optimization

## Objective

Reduce unnecessary traffic while improving useful discovery latency.

## Deliverables

### Adaptive intervals

Inputs may include:

- time of day;
- known campaign windows;
- recent source changes;
- rate-limit responses;
- recent error rate;
- source historical usefulness.

### Backoff

Capped exponential backoff with jitter.

### Source budget

Per-source request ceilings to prevent accidental high-frequency loops.

### Poll effectiveness analytics

Track:

```text
requests per new voucher
requests per meaningful update
average discovery delay
rate-limit incidents
```

## Tasks

- [ ] Adaptive interval engine.
- [ ] Source request budget.
- [ ] Polling metrics.
- [ ] Configuration documentation.

## Exit criteria

- Polling behavior changes based on evidence.
- Service can report cost/effectiveness per source.
- No collector can accidentally issue unlimited requests.

---

# Phase 20 — Session resilience and browser isolation

## Objective

Make browser/session problems unable to destabilize the core watcher.

## Deliverables

### Browser process isolation

If evidence supports it, move the Rust Chromium/CDP session manager into a separate process/service.

Reasons may include:

- Chromium memory growth;
- browser crashes;
- independent restart needs.

### Session synchronization protocol

Provide a narrow internal interface for:

- health state;
- cookie/session refresh;
- login-required status.

Do not expose raw cookies over a public API.

### Recovery controls

Admin command or local endpoint:

```text
pause claims
resume claims
refresh session
show session health
```

## Tasks

- [ ] Measure browser stability first.
- [ ] Separate process only if justified.
- [ ] Add restart supervision.
- [ ] Add health bridge.
- [ ] Harden local IPC/auth.

## Exit criteria

- Chromium crash does not crash voucher discovery/storage.
- Claim service remains paused until session state is valid again.

---

# Phase 21 — Operational health API and admin controls

## Objective

Operate the bot without SSHing into logs for every problem.

## Deliverables

### Health endpoints

```text
/health/live
/health/ready
/health/details
```

### Admin actions

Local/private only:

```text
pause/resume claims
trigger collector once
show pending jobs
cancel a job
show recent claim results
request session health refresh
```

### Security

Admin interface must not be public unauthenticated Internet surface.

## Tasks

- [ ] FastAPI/ASGI admin service or equivalent.
- [ ] Read-only health first.
- [ ] Add guarded mutation endpoints.
- [ ] Add audit logs for admin actions.

## Exit criteria

- Core operational state is queryable in one place.
- Dangerous actions are authenticated/private and audited.

---

# Phase 22 — Production observability

## Objective

Make failures and performance regressions visible before they silently degrade voucher hunting.

## Deliverables

### Metrics exporter

Expose Prometheus-compatible metrics or equivalent.

### Dashboard

Panels for:

```text
collector health
new vouchers/hour
source latency
scheduler lag
claim result rates
Shopee request latency
session state
notification failures
process uptime
```

### Alerts

Examples:

- no successful collector run for threshold;
- session unhealthy near an upcoming claim;
- scheduler lag above threshold;
- DB unavailable;
- repeated unknown Shopee responses;
- Telegram delivery failing;
- process restart loop.

## Tasks

- [ ] Metrics instrumentation.
- [ ] Dashboard definition.
- [ ] Alert thresholds.
- [ ] Alert deduplication/cooldown.

## Exit criteria

- Major service failures produce an alert.
- Performance can be diagnosed from metrics without enabling debug logging.

---

# Phase 23 — Production deployment hardening

## Objective

Create a reproducible VPS deployment.

## Deliverables

### Rust production image

Build with a multi-stage Dockerfile: compile the workspace in a Rust builder image, then copy only the required release binary/assets into a minimal runtime image. The runtime image must not contain Cargo, the Rust compiler, source code, research repositories, or browser debugging tools unless the isolated session component requires them.

Build with `--locked` and treat `Cargo.lock` as deployment input. Record the application version/commit in the binary and startup telemetry.

### Docker Compose

Services:

```text
app
postgres
optional monitoring
```

Browser profile and DB use persistent volumes.

### Secure configuration

- `.env` not committed;
- restrictive filesystem permissions;
- no public DB port;
- no public browser debugging port;
- private/admin endpoint binding.

### Restart behavior

Application must:

- migrate safely;
- rebuild scheduler state;
- verify DB health;
- classify session health;
- not immediately fire stale claim jobs blindly.

### Host documentation

- required RAM/CPU;
- timezone irrelevant internally but host NTP required;
- backup location;
- log rotation/container logging;
- upgrade procedure.

## Tasks

- [ ] Multi-stage Rust production Dockerfile using locked dependencies.
- [ ] Compose file.
- [ ] Healthchecks.
- [ ] Persistent volumes.
- [ ] Backup scripts.
- [ ] Restore test.
- [ ] Upgrade/rollback procedure.

## Exit criteria

- Fresh VPS can be provisioned from documentation.
- Reboot restores service automatically.
- DB restore procedure is tested.

---

# Phase 24 — Rust hot-path profiling and release tuning

## Objective

Make the application-owned portion of discovery and claim latency as small and predictable as practical after correctness, source quality, and session stability are established.

Rust is selected to reduce runtime overhead and improve timing predictability, but this phase must prove improvements with measurements. Network/source latency will often dominate CPU time.

## Deliverables

### End-to-end benchmark harness

Run representative benchmarks in release mode and measure:

- collector fetch-to-normalized duration;
- representative JSON parse/normalization cost;
- internal bounded-channel queue delay;
- DB transaction duration;
- scheduler planned-vs-awake error;
- claim-plan preparation duration;
- HTTP request finalization duration;
- warm pooled request latency;
- cold DNS/TCP/TLS latency separately from warm-path latency.

### Profiling

Use platform-appropriate Rust profiling/flamegraph tooling to identify real CPU, lock, allocation, or syscall hotspots. Do not optimize from intuition alone.

### Precision-path preparation

Before the final execution window, ensure:

- the voucher `ClaimPlan` is already validated and loaded;
- session health/cookie snapshot is already available;
- endpoint/request metadata is prepared;
- the shared `reqwest::Client` and connection pool already exist;
- no migration, fixture parsing, browser navigation, or ordinary DB lookup remains on the T=0 path.

### Concurrency review

Audit for:

- mutex guards held across `.await`;
- accidental unbounded task spawning;
- oversized channel buffers hiding backpressure;
- unnecessary clones of raw payloads;
- synchronous work blocking Tokio worker threads;
- per-request HTTP client creation.

### Release build policy

Benchmark default `cargo build --release` first. Evaluate LTO/codegen settings only when they show repeatable benefit and acceptable build/deploy tradeoffs. Do not use `unsafe` merely to chase theoretical speed.

## Tasks

- [ ] Add release-mode benchmark binary and benchmark fixtures.
- [ ] Record an end-to-end latency baseline.
- [ ] Add queue-delay instrumentation.
- [ ] Profile collector parse/normalize path.
- [ ] Profile claim-plan preparation path.
- [ ] Audit Tokio lock/channel behavior.
- [ ] Verify no browser calls in the precision path.
- [ ] Verify no required DB read at T=0.
- [ ] Compare warm vs cold HTTP latency.
- [ ] Document accepted latency budget per stage.
- [ ] Add a simple regression procedure for future releases.

## Exit criteria

- Application-owned scheduler/serialization/queue overhead is quantified.
- Important latency regressions are detectable with repeatable benchmarks.
- Claim execution at T=0 uses a prebuilt plan and a warm shared HTTP client.
- No speculative unsafe/direct-socket optimization is present.

---

# Phase 25 — Security hardening

## Objective

Reduce the blast radius of a VPS or application compromise.

## Deliverables

### Secret inventory

List every secret and storage location.

### File permissions

Harden:

- browser profile;
- environment files;
- backup files;
- SSH configuration as appropriate.

### Log redaction audit

Automated tests for sensitive field redaction.

### Dependency scanning

Add:

- dependency vulnerability checks;
- secret scanning;
- container scanning where practical.

### Least privilege

Run the application as a non-root user.

### Network exposure review

Only expose ports that are required.

## Tasks

- [ ] Threat model.
- [ ] Secret inventory.
- [ ] Non-root container.
- [ ] Permission checks.
- [ ] Secret scanner.
- [ ] Dependency scanner.
- [ ] Backup security review.

## Exit criteria

- No known plaintext secret is stored in Git or normal logs.
- Browser profile is accessible only to the service owner/process.
- Application runs without root privileges.

---

# Phase 26 — Reliability engineering

## Objective

Prove that the service survives real operational failures.

## Deliverables

### Failure injection tests

Test:

- network timeout;
- DNS failure;
- 429/rate limit;
- malformed response;
- DB restart;
- Telegram outage;
- Chromium crash;
- session expiry;
- process restart immediately before a scheduled job;
- system restart with pending jobs.

### Idempotency audit

Verify:

- voucher upsert;
- schedule reconstruction;
- claim attempt locking;
- notification delivery.

### Soak testing

Run for extended time using fake/replay sources before relying on live behavior.

## Tasks

- [ ] Failure injection harness.
- [ ] Restart tests.
- [ ] 24h/72h soak test with replay collector.
- [ ] Memory/FD monitoring.
- [ ] Connection leak checks.

## Exit criteria

- No known failure mode causes uncontrolled retries or duplicate claim storms.
- Long-running process has stable memory/resource behavior.

---

# Phase 27 — Source quality analytics

## Objective

Determine objectively which discovery sources are worth keeping.

## Deliverables

Per-source statistics:

```text
new unique vouchers discovered
first-discovery wins
average discovery lead time
false/stale voucher rate
parse failure rate
requests per useful discovery
rate-limit incidents
```

### Source scoring

Produce a source quality score used for operational decisions, not necessarily voucher ranking.

### Automatic degradation

Repeated bad data can temporarily reduce polling frequency or disable a collector pending review.

## Exit criteria

- Sources can be compared quantitatively.
- Poor sources no longer consume equal polling resources by default.

---

# Phase 28 — Personal purchase planning support

## Objective

Move beyond collecting vouchers and help decide which ones are useful for the owner's planned purchases.

This phase remains decision support rather than automatic checkout.

## Deliverables

### Watchlist model

Allow owner to define:

- product URLs/IDs;
- shop IDs;
- category tags;
- target price;
- planned spend amount.

### Voucher applicability hints

Based only on information available to the bot, estimate whether a voucher might be relevant.

Represent uncertainty explicitly.

### Planned-order calculator

Given a manual basket estimate, compare voucher economics:

```text
minimum spend
discount amount
cap
voucher type
known restrictions
```

Do not claim guaranteed checkout applicability unless actually verified through a supported flow.

## Tasks

- [ ] Watchlist entities.
- [ ] CRUD via config/admin bot.
- [ ] Relevance scoring.
- [ ] Telegram summaries.

## Exit criteria

- Owner can receive fewer, more relevant voucher alerts based on planned purchases.

---

# Phase 29 — Cart/voucher combination analysis

## Objective

Provide local optimization of voucher combinations when enough basket/voucher data is available.

This phase does **not** automate checkout.

## Deliverables

### Basket input

Manual or safely obtained basket summary:

```text
items
shops
subtotal per shop
shipping estimate if known
payment method preference
```

### Constraint model

Represent known voucher constraints.

### Optimization engine

Evaluate possible compatible voucher choices and estimate savings.

### Explanation

Show:

```text
selected combination
estimated discount per voucher
why alternatives lose
uncertain assumptions
```

## Exit criteria

- The optimizer is deterministic and testable.
- Unknown constraints are surfaced instead of guessed as facts.

---

# Phase 30 — Campaign-aware scheduling

## Objective

Adapt behavior around known Shopee campaign windows without hard-coding one-off dates into worker logic.

## Deliverables

### Campaign calendar

Store campaign metadata:

```text
name
start/end
high-interest windows
source overrides
polling profile
notification profile
```

### Poll profiles

Examples:

```text
NORMAL
PRE_CAMPAIGN
CAMPAIGN_ACTIVE
RECOVERY
```

### Operational controls

Before a major campaign:

- verify session;
- verify DB health;
- verify notifier;
- check clock/NTP;
- validate source health;
- list scheduled voucher jobs.

## Exit criteria

- Campaign behavior is configuration/data-driven.
- No code deployment is needed merely to change a campaign date.

---

# Phase 31 — Advanced scheduler performance tuning

## Objective

Optimize only after measurements show timing is a meaningful limitation.

## Deliverables

### Latency benchmark tooling

Measure from deployment region:

```text
DNS
connect
TLS
TTFB
full request
scheduler lag
```

### HTTP pool tuning

Tune:

- keep-alive;
- connection pool size;
- DNS behavior;
- timeouts.

### Host comparison

Benchmark candidate VPS regions/providers using the actual target host/network path without generating harmful traffic.

### Precision reports

For each claim attempt record:

```text
planned_at
sent_at
delta_ms
response_at
network_latency_ms
```

## Exit criteria

- Optimization decisions are based on measured latency data.
- No tuning sacrifices reliability or causes uncontrolled request volume.

---

# Phase 32 — Admin Telegram commands

## Objective

Use Telegram as a lightweight operations console.

## Deliverables

Safe commands such as:

```text
/status
/session
/sources
/jobs
/recent
/pause_claims
/resume_claims
/watchlist
```

Sensitive or mutating commands require owner chat ID allowlisting.

Do not expose secrets through commands.

## Exit criteria

- Routine operations can be performed safely from Telegram.
- Unauthorized chat IDs cannot operate the bot.

---

# Phase 33 — Data retention and maintenance jobs

## Objective

Prevent an always-on system from accumulating unbounded historical data.

## Deliverables

Retention policies for:

```text
raw observations
old voucher versions
claim attempts
collector run details
outbox events
health events
logs
```

Keep enough history for source-quality and reliability analytics.

### Maintenance worker

Perform bounded cleanup and database maintenance.

## Exit criteria

- Database growth is predictable.
- Critical audit history is retained according to documented policy.

---

# Phase 34 — Upgrade and schema-change resilience

## Objective

Make frequent upstream breakage manageable.

## Deliverables

### Parser versioning

Each unstable parser has a version.

### Fixture regression suite

Maintain representative fixtures across known schema variants.

### Feature flags

Disable broken collector or claim adapter independently.

### Compatibility dashboard

Display last successful parser/client version per source.

### Safe rollout

Deployment strategy:

```text
backup
migrate
start with auto-claim disabled if risky
verify collectors/session
re-enable claim
```

## Exit criteria

- One upstream schema change does not require emergency rewriting of unrelated subsystems.
- Broken integrations can be disabled without shutting down the entire service.

---

# Phase 35 — Disaster recovery

## Objective

Be able to rebuild the service after VPS loss.

## Deliverables

### Backup policy

Back up:

- PostgreSQL;
- config templates;
- operational metadata;
- optionally encrypted browser profile if owner accepts the risk.

### Recovery runbook

Document:

1. provision VPS;
2. install runtime;
3. restore DB;
4. deploy current image;
5. restore or re-bootstrap session;
6. start claims disabled;
7. verify collectors;
8. verify scheduler reconstruction;
9. enable claims.

### Recovery exercise

Perform at least one test restore.

## Exit criteria

- Service can be rebuilt without relying on undocumented manual knowledge.

---

# Phase 36 — Mature operations and continuous improvement

## Objective

Transition from "project being built" to a maintained personal production system.

## Recurring work

### Weekly/monthly checks

- review unknown responses;
- inspect source health;
- inspect session failures;
- check rate-limit incidents;
- review scheduler lag;
- update dependencies;
- validate backups;
- inspect DB growth;
- remove broken sources;
- refresh fixtures when upstream behavior changes.

### Performance review

Track trends rather than individual anecdotal successes.

Key questions:

- Which source finds useful vouchers first?
- How often are discovered vouchers actually claim-ready?
- What fraction of claim attempts succeed?
- Which failure classes dominate?
- Is polling volume justified by unique discoveries?
- Is scheduler/network latency materially affecting outcomes?
- How often does session maintenance require manual intervention?

### Architecture review triggers

Only consider significant architecture changes when evidence shows the need.

Examples:

- split browser service because Chromium destabilizes core process;
- add Redis because DB-backed coordination becomes a measured bottleneck;
- separate collectors because one source requires incompatible runtime dependencies;
- move notifier to separate worker because delivery backlog becomes significant.

## Exit criteria

There is no final "done" state for a system integrating unstable external behavior. The mature state is reached when:

- deployment is reproducible;
- session recovery is documented;
- sources are measurable;
- claim behavior is bounded and auditable;
- important failures alert the owner;
- upstream changes are isolated;
- backups are tested;
- operational maintenance is routine rather than emergency-driven.

---

# Milestone summary

The phases above can be grouped into major product milestones.

## Milestone A — Foundation

Phases 0–4

Outcome:

> Clean repository, stable domain model, database, worker framework, collector architecture.

## Milestone B — Real discovery

Phases 5–8

Outcome:

> Real Shopee VN voucher data enters the system, is deduplicated, stored, and sent to Telegram.

## Milestone C — Authenticated 24/7 session

Phases 9–11

Outcome:

> Secure persistent Shopee session with health monitoring and a tested Shopee client boundary.

## Milestone D — Time-sensitive action engine

Phases 12–16

Outcome:

> Durable scheduler, precision execution, policy engine, controlled claim execution, durable notifications.

## Milestone E — Intelligent hunter

Phases 17–19

Outcome:

> Voucher ranking, lifecycle intelligence, and adaptive source polling.

## Milestone F — Production-grade fast operations

Phases 20–27

Outcome:

> Browser isolation if needed, admin controls, monitoring, hardened deployment, measured Rust hot-path tuning, security, reliability, and source analytics.

## Milestone G — Purchase decision support

Phases 28–32

Outcome:

> Watchlists, voucher-combination analysis, campaign awareness, advanced scheduler optimization, and Telegram admin UX.

## Milestone H — Long-term maintainability

Phases 33–36

Outcome:

> Retention, schema-change resilience, disaster recovery, and mature operating procedures.

---

# Priority rule

When deciding whether to begin a later phase early, use this priority:

```text
correctness
→ session safety
→ discovery freshness
→ claim-path latency and timing precision
→ persistence/restart safety
→ observability
→ claim reliability
→ discovery coverage
→ ranking intelligence
```

Optimize latency early at architectural boundaries (warm clients, durable preloading, monotonic deadlines), but only keep micro-optimizations that are measured and do not make failures opaque.

