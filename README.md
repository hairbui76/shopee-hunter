# shopee-hunter

A personal-use, 24/7 Shopee Vietnam voucher monitoring and claiming assistant,
written entirely in Rust as a modular monolith. It discovers voucher
opportunities from configured sources, normalizes and deduplicates them, keeps a
Shopee session healthy, schedules time-sensitive actions precisely, attempts a
controlled voucher-save when explicitly enabled, and notifies the owner over
Telegram — with strong observability and restart safety.

> Trợ lý cá nhân theo dõi và lưu voucher Shopee Việt Nam chạy 24/7, viết hoàn
> toàn bằng Rust theo kiến trúc modular monolith. Nó phát hiện voucher từ các
> nguồn được cấu hình, chuẩn hóa và khử trùng lặp, giữ session Shopee khỏe mạnh,
> lên lịch hành động chính xác theo thời gian, thử lưu voucher có kiểm soát khi
> được bật rõ ràng, và thông báo cho chủ sở hữu qua Telegram — với khả năng
> quan sát tốt và an toàn khi khởi động lại.

**Scope / Phạm vi:** Shopee Vietnam only, one buyer account. It does **not**
implement CAPTCHA/verification bypass, fingerprint spoofing, proxy rotation,
multi-account farming, or checkout/payment automation. / Chỉ Shopee Việt Nam,
một tài khoản người mua. **Không** giải/vượt CAPTCHA, không giả mạo vân tay
thiết bị, không xoay proxy, không farm đa tài khoản, không tự động thanh toán.

---

## English

### Requirements

- **Rust 1.94.0** (pinned in `rust-toolchain.toml` — `rustup` installs it
  automatically) with a working C linker (`gcc`/`cc`).
- **SQLite** for local development (bundled via SQLx — nothing to install).
- **PostgreSQL** for production (via Docker Compose).
- **Docker + Docker Compose** for containerized deployment.
- Optional: a local **Chromium** only for the browser-based session bootstrap
  (feature-gated; not needed for the normal request path).

### 1. Configure

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

### 2. Run locally

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

### 3. Health & admin endpoints

The API binds privately (default `127.0.0.1:8686`):

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

### 4. Session bootstrap (owner login)

Claiming needs an authenticated Shopee session. Bootstrap it once, manually,
with a local Chromium (this never prints cookies and never bypasses any
verification challenge):

```bash
SHOPEE_PROFILE_PATH=/var/lib/shopee-hunter/browser-profile \
  cargo run -p shopee-hunter-tools --bin login_session --features browser
```

Log in in the opened window, then press Enter. See `docs/session-profile.md`.

### 5. Run with Docker (production)

```bash
cp .env.example .env    # set POSTGRES_PASSWORD, real secrets, ENABLE_* flags
docker compose up -d
docker compose logs -f app
```

This runs the app (non-root) plus PostgreSQL with persistent volumes; neither
the DB port nor the admin API is published. See `docs/deployment.md`.

### 6. Tests & quality gates

```bash
cargo test --workspace --all-features                                   # 430 tests
cargo fmt --all --check                                                 # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings    # lints
cargo deny check                                                        # dependency policy (CI)
```

### 7. Latency benchmark (optional)

```bash
cargo run --release -p shopee-hunter-tools --bin benchmark_latency
```

Prints per-stage application-owned latency. See `docs/latency-budget.md`.

### Documentation

- `ARCHITECTURE.md` — system design and boundaries.
- `ROADMAP.md` — full development path (all phases complete).
- `docs/deployment.md`, `docs/disaster-recovery.md`, `docs/security.md`,
  `docs/observability.md`, `docs/operations.md`, `docs/session-profile.md`,
  `docs/upgrade-resilience.md`, `docs/latency-budget.md`.

---

## Tiếng Việt

### Yêu cầu

- **Rust 1.94.0** (đã ghim trong `rust-toolchain.toml` — `rustup` tự cài) cùng
  trình liên kết C (`gcc`/`cc`).
- **SQLite** cho phát triển cục bộ (đã kèm qua SQLx — không cần cài).
- **PostgreSQL** cho production (qua Docker Compose).
- **Docker + Docker Compose** để triển khai bằng container.
- Tùy chọn: **Chromium** cục bộ chỉ dùng cho bước khởi tạo session bằng trình
  duyệt (bật qua feature; không cần cho luồng request thông thường).

### 1. Cấu hình

Sao chép file môi trường mẫu và điền giá trị thật. Tuyệt đối không commit `.env`.

```bash
cp .env.example .env
```

Các thiết lập chính (đều có mô tả trong `.env.example`):

| Biến | Ý nghĩa |
|---|---|
| `DATABASE_URL` | `sqlite://data/shopee-hunter.db?mode=rwc` cho dev, `postgres://…` cho prod |
| `ENABLE_REPLAY_COLLECTOR` / `ENABLE_EXTERNAL_FEED_COLLECTOR` | nguồn phát hiện voucher nào chạy |
| `EXTERNAL_FEED_URL` | URL feed (bắt buộc nếu bật external-feed collector) |
| `ENABLE_TELEGRAM`, `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID` | thông báo |
| `ENABLE_AUTO_CLAIM` | **mặc định false** — để tắt cho đến khi đã kiểm chứng luồng claim thật |
| `ADMIN_TOKEN` | bắt buộc để dùng các endpoint admin có thay đổi trạng thái |
| `HEALTHCHECK_BIND_ADDR` | địa chỉ bind API admin/health (giữ riêng tư, mặc định `127.0.0.1:8686`) |

### 2. Chạy cục bộ

```bash
# Tải phụ thuộc (dùng Cargo.lock đã commit)
cargo fetch --locked

# Chạy dịch vụ (chế độ phát triển)
DATABASE_URL="sqlite://data/shopee-hunter.db?mode=rwc" \
  cargo run -p shopee-hunter-app
```

Nếu không bật collector nào, dịch vụ sẽ khởi động, phục vụ health API rồi chờ.
Để xem toàn bộ pipeline phát hiện → outbox → notifier chạy mà không cần nguồn
thật, bật replay collector với các fixture đi kèm:

```bash
DATABASE_URL="sqlite://data/dev.db?mode=rwc" \
ENABLE_REPLAY_COLLECTOR=true \
REPLAY_FIXTURE_DIR=tests/fixtures/replay \
COLLECTOR_DEFAULT_INTERVAL_SECS=5 \
  cargo run -p shopee-hunter-app
```

Dịch vụ tắt sạch khi nhấn `Ctrl-C` (SIGINT) hoặc nhận SIGTERM.

### 3. Endpoint health & admin

API bind riêng tư (mặc định `127.0.0.1:8686`):

```bash
curl http://127.0.0.1:8686/health/live      # tiến trình còn sống
curl http://127.0.0.1:8686/health/ready      # DB + worker đã sẵn sàng
curl http://127.0.0.1:8686/health/details    # sức khỏe từng dịch vụ
curl http://127.0.0.1:8686/metrics           # số liệu Prometheus

curl http://127.0.0.1:8686/admin/session         # trạng thái session + claim
curl http://127.0.0.1:8686/admin/jobs            # các job đã lên lịch
curl http://127.0.0.1:8686/admin/claims/recent   # các lần claim gần đây

# Hành động thay đổi trạng thái cần admin token:
curl -X POST -H "x-admin-token: $ADMIN_TOKEN" \
  http://127.0.0.1:8686/admin/claims/pause
curl -X POST -H "x-admin-token: $ADMIN_TOKEN" \
  http://127.0.0.1:8686/admin/claims/resume
```

### 4. Khởi tạo session (đăng nhập chủ tài khoản)

Việc claim cần một session Shopee đã xác thực. Khởi tạo một lần, thủ công, bằng
Chromium cục bộ (không bao giờ in cookie và không vượt qua bất kỳ thử thách xác
minh nào):

```bash
SHOPEE_PROFILE_PATH=/var/lib/shopee-hunter/browser-profile \
  cargo run -p shopee-hunter-tools --bin login_session --features browser
```

Đăng nhập trong cửa sổ mở ra, rồi nhấn Enter. Xem `docs/session-profile.md`.

### 5. Chạy bằng Docker (production)

```bash
cp .env.example .env    # đặt POSTGRES_PASSWORD, secret thật, các cờ ENABLE_*
docker compose up -d
docker compose logs -f app
```

Lệnh này chạy app (non-root) cùng PostgreSQL với volume bền vững; cả cổng DB lẫn
API admin đều không được publish ra ngoài. Xem `docs/deployment.md`.

### 6. Kiểm thử & cổng chất lượng

```bash
cargo test --workspace --all-features                                   # 430 test
cargo fmt --all --check                                                 # định dạng
cargo clippy --workspace --all-targets --all-features -- -D warnings    # lint
cargo deny check                                                        # chính sách phụ thuộc (CI)
```

### 7. Benchmark độ trễ (tùy chọn)

```bash
cargo run --release -p shopee-hunter-tools --bin benchmark_latency
```

In độ trễ từng giai đoạn do ứng dụng sở hữu. Xem `docs/latency-budget.md`.

### Tài liệu

- `ARCHITECTURE.md` — thiết kế hệ thống và ranh giới.
- `ROADMAP.md` — toàn bộ lộ trình phát triển (đã hoàn thành mọi phase).
- `docs/deployment.md`, `docs/disaster-recovery.md`, `docs/security.md`,
  `docs/observability.md`, `docs/operations.md`, `docs/session-profile.md`,
  `docs/upgrade-resilience.md`, `docs/latency-budget.md`.

---

## Workspace layout / Cấu trúc workspace

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
