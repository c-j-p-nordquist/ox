//! US market calendar and sessions (New York time).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Session {
    Pre,
    Regular,
    Post,
    Closed,
}

impl Session {
    pub fn label(self) -> &'static str {
        match self {
            Session::Pre => "pre-market",
            Session::Regular => "open",
            Session::Post => "after-hours",
            Session::Closed => "closed",
        }
    }
}

/// How current a chain snapshot is. Outside the regular session the underlying may be live
/// (pre/post-market) while option quotes are left over from the last close.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Freshness {
    pub session: Session,
    /// Option bid/ask are from the last close, not live.
    pub quotes_stale: bool,
    /// Unix seconds the option quotes reflect.
    pub quotes_as_of: i64,
    /// Unix seconds of the underlying price.
    pub spot_as_of: i64,
}

impl Freshness {
    pub fn assess(spot_as_of: i64, now: DateTime<Utc>) -> Self {
        let session = session(now);
        let quotes_stale = session != Session::Regular;
        let quotes_as_of = if quotes_stale { last_options_close(now).timestamp() } else { spot_as_of };
        Self { session, quotes_stale, quotes_as_of, spot_as_of }
    }
}

/// NYSE full-day holidays. Extend yearly.
const HOLIDAYS: &[(i32, u32, u32)] = &[
    (2026, 1, 1),
    (2026, 1, 19),
    (2026, 2, 16),
    (2026, 4, 3),
    (2026, 5, 25),
    (2026, 6, 19),
    (2026, 7, 3),
    (2026, 9, 7),
    (2026, 11, 26),
    (2026, 12, 25),
    (2027, 1, 1),
    (2027, 1, 18),
    (2027, 2, 15),
    (2027, 3, 26),
    (2027, 5, 31),
    (2027, 6, 18),
    (2027, 7, 5),
    (2027, 9, 6),
    (2027, 11, 25),
    (2027, 12, 24),
];

pub fn is_trading_day(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) && !HOLIDAYS.contains(&(d.year(), d.month(), d.day()))
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// Equity session a bar belongs to, by its New York start time: pre (04:00–09:30), regular
/// (09:30–16:00), post (16:00–20:00), otherwise closed (overnight venues).
pub fn bar_session(t: i64) -> Session {
    let Some(ny) = Utc.timestamp_opt(t, 0).single().map(|d| d.with_timezone(&New_York)) else {
        return Session::Closed;
    };
    let tm = ny.time();
    if tm >= hm(4, 0) && tm < hm(9, 30) {
        Session::Pre
    } else if tm >= hm(9, 30) && tm < hm(16, 0) {
        Session::Regular
    } else if tm >= hm(16, 0) && tm < hm(20, 0) {
        Session::Post
    } else {
        Session::Closed
    }
}

/// Options on broad ETFs/indices quote until 16:15; equities stop at 16:00.
const OPTIONS_CLOSE: (u32, u32) = (16, 15);

pub fn session(now: DateTime<Utc>) -> Session {
    let ny = now.with_timezone(&New_York);
    if !is_trading_day(ny.date_naive()) {
        return Session::Closed;
    }
    let t = ny.time();
    if t < hm(4, 0) {
        Session::Closed
    } else if t < hm(9, 30) {
        Session::Pre
    } else if t < hm(OPTIONS_CLOSE.0, OPTIONS_CLOSE.1) {
        Session::Regular
    } else if t < hm(20, 0) {
        Session::Post
    } else {
        Session::Closed
    }
}

/// End of the most recent regular options session at or before `now`.
pub fn last_options_close(now: DateTime<Utc>) -> DateTime<Utc> {
    let ny = now.with_timezone(&New_York);
    let mut d = ny.date_naive();
    if ny.time() < hm(OPTIONS_CLOSE.0, OPTIONS_CLOSE.1) {
        d -= Duration::days(1);
    }
    while !is_trading_day(d) {
        d -= Duration::days(1);
    }
    let close = d.and_time(hm(OPTIONS_CLOSE.0, OPTIONS_CLOSE.1));
    New_York.from_local_datetime(&close).earliest().map(|t| t.with_timezone(&Utc)).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        New_York.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn sessions() {
        assert_eq!(session(ny(2026, 9, 28, 4, 26)), Session::Pre);
        assert_eq!(session(ny(2026, 9, 28, 10, 0)), Session::Regular);
        assert_eq!(session(ny(2026, 9, 28, 16, 10)), Session::Regular);
        assert_eq!(session(ny(2026, 9, 28, 17, 0)), Session::Post);
        assert_eq!(session(ny(2026, 9, 27, 12, 0)), Session::Closed); // Sunday
        assert_eq!(session(ny(2026, 11, 26, 12, 0)), Session::Closed); // Thanksgiving
    }

    #[test]
    fn last_close_skips_weekend() {
        // Monday pre-market -> Friday 16:15.
        assert_eq!(last_options_close(ny(2026, 9, 28, 4, 26)), ny(2026, 9, 25, 16, 15));
        // Monday after the close -> Monday 16:15.
        assert_eq!(last_options_close(ny(2026, 9, 28, 18, 0)), ny(2026, 9, 28, 16, 15));
        // Tuesday after Labor Day morning -> Friday before.
        assert_eq!(last_options_close(ny(2026, 9, 8, 8, 0)), ny(2026, 9, 4, 16, 15));
    }
}
