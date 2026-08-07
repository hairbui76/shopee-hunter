//! Boundary validation for normalized voucher candidates. Collectors call
//! this before candidates enter the persistence pipeline.

use chrono::{Datelike, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::voucher::VoucherCandidate;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum ValidationIssue {
    #[error("title is empty")]
    EmptyTitle,
    #[error("start_at is not before end_at")]
    StartAfterEnd,
    #[error("timestamp out of plausible range: {0}")]
    ImplausibleTimestamp(String),
    #[error("negative monetary value in {0}")]
    NegativeAmount(&'static str),
    #[error("discount_percent outside (0, 100]: {0}")]
    InvalidPercent(String),
    #[error("voucher code contains invalid characters: {0}")]
    MalformedCode(String),
    #[error("source_key is empty")]
    EmptySourceKey,
    #[error("parser_version is empty")]
    EmptyParserVersion,
}

/// Validate a candidate. Returns all issues, not just the first.
pub fn validate_candidate(candidate: &VoucherCandidate) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();

    if candidate.title.trim().is_empty() {
        issues.push(ValidationIssue::EmptyTitle);
    }
    if candidate.source_key.trim().is_empty() {
        issues.push(ValidationIssue::EmptySourceKey);
    }
    if candidate.parser_version.trim().is_empty() {
        issues.push(ValidationIssue::EmptyParserVersion);
    }

    if let (Some(start), Some(end)) = (candidate.start_at, candidate.end_at) {
        if start >= end {
            issues.push(ValidationIssue::StartAfterEnd);
        }
    }
    let min_plausible = Utc.with_ymd_and_hms(2015, 1, 1, 0, 0, 0).unwrap();
    for t in [candidate.start_at, candidate.end_at].into_iter().flatten() {
        if t < min_plausible || t.year() > 2100 {
            issues.push(ValidationIssue::ImplausibleTimestamp(t.to_rfc3339()));
        }
    }

    for (name, value) in [
        ("discount_amount", candidate.discount_amount),
        ("max_discount", candidate.max_discount),
        ("min_spend", candidate.min_spend),
    ] {
        if let Some(v) = value {
            if v < Decimal::ZERO {
                issues.push(ValidationIssue::NegativeAmount(name));
            }
        }
    }
    if let Some(p) = candidate.discount_percent {
        if p <= Decimal::ZERO || p > Decimal::from(100) {
            issues.push(ValidationIssue::InvalidPercent(p.to_string()));
        }
    }

    if let Some(code) = candidate.code.as_deref() {
        let trimmed = code.trim();
        if trimmed.is_empty()
            || trimmed.len() > 64
            || !trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            issues.push(ValidationIssue::MalformedCode(trimmed.to_string()));
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SourceId;
    use crate::voucher::VoucherType;

    fn base() -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k".into(),
            external_id: None,
            code: Some("SALE50".into()),
            promotion_id: None,
            signature: None,
            title: "Voucher".into(),
            description: None,
            voucher_type: VoucherType::Platform,
            discount_type: None,
            discount_amount: None,
            discount_percent: None,
            max_discount: None,
            min_spend: None,
            start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
            end_at: Some(Utc.with_ymd_and_hms(2026, 8, 11, 5, 0, 0).unwrap()),
            scope: None,
            payment_method: None,
            landing_url: None,
            raw_payload: serde_json::Value::Null,
            observed_at: Utc::now(),
            parser_version: "p1".into(),
        }
    }

    #[test]
    fn valid_candidate_passes() {
        assert!(validate_candidate(&base()).is_ok());
    }

    #[test]
    fn collects_multiple_issues() {
        let mut c = base();
        c.title = "  ".into();
        c.start_at = c.end_at;
        c.discount_percent = Some(Decimal::from(150));
        c.code = Some("has space!".into());
        let issues = validate_candidate(&c).unwrap_err();
        assert!(issues.contains(&ValidationIssue::EmptyTitle));
        assert!(issues.contains(&ValidationIssue::StartAfterEnd));
        assert!(issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::InvalidPercent(_))));
        assert!(issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::MalformedCode(_))));
    }

    #[test]
    fn implausible_epoch_zero_start_is_rejected() {
        let mut c = base();
        c.start_at = Some(Utc.timestamp_opt(0, 0).unwrap());
        let issues = validate_candidate(&c).unwrap_err();
        assert!(issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::ImplausibleTimestamp(_))));
    }

    #[test]
    fn negative_min_spend_is_rejected() {
        let mut c = base();
        c.min_spend = Some(Decimal::from(-1));
        assert!(validate_candidate(&c).is_err());
    }
}
