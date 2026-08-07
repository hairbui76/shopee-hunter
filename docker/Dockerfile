# Multi-stage production build. The runtime image contains only the release
# binary, CA certificates, and migrations — no toolchain, source, or research.

FROM rust:1.94-slim-bookworm AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --locked -p shopee-hunter-app

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/shopee-hunter --shell /usr/sbin/nologin hunter \
    && mkdir -p /var/lib/shopee-hunter \
    && chown -R hunter:hunter /var/lib/shopee-hunter

COPY --from=builder /build/target/release/shopee-hunter-app /usr/local/bin/shopee-hunter-app

USER hunter
WORKDIR /var/lib/shopee-hunter
ENV APP_ENV=production LOG_FORMAT=json
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
    CMD curl -fsS http://127.0.0.1:8686/health/ready || exit 1
ENTRYPOINT ["/usr/local/bin/shopee-hunter-app"]
