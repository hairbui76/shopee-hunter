//! Durable scheduling intent types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScheduleAction {
    /// Attempt the voucher save/claim at the target time.
    ClaimVoucher,
    /// Refresh voucher metadata ahead of the claim window.
    RefreshMetadata,
    /// Verify session health ahead of the claim window.
    SessionPreflight,
    /// Notify the owner that a voucher becomes active soon.
    NotifyUpcoming,
}

impl ScheduleAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaimVoucher => "CLAIM_VOUCHER",
            Self::RefreshMetadata => "REFRESH_METADATA",
            Self::SessionPreflight => "SESSION_PREFLIGHT",
            Self::NotifyUpcoming => "NOTIFY_UPCOMING",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "CLAIM_VOUCHER" => Self::ClaimVoucher,
            "REFRESH_METADATA" => Self::RefreshMetadata,
            "SESSION_PREFLIGHT" => Self::SessionPreflight,
            "NOTIFY_UPCOMING" => Self::NotifyUpcoming,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobStatus {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Stale,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Ready => "READY",
            Self::Running => "RUNNING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::Stale => "STALE",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "PENDING" => Self::Pending,
            "READY" => Self::Ready,
            "RUNNING" => Self::Running,
            "SUCCEEDED" => Self::Succeeded,
            "FAILED" => Self::Failed,
            "CANCELLED" => Self::Cancelled,
            "STALE" => Self::Stale,
            _ => return None,
        })
    }

    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::Ready | Self::Running)
    }
}
