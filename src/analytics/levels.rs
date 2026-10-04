//! Session levels and automatic support/resistance by confluence.
//!
//! Candidate levels come from several independent methods, each with a weight:
//! swing pivots (daily and intraday), volume profile (point of control, value area), moving
//! averages, prior-session and prior-week extremes, pre-market and overnight ranges, and options
//! positioning (call/put wall, zero gamma, max pain). Candidates within a tolerance (a fraction
//! of daily ATR) are merged; a level's strength is the sum of the weights that agree on it.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use super::indicators::{ema, sma};
use super::market::{bar_session, Session};
use crate::model::Bar;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionLevels {
    /// The last completed regular session the levels refer to.
    pub prev_date: Option<NaiveDate>,
    pub prev_close: Option<f64>,
    pub prev_high: Option<f64>,
    pub prev_low: Option<f64>,
    pub today_open: Option<f64>,
    pub premarket_high: Option<f64>,
    pub premarket_low: Option<f64>,
    pub overnight_high: Option<f64>,
    pub overnight_low: Option<f64>,
    pub prev_week_high: Option<f64>,
    pub prev_week_low: Option<f64>,
    /// Gap vs prior close: today's open once the session opens, else the current price.
    pub gap_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Level {
    pub price: f64,
    /// 0–100, relative to the strongest level found.
    pub strength: f64,
    pub distance_pct: f64,
    /// What agrees on this level, e.g. "swing high ×3", "POC 5d", "call wall".
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelsReport {
    pub symbol: String,
    pub spot: f64,
    /// 14-day average true range.
    pub atr: Option<f64>,
    pub session: SessionLevels,
    /// Nearest first.
    pub supports: Vec<Level>,
    /// Nearest first.
    pub resistances: Vec<Level>,
}

/// Options-derived levels, when a chain is available.
#[derive(Default)]
pub struct OptionLevels {
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub zero_gamma: Option<f64>,
    pub max_pain: Option<f64>,
}

struct Candidate {
    price: f64,
    weight: f64,
    label: String,
}

fn ny_date(t: i64) -> Option<NaiveDate> {
    Utc.timestamp_opt(t, 0).single().map(|d| d.with_timezone(&New_York).date_naive())
}

pub fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if bars.len() <= n {
        return None;
    }
    let tr: Vec<f64> =
        bars.windows(2).map(|w| (w[1].h - w[1].l).max((w[1].h - w[0].c).abs()).max((w[1].l - w[0].c).abs())).collect();
    let mut a = tr[..n].iter().sum::<f64>() / n as f64;
    for x in &tr[n..] {
        a = (a * (n - 1) as f64 + x) / n as f64;
    }
    Some(a)
}

/// Bars whose high (low) is the highest (lowest) of the `k` bars on each side.
fn pivots(bars: &[Bar], k: usize) -> Vec<(usize, f64, bool)> {
    let mut out = Vec::new();
    if bars.len() < 2 * k + 1 {
        return out;
    }
    for i in k..bars.len() - k {
        let win = &bars[i - k..=i + k];
        if win.iter().all(|b| b.h <= bars[i].h) {
            out.push((i, bars[i].h, true));
        }
        if win.iter().all(|b| b.l >= bars[i].l) {
            out.push((i, bars[i].l, false));
        }
    }
    out
}

/// Point of control and 70% value area from volume spread evenly over each bar's range.
pub fn volume_profile(bars: &[Bar], bins: usize) -> Option<(f64, f64, f64)> {
    let lo = bars.iter().map(|b| b.l).fold(f64::MAX, f64::min);
    let hi = bars.iter().map(|b| b.h).fold(f64::MIN, f64::max);
    if bars.is_empty() || hi <= lo || bins < 3 {
        return None;
    }
    let step = (hi - lo) / bins as f64;
    let mut vol = vec![0.0; bins];
    for b in bars {
        let (a, z) = (((b.l - lo) / step) as usize, (((b.h - lo) / step) as usize).min(bins - 1));
        let per = b.v / (z - a.min(z) + 1) as f64;
        for v in &mut vol[a.min(z)..=z] {
            *v += per;
        }
    }
    let total: f64 = vol.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let poc = (0..bins).max_by(|&a, &b| vol[a].total_cmp(&vol[b]))?;
    let (mut l, mut h, mut acc) = (poc, poc, vol[poc]);
    while acc < 0.7 * total && (l > 0 || h < bins - 1) {
        let down = if l > 0 { vol[l - 1] } else { -1.0 };
        let up = if h < bins - 1 { vol[h + 1] } else { -1.0 };
        if up >= down {
            h += 1;
            acc += vol[h];
        } else {
            l -= 1;
            acc += vol[l];
        }
    }
    let mid = |i: usize| lo + (i as f64 + 0.5) * step;
    Some((mid(poc), lo + (h + 1) as f64 * step, lo + l as f64 * step))
}

/// Levels from extended-hours intraday bars (and optional overnight bars) around `now`.
pub fn session_levels(
    daily: &[Bar],
    intraday: &[Bar],
    overnight: &[Bar],
    spot: f64,
    now: DateTime<Utc>,
) -> SessionLevels {
    let mut regular: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    let mut pre: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    for b in intraday {
        let Some(d) = ny_date(b.t) else { continue };
        match bar_session(b.t) {
            Session::Regular => regular.entry(d).or_default().push(b),
            Session::Pre => pre.entry(d).or_default().push(b),
            _ => {}
        }
    }
    let today = now.with_timezone(&New_York).date_naive();
    // The previous session is the last full regular session strictly before today.
    let prev_date = regular.keys().rev().find(|d| **d < today).copied();
    let mut s = SessionLevels { prev_date, ..Default::default() };
    if let Some(bars) = prev_date.and_then(|d| regular.get(&d)) {
        s.prev_high = bars.iter().map(|b| b.h).reduce(f64::max);
        s.prev_low = bars.iter().map(|b| b.l).reduce(f64::min);
        s.prev_close = bars.last().map(|b| b.c);
    }
    if let Some(bars) = regular.get(&today) {
        s.today_open = bars.first().map(|b| b.o);
    }
    if let Some(bars) = pre.get(&today) {
        s.premarket_high = bars.iter().map(|b| b.h).reduce(f64::max);
        s.premarket_low = bars.iter().map(|b| b.l).reduce(f64::min);
    }
    // Overnight: after the previous session's 20:00 close, before today's 04:00 pre-market.
    let on: Vec<&Bar> = overnight
        .iter()
        .filter(|b| {
            bar_session(b.t) == Session::Closed && prev_date.is_some_and(|p| ny_date(b.t).is_some_and(|d| d >= p))
        })
        .collect();
    s.overnight_high = on.iter().map(|b| b.h).reduce(f64::max);
    s.overnight_low = on.iter().map(|b| b.l).reduce(f64::min);

    // Previous calendar week from daily bars.
    let this_week = today.iso_week();
    let mut weeks: BTreeMap<(i32, u32), (f64, f64)> = BTreeMap::new();
    for b in daily {
        let Some(d) = ny_date(b.t) else { continue };
        let w = d.iso_week();
        if (w.year(), w.week()) >= (this_week.year(), this_week.week()) {
            continue;
        }
        let e = weeks.entry((w.year(), w.week())).or_insert((f64::MIN, f64::MAX));
        e.0 = e.0.max(b.h);
        e.1 = e.1.min(b.l);
    }
    if let Some((_, (h, l))) = weeks.iter().next_back() {
        s.prev_week_high = Some(*h);
        s.prev_week_low = Some(*l);
    }
    s.gap_pct = s.prev_close.map(|pc| (s.today_open.unwrap_or(spot) / pc - 1.0) * 100.0);
    s
}

pub fn compute(
    symbol: &str,
    spot: f64,
    daily: &[Bar],
    intraday: &[Bar],
    overnight: &[Bar],
    opt: &OptionLevels,
    now: DateTime<Utc>,
) -> LevelsReport {
    let atr14 = atr(daily, 14);
    let session = session_levels(daily, intraday, overnight, spot, now);
    let mut c: Vec<Candidate> = Vec::new();
    let mut add = |price: Option<f64>, weight: f64, label: &str| {
        if let Some(p) = price.filter(|p| p.is_finite() && *p > 0.0) {
            c.push(Candidate { price: p, weight, label: label.to_string() });
        }
    };

    // Daily swing pivots over the last year, recent ones weigh more (half-life ~60 sessions).
    let year = &daily[daily.len().saturating_sub(252)..];
    for (i, p, high) in pivots(year, 5) {
        let age = (year.len() - 1 - i) as f64;
        add(Some(p), 1.0 * 0.5f64.powf(age / 60.0) + 0.3, if high { "swing high" } else { "swing low" });
    }
    // Intraday pivots over the last few sessions (regular hours only).
    let reg: Vec<Bar> = intraday.iter().copied().filter(|b| bar_session(b.t) == Session::Regular).collect();
    for (_, p, high) in pivots(&reg, 6) {
        add(Some(p), 0.4, if high { "intraday high" } else { "intraday low" });
    }
    // Volume profile: recent intraday and one year of daily bars.
    if let Some((poc, vah, val)) = volume_profile(&reg, 60) {
        add(Some(poc), 1.2, "POC 5d");
        add(Some(vah), 0.7, "VAH 5d");
        add(Some(val), 0.7, "VAL 5d");
    }
    if let Some((poc, vah, val)) = volume_profile(year, 80) {
        add(Some(poc), 1.2, "POC 1y");
        add(Some(vah), 0.6, "VAH 1y");
        add(Some(val), 0.6, "VAL 1y");
    }
    // Moving averages.
    let closes: Vec<f64> = daily.iter().map(|b| b.c).collect();
    let last = |v: Vec<f64>| v.last().copied().filter(|x| x.is_finite());
    add(last(ema(&closes, 20)), 0.6, "20d EMA");
    add(last(sma(&closes, 50)), 0.9, "50d SMA");
    add(last(sma(&closes, 200)), 1.1, "200d SMA");
    // Session extremes.
    add(session.prev_high, 1.0, "prior day high");
    add(session.prev_low, 1.0, "prior day low");
    add(session.prev_close, 0.8, "prior close");
    add(session.premarket_high, 0.6, "pre-market high");
    add(session.premarket_low, 0.6, "pre-market low");
    add(session.overnight_high, 0.4, "overnight high");
    add(session.overnight_low, 0.4, "overnight low");
    add(session.prev_week_high, 0.9, "prior week high");
    add(session.prev_week_low, 0.9, "prior week low");
    // Options positioning.
    add(opt.call_wall, 1.5, "call wall");
    add(opt.put_wall, 1.5, "put wall");
    add(opt.zero_gamma, 1.0, "zero gamma");
    add(opt.max_pain, 0.4, "max pain");

    let tol = atr14.map_or(spot * 0.004, |a| (a * 0.25).max(spot * 0.0015));
    let range = atr14.map_or(spot * 0.12, |a| (a * 6.0).min(spot * 0.25));
    let mut levels = cluster(c, tol);
    levels.retain(|l| (l.0 - spot).abs() <= range);
    let max = levels.iter().map(|l| l.1).fold(0.0, f64::max).max(1e-9);
    let to_level = |(price, w, sources): (f64, f64, Vec<String>)| Level {
        price,
        strength: (w / max * 100.0).round(),
        distance_pct: (price / spot - 1.0) * 100.0,
        sources,
    };

    // A level too close to spot to call either way is not reported as S or R.
    let dead = tol * 0.5;
    let mut supports: Vec<Level> = levels.iter().filter(|l| l.0 < spot - dead).cloned().map(to_level).collect();
    let mut resistances: Vec<Level> = levels.iter().filter(|l| l.0 > spot + dead).cloned().map(to_level).collect();
    // Keep the meaningful ones: nearest first among levels with some confluence.
    supports.sort_by(|a, b| b.price.total_cmp(&a.price));
    resistances.sort_by(|a, b| a.price.total_cmp(&b.price));
    let keep = |v: Vec<Level>| -> Vec<Level> {
        let strong: Vec<Level> = v.iter().filter(|l| l.strength >= 20.0).cloned().collect();
        let mut out = if strong.len() >= 3 { strong } else { v };
        out.truncate(6);
        out
    };
    LevelsReport {
        symbol: symbol.to_string(),
        spot,
        atr: atr14,
        session,
        supports: keep(supports),
        resistances: keep(resistances),
    }
}

/// Greedy clustering of sorted candidates; returns (weighted price, strength, sources).
/// Repeats of one source type add with diminishing effect: at most twice its largest single
/// weight, so many small intraday pivots cannot outvote independent kinds of evidence.
fn cluster(mut c: Vec<Candidate>, tol: f64) -> Vec<(f64, f64, Vec<String>)> {
    c.sort_by(|a, b| a.price.total_cmp(&b.price));
    // (weighted price, total weight for pricing, per label: (count, sum, max))
    type Acc = (f64, f64, BTreeMap<String, (usize, f64, f64)>);
    let mut out: Vec<Acc> = Vec::new();
    for x in c {
        let fits = out.last().is_some_and(|(p, _, _)| (x.price - *p).abs() <= tol);
        if !fits {
            out.push((x.price, 0.0, BTreeMap::new()));
        }
        let (p, w, src) = out.last_mut().unwrap();
        *p = (*p * *w + x.price * x.weight) / (*w + x.weight);
        *w += x.weight;
        let e = src.entry(x.label).or_insert((0, 0.0, 0.0));
        e.0 += 1;
        e.1 += x.weight;
        e.2 = e.2.max(x.weight);
    }
    out.into_iter()
        .map(|(p, _, src)| {
            let strength: f64 = src.values().map(|(_, sum, max)| sum.min(2.0 * max)).sum();
            let mut s: Vec<(String, usize, f64)> = src.into_iter().map(|(l, (n, sum, _))| (l, n, sum)).collect();
            s.sort_by(|a, b| b.2.total_cmp(&a.2));
            (p, strength, s.into_iter().map(|(l, n, _)| if n > 1 { format!("{l} ×{n}") } else { l }).collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar { t, o, h, l, c, v }
    }

    #[test]
    fn repeated_highs_form_strong_resistance() {
        // Oscillate between ~95 and ~105 so 105 and 95 are touched repeatedly.
        let daily: Vec<Bar> = (0..300)
            .map(|i| {
                let x = 100.0 + 5.0 * ((i as f64) * std::f64::consts::PI / 10.0).sin();
                bar(1_700_000_000 + i * 86_400, x, x + 0.5, x - 0.5, x, 1e6)
            })
            .collect();
        let r = compute("T", 100.0, &daily, &[], &[], &OptionLevels::default(), Utc::now());
        let top_r = &r.resistances.iter().max_by(|a, b| a.strength.total_cmp(&b.strength)).unwrap();
        assert!((top_r.price - 105.5).abs() < 1.0, "{top_r:?}");
        assert!(top_r.sources.iter().any(|s| s.contains("swing high")));
        let top_s = &r.supports.iter().max_by(|a, b| a.strength.total_cmp(&b.strength)).unwrap();
        assert!((top_s.price - 94.5).abs() < 1.0, "{top_s:?}");
        assert!(r.supports.iter().all(|l| l.price < 100.0) && r.resistances.iter().all(|l| l.price > 100.0));
    }

    #[test]
    fn options_levels_join_confluence() {
        let daily: Vec<Bar> =
            (0..60).map(|i| bar(1_700_000_000 + i * 86_400, 100.0, 101.0, 99.0, 100.0, 1e6)).collect();
        let opt = OptionLevels { call_wall: Some(101.0), ..Default::default() };
        let r = compute("T", 100.0, &daily, &[], &[], &opt, Utc::now());
        let l = r.resistances.iter().find(|l| (l.price - 101.0).abs() < 0.5).unwrap();
        assert!(l.sources.iter().any(|s| s == "call wall"), "{l:?}");
    }

    #[test]
    fn volume_profile_finds_poc() {
        let bars: Vec<Bar> =
            (0..50).map(|i| bar(i, 100.0, if i % 5 == 0 { 110.0 } else { 101.0 }, 99.0, 100.0, 1000.0)).collect();
        let (poc, vah, val) = volume_profile(&bars, 22).unwrap();
        assert!((poc - 100.0).abs() < 1.5, "{poc}");
        assert!(val < poc && vah > poc);
    }
}
