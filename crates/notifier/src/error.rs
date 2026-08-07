//! Notifier error taxonomy.
//!
//! Delivery failures are classified so the retry policy and the outbox worker
//! can react deliberately instead of blind-retrying (CLAUDE.md "Error
//! handling"). Every `detail` string reaching this type must already be
//! scrubbed by [`crate::format::scrub`] and stripped of the bot token by the
//! transport, so an error is always safe to log, persist, and alert on.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum NotifierError {
    /// Network-level failure: DNS, connect, TLS, timeout, broken response.
    #[error("notifier transport failure: {detail}")]
    Transport { detail: String },

    /// Upstream asked us to slow down. Honour `retry_after` when present.
    #[error("notifier rate limited by upstream")]
    RateLimited { retry_after: Option<Duration> },

    /// Bad bot token, bot blocked, or no access to the chat. Retrying cannot
    /// help; the owner must intervene.
    #[error("notifier not authorized: {detail}")]
    Unauthorized { detail: String },

    /// Upstream rejected the request itself (unknown chat, bad payload).
    #[error("notifier request rejected: {detail}")]
    InvalidRequest { detail: String },

    /// Upstream server-side failure (5xx); usually transient.
    #[error("notifier upstream error (status {status}): {detail}")]
    Upstream { status: u16, detail: String },

    /// A response that does not fit any known shape. Surfaced rather than
    /// guessed at, so schema drift becomes visible.
    #[error("notifier response not understood: {detail}")]
    UnknownResponse { detail: String },

    /// Invalid configuration detected at construction time.
    #[error("notifier misconfigured: {detail}")]
    Config { detail: String },

    /// The bounded retry budget was spent; carries the last failure.
    #[error("notifier gave up after {attempts} attempt(s): {source}")]
    RetriesExhausted {
        attempts: u32,
        #[source]
        source: Box<NotifierError>,
    },
}

impl NotifierError {
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::Transport {
            detail: detail.into(),
        }
    }

    pub fn config(detail: impl Into<String>) -> Self {
        Self::Config {
            detail: detail.into(),
        }
    }

    /// Whether another attempt could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::RateLimited { .. } | Self::Upstream { .. } => true,
            Self::Unauthorized { .. }
            | Self::InvalidRequest { .. }
            | Self::UnknownResponse { .. }
            | Self::Config { .. } => false,
            // Already exhausted at a lower level: never re-enter the loop.
            Self::RetriesExhausted { .. } => false,
        }
    }

    /// Whether the owner must act before delivery can work again.
    pub fn needs_owner_action(&self) -> bool {
        matches!(self, Self::Unauthorized { .. } | Self::Config { .. })
    }

    /// Upstream-provided cooldown, when it supplied one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            Self::RetriesExhausted { source, .. } => source.retry_after(),
            _ => None,
        }
    }

    /// Short stable label for metrics and outbox diagnostics.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "TRANSPORT",
            Self::RateLimited { .. } => "RATE_LIMITED",
            Self::Unauthorized { .. } => "UNAUTHORIZED",
            Self::InvalidRequest { .. } => "INVALID_REQUEST",
            Self::Upstream { .. } => "UPSTREAM",
            Self::UnknownResponse { .. } => "UNKNOWN_RESPONSE",
            Self::Config { .. } => "CONFIG",
            Self::RetriesExhausted { .. } => "RETRIES_EXHAUSTED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_drives_retry_decisions() {
        assert!(NotifierError::transport("reset").is_retryable());
        assert!(NotifierError::RateLimited { retry_after: None }.is_retryable());
        assert!(NotifierError::Upstream {
            status: 502,
            detail: "bad gateway".into()
        }
        .is_retryable());

        let unauthorized = NotifierError::Unauthorized {
            detail: "bot blocked".into(),
        };
        assert!(!unauthorized.is_retryable());
        assert!(unauthorized.needs_owner_action());

        assert!(!NotifierError::InvalidRequest {
            detail: "chat not found".into()
        }
        .is_retryable());
    }

    #[test]
    fn exhausted_retries_are_terminal_and_keep_the_cause() {
        let err = NotifierError::RetriesExhausted {
            attempts: 3,
            source: Box::new(NotifierError::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
            }),
        };
        assert!(!err.is_retryable());
        assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(err.class(), "RETRIES_EXHAUSTED");
        assert!(err.to_string().contains("3 attempt(s)"));
    }
}
