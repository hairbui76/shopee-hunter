//! Row-value conversion helpers for the portable TEXT schema.

use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::error::StorageError;

/// Serialize a UTC timestamp as RFC3339 with a FIXED microsecond width.
///
/// The schema compares and orders timestamps as TEXT (`execute_at < $1`,
/// `ORDER BY execute_at`). `to_rfc3339()` emits a variable fractional width,
/// which breaks lexicographic == chronological ordering (`Z` > `.`), so a
/// fixed-width microsecond form is required for the scheduler to fire in the
/// right order.
pub fn ts_to_str(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub fn opt_ts_to_str(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(ts_to_str)
}

pub fn str_to_ts(s: &str, field: &'static str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StorageError::Decode {
            field,
            reason: e.to_string(),
        })
}

pub fn opt_str_to_ts(
    s: Option<String>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    s.as_deref().map(|v| str_to_ts(v, field)).transpose()
}

pub fn dec_to_str(d: Option<Decimal>) -> Option<String> {
    d.map(|v| v.normalize().to_string())
}

pub fn str_to_dec(s: Option<String>, field: &'static str) -> Result<Option<Decimal>, StorageError> {
    match s {
        None => Ok(None),
        Some(v) => Decimal::from_str(&v)
            .map(Some)
            .map_err(|e| StorageError::Decode {
                field,
                reason: e.to_string(),
            }),
    }
}

pub fn uuid_to_str(id: Uuid) -> String {
    id.to_string()
}

pub fn str_to_uuid(s: &str, field: &'static str) -> Result<Uuid, StorageError> {
    Uuid::from_str(s).map_err(|e| StorageError::Decode {
        field,
        reason: e.to_string(),
    })
}
