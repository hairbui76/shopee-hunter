//! Owner-rule filtering, kept strictly separate from scoring.
//!
//! Scoring answers "how useful is this?"; eligibility answers "does this pass
//! the owner's hard rules?" (ARCHITECTURE.md §11). A voucher can score 90 and
//! still be denied because it is locked to an excluded shop.
//!
//! # Deliberately out of scope here
//!
//! * **Time.** No `now` parameter, so expiry and activation are not judged —
//!   those belong to the scheduler and the claim policy engine.
//! * **Session and platform state.** Session health, retry budgets, and the
//!   `ENABLE_AUTO_CLAIM` flag are the claim policy engine's inputs (Phase 14).
//! * **Score thresholds.** Use [`crate::passes_notification_threshold`] and
//!   [`crate::meets_auto_claim_threshold`]; they read the numeric score, which
//!   this function deliberately does not consume.
//!
//! Every decision carries human-readable reasons, and denials report **all**
//! failing rules so the owner sees the whole picture at once.

use serde::{Deserialize, Serialize};
use shopee_hunter_domain::{Voucher, VoucherScope, VoucherStatus};

use crate::rules::{contains_ignore_case, UserRules};
use crate::score::{effective_discount, format_vnd};

/// Outcome of applying the owner's hard rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Eligibility {
    /// No rule rejected the voucher.
    Allow {
        /// Why it passed.
        reasons: Vec<String>,
    },
    /// At least one rule rejected it; every failing rule is listed.
    Deny {
        /// Why it failed.
        reasons: Vec<String>,
    },
}

impl Eligibility {
    /// Whether the voucher passed every rule.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// Reasons behind the decision, whichever way it went.
    pub fn reasons(&self) -> &[String] {
        match self {
            Self::Allow { reasons } | Self::Deny { reasons } => reasons,
        }
    }

    /// Whether any reason contains `needle` (case-sensitive).
    pub fn has_reason(&self, needle: &str) -> bool {
        self.reasons().iter().any(|r| r.contains(needle))
    }
}

/// Apply the owner's hard rules to a voucher.
///
/// Rule summary:
///
/// | Rule | Denies when |
/// |---|---|
/// | terminal status | already saved, exhausted, expired, or ineligible |
/// | `excluded_shops` | shop-scoped voucher whose shop is excluded |
/// | `excluded_categories` | category-scoped voucher whose category is excluded |
/// | `excluded_payment_methods` | voucher requires an excluded payment method |
/// | `min_discount` | known value below the minimum, or value unknowable |
/// | `max_required_spend` | stated minimum spend above the maximum |
///
/// `preferred_voucher_types` is intentionally **not** a filter: it is a soft
/// preference that only adds score, so narrowing it can never silently hide
/// vouchers.
pub fn eligibility(voucher: &Voucher, rules: &UserRules) -> Eligibility {
    let mut denials: Vec<String> = Vec::new();
    let mut approvals: Vec<String> = Vec::new();

    check_status(voucher, &mut denials);
    check_exclusions(voucher, rules, &mut denials, &mut approvals);
    check_min_discount(voucher, rules, &mut denials, &mut approvals);
    check_max_required_spend(voucher, rules, &mut denials, &mut approvals);

    if denials.is_empty() {
        if approvals.is_empty() {
            approvals.push("no owner rule applies".to_string());
        }
        Eligibility::Allow { reasons: approvals }
    } else {
        Eligibility::Deny { reasons: denials }
    }
}

/// A voucher in a terminal state cannot become useful again.
fn check_status(voucher: &Voucher, denials: &mut Vec<String>) {
    if matches!(
        voucher.status,
        VoucherStatus::Saved
            | VoucherStatus::Exhausted
            | VoucherStatus::Expired
            | VoucherStatus::Ineligible
    ) {
        denials.push(format!(
            "voucher status is {} (terminal)",
            voucher.status.as_str()
        ));
    }
}

fn check_exclusions(
    voucher: &Voucher,
    rules: &UserRules,
    denials: &mut Vec<String>,
    approvals: &mut Vec<String>,
) {
    match &voucher.scope {
        Some(VoucherScope::Shop { shop_id }) => {
            if contains_ignore_case(&rules.excluded_shops, shop_id) {
                denials.push(format!("shop {shop_id} is excluded"));
            }
        }
        Some(VoucherScope::Category { name }) => {
            if contains_ignore_case(&rules.excluded_categories, name) {
                denials.push(format!("category {name} is excluded"));
            }
        }
        _ => {}
    }

    // A payment requirement can arrive either as the scope or as the dedicated
    // field; both must be checked or an exclusion is trivially bypassed.
    let payment_method = match &voucher.scope {
        Some(VoucherScope::Payment { method }) => Some(method.as_str()),
        _ => voucher.payment_method.as_deref(),
    };
    if let Some(method) = payment_method {
        if contains_ignore_case(&rules.excluded_payment_methods, method) {
            denials.push(format!("payment method {method} is excluded"));
        } else if !rules.excluded_payment_methods.is_empty() {
            approvals.push(format!("payment method {method} is not excluded"));
        }
    }
}

fn check_min_discount(
    voucher: &Voucher,
    rules: &UserRules,
    denials: &mut Vec<String>,
    approvals: &mut Vec<String>,
) {
    let Some(minimum) = rules.min_discount else {
        return;
    };
    match effective_discount(voucher) {
        Some(value) if value >= minimum => approvals.push(format!(
            "discount {} meets the {} minimum",
            format_vnd(value),
            format_vnd(minimum)
        )),
        Some(value) => denials.push(format!(
            "discount {} is below the {} minimum",
            format_vnd(value),
            format_vnd(minimum)
        )),
        // Conservative on purpose: the owner asked for a floor, and an unknown
        // value cannot be shown to clear it. Percentage-only vouchers with no
        // published cap land here, which is the honest outcome.
        None => denials.push(format!(
            "discount value is unknown, cannot prove it meets the {} minimum",
            format_vnd(minimum)
        )),
    }
}

fn check_max_required_spend(
    voucher: &Voucher,
    rules: &UserRules,
    denials: &mut Vec<String>,
    approvals: &mut Vec<String>,
) {
    let Some(maximum) = rules.max_required_spend else {
        return;
    };
    // Asymmetric with `min_discount` by design: a missing `min_spend` almost
    // always means the voucher has no spend requirement, so absence is treated
    // as "nothing required" rather than "unprovable".
    match voucher.min_spend.filter(|s| s.is_sign_positive()) {
        Some(spend) if spend > maximum => denials.push(format!(
            "minimum spend {} exceeds the {} maximum",
            format_vnd(spend),
            format_vnd(maximum)
        )),
        Some(spend) => approvals.push(format!(
            "minimum spend {} is within the {} maximum",
            format_vnd(spend),
            format_vnd(maximum)
        )),
        None => approvals.push("no minimum spend required".to_string()),
    }
}
