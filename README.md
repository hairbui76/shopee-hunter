# shopee-hunter

**🌐 Language:** **English** · [Tiếng Việt](README.vi.md)

A personal-use, 24/7 Shopee Vietnam voucher monitoring and claiming assistant,
written entirely in Rust as a modular monolith. It discovers voucher
opportunities from configured sources, normalizes and deduplicates them, keeps a
Shopee session healthy, schedules time-sensitive actions precisely, attempts a
controlled voucher-save when explicitly enabled, and notifies the owner over
Telegram — with strong observability and restart safety.

**Scope:** Shopee Vietnam only, one buyer account. It does **not** implement
CAPTCHA/verification bypass, fingerprint spoofing, proxy rotation, multi-account
farming, or checkout/payment automation.

---

## Requirements

- **Rust 1.94.0** (pinned in `rust-toolchain.toml` — `rustup` installs it
  automatically) with a working C linker (`gcc`/`cc`).
- **SQLite** for local development (bundled via SQLx — nothing to install).
- **PostgreSQL** for production (via Docker Compose).
- **Docker + Docker Compose** for containerized deployment.
- Optional: a local **Chromium** only for the browser-based session bootstrap
  (feature-gated; not needed for the normal request path).

## 1. Configure

Copy the example environment file and fill in real values. Never commit `.env`.

```bash
cp .env.example .env
```

Key settings (all documented in `.env.example`):

| Variable | Meaning |
|---|---|
| `DATABASE_URL` | `sqlite://data/shopee-hunter.db?mode=rwc` for dev, `postgres://…` for prod |
| `ENABLE_REPLAY_COLLECTOR` / `ENABLE_EXTERNAL_FEED_COLLECTOR` | which discovery sources run |
| `EXTERNAL_FEED_URL` | feed URL (required if the external-feed collector is enabled) |
| `ENABLE_TELEGRAM`, `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID` | notifications |
| `ENABLE_AUTO_CLAIM` | **default false** — leave off until you have verified the claim path live |
| `ADMIN_TOKEN` | required to use the mutating admin endpoints |
| `HEALTHCHECK_BIND_ADDR` | admin/health API bind (keep it private, default `127.0.0.1:8686`) |

## 2. Run locally

```bash
# Fetch dependencies (uses the committed Cargo.lock)
cargo fetch --locked

# Run the service (development)
DATABASE_URL="sqlite://data/shopee-hunter.db?mode=rwc" \
  cargo run -p shopee-hunter-app
```

With no collectors enabled it will start, serve the health API, and idle. To see
the full discovery → outbox → notifier pipeline run without any real source,
enable the replay collector against the bundled fixtures:

```bash
DATABASE_URL="sqlite://data/dev.db?mode=rwc" \
ENABLE_REPLAY_COLLECTOR=true \
REPLAY_FIXTURE_DIR=tests/fixtures/replay \
COLLECTOR_DEFAULT_INTERVAL_SECS=5 \
  cargo run -p shopee-hunter-app
```

The service shuts down cleanly on `Ctrl-C` (SIGINT) or SIGTERM.

## 3. Web dashboards

The app serves two self-contained web dashboards (no build step, no external
assets) — open them in a browser:

- **`http://127.0.0.1:8686/`** — **Voucher dashboard**: the list of discovered
  vouchers (code, type, discount, minimum spend, validity window, status,
  source, last seen) with search/filter, plus a "last collection" panel showing
  the most recent collector run per source (the cron). Auto-refreshes.
- **`http://127.0.0.1:8686/ops`** — **Operator dashboard**: service health,
  session & claim-gate state, discovery metrics, scheduled jobs, and recent
  claim attempts, with pause/resume/refresh buttons (these require the admin
  token, entered in the page and stored only in your browser).

JSON behind them: `GET /vouchers`, `GET /collectors`.

> **Exposing it:** set `HEALTHCHECK_BIND_ADDR=0.0.0.0:8686` to reach it from
> other machines. The read views are unauthenticated, so only expose it on a
> trusted network / behind a reverse proxy, and set a strong `ADMIN_TOKEN`
> (without it, the mutating actions are disabled).

## 4. Health & admin endpoints

The API binds privately by default (`127.0.0.1:8686`):

```bash
curl http://127.0.0.1:8686/health/live      # process alive
curl http://127.0.0.1:8686/health/ready      # DB + workers ready
curl http://127.0.0.1:8686/health/details    # per-service health
curl http://127.0.0.1:8686/metrics           # Prometheus metrics

curl http://127.0.0.1:8686/admin/session         # session + claim state
curl http://127.0.0.1:8686/admin/jobs            # scheduled jobs
curl http://127.0.0.1:8686/admin/claims/recent   # recent claim attempts

# Mutating actions require the admin token:
curl -X POST -H "x-admin-token: $ADMIN_TOKEN" \
  http://127.0.0.1:8686/admin/claims/pause
curl -X POST -H "x-admin-token: $ADMIN_TOKEN" \
  http://127.0.0.1:8686/admin/claims/resume
```

## 4. Session bootstrap (owner login)

Claiming needs an authenticated Shopee session. Bootstrap it once, manually,
with a local Chromium (this never prints cookies and never bypasses any
verification challenge):

```bash
SHOPEE_PROFILE_PATH=/var/lib/shopee-hunter/browser-profile \
  cargo run -p shopee-hunter-tools --bin login_session --features browser
```

Log in in the opened window, then press Enter. See `docs/session-profile.md`.

## 5. Run with Docker (production)

```bash
cp .env.example .env    # set POSTGRES_PASSWORD, real secrets, ENABLE_* flags
docker compose up -d
docker compose logs -f app
```

This runs the app (non-root) plus PostgreSQL with persistent volumes; neither
the DB port nor the admin API is published. See `docs/deployment.md`.

## 6. Tests & quality gates

```bash
cargo test --workspace --all-features                                   # 430 tests
cargo fmt --all --check                                                 # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings    # lints
cargo deny check                                                        # dependency policy (CI)
```

## 7. Latency benchmark (optional)

```bash
cargo run --release -p shopee-hunter-tools --bin benchmark_latency
```

Prints per-stage application-owned latency. See `docs/latency-budget.md`.

## Documentation

- `ARCHITECTURE.md` — system design and boundaries.
- `ROADMAP.md` — full development path (all phases complete).
- `docs/deployment.md`, `docs/disaster-recovery.md`, `docs/security.md`,
  `docs/observability.md`, `docs/operations.md`, `docs/session-profile.md`,
  `docs/upgrade-resilience.md`, `docs/latency-budget.md`.

## Workspace layout

```text
crates/
  domain/         canonical voucher types, identity, state machines
  collectors/     discovery adapters + supervisor + normalization pipeline
  shopee-client/  authenticated HTTP transport + response classifiers
  session/        cookie store, health, claim gate, browser bootstrap
  scheduler/      durable + precision scheduling
  claimer/        claim policy + controlled execution + retry
  ranking/        scoring and eligibility rules
  notifier/       Telegram + formatting + outbox delivery + admin commands
  storage/        SQLx repositories + migrations (SQLite/PostgreSQL)
  observability/  tracing, metrics, health, alerts, worker supervisor
  analytics/      source quality analytics
  planning/       watchlist relevance + voucher combination optimizer
  campaign/       campaign-aware polling profiles
  app/            composition root + admin/health API + long-running binary
  tools/          login_session, benchmark_latency, inspect_fixture
```

**License:** MIT.
