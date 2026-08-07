//! Owner-configured preferences, and the thresholds that turn a score into an
//! action.
//!
//! These are *preferences*, not platform facts. Nothing here talks to Shopee,
//! reads a clock, or touches storage — `UserRules` is plain configuration that
//! both [`crate::score`] and [`crate::eligibility`] read.

use std::collections::BTreeSet;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use shopee_hunter_domain::{SourceId, VoucherType};

/// Default score at or above which a voucher is worth telling the owner about.
pub const DEFAULT_NOTIFICATION_THRESHOLD: i64 = 25;

/// Default score at or above which an auto-claim may be considered.
///
/// Deliberately well above the notification threshold: surfacing a mediocre
/// voucher costs a message, claiming one costs an account action.
pub const DEFAULT_AUTO_CLAIM_THRESHOLD: i64 = 60;

/// Owner preferences driving ranking and eligibility.
///
/// `#[serde(default)]` means a partial config file is valid and every omitted
/// field falls back to [`UserRules::default`], so adding a preference later
/// cannot break an existing deployment's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserRules {
    /// Smallest absolute discount worth acting on. `None` disables the filter.
    pub min_discount: Option<Decimal>,
    /// Largest minimum-spend the owner is willing to meet. `None` disables the
    /// filter.
    pub max_required_spend: Option<Decimal>,
    /// Voucher types the owner cares about most.
    ///
    /// A **soft** preference: listed types gain score, unlisted types are not
    /// denied. Use the exclusion sets below for hard filtering. A `Vec` rather
    /// than a set so the owner's ordering round-trips through config unchanged.
    pub preferred_voucher_types: Vec<VoucherType>,
    /// Payment methods to reject outright (e.g. a wallet the owner does not use).
    pub excluded_payment_methods: BTreeSet<String>,
    /// Shop ids to reject outright.
    pub excluded_shops: BTreeSet<String>,
    /// Category names to reject outright.
    pub excluded_categories: BTreeSet<String>,
    /// Discovery sources whose data the owner trusts; scored as a bonus.
    pub trusted_sources: BTreeSet<SourceId>,
    /// Score at or above which the owner wants a notification.
    pub notification_threshold: i64,
    /// Score at or above which auto-claim may be considered.
    ///
    /// Reaching it is necessary, never sufficient: the claim policy engine
    /// (Phase 14) still owns the decision.
    pub auto_claim_threshold: i64,
}

impl Default for UserRules {
    fn default() -> Self {
        Self {
            min_discount: None,
            max_required_spend: None,
            preferred_voucher_types: Vec::new(),
            excluded_payment_methods: BTreeSet::new(),
            excluded_shops: BTreeSet::new(),
            excluded_categories: BTreeSet::new(),
            trusted_sources: BTreeSet::new(),
            notification_threshold: DEFAULT_NOTIFICATION_THRESHOLD,
            auto_claim_threshold: DEFAULT_AUTO_CLAIM_THRESHOLD,
        }
    }
}

/// A configuration mistake that would make ranking behave surprisingly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    /// Auto-claiming vouchers the owner would not even be told about.
    #[error(
        "auto_claim_threshold ({auto_claim}) must be >= notification_threshold ({notification})"
    )]
    ThresholdsInverted {
        /// Configured notification threshold.
        notification: i64,
        /// Configured auto-claim threshold.
        auto_claim: i64,
    },
    /// A monetary preference was negative.
    #[error("{field} must not be negative")]
    NegativeAmount {
        /// Offending field name.
        field: &'static str,
    },
}

impl UserRules {
    /// Validate the configuration, reporting **all** problems rather than the
    /// first, so a startup failure lists everything the owner must fix.
    pub fn validate(&self) -> Result<(), Vec<RulesError>> {
        let mut issues = Vec::new();

        if self.auto_claim_threshold < self.notification_threshold {
            issues.push(RulesError::ThresholdsInverted {
                notification: self.notification_threshold,
                auto_claim: self.auto_claim_threshold,
            });
        }
        if self.min_discount.is_some_and(|d| d.is_sign_negative()) {
            issues.push(RulesError::NegativeAmount {
                field: "min_discount",
            });
        }
        if self
            .max_required_spend
            .is_some_and(|d| d.is_sign_negative())
        {
            issues.push(RulesError::NegativeAmount {
                field: "max_required_spend",
            });
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    /// Whether a voucher type is one the owner prefers.
    pub fn prefers_type(&self, voucher_type: VoucherType) -> bool {
        self.preferred_voucher_types.contains(&voucher_type)
    }

    /// Whether a discovery source is trusted.
    pub fn trusts_source(&self, source: &SourceId) -> bool {
        self.trusted_sources.contains(source)
    }
}

/// Whether a score clears the owner's notification threshold.
pub fn passes_notification_threshold(score: i64, rules: &UserRules) -> bool {
    score >= rules.notification_threshold
}

/// Whether a score clears the owner's auto-claim threshold.
///
/// Necessary but **not** sufficient: eligibility, session health, and the claim
/// policy engine all still get a veto.
pub fn meets_auto_claim_threshold(score: i64, rules: &UserRules) -> bool {
    score >= rules.auto_claim_threshold
}

/// Case-insensitive membership test for the exclusion sets.
///
/// Sets are tiny, so a linear scan keeps behaviour deterministic and handles
/// non-ASCII (Vietnamese category names) correctly, which a `BTreeSet::contains`
/// on raw keys would not.
pub(crate) fn contains_ignore_case(set: &BTreeSet<String>, value: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    let needle = value.trim().to_lowercase();
    set.iter()
        .any(|entry| entry.trim().to_lowercase() == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane_and_valid() {
        let rules = UserRules::default();
        assert!(rules.validate().is_ok());
        assert!(rules.auto_claim_threshold > rules.notification_threshold);
        assert!(rules.preferred_voucher_types.is_empty());
    }

    #[test]
    fn thresholds_are_inclusive_and_independent() {
        let rules = UserRules {
            notification_threshold: 25,
            auto_claim_threshold: 60,
            ..UserRules::default()
        };
        assert!(!passes_notification_threshold(24, &rules));
        assert!(passes_notification_threshold(25, &rules));
        assert!(passes_notification_threshold(1_000, &rules));

        assert!(!meets_auto_claim_threshold(59, &rules));
        assert!(meets_auto_claim_threshold(60, &rules));

        // Clearing auto-claim always implies clearing notification when the
        // configuration is valid.
        assert!(passes_notification_threshold(60, &rules));
        // Negative scores clear nothing.
        assert!(!passes_notification_threshold(-100, &rules));
    }

    #[test]
    fn validation_reports_every_problem() {
        let rules = UserRules {
            notification_threshold: 80,
            auto_claim_threshold: 10,
            min_discount: Some(Decimal::from(-5)),
            max_required_spend: Some(Decimal::from(-1)),
            ..UserRules::default()
        };
        let issues = rules.validate().expect_err("must be invalid");
        assert_eq!(issues.len(), 3);
        assert!(issues.contains(&RulesError::ThresholdsInverted {
            notification: 80,
            auto_claim: 10
        }));
        assert!(issues.contains(&RulesError::NegativeAmount {
            field: "min_discount"
        }));
    }

    #[test]
    fn partial_config_deserializes_with_defaults() {
        let rules: UserRules =
            serde_json::from_str(r#"{"notification_threshold": 40, "excluded_shops": ["12345"]}"#)
                .expect("partial config is valid");
        assert_eq!(rules.notification_threshold, 40);
        assert_eq!(rules.auto_claim_threshold, DEFAULT_AUTO_CLAIM_THRESHOLD);
        assert!(rules.excluded_shops.contains("12345"));
        assert_eq!(rules.min_discount, None);
    }

    #[test]
    fn config_round_trips_deterministically() {
        let rules = UserRules {
            preferred_voucher_types: vec![VoucherType::Platform, VoucherType::Freeship],
            trusted_sources: [SourceId::new("shopee-page")].into_iter().collect(),
            min_discount: Some(Decimal::from(50_000)),
            ..UserRules::default()
        };
        let encoded = serde_json::to_string(&rules).expect("serializes");
        let decoded: UserRules = serde_json::from_str(&encoded).expect("round trips");
        assert_eq!(decoded, rules);
        // Preference ordering is preserved, not set-reordered.
        assert_eq!(decoded.preferred_voucher_types[0], VoucherType::Platform);
        assert_eq!(
            serde_json::to_string(&rules).expect("serializes"),
            encoded,
            "serialization must be stable across calls"
        );
    }

    #[test]
    fn exclusion_matching_ignores_case_and_padding() {
        let set: BTreeSet<String> = ["ShopeePay", "Điện Tử"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(contains_ignore_case(&set, "shopeepay"));
        assert!(contains_ignore_case(&set, "  SHOPEEPAY "));
        assert!(contains_ignore_case(&set, "điện tử"));
        assert!(!contains_ignore_case(&set, "momo"));
        assert!(!contains_ignore_case(&BTreeSet::new(), "anything"));
    }
}
