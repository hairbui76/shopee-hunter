//! Deciding what posture the system should be in *right now*.
//!
//! Every entry point takes `now` explicitly (or a [`Clock`]), so the decision
//! is a pure function of calendar plus timestamp: reproducible in tests, and
//! replayable when explaining why the system behaved as it did at 00:00 on
//! 9 September.
//!
//! When campaigns overlap, the most demanding profile wins
//! ([`PollProfile::priority`]); ties break on earlier start, then campaign id,
//! so the outcome never depends on configuration file ordering.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::clock::Clock;

use crate::calendar::{Campaign, CampaignCalendar, HighInterestWindow, SourceOverride};
use crate::profile::{NotificationProfile, PollPosture, PollProfile};

/// The full posture decision, with enough context to explain itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignPosture {
    pub profile: PollProfile,
    /// Polling data the collectors consult.
    pub posture: PollPosture,
    /// Campaign driving the decision, if any.
    pub campaign_id: Option<String>,
    pub campaign_name: Option<String>,
    /// Whether a high-interest window is open right now.
    pub high_interest: bool,
    /// Label of the open high-interest window.
    pub high_interest_window: Option<String>,
    pub notification: NotificationProfile,
    /// Human-readable justification, in Vietnam local time.
    pub reason: String,
}

impl CampaignPosture {
    /// Whether any campaign governs at this moment.
    pub fn is_campaign_governed(&self) -> bool {
        self.campaign_id.is_some()
    }
}

/// Full decision for `now`.
pub fn resolve(calendar: &CampaignCalendar, now: DateTime<Utc>) -> CampaignPosture {
    let governing = governing_campaign(calendar, now);

    let Some((campaign, mut profile)) = governing else {
        return CampaignPosture {
            profile: PollProfile::Normal,
            posture: calendar.posture_for(PollProfile::Normal),
            campaign_id: None,
            campaign_name: None,
            high_interest: false,
            high_interest_window: None,
            notification: calendar.default_notification.clone(),
            reason: "no campaign window covers now".to_string(),
        };
    };

    let window = campaign.high_interest_window(now);
    if let Some(window) = window {
        // A window may demand a different posture than the campaign default,
        // e.g. a midnight drop inside an otherwise ordinary campaign day.
        if let Some(forced) = window.poll_profile {
            profile = forced;
        }
    }

    let reason = describe(campaign, profile, window);

    CampaignPosture {
        profile,
        posture: calendar.posture_for(profile),
        campaign_id: Some(campaign.id.clone()),
        campaign_name: Some(campaign.name.clone()),
        high_interest: window.is_some(),
        high_interest_window: window.map(|w| w.label.clone()),
        notification: campaign
            .notification_profile
            .clone()
            .unwrap_or_else(|| calendar.default_notification.clone()),
        reason,
    }
}

/// [`resolve`] against an injected clock.
pub fn resolve_with_clock(calendar: &CampaignCalendar, clock: &dyn Clock) -> CampaignPosture {
    resolve(calendar, clock.now_utc())
}

/// The polling profile in force at `now`.
pub fn active_profile(calendar: &CampaignCalendar, now: DateTime<Utc>) -> PollProfile {
    resolve(calendar, now).profile
}

/// Whether a high-interest window is open at `now`.
pub fn is_high_interest(calendar: &CampaignCalendar, now: DateTime<Utc>) -> bool {
    calendar
        .campaigns
        .iter()
        .any(|campaign| campaign.high_interest_window(now).is_some())
}

/// The high-interest window open at `now`, searched across all campaigns in
/// configuration order.
pub fn high_interest_window(
    calendar: &CampaignCalendar,
    now: DateTime<Utc>,
) -> Option<&HighInterestWindow> {
    calendar
        .campaigns
        .iter()
        .find_map(|campaign| campaign.high_interest_window(now))
}

/// The campaign whose phase decides the posture at `now`, with that phase.
///
/// Includes campaigns in warm-up or recovery: they govern overrides too, which
/// is the point of having a lead time at all.
pub fn governing_campaign(
    calendar: &CampaignCalendar,
    now: DateTime<Utc>,
) -> Option<(&Campaign, PollProfile)> {
    let mut candidates: Vec<(&Campaign, PollProfile)> = calendar
        .campaigns
        .iter()
        .filter_map(|campaign| campaign.phase(now).map(|profile| (campaign, profile)))
        .collect();

    candidates.sort_by(|(left_campaign, left), (right_campaign, right)| {
        right
            .priority()
            .cmp(&left.priority())
            .then_with(|| left_campaign.start_at.cmp(&right_campaign.start_at))
            .then_with(|| left_campaign.id.cmp(&right_campaign.id))
    });

    candidates.into_iter().next()
}

/// The override for `source` from the governing campaign, if any.
pub fn source_override<'a>(
    calendar: &'a CampaignCalendar,
    source: &str,
    now: DateTime<Utc>,
) -> Option<&'a SourceOverride> {
    let (campaign, _) = governing_campaign(calendar, now)?;
    campaign.source_override(source)
}

/// The polling posture for one source at `now`: the profile posture, with the
/// source's own `interval_percent` applied when the campaign specifies one.
///
/// Returns `None` when the campaign explicitly disables the source, which the
/// collector supervisor should treat as "skip this cycle".
pub fn source_posture(
    calendar: &CampaignCalendar,
    source: &str,
    now: DateTime<Utc>,
) -> Option<PollPosture> {
    let decision = resolve(calendar, now);
    let Some(override_) = source_override(calendar, source, now) else {
        return Some(decision.posture);
    };
    if override_.enabled == Some(false) {
        return None;
    }
    let mut posture = decision.posture;
    if let Some(percent) = override_.interval_percent.filter(|percent| *percent > 0) {
        posture.interval_percent = percent;
    }
    Some(posture)
}

/// The notification profile in force at `now`.
pub fn notification_profile(
    calendar: &CampaignCalendar,
    now: DateTime<Utc>,
) -> NotificationProfile {
    resolve(calendar, now).notification
}

fn describe(
    campaign: &Campaign,
    profile: PollProfile,
    window: Option<&HighInterestWindow>,
) -> String {
    let phase = match profile {
        PollProfile::PreCampaign => "starts soon",
        PollProfile::CampaignActive => "is running",
        PollProfile::Recovery => "just ended",
        PollProfile::Normal => "is being treated as normal",
    };
    let mut reason = format!(
        "{} ({}) {phase}: {}",
        campaign.name,
        campaign.id,
        campaign.display_window()
    );
    if let Some(window) = window {
        reason.push_str(&format!(
            "; high-interest window `{}` {}",
            window.label,
            window.display_window()
        ));
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::HighInterestWindow;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        // `expect` (test-only): a fixture with an impossible hour must fail
        // loudly rather than silently collapsing to the Unix epoch.
        Utc.with_ymd_and_hms(2026, 9, day, hour, 0, 0)
            .single()
            .expect("valid fixture timestamp")
    }

    /// 9.9 runs for one day, with a 2h warm-up and 1h recovery.
    fn calendar() -> CampaignCalendar {
        CampaignCalendar::new().with_campaign(
            Campaign::new("9.9-2026", "9.9 Super Sale", at(9, 0), at(10, 0))
                .with_lead_minutes(120)
                .with_recovery_minutes(60)
                .with_window(HighInterestWindow::new("midnight drop", at(9, 0), at(9, 2))),
        )
    }

    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[test]
    fn profile_transitions_across_every_boundary() {
        let calendar = calendar();
        // Well before warm-up.
        assert_eq!(active_profile(&calendar, at(8, 12)), PollProfile::Normal);
        // One hour before warm-up starts.
        assert_eq!(active_profile(&calendar, at(8, 21)), PollProfile::Normal);
        // Warm-up begins exactly at start - lead.
        assert_eq!(
            active_profile(&calendar, at(8, 22)),
            PollProfile::PreCampaign
        );
        // Campaign start.
        assert_eq!(
            active_profile(&calendar, at(9, 0)),
            PollProfile::CampaignActive
        );
        assert_eq!(
            active_profile(&calendar, at(9, 23)),
            PollProfile::CampaignActive
        );
        // Campaign end flips straight into recovery.
        assert_eq!(active_profile(&calendar, at(10, 0)), PollProfile::Recovery);
        // Recovery ends.
        assert_eq!(active_profile(&calendar, at(10, 1)), PollProfile::Normal);
        assert_eq!(active_profile(&calendar, at(11, 0)), PollProfile::Normal);
    }

    #[test]
    fn posture_follows_the_profile() {
        let calendar = calendar();
        assert_eq!(
            resolve(&calendar, at(9, 12)).posture,
            PollProfile::CampaignActive.default_posture()
        );
        assert_eq!(
            resolve(&calendar, at(11, 0)).posture,
            PollProfile::Normal.default_posture()
        );
    }

    #[test]
    fn high_interest_is_detected_only_inside_its_window() {
        let calendar = calendar();
        assert!(!is_high_interest(&calendar, at(8, 23)));
        assert!(is_high_interest(&calendar, at(9, 0)));
        assert!(is_high_interest(&calendar, at(9, 1)));
        // Half-open: the window's end instant is already outside.
        assert!(!is_high_interest(&calendar, at(9, 2)));

        let decision = resolve(&calendar, at(9, 1));
        assert!(decision.high_interest);
        assert_eq!(
            decision.high_interest_window.as_deref(),
            Some("midnight drop")
        );
        assert!(decision.reason.contains("midnight drop"));
    }

    #[test]
    fn a_window_can_force_a_different_profile() {
        let calendar = CampaignCalendar::new().with_campaign(
            Campaign::new("12.12", "12.12", at(12, 0), at(13, 0))
                .with_poll_profile(PollProfile::PreCampaign)
                .with_window(
                    HighInterestWindow::new("finale", at(12, 20), at(12, 22))
                        .with_poll_profile(PollProfile::CampaignActive),
                ),
        );

        // The campaign itself asked for a calmer posture...
        assert_eq!(
            active_profile(&calendar, at(12, 10)),
            PollProfile::PreCampaign
        );
        // ...but the window overrides it while open.
        assert_eq!(
            active_profile(&calendar, at(12, 21)),
            PollProfile::CampaignActive
        );
    }

    #[test]
    fn overlapping_campaigns_resolve_to_the_most_demanding() {
        let calendar = CampaignCalendar::new()
            .with_campaign(
                Campaign::new("active", "Active one", at(9, 0), at(10, 0))
                    .with_lead_minutes(60)
                    .with_recovery_minutes(60),
            )
            .with_campaign(
                // Its warm-up overlaps the first campaign's active period.
                Campaign::new("next", "Next one", at(10, 0), at(11, 0)).with_lead_minutes(180),
            );

        // Active beats pre-campaign.
        let decision = resolve(&calendar, at(9, 22));
        assert_eq!(decision.profile, PollProfile::CampaignActive);
        assert_eq!(decision.campaign_id.as_deref(), Some("active"));

        // Once the first ends, the second is active and beats recovery.
        let decision = resolve(&calendar, at(10, 12));
        assert_eq!(decision.profile, PollProfile::CampaignActive);
        assert_eq!(decision.campaign_id.as_deref(), Some("next"));
    }

    #[test]
    fn resolution_is_independent_of_configuration_order() {
        let first = Campaign::new("a", "A", at(9, 0), at(10, 0)).with_lead_minutes(60);
        let second = Campaign::new("b", "B", at(9, 0), at(10, 0)).with_lead_minutes(60);

        let forward = CampaignCalendar::new()
            .with_campaign(first.clone())
            .with_campaign(second.clone());
        let reversed = CampaignCalendar::new()
            .with_campaign(second)
            .with_campaign(first);

        // Identical phases and start times: the id breaks the tie both ways.
        assert_eq!(
            resolve(&forward, at(9, 12)).campaign_id,
            resolve(&reversed, at(9, 12)).campaign_id
        );
        assert_eq!(
            resolve(&forward, at(9, 12)).campaign_id.as_deref(),
            Some("a")
        );
    }

    #[test]
    fn disabled_campaigns_are_ignored_entirely() {
        let calendar = CampaignCalendar::new()
            .with_campaign(Campaign::new("old", "Last year", at(9, 0), at(10, 0)).disabled());
        assert_eq!(active_profile(&calendar, at(9, 12)), PollProfile::Normal);
        assert!(!is_high_interest(&calendar, at(9, 12)));
        assert!(!resolve(&calendar, at(9, 12)).is_campaign_governed());
    }

    #[test]
    fn empty_calendar_is_always_normal() {
        let calendar = CampaignCalendar::new();
        let decision = resolve(&calendar, at(9, 12));
        assert_eq!(decision.profile, PollProfile::Normal);
        assert!(!decision.is_campaign_governed());
        assert_eq!(decision.reason, "no campaign window covers now");
        assert_eq!(decision.notification, NotificationProfile::default());
    }

    #[test]
    fn source_overrides_apply_only_while_a_campaign_governs() {
        let calendar = CampaignCalendar::new().with_campaign(
            Campaign::new("9.9-2026", "9.9", at(9, 0), at(10, 0))
                .with_lead_minutes(60)
                .with_source_override(
                    "shopee-page",
                    SourceOverride {
                        interval_percent: Some(10),
                        note: Some("watch closely".into()),
                        ..SourceOverride::default()
                    },
                )
                .with_source_override(
                    "slow-feed",
                    SourceOverride {
                        enabled: Some(false),
                        ..SourceOverride::default()
                    },
                ),
        );

        // Outside the campaign there is no override at all.
        assert!(source_override(&calendar, "shopee-page", at(8, 0)).is_none());
        assert_eq!(
            source_posture(&calendar, "shopee-page", at(8, 0)),
            Some(PollProfile::Normal.default_posture())
        );

        // Warm-up already carries the overrides.
        let override_ = source_override(&calendar, "shopee-page", at(8, 23)).expect("override");
        assert_eq!(override_.interval_percent, Some(10));
        assert_eq!(override_.note.as_deref(), Some("watch closely"));

        // The source-specific percentage replaces the profile's.
        let posture = source_posture(&calendar, "shopee-page", at(9, 12)).expect("posture");
        assert_eq!(posture.interval_percent, 10);
        assert_eq!(
            posture.min_interval_secs,
            PollProfile::CampaignActive
                .default_posture()
                .min_interval_secs
        );

        // A disabled source yields no posture at all.
        assert!(source_posture(&calendar, "slow-feed", at(9, 12)).is_none());

        // An unlisted source just follows the profile.
        assert_eq!(
            source_posture(&calendar, "other-feed", at(9, 12)),
            Some(PollProfile::CampaignActive.default_posture())
        );
    }

    #[test]
    fn notification_profile_falls_back_to_the_calendar_default() {
        let campaign_profile = NotificationProfile {
            label: "CAMPAIGN_QUIET".into(),
            notification_threshold: Some(70),
            digest_only: true,
            ..NotificationProfile::default()
        };
        let calendar = CampaignCalendar::new()
            .with_default_notification(NotificationProfile {
                label: "EVERYDAY".into(),
                ..NotificationProfile::default()
            })
            .with_campaign(
                Campaign::new("9.9", "9.9", at(9, 0), at(10, 0))
                    .with_lead_minutes(0)
                    .with_recovery_minutes(0)
                    .with_notification_profile(campaign_profile.clone()),
            );

        assert_eq!(notification_profile(&calendar, at(9, 12)), campaign_profile);
        assert_eq!(notification_profile(&calendar, at(11, 0)).label, "EVERYDAY");
    }

    #[test]
    fn clock_injection_matches_explicit_now() {
        let calendar = calendar();
        let clock = FixedClock(at(9, 12));
        assert_eq!(
            resolve_with_clock(&calendar, &clock),
            resolve(&calendar, at(9, 12))
        );
    }

    #[test]
    fn reason_text_uses_vietnam_local_time() {
        let decision = resolve(&calendar(), at(9, 12));
        assert!(decision.reason.contains("9.9 Super Sale"));
        // 00:00 UTC on 9 Sep renders as 07:00 on 9 Sep in Asia/Ho_Chi_Minh.
        assert!(decision.reason.contains("09/09 07:00"));
        assert!(decision.reason.contains("GMT+7"));
    }
}
