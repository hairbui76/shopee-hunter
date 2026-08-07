//! Calendar validation errors.
//!
//! A campaign calendar is owner-edited configuration, so it is validated
//! explicitly at load time and **every** problem is reported at once — a
//! startup failure should list everything to fix, not the first mistake.

use chrono::{DateTime, Utc};

/// A problem with a campaign calendar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CampaignError {
    /// The configuration could not be parsed at all.
    #[error("campaign calendar is not valid JSON: {detail}")]
    Parse { detail: String },

    #[error("campaign `{campaign}`: {field} must not be empty")]
    BlankField {
        campaign: String,
        field: &'static str,
    },

    #[error("campaign id `{id}` is defined more than once")]
    DuplicateCampaign { id: String },

    #[error("campaign `{campaign}`: start {start} must be before end {end}")]
    InvalidCampaignWindow {
        campaign: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },

    #[error("campaign `{campaign}` window `{window}`: start {start} must be before end {end}")]
    InvalidHighInterestWindow {
        campaign: String,
        window: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },

    /// A high-interest window must lie inside the campaign it belongs to,
    /// otherwise it could silently never fire.
    #[error(
        "campaign `{campaign}` window `{window}` ({start} .. {end}) falls outside the campaign"
    )]
    WindowOutsideCampaign {
        campaign: String,
        window: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },

    #[error("campaign `{campaign}`: {field} must be between 0 and {max} minutes (got {value})")]
    OffsetOutOfRange {
        campaign: String,
        field: &'static str,
        value: i64,
        max: i64,
    },

    /// A zero multiplier would mean "poll with no delay", which this project
    /// never does (CLAUDE.md: speed comes from architecture, not request
    /// volume).
    #[error("{context}: interval_percent must be at least 1")]
    ZeroIntervalPercent { context: String },

    #[error("{context}: min_interval_secs ({min}) must be >= 1 and <= max_interval_secs ({max})")]
    InvalidPostureBounds { context: String, min: u64, max: u64 },

    #[error("campaign `{campaign}`: source override key must not be empty")]
    BlankSourceOverride { campaign: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn messages_identify_the_offending_configuration() {
        let start = Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 8, 9, 0, 0, 0).unwrap();
        let err = CampaignError::InvalidCampaignWindow {
            campaign: "9.9".into(),
            start,
            end,
        };
        let text = err.to_string();
        assert!(text.contains("9.9"));
        assert!(text.contains("must be before"));

        let err = CampaignError::ZeroIntervalPercent {
            context: "profile CAMPAIGN_ACTIVE".into(),
        };
        assert!(err.to_string().contains("CAMPAIGN_ACTIVE"));
    }
}
