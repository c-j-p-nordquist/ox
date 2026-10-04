//! Trade plans built from measured zones, and a backtest of each plan's exact shape.
//!
//! A plan has scale-in entries (B1, B2), a stop (SL) beyond the level that invalidates it, and
//! two targets (T1, T2). Management is fixed so it can be tested: half off at T1 with the stop
//! moved to breakeven, the rest at T2, everything closed at the time stop.
//!
//! The backtest replays that plan on the stock's own history at every past day the setup's
//! conditions held, using the zones and weights known on that day. Two references keep it
//! honest: the driftless probability of reaching T1 before SL (which is (entry − SL) /
//! (T1 − SL) in ATR units, the "no edge" number), and a trend-only control that enters on every
//! day with the same trend, which shows how much of the result is just the stock's drift.

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use super::regime::Series;
use super::zones::{Calibration, Zone, WARMUP};
use crate::model::Bar;

/// Zone strength needed to anchor an entry, stop or target.
pub const QUALIFY: f64 = 0.3;
/// Of which at least this much must come from backtested sources.
pub const QUALIFY_TESTED: f64 = 0.2;

fn qualifies(z: &Zone) -> bool {
    z.strength >= QUALIFY && z.tested_strength >= QUALIFY_TESTED
}
/// Pseudo-observations pulling backtest results toward "no edge".
const SHRINK_N: f64 = 10.0;
/// Bars skipped after a historical signal so overlapping trades are not double counted.
const COOLDOWN: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    pub fn d(self) -> f64 {
        if self == Direction::Long {
            1.0
        } else {
            -1.0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupKind {
    /// With the trend: buy a pullback into a proven zone once it holds.
    Pullback,
    /// With the trend: buy a daily close through the nearest zone ahead.
    Breakout,
    /// With the trend: enter now at market, stop beyond the nearest proven zone behind.
    TrendNow,
    /// The same, with a proven zone within 0.5 ATR overhead (tested separately).
    TrendUnderZone,
    /// No trend: fade the edge of the range.
    Range,
    /// Levels given by the user.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The trigger has fired and price is still in the entry zone.
    Enter,
    /// Valid plan; the trigger has not fired.
    Wait,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanLevel {
    pub price: f64,
    /// Why this price: the zone's sources, or the rule that produced it.
    pub basis: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<Zone>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Outcomes {
    pub trades: usize,
    pub p_t1: f64,
    pub p_t2: f64,
    pub p_stop: f64,
    /// Mean result in R (multiples of the initial risk).
    pub expectancy_r: f64,
    pub median_days_t1: Option<f64>,
    pub median_days_t2: Option<f64>,
    /// 80th percentile of days to T2, among trades that reached it.
    pub p80_days_t2: Option<f64>,
    /// Standard deviation of trade results, R.
    pub sd_r: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backtest {
    /// Historical signals of this setup with this plan shape.
    pub signals: Outcomes,
    pub first_signal: Option<NaiveDate>,
    pub last_signal: Option<NaiveDate>,
    /// Driftless chance of T1 before SL: what a coin flip with these distances gives.
    pub no_edge_p_t1: f64,
    /// Same plan entered on every day with the same trend condition (no level).
    pub trend_only: Outcomes,
    /// Same plan entered on any day at all: the stock's own drift over the sample.
    pub any_day: Outcomes,
    /// Non-overlapping trades the signals amount to: min(signals, span / holding period).
    pub effective_n: f64,
    /// One-sided t-statistic of the mean result on the effective sample (1.65 ≈ 95%).
    pub t_stat: f64,
    /// The control the edge is measured against: `any_day` for a plain trend entry (the trend
    /// filter is the claim), else the better of `trend_only` and `any_day` (the level must add
    /// timing beyond the trend and the drift).
    pub control: String,
    /// Signal expectancy minus the control's, R.
    pub excess_r: f64,
    /// t-statistic of `excess_r` on the effective sample, the control's mean taken as the
    /// benchmark. Without it a strong trending stock's drift alone makes any long setup look
    /// significant.
    pub t_excess: f64,
    /// Signal results shrunk toward no edge by effective sample size; what ranking uses.
    pub p_t1_shrunk: f64,
    pub expectancy_r_shrunk: f64,
    /// Time stop used, trading days.
    pub hold_days: usize,
    /// Signals split by whether an earnings reaction fell inside the trade (reaction days
    /// reconstructed from the bars; see [`super::earnings`]).
    #[serde(default)]
    pub through_earnings: Outcomes,
    #[serde(default)]
    pub without_earnings: Outcomes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub price: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setup {
    pub kind: SetupKind,
    pub direction: Direction,
    pub status: Status,
    pub name: String,
    /// What has to happen before entering, in checkable terms.
    pub trigger: String,
    pub b1: PlanLevel,
    pub b2: Option<PlanLevel>,
    pub sl: PlanLevel,
    pub t1: PlanLevel,
    pub t2: PlanLevel,
    /// Average fill if both B1 and B2 fill in equal size.
    pub avg_entry: f64,
    pub risk_per_share: f64,
    pub rr_t1: f64,
    pub rr_t2: f64,
    /// Zones between entry and T1 that price must get through.
    pub obstacles: Vec<PlanLevel>,
    pub backtest: Backtest,
    /// Expected trading days to T1 and T2 (backtest medians, else a volatility estimate).
    pub days_to_t1: f64,
    pub days_to_t2: f64,
    pub why: Vec<String>,
    pub why_not_now: Vec<String>,
    pub alerts: Vec<Alert>,
    /// Shrunk expectancy in R; the ranking key.
    pub score: f64,
    /// Whether the backtest supports the plan (enough signals and a positive shrunk expectancy).
    pub has_edge: bool,
    /// Where the edge comes from: drift, the trend filter, or the level's timing.
    pub edge_source: String,
    /// The edge also clears the threshold corrected for the number of setups tested (Šidák).
    pub robust: bool,
}

fn date(t: i64) -> Option<NaiveDate> {
    Utc.timestamp_opt(t, 0).single().map(|x| x.with_timezone(&New_York).date_naive())
}

/// Plan shape in ATR units from the entry.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub stop: f64,
    pub t1: f64,
    pub t2: f64,
}

impl Shape {
    pub fn hold_days(&self) -> usize {
        // About 1.5x the driftless time to cover T2 (daily range ≈ 0.8 ATR), within 15–60 days.
        ((1.5 * (self.t2 / 0.8).powi(2)).round() as usize).clamp(15, 60)
    }
}

#[derive(Debug, Clone, Copy)]
struct Trade {
    r: f64,
    t1_days: Option<usize>,
    t2_days: Option<usize>,
    stopped: bool,
    /// Bar the trade ended on.
    exit: usize,
}

/// Replay the plan from the close of bar `j` (entry at `entry`), managing it bar by bar.
fn replay(b: &[Bar], j: usize, entry: f64, atr: f64, d: f64, sh: Shape, hold: usize) -> Option<Trade> {
    let risk = sh.stop * atr;
    let (sl, t1, t2) = (entry - d * risk, entry + d * sh.t1 * atr, entry + d * sh.t2 * atr);
    let r_at = |p: f64| d * (p - entry) / risk;
    let end = (j + 1 + hold).min(b.len());
    if end <= j + 1 {
        return None;
    }
    let mut half_done: Option<usize> = None;
    let mut banked = 0.0;
    for (k, x) in b.iter().enumerate().take(end).skip(j + 1) {
        let days = k - j;
        let (adverse, favorable) = if d > 0.0 { (x.l, x.h) } else { (x.h, x.l) };
        let stop = if half_done.is_some() { entry } else { sl };
        // Stop first within a bar (conservative); a gap through it fills at the open.
        if d * (adverse - stop) <= 0.0 {
            let fill = if d * (x.o - stop) < 0.0 { x.o } else { stop };
            let rest = if half_done.is_some() { 0.5 } else { 1.0 };
            return Some(Trade {
                r: banked + rest * r_at(fill),
                t1_days: half_done,
                t2_days: None,
                stopped: half_done.is_none(),
                exit: k,
            });
        }
        if half_done.is_none() && d * (favorable - t1) >= 0.0 {
            half_done = Some(days);
            banked += 0.5 * r_at(t1);
        }
        if half_done.is_some() && d * (favorable - t2) >= 0.0 {
            return Some(Trade {
                r: banked + 0.5 * r_at(t2),
                t1_days: half_done,
                t2_days: Some(days),
                stopped: false,
                exit: k,
            });
        }
    }
    let last = b[end - 1].c;
    let rest = if half_done.is_some() { 0.5 } else { 1.0 };
    Some(Trade { r: banked + rest * r_at(last), t1_days: half_done, t2_days: None, stopped: false, exit: end - 1 })
}

fn summarize(trades: &[Trade]) -> Outcomes {
    let n = trades.len();
    if n == 0 {
        return Outcomes::default();
    }
    let nf = n as f64;
    let pct = |mut v: Vec<f64>, q: f64| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(f64::total_cmp);
        Some(v[((v.len() - 1) as f64 * q).round() as usize])
    };
    let d1: Vec<f64> = trades.iter().filter_map(|t| t.t1_days).map(|x| x as f64).collect();
    let d2: Vec<f64> = trades.iter().filter_map(|t| t.t2_days).map(|x| x as f64).collect();
    Outcomes {
        trades: n,
        p_t1: d1.len() as f64 / nf,
        p_t2: d2.len() as f64 / nf,
        p_stop: trades.iter().filter(|t| t.stopped).count() as f64 / nf,
        expectancy_r: trades.iter().map(|t| t.r).sum::<f64>() / nf,
        median_days_t1: pct(d1, 0.5),
        median_days_t2: pct(d2.clone(), 0.5),
        p80_days_t2: pct(d2, 0.8),
        sd_r: if n > 1 {
            let m = trades.iter().map(|t| t.r).sum::<f64>() / nf;
            (trades.iter().map(|t| (t.r - m).powi(2)).sum::<f64>() / (nf - 1.0)).sqrt()
        } else {
            0.0
        },
    }
}

/// Which past days count as the trend condition for a setup.
fn trend_ok(kind: SetupKind, d: f64, score: f64) -> bool {
    match kind {
        SetupKind::Range => score.abs() < 0.3,
        SetupKind::Manual => score.is_finite(),
        _ => d * score >= 0.3,
    }
}

/// Historical signal at bar `i + 1` for a setup, using zones known at the close of bar `i`.
/// Returns the entry price (the signal bar's close).
fn signal(kind: SetupKind, d: f64, b: &[Bar], zones: &[Zone], atr: f64, i: usize) -> Option<f64> {
    let (c, x) = (b[i].c, &b[i + 1]);
    let q = zones.iter().filter(|z| qualifies(z));
    match kind {
        SetupKind::Pullback | SetupKind::Range | SetupKind::Manual => {
            // Nearest qualifying zone behind price; the next bar tests it and closes back above.
            let z = q
                .filter(|z| (0.25 * atr..=4.0 * atr).contains(&(d * (c - z.price))))
                .min_by(|a, b| (d * (c - a.price)).total_cmp(&(d * (c - b.price))))?;
            let ext = if d > 0.0 { x.l } else { x.h };
            (d * (ext - z.price) <= 0.25 * atr && d * (x.c - z.price) > 0.0).then_some(x.c)
        }
        SetupKind::TrendNow | SetupKind::TrendUnderZone => {
            let overhead = q.clone().any(|z| (0.0..=0.5 * atr).contains(&(d * (z.price - c))));
            (overhead == (kind == SetupKind::TrendUnderZone)).then_some(x.c)
        }
        SetupKind::Breakout => {
            let z = q
                .filter(|z| (0.0..=3.0 * atr).contains(&(d * (z.price - c))))
                .min_by(|a, b| (d * (a.price - c)).total_cmp(&(d * (b.price - c))))?;
            (d * (x.c - z.price) >= 0.25 * atr).then_some(x.c)
        }
    }
}

/// `events`: bar indices of past earnings reactions, sorted.
pub fn backtest(s: &Series, cal: &Calibration, kind: SetupKind, d: f64, sh: Shape, events: &[usize]) -> Backtest {
    let b = s.bars;
    let hold = sh.hold_days();
    let (mut sig, mut ctl, mut any) = (Vec::new(), Vec::new(), Vec::new());
    let mut next_any = 0usize;
    let (mut first, mut last) = (None, None);
    let (mut first_i, mut last_i) = (None, 0usize);
    let (mut next_sig, mut next_ctl) = (0usize, 0usize);
    let end = b.len().saturating_sub(1);
    for i in WARMUP.min(end)..end {
        let atr = s.atr[i];
        if !atr.is_finite() {
            continue;
        }
        if i >= next_any {
            if let Some(t) = replay(b, i, b[i].c, atr, d, sh, hold) {
                any.push(t);
                next_any = i + COOLDOWN;
            }
        }
        if !trend_ok(kind, d, s.trend[i]) {
            continue;
        }
        if i >= next_ctl {
            if let Some(t) = replay(b, i, b[i].c, atr, d, sh, hold) {
                ctl.push(t);
                next_ctl = i + COOLDOWN;
            }
        }
        if i + 1 >= next_sig {
            if let Some(entry) = signal(kind, d, b, &cal.daily_zones[i], atr, i) {
                if let Some(t) = replay(b, i + 1, entry, atr, d, sh, hold) {
                    sig.push((i + 1, t));
                    first = first.or(date(b[i + 1].t));
                    last = date(b[i + 1].t);
                    first_i = first_i.or(Some(i + 1));
                    last_i = i + 1;
                    next_sig = i + 1 + COOLDOWN;
                }
            }
        }
    }
    let spans_event = |j: usize, t: &Trade| events.iter().any(|&e| e > j && e <= t.exit);
    let pick = |through: bool| -> Vec<Trade> {
        sig.iter().filter(|(j, t)| spans_event(*j, t) == through).map(|(_, t)| *t).collect()
    };
    let (through_earnings, without_earnings) = (summarize(&pick(true)), summarize(&pick(false)));
    let sig: Vec<Trade> = sig.into_iter().map(|(_, t)| t).collect();
    let signals = summarize(&sig);
    let no_edge = sh.stop / (sh.stop + sh.t1);
    let span = first_i.map_or(0, |f| last_i - f + hold) as f64;
    let n = (signals.trades as f64).min(span / hold as f64);
    let t_stat = if signals.sd_r > 0.0 { signals.expectancy_r / signals.sd_r * n.sqrt() } else { 0.0 };
    let (trend_only, any_day) = (summarize(&ctl), summarize(&any));
    let use_trend =
        kind != SetupKind::TrendNow && trend_only.trades >= 8 && trend_only.expectancy_r > any_day.expectancy_r;
    let (control, c) = if use_trend { ("trend_only", &trend_only) } else { ("any_day", &any_day) };
    // The control's days contain the signal days (same stock, same period), so the two means are
    // not independent samples: test the signal against the control's mean as a fixed benchmark.
    let excess_r = signals.expectancy_r - c.expectancy_r;
    let t_excess = if signals.sd_r > 0.0 { excess_r / signals.sd_r * n.sqrt() } else { 0.0 };
    Backtest {
        effective_n: n,
        t_stat,
        control: control.into(),
        excess_r,
        t_excess,
        p_t1_shrunk: (signals.p_t1 * n + no_edge * SHRINK_N) / (n + SHRINK_N),
        expectancy_r_shrunk: signals.expectancy_r * n / (n + SHRINK_N),
        signals,
        first_signal: first,
        last_signal: last,
        no_edge_p_t1: no_edge,
        trend_only,
        any_day,
        hold_days: hold,
        through_earnings,
        without_earnings,
    }
}

/// Context for building plans from today's zones.
pub struct Ctx<'a> {
    pub series: &'a Series<'a>,
    pub cal: &'a Calibration,
    pub zones: &'a [Zone],
    pub spot: f64,
    pub atr: f64,
    /// Bar indices of past earnings reactions, sorted.
    pub events: &'a [usize],
}

fn level(z: &Zone) -> PlanLevel {
    PlanLevel { price: z.price, basis: z.describe(), zone: Some(z.clone()) }
}

fn qualifying(zones: &[Zone]) -> impl Iterator<Item = &Zone> {
    zones.iter().filter(|z| qualifies(z))
}

/// T1 and T2 beyond `entry` given the risk, plus obstacles before T1.
fn targets(
    zones: &[Zone],
    d: f64,
    entry: f64,
    risk: f64,
    atr: f64,
    exclude: &[f64],
) -> (PlanLevel, PlanLevel, Vec<PlanLevel>) {
    let mut ahead: Vec<&Zone> = qualifying(zones)
        .filter(|z| d * (z.price - entry) > 0.5 * atr && !exclude.iter().any(|e| (e - z.price).abs() < 1e-9))
        .collect();
    ahead.sort_by(|a, b| (d * (a.price - entry)).total_cmp(&(d * (b.price - entry))));
    let t1z = ahead.iter().position(|z| d * (z.price - entry) >= risk.max(atr));
    let t1 = match t1z {
        Some(k) => level(ahead[k]),
        None => PlanLevel {
            price: entry + d * 1.5 * risk,
            basis: "1.5R projection: no zone ahead with a measured hold rate".into(),
            zone: None,
        },
    };
    let obstacles: Vec<PlanLevel> =
        ahead.iter().filter(|z| d * (z.price - t1.price) < -1e-9).map(|z| level(z)).collect();
    let t2 = t1z
        .and_then(|k| {
            ahead[k + 1..].iter().find(|z| d * (z.price - t1.price) >= atr && d * (z.price - entry) >= 2.0 * risk)
        })
        .map(|z| level(z))
        .unwrap_or_else(|| {
            let p = entry + d * 2.5 * risk;
            let p = if d * (p - t1.price) < atr { t1.price + d * atr } else { p };
            PlanLevel {
                price: p,
                basis: "2.5R projection: no zone beyond T1 with a measured hold rate".into(),
                zone: None,
            }
        });
    (t1, t2, obstacles)
}

#[allow(clippy::too_many_arguments)]
fn finish(
    ctx: &Ctx,
    kind: SetupKind,
    dir: Direction,
    name: String,
    trigger: String,
    b1: PlanLevel,
    b2: Option<PlanLevel>,
    sl: PlanLevel,
    exclude: &[f64],
) -> Setup {
    let d = dir.d();
    let avg = b2.as_ref().map_or(b1.price, |b| (b1.price + b.price) / 2.0);
    let risk = (d * (avg - sl.price)).max(0.25 * ctx.atr);
    let (t1, t2, obstacles) = targets(ctx.zones, d, avg, risk, ctx.atr, exclude);
    let shape = Shape { stop: risk / ctx.atr, t1: d * (t1.price - avg) / ctx.atr, t2: d * (t2.price - avg) / ctx.atr };
    let bt = backtest(ctx.series, ctx.cal, kind, d, shape, ctx.events);
    // Expected time: backtest medians, else driftless first-passage scale (distance / daily σ)².
    let vol_days = |atrs: f64| (atrs / 0.8).powi(2).max(1.0);
    let days_to_t1 = bt.signals.median_days_t1.filter(|_| bt.signals.trades >= 8).unwrap_or(vol_days(shape.t1));
    let days_to_t2 =
        bt.signals.median_days_t2.filter(|_| bt.signals.trades >= 8).unwrap_or(vol_days(shape.t2)).max(days_to_t1);
    let has_edge = edge(&bt);
    let mut alerts = vec![Alert { price: b1.price, note: format!("{name}: B1") }];
    if let Some(b) = &b2 {
        alerts.push(Alert { price: b.price, note: format!("{name}: B2") });
    }
    let edge_source = edge_source(&bt, has_edge);
    Setup {
        robust: false,
        edge_source,
        kind,
        direction: dir,
        status: Status::Wait,
        name,
        trigger,
        avg_entry: avg,
        risk_per_share: risk,
        rr_t1: d * (t1.price - avg) / risk,
        rr_t2: d * (t2.price - avg) / risk,
        b1,
        b2,
        sl,
        t1,
        t2,
        obstacles,
        score: bt.expectancy_r_shrunk,
        backtest: bt,
        days_to_t1,
        days_to_t2,
        why: Vec::new(),
        why_not_now: Vec::new(),
        alerts,
        has_edge,
    }
}

/// Minimum one-sided t-statistic for an edge (≈ 95% confidence).
pub const MIN_T: f64 = 1.65;

/// One-sided t threshold keeping 95% family-wise confidence over `k` tests (Šidák, normal approx).
pub fn family_t(k: usize) -> f64 {
    let alpha = 1.0 - 0.95f64.powf(1.0 / k.max(1) as f64);
    // Invert the normal tail by bisection.
    let (mut lo, mut hi) = (0.0, 6.0);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if 1.0 - crate::analytics::bs::norm_cdf(mid) > alpha {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// A measured edge: enough signals, positive after shrinking, unlikely to be luck, and
/// significantly better than its control (so it is more than the drift, or the trend, over the
/// sample).
pub fn edge(bt: &Backtest) -> bool {
    bt.signals.trades >= 8
        && bt.expectancy_r_shrunk > 0.05
        && bt.t_stat >= MIN_T
        && bt.excess_r >= 0.05
        && bt.t_excess >= MIN_T
}

/// Split the plan's result into drift (any day), the trend filter, and the level's timing.
pub fn edge_source(bt: &Backtest, has_edge: bool) -> String {
    let (sig, trend, any) = (bt.signals.expectancy_r, bt.trend_only.expectancy_r, bt.any_day.expectancy_r);
    let split = format!(
        "{sig:+.2}R = drift {any:+.2}R + trend filter {:+.2}R + level timing {:+.2}R",
        trend - any,
        sig - trend
    );
    if !has_edge {
        return format!("none measured ({split})");
    }
    let (t, l) = (trend - any, sig - trend);
    let main = if any >= t.max(l) {
        "mostly the stock's drift over the sample, the least reliable kind"
    } else if t >= l {
        "mostly the trend filter"
    } else {
        "mostly the level's timing"
    };
    format!("{main} ({split})")
}

/// Qualifying zones behind price (support for a long), nearest first.
fn behind<'a>(ctx: &'a Ctx, d: f64) -> Vec<&'a Zone> {
    let (a, spot) = (ctx.atr, ctx.spot);
    let mut v: Vec<&Zone> =
        qualifying(ctx.zones).filter(|z| (-0.1 * a..=4.0 * a).contains(&(d * (spot - z.price)))).collect();
    v.sort_by(|x, y| (d * (spot - x.price)).total_cmp(&(d * (spot - y.price))));
    v
}

/// Buy a pullback into the nearest proven zone behind price (or fade the range edge).
pub fn pullback(ctx: &Ctx, dir: Direction, kind: SetupKind) -> Option<Setup> {
    let b1z = *behind(ctx, dir.d()).first()?;
    pullback_at(ctx, dir, kind, b1z, false)
}

/// Buy a deeper pullback into the strongest zone behind price, when it is not the nearest.
pub fn deep_pullback(ctx: &Ctx, dir: Direction, kind: SetupKind) -> Option<Setup> {
    let zs = behind(ctx, dir.d());
    let nearest = *zs.first()?;
    let strongest = *zs.iter().max_by(|a, b| a.strength.total_cmp(&b.strength))?;
    if std::ptr::eq(nearest, strongest) || strongest.strength < nearest.strength + 0.2 {
        return None;
    }
    pullback_at(ctx, dir, kind, strongest, true)
}

fn pullback_at(ctx: &Ctx, dir: Direction, kind: SetupKind, b1z: &Zone, deep: bool) -> Option<Setup> {
    let (d, a, spot) = (dir.d(), ctx.atr, ctx.spot);
    let behind = behind(ctx, d);
    let b2z = behind
        .iter()
        .filter(|z| (0.75 * a..=2.5 * a).contains(&(d * (b1z.price - z.price))))
        .max_by(|x, y| x.strength.total_cmp(&y.strength))
        .copied();
    let deepest = b2z.unwrap_or(b1z);
    let far = if d > 0.0 { deepest.low } else { deepest.high };
    let sl_price = if d > 0.0 {
        (far - 0.5 * a).min(deepest.price - 0.75 * a)
    } else {
        (far + 0.5 * a).max(deepest.price + 0.75 * a)
    };
    let which = if b2z.is_some() { "B2" } else { "B1" };
    let sl = PlanLevel {
        price: sl_price,
        basis: format!("beyond the {which} zone by 0.5 ATR: a close there means the level failed"),
        zone: None,
    };
    let verb = if d > 0.0 { "above" } else { "below" };
    let (side_word, name) = match (kind, dir) {
        (SetupKind::Range, Direction::Long) => ("support", "range long at support"),
        (SetupKind::Range, Direction::Short) => ("resistance", "range short at resistance"),
        (_, Direction::Long) => ("support", if deep { "deep pullback long" } else { "pullback long" }),
        (_, Direction::Short) => ("resistance", if deep { "deep rally short" } else { "rally short" }),
    };
    let trigger =
        format!("price trades into the B1 {side_word} zone ({:.2}) and a daily bar closes back {verb} it", b1z.price);
    let exclude: Vec<f64> = [Some(b1z.price), b2z.map(|z| z.price)].into_iter().flatten().collect();
    let mut s = finish(ctx, kind, dir, name.into(), trigger, level(b1z), b2z.map(level), sl, &exclude);

    // Status: the last bar is itself a signal bar, and price has not left the zone.
    let bars = ctx.series.bars;
    let last = bars.last()?;
    let ext = if d > 0.0 { last.l } else { last.h };
    let dist = d * (spot - b1z.price) / a;
    let tested = d * (ext - b1z.price) <= 0.25 * a;
    let held = d * (last.c - b1z.price) > 0.0;
    if tested && held && dist <= 0.75 {
        s.status = Status::Enter;
    } else if tested || dist <= 0.25 {
        s.why_not_now.push(format!(
            "price is at B1 {:.2} but has not confirmed: wait for a daily close back {verb} it",
            b1z.price
        ));
    } else {
        let rr_now = d * (s.t1.price - spot) / (d * (spot - s.sl.price)).max(1e-9);
        s.why_not_now.push(format!(
            "price {:.2} is {:.1} ATR from B1 {:.2}: entering here puts the stop {:.1} ATR away and R:R to T1 at {:.1} (vs {:.1} from B1/B2)",
            spot,
            dist,
            b1z.price,
            d * (spot - s.sl.price) / a,
            rr_now.max(0.0),
            s.rr_t1
        ));
    }
    Some(s)
}

/// Enter now with the trend; the stop goes beyond the nearest proven zone at least 1 ATR behind.
pub fn trend_now(ctx: &Ctx, dir: Direction) -> Option<Setup> {
    let (d, a, spot) = (dir.d(), ctx.atr, ctx.spot);
    let support = behind(ctx, d).into_iter().find(|z| d * (spot - z.price) >= a);
    let sl = match support {
        Some(z) => {
            let far = if d > 0.0 { z.low } else { z.high };
            let p =
                if d > 0.0 { (far - 0.5 * a).min(z.price - 0.75 * a) } else { (far + 0.5 * a).max(z.price + 0.75 * a) };
            PlanLevel { price: p, basis: format!("beyond {} by 0.5 ATR", z.describe()), zone: None }
        }
        None => PlanLevel { price: spot - d * 2.0 * a, basis: "2 ATR: no proven zone behind price".into(), zone: None },
    };
    let overhead = qualifying(ctx.zones).find(|z| (0.0..=0.5 * a).contains(&(d * (z.price - spot))));
    let kind = if overhead.is_some() { SetupKind::TrendUnderZone } else { SetupKind::TrendNow };
    let b1 = PlanLevel { price: spot, basis: "market, now".into(), zone: None };
    let name = if overhead.is_some() { "trend entry under resistance" } else { "trend entry now" };
    let trigger = "none: the trend is the signal; enter at market".to_string();
    let mut s = finish(ctx, kind, dir, name.into(), trigger, b1, None, sl, &[]);
    s.status = Status::Enter;
    if let Some(z) = overhead {
        s.why.push(format!(
            "{} is {:.1} ATR overhead; the backtest only counts past entries with a zone this close",
            z.describe(),
            d * (z.price - spot) / a
        ));
    }
    s.alerts.clear();
    Some(s)
}

/// Buy a daily close through the nearest proven zone ahead.
pub fn breakout(ctx: &Ctx, dir: Direction) -> Option<Setup> {
    let (d, a, spot) = (dir.d(), ctx.atr, ctx.spot);
    let z = qualifying(ctx.zones)
        .filter(|z| (0.0..=4.0 * a).contains(&(d * (z.price - spot))))
        .min_by(|x, y| (d * (x.price - spot)).total_cmp(&(d * (y.price - spot))))?;
    let edge = if d > 0.0 { z.high } else { z.low };
    let trigger_px = edge + d * 0.25 * a;
    let verb = if d > 0.0 { "above" } else { "below" };
    let b1 = PlanLevel {
        price: trigger_px,
        basis: format!("daily close {verb} {} (+0.25 ATR)", z.describe()),
        zone: Some(z.clone()),
    };
    let sl = PlanLevel {
        price: z.price - d * a,
        basis: "1 ATR back inside the broken zone: the breakout failed".into(),
        zone: None,
    };
    let name = if d > 0.0 { "breakout long" } else { "breakdown short" };
    let trigger = format!("a daily close {verb} {:.2}; buy the close or a retest of {:.2}", trigger_px, z.price);
    let mut s = finish(ctx, SetupKind::Breakout, dir, name.into(), trigger, b1, None, sl, &[z.price]);
    let last = ctx.series.bars.last()?;
    if d * (last.c - trigger_px) >= 0.0 && d * (spot - trigger_px) <= 0.75 * a {
        s.status = Status::Enter;
    } else if d * (spot - trigger_px) > 0.75 * a {
        s.why_not_now.push(format!(
            "already {:.1} ATR past the trigger: chasing; wait for a retest of {:.2}",
            d * (spot - trigger_px) / a,
            z.price
        ));
    } else {
        s.why_not_now.push(format!("no daily close {verb} {:.2} yet (price {:.2})", trigger_px, spot));
    }
    s.alerts = vec![Alert { price: trigger_px, note: format!("{name}: trigger (needs a daily close)") }];
    Some(s)
}

/// A plan from user-given levels; any missing level is filled from the zones.
pub fn manual(
    ctx: &Ctx,
    dir: Direction,
    entry: Option<f64>,
    stop: Option<f64>,
    t1: Option<f64>,
    t2: Option<f64>,
) -> Setup {
    let (d, a) = (dir.d(), ctx.atr);
    let entry = entry.unwrap_or(ctx.spot);
    let b1 = PlanLevel { price: entry, basis: "user".into(), zone: None };
    let sl = PlanLevel {
        price: stop.unwrap_or(entry - d * 1.5 * a),
        basis: if stop.is_some() { "user".into() } else { "1.5 ATR (default)".into() },
        zone: None,
    };
    let mut s =
        finish(ctx, SetupKind::Manual, dir, "your levels".into(), format!("entry at {entry:.2}"), b1, None, sl, &[]);
    let risk = s.risk_per_share;
    if let Some(p) = t1 {
        s.t1 = PlanLevel { price: p, basis: "user".into(), zone: None };
    }
    if let Some(p) = t2 {
        s.t2 = PlanLevel { price: p, basis: "user".into(), zone: None };
    }
    if t1.is_some() || t2.is_some() {
        s.rr_t1 = d * (s.t1.price - entry) / risk;
        s.rr_t2 = d * (s.t2.price - entry) / risk;
        let shape = Shape { stop: risk / a, t1: s.rr_t1 * risk / a, t2: s.rr_t2 * risk / a };
        s.backtest = backtest(ctx.series, ctx.cal, SetupKind::Manual, d, shape, ctx.events);
        s.score = s.backtest.expectancy_r_shrunk;
        s.has_edge = edge(&s.backtest);
        s.edge_source = edge_source(&s.backtest, s.has_edge);
    }
    if (ctx.spot - entry).abs() <= 0.25 * a {
        s.status = Status::Enter;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(path: &[f64]) -> Vec<Bar> {
        // Each bar opens at the previous close.
        path.iter()
            .enumerate()
            .map(|(i, &c)| {
                let o = if i == 0 { c } else { path[i - 1] };
                Bar { t: i as i64 * 86_400, o, h: o.max(c) + 0.5, l: o.min(c) - 0.5, c, v: 1.0 }
            })
            .collect()
    }

    #[test]
    fn replay_scales_out_and_moves_stop() {
        let sh = Shape { stop: 1.0, t1: 2.0, t2: 4.0 };
        // Entry 100, ATR 1: T1 102 then back to breakeven.
        let b = bars(&[100.0, 101.0, 102.0, 101.0, 99.0]);
        let t = replay(&b, 0, 100.0, 1.0, 1.0, sh, 20).unwrap();
        assert_eq!(t.t1_days, Some(2));
        assert!((t.r - 1.0).abs() < 1e-9, "half at +2R, half at breakeven: {}", t.r);
        // Straight to T2.
        let b = bars(&[100.0, 102.0, 104.0]);
        let t = replay(&b, 0, 100.0, 1.0, 1.0, sh, 20).unwrap();
        assert!((t.r - 3.0).abs() < 1e-9, "{}", t.r);
        // Stopped before T1.
        let b = bars(&[100.0, 99.4, 98.0]);
        let t = replay(&b, 0, 100.0, 1.0, 1.0, sh, 20).unwrap();
        assert!(t.stopped && (t.r + 1.0).abs() < 1e-9, "{t:?}");
    }

    #[test]
    fn family_threshold_grows_with_tests() {
        assert!((family_t(1) - 1.645).abs() < 0.01);
        assert!((family_t(4) - 2.23).abs() < 0.02, "{}", family_t(4));
    }

    #[test]
    fn short_replay_mirrors_long() {
        let sh = Shape { stop: 1.0, t1: 2.0, t2: 4.0 };
        let b = bars(&[100.0, 98.0, 96.0]);
        let t = replay(&b, 0, 100.0, 1.0, -1.0, sh, 20).unwrap();
        assert!((t.r - 3.0).abs() < 1e-9, "{}", t.r);
    }
}
