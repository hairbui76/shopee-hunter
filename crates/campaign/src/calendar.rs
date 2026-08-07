//! Campaign calendar model (ROADMAP Phase 30).
//!
//! Campaign dates are **data**. Nothing in this crate — and nothing in a worker
//! consuming it — should contain a literal campaign date, so moving 9.9 or
//! adding 12.12 is a configuration edit and never a deployment.
//!
//! Times are stored in UTC and rendered in `Asia/Ho_Chi_Minh` only for owner
//! output, matching the rest of the project.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use shopee_hunter_domain::clock::format_display;

use crate::error::CampaignError;
use crate::profile::{NotificationProfile, PollPosture, PollProfile};

/// Default warm-up ahead of a campaign: one day.
pub const DEFAULT_LEAD_MINUTES: i64 = 24 * 60;
/// Default tail-off after a campaign: two hours.
pub const DEFAULT_RECOVERY_MINUTES: i64 = 2 * 60;
/// Upper bound on lead/recovery offsets, so a typo cannot make a campaign
/// permanently "imminent".
pub const MAX_OFFSET_MINUTES: i64 = 365 * 24 * 60;

/// A period of heightened interest inside a campaign, e.g. a midnight drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighInterestWindow {
    pub label: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    /// Profile to force while this window is open. `None` keeps the campaign's
    /// own profile.
    #[serde(default)]
    pub poll_profile: Option<PollProfile>,
}

impl HighInterestWindow {
    pub fn new(label: impl Into<String>, start_at: DateTime<Utc>, end_at: DateTime<Utc>) -> Self {
        Self {
            label: label.into(),
            start_at,
            end_at,
            poll_profile: None,
        }
    }

    pub fn with_poll_profile(mut self, profile: PollProfile) -> Self {
        self.poll_profile = Some(profile);
        self
    }

    /// Half-open `[start, end)`, so adjacent windows never both match.
    pub fn contains(&self, now: DateTime<Utc>) -> bool {
        now >= self.start_at && now < self.end_at
    }

    /// Owner-facing window in Vietnam local time.
    pub fn display_window(&self) -> String {
        format!(
            "{} -> {} (GMT+7)",
            format_display(self.start_at),
            format_display(self.end_at)
        )
    }
}

/// Per-source behaviour while a campaign governs the schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceOverride {
    /// Turn a source off (or back on) for the duration.
    pub enabled: Option<bool>,
    /// Source-specific interval scaling, overriding the profile posture.
    pub interval_percent: Option<u32>,
    /// Why this override exists, for the operator.
    pub note: Option<String>,
}

/// One campaign and everything that changes while it is near.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    /// Stable identifier, e.g. `9.9-2026`.
    pub id: String,
    /// Owner-facing name.
    pub name: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    /// Disabled campaigns are ignored entirely but stay in the file for
    /// next year.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long before `start_at` the system warms up.
    #[serde(default = "default_lead")]
    pub lead_minutes: i64,
    /// How long after `end_at` the system stays in recovery.
    #[serde(default = "default_recovery")]
    pub recovery_minutes: i64,
    #[serde(default)]
    pub high_interest_windows: Vec<HighInterestWindow>,
    /// Keyed by collector source id.
    #[serde(default)]
    pub source_overrides: BTreeMap<String, SourceOverride>,
    /// Profile to use while the campaign is running. Defaults to
    /// [`PollProfile::CampaignActive`].
    #[serde(default)]
    pub poll_profile: Option<PollProfile>,
    /// Notification behaviour while this campaign governs.
    #[serde(default)]
    pub notification_profile: Option<NotificationProfile>,
}

fn default_true() -> bool {
    true
}

fn default_lead() -> i64 {
    DEFAULT_LEAD_MINUTES
}

fn default_recovery() -> i64 {
    DEFAULT_RECOVERY_MINUTES
}

impl Campaign {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        start_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            start_at,
            end_at,
            enabled: true,
            lead_minutes: DEFAULT_LEAD_MINUTES,
            recovery_minutes: DEFAULT_RECOVERY_MINUTES,
            high_interest_windows: Vec::new(),
            source_overrides: BTreeMap::new(),
            poll_profile: None,
            notification_profile: None,
        }
    }

    pub fn with_lead_minutes(mut self, minutes: i64) -> Self {
        self.lead_minutes = minutes;
        self
    }

    pub fn with_recovery_minutes(mut self, minutes: i64) -> Self {
        self.recovery_minutes = minutes;
        self
    }

    pub fn with_window(mut self, window: HighInterestWindow) -> Self {
        self.high_interest_windows.push(window);
        self
    }

    pub fn with_source_override(
        mut self,
        source: impl Into<String>,
        override_: SourceOverride,
    ) -> Self {
        self.source_overrides.insert(source.into(), override_);
        self
    }

    pub fn with_notification_profile(mut self, profile: NotificationProfile) -> Self {
        self.notification_profile = Some(profile);
        self
    }

    pub fn with_poll_profile(mut self, profile: PollProfile) -> Self {
        self.poll_profile = Some(profile);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// When warm-up begins. Falls back to `start_at` if the offset cannot be
    /// represented, so a broken value can never widen the window.
    pub fn warmup_start(&self) -> DateTime<Utc> {
        offset(self.start_at, -self.lead_minutes).unwrap_or(self.start_at)
    }

    /// When recovery ends. Falls back to `end_at` on overflow.
    pub fn recovery_end(&self) -> DateTime<Utc> {
        offset(self.end_at, self.recovery_minutes).unwrap_or(self.end_at)
    }

    /// Campaign proper, `[start, end)`.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.enabled && now >= self.start_at && now < self.end_at
    }

    /// The profile this campaign implies at `now`, or `None` when it has no
    /// opinion (disabled, or `now` is outside warm-up..recovery).
    pub fn phase(&self, now: DateTime<Utc>) -> Option<PollProfile> {
        if !self.enabled {
            return None;
        }
        if now >= self.start_at && now < self.end_at {
            return Some(self.poll_profile.unwrap_or(PollProfile::CampaignActive));
        }
        if now >= self.warmup_start() && now < self.start_at {
            return Some(PollProfile::PreCampaign);
        }
        if now >= self.end_at && now < self.recovery_end() {
            return Some(PollProfile::Recovery);
        }
        None
    }

    /// The high-interest window covering `now`, if any.
    ///
    /// Windows are checked in configuration order, so overlapping windows
    /// resolve deterministically to the first one declared.
    pub fn high_interest_window(&self, now: DateTime<Utc>) -> Option<&HighInterestWindow> {
        if !self.enabled {
            return None;
        }
        self.high_interest_windows
            .iter()
            .find(|window| window.contains(now))
    }

    pub fn source_override(&self, source: &str) -> Option<&SourceOverride> {
        self.source_overrides.get(source)
    }

    /// Owner-facing campaign window in Vietnam local time.
    pub fn display_window(&self) -> String {
        format!(
            "{} -> {} (GMT+7)",
            format_display(self.start_at),
            format_display(self.end_at)
        )
    }

    fn validate(&self, issues: &mut Vec<CampaignError>) {
        if self.id.trim().is_empty() {
            issues.push(CampaignError::BlankField {
                campaign: self.id.clone(),
                field: "id",
            });
        }
        if self.name.trim().is_empty() {
            issues.push(CampaignError::BlankField {
                campaign: self.id.clone(),
                field: "name",
            });
        }
        if self.start_at >= self.end_at {
            issues.push(CampaignError::InvalidCampaignWindow {
                campaign: self.id.clone(),
                start: self.start_at,
                end: self.end_at,
            });
        }
        for (field, value) in [
            ("lead_minutes", self.lead_minutes),
            ("recovery_minutes", self.recovery_minutes),
        ] {
            if !(0..=MAX_OFFSET_MINUTES).contains(&value) {
                issues.push(CampaignError::OffsetOutOfRange {
                    campaign: self.id.clone(),
                    field,
                    value,
                    max: MAX_OFFSET_MINUTES,
                });
            }
        }

        for window in &self.high_interest_windows {
            if window.start_at >= window.end_at {
                issues.push(CampaignError::InvalidHighInterestWindow {
                    campaign: self.id.clone(),
                    window: window.label.clone(),
                    start: window.start_at,
                    end: window.end_at,
                });
                continue;
            }
            // A window outside its campaign would never fire, which is almost
            // certainly a typo rather than an intention.
            if window.start_at < self.start_at || window.end_at > self.end_at {
                issues.push(CampaignError::WindowOutsideCampaign {
                    campaign: self.id.clone(),
                    window: window.label.clone(),
                    start: window.start_at,
                    end: window.end_at,
                });
            }
        }

        for (source, override_) in &self.source_overrides {
            if source.trim().is_empty() {
                issues.push(CampaignError::BlankSourceOverride {
                    campaign: self.id.clone(),
                });
            }
            if override_.interval_percent == Some(0) {
                issues.push(CampaignError::ZeroIntervalPercent {
                    context: format!("campaign `{}` source `{source}`", self.id),
                });
            }
        }
    }
}

/// Shift a timestamp by whole minutes without panicking on absurd input.
fn offset(base: DateTime<Utc>, minutes: i64) -> Option<DateTime<Utc>> {
    Duration::try_minutes(minutes).and_then(|delta| base.checked_add_signed(delta))
}

/// Every campaign the deployment knows about, plus the posture table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CampaignCalendar {
    pub campaigns: Vec<Campaign>,
    /// Overrides for the built-in [`PollProfile::default_posture`] table.
    /// Anything omitted keeps its built-in value.
    pub postures: BTreeMap<PollProfile, PollPosture>,
    /// Notification behaviour when no campaign governs.
    pub default_notification: NotificationProfile,
}

impl CampaignCalendar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_campaign(mut self, campaign: Campaign) -> Self {
        self.campaigns.push(campaign);
        self
    }

    pub fn with_posture(mut self, profile: PollProfile, posture: PollPosture) -> Self {
        self.postures.insert(profile, posture);
        self
    }

    pub fn with_default_notification(mut self, profile: NotificationProfile) -> Self {
        self.default_notification = profile;
        self
    }

    /// Parse a calendar from JSON and validate it in one step.
    ///
    /// The same `serde` model deserializes from any self-describing format, so
    /// a TOML or YAML loader can reuse [`CampaignCalendar::validate`] directly.
    pub fn from_json(raw: &str) -> Result<Self, Vec<CampaignError>> {
        let calendar: Self = serde_json::from_str(raw).map_err(|err| {
            vec![CampaignError::Parse {
                detail: err.to_string(),
            }]
        })?;
        calendar.validate()?;
        Ok(calendar)
    }

    pub fn campaign(&self, id: &str) -> Option<&Campaign> {
        self.campaigns.iter().find(|campaign| campaign.id == id)
    }

    /// Posture for a profile: the configured override if present, else the
    /// built-in default.
    pub fn posture_for(&self, profile: PollProfile) -> PollPosture {
        self.postures
            .get(&profile)
            .copied()
            .unwrap_or_else(|| profile.default_posture())
    }

    pub fn is_empty(&self) -> bool {
        self.campaigns.is_empty()
    }

    /// Report every configuration problem at once.
    pub fn validate(&self) -> Result<(), Vec<CampaignError>> {
        let mut issues = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();

        for campaign in &self.campaigns {
            campaign.validate(&mut issues);
            if !seen.insert(campaign.id.as_str()) {
                issues.push(CampaignError::DuplicateCampaign {
                    id: campaign.id.clone(),
                });
            }
        }

        for (profile, posture) in &self.postures {
            let context = format!("profile {}", profile.as_str());
            if posture.interval_percent == 0 {
                issues.push(CampaignError::ZeroIntervalPercent {
                    context: context.clone(),
                });
            }
            if posture.min_interval_secs == 0
                || posture.min_interval_secs > posture.max_interval_secs
            {
                issues.push(CampaignError::InvalidPostureBounds {
                    context,
                    min: posture.min_interval_secs,
                    max: posture.max_interval_secs,
                });
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        // `expect` (test-only): a fixture with an impossible hour must fail
        // loudly rather than silently collapsing to the Unix epoch.
        Utc.with_ymd_and_hms(2026, 9, day, hour, 0, 0)
            .single()
            .expect("valid fixture timestamp")
    }

    fn campaign() -> Campaign {
        Campaign::new("9.9-2026", "9.9 Super Sale", at(9, 0), at(10, 0))
    }

    #[test]
    fn phase_moves_through_warmup_active_and_recovery() {
        let campaign = campaign().with_lead_minutes(120).with_recovery_minutes(60);

        assert_eq!(campaign.warmup_start(), at(8, 22));
        assert_eq!(campaign.recovery_end(), at(10, 1));

        assert_eq!(campaign.phase(at(8, 21)), None);
        assert_eq!(campaign.phase(at(8, 22)), Some(PollProfile::PreCampaign));
        assert_eq!(campaign.phase(at(8, 23)), Some(PollProfile::PreCampaign));
        assert_eq!(campaign.phase(at(9, 0)), Some(PollProfile::CampaignActive));
        assert_eq!(campaign.phase(at(9, 23)), Some(PollProfile::CampaignActive));
        assert_eq!(campaign.phase(at(10, 0)), Some(PollProfile::Recovery));
        assert_eq!(campaign.phase(at(10, 1)), None);
    }

    #[test]
    fn campaign_boundaries_are_half_open() {
        let campaign = campaign();
        assert!(!campaign.is_active(at(8, 23)));
        assert!(campaign.is_active(at(9, 0)));
        assert!(!campaign.is_active(at(10, 0)));
    }

    #[test]
    fn disabled_campaigns_have_no_opinion() {
        let campaign = campaign().disabled();
        assert_eq!(campaign.phase(at(9, 12)), None);
        assert!(!campaign.is_active(at(9, 12)));
        assert!(campaign.high_interest_window(at(9, 12)).is_none());
    }

    #[test]
    fn a_campaign_can_declare_its_own_active_profile() {
        let campaign = campaign().with_poll_profile(PollProfile::PreCampaign);
        assert_eq!(campaign.phase(at(9, 12)), Some(PollProfile::PreCampaign));
    }

    #[test]
    fn high_interest_windows_are_half_open_and_ordered() {
        let campaign = campaign()
            .with_window(HighInterestWindow::new("midnight drop", at(9, 0), at(9, 2)))
            .with_window(HighInterestWindow::new("noon drop", at(9, 12), at(9, 14)));

        assert!(campaign.high_interest_window(at(8, 23)).is_none());
        assert_eq!(
            campaign
                .high_interest_window(at(9, 0))
                .map(|w| w.label.as_str()),
            Some("midnight drop")
        );
        assert!(campaign.high_interest_window(at(9, 2)).is_none());
        assert_eq!(
            campaign
                .high_interest_window(at(9, 13))
                .map(|w| w.label.as_str()),
            Some("noon drop")
        );
    }

    #[test]
    fn validation_rejects_inverted_campaign_windows() {
        let calendar = CampaignCalendar::new().with_campaign(Campaign::new(
            "bad",
            "Backwards",
            at(10, 0),
            at(9, 0),
        ));
        let issues = calendar.validate().expect_err("must be invalid");
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::InvalidCampaignWindow { .. })));
    }

    #[test]
    fn validation_rejects_windows_outside_their_campaign() {
        let calendar = CampaignCalendar::new().with_campaign(
            campaign()
                .with_window(HighInterestWindow::new("too early", at(8, 0), at(8, 12)))
                .with_window(HighInterestWindow::new("inverted", at(9, 5), at(9, 1)))
                .with_window(HighInterestWindow::new("fine", at(9, 1), at(9, 2))),
        );

        let issues = calendar.validate().expect_err("must be invalid");
        assert!(issues.iter().any(|i| matches!(
            i,
            CampaignError::WindowOutsideCampaign { window, .. } if window == "too early"
        )));
        assert!(issues.iter().any(|i| matches!(
            i,
            CampaignError::InvalidHighInterestWindow { window, .. } if window == "inverted"
        )));
        // The valid window contributes no issue.
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn validation_reports_duplicates_blanks_and_bad_offsets() {
        let calendar = CampaignCalendar::new()
            .with_campaign(campaign())
            .with_campaign(campaign().with_lead_minutes(-5))
            .with_campaign(Campaign::new("", "", at(9, 0), at(10, 0)));

        let issues = calendar.validate().expect_err("must be invalid");
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::DuplicateCampaign { .. })));
        assert!(issues.iter().any(|i| matches!(
            i,
            CampaignError::OffsetOutOfRange {
                field: "lead_minutes",
                ..
            }
        )));
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::BlankField { field: "id", .. })));
    }

    #[test]
    fn validation_rejects_postures_that_would_hammer_a_source() {
        let calendar = CampaignCalendar::new().with_posture(
            PollProfile::CampaignActive,
            PollPosture {
                interval_percent: 0,
                min_interval_secs: 0,
                max_interval_secs: 10,
            },
        );
        let issues = calendar.validate().expect_err("must be invalid");
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::ZeroIntervalPercent { .. })));
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::InvalidPostureBounds { .. })));

        let inverted = CampaignCalendar::new().with_posture(
            PollProfile::Normal,
            PollPosture {
                interval_percent: 100,
                min_interval_secs: 600,
                max_interval_secs: 60,
            },
        );
        assert!(inverted.validate().is_err());
    }

    #[test]
    fn posture_lookup_prefers_configuration_over_defaults() {
        let custom = PollPosture {
            interval_percent: 10,
            min_interval_secs: 5,
            max_interval_secs: 60,
        };
        let calendar = CampaignCalendar::new().with_posture(PollProfile::CampaignActive, custom);

        assert_eq!(calendar.posture_for(PollProfile::CampaignActive), custom);
        assert_eq!(
            calendar.posture_for(PollProfile::Normal),
            PollProfile::Normal.default_posture()
        );
    }

    #[test]
    fn absurd_offsets_never_panic() {
        let campaign = Campaign::new("x", "x", at(9, 0), at(10, 0))
            .with_lead_minutes(i64::MAX)
            .with_recovery_minutes(i64::MAX);
        // Falls back to the campaign bounds instead of overflowing.
        assert_eq!(campaign.warmup_start(), at(9, 0));
        assert_eq!(campaign.recovery_end(), at(10, 0));

        // Validation still flags the nonsense rather than tolerating it.
        let mut issues = Vec::new();
        campaign.validate(&mut issues);
        assert!(issues
            .iter()
            .any(|i| matches!(i, CampaignError::OffsetOutOfRange { .. })));
    }

    #[test]
    fn calendar_round_trips_through_json_config() {
        let raw = r#"{
            "campaigns": [
                {
                    "id": "9.9-2026",
                    "name": "9.9 Super Sale",
                    "start_at": "2026-09-09T00:00:00Z",
                    "end_at": "2026-09-10T00:00:00Z",
                    "lead_minutes": 180,
                    "high_interest_windows": [
                        {
                            "label": "midnight drop",
                            "start_at": "2026-09-09T00:00:00Z",
                            "end_at": "2026-09-09T02:00:00Z",
                            "poll_profile": "CAMPAIGN_ACTIVE"
                        }
                    ],
                    "source_overrides": {
                        "shopee-page": { "interval_percent": 50, "note": "watch closely" }
                    },
                    "notification_profile": { "label": "CAMPAIGN_QUIET", "digest_only": true }
                }
            ],
            "postures": {
                "CAMPAIGN_ACTIVE": { "interval_percent": 20, "min_interval_secs": 15, "max_interval_secs": 120 }
            }
        }"#;

        let calendar = CampaignCalendar::from_json(raw).expect("valid config");
        let campaign = calendar.campaign("9.9-2026").expect("campaign present");

        assert_eq!(campaign.lead_minutes, 180);
        // Omitted fields fall back to defaults.
        assert_eq!(campaign.recovery_minutes, DEFAULT_RECOVERY_MINUTES);
        assert!(campaign.enabled);
        assert_eq!(campaign.high_interest_windows.len(), 1);
        assert_eq!(
            campaign
                .source_override("shopee-page")
                .and_then(|o| o.interval_percent),
            Some(50)
        );
        assert_eq!(
            calendar
                .posture_for(PollProfile::CampaignActive)
                .interval_percent,
            20
        );

        // Re-serializing and re-parsing preserves everything.
        let encoded = serde_json::to_string(&calendar).expect("serializes");
        let decoded = CampaignCalendar::from_json(&encoded).expect("round trips");
        assert_eq!(decoded, calendar);
    }

    #[test]
    fn malformed_config_reports_a_parse_error() {
        let issues = CampaignCalendar::from_json("{ not json").expect_err("must fail");
        assert!(matches!(issues[0], CampaignError::Parse { .. }));
    }

    #[test]
    fn windows_display_in_vietnam_local_time() {
        // 00:00 UTC on 9 Sep is 07:00 on 9 Sep in Asia/Ho_Chi_Minh.
        let campaign = campaign();
        assert_eq!(
            campaign.display_window(),
            "09/09 07:00 -> 10/09 07:00 (GMT+7)"
        );
    }
}
