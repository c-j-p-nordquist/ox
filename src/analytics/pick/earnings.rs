//! Earnings as an event: when past reports landed in the bars, how far the stock moved on them,
//! and how far the options market expects it to move on the next one.
//!
//! - Past reaction days: the known report dates (about four quarters) plus projections of them a
//!   year back at a time, each snapped to the bar with the largest opening gap near the date
//!   (a report lands as a gap: before the open, or after the prior close).
//! - Implied move: the extra variance in the first expiry after the report over the last one
//!   before it, `T₂·(σ₂² − σ₁²)`, taking the pre-report expiry's IV as the base vol.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use crate::analytics::bs;
use crate::model::{Bar, ChainSnapshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PastMove {
    /// The reaction day (the bar the report gapped into).
    pub date: NaiveDate,
    /// Close-to-close move that day, %.
    pub move_pct: f64,
    /// From a reported date; otherwise projected a year back and snapped to the largest gap.
    pub known: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EarningsMove {
    pub date: NaiveDate,
    pub estimated: bool,
    /// One-standard-deviation move the options market prices for the report, % (log).
    pub implied_move_pct: Option<f64>,
    /// Expiries the implied move came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implied_from: Option<(NaiveDate, NaiveDate)>,
    /// Past reaction days, newest first.
    pub past: Vec<PastMove>,
    /// Root-mean-square of the past moves, %.
    pub past_rms_pct: Option<f64>,
    /// Move size used to simulate the report, % (log, 1σ): implied, else past RMS.
    pub sim_move_pct: Option<f64>,
}

fn bar_date(b: &Bar) -> NaiveDate {
    Utc.timestamp_opt(b.t, 0).single().map_or(NaiveDate::MIN, |t| t.with_timezone(&New_York).date_naive())
}

/// Bar indices of past earnings reactions, oldest first, with whether each came from a known date.
pub fn reaction_days(bars: &[Bar], known: &[NaiveDate]) -> Vec<(usize, bool)> {
    let Some(first) = bars.first().map(bar_date) else { return Vec::new() };
    let last = bars.last().map(bar_date).unwrap_or(first);
    // Anchors: known dates, then each projected back a year at a time.
    let mut anchors: Vec<(NaiveDate, bool)> = Vec::new();
    for &k in known {
        anchors.push((k, true));
        let mut d = k - Duration::weeks(52);
        while d >= first {
            anchors.push((d, false));
            d -= Duration::weeks(52);
        }
    }
    anchors.sort_by_key(|(d, known)| (*d, !known));
    let mut kept: Vec<(NaiveDate, bool)> = Vec::new();
    for a in anchors {
        match kept.last_mut() {
            Some(prev) if (a.0 - prev.0).num_days() < 30 => {
                if a.1 && !prev.1 {
                    *prev = a;
                }
            }
            _ => kept.push(a),
        }
    }
    let gap = |i: usize| if i == 0 { 0.0 } else { (bars[i].o / bars[i - 1].c).ln().abs() };
    let mut out = Vec::new();
    for (d, known) in kept.into_iter().filter(|(d, _)| *d <= last) {
        // Known: the report day (before the open) or the next session (after the close).
        let (lo, hi) =
            if known { (d, d + Duration::days(4)) } else { (d - Duration::days(10), d + Duration::days(10)) };
        let window: Vec<usize> = (1..bars.len()).filter(|&i| (lo..=hi).contains(&bar_date(&bars[i]))).collect();
        let window = if known { window.into_iter().take(2).collect() } else { window };
        if let Some(i) = window.into_iter().max_by(|&a, &b| gap(a).total_cmp(&gap(b))) {
            if out.last().is_none_or(|(p, _)| *p != i) {
                out.push((i, known));
            }
        }
    }
    out
}

fn atm_iv(chain: &ChainSnapshot, expiry: NaiveDate) -> Option<f64> {
    let mut best: Option<(f64, Vec<f64>)> = None;
    for c in chain.contracts.iter().filter(|c| c.expiry == expiry && c.iv > 0.0) {
        let dist = (c.strike - chain.spot).abs();
        match &mut best {
            Some((d, v)) if (dist - *d).abs() < 1e-9 => v.push(c.iv),
            Some((d, _)) if dist >= *d => {}
            _ => best = Some((dist, vec![c.iv])),
        }
    }
    best.map(|(_, v)| v.iter().sum::<f64>() / v.len() as f64)
}

/// The report's 1σ log move priced by the options: `√(T₂·(σ₂² − σ₁²))` from the last expiry at
/// least two days before the report and the first expiry after it.
pub fn implied_move(
    chain: &ChainSnapshot,
    date: NaiveDate,
    now: chrono::DateTime<Utc>,
) -> Option<(f64, NaiveDate, NaiveDate)> {
    let today = now.with_timezone(&New_York).date_naive();
    let exps = chain.expirations();
    let e1 = exps.iter().copied().filter(|e| *e < date && (*e - today).num_days() >= 2).max()?;
    let e2 = exps.iter().copied().filter(|e| *e > date).min()?;
    let (s1, s2) = (atm_iv(chain, e1)?, atm_iv(chain, e2)?);
    let t2 = bs::years_to_expiry(e2, now);
    let var = t2 * (s2 * s2 - s1 * s1);
    (var > 0.0).then(|| (var.sqrt(), e1, e2))
}

pub fn model(
    bars: &[Bar],
    reactions: &[(usize, bool)],
    chain: Option<&ChainSnapshot>,
    date: NaiveDate,
    estimated: bool,
    now: chrono::DateTime<Utc>,
) -> EarningsMove {
    let past: Vec<PastMove> = reactions
        .iter()
        .rev()
        .map(|&(i, known)| PastMove {
            date: bar_date(&bars[i]),
            move_pct: (bars[i].c / bars[i - 1].c - 1.0) * 100.0,
            known,
        })
        .collect();
    let past_rms_pct = (past.len() >= 3).then(|| {
        (past.iter().map(|m| (1.0 + m.move_pct / 100.0).ln().powi(2)).sum::<f64>() / past.len() as f64).sqrt() * 100.0
    });
    let implied = chain.and_then(|c| implied_move(c, date, now));
    let implied_move_pct = implied.map(|x| x.0 * 100.0);
    EarningsMove {
        date,
        estimated,
        implied_move_pct,
        implied_from: implied.map(|x| (x.1, x.2)),
        sim_move_pct: implied_move_pct.or(past_rms_pct),
        past,
        past_rms_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OptionContract, Right};

    fn day(d: NaiveDate) -> i64 {
        New_York.from_local_datetime(&d.and_hms_opt(16, 0, 0).unwrap()).unwrap().timestamp()
    }

    #[test]
    fn finds_report_gaps_back_in_time() {
        // Two years of flat weekdays with gaps on 2025-07-24 and 2024-07-25 (a year apart, 52w ≈ 364d).
        let start = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let mut bars = Vec::new();
        let mut d = start;
        let mut c = 100.0;
        while d <= NaiveDate::from_ymd_opt(2025, 12, 31).unwrap() {
            if chrono::Datelike::weekday(&d).number_from_monday() <= 5 {
                let gap = d == NaiveDate::from_ymd_opt(2025, 7, 24).unwrap()
                    || d == NaiveDate::from_ymd_opt(2024, 7, 25).unwrap();
                let o = if gap { c * 1.08 } else { c };
                bars.push(Bar { t: day(d), o, h: o * 1.01, l: o * 0.99, c: o, v: 1.0 });
                c = o;
            }
            d += Duration::days(1);
        }
        let r = reaction_days(&bars, &[NaiveDate::from_ymd_opt(2025, 7, 23).unwrap()]);
        let dates: Vec<NaiveDate> = r.iter().map(|&(i, _)| bar_date(&bars[i])).collect();
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2025, 7, 24).unwrap()), "{dates:?}");
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 7, 25).unwrap()), "{dates:?}");
        let m = model(&bars, &r, None, NaiveDate::from_ymd_opt(2026, 7, 23).unwrap(), true, Utc::now());
        assert!((m.past[0].move_pct - 8.0).abs() < 1e-6, "{:?}", m.past);
    }

    #[test]
    fn implied_move_from_term_structure() {
        let now = New_York.with_ymd_and_hms(2026, 10, 2, 16, 0, 0).unwrap().with_timezone(&Utc);
        let e1 = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let e2 = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        let mk = |expiry, iv| OptionContract {
            symbol: String::new(),
            root: String::new(),
            expiry,
            right: Right::Call,
            strike: 100.0,
            bid: 1.0,
            ask: 1.0,
            last: 0.0,
            iv,
            delta: 0.5,
            gamma: 0.0,
            theta: 0.0,
            vega: 0.0,
            open_interest: 1.0,
            volume: 0.0,
            prev_close: 0.0,
        };
        let chain = ChainSnapshot {
            underlying: "X".into(),
            spot: 100.0,
            prev_close: 100.0,
            iv30: None,
            as_of: 0,
            source: String::new(),
            delayed: true,
            freshness: crate::analytics::market::Freshness {
                session: crate::analytics::market::Session::Closed,
                quotes_stale: false,
                quotes_as_of: 0,
                spot_as_of: 0,
            },
            regular_close: None,
            contracts: vec![mk(e1, 0.30), mk(e2, 0.50)],
        };
        let (sd, a, b) = implied_move(&chain, NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(), now).unwrap();
        assert_eq!((a, b), (e1, e2));
        let t2 = bs::years_to_expiry(e2, now);
        assert!((sd - (t2 * (0.25 - 0.09)).sqrt()).abs() < 1e-9);
        assert!(sd > 0.05 && sd < 0.15, "{sd}");
    }
}
