//! Campaign-aware scheduling (ROADMAP Phase 30).
//!
//! Shopee's big sale days (9.9, 11.11, 12.12, payday windows) need different
//! behaviour from an ordinary Tuesday — but *which days* those are, and *how
//! different* the behaviour should be, are both configuration. This crate
//! holds the model and the decision function; it never holds a date.
//!
//! ```text
//! CampaignCalendar (config)  +  now  ──►  CampaignPosture
//!                                          ├─ PollProfile      what situation
//!                                          ├─ PollPosture      how fast to poll
//!                                          ├─ source overrides per-collector
//!                                          └─ NotificationProfile how loud
//! ```
//!
//! * [`calendar`] — [`Campaign`], [`HighInterestWindow`], [`SourceOverride`],
//!   [`CampaignCalendar`], and validation.
//! * [`profile`] — [`PollProfile`] names and the [`PollPosture`] data they map
//!   to; collectors consult the posture, never the name.
//! * [`resolve`] — [`active_profile`], [`is_high_interest`], override lookups,
//!   and the full [`resolve`](resolve()) decision.
//! * [`checklist`] — the documented pre-campaign readiness items.
//!
//! # Guarantees
//!
//! * **No hard-coded dates.** Adding or moving a campaign is a config edit; the
//!   exit criterion for Phase 30 is that no deployment is needed to change a
//!   campaign date.
//! * **Deterministic.** Every entry point takes `now` (or a
//!   [`Clock`](shopee_hunter_domain::clock::Clock)); nothing here reads the
//!   system clock. Overlapping campaigns resolve by explicit priority and
//!   tie-breakers, never by file ordering.
//! * **Bounded aggression.** Every posture carries a minimum interval, and
//!   validation rejects a zero multiplier, so no configuration can turn
//!   campaign mode into a request flood.
//! * **UTC inside, `Asia/Ho_Chi_Minh` outside.** Windows are stored and
//!   compared in UTC; only owner-facing strings are localised.
//!
//! # Example
//!
//! ```
//! use shopee_hunter_campaign::{active_profile, CampaignCalendar, PollProfile};
//!
//! let calendar = CampaignCalendar::from_json(r#"{
//!     "campaigns": [{
//!         "id": "9.9-2026",
//!         "name": "9.9 Super Sale",
//!         "start_at": "2026-09-09T00:00:00Z",
//!         "end_at": "2026-09-10T00:00:00Z"
//!     }]
//! }"#).expect("valid calendar");
//!
//! let now = "2026-09-09T05:00:00Z".parse().expect("timestamp");
//! assert_eq!(active_profile(&calendar, now), PollProfile::CampaignActive);
//! ```

#![forbid(unsafe_code)]

pub mod calendar;
pub mod checklist;
pub mod error;
pub mod profile;
pub mod resolve;

pub use calendar::{
    Campaign, CampaignCalendar, HighInterestWindow, SourceOverride, DEFAULT_LEAD_MINUTES,
    DEFAULT_RECOVERY_MINUTES, MAX_OFFSET_MINUTES,
};
pub use checklist::{blocking_items, pre_campaign_checklist, CheckSeverity, ChecklistItem};
pub use error::CampaignError;
pub use profile::{NotificationProfile, PollPosture, PollProfile};
pub use resolve::{
    active_profile, governing_campaign, high_interest_window, is_high_interest,
    notification_profile, resolve, resolve_with_clock, source_override, source_posture,
    CampaignPosture,
};
