//! Provider-neutral market data types. Every provider normalizes into these.

use std::fmt;
use std::str::FromStr;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::analytics::market::Freshness;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Right {
    #[serde(rename = "C")]
    Call,
    #[serde(rename = "P")]
    Put,
}

impl Right {
    pub fn letter(self) -> char {
        match self {
            Right::Call => 'C',
            Right::Put => 'P',
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    pub last: f64,
    pub prev_close: f64,
    pub change: f64,
    pub change_pct: f64,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub volume: Option<f64>,
    /// Unix seconds of the last trade.
    pub time: i64,
    pub delayed: bool,
    pub source: String,
    /// Session the price is from, when the provider says (pre-market, after-hours...).
    #[serde(default)]
    pub session: Option<crate::analytics::market::Session>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bar {
    /// Unix seconds, bar open time.
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "1d")]
    D1,
}

impl Timeframe {
    pub const ALL: [Timeframe; 5] = [Self::M1, Self::M5, Self::M15, Self::H1, Self::D1];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::D1 => "1d",
        }
    }

    pub fn is_intraday(self) -> bool {
        self != Self::D1
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Timeframe {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|tf| tf.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| anyhow::anyhow!("unknown timeframe {s:?} (use 1m, 5m, 15m, 1h, 1d)"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionContract {
    /// OCC symbol without padding, e.g. `SPY260928C00550000`.
    pub symbol: String,
    pub root: String,
    pub expiry: NaiveDate,
    pub right: Right,
    pub strike: f64,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    /// Implied volatility as a decimal (0.25 = 25%).
    pub iv: f64,
    pub delta: f64,
    pub gamma: f64,
    /// Per calendar day.
    pub theta: f64,
    /// Per 1 vol point.
    pub vega: f64,
    pub open_interest: f64,
    pub volume: f64,
    pub prev_close: f64,
}

impl OptionContract {
    pub fn mid(&self) -> f64 {
        if self.bid > 0.0 && self.ask > 0.0 {
            (self.bid + self.ask) / 2.0
        } else if self.last > 0.0 {
            self.last
        } else {
            self.ask.max(self.bid)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSnapshot {
    pub underlying: String,
    pub spot: f64,
    pub prev_close: f64,
    /// 30-day implied vol as a decimal, when the provider supplies it.
    pub iv30: Option<f64>,
    /// Provider timestamp of the data (unix seconds).
    pub as_of: i64,
    pub source: String,
    pub delayed: bool,
    pub freshness: Freshness,
    /// Last regular-session close of the underlying, if the provider reports it.
    pub regular_close: Option<f64>,
    pub contracts: Vec<OptionContract>,
}

impl ChainSnapshot {
    pub fn expirations(&self) -> Vec<NaiveDate> {
        let mut v: Vec<NaiveDate> = self.contracts.iter().map(|c| c.expiry).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Parse an OCC option symbol (`ROOT` + `YYMMDD` + `C|P` + strike*1000 as 8 digits).
pub fn parse_occ(sym: &str) -> Option<(String, NaiveDate, Right, f64)> {
    let sym = sym.trim();
    if sym.len() < 16 || !sym.is_ascii() {
        return None;
    }
    let (root, tail) = sym.split_at(sym.len() - 15);
    let yy: i32 = tail[0..2].parse().ok()?;
    let mm: u32 = tail[2..4].parse().ok()?;
    let dd: u32 = tail[4..6].parse().ok()?;
    let right = match &tail[6..7] {
        "C" => Right::Call,
        "P" => Right::Put,
        _ => return None,
    };
    let strike: f64 = tail[7..15].parse::<u64>().ok()? as f64 / 1000.0;
    let expiry = NaiveDate::from_ymd_opt(2000 + yy, mm, dd)?;
    Some((root.trim().to_string(), expiry, right, strike))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occ_parsing() {
        let (root, exp, right, strike) = parse_occ("SPY260928C00550000").unwrap();
        assert_eq!(root, "SPY");
        assert_eq!(exp, NaiveDate::from_ymd_opt(2026, 9, 28).unwrap());
        assert_eq!(right, Right::Call);
        assert_eq!(strike, 550.0);

        let (root, _, right, strike) = parse_occ("SPXW261016P05512500").unwrap();
        assert_eq!(root, "SPXW");
        assert_eq!(right, Right::Put);
        assert_eq!(strike, 5512.5);

        assert!(parse_occ("garbage").is_none());
    }
}
