# Upgrade & schema-change resilience (ROADMAP Phase 34)

Shopee's private behavior changes without notice. The system is built so one
upstream change does not force an emergency rewrite of unrelated subsystems.

## Parser versioning

Every collector stamps a `parser_version` on each observation
(`external-feed/1`, `replay/1`, …). Stored observations therefore record which
parser produced them, so a schema change is attributable and a re-parse is
possible. Bump the version string when the parse logic changes.

## Anti-corruption boundary

All Shopee transport assumptions live in `shopee-hunter-client`
(`endpoints.rs`, `dto.rs`, `classify.rs`, `plan.rs`). The expected blast radius
of an upstream change is that module plus the classification tables — no other
crate parses Shopee JSON or hard-codes a path.

## Fixture regression suite

`tests/fixtures/shopee/` holds sanitized fixtures for every response class and
session-probe state. Build-failing coverage tests ensure a class never loses
its fixture. When upstream changes, add a fixture for the new variant and adapt
the classifier; the suite pins the old and new behavior side by side.

## Feature flags

Risky integrations are individually disableable via configuration so a broken
one can be shut off without stopping the service:

```text
ENABLE_SHOPEE_PAGE_COLLECTOR
ENABLE_EXTERNAL_FEED_COLLECTOR
ENABLE_MANUAL_COLLECTOR
ENABLE_REPLAY_COLLECTOR
ENABLE_AUTO_CLAIM
ENABLE_BROWSER_SESSION_REFRESH
ENABLE_TELEGRAM
```

New mutating behavior defaults to disabled until tested.

## Compatibility visibility

`collector_runs` records per-source outcome and parse errors; the analytics
crate (Phase 27) surfaces the last successful run and parse-failure rate per
source — the data behind a "which integration is currently healthy" view.

## Safe rollout

```text
1. backup (docker/backup.sh)
2. migrate (automatic, idempotent on startup)
3. start with ENABLE_AUTO_CLAIM=false if the change is risky
4. verify collectors + session (/admin/session, /admin/jobs, source health)
5. re-enable claim
```

A broken collector or claim adapter can be disabled independently, so an
upstream break degrades one source rather than the whole service.
