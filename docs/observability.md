# Observability (ROADMAP Phase 22)

## Structured logs

JSON in production (`LOG_FORMAT=json`), pretty in development. Every event
carries a correlation-friendly field set where applicable: `event`, `service`,
`voucher_id`, `source`, `source_key`, `attempt_id`, `session_state`,
`latency_ms`, `result_class`, `timestamp`. Secrets are redacted centrally
(`observability::redact`); raw payloads are never logged at INFO.

## Metrics

Exposed at `GET /metrics` (Prometheus text) on the private admin bind. Key
series produced today:

| Metric | Meaning |
|---|---|
| `collector_runs_total{source}` | collector executions |
| `collector_failures_total{source}` | collector failures |
| `collector_candidates_total{source}` | candidates emitted |
| `collector_new_total{source}` / `collector_updated_total{source}` | discoveries |
| `collector_fetch_latency_ms{source}` | fetch+ingest latency histogram |
| `worker_iterations_total{service,result}` | supervised worker outcomes |
| `app_heartbeats_total` | liveness heartbeat |

Scheduler lag, claim result rates, Shopee request latency, session transitions,
and notifier failures are recorded by their owning crates and surfaced the same
way as they are wired in.

## Alerts

`observability::alerts::AlertEvaluator` maps conditions to a finite alert set
with per-kind cooldown (default 30m) so a persistent condition does not spam:

| Alert | Fires when |
|---|---|
| `NO_COLLECTOR_RUN` | no successful collector run within `collector_stale` (15m) |
| `SESSION_UNHEALTHY_NEAR_CLAIM` | session not healthy with a claim scheduled soon |
| `SCHEDULER_LAG_HIGH` | execution lag over `scheduler_lag_ms` (2s) |
| `DATABASE_UNAVAILABLE` | DB health probe fails |
| `REPEATED_UNKNOWN_RESPONSES` | ≥3 unknown Shopee responses |
| `NOTIFIER_DELIVERY_FAILING` | ≥5 notifier failures |
| `PROCESS_RESTART_LOOP` | ≥3 restarts in the window |

Thresholds are configurable via `AlertThresholds`. Alerts are delivered through
the notifier like any other event, so they reach the owner without SSH.

## Health endpoints

- `GET /health/live` — process/event loop alive.
- `GET /health/ready` — DB reachable, migrations current, workers started.
  Session expiry does NOT make the service unready; passive discovery continues.
- `GET /health/details` — per-service health snapshot + uptime.
