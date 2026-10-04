//! `ox pick`: from a ticker to a trade plan and the contracts that fit it, with the reasons.
//!
//! 1. Regime: trend score and volatility pricing ([`regime`]).
//! 2. Zones: support/resistance whose source weights are measured on this stock's history
//!    ([`zones`]).
//! 3. Setups: with-trend pullback and breakout plans (or range fades without a trend), each
//!    with B1/B2/SL/T1/T2 from the zones and a backtest of that exact plan ([`setup`]).
//! 4. Verdict: ENTER when a setup with a measured edge has triggered; WAIT with the
//!    conditions when one has an edge but has not triggered; NO TRADE when none has an edge.
//! 5. Contracts for the leading setup, ranked by simulating the plan ([`contracts`]).

pub mod contracts;
pub mod earnings;
pub mod regime;
pub mod setup;
pub mod zones;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use crate::analytics::gex::GexReport;
use crate::model::{Bar, ChainSnapshot};
use crate::provider::Earnings;
use contracts::{ContractParams, ContractsReport};
use regime::{Regime, Series};
use setup::{Direction, Setup, SetupKind, Status};
use zones::{Calibration, Kind, KindStat, Sources, Src, Zone};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickParams {
    /// Force a direction instead of reading it from the trend.
    pub bias: Option<Direction>,
    /// User levels; any given adds a "your levels" plan.
    pub entry: Option<f64>,
    pub stop: Option<f64>,
    pub t1: Option<f64>,
    pub t2: Option<f64>,
    /// Price contracts for the leading setup even when no setup has an edge.
    pub force_contracts: bool,
    pub contracts: ContractParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Enter,
    Wait,
    NoTrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gamma {
    pub zero_gamma: Option<f64>,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickReport {
    pub symbol: String,
    pub spot: f64,
    pub as_of: i64,
    pub verdict: Verdict,
    pub headline: String,
    /// Direction the setups take, and why.
    pub bias: Option<Direction>,
    pub bias_basis: String,
    pub regime: Regime,
    pub gamma: Option<Gamma>,
    pub earnings: Option<Earnings>,
    /// The next report's expected move (options-implied and past reactions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earnings_move: Option<earnings::EarningsMove>,
    /// The case for (or against) trading, one claim per line, each traceable to a number.
    pub thesis: Vec<String>,
    /// Why the verdict is not ENTER (empty when it is).
    pub why_not_now: Vec<String>,
    /// Leading setup first.
    pub setups: Vec<Setup>,
    /// Zones within 6 ATR, nearest first per side.
    pub supports: Vec<Zone>,
    pub resistances: Vec<Zone>,
    /// Measured hold rates per level source on this stock.
    pub calibration: Vec<KindStat>,
    pub history_from: Option<NaiveDate>,
    pub history_days: usize,
    pub contracts: Option<ContractsReport>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::analytics::sizing::Budget>,
    /// When ENTER but the book cannot fund any contract: holdings worth closing for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<crate::analytics::review::Replacement>,
}

pub struct Input<'a> {
    pub symbol: &'a str,
    pub daily: &'a [Bar],
    pub cal: &'a Calibration,
    pub spot: f64,
    pub chain: Option<&'a ChainSnapshot>,
    pub gex: Option<&'a GexReport>,
    pub earnings: Option<&'a Earnings>,
    pub iv_rank: Option<&'a crate::history::IvRank>,
    pub now: DateTime<Utc>,
    pub r: f64,
    /// OCC symbols whose quotes are live (ingested from the broker).
    pub live: &'a std::collections::HashSet<String>,
}

/// The walk-forward calibration for a bar series (expensive; the engine memoizes it).
pub fn calibrate(daily: &[Bar]) -> Calibration {
    let s = Series::new(daily);
    let src = Sources::new(&s);
    zones::calibrate(&s, &src)
}

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

fn lift_text(z: &Zone) -> String {
    let mut seen = std::collections::HashSet::new();
    z.sources
        .iter()
        .filter(|s| seen.insert(s.kind))
        .map(|s| {
            if matches!(s.kind, Kind::CallWall | Kind::PutWall | Kind::ZeroGamma) {
                format!("{} (options positioning, not backtested)", s.label)
            } else if s.n == 0 {
                format!("{} (no history)", s.label)
            } else {
                format!("{} (lift {:.2}, {} touches)", s.label, s.lift, s.n)
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Realized vol forecast over a plan's holding period, for simulating it.
pub fn sim_vol(reg: &Regime, hold_days: usize) -> f64 {
    match (reg.ewma_vol, reg.rv252) {
        (Some(e), Some(l)) => regime::forecast_vol(e, l, hold_days as f64, 0.97),
        _ => reg.rv_forecast_30d.or(reg.iv30).unwrap_or(0.3),
    }
}

pub fn pick(inp: &Input, p: &PickParams) -> anyhow::Result<PickReport> {
    let s = Series::new(inp.daily);
    let last = inp.daily.len().checked_sub(1).ok_or_else(|| anyhow::anyhow!("no daily history"))?;
    anyhow::ensure!(last >= zones::WARMUP, "need at least a year of daily history ({} bars)", inp.daily.len());
    let spot = inp.spot;
    let iv30 = inp.chain.and_then(|c| c.iv30);
    let reg =
        regime::regime(&s, iv30, inp.iv_rank).ok_or_else(|| anyhow::anyhow!("not enough history for the regime"))?;
    let atr = reg.atr;
    let today = inp.now.with_timezone(&New_York).date_naive();

    // Today's zones: price sources plus options positioning, weighted by the calibration.
    let mut srcs = Sources::new(&s).at(last);
    if let Some(g) = inp.gex {
        for (p, k) in [(g.call_wall, Kind::CallWall), (g.put_wall, Kind::PutWall), (g.zero_gamma, Kind::ZeroGamma)] {
            if let Some(p) = p {
                srcs.push(Src { price: p, kind: k, mult: 1.0 });
            }
        }
    }
    let all_zones = zones::cluster(&srcs, spot, zones::tolerance(atr), &inp.cal.stats, true);
    let near = |side: zones::Side| -> Vec<Zone> {
        let mut v: Vec<Zone> = all_zones
            .iter()
            .filter(|z| z.side == side && (z.price - spot).abs() <= 6.0 * atr && z.strength >= 0.15)
            .cloned()
            .collect();
        v.sort_by(|a, b| (a.price - spot).abs().total_cmp(&(b.price - spot).abs()));
        v
    };
    let (supports, resistances) = (near(zones::Side::Support), near(zones::Side::Resistance));

    let (bias, bias_basis) = match p.bias {
        Some(b) => (Some(b), "set by you (--bias)".to_string()),
        None if reg.trend_score >= 0.3 => {
            (Some(Direction::Long), format!("{} (score {:+.2})", reg.trend, reg.trend_score))
        }
        None if reg.trend_score <= -0.3 => {
            (Some(Direction::Short), format!("{} (score {:+.2})", reg.trend, reg.trend_score))
        }
        None => (None, format!("{} (score {:+.2}): range setups both ways", reg.trend, reg.trend_score)),
    };

    // Past earnings reactions in the bars, for the backtest split and the report-move history.
    let reactions = inp.earnings.map_or_else(Vec::new, |e| earnings::reaction_days(inp.daily, &e.past));
    let event_bars: Vec<usize> = reactions.iter().map(|(i, _)| *i).collect();
    let ctx = setup::Ctx { series: &s, cal: inp.cal, zones: &all_zones, spot, atr, events: &event_bars };
    let mut setups: Vec<Setup> = Vec::new();
    match bias {
        Some(dir) => {
            let kind = if p.bias.is_some() && dir.d() * reg.trend_score < 0.3 {
                SetupKind::Range
            } else {
                SetupKind::Pullback
            };
            setups.extend(setup::pullback(&ctx, dir, kind));
            setups.extend(setup::deep_pullback(&ctx, dir, kind));
            if kind == SetupKind::Pullback {
                setups.extend(setup::breakout(&ctx, dir));
                setups.extend(setup::trend_now(&ctx, dir));
            }
        }
        None => {
            for dir in [Direction::Long, Direction::Short] {
                setups.extend(setup::pullback(&ctx, dir, SetupKind::Range));
                setups.extend(setup::deep_pullback(&ctx, dir, SetupKind::Range));
            }
        }
    }
    if p.entry.is_some() || p.stop.is_some() || p.t1.is_some() || p.t2.is_some() {
        let entry = p.entry.unwrap_or(spot);
        let dir = bias.unwrap_or(match p.t1 {
            Some(t) if t < entry => Direction::Short,
            _ => Direction::Long,
        });
        setups.insert(0, setup::manual(&ctx, dir, p.entry, p.stop, p.t1, p.t2));
    }

    // Earnings: no fresh entries into the print.
    let earnings = inp.earnings.filter(|e| e.date >= today).cloned();
    let earnings_move =
        earnings.as_ref().map(|e| earnings::model(inp.daily, &reactions, inp.chain, e.date, e.estimated, inp.now));
    let mut warnings = Vec::new();
    if let Some(e) = &earnings {
        let days = (e.date - today).num_days();
        for st in &mut setups {
            let hold_end = today + Duration::days((st.days_to_t2 * 1.45).ceil() as i64);
            if days <= 5 && st.status == Status::Enter {
                st.status = Status::Wait;
                st.why_not_now.push(format!("earnings {} in {days} days: enter after the print, not into it", e.date));
            } else if e.date <= hold_end {
                st.why.push(format!("earnings {} falls inside the expected holding period: gap risk", e.date));
                let (th, wo) = (&st.backtest.through_earnings, &st.backtest.without_earnings);
                if th.trades >= 3 && wo.trades >= 3 {
                    st.why.push(format!(
                        "past signals held through a report: {} trades at {:+.2}R vs {} without at {:+.2}R",
                        th.trades, th.expectancy_r, wo.trades, wo.expectancy_r
                    ));
                }
            }
        }
        if days <= 5 {
            warnings.push(format!("earnings {}{}", e.date, if e.estimated { " (estimated)" } else { "" }));
        }
    }

    let t_family = setup::family_t(setups.len());
    for st in &mut setups {
        st.robust = st.has_edge && st.backtest.t_stat >= t_family && st.backtest.t_excess >= t_family;
    }

    // Choose: enter now only if no waiting setup has done meaningfully better (0.1R) historically.
    let by_score = |a: &Setup, b: &Setup| b.score.total_cmp(&a.score);
    setups.sort_by(|a, b| (b.kind == SetupKind::Manual).cmp(&(a.kind == SetupKind::Manual)).then(by_score(a, b)));
    let best_enter =
        setups.iter().filter(|s| s.has_edge && s.status == Status::Enter).map(|s| s.score).fold(f64::NAN, f64::max);
    let best_wait =
        setups.iter().filter(|s| s.has_edge && s.status == Status::Wait).map(|s| s.score).fold(f64::NAN, f64::max);
    let enter_wins = best_enter.is_finite() && (best_wait.is_nan() || best_wait - best_enter <= 0.1);
    if !setups.first().is_some_and(|s| s.kind == SetupKind::Manual) {
        let key = |s: &Setup| (s.has_edge && (s.status == Status::Enter) == enter_wins, s.has_edge);
        setups.sort_by(|a, b| key(b).cmp(&key(a)).then(by_score(a, b)));
    }
    if enter_wins && best_wait.is_finite() {
        for s in setups.iter_mut().filter(|s| s.status == Status::Wait && s.has_edge) {
            s.why_not_now.push(format!(
                "waiting for it has not paid more than entering now ({:+.2}R vs {best_enter:+.2}R)",
                s.score
            ));
        }
    } else if best_wait.is_finite() && best_enter.is_finite() {
        for s in setups.iter_mut().filter(|s| s.status == Status::Enter && s.has_edge) {
            s.why_not_now.push(format!(
                "entering now made {:+.2}R historically vs {best_wait:+.2}R waiting for the trigger",
                s.score
            ));
        }
    }

    let trend_brief = reg.trend_signals.iter().map(|(_, t)| t.as_str()).take(4).collect::<Vec<_>>().join(", ");
    for st in &mut setups {
        let bt = &st.backtest;
        let mut why = vec![format!("trend: {} — {}", reg.trend, trend_brief)];
        if let Some(z) = &st.b1.zone {
            why.push(format!("B1 {:.2}: {}", st.b1.price, lift_text(z)));
        }
        if let Some(b2) = &st.b2 {
            if let Some(z) = &b2.zone {
                why.push(format!("B2 {:.2}: {}", b2.price, lift_text(z)));
            }
        }
        why.push(format!("SL {:.2}: {}", st.sl.price, st.sl.basis));
        why.push(format!("T1 {:.2}: {}", st.t1.price, st.t1.zone.as_ref().map_or(st.t1.basis.clone(), lift_text)));
        why.push(format!("T2 {:.2}: {}", st.t2.price, st.t2.zone.as_ref().map_or(st.t2.basis.clone(), lift_text)));
        if bt.signals.trades > 0 {
            why.push(format!(
                "history: {} past signals (~{:.0} independent) {} to {}: T1 before SL {} vs {} no-edge, T2 {}, expectancy {:+.2}R ({:+.2}R after shrinking for sample size, t = {:.2}); trend-only control {:+.2}R",
                bt.signals.trades,
                bt.effective_n,
                bt.first_signal.map_or("?".into(), |d| d.to_string()),
                bt.last_signal.map_or("?".into(), |d| d.to_string()),
                pct(bt.signals.p_t1),
                pct(bt.no_edge_p_t1),
                pct(bt.signals.p_t2),
                bt.signals.expectancy_r,
                bt.expectancy_r_shrunk,
                bt.t_stat,
                bt.trend_only.expectancy_r
            ));
        } else {
            why.push("history: no past signals of this setup with this plan shape".into());
        }
        if !st.obstacles.is_empty() {
            why.push(format!(
                "in the way before T1: {}",
                st.obstacles.iter().map(|o| format!("{:.2} ({})", o.price, o.basis)).collect::<Vec<_>>().join("; ")
            ));
        }
        why.append(&mut st.why);
        st.why = why;
        if !st.has_edge {
            let reason = if bt.signals.trades < 8 {
                format!("only {} past signals: too few to measure an edge", bt.signals.trades)
            } else if bt.expectancy_r_shrunk <= 0.05 {
                format!("expectancy {:+.2}R ({:+.2}R shrunk): no edge", bt.signals.expectancy_r, bt.expectancy_r_shrunk)
            } else if bt.t_stat < setup::MIN_T {
                format!(
                    "{:+.2}R could be luck: t = {:.2} on ~{:.0} independent trades (needs {:.2})",
                    bt.signals.expectancy_r,
                    bt.t_stat,
                    bt.effective_n,
                    setup::MIN_T
                )
            } else {
                let (what, ctl) = if bt.control == "trend_only" {
                    ("entering on any day in the same trend", bt.trend_only.expectancy_r)
                } else {
                    ("entering on any day", bt.any_day.expectancy_r)
                };
                let kind = if bt.control == "trend_only" { "the trend" } else { "the stock's past drift" };
                if bt.excess_r < 0.05 {
                    format!(
                        "{:+.2}R is no better than {what} ({ctl:+.2}R): it is {kind}, not a signal",
                        bt.signals.expectancy_r
                    )
                } else {
                    format!(
                        "{:+.2}R beats {what} ({ctl:+.2}R) by {:+.2}R, but that could be luck: t = {:.2} (needs {:.2}); the rest is {kind}",
                        bt.signals.expectancy_r,
                        bt.excess_r,
                        bt.t_excess,
                        setup::MIN_T
                    )
                }
            };
            st.why_not_now.push(reason);
        }
    }

    let verdict = match setups.first() {
        Some(st) if st.has_edge && st.status == Status::Enter => Verdict::Enter,
        Some(st) if st.has_edge => Verdict::Wait,
        _ => Verdict::NoTrade,
    };
    let sym = inp.symbol.to_ascii_uppercase();
    let headline = match (verdict, setups.first()) {
        (Verdict::Enter, Some(st)) if st.kind == SetupKind::TrendNow || st.kind == SetupKind::TrendUnderZone => {
            format!(
                "ENTER {} at market — stop {:.2}, targets {:.2} / {:.2}",
                st.name, st.sl.price, st.t1.price, st.t2.price
            )
        }
        (Verdict::Enter, Some(st)) => format!(
            "ENTER {} — triggered: {}; stop {:.2}, targets {:.2} / {:.2}",
            st.name, st.trigger, st.sl.price, st.t1.price, st.t2.price
        ),
        (Verdict::Wait, Some(st)) => format!("WAIT — not at {spot:.2}. {}: {}", st.name, st.trigger),
        (_, Some(st)) => format!(
            "NO TRADE — no setup on {sym} has a measured edge right now (closest: {}, {:+.2}R over {} signals)",
            st.name, st.backtest.signals.expectancy_r, st.backtest.signals.trades
        ),
        (_, None) => format!("NO TRADE — no proven zones near {spot:.2} to build a plan from"),
    };

    let mut why_not_now = Vec::new();
    if verdict != Verdict::Enter {
        if bias.is_none() {
            why_not_now.push(format!("no trend to lean on ({}); only range fades at the edges qualify", reg.trend));
        }
        for st in setups.iter().filter(|s| s.kind != SetupKind::Manual || setups.len() == 1) {
            for w in &st.why_not_now {
                why_not_now.push(format!("{}: {w}", st.name));
            }
        }
    }

    let gamma = inp.gex.map(|g| {
        let note = match g.zero_gamma {
            Some(z) if spot >= z => {
                format!("above zero gamma {z:.2}: dealer hedging damps moves; pullbacks tend to be bought")
            }
            Some(z) => format!("below zero gamma {z:.2}: dealer hedging amplifies moves; levels break more easily"),
            None => "no zero-gamma crossing found".into(),
        };
        Gamma { zero_gamma: g.zero_gamma, call_wall: g.call_wall, put_wall: g.put_wall, note }
    });

    let mut thesis = vec![format!("trend: {} — {}", bias_basis, trend_brief)];
    match (supports.first(), resistances.first()) {
        (Some(su), Some(re)) => thesis.push(format!(
            "location: {spot:.2} is {:.1} ATR above support {:.2} ({}) and {:.1} ATR below resistance {:.2} ({})",
            (spot - su.price) / atr,
            su.price,
            su.describe(),
            (re.price - spot) / atr,
            re.price,
            re.describe()
        )),
        (Some(su), None) => thesis.push(format!(
            "location: no proven resistance within 6 ATR; support {:.2} ({})",
            su.price,
            su.describe()
        )),
        (None, Some(re)) => thesis.push(format!(
            "location: no proven support within 6 ATR; resistance {:.2} ({})",
            re.price,
            re.describe()
        )),
        (None, None) => {}
    }
    let mut vol = reg.vol_note.clone();
    match (reg.iv_rank, reg.iv_history_days) {
        (Some(rank), Some(n)) if n >= 60 => vol.push_str(&format!("; IV rank {rank:.0} over {n} recorded days")),
        (_, Some(n)) => vol.push_str(&format!("; IV rank not yet meaningful ({n} days recorded)")),
        _ => {}
    }
    thesis.push(format!("volatility: {vol}"));
    if let Some(g) = &gamma {
        thesis.push(format!("positioning: {}", g.note));
    }
    if let Some(st) = setups.first() {
        let bt = &st.backtest;
        thesis.push(format!(
            "evidence: {} has {} past signals (~{:.0} independent), {:+.2}R expectancy ({:+.2}R shrunk), {:+.2}R over the {} control, t = {:.2} ({}); edge source: {}",
            st.name,
            bt.signals.trades,
            bt.effective_n,
            bt.signals.expectancy_r,
            bt.expectancy_r_shrunk,
            bt.excess_r,
            bt.control.replace('_', "-"),
            bt.t_excess,
            if st.robust {
                format!("clears {t_family:.2}, the bar after testing {} setups", setups.len())
            } else if st.has_edge {
                format!("clears {:.2} but not {t_family:.2}, the bar after testing {} setups: treat as tentative", setup::MIN_T, setups.len())
            } else {
                format!("needs {:.2}", setup::MIN_T)
            },
            st.edge_source
        ));
    }
    if let Some(e) = &earnings {
        let mut line = format!(
            "event: earnings {}{} ({} days)",
            e.date,
            if e.estimated { " (estimated)" } else { "" },
            (e.date - today).num_days()
        );
        if let Some(m) = &earnings_move {
            if let Some(x) = m.implied_move_pct {
                line.push_str(&format!("; options imply a ±{x:.1}% report move"));
            }
            if let Some(x) = m.past_rms_pct {
                let last: Vec<String> = m.past.iter().take(4).map(|p| format!("{:+.1}%", p.move_pct)).collect();
                line.push_str(&format!(
                    "; past {} reports moved ±{x:.1}% (RMS; last: {})",
                    m.past.len(),
                    last.join(", ")
                ));
            }
        }
        thesis.push(line);
    }

    // Contracts for the leading setup, unless nothing has an edge and no levels were given.
    let manual = setups.first().is_some_and(|s| s.kind == SetupKind::Manual);
    let contracts = match (inp.chain, setups.first()) {
        (Some(chain), Some(st)) if verdict != Verdict::NoTrade || manual || p.force_contracts => {
            let sim_vol = sim_vol(&reg, st.backtest.hold_days);
            let event = earnings_move
                .as_ref()
                .map(|m| contracts::Event { date: m.date, sd: m.sim_move_pct.map(|x| x / 100.0) });
            Some(contracts::rank(chain, st, spot, sim_vol, event, &p.contracts, inp.now, inp.r, inp.live))
        }
        _ => None,
    };
    if let Some(c) = inp.chain {
        if c.freshness.quotes_stale && contracts.is_none() {
            warnings.push("option quotes are from the last close".into());
        }
    }

    Ok(PickReport {
        symbol: sym,
        spot,
        as_of: inp.now.timestamp(),
        verdict,
        headline,
        bias,
        bias_basis,
        regime: reg,
        gamma,
        earnings,
        earnings_move,
        thesis,
        why_not_now,
        setups,
        supports: supports.into_iter().take(4).collect(),
        resistances: resistances.into_iter().take(4).collect(),
        calibration: inp.cal.table(),
        history_from: inp
            .daily
            .first()
            .and_then(|b| chrono::TimeZone::timestamp_opt(&Utc, b.t, 0).single())
            .map(|t| t.date_naive()),
        history_days: inp.daily.len(),
        contracts,
        warnings,
        budget: None,
        replace: Vec::new(),
    })
}
