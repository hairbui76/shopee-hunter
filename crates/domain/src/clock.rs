//! Wall-clock abstraction so scheduling-sensitive code is testable.
//! Monotonic deadlines live in the scheduler crate (they need Tokio).

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// Vietnam display timezone. Persisted timestamps are always UTC.
pub const DISPLAY_TZ: Tz = chrono_tz::Asia::Ho_Chi_Minh;

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Convert a UTC timestamp for owner-facing display.
pub fn to_display_tz(utc: DateTime<Utc>) -> DateTime<Tz> {
    utc.with_timezone(&DISPLAY_TZ)
}

/// Format for Telegram/notification display, e.g. `10/08 12:00`.
pub fn format_display(utc: DateTime<Utc>) -> String {
    to_display_tz(utc).format("%d/%m %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn converts_utc_to_vietnam_time() {
        // 05:00 UTC == 12:00 Asia/Ho_Chi_Minh (UTC+7, no DST).
        let utc = Utc.with_ymd_and_hms(2026, 8, 10, 5, 0, 0).unwrap();
        assert_eq!(format_display(utc), "10/08 12:00");
    }
}
