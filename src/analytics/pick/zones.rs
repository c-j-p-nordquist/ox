//! Support/resistance zones whose weights are measured, not assumed.
//!
//! Every level source (swing pivots, Fibonacci retracements and extensions, moving averages,
//! volume-profile nodes, anchored VWAPs, prior week/month extremes) is computed as of each
//! past day. A level is scored when price later touches it: it *held* if price then moved one
//! ATR away from it before closing half an ATR through it. The same test on control levels
//! (fixed ATR offsets from the close, i.e. no information) gives the base rate, which absorbs
//! the stock's drift and volatility. A source's lift is its shrunk hold rate over the base rate.
//!
//! The pass is walk-forward: the weights used on day `i` only include touches that resolved
//! before `i`, so historical zones (used by the setup backtest) carry no look-ahead.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use super::regime::Series;
use crate::analytics::levels::volume_profile;
use crate::model::Bar;

/// Bars after a level is noted within which a touch counts.
const TOUCH_WINDOW: usize = 20;
/// Bars after a touch within which the level must resolve.
const REACT_WINDOW: usize = 15;
/// Prior strength (pseudo-observations at the base rate) when shrinking hold rates.
const PRIOR_N: f64 = 20.0;
/// Days of history needed before levels are computed (one year for the 200d SMA and profile).
pub const WARMUP: usize = 252;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    SwingHigh,
    SwingLow,
    Fib382,
    Fib500,
    Fib618,
    Fib786,
    FibExt1272,
    FibExt1618,
    Ema20,
    Sma50,
    Sma100,
    Sma200,
    Poc1y,
    ValueArea1y,
    Poc3m,
    PrevWeek,
    PrevMonth,
    AvwapSwingLow,
    AvwapSwingHigh,
    CallWall,
    PutWall,
    ZeroGamma,
    Control,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::SwingHigh => "swing high",
            Kind::SwingLow => "swing low",
            Kind::Fib382 => "fib 0.382",
            Kind::Fib500 => "fib 0.5",
            Kind::Fib618 => "fib 0.618",
            Kind::Fib786 => "fib 0.786",
            Kind::FibExt1272 => "fib ext 1.272",
            Kind::FibExt1618 => "fib ext 1.618",
            Kind::Ema20 => "20d EMA",
            Kind::Sma50 => "50d SMA",
            Kind::Sma100 => "100d SMA",
            Kind::Sma200 => "200d SMA",
            Kind::Poc1y => "1y volume POC",
            Kind::ValueArea1y => "1y value area edge",
            Kind::Poc3m => "3m volume POC",
            Kind::PrevWeek => "prior week high/low",
            Kind::PrevMonth => "prior month high/low",
            Kind::AvwapSwingLow => "VWAP anchored at swing low",
            Kind::AvwapSwingHigh => "VWAP anchored at swing high",
            Kind::CallWall => "call wall",
            Kind::PutWall => "put wall",
            Kind::ZeroGamma => "zero gamma",
            Kind::Control => "control",
        }
    }

    /// Options-positioning levels only exist for today; they get a fixed, modest prior.
    fn is_options(self) -> bool {
        matches!(self, Kind::CallWall | Kind::PutWall | Kind::ZeroGamma)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Support,
    Resistance,
}

#[derive(Debug, Clone, Copy)]
pub struct Src {
    pub price: f64,
    pub kind: Kind,
    /// Recency or other per-instance multiplier on the kind's weight.
    pub mult: f64,
}

/// Bar indices grouped by calendar period, for prior week/month extremes.
struct Periods {
    /// (high, low) of the previous completed period, per bar.
    prev: Vec<Option<(f64, f64)>>,
}

impl Periods {
    fn new(bars: &[Bar], key: impl Fn(chrono::NaiveDate) -> (i32, u32)) -> Self {
        let mut prev = vec![None; bars.len()];
        let (mut cur_key, mut cur, mut last) = (None, (f64::MIN, f64::MAX), None);
        for (i, b) in bars.iter().enumerate() {
            let d = Utc.timestamp_opt(b.t, 0).single().map(|t| t.with_timezone(&New_York).date_naive());
            let k = d.map(&key);
            if k != cur_key {
                if cur_key.is_some() {
                    last = Some(cur);
                }
                cur_key = k;
                cur = (f64::MIN, f64::MAX);
            }
            cur = (cur.0.max(b.h), cur.1.min(b.l));
            prev[i] = last;
        }
        Self { prev }
    }
}

/// Source generator over one bar series.
pub struct Sources<'a> {
    s: &'a Series<'a>,
    /// (bar index, price, is_high) of 5-bar swing pivots.
    pivots: Vec<(usize, f64, bool)>,
    week: Periods,
    month: Periods,
}

const PIVOT_K: usize = 5;

impl<'a> Sources<'a> {
    pub fn new(s: &'a Series<'a>) -> Self {
        let b = s.bars;
        let mut pivots = Vec::new();
        if b.len() > 2 * PIVOT_K {
            for i in PIVOT_K..b.len() - PIVOT_K {
                let w = &b[i - PIVOT_K..=i + PIVOT_K];
                if w.iter().all(|x| x.h <= b[i].h) {
                    pivots.push((i, b[i].h, true));
                }
                if w.iter().all(|x| x.l >= b[i].l) {
                    pivots.push((i, b[i].l, false));
                }
            }
        }
        Self {
            week: Periods::new(b, |d| (d.iso_week().year(), d.iso_week().week())),
            month: Periods::new(b, |d| (d.year(), d.month())),
            pivots,
            s,
        }
    }

    /// Every level source known at the close of bar `i` (no look-ahead).
    pub fn at(&self, i: usize) -> Vec<Src> {
        let s = self.s;
        let b = s.bars;
        let atr = s.atr[i];
        let mut out = Vec::new();
        let mut add = |price: f64, kind: Kind, mult: f64| {
            if price.is_finite() && price > 0.0 {
                out.push(Src { price, kind, mult });
            }
        };

        // Swing pivots confirmed by bar i, last year; recent ones count more (half-life ~120d).
        let lo = i.saturating_sub(252);
        for &(j, p, high) in &self.pivots {
            if j < lo || j + PIVOT_K > i {
                continue;
            }
            let age = (i - j) as f64;
            add(p, if high { Kind::SwingHigh } else { Kind::SwingLow }, 0.3 + 0.7 * 0.5f64.powf(age / 120.0));
        }

        // The dominant swing of the last six months: Fibonacci levels and anchored VWAPs.
        let w0 = i.saturating_sub(125);
        let (hi_j, lo_j) = (w0..=i)
            .fold((w0, w0), |(h, l), j| (if b[j].h > b[h].h { j } else { h }, if b[j].l < b[l].l { j } else { l }));
        let (h, l) = (b[hi_j].h, b[lo_j].l);
        if atr.is_finite() && h - l >= 3.0 * atr {
            let up_leg = lo_j < hi_j;
            let r = h - l;
            for (ratio, kind) in
                [(0.382, Kind::Fib382), (0.5, Kind::Fib500), (0.618, Kind::Fib618), (0.786, Kind::Fib786)]
            {
                add(if up_leg { h - ratio * r } else { l + ratio * r }, kind, 1.0);
            }
            for (ext, kind) in [(1.272, Kind::FibExt1272), (1.618, Kind::FibExt1618)] {
                add(if up_leg { l + ext * r } else { h - ext * r }, kind, 1.0);
            }
            let avwap = |from: usize| {
                let (pv, v) =
                    b[from..=i].iter().fold((0.0, 0.0), |(pv, v), x| (pv + (x.h + x.l + x.c) / 3.0 * x.v, v + x.v));
                if v > 0.0 {
                    pv / v
                } else {
                    f64::NAN
                }
            };
            if lo_j < i {
                add(avwap(lo_j), Kind::AvwapSwingLow, 1.0);
            }
            if hi_j < i {
                add(avwap(hi_j), Kind::AvwapSwingHigh, 1.0);
            }
        }

        add(s.ema20[i], Kind::Ema20, 1.0);
        add(s.sma50[i], Kind::Sma50, 1.0);
        add(s.sma100[i], Kind::Sma100, 1.0);
        add(s.sma200[i], Kind::Sma200, 1.0);

        if i + 1 >= 252 {
            if let Some((poc, vah, val)) = volume_profile(&b[i + 1 - 252..=i], 80) {
                add(poc, Kind::Poc1y, 1.0);
                add(vah, Kind::ValueArea1y, 1.0);
                add(val, Kind::ValueArea1y, 1.0);
            }
        }
        if i + 1 >= 63 {
            if let Some((poc, _, _)) = volume_profile(&b[i + 1 - 63..=i], 50) {
                add(poc, Kind::Poc3m, 1.0);
            }
        }
        for (p, kind) in [(&self.week, Kind::PrevWeek), (&self.month, Kind::PrevMonth)] {
            if let Some((h, l)) = p.prev[i] {
                add(h, kind, 1.0);
                add(l, kind, 1.0);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Count {
    n: f64,
    held: f64,
}

/// Hold-rate statistics per (kind, side), accumulated walk-forward.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    counts: HashMap<(Kind, Side), Count>,
}

impl Stats {
    fn add(&mut self, kind: Kind, side: Side, held: bool) {
        let c = self.counts.entry((kind, side)).or_default();
        c.n += 1.0;
        c.held += held as u8 as f64;
    }

    pub fn base(&self, side: Side) -> f64 {
        let c = self.counts.get(&(Kind::Control, side)).copied().unwrap_or_default();
        // Random-walk prior for a +1 ATR bounce vs a −0.5 ATR break: about one in three.
        (c.held + 10.0 / 3.0) / (c.n + 10.0)
    }

    /// Shrunk hold rate divided by the base rate (1.0 = no better than a random level).
    pub fn lift(&self, kind: Kind, side: Side) -> f64 {
        if kind.is_options() {
            return 1.1;
        }
        let base = self.base(side);
        let c = self.counts.get(&(kind, side)).copied().unwrap_or_default();
        ((c.held + PRIOR_N * base) / (c.n + PRIOR_N)) / base
    }

    pub fn n(&self, kind: Kind, side: Side) -> usize {
        self.counts.get(&(kind, side)).map_or(0, |c| c.n as usize)
    }

    pub fn hold_rate(&self, kind: Kind, side: Side) -> Option<f64> {
        self.counts.get(&(kind, side)).filter(|c| c.n > 0.0).map(|c| c.held / c.n)
    }
}

/// Weight a source contributes to a zone, from its measured lift.
pub fn weight(lift: f64) -> f64 {
    (0.1 + 2.0 * (lift - 1.0).max(0.0)).min(1.2)
}

/// Outcome of the first touch of `level` after bar `i`, as (resolve bar, held).
fn touch_outcome(b: &[Bar], i: usize, level: f64, atr: f64, side: Side) -> Option<(usize, bool)> {
    let d = if side == Side::Support { 1.0 } else { -1.0 };
    let end = (i + 1 + TOUCH_WINDOW).min(b.len());
    let j = (i + 1..end).find(|&j| {
        let x = if side == Side::Support { b[j].l } else { b[j].h };
        d * (x - level) <= 0.1 * atr
    })?;
    for (m, x) in b.iter().enumerate().take((j + REACT_WINDOW).min(b.len())).skip(j) {
        if d * (x.c - level) < -0.5 * atr {
            return Some((m, false));
        }
        let ext = if side == Side::Support { x.h } else { x.l };
        if m > j && d * (ext - level) >= atr {
            return Some((m, true));
        }
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneSource {
    pub kind: Kind,
    pub label: String,
    pub price: f64,
    /// Measured lift of this kind on this side (1.0 = no edge over a random level).
    pub lift: f64,
    /// Resolved historical touches behind the lift.
    pub n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub price: f64,
    pub low: f64,
    pub high: f64,
    pub side: Side,
    /// Sum over distinct kinds of their weight (see [`weight`]).
    pub strength: f64,
    /// The part of `strength` from backtested (price) sources, excluding options positioning.
    pub tested_strength: f64,
    pub sources: Vec<ZoneSource>,
}

impl Zone {
    pub fn describe(&self) -> String {
        let mut seen = HashSet::new();
        self.sources.iter().filter(|s| seen.insert(s.kind)).map(|s| s.label.clone()).collect::<Vec<_>>().join(" + ")
    }
}

/// Cluster sources within `tol` of each other into zones, nearest the close first per side.
pub fn cluster(srcs: &[Src], close: f64, tol: f64, stats: &Stats, labels: bool) -> Vec<Zone> {
    let mut v: Vec<&Src> = srcs.iter().filter(|s| s.kind != Kind::Control).collect();
    v.sort_by(|a, b| a.price.total_cmp(&b.price));
    let mut groups: Vec<Vec<&Src>> = Vec::new();
    for s in v {
        match groups.last_mut() {
            Some(g) if s.price - g.iter().map(|x| x.price).sum::<f64>() / g.len() as f64 <= tol => g.push(s),
            _ => groups.push(vec![s]),
        }
    }
    groups
        .into_iter()
        .map(|g| {
            let mid = g.iter().map(|x| x.price).sum::<f64>() / g.len() as f64;
            let side = if mid < close { Side::Support } else { Side::Resistance };
            let mut best: HashMap<Kind, f64> = HashMap::new();
            let mut wsum = 0.0;
            let mut psum = 0.0;
            for s in &g {
                let w = weight(stats.lift(s.kind, side)) * s.mult;
                let e = best.entry(s.kind).or_insert(0.0);
                *e = e.max(w);
                wsum += w;
                psum += w * s.price;
            }
            let mut sources: Vec<ZoneSource> = if labels {
                g.iter()
                    .map(|s| ZoneSource {
                        kind: s.kind,
                        label: format!("{} {:.2}", s.kind.name(), s.price),
                        price: s.price,
                        lift: stats.lift(s.kind, side),
                        n: stats.n(s.kind, side),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            sources.sort_by(|a, b| weight(b.lift).total_cmp(&weight(a.lift)));
            Zone {
                price: if wsum > 0.0 { psum / wsum } else { mid },
                low: g.iter().map(|x| x.price).fold(f64::MAX, f64::min),
                high: g.iter().map(|x| x.price).fold(f64::MIN, f64::max),
                side,
                strength: best.values().sum(),
                tested_strength: best.iter().filter(|(k, _)| !k.is_options()).map(|(_, w)| w).sum(),
                sources,
            }
        })
        .collect()
}

/// Zone tolerance: sources within this distance are one level.
pub fn tolerance(atr: f64) -> f64 {
    0.3 * atr
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindStat {
    pub kind: Kind,
    pub source: String,
    pub side: Side,
    pub touches: usize,
    pub hold_rate: Option<f64>,
    pub base_rate: f64,
    pub lift: f64,
}

/// Result of the walk-forward pass.
pub struct Calibration {
    /// Statistics from every resolved touch (for today's zones).
    pub stats: Stats,
    /// Zones as of each bar's close, with weights known at that time (empty in warm-up).
    pub daily_zones: Vec<Vec<Zone>>,
}

impl Calibration {
    pub fn table(&self) -> Vec<KindStat> {
        let mut kinds: Vec<(Kind, Side)> =
            self.stats.counts.keys().copied().filter(|(k, _)| *k != Kind::Control).collect();
        kinds.sort();
        kinds
            .into_iter()
            .map(|(kind, side)| KindStat {
                kind,
                source: kind.name().into(),
                side,
                touches: self.stats.n(kind, side),
                hold_rate: self.stats.hold_rate(kind, side),
                base_rate: self.stats.base(side),
                lift: self.stats.lift(kind, side),
            })
            .collect()
    }
}

pub fn calibrate(s: &Series, src: &Sources) -> Calibration {
    let b = s.bars;
    let n = b.len();
    let mut pending: Vec<Vec<(Kind, Side, bool)>> = vec![Vec::new(); n + 1];
    let mut stats = Stats::default();
    let mut daily_zones = vec![Vec::new(); n];
    let mut seen: HashSet<(Kind, Side, usize, i64)> = HashSet::new();
    for i in WARMUP.min(n)..n {
        // Fold in touches that resolved before today's close.
        for (k, side, held) in std::mem::take(&mut pending[i]) {
            stats.add(k, side, held);
        }
        let atr = s.atr[i];
        if !atr.is_finite() || atr <= 0.0 {
            continue;
        }
        let close = b[i].c;
        let mut srcs = src.at(i);
        daily_zones[i] = cluster(&srcs, close, tolerance(atr), &stats, false);
        for k in [0.75, 1.5, 2.25, 3.0] {
            srcs.push(Src { price: close + k * atr, kind: Kind::Control, mult: 1.0 });
            srcs.push(Src { price: close - k * atr, kind: Kind::Control, mult: 1.0 });
        }
        for x in &srcs {
            let dist = (x.price - close).abs() / atr;
            if !(0.5..=4.0).contains(&dist) {
                continue;
            }
            let side = if x.price < close { Side::Support } else { Side::Resistance };
            if let Some((resolved, held)) = touch_outcome(b, i, x.price, atr, side) {
                // One observation per kind, side, resolving bar and price bucket.
                let bucket = (x.price / (0.5 * atr)).round() as i64;
                if seen.insert((x.kind, side, resolved, bucket)) {
                    pending[(resolved + 1).min(n)].push((x.kind, side, held));
                }
            }
        }
    }
    for (k, side, held) in std::mem::take(&mut pending[n]) {
        stats.add(k, side, held);
    }
    Calibration { stats, daily_zones }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(i: usize, c: f64) -> Bar {
        Bar { t: 1_600_000_000 + i as i64 * 86_400, o: c, h: c + 0.6, l: c - 0.6, c, v: 1e6 }
    }

    /// Price oscillating between 90 and 110 with a period of 40 bars.
    fn range_bars(n: usize) -> Vec<Bar> {
        (0..n).map(|i| bar(i, 100.0 + 10.0 * (i as f64 * std::f64::consts::TAU / 40.0).sin())).collect()
    }

    #[test]
    fn swing_levels_in_a_range_beat_control() {
        let b = range_bars(700);
        let s = Series::new(&b);
        let src = Sources::new(&s);
        let cal = calibrate(&s, &src);
        let swing = cal.stats.lift(Kind::SwingLow, Side::Support);
        assert!(cal.stats.n(Kind::SwingLow, Side::Support) > 10);
        assert!(swing > 1.2, "swing low lift {swing}");
    }

    #[test]
    fn no_lookahead_in_sources() {
        let b = range_bars(400);
        let s = Series::new(&b);
        let src = Sources::new(&s);
        let a = src.at(300);
        // Truncating the future must not change what is known at bar 300.
        let b2 = b[..301].to_vec();
        let s2 = Series::new(&b2);
        let src2 = Sources::new(&s2);
        let a2 = src2.at(300);
        assert_eq!(a.len(), a2.len());
        for (x, y) in a.iter().zip(&a2) {
            assert_eq!(x.kind, y.kind);
            assert!((x.price - y.price).abs() < 1e-9);
        }
    }

    #[test]
    fn clustering_merges_nearby_sources() {
        let stats = Stats::default();
        let srcs = [
            Src { price: 100.0, kind: Kind::Sma200, mult: 1.0 },
            Src { price: 100.2, kind: Kind::Fib618, mult: 1.0 },
            Src { price: 105.0, kind: Kind::SwingHigh, mult: 1.0 },
        ];
        let z = cluster(&srcs, 102.0, 0.5, &stats, true);
        assert_eq!(z.len(), 2);
        assert_eq!(z[0].side, Side::Support);
        assert_eq!(z[0].sources.len(), 2);
        assert_eq!(z[1].side, Side::Resistance);
    }
}
