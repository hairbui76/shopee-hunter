-- Initial schema. Written in portable SQL that applies identically on SQLite
-- (dev/replay) and PostgreSQL (prod): TEXT for timestamps (RFC3339 UTC),
-- decimals, UUIDs, and enum codes; INTEGER for booleans and counters.

CREATE TABLE IF NOT EXISTS vouchers (
    id                TEXT PRIMARY KEY,
    identity_key      TEXT NOT NULL UNIQUE,
    identity_basis    TEXT NOT NULL,
    source            TEXT NOT NULL,
    source_key        TEXT NOT NULL,
    code              TEXT,
    promotion_id      TEXT,
    signature         TEXT,
    title             TEXT NOT NULL,
    description       TEXT,
    voucher_type      TEXT NOT NULL,
    discount_type     TEXT,
    discount_amount   TEXT,
    discount_percent  TEXT,
    max_discount      TEXT,
    min_spend         TEXT,
    start_at          TEXT,
    end_at            TEXT,
    scope             TEXT,
    payment_method    TEXT,
    landing_url       TEXT,
    status            TEXT NOT NULL,
    first_seen_at     TEXT NOT NULL,
    last_seen_at      TEXT NOT NULL,
    version_hash      TEXT NOT NULL,
    raw_hash          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_vouchers_status ON vouchers(status);
CREATE INDEX IF NOT EXISTS idx_vouchers_start_at ON vouchers(start_at);

-- Every source observation of a voucher (same voucher may be seen repeatedly
-- or via multiple sources). Append-oriented; unique per source item version.
CREATE TABLE IF NOT EXISTS voucher_observations (
    id                TEXT PRIMARY KEY,
    voucher_id        TEXT NOT NULL REFERENCES vouchers(id) ON DELETE CASCADE,
    source            TEXT NOT NULL,
    source_key        TEXT NOT NULL,
    observed_at       TEXT NOT NULL,
    source_updated_at TEXT,
    raw_hash          TEXT NOT NULL,
    normalized_hash   TEXT NOT NULL,
    raw_payload       TEXT,
    parser_version    TEXT NOT NULL,
    UNIQUE(source, source_key, normalized_hash)
);
CREATE INDEX IF NOT EXISTS idx_observations_voucher ON voucher_observations(voucher_id);

-- Version history: distinct normalized versions of a logical voucher.
CREATE TABLE IF NOT EXISTS voucher_versions (
    id              TEXT PRIMARY KEY,
    voucher_id      TEXT NOT NULL REFERENCES vouchers(id) ON DELETE CASCADE,
    version_hash    TEXT NOT NULL,
    changed_fields  TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE(voucher_id, version_hash)
);

CREATE TABLE IF NOT EXISTS collector_runs (
    id              TEXT PRIMARY KEY,
    source          TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    latency_ms      INTEGER,
    candidate_count INTEGER NOT NULL DEFAULT 0,
    new_count       INTEGER NOT NULL DEFAULT 0,
    updated_count   INTEGER NOT NULL DEFAULT 0,
    parse_errors    INTEGER NOT NULL DEFAULT 0,
    outcome         TEXT NOT NULL,
    detail          TEXT
);
CREATE INDEX IF NOT EXISTS idx_collector_runs_source ON collector_runs(source, started_at);

CREATE TABLE IF NOT EXISTS schedule_jobs (
    id                TEXT PRIMARY KEY,
    voucher_id        TEXT NOT NULL REFERENCES vouchers(id) ON DELETE CASCADE,
    action            TEXT NOT NULL,
    execute_at        TEXT NOT NULL,
    preflight_at      TEXT NOT NULL,
    status            TEXT NOT NULL,
    scheduler_version INTEGER NOT NULL DEFAULT 1,
    attempt_count     INTEGER NOT NULL DEFAULT 0,
    last_result       TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    -- One open job per (voucher, action): prevents duplicate scheduling.
    UNIQUE(voucher_id, action)
);
CREATE INDEX IF NOT EXISTS idx_schedule_jobs_due ON schedule_jobs(status, execute_at);

CREATE TABLE IF NOT EXISTS claim_attempts (
    id              TEXT PRIMARY KEY,
    voucher_id      TEXT NOT NULL REFERENCES vouchers(id) ON DELETE CASCADE,
    schedule_job_id TEXT REFERENCES schedule_jobs(id) ON DELETE SET NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    request_version TEXT,
    result_class    TEXT,
    upstream_status INTEGER,
    latency_ms      INTEGER,
    retry_index     INTEGER NOT NULL DEFAULT 0,
    diagnostic_code TEXT
);
CREATE INDEX IF NOT EXISTS idx_claim_attempts_voucher ON claim_attempts(voucher_id, started_at);

CREATE TABLE IF NOT EXISTS notification_outbox (
    id              TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    event_kind      TEXT NOT NULL,
    payload         TEXT NOT NULL,
    status          TEXT NOT NULL,
    attempt_count   INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    next_attempt_at TEXT NOT NULL,
    last_error      TEXT
);
CREATE INDEX IF NOT EXISTS idx_outbox_pending ON notification_outbox(status, next_attempt_at);

CREATE TABLE IF NOT EXISTS service_health_events (
    id          TEXT PRIMARY KEY,
    service     TEXT NOT NULL,
    from_state  TEXT,
    to_state    TEXT NOT NULL,
    reason      TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_health_events_service ON service_health_events(service, created_at);
