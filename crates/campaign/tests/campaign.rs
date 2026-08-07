//! End-to-end tests through the public API.
//!
//! The unit tests cover each rule; these prove the Phase 30 exit criterion —
//! campaign behaviour is entirely configuration-driven, so moving a campaign
//! date changes what the system does with no code change at all.

use std::time::Duration;

use chrono::{DateTime, Utc};
use shopee_hunter_campaign::{
    active_profile, is_high_interest, notification_profile, pre_campaign_checklist, resolve,
    source_override, source_posture, CampaignCalendar, CampaignError, CheckSeverity, PollProfile,
};

fn ts(raw: &str) -> DateTime<Utc> {
    raw.parse().expect("valid timestamp")
}

/// A realistic owner configuration: one live campaign with a midnight drop and
/// per-source overrides, plus last year's campaign kept but disabled.
fn config(start: &str, end: &str, window_end: &str) -> String {
    format!(
        r#"{{
            "campaigns": [
                {{
                    "id": "9.9-2026",
                    "name": "9.9 Super Sale",
                    "start_at": "{start}",
                    "end_at": "{end}",
                    "lead_minutes": 120,
                    "recovery_minutes": 60,
                    "high_interest_windows": [
                        {{
                            "label": "midnight drop",
                            "start_at": "{start}",
                            "end_at": "{window_end}"
                        }}
                    ],
                    "source_overrides": {{
                        "shopee-page": {{ "interval_percent": 10, "note": "primary source" }},
                        "slow-feed": {{ "enabled": false, "note": "noisy during sales" }}
                    }},
                    "notification_profile": {{
                        "label": "CAMPAIGN_QUIET",
                        "notification_threshold": 70,
                        "digest_only": true
                    }}
                }},
                {{
                    "id": "9.9-2025",
                    "name": "9.9 last year",
                    "start_at": "2025-09-09T00:00:00Z",
                    "end_at": "2025-09-10T00:00:00Z",
                    "enabled": false
                }}
            ],
            "postures": {{
                "CAMPAIGN_ACTIVE": {{
                    "interval_percent": 20,
                    "min_interval_secs": 15,
                    "max_interval_secs": 120
                }}
            }},
            "default_notification": {{ "label": "EVERYDAY" }}
        }}"#
    )
}

#[test]
fn a_json_calendar_drives_the_whole_timeline() {
    let calendar = CampaignCalendar::from_json(&config(
        "2026-09-09T00:00:00Z",
        "2026-09-10T00:00:00Z",
        "2026-09-09T02:00:00Z",
    ))
    .expect("valid configuration");

    // Long before: ordinary operation.
    assert_eq!(
        active_profile(&calendar, ts("2026-09-08T10:00:00Z")),
        PollProfile::Normal
    );
    // Two hours before the start: warm up.
    assert_eq!(
        active_profile(&calendar, ts("2026-09-08T22:00:00Z")),
        PollProfile::PreCampaign
    );
    // During the sale.
    assert_eq!(
        active_profile(&calendar, ts("2026-09-09T12:00:00Z")),
        PollProfile::CampaignActive
    );
    // The hour after it ends.
    assert_eq!(
        active_profile(&calendar, ts("2026-09-10T00:30:00Z")),
        PollProfile::Recovery
    );
    // Back to normal once recovery lapses.
    assert_eq!(
        active_profile(&calendar, ts("2026-09-10T01:30:00Z")),
        PollProfile::Normal
    );

    // The high-interest window only counts inside itself.
    assert!(is_high_interest(&calendar, ts("2026-09-09T01:00:00Z")));
    assert!(!is_high_interest(&calendar, ts("2026-09-09T03:00:00Z")));

    // Last year's campaign is inert but still on file.
    assert_eq!(
        active_profile(&calendar, ts("2025-09-09T12:00:00Z")),
        PollProfile::Normal
    );
    assert!(calendar.campaign("9.9-2025").is_some());
}

#[test]
fn moving_the_date_needs_no_code_change() {
    let september = CampaignCalendar::from_json(&config(
        "2026-09-09T00:00:00Z",
        "2026-09-10T00:00:00Z",
        "2026-09-09T02:00:00Z",
    ))
    .expect("valid configuration");
    let november = CampaignCalendar::from_json(&config(
        "2026-11-11T00:00:00Z",
        "2026-11-12T00:00:00Z",
        "2026-11-11T02:00:00Z",
    ))
    .expect("valid configuration");

    let moment = ts("2026-09-09T12:00:00Z");

    // Identical code, identical timestamp, different configuration.
    assert_eq!(
        active_profile(&september, moment),
        PollProfile::CampaignActive
    );
    assert_eq!(active_profile(&november, moment), PollProfile::Normal);
}

#[test]
fn collectors_get_posture_data_not_profile_names() {
    let calendar = CampaignCalendar::from_json(&config(
        "2026-09-09T00:00:00Z",
        "2026-09-10T00:00:00Z",
        "2026-09-09T02:00:00Z",
    ))
    .expect("valid configuration");
    let during = ts("2026-09-09T12:00:00Z");
    let base = Duration::from_secs(300);

    // The configured CAMPAIGN_ACTIVE posture (20%) replaces the built-in 25%.
    let decision = resolve(&calendar, during);
    assert_eq!(decision.posture.interval_percent, 20);
    assert_eq!(decision.posture.apply(base), Duration::from_secs(60));
    assert!(decision.is_campaign_governed());
    assert_eq!(decision.campaign_name.as_deref(), Some("9.9 Super Sale"));

    // A source override sharpens one collector without touching the others.
    let primary = source_posture(&calendar, "shopee-page", during).expect("posture");
    assert_eq!(primary.interval_percent, 10);
    assert_eq!(primary.apply(base), Duration::from_secs(30));

    let other = source_posture(&calendar, "unlisted-feed", during).expect("posture");
    assert_eq!(other.interval_percent, 20);

    // A disabled source yields no posture at all.
    assert!(source_posture(&calendar, "slow-feed", during).is_none());
    assert_eq!(
        source_override(&calendar, "slow-feed", during).and_then(|o| o.note.as_deref()),
        Some("noisy during sales")
    );

    // Outside the campaign, overrides do not apply and the floor still holds.
    let quiet = ts("2026-09-20T12:00:00Z");
    assert!(source_override(&calendar, "shopee-page", quiet).is_none());
    let normal = source_posture(&calendar, "shopee-page", quiet).expect("posture");
    assert_eq!(
        normal.apply(Duration::from_secs(1)),
        Duration::from_secs(30)
    );
}

#[test]
fn notification_behaviour_switches_with_the_campaign() {
    let calendar = CampaignCalendar::from_json(&config(
        "2026-09-09T00:00:00Z",
        "2026-09-10T00:00:00Z",
        "2026-09-09T02:00:00Z",
    ))
    .expect("valid configuration");

    let during = notification_profile(&calendar, ts("2026-09-09T12:00:00Z"));
    assert_eq!(during.label, "CAMPAIGN_QUIET");
    assert_eq!(during.notification_threshold, Some(70));
    assert!(during.digest_only);

    let after = notification_profile(&calendar, ts("2026-09-20T12:00:00Z"));
    assert_eq!(after.label, "EVERYDAY");
    assert_eq!(after.notification_threshold, None);
    assert!(!after.digest_only);
}

#[test]
fn invalid_configuration_is_rejected_with_every_problem_listed() {
    let raw = r#"{
        "campaigns": [
            {
                "id": "broken",
                "name": "Backwards",
                "start_at": "2026-09-10T00:00:00Z",
                "end_at": "2026-09-09T00:00:00Z",
                "high_interest_windows": [
                    {
                        "label": "stray",
                        "start_at": "2026-01-01T00:00:00Z",
                        "end_at": "2026-01-02T00:00:00Z"
                    }
                ]
            }
        ]
    }"#;

    let issues = CampaignCalendar::from_json(raw).expect_err("must be rejected");
    assert!(issues
        .iter()
        .any(|i| matches!(i, CampaignError::InvalidCampaignWindow { .. })));
    assert!(issues
        .iter()
        .any(|i| matches!(i, CampaignError::WindowOutsideCampaign { .. })));

    // Malformed JSON is a parse error, not a panic.
    let issues = CampaignCalendar::from_json("nope").expect_err("must be rejected");
    assert!(matches!(issues[0], CampaignError::Parse { .. }));
}

#[test]
fn the_checklist_is_renderable_by_the_control_plane() {
    let items = pre_campaign_checklist();
    assert_eq!(items.len(), 6);

    let blocking: Vec<&str> = items
        .iter()
        .filter(|item| item.severity == CheckSeverity::Blocking)
        .map(|item| item.id)
        .collect();
    assert!(blocking.contains(&"SESSION_HEALTHY"));
    assert!(blocking.contains(&"CLOCK_ACCURATE"));

    // Every item carries enough text to render a report line.
    for item in &items {
        assert!(!item.title.is_empty());
        assert!(!item.detail.is_empty());
        assert!(!item.severity.as_str().is_empty());
    }
}
