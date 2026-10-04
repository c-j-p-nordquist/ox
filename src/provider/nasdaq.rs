//! Nasdaq public quote API: real-time last trade including pre- and post-market (one symbol per
//! request), and next earnings dates. Unofficial and keyless; used for the symbol in focus only.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::America::New_York;
use serde::Deserialize;

use super::{Earnings, EventSource, QuoteSource};
use crate::analytics::market::Session;
use crate::model::Quote;

const BASE: &str = "https://api.nasdaq.com/api";

pub struct Nasdaq {
    http: reqwest::Client,
}

impl Nasdaq {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<Option<T>> {
        let env: Envelope<T> = self
            .http
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("requesting Nasdaq")?
            .json()
            .await
            .context("decoding Nasdaq")?;
        Ok(env.data)
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: Option<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    symbol: Option<String>,
    market_status: Option<String>,
    primary_data: Primary,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Primary {
    last_sale_price: Option<String>,
    net_change: Option<String>,
    last_trade_timestamp: Option<String>,
    #[serde(default)]
    is_real_time: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EarningsData {
    report_text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Surprise {
    earnings_surprise_table: Option<SurpriseTable>,
}

#[derive(Deserialize)]
struct SurpriseTable {
    #[serde(default)]
    rows: Vec<SurpriseRow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SurpriseRow {
    date_reported: Option<String>,
}

/// Next report estimated from past report dates (newest first): the same quarter a year on
/// (52 weeks after the report four quarters back), else the last report plus the median gap.
fn estimate_next(reported: &[NaiveDate], today: NaiveDate) -> Option<NaiveDate> {
    let mut d: Vec<NaiveDate> = reported.to_vec();
    d.sort_by(|a, b| b.cmp(a));
    let last = *d.first()?;
    let year_on = d.get(3).map(|x| *x + chrono::Duration::weeks(52)).filter(|x| *x > last + chrono::Duration::days(45));
    let mut gaps: Vec<i64> = d.windows(2).map(|w| (w[0] - w[1]).num_days()).collect();
    gaps.sort();
    let gap = gaps.get(gaps.len() / 2).copied().unwrap_or(91);
    let next = year_on.unwrap_or(last + chrono::Duration::days(gap));
    (next >= today).then_some(next)
}

fn num(s: &Option<String>) -> Option<f64> {
    let t: String = s.as_deref()?.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    t.parse().ok()
}

fn session(status: Option<&str>) -> Option<Session> {
    match status?.to_ascii_lowercase().as_str() {
        s if s.contains("pre") => Some(Session::Pre),
        s if s.contains("after") || s.contains("post") => Some(Session::Post),
        s if s.contains("open") => Some(Session::Regular),
        s if s.contains("closed") => Some(Session::Closed),
        _ => None,
    }
}

/// "Sep 28, 2026 7:35 AM ET" (possibly prefixed with "Closed at ").
fn trade_time(s: Option<&str>) -> Option<i64> {
    let s = s?.trim_start_matches("Closed at ").trim_end_matches(" ET").trim();
    let t = NaiveDateTime::parse_from_str(s, "%b %d, %Y %I:%M %p").ok()?;
    New_York.from_local_datetime(&t).earliest().map(|d| d.timestamp())
}

/// First `MM/DD/YYYY` in the text.
fn first_date(text: &str) -> Option<NaiveDate> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_digit() && c != '/'))
        .find_map(|w| NaiveDate::parse_from_str(w, "%m/%d/%Y").ok())
}

#[async_trait]
impl QuoteSource for Nasdaq {
    fn name(&self) -> &'static str {
        "nasdaq"
    }

    async fn quote(&self, symbol: &str) -> Result<Quote> {
        let sym = symbol.to_ascii_uppercase();
        for class in ["stocks", "etf"] {
            let url = format!("{BASE}/quote/{sym}/info?assetclass={class}");
            let Some(info) = self.get::<Info>(&url).await? else { continue };
            let p = &info.primary_data;
            let Some(last) = num(&p.last_sale_price) else { continue };
            let change = num(&p.net_change).unwrap_or(0.0);
            let prev = last - change;
            return Ok(Quote {
                symbol: info.symbol.unwrap_or(sym.clone()).to_ascii_uppercase(),
                last,
                prev_close: prev,
                change,
                change_pct: if prev > 0.0 { change / prev * 100.0 } else { 0.0 },
                high: None,
                low: None,
                volume: None,
                time: trade_time(p.last_trade_timestamp.as_deref()).unwrap_or_else(|| chrono::Utc::now().timestamp()),
                delayed: !p.is_real_time,
                source: "nasdaq".into(),
                session: session(info.market_status.as_deref()),
            });
        }
        bail!("Nasdaq has no quote for {sym}")
    }
}

#[async_trait]
impl EventSource for Nasdaq {
    fn name(&self) -> &'static str {
        "nasdaq"
    }

    async fn next_earnings(&self, symbol: &str) -> Result<Option<Earnings>> {
        let sym = symbol.to_ascii_uppercase();
        let Some(d) = self.get::<EarningsData>(&format!("{BASE}/analyst/{sym}/earnings-date")).await? else {
            return Ok(None); // ETFs and indices have no earnings
        };
        let text = d.report_text.unwrap_or_default();
        let vendor = first_date(&text).map(|date| (date, text.contains("estimated")));
        let mut past: Vec<NaiveDate> =
            match self.get::<Surprise>(&format!("{BASE}/company/{sym}/earnings-surprise")).await {
                Ok(Some(h)) => h
                    .earnings_surprise_table
                    .map(|t| t.rows)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| NaiveDate::parse_from_str(r.date_reported.as_deref()?, "%m/%d/%Y").ok())
                    .collect(),
                _ => Vec::new(),
            };
        past.sort_by(|a, b| b.cmp(a));
        let today = chrono::Utc::now().with_timezone(&New_York).date_naive();
        let from_history = estimate_next(&past, today);
        let (date, estimated) = match (vendor, from_history) {
            (Some((v, false)), _) => (v, false),
            // The vendor's own estimate runs late for companies that report early (Zacks put
            // GOOG and META a week after their usual week); the earlier one is the safer bound.
            (Some((v, true)), Some(h)) => (v.min(h), true),
            (Some((v, true)), None) => (v, true),
            // Right after a report the vendor often has no next date yet.
            (None, Some(h)) => (h, true),
            (None, None) => return Ok(None),
        };
        Ok(Some(Earnings { symbol: sym, date, estimated, past }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields() {
        assert_eq!(num(&Some("$1,227.23".into())), Some(1227.23));
        assert_eq!(num(&Some("-3.5095".into())), Some(-3.5095));
        assert_eq!(session(Some("Pre-Market")), Some(Session::Pre));
        assert_eq!(session(Some("After-Hours")), Some(Session::Post));
        assert!(trade_time(Some("Sep 28, 2026 7:35 AM ET")).is_some());
        assert!(trade_time(Some("Closed at Sep 25, 2026 4:00 PM ET")).is_some());
        assert_eq!(
            first_date("is estimated to report earnings on  10/28/2026. The upcoming"),
            NaiveDate::from_ymd_opt(2026, 10, 28)
        );
    }

    #[test]
    fn estimates_next_report_from_history() {
        let d = |m, day, y| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        // MU: reported 9/30 last; the same quarter a year earlier was 12/17/2025.
        let past = [d(9, 30, 2026), d(6, 24, 2026), d(3, 18, 2026), d(12, 17, 2025)];
        assert_eq!(estimate_next(&past, d(10, 3, 2026)), Some(d(12, 16, 2026)));
        // Fewer than four reports: last plus the median gap.
        assert_eq!(estimate_next(&past[..2], d(10, 3, 2026)), Some(d(9, 30, 2026) + chrono::Duration::days(98)));
        assert_eq!(estimate_next(&[], d(10, 3, 2026)), None);
    }
}
