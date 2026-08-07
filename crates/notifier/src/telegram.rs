//! Telegram Bot API transport.
//!
//! One long-lived `reqwest::Client` is reused for every message so DNS/TCP/TLS
//! stay warm (CLAUDE.md "Performance-first rule"), and every response is
//! classified into [`NotifierError`] instead of being treated as a generic
//! failure.
//!
//! ## Token handling
//!
//! The bot token appears in the request *path* (`/bot<token>/sendMessage`),
//! which makes it easy to leak through error messages: `reqwest` embeds the
//! URL in its `Display` output. Two defences are applied to every string that
//! can escape this module:
//!
//! 1. `reqwest::Error::without_url()` strips the URL before formatting;
//! 2. [`TelegramNotifier::sanitize`] replaces any literal occurrence of the
//!    token with `[REDACTED]` and then applies [`crate::format::scrub`].

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use shopee_hunter_observability::redact::REDACTED;

use crate::error::NotifierError;
use crate::format::scrub;
use crate::notifier::Notifier;

/// Public Telegram Bot API endpoint.
pub const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound on an upstream-provided cooldown we are willing to sleep for
/// inside one `send` call; longer waits belong to the outbox schedule.
const MAX_HONOURED_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Bounded retry budget for one `send` call.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts, including the first one. Never unbounded.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff for `attempt` (1-based), capped at `max_delay`.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exp = attempt.saturating_sub(1).min(16);
        self.base_delay
            .saturating_mul(2u32.saturating_pow(exp))
            .min(self.max_delay)
    }
}

/// Telegram implementation of [`Notifier`].
pub struct TelegramNotifier {
    client: reqwest::Client,
    base_url: String,
    token: String,
    retry: RetryPolicy,
}

impl std::fmt::Debug for TelegramNotifier {
    /// Hand-written so the bot token can never reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramNotifier")
            .field("base_url", &self.base_url)
            .field("token", &REDACTED)
            .field("retry", &self.retry)
            .finish()
    }
}

impl TelegramNotifier {
    /// Build a notifier against the public Telegram API.
    pub fn new(bot_token: impl Into<String>) -> Result<Self, NotifierError> {
        let token = bot_token.into();
        if token.trim().is_empty() {
            return Err(NotifierError::config("bot token is empty"));
        }

        let client = reqwest::Client::builder()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .map_err(|err| NotifierError::config(scrub(&err.to_string())))?;

        Ok(Self {
            client,
            base_url: TELEGRAM_API_BASE.to_string(),
            token,
            retry: RetryPolicy::default(),
        })
    }

    /// Point at a different API host (tests, self-hosted Bot API server).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Reuse an externally configured client (shared pool, proxy settings).
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/bot{}/sendMessage", self.base_url, self.token)
    }

    /// Remove the token from any text that may escape this module, then apply
    /// the shared free-text scrubbing rules.
    fn sanitize(&self, text: &str) -> String {
        scrub(&text.replace(self.token.as_str(), REDACTED))
    }

    async fn send_once(&self, chat_id: &str, text: &str) -> Result<(), NotifierError> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });

        let response = self
            .client
            .post(self.endpoint())
            .json(&body)
            .send()
            .await
            .map_err(|err| self.transport_error(err))?;

        let status = response.status();
        let header_retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);

        let raw_body = response
            .text()
            .await
            .map_err(|err| self.transport_error(err))?;

        self.classify(status, &raw_body, header_retry_after)
    }

    fn transport_error(&self, err: reqwest::Error) -> NotifierError {
        let kind = if err.is_timeout() {
            "timeout"
        } else if err.is_connect() {
            "connect"
        } else if err.is_body() || err.is_decode() {
            "body"
        } else {
            "request"
        };
        // `without_url` drops the token-bearing URL before formatting.
        let detail = self.sanitize(&err.without_url().to_string());
        NotifierError::transport(format!("{kind}: {detail}"))
    }

    fn classify(
        &self,
        status: StatusCode,
        raw_body: &str,
        header_retry_after: Option<Duration>,
    ) -> Result<(), NotifierError> {
        let parsed = serde_json::from_str::<serde_json::Value>(raw_body).ok();
        let ok_flag = parsed
            .as_ref()
            .and_then(|v| v.get("ok"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if status.is_success() && ok_flag {
            return Ok(());
        }

        let detail = parsed
            .as_ref()
            .and_then(|v| v.get("description"))
            .and_then(serde_json::Value::as_str)
            .map(|description| self.sanitize(description))
            .unwrap_or_else(|| self.sanitize(raw_body));

        let retry_after = parsed
            .as_ref()
            .and_then(|v| v.get("parameters"))
            .and_then(|v| v.get("retry_after"))
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs)
            .or(header_retry_after);

        let code = status.as_u16();
        Err(match code {
            429 => NotifierError::RateLimited { retry_after },
            401 | 403 => NotifierError::Unauthorized { detail },
            _ if status.is_client_error() => NotifierError::InvalidRequest { detail },
            _ if status.is_server_error() => NotifierError::Upstream {
                status: code,
                detail,
            },
            // 2xx with `ok: false`, or a redirect/informational response:
            // never guess, surface it for diagnosis.
            _ => NotifierError::UnknownResponse {
                detail: format!("status {code}: {detail}"),
            },
        })
    }
}

#[async_trait::async_trait]
impl Notifier for TelegramNotifier {
    async fn send(&self, chat_id: &str, text: &str) -> Result<(), NotifierError> {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.send_once(chat_id, text).await {
                Ok(()) => {
                    tracing::debug!(
                        event = "notifier_delivered",
                        service = "notifier",
                        attempt,
                        text_len = text.len()
                    );
                    return Ok(());
                }
                Err(err) => {
                    if !err.is_retryable() {
                        tracing::warn!(
                            event = "notifier_failed",
                            service = "notifier",
                            attempt,
                            result_class = err.class(),
                            error = %err
                        );
                        return Err(err);
                    }
                    if attempt >= self.retry.max_attempts {
                        tracing::warn!(
                            event = "notifier_retries_exhausted",
                            service = "notifier",
                            attempt,
                            result_class = err.class(),
                            error = %err
                        );
                        return Err(NotifierError::RetriesExhausted {
                            attempts: attempt,
                            source: Box::new(err),
                        });
                    }

                    let delay = err
                        .retry_after()
                        .map(|d| d.min(MAX_HONOURED_RETRY_AFTER))
                        .unwrap_or_else(|| self.retry.delay_for(attempt));
                    tracing::debug!(
                        event = "notifier_retry_scheduled",
                        service = "notifier",
                        attempt,
                        result_class = err.class(),
                        delay_ms = delay.as_millis() as u64
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    fn name(&self) -> &str {
        "telegram"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier() -> TelegramNotifier {
        TelegramNotifier::new("123456:SUPER-SECRET-TOKEN").expect("builds")
    }

    #[test]
    fn empty_token_is_rejected_at_construction() {
        let err = TelegramNotifier::new("   ").expect_err("must reject");
        assert!(matches!(err, NotifierError::Config { .. }));
        assert!(!err.is_retryable());
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let rendered = format!("{:?}", notifier());
        assert!(!rendered.contains("SUPER-SECRET-TOKEN"));
        assert!(rendered.contains(REDACTED));
    }

    #[test]
    fn sanitize_removes_the_token_from_arbitrary_text() {
        let n = notifier();
        let dirty =
            "failed calling https://api.telegram.org/bot123456:SUPER-SECRET-TOKEN/sendMessage";
        let clean = n.sanitize(dirty);
        assert!(!clean.contains("SUPER-SECRET-TOKEN"));
        assert!(clean.contains(REDACTED));
    }

    #[test]
    fn classifies_success() {
        let n = notifier();
        assert!(n
            .classify(StatusCode::OK, r#"{"ok":true,"result":{}}"#, None)
            .is_ok());
    }

    #[test]
    fn classifies_rate_limit_with_retry_after() {
        let n = notifier();
        let err = n
            .classify(
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"ok":false,"description":"Too Many Requests","parameters":{"retry_after":12}}"#,
                None,
            )
            .expect_err("must fail");
        assert!(err.is_retryable());
        assert_eq!(err.retry_after(), Some(Duration::from_secs(12)));
    }

    #[test]
    fn falls_back_to_retry_after_header() {
        let n = notifier();
        let err = n
            .classify(
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"ok":false}"#,
                Some(Duration::from_secs(7)),
            )
            .expect_err("must fail");
        assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));
    }

    #[test]
    fn classifies_terminal_and_transient_statuses() {
        let n = notifier();
        let unauthorized = n
            .classify(
                StatusCode::UNAUTHORIZED,
                r#"{"ok":false,"description":"Unauthorized"}"#,
                None,
            )
            .expect_err("must fail");
        assert!(matches!(unauthorized, NotifierError::Unauthorized { .. }));
        assert!(!unauthorized.is_retryable());

        let bad_request = n
            .classify(
                StatusCode::BAD_REQUEST,
                r#"{"ok":false,"description":"chat not found"}"#,
                None,
            )
            .expect_err("must fail");
        assert!(matches!(bad_request, NotifierError::InvalidRequest { .. }));
        assert!(!bad_request.is_retryable());

        let upstream = n
            .classify(StatusCode::BAD_GATEWAY, "<html>oops</html>", None)
            .expect_err("must fail");
        assert!(matches!(
            upstream,
            NotifierError::Upstream { status: 502, .. }
        ));
        assert!(upstream.is_retryable());

        let ok_false = n
            .classify(
                StatusCode::OK,
                r#"{"ok":false,"description":"weird"}"#,
                None,
            )
            .expect_err("must fail");
        assert!(matches!(ok_false, NotifierError::UnknownResponse { .. }));
        assert!(!ok_false.is_retryable());
    }

    #[test]
    fn retry_backoff_grows_and_caps() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(400),
        };
        assert_eq!(policy.delay_for(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for(3), Duration::from_millis(400));
        assert_eq!(policy.delay_for(9), Duration::from_millis(400));
    }
}
