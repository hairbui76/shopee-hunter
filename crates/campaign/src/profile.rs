//! Polling and notification postures.
//!
//! A [`PollProfile`] is a *name* for a situation; a [`PollPosture`] is the
//! *data* that name maps to. Collectors consult the posture and never branch on
//! the profile name, so retuning campaign behaviour is a configuration edit,
//! not a code change (ROADMAP Phase 30 exit criterion).
//!
//! Postures scale a collector's own base interval rather than replacing it, so
//! a source that is naturally slow stays proportionally slow. Every posture
//! carries a floor: no configuration can make the system poll without delay
//! (CLAUDE.md — speed comes from architecture and warm state, never from
//! request volume).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The situation the scheduler believes it is in.
///
/// The derived `Ord` exists only so this can key a `BTreeMap` in config; use
/// [`PollProfile::priority`] for "which profile wins" decisions.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PollProfile {
    /// No campaign is near.
    #[default]
    Normal,
    /// A campaign starts soon: warm up.
    PreCampaign,
    /// A campaign is running.
    CampaignActive,
    /// A campaign just ended: back off deliberately.
    Recovery,
}

impl PollProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::PreCampaign => "PRE_CAMPAIGN",
            Self::CampaignActive => "CAMPAIGN_ACTIVE",
            Self::Recovery => "RECOVERY",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "NORMAL" => Self::Normal,
            "PRE_CAMPAIGN" => Self::PreCampaign,
            "CAMPAIGN_ACTIVE" => Self::CampaignActive,
            "RECOVERY" => Self::Recovery,
            _ => return None,
        })
    }

    /// Which profile wins when several campaigns overlap.
    ///
    /// An active campaign outranks an imminent one, which outranks a tail-off:
    /// the most demanding situation sets the posture.
    pub fn priority(&self) -> u8 {
        match self {
            Self::CampaignActive => 3,
            Self::PreCampaign => 2,
            Self::Recovery => 1,
            Self::Normal => 0,
        }
    }

    /// Built-in posture, used when the calendar does not override it.
    pub fn default_posture(&self) -> PollPosture {
        match self {
            Self::Normal => PollPosture {
                interval_percent: 100,
                min_interval_secs: 30,
                max_interval_secs: 3_600,
            },
            Self::PreCampaign => PollPosture {
                interval_percent: 50,
                min_interval_secs: 20,
                max_interval_secs: 900,
            },
            Self::CampaignActive => PollPosture {
                interval_percent: 25,
                min_interval_secs: 10,
                max_interval_secs: 300,
            },
            Self::Recovery => PollPosture {
                interval_percent: 200,
                min_interval_secs: 60,
                max_interval_secs: 7_200,
            },
        }
    }

    /// Every profile, in priority order (lowest first). Useful for rendering
    /// configuration tables.
    pub fn all() -> [PollProfile; 4] {
        [
            Self::Normal,
            Self::Recovery,
            Self::PreCampaign,
            Self::CampaignActive,
        ]
    }
}

/// How aggressively to poll, as pure data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PollPosture {
    /// Scaling of a collector's base interval, in percent. `100` leaves it
    /// unchanged, `25` polls four times as often, `200` halves the rate.
    pub interval_percent: u32,
    /// Hard floor: the resulting interval is never shorter than this.
    pub min_interval_secs: u64,
    /// Ceiling, so a slow source still gets checked during a campaign.
    pub max_interval_secs: u64,
}

impl Default for PollPosture {
    fn default() -> Self {
        PollProfile::Normal.default_posture()
    }
}

impl PollPosture {
    /// Apply this posture to a collector's base interval.
    ///
    /// Saturating integer arithmetic throughout: no float, and no overflow
    /// however odd the configured numbers are.
    pub fn apply(&self, base: Duration) -> Duration {
        let scaled_ms = base
            .as_millis()
            .saturating_mul(u128::from(self.interval_percent))
            / 100;
        let floor_ms = u128::from(self.min_interval_secs).saturating_mul(1_000);
        let ceiling_ms =
            u128::from(self.max_interval_secs.max(self.min_interval_secs)).saturating_mul(1_000);

        let clamped = scaled_ms.clamp(floor_ms, ceiling_ms);
        Duration::from_millis(u64::try_from(clamped).unwrap_or(u64::MAX))
    }

    /// One-line summary for operator output.
    pub fn describe(&self) -> String {
        format!(
            "{}% of base interval, clamped to {}s..{}s",
            self.interval_percent, self.min_interval_secs, self.max_interval_secs
        )
    }
}

/// How loudly to notify while a campaign is running.
///
/// Campaigns produce far more vouchers than a normal day, so the owner needs a
/// way to raise the bar without editing ranking code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationProfile {
    /// Owner-facing name, e.g. `NORMAL`, `CAMPAIGN_QUIET`.
    pub label: String,
    /// Replaces the owner's usual ranking notification threshold while active.
    pub notification_threshold: Option<i64>,
    /// Send only alert-class messages (failures, session problems).
    pub suppress_non_alerts: bool,
    /// Announce vouchers that are about to start.
    pub notify_upcoming: bool,
    /// Batch routine discoveries instead of sending them individually.
    pub digest_only: bool,
}

impl Default for NotificationProfile {
    fn default() -> Self {
        Self {
            label: "NORMAL".to_string(),
            notification_threshold: None,
            suppress_non_alerts: false,
            notify_upcoming: true,
            digest_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip_through_strings_and_serde() {
        for profile in PollProfile::all() {
            assert_eq!(PollProfile::parse(profile.as_str()), Some(profile));
            let encoded = serde_json::to_string(&profile).expect("serializes");
            assert_eq!(encoded, format!("\"{}\"", profile.as_str()));
            let decoded: PollProfile = serde_json::from_str(&encoded).expect("round trips");
            assert_eq!(decoded, profile);
        }
        assert_eq!(PollProfile::parse("BLACK_FRIDAY"), None);
    }

    #[test]
    fn priority_orders_the_most_demanding_situation_first() {
        assert!(PollProfile::CampaignActive.priority() > PollProfile::PreCampaign.priority());
        assert!(PollProfile::PreCampaign.priority() > PollProfile::Recovery.priority());
        assert!(PollProfile::Recovery.priority() > PollProfile::Normal.priority());
    }

    #[test]
    fn default_postures_get_progressively_more_eager() {
        let normal = PollProfile::Normal.default_posture();
        let pre = PollProfile::PreCampaign.default_posture();
        let active = PollProfile::CampaignActive.default_posture();
        let recovery = PollProfile::Recovery.default_posture();

        assert!(active.interval_percent < pre.interval_percent);
        assert!(pre.interval_percent < normal.interval_percent);
        assert!(recovery.interval_percent > normal.interval_percent);
        // Even the most eager posture keeps a floor.
        assert!(active.min_interval_secs >= 10);
    }

    #[test]
    fn posture_scales_the_base_interval() {
        let base = Duration::from_secs(120);
        assert_eq!(
            PollProfile::Normal.default_posture().apply(base),
            Duration::from_secs(120)
        );
        assert_eq!(
            PollProfile::PreCampaign.default_posture().apply(base),
            Duration::from_secs(60)
        );
        assert_eq!(
            PollProfile::CampaignActive.default_posture().apply(base),
            Duration::from_secs(30)
        );
        assert_eq!(
            PollProfile::Recovery.default_posture().apply(base),
            Duration::from_secs(240)
        );
    }

    #[test]
    fn posture_floor_and_ceiling_are_enforced() {
        let posture = PollPosture {
            interval_percent: 1,
            min_interval_secs: 15,
            max_interval_secs: 60,
        };
        // Scaling would give 0.6s; the floor wins.
        assert_eq!(
            posture.apply(Duration::from_secs(60)),
            Duration::from_secs(15)
        );
        // A very slow source is pulled down to the ceiling.
        assert_eq!(
            posture.apply(Duration::from_secs(100_000)),
            Duration::from_secs(60)
        );
        // Zero base still respects the floor: nothing polls with no delay.
        assert_eq!(posture.apply(Duration::ZERO), Duration::from_secs(15));
    }

    #[test]
    fn posture_arithmetic_never_overflows() {
        let posture = PollPosture {
            interval_percent: u32::MAX,
            min_interval_secs: 1,
            max_interval_secs: u64::MAX,
        };
        let applied = posture.apply(Duration::from_secs(u64::MAX));
        assert!(applied >= Duration::from_secs(1));
    }

    #[test]
    fn postures_and_notification_profiles_deserialize_partially() {
        let posture: PollPosture =
            serde_json::from_str(r#"{"interval_percent": 40}"#).expect("partial posture");
        assert_eq!(posture.interval_percent, 40);
        assert_eq!(posture.min_interval_secs, 30);

        let notification: NotificationProfile =
            serde_json::from_str(r#"{"label":"CAMPAIGN_QUIET","digest_only":true}"#)
                .expect("partial notification profile");
        assert_eq!(notification.label, "CAMPAIGN_QUIET");
        assert!(notification.digest_only);
        assert!(notification.notify_upcoming);
        assert_eq!(notification.notification_threshold, None);
    }

    #[test]
    fn describe_is_operator_readable() {
        let text = PollProfile::CampaignActive.default_posture().describe();
        assert!(text.contains("25%"));
        assert!(text.contains("10s..300s"));
    }
}
