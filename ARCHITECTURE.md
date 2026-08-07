# ARCHITECTURE.md

## 1. Overview

`shopee-hunter` is a continuously running personal automation service for monitoring Shopee Vietnam voucher opportunities and assisting with time-sensitive voucher saving/claiming.

The architecture is intentionally designed around unstable external integrations. Shopee web behavior, private endpoints, page schemas, and response structures may change without notice. Therefore, the system isolates external assumptions behind adapters and preserves raw observations for diagnosis.

The core architectural goal is not "send as many requests as possible." It is:

> Discover useful voucher opportunities early, understand them correctly, act at the intended time under a bounded policy, and remain diagnosable and recoverable over long-running operation.

---

## 2. Goals

### Functional goals

- Run continuously on a VPS.
- Monitor one or more voucher discovery sources.
- Normalize heterogeneous voucher data into a canonical model.
- Detect newly discovered vouchers and meaningful updates.
- Rank/filter vouchers by user usefulness.
- Persist future claim/scheduling intent.
- Maintain one authenticated Shopee buyer session.
- Execute controlled voucher-save attempts when configured.
- Notify the owner of discoveries, upcoming starts, claim outcomes, and service/session problems.
- Recover safely after service or host restart.

### Non-functional goals

- Strong isolation between discovery, session, claim, and notification logic.
- Low operational complexity.
- Good observability.
- Bounded request rates.
- Safe failure behavior.
- Deterministic, low-jitter scheduling on Tokio monotonic deadlines.
- Low application-owned latency on discovery and claim paths, with pooled/warm network state.
- Rust-only production implementation with no scripting-language runtime dependency.
- Easy replacement of unstable external adapters.
- Minimal secret exposure.

---

## 3. Non-goals

The architecture does not target:

- multi-account farms;
- CAPTCHA bypass;
- anti-detection fingerprint manipulation;
- IP/proxy rotation for bypass purposes;
- checkout/order/payment automation;
- high-volume scraping of the entire marketplace;
- distributed multi-region claiming;
- commercial SaaS multi-tenancy in the initial product;
- dependence on affiliate or seller APIs.

---

## 4. System context

```text
                         ┌──────────────────────┐
                         │      Shopee VN       │
                         │  pages / web APIs    │
                         └──────────┬───────────┘
                                    │
                                    │
     ┌──────────────────┐           │           ┌──────────────────┐
     │ External voucher │───────────┼───────────│ Manual / curated │
     │ sources          │           │           │ inputs           │
     └────────┬─────────┘           │           └────────┬─────────┘
              │                     │                    │
              └─────────────┬───────┴────────────┬───────┘
                            ▼                    │
                    ┌───────────────┐            │
                    │   Collectors  │            │
                    └───────┬───────┘            │
                            ▼                    │
                    ┌───────────────┐            │
                    │ Normalization │            │
                    │ + Dedup       │            │
                    └───────┬───────┘            │
                            ▼                    │
                    ┌───────────────┐            │
                    │   Database    │◄───────────┘
                    └───────┬───────┘
                            │
              ┌─────────────┼───────────────┐
              ▼             ▼               ▼
       ┌────────────┐ ┌────────────┐ ┌─────────────┐
       │  Ranking   │ │ Scheduler  │ │  Notifier   │
       └──────┬─────┘ └─────┬──────┘ └─────────────┘
              │             │
              └──────┬──────┘
                     ▼
               ┌────────────┐
               │ Claim svc  │
               └─────┬──────┘
                     ▼
               ┌────────────┐
               │ Shopee HTTP│
               │   client   │
               └─────┬──────┘
                     │
          ┌──────────┴──────────┐
          ▼                     ▼
   ┌─────────────┐       ┌──────────────┐
   │Session mgr  │◄─────►│Browser ctx   │
   └─────────────┘       │(Rust CDP)  │
                         └──────────────┘
```

---

## 5. Runtime topology

### Rust runtime model

The core application is a Rust binary on Tokio's multi-thread runtime. Network collectors, scheduler coordination, Telegram delivery, database access, and Shopee HTTP operations share this runtime but remain isolated by bounded concurrency, explicit task ownership, and cancellation-aware supervision.

The browser/session component is deliberately outside the low-latency request path. Chromium is controlled through a Rust-native CDP adapter (for example `chromiumoxide`) or a separately supervised Rust session process. The session component may refresh cookie state and publish a health snapshot, but the claim executor uses the pooled HTTP client whenever the required flow is representable over HTTP.

Hot-path target:

```text
preloaded voucher claim plan
        +
healthy session snapshot
        +
warm reqwest connection pool
        +
Tokio monotonic deadline
        ↓
minimal request finalization
        ↓
claim request
```

No discovery parsing, browser navigation, migration work, ordinary database lookup, or client construction should be required after entering the final precision window.

### Phase-appropriate deployment

Initial production deployment can remain a single application container plus persistent services:

```text
VPS
├── shopee-hunter application
├── PostgreSQL
├── persistent Shopee browser profile volume
└── optional monitoring stack
```

Internally, the application runs multiple async services:

```text
main process
├── collector supervisor
├── voucher processor
├── scheduler service
├── claim worker
├── session health worker
├── notification worker
└── health/admin HTTP service
```

The code should still use clear service boundaries so any component can later become a separate process if operational evidence justifies it.

### Why not microservices initially?

The workload is small, shared state is important, and deployment simplicity matters more than independent scaling. Process separation adds network failure modes, distributed tracing requirements, message broker complexity, and deployment overhead without immediate benefit.

### Core Rust technology choices

```text
Async runtime         Tokio
HTTP fast path        reqwest + rustls
Serialization         serde / serde_json
Persistence           SQLx
Health/admin HTTP     axum
Structured telemetry  tracing / tracing-subscriber
Errors                thiserror (+ anyhow at binary boundary)
Money                 rust_decimal
Time                  chrono + chrono-tz + tokio::time::Instant
Browser fallback      Rust-native Chromium CDP adapter
```

These are defaults, not a requirement for every crate to import every dependency. Domain crates should remain narrow and transport-independent.

---

## 6. Core data flow

### 6.1 Voucher discovery

```text
Collector timer
    ↓
Fetch source
    ↓
Boundary validation
    ↓
Parse source-specific model
    ↓
Normalize into VoucherCandidate
    ↓
Compute identity + raw hash
    ↓
Upsert observation
    ↓
New / changed?
    ├── no → update last_seen
    └── yes
         ↓
       emit domain event
         ↓
      ranking/policy
         ↓
  notifications + scheduling
```

The collector does not directly decide to claim.

### 6.2 Scheduled claim flow

```text
Voucher is eligible
       ↓
Persist schedule intent
       ↓
Scheduler watches durable jobs
       ↓
Pre-flight window
       ├── validate session
       ├── refresh voucher metadata if configured
       ├── validate claim policy
       └── warm required client resources
       ↓
Target time reached
       ↓
Create ClaimAttempt
       ↓
Shopee client sends request
       ↓
Response classifier
       ↓
ClaimResult
       ↓
Persist result
       ↓
Policy decides terminal / retry / reschedule
       ↓
Notify owner
```

### 6.3 Session refresh flow

```text
Session health check
       ↓
Healthy? ── yes ──► update timestamp
       │
       no
       ↓
Classify
├── expired
├── login required
└── verification required
       ↓
Pause claims
       ↓
Browser-assisted/manual login workflow
       ↓
Validate session again
       ↓
Healthy
       ↓
Resume claim eligibility
```

No verification challenge is automatically bypassed.

---

## 7. Domain model

### 7.1 Voucher

Canonical logical voucher entity:

```text
Voucher
- id
- source-independent identity
- canonical title/description
- voucher type
- discount semantics
- spend requirements
- time window
- scope/payment constraints
- latest known identifiers needed for save/claim
- first_seen_at
- last_seen_at
- status
```

Voucher type examples:

```text
PLATFORM
SHOP
FREESHIP
PAYMENT
LIVE
VIDEO
CATEGORY
UNKNOWN
```

### 7.2 VoucherObservation

Represents a single source observation.

```text
VoucherObservation
- id
- voucher_id
- source
- source_key
- observed_at
- source_updated_at
- raw_hash
- normalized_hash
- raw_payload or raw_payload_reference
- parser_version
```

This is useful because the same voucher may be observed repeatedly or through multiple sources.

### 7.3 ScheduleJob

```text
ScheduleJob
- id
- voucher_id
- action_type
- execute_at
- preflight_at
- status
- scheduler_version
- attempt_count
- created_at
- updated_at
```

Statuses:

```text
PENDING
READY
RUNNING
SUCCEEDED
FAILED
CANCELLED
STALE
```

### 7.4 ClaimAttempt

Append-oriented audit record:

```text
ClaimAttempt
- id
- voucher_id
- schedule_job_id
- started_at
- completed_at
- request_version
- result_class
- upstream_status
- latency_ms
- retry_index
- diagnostic_code
```

Never store sensitive cookie values in claim records.

### 7.5 SessionStatus

```text
SessionStatus
- state
- checked_at
- last_healthy_at
- reason_code
- browser_profile_version
```

This is operational metadata, not a duplicate credential store.

---

## 8. Voucher state machine

A useful logical state machine:

```text
DISCOVERED
   ↓
VALIDATED
   ↓
ELIGIBLE ───────────────┐
   ↓                    │
SCHEDULED               │
   ↓                    │
CLAIMING                 │
   ├── success ──► SAVED
   ├── already ──► SAVED
   ├── exhausted ─► EXHAUSTED
   ├── ineligible ► INELIGIBLE
   ├── expired ───► EXPIRED
   ├── retryable ─► SCHEDULED
   └── unknown ───► REVIEW_REQUIRED
```

Not every voucher needs to go through claim states. Some may remain informational only.

---

## 9. Collector architecture

### Collector interface

```rust
#[async_trait::async_trait]
pub trait VoucherCollector: Send + Sync {
    fn name(&self) -> &str;
    async fn collect(&self, context: &CollectionContext) -> Result<CollectionResult, CollectorError>;
}
```

### Required collector behavior

Each collector must:

- own its fetch logic;
- use bounded timeouts;
- expose source health;
- return partial results safely if possible;
- validate external data;
- emit canonical candidates;
- never mutate Shopee account state.

### Collector classes

Potential collectors:

1. **Shopee page/API collector**
   - observes configured Shopee voucher/campaign resources;
   - authenticated only if required;
   - isolated because private schemas may change.

2. **External feed collector**
   - optional feeds or community data sources;
   - each source gets its own parser/version.

3. **Manual collector**
   - accepts user-provided code, URL, or structured voucher data;
   - useful for Telegram/community discoveries.

4. **Replay collector**
   - development-only source that emits recorded fixtures.

### Source health model

```text
HEALTHY
DEGRADED
RATE_LIMITED
AUTH_REQUIRED
FAILED
DISABLED
```

Health should be per source, not global.

---

## 10. Normalization and deduplication

External voucher data is often incomplete and inconsistent.

### Normalization stages

```text
raw source payload
      ↓
boundary schema
      ↓
source parser
      ↓
canonical field mapping
      ↓
time/currency normalization
      ↓
identity calculation
      ↓
version hashing
```

### Identity strategy

Preferred ranking of identifiers:

1. source-provided stable voucher ID;
2. Shopee `promotion_id` if present and confirmed stable for this purpose;
3. a deterministic composite identity.

Composite identity candidates:

```text
source
code
start_at
end_at
voucher_type
scope
min_spend
discount properties
```

### Version detection

A logical voucher can change while keeping the same identity.

Examples:

- start time changes;
- minimum spend changes;
- discount cap changes;
- scope becomes more restrictive;
- source adds missing signature information.

Use a normalized hash to detect meaningful changes and trigger reevaluation.

---

## 11. Ranking and eligibility

Ranking answers "How useful is this voucher?"

Eligibility answers "Should the system schedule an action?"

Keep them separate.

### Ranking inputs

Potential inputs:

- nominal discount;
- effective discount relative to minimum spend;
- discount cap;
- voucher type;
- time until activation;
- duration;
- known restrictions;
- owner's configured categories/shops;
- source confidence;
- whether required claim identifiers are present.

### Eligibility policy

A voucher is auto-claim eligible only if policy allows it.

Example conditions:

- `ENABLE_AUTO_CLAIM=true`;
- source is trusted enough;
- session is healthy;
- start time is known;
- necessary request identifiers exist;
- voucher is not expired;
- voucher is not in an exclusion rule;
- daily/operational retry limits are not exceeded.

Policy should produce both a boolean decision and human-readable reasons.

---

## 12. Session architecture

### Design goals

- Keep browser credentials/session material off Git.
- Avoid requiring an interactive browser for every request.
- Detect expiration before critical claim windows.
- Support manual login recovery.
- Keep authentication concerns out of voucher logic.

### Components

#### Persistent browser context

Stored in a dedicated mounted volume:

```text
/var/lib/shopee-hunter/browser-profile/
```

The exact path is configurable.

This context is used for:

- initial login;
- session refresh;
- diagnosis;
- browser-required flows.

#### HTTP session

The HTTP client should maintain a controlled cookie jar derived from an approved session state when needed.

Do not expose the cookie jar globally.

#### Session manager

Single owner of authentication lifecycle.

Responsibilities:

- initialize state;
- health check;
- refresh/synchronize authentication;
- transition states;
- coordinate pause/resume of claim worker;
- surface manual action requirements.

### Session health states

```text
UNKNOWN
HEALTHY
DEGRADED
EXPIRED
LOGIN_REQUIRED
VERIFICATION_REQUIRED
DISABLED
```

Transitions are evented and logged.

---

## 13. Shopee client architecture

The Shopee client is an anti-corruption layer around unstable platform behavior.

### Rust transport

The normal transport is one shared `reqwest::Client` constructed at startup and reused for the process lifetime. It owns connection pooling, keep-alive behavior, cookie/header integration, decompression policy, and timeout configuration. Rebuilding a client per request is prohibited because it discards pool state and adds avoidable DNS/TCP/TLS overhead.

Use `hyper` directly only if profiling proves a material bottleneck that cannot be addressed through `reqwest` configuration. Rust makes lower-level networking possible, but the architecture should not become lower-level without evidence.

Request construction is split into two stages:

1. **prepare** — validate voucher/session state and build an immutable `ClaimPlan` before the precision window;
2. **execute** — at the target deadline, attach only truly volatile request metadata and send through the already-warm client.

### Responsibilities

- construct requests;
- apply current authenticated state;
- centralize endpoint definitions;
- enforce timeouts;
- classify HTTP/network errors;
- validate response schemas;
- map responses to domain result types;
- collect latency metrics;
- redact diagnostics.

### Response classification

All claim responses should become one of a finite set:

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

The claimer should never parse arbitrary JSON itself.

---

## 14. Scheduler architecture

Voucher timing is sensitive, so scheduling is durable.

### Tokio deadline model

Persisted schedule times are wall-clock UTC. During the final preflight window the scheduler converts the target into a monotonic `tokio::time::Instant` and waits with `tokio::time::sleep_until`. This keeps short final waits independent from ordinary wall-clock corrections.

The scheduler records planned wall time, precision-window entry time, actual wake time, request-send time, and observed lag. If host clock state changes materially before the precision window, the monotonic deadline is recomputed from persisted intent.

### Two-level scheduling

#### Coarse scheduler

Handles future jobs minutes/hours away.

Use durable database state plus a coarse Tokio wake loop (`tokio::time::interval_at` or repository-driven next-deadline lookup).

#### Precision execution window

When a job enters a short preflight window, a dedicated Tokio precision task handles:

- session verification;
- metadata refresh if required;
- HTTP connection readiness;
- monotonic wait until target;
- bounded attempt execution.

### Why this split?

Keeping thousands of long-lived Tokio sleep tasks is unnecessary. Conversely, relying only on a coarse database polling loop can introduce avoidable lag near target time.

### Restart recovery

At startup:

1. load pending jobs;
2. classify stale vs future;
3. reschedule future jobs;
4. handle recently missed jobs according to policy;
5. never create duplicate active jobs for the same logical action.

---

## 15. Claim architecture

### Claim service input

```text
voucher
session status
policy configuration
attempt history
schedule context
```

### Preflight

Before executing:

- ensure session is healthy;
- ensure voucher still makes sense;
- ensure not already saved successfully;
- check retry budget;
- verify required request fields;
- create durable attempt intent if needed.

### Execution

The HTTP client makes the request.

The response classifier returns a domain result.

### Retry

Retry only when classification allows it.

Example conceptual policy:

```text
TRANSIENT_FAILURE     -> limited retry
RATE_LIMITED          -> delayed retry
NOT_ACTIVE            -> reschedule if start time reliable
SESSION_EXPIRED       -> pause and refresh session
VERIFICATION_REQUIRED -> pause and notify
EXHAUSTED             -> terminal
INELIGIBLE            -> terminal
SUCCESS               -> terminal
ALREADY_SAVED         -> terminal success-equivalent
UNKNOWN_RESPONSE      -> diagnostic limit, then review
```

No unbounded retry loops.

---

## 16. Notification architecture

Notifications are application events delivered asynchronously.

### Event examples

```text
voucher.discovered
voucher.updated
voucher.upcoming
claim.succeeded
claim.failed
claim.exhausted
session.expired
session.verification_required
collector.degraded
service.unhealthy
```

### Notification outbox

For reliability, later phases should use a database outbox:

```text
transaction commits domain change + outbox event
           ↓
notification worker reads outbox
           ↓
Telegram
           ↓
mark delivered
```

This prevents losing important notifications between DB commit and network send.

---

## 17. Storage architecture

### Development

SQLite is acceptable for local development and early single-process operation.

Use WAL mode if concurrent reads/writes are needed.

### Production

PostgreSQL is preferred once the project reaches persistent 24/7 operation.

### Suggested tables

```text
vouchers
voucher_observations
voucher_versions
schedule_jobs
claim_attempts
collector_runs
notification_outbox
service_health_events
```

Session secrets should not be replicated into ordinary database tables.

### Transaction boundaries

Important transitions should be atomic.

Example:

```text
new voucher version
+ schedule intent
+ notification outbox event
```

may be committed in one transaction when logically coupled.

---

## 18. Observability architecture

### Structured logs

Use JSON logs in production.

Required common fields:

```text
timestamp
level
service
event
voucher_id
source
attempt_id
session_state
latency_ms
result_class
```

### Metrics

Recommended:

#### Collector

- runs total;
- successes/failures;
- fetch latency;
- parse failures;
- discovered candidates;
- new vouchers;
- changed vouchers;
- rate-limit events.

#### Scheduler

- pending jobs;
- execution lag milliseconds;
- missed jobs;
- reconstructed jobs after restart.

#### Claimer

- attempts;
- results by class;
- latency;
- retries;
- success rate.

#### Session

- health check status;
- transitions;
- time since last healthy check.

#### Infrastructure

- database errors;
- notifier failures;
- worker loop lag;
- process uptime.

### Tracing

Optional initially. Correlation IDs in structured logs may be enough for a single-process service.

---

## 19. Health endpoints

Expose a local/admin-only HTTP health service.

Suggested endpoints:

```text
GET /health/live
GET /health/ready
GET /health/details
```

### Liveness

Answers whether process/event loop is alive.

### Readiness

Should consider:

- database reachable;
- migrations current;
- critical workers started;
- scheduler operating.

Session expiration should not necessarily make the entire service unready; passive discovery can continue. Instead expose session state in detailed health.

---

## 20. Configuration architecture

Use one typed settings model.

Categories:

```text
Application
Database
Shopee HTTP
Session/browser
Collectors
Scheduler
Claim policy
Telegram
Observability
```

All polling intervals, timeouts, retry budgets, and feature toggles should be configurable.

Avoid magic constants inside worker implementations.

---

## 21. Security architecture

### Secrets

Treat these as secrets:

- Shopee session cookies;
- persistent browser profile;
- Telegram token;
- database password;
- any external-source authentication token.

### Filesystem

Recommended production layout:

```text
/opt/shopee-hunter/          application
/etc/shopee-hunter/          configuration
/var/lib/shopee-hunter/      persistent data
/var/lib/shopee-hunter/browser-profile/
/var/log/shopee-hunter/      if not using container stdout
```

Session/profile directories should have restrictive permissions.

### Network

- Do not publicly expose browser DevTools.
- Bind admin endpoints to localhost or a private network.
- If remote administration is introduced, use authentication and TLS.

### Logging redaction

Redact headers/fields such as:

```text
cookie
set-cookie
authorization
token
secret
password
```

Use centralized redaction logic.

---

## 22. Deployment architecture

### Rust release artifact

Production deploys a compiled release binary, not a source/runtime environment. Use a multi-stage build with locked dependencies. The final application image should contain only the release executable, runtime certificates/libraries actually required, configuration entrypoints, and migrations/assets required at runtime.

The normal application container must not include Cargo/rustc or the research repositories. If Chromium is required for session maintenance, either install it only in an isolated session image or explicitly justify co-locating it.

### Docker Compose target

```text
services:
  app:
    image: shopee-hunter
    restart: unless-stopped
    volumes:
      - browser-profile:/var/lib/shopee-hunter/browser-profile
    depends_on:
      - postgres

  postgres:
    image: postgres
    restart: unless-stopped
    volumes:
      - postgres-data:/var/lib/postgresql/data

volumes:
  browser-profile:
  postgres-data:
```

Monitoring services can be added later.

### Restart policy

Use `restart: unless-stopped` or equivalent systemd behavior.

Application startup must be idempotent.

---

## 23. Backup and recovery

Back up:

- PostgreSQL data;
- configuration excluding secrets where possible;
- migrations;
- optionally browser profile if operationally necessary and secured.

Recovery procedure should include:

1. restore DB;
2. start application with claims disabled;
3. verify migrations;
4. verify session state;
5. rebuild scheduler state;
6. inspect pending jobs;
7. enable claim worker.

---

## 24. Failure modes

### Collector schema changes

Symptoms:

- parser errors;
- sudden zero-result feed;
- unexpected field changes.

Response:

- mark source degraded;
- preserve raw redacted sample;
- continue other collectors;
- alert owner;
- update fixture/parser.

### Shopee session expires

Response:

- transition session state;
- stop claim attempts;
- continue safe discovery;
- notify owner;
- require session recovery.

### Database unavailable

Response:

- stop actions requiring durable audit state;
- expose unhealthy readiness;
- do not claim without persistence.

### Telegram outage

Response:

- retain event if outbox exists;
- continue core services when safe;
- retry notifier separately.

### Scheduler delay

Response:

- measure execution lag;
- classify late jobs;
- do not blindly execute an expired voucher action;
- alert if lag exceeds threshold.

### Unknown Shopee claim response

Response:

- classify UNKNOWN_RESPONSE;
- store sanitized diagnostic data;
- limit retries;
- require review if repeated.

---

## 25. Performance model

The system's important latency chain is:

```text
source publishes/returns voucher
        ↓
collector polling delay
        ↓
network fetch latency
        ↓
parser + persistence
        ↓
notification or scheduler delay
        ↓
claim execution lag
        ↓
Shopee response latency
```

Measure each independently.

Do not attribute all delay to VPS region without evidence.

### Connection reuse

Use a process-long `reqwest::Client` and persistent connection pooling. Preflight may deliberately warm DNS/TCP/TLS/HTTP state with a safe read-only request when measurements prove that doing so lowers first-request latency. Never create a fresh client at claim time.

### Rust hot-path discipline

The claim path should be allocation-light and lock-light, but correctness wins over micro-optimization. Prepare owned or immutable request data before the target window, share through `Arc` only where ownership requires it, and avoid contended global mutexes.

Do not introduce `unsafe`, custom allocators, busy-spinning, direct socket code, or a hand-written HTTP stack without reproducible benchmark evidence.

Release-mode measurement must distinguish:

- application scheduler lag;
- queue/lock contention;
- serialization time;
- database interaction time;
- DNS/connect/TLS time;
- upstream TTFB.

### Database

Voucher volume for personal use is modest. Proper indexes are more important than horizontal scaling.

---

## 26. Scaling path

Scale only if needed.

### Stage 1

Single Tokio multi-thread process + PostgreSQL, with bounded tasks and long-lived connection pools.

### Stage 2

Separate browser/session worker if Chromium instability affects the core process.

### Stage 3

Separate collector and claim workers using a durable queue/outbox if isolation is operationally useful.

### Stage 4

Only consider Redis/message broker if there is clear evidence that database-backed coordination is insufficient.

No Kubernetes requirement is anticipated for personal deployment.

---

## 27. Testing architecture

### Unit

Pure domain logic and parsers.

### Contract/fixture

Captured redacted upstream payloads.

### Integration

Application + database + fake HTTP servers.

### End-to-end local

Replay collector -> DB -> scheduler -> fake Shopee client -> notifier stub.

### Live smoke

Opt-in, manual, minimal, and never required by CI.

---

## 28. Architectural decision records

Create `docs/adr/` when implementation begins to make significant choices.

Suggested initial ADRs:

```text
0001-single-process-modular-monolith.md
0002-postgresql-production-storage.md
0003-browser-as-session-fallback.md
0004-durable-scheduler-state.md
0005-bounded-claim-retries.md
0006-no-live-shopee-tests-in-ci.md
```

---

## 29. Target mature architecture

The mature system should operate like this:

```text
                         ┌──────────────────────┐
                         │ Collector Supervisor │
                         └──────────┬───────────┘
                                    ▼
                         ┌──────────────────────┐
                         │ Normalizer + Identity│
                         └──────────┬───────────┘
                                    ▼
                         ┌──────────────────────┐
                         │     PostgreSQL       │
                         └───────┬──────┬───────┘
                                 │      │
                     ┌───────────┘      └────────────┐
                     ▼                               ▼
            ┌────────────────┐             ┌────────────────┐
            │ Ranking/Policy │             │ Event / Outbox │
            └───────┬────────┘             └───────┬────────┘
                    ▼                              ▼
            ┌────────────────┐             ┌────────────────┐
            │ Durable Sched. │             │ Telegram worker│
            └───────┬────────┘             └────────────────┘
                    ▼
            ┌────────────────┐
            │ Claim Service  │
            └───────┬────────┘
                    ▼
            ┌────────────────┐
            │ Shopee Client  │
            └───────┬────────┘
                    │
           ┌────────┴─────────┐
           ▼                  ▼
   ┌──────────────┐   ┌───────────────┐
   │ HTTP session │   │ Session Mgr   │
   └──────────────┘   └───────┬───────┘
                              ▼
                      ┌───────────────┐
                      │ Rust CDP      │
                      │ persistent ctx│
                      └───────────────┘
```

The mature architecture is still intentionally a modular monolith unless operational evidence proves a need for service decomposition.

