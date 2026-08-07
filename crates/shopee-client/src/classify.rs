//! Response classification: the only place raw Shopee responses are interpreted.
//!
//! Everything downstream (claimer, scheduler, notifier) consumes
//! [`shopee_hunter_domain::ClaimResultClass`] and never touches the body.
//!
//! # Stability
//!
//! UNSTABLE — the code numbers and message fragments below come from
//! community-observed behaviour recorded in the reference repositories plus the
//! Vietnamese strings Shopee's web UI renders. They are **assumptions** until
//! confirmed by an opt-in live smoke run. Each is a table entry rather than an
//! `if` in the control flow so a correction is a one-line edit.
//!
//! # Design rules
//!
//! * Never infer success from anything except an explicit success code.
//! * Prefer `UNKNOWN_RESPONSE` over a guess; unknown is observable, a wrong
//!   guess is not.
//! * Diagnostics carry whitelisted fields only: upstream code, HTTP status, and
//!   a redacted 120-character message excerpt.

use shopee_hunter_domain::{ClaimResultClass, SessionState};

use crate::dto::ShopeeEnvelope;

/// Maximum length of the message excerpt kept for diagnostics.
pub const MAX_MESSAGE_EXCERPT: usize = 120;

/// Body prefix scanned for keywords. Bounds the cost of classifying a large
/// HTML error page and keeps excerpts far away from any trailing payload.
const MAX_SCAN_BYTES: usize = 4096;

/// Placeholder excerpt used when the body is a web page rather than an API
/// response; the markup itself is never copied into diagnostics.
const HTML_EXCERPT: &str = "<html document>";

/// Redaction marker for an excerpt that looked session-bearing.
const REDACTED_EXCERPT: &str = "[REDACTED]";

// ---------------------------------------------------------------------------
// Observed upstream codes (UNSTABLE assumptions)
// ---------------------------------------------------------------------------

/// UNSTABLE: `0` is the success code on every Shopee surface seen so far.
const CODE_SUCCESS: i64 = 0;

/// UNSTABLE ASSUMPTION: `save_voucher` returns `5` when the voucher is already
/// in the account wallet. Success-equivalent, terminal.
const CODE_ALREADY_SAVED: i64 = 5;

/// UNSTABLE ASSUMPTION: `save_voucher` returns `7` when the voucher's claim
/// quota is exhausted. Terminal.
const CODE_EXHAUSTED: i64 = 7;

/// One entry of the code → class table.
struct CodeRule {
    code: i64,
    class: ClaimResultClass,
}

/// Code table for `save_voucher`. Add rows here as live captures confirm codes;
/// do not add branches to [`classify_save_response`].
const SAVE_CODE_RULES: &[CodeRule] = &[
    CodeRule {
        code: CODE_ALREADY_SAVED,
        class: ClaimResultClass::AlreadySaved,
    },
    CodeRule {
        code: CODE_EXHAUSTED,
        class: ClaimResultClass::Exhausted,
    },
];

/// One entry of the message-keyword table.
struct KeywordRule {
    class: ClaimResultClass,
    needles: &'static [&'static str],
}

/// Message keyword table, **ordered — first match wins**.
///
/// The ordering is load-bearing and two entries deviate from the naive keyword
/// list on purpose:
///
/// * `Expired` precedes `Exhausted` because the Vietnamese "hết hạn" (expired)
///   contains the exhausted needle "hết".
/// * `InvalidVoucher`'s non-existence needles precede `AlreadySaved` because
///   "does not exist" contains the already-saved needle "exist".
/// * `Expired` matches "has ended"/"đã kết thúc" rather than a bare "end",
///   which would false-positive on "min sp**end**".
const SAVE_KEYWORD_RULES: &[KeywordRule] = &[
    KeywordRule {
        class: ClaimResultClass::RateLimited,
        needles: &[
            "too many request",
            "too many attempt",
            "rate limit",
            "quá nhiều",
            "thử lại sau",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::Expired,
        needles: &[
            "expired",
            "has ended",
            "already ended",
            "hết hạn",
            "đã kết thúc",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::InvalidVoucher,
        needles: &[
            "does not exist",
            "doesn't exist",
            "not exist",
            "no longer exist",
            "không tồn tại",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::Ineligible,
        needles: &[
            "not eligible",
            "not qualified",
            "not applicable",
            "điều kiện",
            "không đủ",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::NotActive,
        needles: &[
            "not started",
            "not yet available",
            "chưa bắt đầu",
            "chưa diễn ra",
            "chưa",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::AlreadySaved,
        needles: &[
            "already exist",
            "already claimed",
            "already saved",
            "already collected",
            "exist",
            "claimed",
            "đã lưu",
            "được lưu",
            "đã nhận",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::Exhausted,
        needles: &[
            "out of stock",
            "sold out",
            "fully redeemed",
            "no more",
            "đã hết",
            "hết lượt",
            "hết",
        ],
    },
    KeywordRule {
        class: ClaimResultClass::InvalidVoucher,
        needles: &["invalid", "không hợp lệ"],
    },
];

/// Markers meaning "a human verification challenge is in the way".
/// Never bypassed — the caller pauses and notifies the owner.
const VERIFICATION_MARKERS: &[&str] = &[
    "captcha",
    "verification",
    "verify",
    "security check",
    "xác minh",
    "xác thực",
];

/// Markers meaning "we are not authenticated any more".
const LOGIN_MARKERS: &[&str] = &[
    "/buyer/login",
    "login_required",
    "please login",
    "please log in",
    "not logged in",
    "unauthorized",
    "đăng nhập",
];

/// Substrings that make an excerpt too risky to keep verbatim.
const SECRET_MARKERS: &[&str] = &["spc_", "cookie", "csrftoken", "bearer ", "authorization"];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Redacted, whitelisted evidence for why a response was classified as it was.
///
/// This is the only response-derived data that may be persisted or logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Upstream status code (`error` or `code`), when the body parsed.
    pub upstream_code: Option<i64>,
    /// At most [`MAX_MESSAGE_EXCERPT`] characters, whitespace-collapsed and
    /// redacted if it looked session-bearing.
    pub message_excerpt: Option<String>,
    /// HTTP status observed on the wire.
    pub http_status: u16,
}

/// A classified claim response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// Domain class the claimer acts on.
    pub class: ClaimResultClass,
    /// Redacted evidence retained for audit and alerting.
    pub diagnostic: Diagnostic,
}

/// Outcome of the low-impact session health probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionProbe {
    /// Authenticated identity positively proven.
    Healthy,
    /// Authenticated once, no longer accepted.
    Expired,
    /// A login page or explicit login demand was returned.
    LoginRequired,
    /// A human verification challenge is in the way.
    VerificationRequired,
    /// Upstream or network trouble; says nothing about the session.
    Transient,
    /// Could not be interpreted. Never treated as healthy.
    Unknown,
}

impl SessionProbe {
    /// Map onto the domain session state machine.
    ///
    /// `Transient`/`Unknown` become `Degraded`/`Unknown`, both of which block
    /// automatic claiming without asserting the session is dead.
    pub fn to_session_state(self) -> SessionState {
        match self {
            Self::Healthy => SessionState::Healthy,
            Self::Expired => SessionState::Expired,
            Self::LoginRequired => SessionState::LoginRequired,
            Self::VerificationRequired => SessionState::VerificationRequired,
            Self::Transient => SessionState::Degraded,
            Self::Unknown => SessionState::Unknown,
        }
    }

    /// Stable label for logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "HEALTHY",
            Self::Expired => "EXPIRED",
            Self::LoginRequired => "LOGIN_REQUIRED",
            Self::VerificationRequired => "VERIFICATION_REQUIRED",
            Self::Transient => "TRANSIENT",
            Self::Unknown => "UNKNOWN",
        }
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify a `save_voucher` response.
///
/// Pure and total: it never allocates beyond the diagnostic, never performs
/// I/O, and never panics — every fixture in `tests/fixtures/shopee/` runs
/// through this function.
pub fn classify_save_response(http_status: u16, body: &str) -> Classified {
    let scan = BodyScan::new(body);
    let diagnostic = scan.diagnostic(http_status);
    let class = classify_save_class(http_status, &scan);
    Classified { class, diagnostic }
}

fn classify_save_class(http_status: u16, scan: &BodyScan) -> ClaimResultClass {
    // 1. Transport-level verdicts that override any body content.
    if http_status == 429 {
        return ClaimResultClass::RateLimited;
    }
    if http_status >= 500 {
        return ClaimResultClass::TransientFailure;
    }

    // 2. Platform controls. Verification is checked before the login wall
    //    because a challenge page is often served with a 403.
    if scan.matches_any(VERIFICATION_MARKERS) {
        return ClaimResultClass::VerificationRequired;
    }
    if http_status == 401 || http_status == 403 {
        return ClaimResultClass::SessionExpired;
    }
    if scan.is_html || scan.matches_any(LOGIN_MARKERS) {
        // An HTML body where JSON was expected is Shopee's login page.
        return ClaimResultClass::SessionExpired;
    }

    // 3. Envelope-driven verdicts.
    let Some(envelope) = scan.envelope.as_ref() else {
        return ClaimResultClass::UnknownResponse;
    };

    if http_status == 200 && envelope.effective_code() == Some(CODE_SUCCESS) {
        return ClaimResultClass::Success;
    }

    if let Some(code) = envelope.effective_code() {
        if let Some(rule) = SAVE_CODE_RULES.iter().find(|rule| rule.code == code) {
            return rule.class;
        }
    }

    if let Some(rule) = SAVE_KEYWORD_RULES
        .iter()
        .find(|rule| scan.matches_any(rule.needles))
    {
        return rule.class;
    }

    // 4. Explicitly unknown: preserved with diagnostics, alerted on, never
    //    retried without a budget.
    ClaimResultClass::UnknownResponse
}

/// Classify the account-info session probe.
///
/// Mirrors [`classify_save_response`]'s precedence so a login wall or challenge
/// is recognised identically on both paths.
pub fn classify_probe_response(http_status: u16, body: &str) -> SessionProbe {
    let scan = BodyScan::new(body);

    if http_status == 429 || http_status >= 500 {
        return SessionProbe::Transient;
    }
    if scan.matches_any(VERIFICATION_MARKERS) {
        return SessionProbe::VerificationRequired;
    }
    if http_status == 401 || http_status == 403 {
        return SessionProbe::Expired;
    }
    if scan.is_html {
        // The probe endpoint returns JSON when it is reachable at all; HTML
        // means we were redirected to the login page.
        return SessionProbe::LoginRequired;
    }
    if scan.matches_any(LOGIN_MARKERS) {
        return SessionProbe::LoginRequired;
    }

    let Some(envelope) = scan.envelope.as_ref() else {
        return SessionProbe::Unknown;
    };

    if http_status != 200 {
        return SessionProbe::Unknown;
    }

    match envelope.effective_code() {
        Some(CODE_SUCCESS) => match envelope.account_info() {
            // Healthy is asserted only on a positively identified account.
            Some(info) if info.identifies_an_account() => SessionProbe::Healthy,
            // Success code with no identity: schema drift or a silently
            // logged-out response. Never assume healthy.
            _ => SessionProbe::Unknown,
        },
        Some(_) => SessionProbe::Expired,
        None => SessionProbe::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Body scanning helpers
// ---------------------------------------------------------------------------

/// One pass over a response body: shape detection, envelope parse, and a
/// lowercased haystack reused by every rule table.
struct BodyScan<'a> {
    is_html: bool,
    envelope: Option<ShopeeEnvelope>,
    haystack: String,
    raw: &'a str,
}

impl<'a> BodyScan<'a> {
    fn new(body: &'a str) -> Self {
        let trimmed = body.trim_start();
        let is_html = trimmed.starts_with('<');
        let envelope = if is_html {
            None
        } else {
            ShopeeEnvelope::parse(body).ok()
        };

        // Keywords are matched against the upstream message when there is one
        // (precise), otherwise against a bounded prefix of the body.
        let haystack = match envelope.as_ref().and_then(ShopeeEnvelope::message) {
            Some(message) => message.to_lowercase(),
            None => truncate_chars(trimmed, MAX_SCAN_BYTES).to_lowercase(),
        };

        Self {
            is_html,
            envelope,
            haystack,
            raw: trimmed,
        }
    }

    fn matches_any(&self, needles: &[&str]) -> bool {
        needles.iter().any(|needle| self.haystack.contains(needle))
    }

    fn diagnostic(&self, http_status: u16) -> Diagnostic {
        let upstream_code = self
            .envelope
            .as_ref()
            .and_then(ShopeeEnvelope::effective_code);
        let message_excerpt = if self.is_html {
            Some(HTML_EXCERPT.to_string())
        } else {
            match self.envelope.as_ref().and_then(ShopeeEnvelope::message) {
                Some(message) => redact_excerpt(message),
                // No structured message: keep a short excerpt of the raw body so
                // an unknown response is still diagnosable.
                None => redact_excerpt(self.raw),
            }
        };
        Diagnostic {
            upstream_code,
            message_excerpt,
            http_status,
        }
    }
}

/// Collapse whitespace, drop control characters, redact anything
/// session-bearing, and truncate to [`MAX_MESSAGE_EXCERPT`] characters.
fn redact_excerpt(raw: &str) -> Option<String> {
    let collapsed: String = {
        let mut out = String::with_capacity(raw.len().min(MAX_SCAN_BYTES));
        let mut pending_space = false;
        for ch in raw.chars().take(MAX_SCAN_BYTES) {
            if ch.is_whitespace() || ch.is_control() {
                pending_space = !out.is_empty();
                continue;
            }
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
        out
    };

    if collapsed.is_empty() {
        return None;
    }

    let lowered = collapsed.to_lowercase();
    if SECRET_MARKERS.iter().any(|m| lowered.contains(m)) {
        return Some(REDACTED_EXCERPT.to_string());
    }

    if collapsed.chars().count() > MAX_MESSAGE_EXCERPT {
        let mut truncated: String = collapsed.chars().take(MAX_MESSAGE_EXCERPT - 1).collect();
        truncated.push('…');
        return Some(truncated);
    }
    Some(collapsed)
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of(status: u16, body: &str) -> ClaimResultClass {
        classify_save_response(status, body).class
    }

    #[test]
    fn success_requires_an_explicit_success_code_on_200() {
        assert_eq!(
            class_of(200, r#"{"error":0,"error_msg":"","data":{}}"#),
            ClaimResultClass::Success
        );
        assert_eq!(
            class_of(200, r#"{"code":0,"msg":"ok"}"#),
            ClaimResultClass::Success
        );
        // Success code on a non-200 is not trusted.
        assert_eq!(
            class_of(202, r#"{"error":0}"#),
            ClaimResultClass::UnknownResponse
        );
        // Empty body is not success.
        assert_eq!(class_of(200, ""), ClaimResultClass::UnknownResponse);
    }

    #[test]
    fn known_codes_map_before_keywords() {
        assert_eq!(
            class_of(200, r#"{"error":5,"error_msg":"whatever"}"#),
            ClaimResultClass::AlreadySaved
        );
        assert_eq!(
            class_of(200, r#"{"error":7,"error_msg":"whatever"}"#),
            ClaimResultClass::Exhausted
        );
    }

    #[test]
    fn keyword_table_ordering_resolves_vietnamese_overlaps() {
        // "hết hạn" contains the exhausted needle "hết" — expired must win.
        assert_eq!(
            class_of(200, r#"{"error":1,"error_msg":"Mã giảm giá đã hết hạn"}"#),
            ClaimResultClass::Expired
        );
        assert_eq!(
            class_of(
                200,
                r#"{"error":1,"error_msg":"Voucher đã hết lượt sử dụng"}"#
            ),
            ClaimResultClass::Exhausted
        );
        // "does not exist" contains the already-saved needle "exist".
        assert_eq!(
            class_of(200, r#"{"error":1,"error_msg":"Voucher does not exist"}"#),
            ClaimResultClass::InvalidVoucher
        );
        // A bare "end" needle would have false-positived on "spend".
        assert_eq!(
            class_of(
                200,
                r#"{"error":1,"error_msg":"You are not eligible: min spend not met"}"#
            ),
            ClaimResultClass::Ineligible
        );
    }

    #[test]
    fn covers_every_message_driven_class() {
        let cases = [
            ("voucher already claimed", ClaimResultClass::AlreadySaved),
            ("Voucher đã lưu vào ví", ClaimResultClass::AlreadySaved),
            ("out of stock", ClaimResultClass::Exhausted),
            ("Chương trình chưa bắt đầu", ClaimResultClass::NotActive),
            ("promotion has ended", ClaimResultClass::Expired),
            ("Bạn không đủ điều kiện", ClaimResultClass::Ineligible),
            ("voucher không hợp lệ", ClaimResultClass::InvalidVoucher),
            ("invalid voucher", ClaimResultClass::InvalidVoucher),
            (
                "too many requests, thử lại sau",
                ClaimResultClass::RateLimited,
            ),
        ];
        for (msg, expected) in cases {
            let body = serde_json::json!({"error": 1, "error_msg": msg}).to_string();
            assert_eq!(class_of(200, &body), expected, "message: {msg}");
        }
    }

    #[test]
    fn transport_and_platform_controls_take_precedence() {
        assert_eq!(
            class_of(429, r#"{"error":0}"#),
            ClaimResultClass::RateLimited
        );
        assert_eq!(
            class_of(503, "<html>gateway</html>"),
            ClaimResultClass::TransientFailure
        );
        assert_eq!(class_of(401, "{}"), ClaimResultClass::SessionExpired);
        assert_eq!(class_of(403, "{}"), ClaimResultClass::SessionExpired);
        assert_eq!(
            class_of(200, "<!doctype html><html><body>Đăng nhập</body></html>"),
            ClaimResultClass::SessionExpired
        );
        assert_eq!(
            class_of(200, r#"{"error":1,"error_msg":"please login to continue"}"#),
            ClaimResultClass::SessionExpired
        );
        // A challenge page beats the 403 login verdict.
        assert_eq!(
            class_of(403, "<html><div id=\"captcha\"></div></html>"),
            ClaimResultClass::VerificationRequired
        );
        assert_eq!(
            class_of(
                200,
                r#"{"error":1,"error_msg":"Vui lòng xác minh tài khoản"}"#
            ),
            ClaimResultClass::VerificationRequired
        );
    }

    #[test]
    fn unrecognised_responses_stay_unknown_with_diagnostics() {
        let classified =
            classify_save_response(200, r#"{"error":99999,"error_msg":"brand new failure"}"#);
        assert_eq!(classified.class, ClaimResultClass::UnknownResponse);
        assert_eq!(classified.diagnostic.upstream_code, Some(99999));
        assert_eq!(
            classified.diagnostic.message_excerpt.as_deref(),
            Some("brand new failure")
        );
        assert_eq!(classified.diagnostic.http_status, 200);
    }

    #[test]
    fn diagnostics_are_truncated_and_redacted() {
        let long = "x".repeat(500);
        let body = serde_json::json!({"error": 1, "error_msg": long}).to_string();
        let excerpt = classify_save_response(200, &body)
            .diagnostic
            .message_excerpt
            .expect("excerpt present");
        assert_eq!(excerpt.chars().count(), MAX_MESSAGE_EXCERPT);
        assert!(excerpt.ends_with('…'));

        let leaky = r#"{"error":1,"error_msg":"echo of SPC_EC=abcdef123456"}"#;
        assert_eq!(
            classify_save_response(200, leaky)
                .diagnostic
                .message_excerpt
                .as_deref(),
            Some(REDACTED_EXCERPT)
        );

        // HTML bodies never contribute markup to diagnostics.
        let html = classify_save_response(200, "<html><body>login SPC_EC=abc</body></html>");
        assert_eq!(
            html.diagnostic.message_excerpt.as_deref(),
            Some(HTML_EXCERPT)
        );
    }

    #[test]
    fn probe_asserts_healthy_only_with_an_identified_account() {
        assert_eq!(
            classify_probe_response(200, r#"{"error":0,"data":{"userid":123,"username":"b"}}"#),
            SessionProbe::Healthy
        );
        assert_eq!(
            classify_probe_response(200, r#"{"error":0,"data":{}}"#),
            SessionProbe::Unknown
        );
        assert_eq!(
            classify_probe_response(200, r#"{"error":0}"#),
            SessionProbe::Unknown
        );
    }

    #[test]
    fn probe_mirrors_login_and_verification_rules() {
        assert_eq!(
            classify_probe_response(200, r#"{"error":3,"error_msg":"not logged in"}"#),
            SessionProbe::LoginRequired
        );
        assert_eq!(
            classify_probe_response(200, r#"{"error":3,"error_msg":"token invalid"}"#),
            SessionProbe::Expired
        );
        assert_eq!(classify_probe_response(403, "{}"), SessionProbe::Expired);
        assert_eq!(
            classify_probe_response(200, "<html>login</html>"),
            SessionProbe::LoginRequired
        );
        assert_eq!(
            classify_probe_response(200, r#"{"error":1,"error_msg":"captcha required"}"#),
            SessionProbe::VerificationRequired
        );
        assert_eq!(
            classify_probe_response(500, "boom"),
            SessionProbe::Transient
        );
        assert_eq!(classify_probe_response(429, "{}"), SessionProbe::Transient);
        assert_eq!(
            classify_probe_response(200, "not json"),
            SessionProbe::Unknown
        );
    }

    #[test]
    fn probe_states_map_onto_the_domain_session_machine() {
        assert!(SessionProbe::Healthy.to_session_state().allows_claims());
        for probe in [
            SessionProbe::Expired,
            SessionProbe::LoginRequired,
            SessionProbe::VerificationRequired,
        ] {
            assert!(probe.to_session_state().blocks_claims(), "{probe:?}");
        }
        // Transient/Unknown must not allow claims and must not assert death.
        for probe in [SessionProbe::Transient, SessionProbe::Unknown] {
            let state = probe.to_session_state();
            assert!(!state.allows_claims());
            assert!(!state.blocks_claims());
        }
    }
}
