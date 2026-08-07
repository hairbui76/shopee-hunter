#!/usr/bin/env bash
# PostgreSQL backup for shopee-hunter (ROADMAP Phases 23/35).
# Usage: docker/backup.sh [output-dir]
# Backs up the database only. Session material (cookies, browser profile) is
# NOT included by default; back it up separately and encrypted if you accept
# the risk (see docs/disaster-recovery.md).
set -euo pipefail

OUT_DIR="${1:-./backups}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$OUT_DIR"
FILE="$OUT_DIR/shopee-hunter-db-${STAMP}.sql.gz"

# Dump from the compose postgres service; adjust service name if different.
docker compose exec -T postgres \
  pg_dump -U shopee_hunter -d shopee_hunter --no-owner --no-privileges \
  | gzip -9 > "$FILE"

echo "backup written: $FILE ($(du -h "$FILE" | cut -f1))"
# Retention: keep the newest 14 dumps.
ls -1t "$OUT_DIR"/shopee-hunter-db-*.sql.gz 2>/dev/null | tail -n +15 | xargs -r rm -f
