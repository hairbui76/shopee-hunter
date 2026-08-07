//! The collector contract and shared value types.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use shopee_hunter_domain::voucher::VoucherCandidate;
use shopee_hunter_domain::SourceId;
use thiserror::Error;

/// Context passed to each collection run.
#[derive(Debug, Clone)]
pub struct CollectionContext {
    pub now: DateTime<Utc>,
    /// Soft budget: collectors should return partial results rather than
    /// exceed this by a wide margin.
    pub deadline: DateTime<Utc>,
}

/// A hint that the source is rate limiting us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitHint {
    pub retry_after: Option<std::time::Duration>,
    pub detail: String,
}

/// A single item that failed to parse without failing the whole run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialFailure {
    pub source_key: Option<String>,
    pub reason: String,
}

/// Output of one collection run.
#[derive(Debug, Clone, Default)]
pub struct CollectionResult {
    pub candidates: Vec<VoucherCandidate>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub source_latency: Option<std::time::Duration>,
    pub partial_failures: Vec<PartialFailure>,
    pub rate_limit: Option<RateLimitHint>,
}

impl CollectionResult {
    pub fn is_partial(&self) -> bool {
        !self.partial_failures.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("transient network error: {0}")]
    Transient(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("malformed upstream payload: {0}")]
    Malformed(String),
    #[error("source misconfigured: {0}")]
    Config(String),
    #[error("timed out")]
    Timeout,
}

impl CollectorError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Transient(_) | Self::RateLimited(_) | Self::Timeout
        )
    }
}

/// Per-source health state (independent of other sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceHealthState {
    Healthy,
    Degraded,
    RateLimited,
    AuthRequired,
    Failed,
    Disabled,
}

impl SourceHealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "HEALTHY",
            Self::Degraded => "DEGRADED",
            Self::RateLimited => "RATE_LIMITED",
            Self::AuthRequired => "AUTH_REQUIRED",
            Self::Failed => "FAILED",
            Self::Disabled => "DISABLED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceHealth {
    pub state: SourceHealthState,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    pub last_latency: Option<std::time::Duration>,
    pub last_result_count: usize,
    pub consecutive_failures: u32,
    pub detail: Option<String>,
}

impl Default for SourceHealth {
    fn default() -> Self {
        Self {
            state: SourceHealthState::Healthy,
            last_success: None,
            last_failure: None,
            last_latency: None,
            last_result_count: 0,
            consecutive_failures: 0,
            detail: None,
        }
    }
}

/// Thread-safe shared handle to one source's health.
#[derive(Debug, Clone, Default)]
pub struct SharedSourceHealth {
    inner: Arc<RwLock<SourceHealth>>,
}

impl SharedSourceHealth {
    pub fn snapshot(&self) -> SourceHealth {
        self.inner.read().expect("source health lock").clone()
    }

    pub fn record_success(
        &self,
        at: DateTime<Utc>,
        latency: Option<std::time::Duration>,
        count: usize,
        partial: bool,
    ) {
        let mut h = self.inner.write().expect("source health lock");
        h.state = if partial {
            SourceHealthState::Degraded
        } else {
            SourceHealthState::Healthy
        };
        h.last_success = Some(at);
        h.last_latency = latency;
        h.last_result_count = count;
        h.consecutive_failures = 0;
        h.detail = None;
    }

    pub fn record_failure(&self, at: DateTime<Utc>, err: &CollectorError) {
        let mut h = self.inner.write().expect("source health lock");
        h.state = match err {
            CollectorError::RateLimited(_) => SourceHealthState::RateLimited,
            CollectorError::AuthRequired(_) => SourceHealthState::AuthRequired,
            e if e.is_transient() => SourceHealthState::Degraded,
            _ => SourceHealthState::Failed,
        };
        h.last_failure = Some(at);
        h.consecutive_failures = h.consecutive_failures.saturating_add(1);
        h.detail = Some(err.to_string());
    }

    pub fn set_disabled(&self) {
        self.inner.write().expect("source health lock").state = SourceHealthState::Disabled;
    }
}

/// The collector contract every source implements.
#[async_trait]
pub trait VoucherCollector: Send + Sync {
    fn name(&self) -> &str;

    fn source_id(&self) -> SourceId {
        SourceId::new(self.name())
    }

    async fn collect(
        &self,
        context: &CollectionContext,
    ) -> Result<CollectionResult, CollectorError>;
}
