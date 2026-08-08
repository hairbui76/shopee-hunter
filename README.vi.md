# shopee-hunter

**🌐 Ngôn ngữ:** [English](README.md) · **Tiếng Việt**

Trợ lý cá nhân theo dõi và lưu voucher Shopee Việt Nam chạy 24/7, viết hoàn toàn
bằng Rust theo kiến trúc modular monolith. Nó phát hiện voucher từ các nguồn
được cấu hình, chuẩn hóa và khử trùng lặp, giữ session Shopee khỏe mạnh, lên
lịch hành động chính xác theo thời gian, thử lưu voucher có kiểm soát khi được
bật rõ ràng, và thông báo cho chủ sở hữu qua Telegram — với khả năng quan sát
tốt và an toàn khi khởi động lại.

**Phạm vi:** Chỉ Shopee Việt Nam, một tài khoản người mua. **Không** giải/vượt
CAPTCHA, không giả mạo vân tay thiết bị, không xoay proxy, không farm đa tài
khoản, không tự động thanh toán/đặt hàng.

---

## Yêu cầu

- **Rust 1.94.0** (đã ghim trong `rust-toolchain.toml` — `rustup` tự cài) cùng
  trình liên kết C (`gcc`/`cc`).
- **SQLite** cho phát triển cục bộ (đã kèm qua SQLx — không cần cài).
- **PostgreSQL** cho production (qua Docker Compose).
- **Docker + Docker Compose** để triển khai bằng container.
- Tùy chọn: **Chromium** cục bộ chỉ dùng cho bước khởi tạo session bằng trình
  duyệt (bật qua feature; không cần cho luồng request thông thường).

## 1. Cấu hình

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

## 2. Chạy cục bộ

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

## 3. Dashboard web

App phục vụ sẵn hai dashboard web tự chứa (không cần build, không tải asset
ngoài) — mở bằng trình duyệt:

- **`http://127.0.0.1:8686/`** — **Dashboard voucher**: danh sách voucher đã
  phát hiện (mã, loại, mức giảm, đơn tối thiểu, thời gian hiệu lực, trạng thái,
  nguồn, thấy lần cuối) kèm tìm kiếm/lọc, và panel "lần thu thập gần nhất" hiển
  thị lần chạy collector mới nhất theo từng nguồn (cron). Tự làm mới.
- **`http://127.0.0.1:8686/ops`** — **Dashboard vận hành**: sức khỏe dịch vụ,
  trạng thái session & claim-gate, số liệu phát hiện, job đã lên lịch, các lần
  claim gần đây, kèm nút pause/resume/refresh (cần admin token, nhập trong
  trang và chỉ lưu ở trình duyệt).

JSON phía sau: `GET /vouchers`, `GET /collectors`.

> **Mở ra ngoài:** đặt `HEALTHCHECK_BIND_ADDR=0.0.0.0:8686` để truy cập từ máy
> khác. Các view đọc không có xác thực, nên chỉ mở trong mạng tin cậy / sau
> reverse proxy, và đặt `ADMIN_TOKEN` mạnh (không có nó thì các thao tác thay
> đổi trạng thái bị tắt).

## 4. Endpoint health & admin

API bind riêng tư mặc định (`127.0.0.1:8686`):

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

## 4. Khởi tạo session (đăng nhập chủ tài khoản)

Việc claim cần một session Shopee đã xác thực. Khởi tạo một lần, thủ công, bằng
Chromium cục bộ (không bao giờ in cookie và không vượt qua bất kỳ thử thách xác
minh nào):

```bash
SHOPEE_PROFILE_PATH=/var/lib/shopee-hunter/browser-profile \
  cargo run -p shopee-hunter-tools --bin login_session --features browser
```

Đăng nhập trong cửa sổ mở ra, rồi nhấn Enter. Xem `docs/session-profile.md`.

## 5. Chạy bằng Docker (production)

```bash
cp .env.example .env    # đặt POSTGRES_PASSWORD, secret thật, các cờ ENABLE_*
docker compose up -d
docker compose logs -f app
```

Lệnh này chạy app (non-root) cùng PostgreSQL với volume bền vững; cả cổng DB lẫn
API admin đều không được publish ra ngoài. Xem `docs/deployment.md`.

## 6. Kiểm thử & cổng chất lượng

```bash
cargo test --workspace --all-features                                   # 430 test
cargo fmt --all --check                                                 # định dạng
cargo clippy --workspace --all-targets --all-features -- -D warnings    # lint
cargo deny check                                                        # chính sách phụ thuộc (CI)
```

## 7. Benchmark độ trễ (tùy chọn)

```bash
cargo run --release -p shopee-hunter-tools --bin benchmark_latency
```

In độ trễ từng giai đoạn do ứng dụng sở hữu. Xem `docs/latency-budget.md`.

## Tài liệu

- `ARCHITECTURE.md` — thiết kế hệ thống và ranh giới.
- `ROADMAP.md` — toàn bộ lộ trình phát triển (đã hoàn thành mọi phase).
- `docs/deployment.md`, `docs/disaster-recovery.md`, `docs/security.md`,
  `docs/observability.md`, `docs/operations.md`, `docs/session-profile.md`,
  `docs/upgrade-resilience.md`, `docs/latency-budget.md`.

## Cấu trúc workspace

```text
crates/
  domain/         kiểu voucher chuẩn tắc, định danh, máy trạng thái
  collectors/     adapter phát hiện + supervisor + pipeline chuẩn hóa
  shopee-client/  transport HTTP đã xác thực + bộ phân loại phản hồi
  session/        cookie store, health, claim gate, khởi tạo trình duyệt
  scheduler/      lập lịch bền vững + chính xác
  claimer/        chính sách claim + thực thi có kiểm soát + retry
  ranking/        chấm điểm và quy tắc đủ điều kiện
  notifier/       Telegram + định dạng + phân phối outbox + lệnh admin
  storage/        repository SQLx + migration (SQLite/PostgreSQL)
  observability/  tracing, metrics, health, alert, worker supervisor
  analytics/      phân tích chất lượng nguồn
  planning/       độ liên quan watchlist + tối ưu tổ hợp voucher
  campaign/       hồ sơ polling theo chiến dịch
  app/            composition root + API admin/health + binary chạy dài
  tools/          login_session, benchmark_latency, inspect_fixture
```

**Giấy phép:** MIT.
