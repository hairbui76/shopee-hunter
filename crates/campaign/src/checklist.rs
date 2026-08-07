//! Pre-campaign operational checklist (ROADMAP Phase 30, "Operational
//! controls").
//!
//! This crate only *names* the readiness items and how serious each one is.
//! It deliberately performs no checks: it has no database handle, no session
//! manager, and no notifier. The operator control plane owns the actual
//! verification and renders these items as a report.
//!
//! Keeping the list here means the checklist is versioned with the campaign
//! model rather than living in a wiki page nobody reads at 23:50 before a sale.

use serde::{Deserialize, Serialize};

/// How badly a failed item should block the campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckSeverity {
    /// The campaign should not be relied on until this passes.
    Blocking,
    /// Degrades the campaign but does not make it useless.
    Advisory,
    /// Nothing can fail here; it is a review step for the owner.
    Informational,
}

impl CheckSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Blocking => "BLOCKING",
            Self::Advisory => "ADVISORY",
            Self::Informational => "INFORMATIONAL",
        }
    }
}

/// One readiness item.
///
/// Serialize-only, and deliberately so: the checklist is a code-owned
/// catalogue rendered by the control plane, not configuration to be read back
/// in. That is what lets the fields be `&'static str` with no allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChecklistItem {
    /// Stable machine id, safe to use as a metric label or report key.
    pub id: &'static str,
    /// Short imperative title.
    pub title: &'static str,
    /// What "passing" means, so the operator layer knows what to assert.
    pub detail: &'static str,
    pub severity: CheckSeverity,
}

/// The documented pre-campaign checklist, in the order it should be worked
/// through: authentication first, then storage, then delivery, then inputs,
/// then a review of what is already scheduled.
pub fn pre_campaign_checklist() -> Vec<ChecklistItem> {
    vec![
        ChecklistItem {
            id: "SESSION_HEALTHY",
            title: "Verify the Shopee session",
            detail: "session state is HEALTHY; no verification or re-login is pending",
            severity: CheckSeverity::Blocking,
        },
        ChecklistItem {
            id: "DATABASE_HEALTHY",
            title: "Verify database health",
            detail: "the database answers a health probe and migrations are applied",
            severity: CheckSeverity::Blocking,
        },
        ChecklistItem {
            id: "CLOCK_ACCURATE",
            title: "Check clock and NTP sync",
            detail: "system time is NTP-synchronised; scheduler accuracy depends on it",
            severity: CheckSeverity::Blocking,
        },
        ChecklistItem {
            id: "NOTIFIER_REACHABLE",
            title: "Verify the notifier",
            detail: "a test notification reaches Telegram and the outbox is draining",
            severity: CheckSeverity::Advisory,
        },
        ChecklistItem {
            id: "SOURCE_HEALTH",
            title: "Validate source health",
            detail: "every enabled collector has succeeded recently and is not rate-limited",
            severity: CheckSeverity::Advisory,
        },
        ChecklistItem {
            id: "SCHEDULED_JOBS_REVIEWED",
            title: "List scheduled voucher jobs",
            detail: "open scheduler jobs are reviewed for the campaign window",
            severity: CheckSeverity::Informational,
        },
    ]
}

/// Only the items that must pass before relying on a campaign.
pub fn blocking_items() -> Vec<ChecklistItem> {
    pre_campaign_checklist()
        .into_iter()
        .filter(|item| item.severity == CheckSeverity::Blocking)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn checklist_covers_every_documented_control() {
        let ids: BTreeSet<&str> = pre_campaign_checklist()
            .iter()
            .map(|item| item.id)
            .collect();

        for expected in [
            "SESSION_HEALTHY",
            "DATABASE_HEALTHY",
            "CLOCK_ACCURATE",
            "NOTIFIER_REACHABLE",
            "SOURCE_HEALTH",
            "SCHEDULED_JOBS_REVIEWED",
        ] {
            assert!(ids.contains(expected), "missing checklist item {expected}");
        }
        assert_eq!(ids.len(), 6, "ids must be unique");
    }

    #[test]
    fn every_item_is_renderable_and_ordered_by_urgency() {
        let items = pre_campaign_checklist();
        assert!(items
            .iter()
            .all(|item| !item.title.is_empty() && !item.detail.is_empty()));

        // Blocking items come first so a truncated report still shows them.
        let first_advisory = items
            .iter()
            .position(|item| item.severity != CheckSeverity::Blocking)
            .unwrap_or(items.len());
        assert!(items[..first_advisory]
            .iter()
            .all(|item| item.severity == CheckSeverity::Blocking));
    }

    #[test]
    fn blocking_subset_is_the_hard_gate() {
        let blocking = blocking_items();
        assert_eq!(blocking.len(), 3);
        let ids: Vec<&str> = blocking.iter().map(|item| item.id).collect();
        assert_eq!(
            ids,
            vec!["SESSION_HEALTHY", "DATABASE_HEALTHY", "CLOCK_ACCURATE"]
        );
    }

    #[test]
    fn items_serialize_for_the_control_plane() {
        let items = pre_campaign_checklist();
        let encoded = serde_json::to_string(&items).expect("serializes");
        assert!(encoded.contains("SESSION_HEALTHY"));
        assert!(encoded.contains("BLOCKING"));
        assert!(encoded.contains("NTP"));

        // The rendered shape is stable enough for a control-plane response.
        let parsed: serde_json::Value = serde_json::from_str(&encoded).expect("valid json");
        let first = &parsed[0];
        assert_eq!(first["id"], "SESSION_HEALTHY");
        assert_eq!(first["severity"], "BLOCKING");
        assert!(first["title"].is_string());
        assert!(first["detail"].is_string());
    }
}
