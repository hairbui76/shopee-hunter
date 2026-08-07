# Deployment (ROADMAP Phase 23)

## Image

Production is a compiled release binary, built with a multi-stage Dockerfile
(`docker/Dockerfile`). The runtime image contains only the executable, CA
certificates, and `curl` for the healthcheck — no Cargo/rustc, no source, no
research repos. It runs as a non-root `hunter` user and records the app version
in startup telemetry (`CARGO_PKG_VERSION`).

Build with locked dependencies:

```bash
docker build -f docker/Dockerfile -t shopee-hunter .
```

## Compose

`docker-compose.yml` runs `app` + `postgres` with persistent volumes for the
browser profile, session data, and PostgreSQL. Neither the DB port nor the
admin/health port is published. `restart: unless-stopped` on both services.

```bash
cp .env.example .env    # fill in real values
docker compose up -d
docker compose logs -f app
```

## Restart behavior (idempotent startup)

On start the application:

1. connects to the DB and applies migrations (idempotent);
2. rebuilds scheduler state from `schedule_jobs`, marking far-past jobs STALE
   rather than blindly firing them;
3. classifies session health before any claim;
4. serves `/health/ready` only once workers are up.

A reboot therefore restores the service automatically without losing future
voucher actions or firing stale claims.

## Host requirements

- Modest RAM/CPU (personal single-account workload).
- Host NTP synchronization is an operational dependency — the scheduler's
  precision window relies on a correct wall clock (internal timestamps are UTC;
  display is `Asia/Ho_Chi_Minh`).
- Container logging (JSON to stdout) or a mounted log dir with rotation.

## Backup / restore

- `docker/backup.sh [dir]` — gzipped `pg_dump`, keeps the newest 14.
- `docker/restore.sh <file>` — restores into the running postgres service.
- See [disaster-recovery.md](disaster-recovery.md) for the full runbook and the
  required restore exercise.

## Upgrade / rollback

1. `docker/backup.sh` first.
2. Build and tag the new image; `docker compose up -d` (migrations run on start).
3. If a risky integration changed, start with `ENABLE_AUTO_CLAIM=false`, verify
   collectors + session, then re-enable.
4. Rollback = redeploy the previous image tag; migrations are forward-only, so
   avoid destructive migrations without a tested down path.
