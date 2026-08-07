# Disaster recovery (ROADMAP Phase 35)

The service integrates unstable external behavior; the goal is to rebuild it
after VPS loss without relying on undocumented manual knowledge.

## Backup policy

Back up:

- **PostgreSQL** — `docker/backup.sh` (gzipped `pg_dump`, 14 kept). Schedule via
  cron; store off-host.
- **Configuration templates** — `.env.example`, compose, migrations (in git).
  Never back up the real `.env` in plaintext.
- **Operational metadata** — this runbook and `docs/`.
- **Browser profile / session** — OPTIONAL and only if you accept the risk.
  If backed up, it MUST be encrypted; it contains live session material.

## Recovery runbook

1. Provision a fresh VPS; install Docker + Compose.
2. Restore config: clone the repo, create `.env` from `.env.example` with real
   secrets from your secret store.
3. `docker compose up -d postgres` and wait for it to become healthy.
4. `docker/restore.sh <latest-backup.sql.gz>`.
5. Deploy the app image with **claims disabled** (`ENABLE_AUTO_CLAIM=false`).
6. Restore or re-bootstrap the Shopee session
   (`cargo run -p shopee-hunter-tools --bin login_session --features browser`).
7. Verify `/health/ready`, `/admin/session`, and `/admin/jobs`.
8. Confirm scheduler reconstruction (future jobs present, stale jobs marked).
9. Re-enable claims (`ENABLE_AUTO_CLAIM=true`) once session is HEALTHY.

## Recovery exercise (required)

Perform at least one test restore into a scratch environment:

```bash
docker/backup.sh ./backups
# in a scratch compose project:
docker/restore.sh ./backups/shopee-hunter-db-<stamp>.sql.gz
```

Verify row counts for `vouchers`, `schedule_jobs`, and `claim_attempts` match
the source, and that the app starts `ready` against the restored DB. Record the
date of the last successful exercise here:

- Last restore exercise: _not yet run_ (fill in after the first exercise).
