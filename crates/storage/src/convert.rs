//! Row-value conversion helpers for the portable TEXT schema.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::error::StorageError;

pub fn ts_to_str(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
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
