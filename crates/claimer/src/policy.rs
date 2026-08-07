//! Claim policy engine. Produces an explainable `ClaimDecision` from inputs;
//! it never sends a request and never parses Shopee JSON.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::claim::ClaimDecision;
use shopee_hunter_domain::voucher::Voucher;
use shopee_hunter_domain::SessionState;

/// Everything the policy needs to decide. Assembled by the claim service from
/// the voucher, session, config, and attempt history.
pub struct PolicyInputs<'a> {
    pub voucher: &'a Voucher,
    pub session_state: SessionState,
    pub now: DateTime<Utc>,
    /// A prior attempt already succeeded (or was already-saved).
    pub already_succeeded: bool,
    pub attempts_used: u32,
    pub max_attempts: u32,
    /// `ENABLE_AUTO_CLAIM`.
    pub auto_claim_enabled: bool,
    /// Owner min-score threshold and this voucher's score (if ranked).
    pub min_score: i64,
    pub score: Option<i64>,
    /// Voucher matched an explicit exclusion rule.
    pub excluded: bool,
    /// Source is trusted enough to auto-claim from.
    pub trusted_source: bool,
}

/// Evaluate claim policy. Order matters: terminal denials first, then
/// deferrals, then the positive path. Every branch carries reasons.
pub fn evaluate(input: &PolicyInputs<'_>) -> ClaimDecision {
    let deny = |reason: &str| ClaimDecision::Deny {
        reasons: vec![reason.to_string()],
    };

    if !input.auto_claim_enabled {
        return deny("auto-claim disabled by configuration");
    }
    if input.excluded {
        return deny("voucher matches an exclusion rule");
    }
    if input.already_succeeded {
        return deny("voucher already claimed successfully");
    }
    if !input.trusted_source {
        return deny("source is not trusted for auto-claim");
    }

    // Session gating.
    if input.session_state.needs_manual_action() {
        return ClaimDecision::ManualReview {
            reasons: vec![format!(
                "session requires manual action: {}",
                input.session_state.as_str()
            )],
        };
    }
    if input.session_state.blocks_claims() {
        return ClaimDecision::Defer {
            reasons: vec![format!(
                "session not usable: {}",
                input.session_state.as_str()
            )],
            until_hint: None,
        };
    }
    if !input.session_state.allows_claims() {
        return ClaimDecision::Defer {
            reasons: vec![format!(
                "session not positively healthy: {}",
                input.session_state.as_str()
            )],
            until_hint: None,
        };
    }

    if !input.voucher.has_claim_identifiers() {
        return deny("voucher lacks required claim identifiers");
    }

    if let Some(end) = input.voucher.end_at {
        if end <= input.now {
            return deny("voucher has expired");
        }
    }
    if let Some(start) = input.voucher.start_at {
        if start > input.now {
            return ClaimDecision::Defer {
                reasons: vec!["voucher is not active yet".to_string()],
                until_hint: Some(start),
            };
        }
    }

    if input.attempts_used >= input.max_attempts {
        return deny("retry budget exhausted");
    }

    if let Some(score) = input.score {
        if score < input.min_score {
            return deny(&format!("score {score} below minimum {}", input.min_score));
        }
    }

    let mut reasons = vec![
        "auto-claim enabled".to_string(),
        "session healthy".to_string(),
        "claim identifiers present".to_string(),
        "within time window".to_string(),
    ];
    if let Some(score) = input.score {
        reasons.push(format!("score {score} >= {}", input.min_score));
    }
    ClaimDecision::Allow { reasons }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherType};
    use shopee_hunter_domain::SourceId;

    fn voucher_with(
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        with_ids: bool,
    ) -> Voucher {
        let candidate = VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k".into(),
            external_id: Some("x".into()),
            code: if with_ids { Some("CODE".into()) } else { None },
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
        Voucher::from_candidate(&candidate, Utc::now())
    }

    fn base<'a>(v: &'a Voucher, now: DateTime<Utc>) -> PolicyInputs<'a> {
        PolicyInputs {
            voucher: v,
            session_state: SessionState::Healthy,
            now,
            already_succeeded: false,
            attempts_used: 0,
            max_attempts: 3,
            auto_claim_enabled: true,
            min_score: 0,
            score: Some(50),
            excluded: false,
            trusted_source: true,
        }
    }

    #[test]
    fn allows_when_all_conditions_met() {
        let now = Utc::now();
        let v = voucher_with(
            Some(now - chrono::Duration::hours(1)),
            Some(now + chrono::Duration::hours(1)),
            true,
        );
        assert!(evaluate(&base(&v, now)).is_allow());
    }

    #[test]
    fn denies_when_auto_claim_disabled() {
        let now = Utc::now();
        let v = voucher_with(None, None, true);
        let mut i = base(&v, now);
        i.auto_claim_enabled = false;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));
    }

    #[test]
    fn session_verification_needs_manual_review() {
        let now = Utc::now();
        let v = voucher_with(None, None, true);
        let mut i = base(&v, now);
        i.session_state = SessionState::VerificationRequired;
        assert!(matches!(evaluate(&i), ClaimDecision::ManualReview { .. }));
    }

    #[test]
    fn expired_session_defers() {
        let now = Utc::now();
        let v = voucher_with(None, None, true);
        let mut i = base(&v, now);
        i.session_state = SessionState::Expired;
        assert!(matches!(evaluate(&i), ClaimDecision::Defer { .. }));
    }

    #[test]
    fn not_active_defers_until_start() {
        let now = Utc::now();
        let start = now + chrono::Duration::hours(2);
        let v = voucher_with(Some(start), Some(now + chrono::Duration::hours(3)), true);
        match evaluate(&base(&v, now)) {
            ClaimDecision::Defer { until_hint, .. } => assert_eq!(until_hint, Some(start)),
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn missing_identifiers_and_expiry_and_budget_deny() {
        let now = Utc::now();
        // Missing identifiers.
        let v = voucher_with(None, None, false);
        assert!(matches!(
            evaluate(&base(&v, now)),
            ClaimDecision::Deny { .. }
        ));

        // Expired.
        let v = voucher_with(None, Some(now - chrono::Duration::minutes(1)), true);
        assert!(matches!(
            evaluate(&base(&v, now)),
            ClaimDecision::Deny { .. }
        ));

        // Budget exhausted.
        let v = voucher_with(None, None, true);
        let mut i = base(&v, now);
        i.attempts_used = 3;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));
    }

    #[test]
    fn below_min_score_and_excluded_and_already_saved_deny() {
        let now = Utc::now();
        let v = voucher_with(None, None, true);

        let mut i = base(&v, now);
        i.score = Some(10);
        i.min_score = 50;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));

        let mut i = base(&v, now);
        i.excluded = true;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));

        let mut i = base(&v, now);
        i.already_succeeded = true;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));
    }

    #[test]
    fn untrusted_source_denies() {
        let now = Utc::now();
        let v = voucher_with(None, None, true);
        let mut i = base(&v, now);
        i.trusted_source = false;
        assert!(matches!(evaluate(&i), ClaimDecision::Deny { .. }));
    }
}
