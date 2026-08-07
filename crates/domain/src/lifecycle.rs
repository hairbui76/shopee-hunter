//! Voucher lifecycle intelligence (ROADMAP Phase 18): derive the current
//! lifecycle stage from status + time window, and diff two versions of the
//! same logical voucher into a set of meaningful changes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::voucher::{Voucher, VoucherStatus};

/// Observable lifecycle stage, richer than raw status: distinguishes upcoming
/// vs active vs expired using the time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifecycleStage {
    FirstSeen,
    Changed,
    Upcoming,
    Active,
    Saved,
    Exhausted,
    Expired,
    Ineligible,
}

/// Derive the lifecycle stage of a voucher at `now`.
pub fn derive_stage(voucher: &Voucher, now: DateTime<Utc>) -> LifecycleStage {
    match voucher.status {
        VoucherStatus::Saved => return LifecycleStage::Saved,
        VoucherStatus::Exhausted => return LifecycleStage::Exhausted,
        VoucherStatus::Ineligible => return LifecycleStage::Ineligible,
        VoucherStatus::Expired => return LifecycleStage::Expired,
        _ => {}
    }
    if let Some(end) = voucher.end_at {
        if end <= now {
            return LifecycleStage::Expired;
        }
    }
    if let Some(start) = voucher.start_at {
        if start > now {
            return LifecycleStage::Upcoming;
        }
        return LifecycleStage::Active;
    }
    // No window: treat first observation as first-seen, later as active.
    if voucher.first_seen_at == voucher.last_seen_at {
        LifecycleStage::FirstSeen
    } else {
        LifecycleStage::Active
    }
}

/// Whether a voucher becomes active within `lead` of `now` (drives the
/// upcoming-voucher notification/scheduling).
pub fn is_upcoming_within(voucher: &Voucher, now: DateTime<Utc>, lead: chrono::Duration) -> bool {
    match voucher.start_at {
        Some(start) => start > now && start - now <= lead,
        None => false,
    }
}

/// A single meaningful change between two versions of the same voucher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: &'static str,
    pub from: Option<String>,
    pub to: Option<String>,
}

fn opt_str<T: ToString>(v: &Option<T>) -> Option<String> {
    v.as_ref().map(|x| x.to_string())
}

/// Diff the meaningful fields of two versions of the same logical voucher.
pub fn diff_versions(old: &Voucher, new: &Voucher) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    macro_rules! cmp {
        ($field:ident, $name:literal) => {
            if old.$field != new.$field {
                changes.push(FieldChange {
                    field: $name,
                    from: opt_str(&old.$field),
                    to: opt_str(&new.$field),
                });
            }
        };
    }
    cmp!(start_at, "start_at");
    cmp!(end_at, "end_at");
    cmp!(min_spend, "min_spend");
    cmp!(discount_amount, "discount_amount");
    cmp!(discount_percent, "discount_percent");
    cmp!(max_discount, "max_discount");
    if old.scope != new.scope {
        changes.push(FieldChange {
            field: "scope",
            from: old.scope.as_ref().map(|s| s.canonical_string()),
            to: new.scope.as_ref().map(|s| s.canonical_string()),
        });
    }
    // Signature becoming present is a meaningful enrichment.
    if old.signature != new.signature {
        changes.push(FieldChange {
            field: "signature",
            from: old.signature.as_ref().map(|_| "***".to_string()),
            to: new.signature.as_ref().map(|_| "***".to_string()),
        });
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SourceId;
    use crate::voucher::{VoucherCandidate, VoucherType};
    use rust_decimal::Decimal;

    fn voucher(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Voucher {
        let c = VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k".into(),
            external_id: Some("x".into()),
            code: Some("C".into()),
            promotion_id: None,
            signature: None,
            title: "V".into(),
            description: None,
            voucher_type: VoucherType::Platform,
            discount_type: None,
            discount_amount: None,
            discount_percent: None,
            max_discount: None,
            min_spend: None,
            start_at: start,
            end_at: end,
            scope: None,
            payment_method: None,
            landing_url: None,
            raw_payload: serde_json::Value::Null,
            observed_at: Utc::now(),
            parser_version: "t".into(),
        };
        Voucher::from_candidate(&c, Utc::now())
    }

    #[test]
    fn stage_reflects_window_and_status() {
        let now = Utc::now();
        let upcoming = voucher(
            Some(now + chrono::Duration::hours(2)),
            Some(now + chrono::Duration::hours(3)),
        );
        assert_eq!(derive_stage(&upcoming, now), LifecycleStage::Upcoming);

        let active = voucher(
            Some(now - chrono::Duration::hours(1)),
            Some(now + chrono::Duration::hours(1)),
        );
        assert_eq!(derive_stage(&active, now), LifecycleStage::Active);

        let expired = voucher(
            Some(now - chrono::Duration::hours(2)),
            Some(now - chrono::Duration::hours(1)),
        );
        assert_eq!(derive_stage(&expired, now), LifecycleStage::Expired);

        let mut saved = active.clone();
        saved.status = VoucherStatus::Saved;
        assert_eq!(derive_stage(&saved, now), LifecycleStage::Saved);
    }

    #[test]
    fn upcoming_within_lead() {
        let now = Utc::now();
        let v = voucher(Some(now + chrono::Duration::minutes(30)), None);
        assert!(is_upcoming_within(&v, now, chrono::Duration::hours(1)));
        assert!(!is_upcoming_within(&v, now, chrono::Duration::minutes(10)));
    }

    #[test]
    fn diff_lists_meaningful_changes() {
        let now = Utc::now();
        let old = voucher(Some(now), Some(now + chrono::Duration::hours(1)));
        let mut new = old.clone();
        new.min_spend = Some(Decimal::new(100_000, 0));
        new.end_at = Some(now + chrono::Duration::hours(2));

        let changes = diff_versions(&old, &new);
        let fields: Vec<_> = changes.iter().map(|c| c.field).collect();
        assert!(fields.contains(&"min_spend"));
        assert!(fields.contains(&"end_at"));
        assert!(!fields.contains(&"start_at"));
    }

    #[test]
    fn signature_diff_is_redacted() {
        let old = voucher(None, None);
        let mut new = old.clone();
        new.signature = Some("realsecret".into());
        let changes = diff_versions(&old, &new);
        let sig = changes.iter().find(|c| c.field == "signature").unwrap();
        assert_eq!(sig.to.as_deref(), Some("***"));
        assert!(!format!("{sig:?}").contains("realsecret"));
    }
}
