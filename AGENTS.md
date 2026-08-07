# AGENTS.md

## Mission

This document defines how coding agents should work inside the `shopee-hunter` repository.

The repository implements a continuously running personal Shopee Vietnam voucher-hunting service. Agents should treat the codebase as a long-lived production service rather than a disposable automation script.

The system has four high-level responsibilities:

1. **Discover** voucher opportunities.
2. **Understand** and normalize voucher data.
3. **Schedule and act** at the correct time under a controlled claim policy.
4. **Operate reliably** on a VPS with secure session handling and useful observability.

---

## Core constraints

Agents must preserve the following project constraints unless the owner explicitly changes them:

- Market: Shopee Vietnam only.
- Primary mode: personal buyer account.
- Runtime: approximately 24/7.
- Implementation language: Rust only for production services and utilities.
- Performance: discovery freshness and claim-path latency are first-class requirements; optimize with measurement and warm reusable state.
- Deployment: VPS/container environment.
- Authentication: owner-provided Shopee session, managed securely.
- Browser automation: session/bootstrap/fallback only, not the default data plane.
- Claim attempts: bounded and policy-driven.
- Human verification: pause and notify; do not bypass.
- No automatic checkout or payment.
- No account farming or abuse-oriented scaling.

---

## Agent responsibilities

### Repository understanding

Before changing code, inspect:

- `CLAUDE.md`;
- `ARCHITECTURE.md`;
- relevant roadmap phase in `ROADMAP.md`;
- package configuration;
- existing interfaces and tests;
- recent migrations if storage is involved.

Do not assume the current repository exactly matches planned architecture. Prefer the actual code for immediate behavior and the architecture documents for intended boundaries.

### Scope discipline

For each task, identify:

- which subsystem owns the behavior;
- what state is read;
- what state is written;
- which external systems are contacted;
- how failure should be represented;
- what should be observable.

If a change spans many subsystems, prefer introducing or strengthening an interface rather than creating direct cross-module dependencies.

---

## Subsystem ownership

### Collectors

Responsible for obtaining candidate voucher information.

Collectors may:

- fetch configured sources;
- parse source payloads;
- emit normalized candidates plus source metadata;
- report source health.

Collectors must not:

- claim vouchers;
- send Telegram messages directly;
- own session-refresh policy;
- write arbitrary database rows outside repository abstractions.

### Domain

Responsible for source-independent concepts:

- voucher identity;
- voucher state;
- claim result classes;
- session state;
- domain events;
- scheduling intent.

Domain code should not import Chromium/CDP clients, `reqwest`, Telegram SDKs, or SQLx database types. Domain crates should remain transport-independent.

### Shopee client

Responsible for transport and Shopee-specific response interpretation.

It should expose operations such as:

- check session health;
- fetch a supported voucher-related resource;
- attempt a voucher save/claim;
- parse/classify Shopee responses.

It must not decide whether a voucher *should* be claimed. That belongs to claim policy.

### Session manager

Responsible for:

- current authentication state;
- browser profile lifecycle;
- safe transfer of cookie state to the HTTP client when required;
- session health classification;
- user notification trigger when manual login/verification is needed.

It must never silently defeat an interactive platform verification challenge.

### Scheduler

Responsible for durable timing intent.

It should answer:

- what should happen;
- when it should happen;
- whether it already happened;
- how restart recovery works.

It must not hide scheduling state only in memory.

### Claimer

Responsible for executing allowed voucher-save attempts under policy.

It owns:

- precondition checks;
- bounded retry decisions;
- result persistence;
- claim attempt correlation IDs;
- handing response classes back to domain/application services.

### Notifier

Responsible for message delivery and formatting.

It should not contain business decisions.

### Storage

Responsible for persistence abstractions, transactions, migrations, and durable state.

Do not leak ORM models into every subsystem. Map storage models to domain structures at repository boundaries.

---

## Working with unstable external behavior

Shopee private web behavior may change. Agents must assume any private endpoint, parameter, signature field, or response shape is unstable.

Whenever adding or updating such an integration:

1. isolate it behind a single adapter or client method;
2. validate the response at the boundary;
3. add redacted fixtures;
4. add parser/response-classifier tests;
5. make unknown response handling explicit;
6. add useful diagnostic logging without secrets;
7. document the assumption in code or architecture notes.

Do not scatter endpoint paths across unrelated files. Centralize them in a Shopee endpoint/config module.

---

## Reverse-engineering workflow

When studying reference repositories or browser behavior:

1. identify the minimum behavior needed;
2. capture request/response metadata without secrets;
3. determine required vs incidental headers/fields;
4. create a sanitized fixture;
5. model the behavior through the project's own interface;
6. write a contract test;
7. only then wire it into a long-running worker.

Do not copy entire scripts into production code.

Keep research artifacts under `research/` and ensure they contain no sensitive session material.

---

## Long-running worker standard

Every worker loop must have:

- a named service identity;
- explicit startup log;
- explicit shutdown log;
- graceful cancellation handling;
- top-level error isolation;
- bounded retry/backoff;
- health/heartbeat signal;
- configurable interval;
- jitter where synchronized polling is undesirable;
- metrics for success and failure;
- no busy loop.

Example conceptual loop:

```rust
loop {
    tokio::select! {
        _ = cancel.cancelled() => break,
        result = run_iteration() => {
            match result {
                Ok(()) => health.mark_success(),
                Err(err) if err.is_transient() => health.mark_degraded(&err),
                Err(err) => {
                    health.mark_failure(&err);
                    tracing::error!(error = ?err, "worker_iteration_failed");
                }
            }
        }
    }

    tokio::select! {
        _ = cancel.cancelled() => break,
        _ = tokio::time::sleep(next_delay()) => {}
    }
}
```

The exact code may differ, but these properties must remain.

---

## Retry policy standard

Retries are never generic.

Classify before retrying.

Examples:

- network timeout -> retry with capped exponential backoff;
- HTTP 429 -> obey retry hints if available and back off;
- session expired -> do not retry claim repeatedly; transition session state;
- voucher not active -> reschedule if a trustworthy start time exists;
- voucher exhausted -> terminal;
- already saved -> terminal success-equivalent;
- account not eligible -> terminal for that account/voucher;
- verification required -> pause and notify;
- unknown response -> small diagnostic budget, then stop and alert.

No infinite retries.

---

## Time handling standard

Time-sensitive automation is a core feature.

Rules:

- Persist timestamps in UTC.
- Use `DateTime<Utc>` (or the project-wide accepted UTC type) for persisted timestamps; never persist naive local time.
- Display times in `Asia/Ho_Chi_Minh` by default.
- Use a monotonic clock for short waits.
- Do not use blocking thread sleep inside Tokio tasks; use `tokio::time::sleep` / `sleep_until`.
- Inject a clock in scheduler tests.
- Treat server clock synchronization as an operational dependency.

Tests should cover:

- jobs before/after start time;
- process restart near start time;
- stale jobs;
- late execution;
- duplicate scheduler reconstruction;
- timezone conversion.

---

## Persistence standard

Important state must survive process restarts.

Persist at least:

- known voucher identity;
- latest voucher version;
- source observations;
- claim eligibility state;
- schedule intent;
- claim attempts/results;
- notification delivery state when idempotency matters;
- session health metadata, excluding unsafe secret duplication.

Avoid using local files as ad hoc databases once database-backed storage exists.

---

## Data retention standard

The repository should distinguish:

- normalized voucher records;
- observation history;
- claim audit history;
- raw fixtures/debug payloads;
- sensitive session state.

Sensitive session state should have the shortest practical exposure and must not be copied into ordinary audit tables.

Raw payload retention should be configurable.

---

## Logging standard

Use structured logs.

Good:

```text
{"event":"voucher_discovered","voucher_id":"...","source":"...","latency_ms":42}
```

Bad:

```text
Found thing!!! response={huge payload including cookies}
```

Never log:

- full cookie values;
- authorization tokens;
- Telegram bot token;
- browser profile contents;
- full request headers if they may contain secrets.

Provide helper functions for redaction rather than relying on developers to remember every call site.

---

## Security-sensitive changes

Changes to any of the following require extra review:

- cookie serialization;
- browser profile handling;
- admin endpoints;
- remote debugging;
- Docker volume mounts;
- secret configuration;
- logging middleware;
- HTTP request dumps;
- database backups;
- notification contents.

Before finishing such a task, search the changed code for common secret-bearing field names and verify redaction.

---

## Testing expectations

### Minimum per feature

A feature change normally requires:

- happy path test;
- at least one important failure-path test;
- boundary validation test if external payload parsing changed.

### Parser changes

Must include fixtures.

### Scheduler changes

Must use a fake clock or deterministic timestamp inputs.

### Claim policy changes

Must test each newly introduced terminal/retry classification.

### Storage changes

Must test migration/constraint behavior where relevant.

### Notification changes

Must test rendering separately from network delivery.

---

## Rust implementation standard

- Production code is Rust-only. Do not add Python/Node helper daemons to work around Rust design work.
- Prefer safe Rust. `unsafe` requires an ADR-level justification, a benchmark proving material benefit, and focused tests around invariants.
- Use Tokio as the sole async runtime.
- Reuse `reqwest::Client`; it is intended to be long-lived and shared.
- Do not clone large payloads merely to satisfy ownership. Introduce `Arc`, borrowed views, or smaller owned DTOs where lifetime/ownership boundaries are clear.
- Do not put every service behind `Arc<Mutex<_>>`. Separate mutable ownership and communicate through bounded channels where practical.
- Use `thiserror` enums at crate boundaries; preserve source errors and classify them once.
- Never `unwrap()`/`expect()` on network, external JSON, DB, clock, or session paths. `expect()` is acceptable only for compile-time/static invariants with an explanatory message.
- Keep release performance measurable with Criterion/microbenchmarks only for actual hot functions, and end-to-end latency benchmarks for the real request path.
- Optimize release profile only after measurement. Prefer algorithmic/I/O improvements over exotic compiler flags.

## CI quality gates

The project should eventually require:

- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --workspace --all-features`;
- integration tests that do not require a live Shopee session;
- SQLx migration consistency checks;
- `cargo deny check` and/or `cargo audit` according to repository policy;
- secret scanning;
- release container build.

Live Shopee checks must be opt-in and not block ordinary CI.

---

## Performance guidance

Do not prematurely optimize.

Measure these first:

- source fetch latency;
- normalization latency;
- database write latency;
- scheduler execution lag;
- Shopee request latency;
- notifier latency.

Optimize the highest-impact portion.

For claim timing, connection reuse, DNS/TLS readiness, and scheduler lag are usually more relevant than micro-optimizing allocations or copies. Profile before introducing unsafe code or low-level HTTP replacements.

---

## Polling guidance

Polling intervals must be configuration-driven.

Prefer adaptive behavior such as:

- normal interval during quiet periods;
- shorter interval near known campaign windows;
- temporary fast refresh after a meaningful upstream change;
- backoff during errors/rate limits.

Do not introduce aggressive constant-frequency polling without measurements and a documented reason.

---

## Feature flags

Potentially risky or unstable integrations should be individually disableable.

Examples:

```text
ENABLE_SHOPEE_PAGE_COLLECTOR
ENABLE_EXTERNAL_FEED_COLLECTOR
ENABLE_AUTO_CLAIM
ENABLE_BROWSER_SESSION_REFRESH
ENABLE_TELEGRAM
```

Default new mutating behavior to disabled until tested.

---

## Operational behavior

The service should fail safely.

Examples:

### Database unavailable

- stop mutation-heavy workers;
- do not continue claiming without durable audit state;
- expose unhealthy status;
- notify once without spamming.

### Telegram unavailable

- continue core watcher if safe;
- persist/send notification later if outbox is implemented;
- report notifier health.

### Shopee session expires

- pause claim worker;
- keep passive collectors running if they do not require authentication;
- request manual login;
- automatically resume only after session health is verified.

### Unknown claim response

- do not assume success;
- persist redacted response classification data;
- avoid repeated uncontrolled attempts;
- alert owner.

---

## Documentation updates

Update documentation when:

- introducing a subsystem;
- changing data flow;
- changing session handling;
- adding a collector source;
- adding a new persistent entity;
- changing deployment topology;
- changing retry or claim policy materially;
- completing a roadmap phase.

`ROADMAP.md` should track outcomes, not implementation trivia.

`ARCHITECTURE.md` should describe current target architecture and accepted tradeoffs.

---

## Pull request / task completion format

When summarizing work, use this structure:

```text
Summary
- what changed

Behavior
- user-visible/system-visible effect

Tests
- commands run and result

Operational notes
- config/migration/deployment changes

Risks / follow-ups
- remaining uncertainty
```

---

## Anti-patterns

Agents should actively avoid:

- one giant `ShopeeBot` class;
- raw dictionaries passed through every layer;
- endpoint strings duplicated across collectors and claimer;
- unbounded retries;
- arbitrary `sleep()` calls used as synchronization;
- direct Telegram calls from parsers;
- DB writes from response-classification functions;
- global cookie jars;
- plaintext session dumps;
- relying on process memory as the sole schedule source;
- swallowing unknown Shopee responses;
- live-account-dependent unit tests;
- copying entire reference repositories into this project;
- optimizing for maximum request throughput rather than reliability.

---

## Definition of a healthy implementation

A healthy implementation can answer, from logs/database/metrics:

- Which sources are currently healthy?
- When was the last successful collection from each source?
- Which vouchers were discovered recently?
- Why was a voucher considered eligible or ineligible?
- What jobs are scheduled in the next hour?
- Did a claim attempt occur?
- What response class did it receive?
- Is the Shopee session currently healthy?
- When did session state last change?
- Is the bot running behind schedule?
- Are notifications being delivered?

If the system cannot answer these questions, agents should prioritize observability before adding more automation.

