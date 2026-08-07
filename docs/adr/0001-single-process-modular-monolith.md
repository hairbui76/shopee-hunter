# ADR 0001 — Single-process modular monolith

- Status: accepted
- Date: 2026-08-08

## Context

`shopee-hunter` is a personal, single-account, 24/7 voucher monitoring and
claiming assistant. The workload is small (one account, modest voucher volume),
but has strict requirements on latency, restart safety, and observability.
Splitting into services would add network failure modes, distributed tracing,
broker infrastructure, and deployment overhead with no immediate benefit.

## Decision

Build one Rust binary (`shopee-hunter-app`) on a single Tokio multi-thread
runtime, composed from narrowly scoped workspace crates:

```text
domain, collectors, shopee-client, session, scheduler,
claimer, ranking, notifier, storage, observability, app, tools
```

Crate boundaries follow subsystem ownership from `AGENTS.md`. The composition
root (`crates/app`) wires services together; business logic never lives there.
Internal services communicate through bounded channels and repository
abstractions so any component can later become a separate process if
operational evidence justifies it (see ARCHITECTURE.md §26 scaling path).

## Consequences

- Single deployable container plus PostgreSQL; simple operations.
- Shared process means the browser/CDP component must be kept off the hot path
  and isolated behind the session crate so instability cannot take down
  discovery (revisited in Phase 20).
- Crate boundaries are enforced at compile time: domain has no transport or
  storage dependencies; collectors cannot claim; notifier holds no business
  rules.
