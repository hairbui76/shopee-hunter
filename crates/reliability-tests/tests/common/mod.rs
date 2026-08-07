//! Shared fixtures for the failure-injection scenarios.
//!
//! Every helper builds *real* runtime objects — a real migrated SQLite
//! database, real domain candidates — so the scenarios exercise production
//! code paths rather than stand-ins.

#![allow(dead_code)]

use chrono::{DateTime, Duration, TimeZone, Utc};
use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherScope, VoucherType};
use shopee_hunter_domain::SourceId;
use shopee_hunter_storage::Database;

/// A migrated SQLite database in a temp dir.
///
/// The `TempDir` must stay alive for the duration of the test: dropping it
/// deletes the database file out from under the pool.
pub async fn temp_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("reliability.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url, 4)
        .await
        .expect("connect and migrate temp database");
    (db, dir)
}

/// A fixed timestamp, so nothing in a scenario depends on wall-clock drift.
pub fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
        .single()
        .expect("fixed timestamp is unambiguous")
}

/// A minimal valid candidate, keyed by `external_id` so identity is stable
/// and scoped to `source`.
pub fn candidate(source: &str, key: &str) -> VoucherCandidate {
    VoucherCandidate {
        source: SourceId::new(source),
        source_key: key.to_string(),
        external_id: Some(key.to_string()),
        code: Some(format!("CODE-{key}")),
        promotion_id: None,
        signature: None,
        title: format!("Voucher {key}"),
        description: None,
        voucher_type: VoucherType::Platform,
        discount_type: None,
        discount_amount: None,
        discount_percent: None,
        max_discount: None,
        min_spend: None,
        start_at: None,
        end_at: None,
        scope: Some(VoucherScope::Platform),
        payment_method: None,
        landing_url: None,
        raw_payload: serde_json::json!({ "key": key }),
        observed_at: base_time(),
        parser_version: "reliability-test/1".into(),
    }
}

/// A candidate that satisfies every claim-policy precondition: full
/// identifiers, already active, not yet expired. Used by scenarios that need
/// policy to reach `Allow` so the *failure* under test is the thing that stops
/// the claim, not a missing precondition.
pub fn claimable_candidate(source: &str, key: &str) -> VoucherCandidate {
    let mut candidate = candidate(source, key);
    candidate.promotion_id = Some(format!("promo-{key}"));
    candidate.signature = Some("signature".into());
    // Anchored to the live clock, NOT `base_time()`: claim policy compares the
    // window against `Utc::now()`, so a fixed timestamp would make the voucher
    // read as not-yet-active or expired depending on when the suite runs.
    candidate.start_at = Some(Utc::now() - Duration::hours(1));
    candidate.end_at = Some(Utc::now() + Duration::days(7));
    candidate
}

/// A feed body in the shape `ExternalFeedCollector` expects.
pub fn feed_body(ids: &[&str]) -> String {
    let vouchers: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "code": format!("CODE-{id}"),
                "promotion_id": format!("promo-{id}"),
                "title": format!("Voucher {id}"),
                "type": "PLATFORM",
                "discount_amount": 50000.0,
                "min_spend": 100000.0,
                "scope": "platform",
            })
        })
        .collect();
    serde_json::json!({ "vouchers": vouchers }).to_string()
}
