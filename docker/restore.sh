#!/usr/bin/env bash
# Restore a shopee-hunter PostgreSQL backup produced by backup.sh.
# Usage: docker/restore.sh <backup-file.sql.gz>
# Restores into the running postgres service. Start the app with claims
# DISABLED (ENABLE_AUTO_CLAIM=false) until you have verified state.
set -euo pipefail

FILE="${1:?usage: restore.sh <backup-file.sql.gz>}"
[ -f "$FILE" ] || { echo "no such file: $FILE" >&2; exit 1; }

echo "restoring $FILE into postgres ..."
gunzip -c "$FILE" | docker compose exec -T postgres \
  psql -U shopee_hunter -d shopee_hunter -v ON_ERROR_STOP=1

echo "restore complete. Recommended next steps:"
echo "  1) start app with ENABLE_AUTO_CLAIM=false"
echo "  2) verify /health/ready and /admin/jobs"
echo "  3) re-bootstrap or verify session, then re-enable claims"
