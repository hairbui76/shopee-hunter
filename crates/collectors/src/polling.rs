//! Adaptive polling (ROADMAP Phase 19). Computes each source's next poll
//! interval from evidence — time of day, campaign windows, recent errors,
//! rate-limit responses — and enforces a per-source hourly request budget so a
//! collector can never accidentally issue unlimited requests.

use std::time::Duration;

use crate::contract::SourceHealthState;

#[derive(Debug, Clone)]
pub struct PollingConfig {
    /// Interval during normal quiet periods.
    pub normal: Duration,
    /// Faster interval near a known campaign window or after a recent change.
    pub active: Duration,
    /// Backoff ceiling during errors / rate limits.
    pub backoff_max: Duration,
    /// Fraction of the interval applied as random jitter.
    pub jitter: f64,
    /// Maximum requests per rolling hour for this source.
    pub hourly_budget: u32,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            normal: Duration::from_secs(120),
            active: Duration::from_secs(20),
            backoff_max: Duration::from_secs(600),
            jitter: 0.15,
            hourly_budget: 720,
        }
    }
}

/// Inputs the policy weighs to pick the next interval.
#[derive(Debug, Clone, Default)]
pub struct PollingSignals {
    pub health: Option<SourceHealthState>,
    /// Consecutive failures observed so far.
    pub consecutive_failures: u32,
    /// A campaign / high-interest window is active.
    pub campaign_active: bool,
    /// A meaningful upstream change was seen recently (temporary fast refresh).
    pub recent_change: bool,
    /// Rate-limit hint retry-after, if the source provided one.
    pub retry_after: Option<Duration>,
}

/// Compute the next poll delay for a source. Rate-limit hints and error backoff
/// take precedence; then campaign/recent-change shorten; else normal.
pub fn next_interval(config: &PollingConfig, signals: &PollingSignals) -> Duration {
    // Rate limited: obey the hint (bounded), else back off.
    if signals.health == Some(SourceHealthState::RateLimited) {
        let d = signals
            .retry_after
            .unwrap_or(config.backoff_max)
            .min(config.backoff_max)
            .max(config.normal);
        return jitter(d, config.jitter);
    }

    // Errors: capped exponential backoff on the normal interval.
    if signals.consecutive_failures > 0 {
        let factor = 2u32.saturating_pow(signals.consecutive_failures.saturating_sub(1).min(16));
        let d = config.normal.saturating_mul(factor).min(config.backoff_max);
        return jitter(d, config.jitter);
    }

    // Healthy: campaign or a recent change warrants faster polling.
    let base = if signals.campaign_active || signals.recent_change {
        config.active
    } else {
        config.normal
    };
    jitter(base, config.jitter)
}

fn jitter(base: Duration, jitter: f64) -> Duration {
    if jitter <= 0.0 {
        return base;
    }
    let spread = base.as_secs_f64() * jitter.clamp(0.0, 1.0);
    // Deterministic-ish jitter without RNG: symmetric using nanos parity.
    let sign = if base.subsec_nanos().is_multiple_of(2) {
        1.0
    } else {
        -1.0
    };
    Duration::from_secs_f64((base.as_secs_f64() + sign * spread * 0.5).max(0.0))
}

/// Rolling per-source request budget. Rejects a request when the hourly ceiling
/// is reached, so no collector can loop unbounded.
#[derive(Debug)]
pub struct RequestBudget {
    limit: u32,
    window: Duration,
    /// Monotonic-ish timestamps of recent requests (seconds since an epoch the
    /// caller provides).
    timestamps: Vec<u64>,
}

impl RequestBudget {
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            window: Duration::from_secs(3600),
            timestamps: Vec::new(),
        }
    }

    /// Try to consume one request slot at time `now_secs`. Returns false when
    /// the budget is exhausted for the current window.
    pub fn try_consume(&mut self, now_secs: u64) -> bool {
        // Keep timestamps whose age is still within the window.
        let window = self.window.as_secs();
        self.timestamps
            .retain(|&t| now_secs.saturating_sub(t) < window);
        if (self.timestamps.len() as u32) >= self.limit {
            return false;
        }
        self.timestamps.push(now_secs);
        true
    }

    pub fn used(&self) -> usize {
        self.timestamps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_hint_is_obeyed_within_bounds() {
        let config = PollingConfig {
            jitter: 0.0,
            ..PollingConfig::default()
        };
        let d = next_interval(
            &config,
            &PollingSignals {
                health: Some(SourceHealthState::RateLimited),
                retry_after: Some(Duration::from_secs(300)),
                ..Default::default()
            },
        );
        assert_eq!(d, Duration::from_secs(300));
    }

    #[test]
    fn errors_back_off_exponentially_and_cap() {
        let config = PollingConfig {
            jitter: 0.0,
            normal: Duration::from_secs(100),
            backoff_max: Duration::from_secs(600),
            ..PollingConfig::default()
        };
        let mk = |f| {
            next_interval(
                &config,
                &PollingSignals {
                    consecutive_failures: f,
                    ..Default::default()
                },
            )
        };
        assert_eq!(mk(1), Duration::from_secs(100));
        assert_eq!(mk(2), Duration::from_secs(200));
        assert_eq!(mk(3), Duration::from_secs(400));
        assert_eq!(mk(4), Duration::from_secs(600)); // capped
    }

    #[test]
    fn campaign_and_recent_change_shorten_interval() {
        let config = PollingConfig {
            jitter: 0.0,
            normal: Duration::from_secs(120),
            active: Duration::from_secs(20),
            ..PollingConfig::default()
        };
        assert_eq!(
            next_interval(
                &config,
                &PollingSignals {
                    campaign_active: true,
                    ..Default::default()
                }
            ),
            Duration::from_secs(20)
        );
        assert_eq!(
            next_interval(&config, &PollingSignals::default()),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn request_budget_enforces_hourly_ceiling() {
        let mut budget = RequestBudget::new(3);
        assert!(budget.try_consume(0));
        assert!(budget.try_consume(10));
        assert!(budget.try_consume(20));
        assert!(!budget.try_consume(30)); // exhausted within the hour
                                          // Once every earlier request ages out (>1h), slots free up.
        assert!(budget.try_consume(3700));
        assert_eq!(budget.used(), 1);
    }
}
