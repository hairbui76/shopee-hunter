//! Message rendering: `DomainEvent` → owner-facing Telegram text.
//!
//! This module is pure and I/O-free so message content can be tested without a
//! network or database (ROADMAP Phase 8, "Message formatting"). Delivery lives
//! in [`crate::telegram`]; the outbox worker only glues the two together.
//!
//! ## Safety rules encoded here
//!
//! * Messages are **plain text** — no `parse_mode`. Titles and error details
//!   come from upstream sources, and un-escaped Markdown/HTML from an
//!   untrusted string would either break the message or inject markup.
//! * Every free-text value passes through [`scrub`], which masks
//!   secret-bearing `key=value` clauses using the shared
//!   `shopee_hunter_observability::redact` rules and bounds the length.
//! * The voucher `signature` and `raw_hash` are **never** rendered: the
//!   signature is a claim credential, not user-facing information.
//! * The whole message is truncated to [`MAX_MESSAGE_CHARS`] so a pathological
//!   payload cannot be rejected by Telegram's 4096-character limit.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use shopee_hunter_domain::clock::format_display;
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::voucher::{DiscountType, Voucher};
use shopee_hunter_domain::SessionState;
use shopee_hunter_observability::redact::{is_sensitive_key, REDACTED};
use uuid::Uuid;

/// Telegram's hard limit is 4096 characters; stay well under it.
pub const MAX_MESSAGE_CHARS: usize = 3500;

/// Per-field bound for free text coming from sources or error paths.
const MAX_FREE_TEXT_CHARS: usize = 300;

/// Timezone hint appended to rendered timestamps (`Asia/Ho_Chi_Minh`).
const TZ_HINT: &str = "(GMT+7)";

/// Owner-facing message categories (ROADMAP Phase 8).
///
/// `SessionState` is not in the roadmap's list: `DomainEvent::SessionStateChanged`
/// is total over every `SessionState`, and the nine roadmap categories only
/// cover the alerting subset (expired / verification required). Recovery and
/// degradation transitions still deserve a message, so they render under this
/// non-alerting category rather than being silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageCategory {
    NewVoucher,
    VoucherUpdated,
    Upcoming,
    ClaimSuccess,
    ClaimFailure,
    SessionExpired,
    VerificationRequired,
    SourceDegraded,
    ServiceUnhealthy,
    SessionState,
}

impl MessageCategory {
    /// Stable machine label (metrics, tests, log fields).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NewVoucher => "NEW_VOUCHER",
            Self::VoucherUpdated => "VOUCHER_UPDATED",
            Self::Upcoming => "UPCOMING",
            Self::ClaimSuccess => "CLAIM_SUCCESS",
            Self::ClaimFailure => "CLAIM_FAILURE",
            Self::SessionExpired => "SESSION_EXPIRED",
            Self::VerificationRequired => "VERIFICATION_REQUIRED",
            Self::SourceDegraded => "SOURCE_DEGRADED",
            Self::ServiceUnhealthy => "SERVICE_UNHEALTHY",
            Self::SessionState => "SESSION_STATE",
        }
    }

    /// First line of the message.
    pub fn heading(&self) -> &'static str {
        match self {
            Self::NewVoucher => "NEW VOUCHER",
            Self::VoucherUpdated => "VOUCHER UPDATED",
            Self::Upcoming => "UPCOMING VOUCHER",
            Self::ClaimSuccess => "CLAIM SUCCESS",
            Self::ClaimFailure => "CLAIM FAILURE",
            Self::SessionExpired => "SESSION EXPIRED",
            Self::VerificationRequired => "VERIFICATION REQUIRED",
            Self::SourceDegraded => "SOURCE DEGRADED",
            Self::ServiceUnhealthy => "SERVICE UNHEALTHY",
            Self::SessionState => "SESSION STATE CHANGED",
        }
    }

    /// Whether this category represents a condition the owner must look at.
    pub fn is_alert(&self) -> bool {
        matches!(
            self,
            Self::ClaimFailure
                | Self::SessionExpired
                | Self::VerificationRequired
                | Self::SourceDegraded
                | Self::ServiceUnhealthy
        )
    }
}

/// A message ready to hand to a [`crate::Notifier`].
#[derive(Debug, Clone)]
pub struct RenderedMessage {
    pub category: MessageCategory,
    /// Mirrors the event's outbox key so delivery stays deduplicable.
    pub idempotency_key: String,
    pub text: String,
}

/// Which category an event renders as.
pub fn category_for(event: &DomainEvent) -> MessageCategory {
    match event {
        DomainEvent::VoucherDiscovered { .. } => MessageCategory::NewVoucher,
        DomainEvent::VoucherUpdated { .. } => MessageCategory::VoucherUpdated,
        DomainEvent::VoucherUpcoming { .. } => MessageCategory::Upcoming,
        DomainEvent::ClaimSucceeded { .. } => MessageCategory::ClaimSuccess,
        DomainEvent::ClaimFailed { .. } => MessageCategory::ClaimFailure,
        DomainEvent::SessionStateChanged { to, .. } => match to {
            SessionState::VerificationRequired => MessageCategory::VerificationRequired,
            SessionState::Expired | SessionState::LoginRequired | SessionState::Disabled => {
                MessageCategory::SessionExpired
            }
            _ => MessageCategory::SessionState,
        },
        DomainEvent::CollectorDegraded { .. } => MessageCategory::SourceDegraded,
        DomainEvent::ServiceUnhealthy { .. } => MessageCategory::ServiceUnhealthy,
    }
}

/// The voucher an event refers to, if any. The outbox worker uses this to load
/// voucher details for enrichment.
pub fn voucher_id_of(event: &DomainEvent) -> Option<Uuid> {
    match event {
        DomainEvent::VoucherDiscovered { voucher_id, .. }
        | DomainEvent::VoucherUpdated { voucher_id, .. }
        | DomainEvent::VoucherUpcoming { voucher_id, .. }
        | DomainEvent::ClaimSucceeded { voucher_id, .. }
        | DomainEvent::ClaimFailed { voucher_id, .. } => Some(*voucher_id),
        _ => None,
    }
}

/// Render without voucher details (events carry only identifiers).
pub fn render_event(event: &DomainEvent) -> RenderedMessage {
    render(event, None)
}

/// Render an event, enriched with the voucher when the caller could load it.
///
/// Enrichment is optional by design: a missing or unreadable voucher degrades
/// the message, it never blocks the notification.
pub fn render(event: &DomainEvent, voucher: Option<&Voucher>) -> RenderedMessage {
    let category = category_for(event);
    let mut lines: Vec<String> = vec![category.heading().to_string()];

    match event {
        DomainEvent::VoucherDiscovered {
            voucher_id,
            source,
            version_hash,
        } => {
            push_voucher_block(&mut lines, voucher, *voucher_id);
            lines.push(format!(
                "Source: {} | Version: {}",
                scrub(source.as_str()),
                short_hash(version_hash)
            ));
        }
        DomainEvent::VoucherUpdated {
            voucher_id,
            source,
            version_hash,
            changed_fields,
        } => {
            push_voucher_block(&mut lines, voucher, *voucher_id);
            if !changed_fields.is_empty() {
                let changed: Vec<String> = changed_fields.iter().map(|f| scrub(f)).collect();
                lines.push(format!("Changed: {}", changed.join(", ")));
            }
            lines.push(format!(
                "Source: {} | Version: {}",
                scrub(source.as_str()),
                short_hash(version_hash)
            ));
        }
        DomainEvent::VoucherUpcoming {
            voucher_id,
            starts_at,
        } => {
            push_voucher_block(&mut lines, voucher, *voucher_id);
            lines.push(format!("Starts: {}", fmt_time(*starts_at)));
        }
        DomainEvent::ClaimSucceeded {
            voucher_id,
            attempt_id,
            already_saved,
        } => {
            lines.push(if *already_saved {
                "Voucher was already saved to the account.".to_string()
            } else {
                "Voucher saved to the account.".to_string()
            });
            push_voucher_block(&mut lines, voucher, *voucher_id);
            lines.push(format!("Attempt: {}", short_id(*attempt_id)));
        }
        DomainEvent::ClaimFailed {
            voucher_id,
            attempt_id,
            result_class,
            terminal,
        } => {
            lines.push(format!("Result: {}", result_class.as_str()));
            lines.push(
                if *terminal {
                    "Terminal: no further attempts will be made."
                } else {
                    "Retry: the claimer will try again within its budget."
                }
                .to_string(),
            );
            push_voucher_block(&mut lines, voucher, *voucher_id);
            lines.push(format!("Attempt: {}", short_id(*attempt_id)));
        }
        DomainEvent::SessionStateChanged { from, to, reason } => {
            lines.push(format!("Session: {} -> {}", from.as_str(), to.as_str()));
            push_reason(&mut lines, reason);
            match category {
                MessageCategory::VerificationRequired => {
                    lines.push(
                        "Shopee requires manual verification. Automatic claiming is paused."
                            .to_string(),
                    );
                    lines.push(
                        "Action: complete the verification in the browser session, then re-check health."
                            .to_string(),
                    );
                }
                MessageCategory::SessionExpired => {
                    lines.push(
                        "Automatic claiming is paused until the session recovers.".to_string(),
                    );
                    lines.push(
                        "Action: re-authenticate with `cargo run -p shopee-hunter-tools --bin login_session`."
                            .to_string(),
                    );
                }
                _ => {}
            }
        }
        DomainEvent::CollectorDegraded { source, detail } => {
            lines.push(format!("Source: {}", scrub(source.as_str())));
            lines.push(format!("Detail: {}", scrub(detail)));
            lines.push("Other sources are unaffected.".to_string());
        }
        DomainEvent::ServiceUnhealthy { service, detail } => {
            lines.push(format!("Service: {}", scrub(service)));
            lines.push(format!("Detail: {}", scrub(detail)));
        }
    }

    RenderedMessage {
        category,
        idempotency_key: event.idempotency_key(),
        text: truncate(&lines.join("\n"), MAX_MESSAGE_CHARS),
    }
}

/// Voucher detail block, or a minimal identifier line when the voucher could
/// not be loaded.
fn push_voucher_block(lines: &mut Vec<String>, voucher: Option<&Voucher>, voucher_id: Uuid) {
    let Some(v) = voucher else {
        lines.push(format!("Voucher: {}", short_id(voucher_id)));
        return;
    };

    lines.push(scrub(&v.title));

    let mut identity = format!("Type: {}", v.voucher_type.as_str());
    if let Some(code) = v.code.as_deref() {
        identity.push_str(&format!(" | Code: {}", scrub(code)));
    }
    lines.push(identity);

    if let Some(discount) = discount_line(v) {
        lines.push(discount);
    }
    if let Some(min_spend) = v.min_spend {
        lines.push(format!(
            "Min spend: {}",
            if min_spend.is_zero() {
                "none".to_string()
            } else {
                fmt_money(min_spend)
            }
        ));
    }
    if let Some(window) = window_line(v) {
        lines.push(window);
    }
    if let Some(scope) = v.scope.as_ref() {
        lines.push(format!("Scope: {}", scrub(&scope.canonical_string())));
    }
    if let Some(method) = v.payment_method.as_deref() {
        lines.push(format!("Payment: {}", scrub(method)));
    }
    if let Some(url) = v.landing_url.as_deref() {
        lines.push(format!("Link: {}", scrub(url)));
    }
    lines.push(format!(
        "Status: {} | Claim: {}",
        v.status.as_str(),
        if v.has_claim_identifiers() {
            "identifiers ready"
        } else {
            "missing identifiers"
        }
    ));
    lines.push(format!("Voucher: {}", short_id(v.id)));
}

fn discount_line(v: &Voucher) -> Option<String> {
    if let Some(percent) = v.discount_percent {
        let mut line = format!("Discount: {}", fmt_percent(percent));
        if let Some(max) = v.max_discount {
            line.push_str(&format!(" (max {})", fmt_money(max)));
        }
        return Some(line);
    }
    if let Some(amount) = v.discount_amount {
        return Some(format!("Discount: {}", fmt_money(amount)));
    }
    if matches!(v.discount_type, Some(DiscountType::FreeShipping)) {
        return Some("Discount: free shipping".to_string());
    }
    None
}

fn window_line(v: &Voucher) -> Option<String> {
    match (v.start_at, v.end_at) {
        (Some(start), Some(end)) => Some(format!(
            "Window: {} -> {} {TZ_HINT}",
            format_display(start),
            format_display(end)
        )),
        (Some(start), None) => Some(format!("Starts: {}", fmt_time(start))),
        (None, Some(end)) => Some(format!("Ends: {}", fmt_time(end))),
        (None, None) => None,
    }
}

fn push_reason(lines: &mut Vec<String>, reason: &str) {
    let reason = scrub(reason);
    if !reason.is_empty() {
        lines.push(format!("Reason: {reason}"));
    }
}

fn fmt_time(ts: DateTime<Utc>) -> String {
    format!("{} {TZ_HINT}", format_display(ts))
}

fn short_id(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple.get(..8).unwrap_or(&simple).to_string()
}

fn short_hash(hash: &str) -> String {
    let clean: String = hash.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    clean.get(..12).unwrap_or(&clean).to_string()
}

/// Vietnamese money formatting: `50000` renders as `50.000₫`.
fn fmt_money(value: Decimal) -> String {
    let text = value.normalize().to_string();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.as_str()),
    };
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((int_part, frac)) => (int_part, Some(frac)),
        None => (digits, None),
    };

    let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (idx, ch) in int_part.chars().enumerate() {
        if idx > 0 && (int_part.len() - idx) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(ch);
    }

    match frac_part {
        Some(frac) => format!("{sign}{grouped},{frac}₫"),
        None => format!("{sign}{grouped}₫"),
    }
}

fn fmt_percent(value: Decimal) -> String {
    format!("{}%", value.normalize())
}

/// Mask secret-bearing clauses, flatten whitespace, and bound the length.
///
/// Free text reaching a notification can come from an upstream payload or an
/// error path, so it is treated as untrusted: `cookie=…`, `authorization: …`,
/// `…?token=…` and bearer tokens are replaced with `[REDACTED]` using the
/// shared sensitivity rules.
pub fn scrub(text: &str) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // `split_inclusive` keeps the delimiter, so re-joining is lossless.
    let masked: String = flattened
        .split_inclusive([';', '&', '?'])
        .map(scrub_clause)
        .collect();
    truncate(&masked, MAX_FREE_TEXT_CHARS)
}

fn scrub_clause(clause: &str) -> String {
    let trailing_delimiter = clause
        .chars()
        .next_back()
        .filter(|c| matches!(c, ';' | '&' | '?'))
        .map(|c| c.to_string())
        .unwrap_or_default();
    let body = &clause[..clause.len() - trailing_delimiter.len()];

    let masked = match body.find(['=', ':']) {
        Some(delimiter) => {
            let head = &body[..delimiter];
            // The key is the last whitespace-delimited token before the
            // delimiter, so surrounding prose is preserved.
            let key_start = head
                .rfind(char::is_whitespace)
                .map(|i| i + 1)
                .unwrap_or_default();
            let key = head[key_start..].trim();
            if !key.is_empty() && is_sensitive_key(key) {
                format!("{}{key}={REDACTED}", &body[..key_start])
            } else {
                mask_bearer(body)
            }
        }
        None => mask_bearer(body),
    };

    format!("{masked}{trailing_delimiter}")
}

/// `Bearer <token>` carries a secret without any `key=value` shape.
fn mask_bearer(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut mask_next = false;
    for token in text.split(' ') {
        if mask_next && !token.is_empty() {
            out.push(REDACTED.to_string());
            mask_next = false;
            continue;
        }
        if token.eq_ignore_ascii_case("bearer") {
            mask_next = true;
        }
        out.push(token.to_string());
    }
    out.join(" ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use shopee_hunter_domain::claim::ClaimResultClass;
    use shopee_hunter_domain::ids::SourceId;
    use shopee_hunter_domain::voucher::{VoucherCandidate, VoucherScope, VoucherType};

    fn candidate() -> VoucherCandidate {
        VoucherCandidate {
            source: SourceId::new("feed"),
            source_key: "k1".into(),
            external_id: None,
            code: Some("FREESHIP50".into()),
            promotion_id: Some("promo-1".into()),
            signature: Some("super-secret-signature".into()),
            title: "Freeship 50k toan san".into(),
            description: Some("desc".into()),
            voucher_type: VoucherType::Freeship,
            discount_type: None,
            discount_amount: Some(Decimal::new(50_000, 0)),
            discount_percent: None,
            max_discount: Some(Decimal::new(100_000, 0)),
            min_spend: Some(Decimal::new(200_000, 0)),
            start_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap()),
            end_at: Some(Utc.with_ymd_and_hms(2026, 8, 10, 17, 0, 0).unwrap()),
            scope: Some(VoucherScope::Platform),
            payment_method: None,
            landing_url: Some("https://shopee.vn/voucher".into()),
            raw_payload: serde_json::json!({"code": "FREESHIP50"}),
            observed_at: Utc::now(),
            parser_version: "test-1".into(),
        }
    }

    fn voucher() -> Voucher {
        Voucher::from_candidate(&candidate(), Utc::now())
    }

    fn assert_no_secrets(text: &str) {
        for forbidden in [
            "super-secret-signature",
            "SPC_EC",
            "verysecretcookievalue",
            "abcdefghijklmnop",
        ] {
            assert!(
                !text.contains(forbidden),
                "message leaked {forbidden}: {text}"
            );
        }
    }

    // --- one test per category ---------------------------------------------

    #[test]
    fn renders_new_voucher_with_relevant_fields_only() {
        let v = voucher();
        let event = DomainEvent::VoucherDiscovered {
            voucher_id: v.id,
            source: SourceId::new("feed"),
            version_hash: "abc123def456789".into(),
        };
        let msg = render(&event, Some(&v));

        assert_eq!(msg.category, MessageCategory::NewVoucher);
        assert!(msg.text.starts_with("NEW VOUCHER"));
        assert!(msg.text.contains("Freeship 50k toan san"));
        assert!(msg.text.contains("Type: FREESHIP"));
        assert!(msg.text.contains("Code: FREESHIP50"));
        assert!(msg.text.contains("Discount: 50.000₫"));
        assert!(msg.text.contains("Min spend: 200.000₫"));
        assert!(msg
            .text
            .contains("Window: 10/08 12:00 -> 11/08 00:00 (GMT+7)"));
        assert!(msg.text.contains("Claim: identifiers ready"));
        assert!(msg.text.contains("Source: feed"));
        // Claim credentials and raw hashes are never user-facing.
        assert_no_secrets(&msg.text);
        assert!(!msg.text.contains(&v.raw_hash));
        assert_eq!(msg.idempotency_key, event.idempotency_key());
    }

    #[test]
    fn renders_voucher_updated_with_changed_fields() {
        let v = voucher();
        let event = DomainEvent::VoucherUpdated {
            voucher_id: v.id,
            source: SourceId::new("feed"),
            version_hash: "def456".into(),
            changed_fields: vec!["min_spend".into(), "end_at".into()],
        };
        let msg = render(&event, Some(&v));

        assert_eq!(msg.category, MessageCategory::VoucherUpdated);
        assert!(msg.text.starts_with("VOUCHER UPDATED"));
        assert!(msg.text.contains("Changed: min_spend, end_at"));
        assert_no_secrets(&msg.text);
    }

    #[test]
    fn renders_upcoming_with_vietnam_local_time() {
        let v = voucher();
        let event = DomainEvent::VoucherUpcoming {
            voucher_id: v.id,
            // 05:00 UTC == 12:00 Asia/Ho_Chi_Minh
            starts_at: Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap(),
        };
        let msg = render(&event, Some(&v));

        assert_eq!(msg.category, MessageCategory::Upcoming);
        assert!(msg.text.starts_with("UPCOMING VOUCHER"));
        assert!(msg.text.contains("Starts: 10/08 12:00 (GMT+7)"));
        assert_no_secrets(&msg.text);
    }

    #[test]
    fn renders_claim_success_and_distinguishes_already_saved() {
        let v = voucher();
        let saved = render(
            &DomainEvent::ClaimSucceeded {
                voucher_id: v.id,
                attempt_id: Uuid::new_v4(),
                already_saved: false,
            },
            Some(&v),
        );
        assert_eq!(saved.category, MessageCategory::ClaimSuccess);
        assert!(saved.text.starts_with("CLAIM SUCCESS"));
        assert!(saved.text.contains("Voucher saved to the account."));

        let already = render_event(&DomainEvent::ClaimSucceeded {
            voucher_id: v.id,
            attempt_id: Uuid::new_v4(),
            already_saved: true,
        });
        assert!(already.text.contains("was already saved"));
        assert_no_secrets(&saved.text);
    }

    #[test]
    fn renders_claim_failure_with_class_and_retry_intent() {
        let v = voucher();
        let terminal = render(
            &DomainEvent::ClaimFailed {
                voucher_id: v.id,
                attempt_id: Uuid::new_v4(),
                result_class: ClaimResultClass::Exhausted,
                terminal: true,
            },
            Some(&v),
        );
        assert_eq!(terminal.category, MessageCategory::ClaimFailure);
        assert!(terminal.text.starts_with("CLAIM FAILURE"));
        assert!(terminal.text.contains("Result: EXHAUSTED"));
        assert!(terminal.text.contains("no further attempts"));

        let retryable = render_event(&DomainEvent::ClaimFailed {
            voucher_id: v.id,
            attempt_id: Uuid::new_v4(),
            result_class: ClaimResultClass::RateLimited,
            terminal: false,
        });
        assert!(retryable.text.contains("Result: RATE_LIMITED"));
        assert!(retryable.text.contains("try again"));
        assert_no_secrets(&terminal.text);
    }

    #[test]
    fn renders_session_expired_with_recovery_action() {
        let msg = render_event(&DomainEvent::SessionStateChanged {
            from: SessionState::Healthy,
            to: SessionState::Expired,
            reason: "cookie=SPC_EC=verysecretcookievalue rejected".into(),
        });

        assert_eq!(msg.category, MessageCategory::SessionExpired);
        assert!(msg.text.starts_with("SESSION EXPIRED"));
        assert!(msg.text.contains("Session: HEALTHY -> EXPIRED"));
        assert!(msg.text.contains("claiming is paused"));
        assert!(msg.text.contains("login_session"));
        assert!(msg.text.contains(REDACTED));
        assert_no_secrets(&msg.text);
    }

    #[test]
    fn renders_verification_required_as_manual_action() {
        let msg = render_event(&DomainEvent::SessionStateChanged {
            from: SessionState::Healthy,
            to: SessionState::VerificationRequired,
            reason: "captcha challenge returned".into(),
        });

        assert_eq!(msg.category, MessageCategory::VerificationRequired);
        assert!(msg.text.starts_with("VERIFICATION REQUIRED"));
        assert!(msg.text.contains("manual verification"));
        assert!(msg.text.contains("Reason: captcha challenge returned"));
        assert!(msg.category.is_alert());
    }

    #[test]
    fn renders_source_degraded_and_reassures_isolation() {
        let msg = render_event(&DomainEvent::CollectorDegraded {
            source: SourceId::new("feed"),
            detail: "5 consecutive parse failures".into(),
        });

        assert_eq!(msg.category, MessageCategory::SourceDegraded);
        assert!(msg.text.starts_with("SOURCE DEGRADED"));
        assert!(msg.text.contains("Source: feed"));
        assert!(msg.text.contains("Detail: 5 consecutive parse failures"));
        assert!(msg.text.contains("Other sources are unaffected."));
    }

    #[test]
    fn renders_service_unhealthy() {
        let msg = render_event(&DomainEvent::ServiceUnhealthy {
            service: "claimer".into(),
            detail: "worker loop stalled".into(),
        });

        assert_eq!(msg.category, MessageCategory::ServiceUnhealthy);
        assert!(msg.text.starts_with("SERVICE UNHEALTHY"));
        assert!(msg.text.contains("Service: claimer"));
        assert!(msg.text.contains("Detail: worker loop stalled"));
    }

    #[test]
    fn renders_non_alerting_session_transitions() {
        let msg = render_event(&DomainEvent::SessionStateChanged {
            from: SessionState::Expired,
            to: SessionState::Healthy,
            reason: "re-authenticated".into(),
        });

        assert_eq!(msg.category, MessageCategory::SessionState);
        assert!(!msg.category.is_alert());
        assert!(msg.text.contains("Session: EXPIRED -> HEALTHY"));
        // Recovery must not advertise the login action.
        assert!(!msg.text.contains("login_session"));
    }

    // --- safety properties --------------------------------------------------

    #[test]
    fn never_renders_the_claim_signature() {
        let v = voucher();
        assert!(v.signature.is_some(), "fixture must carry a signature");
        for event in [
            DomainEvent::VoucherDiscovered {
                voucher_id: v.id,
                source: v.source.clone(),
                version_hash: v.version_hash.clone(),
            },
            DomainEvent::ClaimSucceeded {
                voucher_id: v.id,
                attempt_id: Uuid::new_v4(),
                already_saved: false,
            },
        ] {
            let msg = render(&event, Some(&v));
            assert!(!msg.text.contains("super-secret-signature"));
        }
    }

    #[test]
    fn scrub_masks_secret_bearing_text_but_keeps_context() {
        let scrubbed = scrub("refresh failed cookie=SPC_EC=verysecretcookievalue; retry=1");
        assert!(scrubbed.contains("refresh failed"));
        assert!(scrubbed.contains(REDACTED));
        assert!(!scrubbed.contains("verysecretcookievalue"));
        assert!(scrubbed.contains("retry=1"));

        let header = scrub("authorization: Bearer abcdefghijklmnop");
        assert!(!header.contains("abcdefghijklmnop"));
        assert!(header.contains(REDACTED));

        let bearer_only = scrub("sent Bearer abcdefghijklmnop upstream");
        assert!(!bearer_only.contains("abcdefghijklmnop"));

        let query = scrub("callback https://example.test/hook?token=abcdefghijklmnop&page=2");
        assert!(!query.contains("abcdefghijklmnop"));
        assert!(query.contains("page=2"));

        // Non-secret text survives untouched.
        assert_eq!(scrub("promotion_id=123"), "promotion_id=123");
    }

    #[test]
    fn long_input_is_bounded_for_telegram() {
        let msg = render_event(&DomainEvent::ServiceUnhealthy {
            service: "collector".into(),
            detail: "x".repeat(10_000),
        });
        assert!(msg.text.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(msg.text.ends_with('…'));
    }

    #[test]
    fn every_event_variant_renders_non_empty_bounded_text() {
        let id = Uuid::new_v4();
        let events = [
            DomainEvent::VoucherDiscovered {
                voucher_id: id,
                source: SourceId::new("feed"),
                version_hash: "v1".into(),
            },
            DomainEvent::VoucherUpdated {
                voucher_id: id,
                source: SourceId::new("feed"),
                version_hash: "v2".into(),
                changed_fields: vec![],
            },
            DomainEvent::VoucherUpcoming {
                voucher_id: id,
                starts_at: Utc::now(),
            },
            DomainEvent::ClaimSucceeded {
                voucher_id: id,
                attempt_id: id,
                already_saved: false,
            },
            DomainEvent::ClaimFailed {
                voucher_id: id,
                attempt_id: id,
                result_class: ClaimResultClass::UnknownResponse,
                terminal: false,
            },
            DomainEvent::SessionStateChanged {
                from: SessionState::Unknown,
                to: SessionState::Degraded,
                reason: String::new(),
            },
            DomainEvent::CollectorDegraded {
                source: SourceId::new("feed"),
                detail: "slow".into(),
            },
            DomainEvent::ServiceUnhealthy {
                service: "app".into(),
                detail: "down".into(),
            },
        ];

        let mut categories = std::collections::HashSet::new();
        for event in &events {
            let msg = render_event(event);
            assert!(!msg.text.is_empty());
            assert!(msg.text.chars().count() <= MAX_MESSAGE_CHARS);
            assert!(msg.text.starts_with(msg.category.heading()));
            assert_eq!(msg.idempotency_key, event.idempotency_key());
            categories.insert(msg.category);
        }
        assert_eq!(categories.len(), 8, "each variant maps to its own category");
    }

    #[test]
    fn money_and_percent_formatting_is_vietnamese() {
        assert_eq!(fmt_money(Decimal::new(50_000, 0)), "50.000₫");
        assert_eq!(fmt_money(Decimal::new(1_234_567, 0)), "1.234.567₫");
        assert_eq!(fmt_money(Decimal::new(500, 0)), "500₫");
        assert_eq!(fmt_money(Decimal::new(500_000, 1)), "50.000₫"); // 50000.0
        assert_eq!(fmt_percent(Decimal::new(150, 1)), "15%");
    }

    #[test]
    fn voucher_block_degrades_without_enrichment() {
        let v = voucher();
        let event = DomainEvent::VoucherDiscovered {
            voucher_id: v.id,
            source: SourceId::new("feed"),
            version_hash: "abc".into(),
        };
        let plain = render_event(&event);
        assert!(plain.text.contains(&short_id(v.id)));
        assert!(!plain.text.contains("Freeship 50k"));

        let enriched = render(&event, Some(&v));
        assert!(enriched.text.contains("Freeship 50k"));
    }
}
