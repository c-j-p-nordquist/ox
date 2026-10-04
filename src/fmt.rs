//! Number and time formatting shared by the CLI and TUI.

use chrono::{TimeZone, Utc};
use chrono_tz::America::New_York;

/// Compact magnitude: 1.23B, 45.6M, 12.3K.
pub fn big(x: f64) -> String {
    let a = x.abs();
    let (v, suffix) = if a >= 1e9 {
        (x / 1e9, "B")
    } else if a >= 1e6 {
        (x / 1e6, "M")
    } else if a >= 1e3 {
        (x / 1e3, "K")
    } else {
        return format!("{x:.0}");
    };
    if v.abs() >= 100.0 {
        format!("{v:.0}{suffix}")
    } else {
        format!("{v:.1}{suffix}")
    }
}

pub fn signed_big(x: f64) -> String {
    if x > 0.0 {
        format!("+{}", big(x))
    } else {
        big(x)
    }
}

pub fn price(x: f64) -> String {
    if x.abs() >= 10_000.0 {
        format!("{x:.0}")
    } else {
        format!("{x:.2}")
    }
}

pub fn strike(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else if (x * 10.0).fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    }
}

pub fn pct(x: f64) -> String {
    format!("{x:+.2}%")
}

pub fn opt_num(x: Option<f64>, f: impl Fn(f64) -> String) -> String {
    x.map(f).unwrap_or_else(|| "–".into())
}

/// `Fri 16:00 ET` for a unix timestamp.
pub fn time_et(unix: i64) -> String {
    Utc.timestamp_opt(unix, 0)
        .single()
        .map(|t| t.with_timezone(&New_York).format("%a %H:%M ET").to_string())
        .unwrap_or_default()
}
