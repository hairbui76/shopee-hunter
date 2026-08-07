//! Alert evaluation (ROADMAP Phase 22). Turns health/metric conditions into a
//! finite set of actionable alerts, with per-alert cooldown so a persistent
//! condition does not spam. The notifier delivers what this produces.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AlertKind {
    NoCollectorRun,
    SessionUnhealthyNearClaim,
    SchedulerLagHigh,
    DatabaseUnavailable,
    RepeatedUnknownResponses,
    NotifierDeliveryFailing,
    ProcessRestartLoop,
}

impl AlertKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoCollectorRun => "NO_COLLECTOR_RUN",
            Self::SessionUnhealthyNearClaim => "SESSION_UNHEALTHY_NEAR_CLAIM",
            Self::SchedulerLagHigh => "SCHEDULER_LAG_HIGH",
            Self::DatabaseUnavailable => "DATABASE_UNAVAILABLE",
            Self::RepeatedUnknownResponses => "REPEATED_UNKNOWN_RESPONSES",
            Self::NotifierDeliveryFailing => "NOTIFIER_DELIVERY_FAILING",
            Self::ProcessRestartLoop => "PROCESS_RESTART_LOOP",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub kind: AlertKind,
    pub detail: String,
}

/// Thresholds driving alert evaluation. All configurable.
#[derive(Debug, Clone)]
pub struct AlertThresholds {
    pub collector_stale: Duration,
    pub scheduler_lag_ms: i64,
    pub unknown_response_count: u64,
    pub notifier_failure_count: u64,
    pub restart_count_window: u64,
    /// Suppress re-firing the same alert kind within this cooldown.
    pub cooldown: Duration,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            collector_stale: Duration::minutes(15),
            scheduler_lag_ms: 2_000,
            unknown_response_count: 3,
            notifier_failure_count: 5,
            restart_count_window: 3,
            cooldown: Duration::minutes(30),
        }
    }
}

/// Snapshot of conditions to evaluate. The caller assembles it from metrics,
/// health, and repository queries.
#[derive(Debug, Clone, Default)]
pub struct AlertInputs {
    pub last_collector_success: Option<DateTime<Utc>>,
    pub session_unhealthy_with_claim_soon: bool,
    pub scheduler_lag_ms: Option<i64>,
    pub database_ok: bool,
    pub unknown_response_count: u64,
    pub notifier_failure_count: u64,
    pub restart_count: u64,
}

impl AlertInputs {
    /// Sensible baseline: DB is assumed reachable unless told otherwise.
    pub fn healthy() -> Self {
        Self {
            database_ok: true,
            ..Default::default()
        }
    }
}

/// Evaluates conditions into alerts with per-kind cooldown.
pub struct AlertEvaluator {
    thresholds: AlertThresholds,
    last_fired: HashMap<AlertKind, DateTime<Utc>>,
}

impl AlertEvaluator {
    pub fn new(thresholds: AlertThresholds) -> Self {
        Self {
            thresholds,
            last_fired: HashMap::new(),
        }
    }

    /// Evaluate and return alerts that are firing now and past cooldown.
    pub fn evaluate(&mut self, inputs: &AlertInputs, now: DateTime<Utc>) -> Vec<Alert> {
        let mut candidates: Vec<Alert> = Vec::new();

        match inputs.last_collector_success {
            Some(t) if now - t > self.thresholds.collector_stale => candidates.push(Alert {
                kind: AlertKind::NoCollectorRun,
                detail: format!("no successful collector run since {}", t.to_rfc3339()),
            }),
            None => candidates.push(Alert {
                kind: AlertKind::NoCollectorRun,
                detail: "no successful collector run recorded".into(),
            }),
            _ => {}
        }
        if inputs.session_unhealthy_with_claim_soon {
            candidates.push(Alert {
                kind: AlertKind::SessionUnhealthyNearClaim,
                detail: "session not healthy with a claim scheduled soon".into(),
            });
        }
        if let Some(lag) = inputs.scheduler_lag_ms {
            if lag > self.thresholds.scheduler_lag_ms {
                candidates.push(Alert {
                    kind: AlertKind::SchedulerLagHigh,
                    detail: format!("scheduler lag {lag}ms"),
                });
            }
        }
        if !inputs.database_ok {
            candidates.push(Alert {
                kind: AlertKind::DatabaseUnavailable,
                detail: "database unavailable".into(),
            });
        }
        if inputs.unknown_response_count >= self.thresholds.unknown_response_count {
            candidates.push(Alert {
                kind: AlertKind::RepeatedUnknownResponses,
                detail: format!("{} unknown Shopee responses", inputs.unknown_response_count),
            });
        }
        if inputs.notifier_failure_count >= self.thresholds.notifier_failure_count {
            candidates.push(Alert {
                kind: AlertKind::NotifierDeliveryFailing,
                detail: format!("{} notifier failures", inputs.notifier_failure_count),
            });
        }
        if inputs.restart_count >= self.thresholds.restart_count_window {
            candidates.push(Alert {
                kind: AlertKind::ProcessRestartLoop,
                detail: format!("{} restarts in window", inputs.restart_count),
            });
        }

        candidates
            .into_iter()
            .filter(|a| self.past_cooldown(a.kind, now))
            .map(|a| {
                self.last_fired.insert(a.kind, now);
                a
            })
            .collect()
    }

    fn past_cooldown(&self, kind: AlertKind, now: DateTime<Utc>) -> bool {
        match self.last_fired.get(&kind) {
            Some(last) => now - *last >= self.thresholds.cooldown,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_expected_alerts_and_respects_cooldown() {
        let mut ev = AlertEvaluator::new(AlertThresholds::default());
        let now = Utc::now();

        let mut inputs = AlertInputs::healthy();
        inputs.database_ok = false;
        inputs.scheduler_lag_ms = Some(5_000);

        let alerts = ev.evaluate(&inputs, now);
        let kinds: Vec<_> = alerts.iter().map(|a| a.kind).collect();
        assert!(kinds.contains(&AlertKind::DatabaseUnavailable));
        assert!(kinds.contains(&AlertKind::SchedulerLagHigh));
        // NoCollectorRun also fires (no success recorded).
        assert!(kinds.contains(&AlertKind::NoCollectorRun));

        // Immediately re-evaluating the same condition is suppressed by cooldown.
        let again = ev.evaluate(&inputs, now + Duration::minutes(1));
        assert!(again.is_empty());

        // After the cooldown, it fires again.
        let later = ev.evaluate(&inputs, now + Duration::minutes(31));
        assert!(!later.is_empty());
    }

    #[test]
    fn healthy_inputs_with_recent_collector_are_quiet() {
        let mut ev = AlertEvaluator::new(AlertThresholds::default());
        let now = Utc::now();
        let mut inputs = AlertInputs::healthy();
        inputs.last_collector_success = Some(now - Duration::minutes(1));
        inputs.scheduler_lag_ms = Some(100);
        assert!(ev.evaluate(&inputs, now).is_empty());
    }
}
