//! Precision reports (ROADMAP Phase 31). For each executed action, capture the
//! planned vs actual timing and the network round-trip so scheduler/network
//! latency can be analyzed from data rather than intuition.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// One claim-attempt timing record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrecisionReport {
    pub planned_at: DateTime<Utc>,
    pub sent_at: DateTime<Utc>,
    pub response_at: Option<DateTime<Utc>>,
    /// sent_at - planned_at, in milliseconds (positive = fired late).
    pub delta_ms: i64,
    /// response_at - sent_at, in milliseconds (network + upstream time).
    pub network_latency_ms: Option<i64>,
}

impl PrecisionReport {
    pub fn new(
        planned_at: DateTime<Utc>,
        sent_at: DateTime<Utc>,
        response_at: Option<DateTime<Utc>>,
    ) -> Self {
        let delta_ms = (sent_at - planned_at).num_milliseconds();
        let network_latency_ms = response_at.map(|r| (r - sent_at).num_milliseconds());
        Self {
            planned_at,
            sent_at,
            response_at,
            delta_ms,
            network_latency_ms,
        }
    }

    /// Whether execution lag exceeded a threshold (drives the scheduler-lag alert).
    pub fn is_late_beyond(&self, threshold_ms: i64) -> bool {
        self.delta_ms > threshold_ms
    }
}

/// Rolling aggregate of precision reports for reporting/metrics.
#[derive(Debug, Clone, Default)]
pub struct PrecisionStats {
    count: u64,
    sum_delta_ms: i64,
    max_delta_ms: i64,
    late_count: u64,
}

impl PrecisionStats {
    pub fn record(&mut self, report: &PrecisionReport, late_threshold_ms: i64) {
        self.count += 1;
        self.sum_delta_ms += report.delta_ms;
        self.max_delta_ms = self.max_delta_ms.max(report.delta_ms);
        if report.is_late_beyond(late_threshold_ms) {
            self.late_count += 1;
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn mean_delta_ms(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_delta_ms as f64 / self.count as f64
        }
    }

    pub fn max_delta_ms(&self) -> i64 {
        self.max_delta_ms
    }

    pub fn late_count(&self) -> u64 {
        self.late_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_deltas_and_network_latency() {
        let planned = Utc::now();
        let sent = planned + chrono::Duration::milliseconds(3);
        let resp = sent + chrono::Duration::milliseconds(120);
        let r = PrecisionReport::new(planned, sent, Some(resp));
        assert_eq!(r.delta_ms, 3);
        assert_eq!(r.network_latency_ms, Some(120));
        assert!(!r.is_late_beyond(2000));
    }

    #[test]
    fn stats_aggregate() {
        let planned = Utc::now();
        let mut stats = PrecisionStats::default();
        for lag in [1, 5, 3000] {
            let sent = planned + chrono::Duration::milliseconds(lag);
            stats.record(&PrecisionReport::new(planned, sent, None), 2000);
        }
        assert_eq!(stats.count(), 3);
        assert_eq!(stats.max_delta_ms(), 3000);
        assert_eq!(stats.late_count(), 1); // only the 3000ms one is late
        assert!((stats.mean_delta_ms() - 1002.0).abs() < 0.01);
    }
}
