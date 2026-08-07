//! Transport error hierarchy for the Shopee client.
//!
//! Errors here describe *how the request failed*, never *what the platform
//! decided about a voucher* — that is [`crate::classify`]'s job.
//!
//! Redaction rule: no variant may carry a raw upstream body, a URL with query
//! parameters, or any header value. `reqwest` error text is always passed
//! through [`reqwest::Error::without_url`] before it is stored.

use std::time::Duration;

use shopee_hunter_domain::ClaimResultClass;

/// Everything that can go wrong between "we decided to send a request" and
/// "we hold a response body".
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Connect/read/overall deadline elapsed.
    #[error("shopee request timed out")]
    Timeout,

    /// DNS, TCP or TLS could not be established.
    #[error("shopee connection failed: {detail}")]
    Connect {
        /// Sanitised transport description, never a URL or header value.
        detail: String,
    },

    /// A response arrived but `reqwest` surfaced the status as the error.
    #[error("shopee returned http {status}")]
    Http {
        /// HTTP status code.
        status: u16,
    },

    /// The body could not be decoded or did not match the expected envelope.
    #[error("malformed shopee payload: {detail}")]
    MalformedPayload {
        /// Structural description only (parser category, line/column).
        detail: String,
    },

    /// Upstream explicitly rate limited us.
    #[error("shopee rate limited the client")]
    RateLimited {
        /// Hint parsed from `Retry-After`, when the header was present and sane.
        retry_after: Option<Duration>,
    },

    /// A platform control (login wall or verification challenge) stood in the
    /// way. This is never bypassed: the caller must pause and notify the owner.
    #[error("shopee returned a login or verification wall")]
    Blocked,

    /// Local I/O failure while building or streaming the request.
    #[error("io error: {detail}")]
    Io {
        /// `std::io::Error` kind description.
        detail: String,
    },

    /// The client was configured with values it cannot operate on. Raised at
    /// construction time only, never mid-flight.
    #[error("invalid shopee client configuration: {detail}")]
    InvalidConfig {
        /// What is wrong with the configuration.
        detail: String,
    },

    /// Transport failure that does not fit any known class. Surfaced rather
    /// than swallowed so it becomes visible in metrics.
    #[error("unclassified shopee transport error: {detail}")]
    Other {
        /// Sanitised description.
        detail: String,
    },
}

impl ClientError {
    /// Whether retrying the same request could plausibly succeed.
    ///
    /// Note that `RateLimited` is *not* reported as transient: it needs the
    /// rate-limit backoff path, not ordinary retry.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Timeout | Self::Connect { .. } | Self::Io { .. } => true,
            Self::Http { status } => *status >= 500,
            Self::RateLimited { .. }
            | Self::Blocked
            | Self::MalformedPayload { .. }
            | Self::InvalidConfig { .. }
            | Self::Other { .. } => false,
        }
    }

    /// Domain result class to record when a claim attempt ends in this error
    /// before any response could be classified.
    pub fn as_result_class(&self) -> ClaimResultClass {
        match self {
            Self::Timeout | Self::Connect { .. } | Self::Io { .. } => {
                ClaimResultClass::TransientFailure
            }
            Self::Http { status } if *status >= 500 => ClaimResultClass::TransientFailure,
            Self::RateLimited { .. } => ClaimResultClass::RateLimited,
            Self::Blocked => ClaimResultClass::VerificationRequired,
            Self::Http { .. }
            | Self::MalformedPayload { .. }
            | Self::InvalidConfig { .. }
            | Self::Other { .. } => ClaimResultClass::UnknownResponse,
        }
    }

    /// Short stable label for metrics dimensions.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect { .. } => "connect",
            Self::Http { .. } => "http_status",
            Self::MalformedPayload { .. } => "malformed_payload",
            Self::RateLimited { .. } => "rate_limited",
            Self::Blocked => "blocked",
            Self::Io { .. } => "io",
            Self::InvalidConfig { .. } => "invalid_config",
            Self::Other { .. } => "other",
        }
    }
}

/// Map a `reqwest` failure onto the typed hierarchy.
///
/// The source error is deliberately **not** retained: its `Display` embeds the
/// request URL, and callers only ever need the class plus a short description.
/// Ownership is taken because stripping the URL consumes the error.
pub fn classify_reqwest_error(err: reqwest::Error) -> ClientError {
    if err.is_timeout() {
        return ClientError::Timeout;
    }
    if let Some(status) = err.status() {
        let status = status.as_u16();
        if status == 429 {
            return ClientError::RateLimited { retry_after: None };
        }
        return ClientError::Http { status };
    }

    let is_connect = err.is_connect();
    let is_payload = err.is_decode() || err.is_body();
    let detail = sanitize_reqwest_detail(err);

    if is_connect {
        ClientError::Connect { detail }
    } else if is_payload {
        ClientError::MalformedPayload { detail }
    } else {
        ClientError::Other { detail }
    }
}

/// Strip the URL from a `reqwest` error before it is stored or logged.
fn sanitize_reqwest_detail(err: reqwest::Error) -> String {
    let mut text = err.without_url().to_string();
    const MAX: usize = 160;
    if text.chars().count() > MAX {
        text = text.chars().take(MAX).collect();
    }
    text
}

/// Parse a `Retry-After` header value into a duration.
///
/// Only the delta-seconds form is honoured; HTTP-date form returns `None`
/// rather than guessing, so the caller falls back to its own backoff.
pub fn parse_retry_after(raw: Option<&str>) -> Option<Duration> {
    let secs: u64 = raw?.trim().parse().ok()?;
    // Ignore absurd hints; a mis-parsed header must never stall the scheduler.
    const MAX_RETRY_AFTER_SECS: u64 = 3_600;
    if secs > MAX_RETRY_AFTER_SECS {
        return None;
    }
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification_matches_retry_intent() {
        assert!(ClientError::Timeout.is_transient());
        assert!(ClientError::Connect {
            detail: "refused".into()
        }
        .is_transient());
        assert!(ClientError::Http { status: 503 }.is_transient());
        assert!(!ClientError::Http { status: 403 }.is_transient());
        assert!(!ClientError::RateLimited { retry_after: None }.is_transient());
        assert!(!ClientError::Blocked.is_transient());
    }

    #[test]
    fn result_class_mapping_never_claims_success() {
        for err in [
            ClientError::Timeout,
            ClientError::Connect { detail: "x".into() },
            ClientError::Http { status: 500 },
            ClientError::Http { status: 404 },
            ClientError::MalformedPayload { detail: "x".into() },
            ClientError::RateLimited { retry_after: None },
            ClientError::Blocked,
            ClientError::InvalidConfig { detail: "x".into() },
            ClientError::Other { detail: "x".into() },
        ] {
            assert!(!err.as_result_class().is_success_equivalent());
            assert!(!err.kind().is_empty());
        }
        assert_eq!(
            ClientError::Http { status: 500 }.as_result_class(),
            ClaimResultClass::TransientFailure
        );
        assert_eq!(
            ClientError::Http { status: 404 }.as_result_class(),
            ClaimResultClass::UnknownResponse
        );
    }

    #[test]
    fn retry_after_parsing_is_bounded() {
        assert_eq!(parse_retry_after(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after(Some(" 30 ")),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after(Some("99999")), None);
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(None), None);
    }

    #[tokio::test]
    async fn connect_failures_map_to_connect_without_leaking_url() {
        // Port 1 on loopback is reliably closed; no external network needed.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .build()
            .expect("client builds");
        let err = client
            .get("http://127.0.0.1:1/api/v4/account/basic/get_account_info?secret=should_not_leak")
            .send()
            .await
            .expect_err("connection to a closed port must fail");

        let mapped = classify_reqwest_error(err);
        let rendered = mapped.to_string();
        assert!(!rendered.contains("secret"), "error text leaked the URL");
        assert!(
            !rendered.contains("127.0.0.1"),
            "error text leaked the host"
        );
        assert!(mapped.is_transient());
    }
}
