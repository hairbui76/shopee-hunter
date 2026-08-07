//! The single long-lived Shopee HTTP client.
//!
//! One `reqwest::Client` is built at startup and reused for the process
//! lifetime (ARCHITECTURE.md §13). Rebuilding it per request would discard
//! connection-pool state and reintroduce DNS/TCP/TLS cost on the claim path,
//! which is exactly the latency this design exists to remove.
//!
//! Header policy: user-agent, referer, accept, content-type, and the session
//! cookie. Nothing else. No header is ever logged, and the cookie only exists
//! inside a [`SecretString`] or a `sensitive`-marked `HeaderValue`.

use std::fmt;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, REFERER};

use crate::classify::{classify_probe_response, classify_save_response, Classified, SessionProbe};
use crate::endpoints::EndpointRegistry;
use crate::error::{classify_reqwest_error, parse_retry_after, ClientError};
use crate::plan::ClaimPlan;

/// A normal, current desktop Chrome user-agent.
///
/// This is honest identification of an ordinary browser client, not
/// fingerprint spoofing: no stealth patching, no per-request rotation, no
/// device-signal forgery (CLAUDE.md, "Explicitly out of scope").
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Default production base URL.
pub const DEFAULT_BASE_URL: &str = "https://shopee.vn";

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum response body read into memory before classification.
const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB

/// A string that must never reach a log, a notification, or a panic message.
///
/// `Debug` and `Display` are implemented by hand to print `[REDACTED]`, so a
/// stray `{:?}` on a struct containing one cannot leak session material. The
/// value is only readable inside this crate.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap secret material.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Whether the secret is empty or whitespace only.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Read the secret. Crate-private on purpose: no caller outside this
    /// transport layer has a reason to see it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Transport configuration. Contains no secrets and is safe to log.
#[derive(Debug, Clone)]
pub struct ShopeeClientConfig {
    /// Base URL, e.g. `https://shopee.vn`. Must be https (see
    /// [`EndpointRegistry::new`]).
    pub base_url: String,
    /// Overall deadline for a single request, including body read.
    pub request_timeout: Duration,
    /// Deadline for DNS + TCP + TLS.
    pub connect_timeout: Duration,
    /// User-agent sent on every request.
    pub user_agent: String,
}

impl Default for ShopeeClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

/// Result of a session health probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// Classified session state evidence.
    pub probe: SessionProbe,
    /// Measured round trip, monotonic.
    pub latency: Duration,
}

/// Result of one claim attempt that reached a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimOutcome {
    /// Classified response plus redacted diagnostics.
    pub classified: Classified,
    /// Measured round trip, monotonic.
    pub latency: Duration,
    /// Wall-clock instant the request was handed to the transport; recorded
    /// for scheduler lag reporting (`planned_at` vs `sent_at`).
    pub sent_at: DateTime<Utc>,
}

/// Authenticated Shopee transport and anti-corruption boundary.
pub struct ShopeeClient {
    http: reqwest::Client,
    endpoints: EndpointRegistry,
    config: ShopeeClientConfig,
    referer: HeaderValue,
    /// Prebuilt, `sensitive`-marked cookie header. Stored ready-to-attach so
    /// the claim path performs no header parsing or allocation.
    cookie: RwLock<Option<HeaderValue>>,
}

impl fmt::Debug for ShopeeClient {
    /// Never renders the cookie, only whether one is present.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShopeeClient")
            .field("base_url", &self.endpoints.base_url())
            .field("request_timeout", &self.config.request_timeout)
            .field("connect_timeout", &self.config.connect_timeout)
            .field("has_cookie", &self.has_cookie())
            .finish()
    }
}

impl ShopeeClient {
    /// Build the process-long client.
    ///
    /// The pool is configured to stay warm (`pool_idle_timeout = None`) because
    /// a claim may fire hours after the last request and must not pay for a
    /// fresh TLS handshake at `T=0`.
    ///
    /// # Errors
    ///
    /// [`ClientError::InvalidConfig`] for a rejected base URL or user-agent,
    /// and for a TLS/transport backend that fails to initialise.
    pub fn new(config: ShopeeClientConfig) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .user_agent(&config.user_agent)
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout)
            // Keep pooled connections indefinitely; warmth is the point.
            .pool_idle_timeout(None)
            .gzip(true)
            .cookie_store(true)
            .use_rustls_tls()
            .build()
            .map_err(|err| ClientError::InvalidConfig {
                detail: format!("could not build http client: {}", err.without_url()),
            })?;

        Self::with_client(http, config)
    }

    /// Build around a caller-supplied `reqwest::Client`.
    ///
    /// Intended for tests against a local mock server, and for a future
    /// composition root that wants to own client construction. Base-URL
    /// validation still applies.
    pub fn with_client(
        http: reqwest::Client,
        config: ShopeeClientConfig,
    ) -> Result<Self, ClientError> {
        let endpoints = EndpointRegistry::new(&config.base_url)?;
        let referer = HeaderValue::from_str(endpoints.base_url()).map_err(|_| {
            ClientError::InvalidConfig {
                detail: "base_url is not usable as a Referer header".to_string(),
            }
        })?;

        Ok(Self {
            http,
            endpoints,
            config,
            referer,
            cookie: RwLock::new(None),
        })
    }

    /// Endpoint registry backing this client.
    pub fn endpoints(&self) -> &EndpointRegistry {
        &self.endpoints
    }

    /// Transport configuration in use.
    pub fn config(&self) -> &ShopeeClientConfig {
        &self.config
    }

    /// Install the session cookie material used for subsequent requests.
    ///
    /// The value is converted once into a `sensitive`-marked `HeaderValue`, so
    /// the hot path only clones a reference-counted buffer.
    ///
    /// If the material cannot form a valid header (control characters, for
    /// example) the stored cookie is **cleared** rather than left stale, and a
    /// warning is emitted without the value. Callers can confirm with
    /// [`ShopeeClient::has_cookie`]; the claim gate must not proceed on a
    /// session it could not install.
    pub fn set_cookie_header(&self, cookies: SecretString) {
        let header = if cookies.is_empty() {
            None
        } else {
            match HeaderValue::from_str(cookies.expose()) {
                Ok(mut value) => {
                    // Marks the value as sensitive so hyper/http will not print
                    // it in any Debug output.
                    value.set_sensitive(true);
                    Some(value)
                }
                Err(_) => {
                    tracing::warn!(
                        event = "shopee_session_cookie_rejected",
                        service = "shopee-client",
                        reason = "not_a_valid_header_value",
                        "session cookie could not be installed; cleared instead"
                    );
                    None
                }
            }
        };
        self.store_cookie(header);
    }

    /// Drop any installed session cookie.
    pub fn clear_cookie_header(&self) {
        self.store_cookie(None);
    }

    /// Whether a session cookie is currently installed.
    pub fn has_cookie(&self) -> bool {
        self.read_cookie().is_some()
    }

    /// Low-impact authenticated probe of session health.
    ///
    /// Returns `Ok` for any response that could be classified — including
    /// "expired" — and `Err` only for transport failures.
    pub async fn probe_session(&self) -> Result<ProbeOutcome, ClientError> {
        let started = Instant::now();
        let request = self
            .http
            .get(self.endpoints.account_info())
            .header(ACCEPT, HeaderValue::from_static("application/json"));
        let (status, body) = self
            .send_and_read(self.with_session_headers(request))
            .await?;
        let latency = started.elapsed();
        let probe = classify_probe_response(status, &body);

        tracing::debug!(
            event = "shopee_session_probed",
            service = "shopee-client",
            session_state = probe.as_str(),
            http_status = status,
            latency_ms = latency.as_millis() as u64,
        );

        Ok(ProbeOutcome { probe, latency })
    }

    /// Execute a prebuilt claim plan. This is the `T=0` hot path.
    ///
    /// No JSON is built, no DNS is resolved, no database is touched and no
    /// browser is involved: the body was serialized when the plan was created
    /// and the connection was warmed during preflight.
    ///
    /// Deciding *whether* to claim belongs to the claim policy engine; this
    /// method only executes an already-approved plan.
    pub async fn execute_claim(&self, plan: &ClaimPlan) -> Result<ClaimOutcome, ClientError> {
        let sent_at = Utc::now();
        let started = Instant::now();

        let request = self
            .http
            .post(self.endpoints.url(plan.endpoint()))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .body(plan.body_bytes().to_vec());

        let (status, body) = self
            .send_and_read(self.with_session_headers(request))
            .await?;
        let latency = started.elapsed();
        let classified = classify_save_response(status, &body);

        let fields = plan.log_fields();
        tracing::info!(
            event = "shopee_claim_executed",
            service = "shopee-client",
            voucher_id = fields.voucher_id,
            endpoint = fields.endpoint,
            identifier = fields.identifier,
            result_class = classified.class.as_str(),
            http_status = status,
            upstream_code = classified.diagnostic.upstream_code,
            latency_ms = latency.as_millis() as u64,
        );

        Ok(ClaimOutcome {
            classified,
            latency,
            sent_at,
        })
    }

    /// Pre-establish DNS, TCP and TLS to the Shopee origin.
    ///
    /// Called during preflight so the claim request reuses a live pooled
    /// connection. Deliberately unauthenticated — warming needs no session
    /// material, so none is exposed. The response status is irrelevant; only
    /// reachability and the elapsed time matter.
    pub async fn warm_connection(&self) -> Result<Duration, ClientError> {
        let started = Instant::now();
        let response = self
            .http
            .head(self.endpoints.base_url())
            .header(REFERER, self.referer.clone())
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        let status = response.status().as_u16();
        let elapsed = started.elapsed();

        tracing::debug!(
            event = "shopee_connection_warmed",
            service = "shopee-client",
            http_status = status,
            latency_ms = elapsed.as_millis() as u64,
        );

        Ok(elapsed)
    }

    /// Attach referer plus the session cookie, if one is installed.
    fn with_session_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = request.header(REFERER, self.referer.clone());
        match self.read_cookie() {
            Some(cookie) => request.header(COOKIE, cookie),
            None => request,
        }
    }

    /// Send, then read a bounded body as UTF-8 (lossy, so a truncated or
    /// mis-encoded response still classifies instead of erroring out).
    async fn send_and_read(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(u16, String), ClientError> {
        let response = request.send().await.map_err(classify_reqwest_error)?;

        let status = response.status().as_u16();
        if status == 429 {
            let retry_after = parse_retry_after(
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
            );
            // Surface the hint as a typed error; classification of a 429 body
            // adds nothing the caller can act on.
            return Err(ClientError::RateLimited { retry_after });
        }

        let bytes = response.bytes().await.map_err(classify_reqwest_error)?;
        let truncated = &bytes[..bytes.len().min(MAX_BODY_BYTES)];
        Ok((status, String::from_utf8_lossy(truncated).into_owned()))
    }

    fn read_cookie(&self) -> Option<HeaderValue> {
        match self.cookie.read() {
            Ok(guard) => guard.clone(),
            // A poisoned lock must not take the claim path down; the stored
            // value is still structurally valid.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn store_cookie(&self, header: Option<HeaderValue>) {
        match self.cookie.write() {
            Ok(mut guard) => *guard = header,
            Err(poisoned) => *poisoned.into_inner() = header,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_string_never_renders_its_value() {
        let secret = SecretString::new("SPC_EC=super-secret-cookie-value");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert!(!format!("{secret:?} {secret}").contains("SPC_EC"));
        assert!(SecretString::new("   ").is_empty());
        assert!(!SecretString::new("a").is_empty());
    }

    #[test]
    fn default_config_targets_shopee_vn_over_https() {
        let config = ShopeeClientConfig::default();
        assert_eq!(config.base_url, "https://shopee.vn");
        assert!(config.user_agent.contains("Chrome"));
        assert!(config.request_timeout > config.connect_timeout);
        assert!(ShopeeClient::new(config).is_ok());
    }

    #[test]
    fn construction_rejects_a_non_https_base_url() {
        let config = ShopeeClientConfig {
            base_url: "http://shopee.vn".to_string(),
            ..ShopeeClientConfig::default()
        };
        assert!(matches!(
            ShopeeClient::new(config),
            Err(ClientError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn cookie_lifecycle_and_debug_redaction() {
        let client = ShopeeClient::new(ShopeeClientConfig::default()).expect("client builds");
        assert!(!client.has_cookie());

        client.set_cookie_header(SecretString::new("SPC_EC=abc; SPC_ST=def"));
        assert!(client.has_cookie());

        let rendered = format!("{client:?}");
        assert!(rendered.contains("has_cookie: true"));
        assert!(!rendered.contains("SPC_EC"));

        // Invalid header material clears rather than leaving a stale cookie.
        client.set_cookie_header(SecretString::new("bad\nvalue"));
        assert!(!client.has_cookie());

        client.set_cookie_header(SecretString::new("SPC_EC=abc"));
        client.clear_cookie_header();
        assert!(!client.has_cookie());

        // Empty material is treated as "no session".
        client.set_cookie_header(SecretString::new("   "));
        assert!(!client.has_cookie());
    }
}
