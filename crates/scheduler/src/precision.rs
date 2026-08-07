//! Precision execution window. Converts a target wall-clock time into a
//! monotonic Tokio deadline and waits with `sleep_until`, then measures the
//! execution lag (actual minus planned). Nothing here does DB or browser work
//! on the final wait — callers preload the claim plan beforehand.

use std::time::Duration;

use chrono::{DateTime, Utc};
use shopee_hunter_domain::clock::Clock;

/// Wall-clock delay from `now` until `target`, clamped to zero (a target in
/// the past means "run immediately"). Pure and testable.
pub fn monotonic_delay(now: DateTime<Utc>, target: DateTime<Utc>) -> Duration {
    let delta = target - now;
    if delta <= chrono::Duration::zero() {
        Duration::ZERO
    } else {
        delta.to_std().unwrap_or(Duration::ZERO)
    }
}

/// Report of a precision wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub planned_at: DateTime<Utc>,
    pub woke_at: DateTime<Utc>,
    /// Signed lag in milliseconds (positive = late). Firing early yields a
    /// small negative value.
    pub lag_ms: i64,
}

/// Executes an action at a precise target time using a monotonic deadline.
pub struct PrecisionRunner<C: Clock> {
    clock: C,
}

impl<C: Clock> PrecisionRunner<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    /// Wait until `target` (monotonic deadline computed once from the current
    /// wall time), then return a lag report. The caller runs the action after
    /// this resolves, keeping the T=0 path free of scheduling logic.
    pub async fn wait_until(&self, target: DateTime<Utc>) -> ExecutionReport {
        let now = self.clock.now_utc();
        let delay = monotonic_delay(now, target);
        let deadline = tokio::time::Instant::now() + delay;
        tokio::time::sleep_until(deadline).await;
        let woke_at = self.clock.now_utc();
        ExecutionReport {
            planned_at: target,
            woke_at,
            lag_ms: (woke_at - target).num_milliseconds(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    /// A clock whose wall time is driven manually (milliseconds since epoch).
    #[derive(Clone)]
    struct TestClock {
        millis: Arc<AtomicI64>,
    }
    impl TestClock {
        fn new(start: DateTime<Utc>) -> Self {
            Self {
                millis: Arc::new(AtomicI64::new(start.timestamp_millis())),
            }
        }
        fn set(&self, t: DateTime<Utc>) {
            self.millis.store(t.timestamp_millis(), Ordering::SeqCst);
        }
    }
    impl Clock for TestClock {
        fn now_utc(&self) -> DateTime<Utc> {
            DateTime::from_timestamp_millis(self.millis.load(Ordering::SeqCst)).unwrap()
        }
    }

    #[test]
    fn delay_clamps_past_targets_to_zero() {
        let now = Utc::now();
        assert_eq!(
            monotonic_delay(now, now + chrono::Duration::seconds(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            monotonic_delay(now, now - chrono::Duration::seconds(5)),
            Duration::ZERO
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_until_fires_at_target_with_low_lag() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let clock = TestClock::new(start);
        let runner = PrecisionRunner::new(clock.clone());
        let target = start + chrono::Duration::seconds(10);

        // Drive the fake wall clock forward in lockstep with tokio's paused
        // time so the lag measurement reflects the monotonic wait.
        let clock_task = clock.clone();
        let advancer = tokio::spawn(async move {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let cur = clock_task.now_utc();
                clock_task.set(cur + chrono::Duration::seconds(1));
            }
        });

        let report = runner.wait_until(target).await;
        advancer.await.unwrap();
        assert_eq!(report.planned_at, target);
        // Fired within a second of target.
        assert!(report.lag_ms.abs() <= 1000, "lag was {}", report.lag_ms);
    }

    #[tokio::test(start_paused = true)]
    async fn past_target_fires_immediately() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let clock = TestClock::new(start);
        let runner = PrecisionRunner::new(clock.clone());
        let report = runner
            .wait_until(start - chrono::Duration::seconds(30))
            .await;
        assert!(report.lag_ms >= 0);
    }
}
