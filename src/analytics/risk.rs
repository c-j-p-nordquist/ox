//! Portfolio risk: Greeks per underlying, scenario P&L at GEX levels and price moves, and flags.
//!
//! Scenarios are instantaneous moves with IV held constant (sticky strike), repriced with
//! Black-Scholes, reported as P&L change from the current model value.
//!
//! Book-level risk comes from the holdings' own daily-return covariance (delta-weighted), split
//! into the part a benchmark explains and the company-specific rest. It is first order in delta:
//! right for typical days; for gaps, the repriced ladders and shocks are the better guide.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::bs;
use super::factor;
use super::gex::GexReport;
use super::market::Freshness;
use super::sizing::{self, Budget, SizingConfig};
use crate::model::{Bar, ChainSnapshot, OptionContract, Right};
use crate::portfolio::{PortfolioFile, Position, SecType};

/// Market data for one underlying, gathered by the engine.
pub struct UnderlyingData<'a> {
    pub chain: Option<&'a ChainSnapshot>,
    pub gex: Option<&'a GexReport>,
    /// Fallback spot when there is no chain (stocks without options data).
    pub quote: Option<f64>,
    /// Daily bars for return statistics.
    pub history: Option<&'a [Bar]>,
    pub earnings: Option<crate::provider::Earnings>,
}

/// Benchmarks the book's market risk is measured against.
pub struct FactorInput<'a> {
    /// Candidate benchmarks with their daily bars (e.g. SPY, QQQ).
    pub benchmarks: Vec<(String, &'a [Bar])>,
    /// Use this benchmark instead of the best-fitting one.
    pub force: Option<String>,
    /// Trading days of history to use.
    pub lookback: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkFit {
    pub benchmark: String,
    /// Exposure-weighted R² of the holdings against this benchmark.
    pub weighted_r2: f64,
}

/// Covariance-based view of the whole book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookRisk {
    pub benchmark: String,
    pub fits: Vec<BenchmarkFit>,
    pub observations: usize,
    /// One-standard-deviation daily P&L of the book, $.
    pub daily_sigma: f64,
    pub daily_sigma_pct: Option<f64>,
    /// Part explained by the benchmark, $ (1σ).
    pub market_sigma: f64,
    /// Company-specific remainder, $ (1σ).
    pub specific_sigma: f64,
    /// Share of the book's daily variance explained by the benchmark (0–1).
    pub market_share: f64,
    /// Sum of each position's standalone 1σ: the risk if everything moved together.
    pub standalone_sigma: f64,
    /// `1 − daily_sigma / standalone_sigma`.
    pub diversification: f64,
    /// Benchmark moves with each name following its beta, fully repriced.
    pub market_shocks: Vec<Scenario>,
}

/// Broad-market conditions the book is judged against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketContext {
    pub spy: f64,
    pub spy_change_pct: f64,
    /// Daily trend from close vs EMA9/EMA21: up, down or mixed.
    pub spy_trend: String,
    pub spy_rsi: Option<f64>,
    pub spy_above_50d: Option<bool>,
    pub spy_above_200d: Option<bool>,
    pub spy_zero_gamma: Option<f64>,
    pub spy_call_wall: Option<f64>,
    pub spy_put_wall: Option<f64>,
    pub vix: Option<f64>,
    pub vix_change_pct: Option<f64>,
    /// Conditions that argue for defense (each is a fact, not a verdict).
    pub risk_off: Vec<String>,
    pub risk_on: Vec<String>,
}

/// How one-sided the book is.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Directional {
    pub net_delta_dollars: f64,
    /// Benchmark the equivalent exposure is expressed in.
    pub benchmark: Option<String>,
    /// Sum of delta dollars × beta: the book expressed as benchmark exposure.
    pub benchmark_equivalent: f64,
    pub benchmark_equivalent_pct: Option<f64>,
    /// P&L for a 1% benchmark move via beta (first order).
    pub pnl_per_benchmark_pct: f64,
    pub long_call_premium: f64,
    pub long_put_premium: f64,
    /// Share of long premium in calls (0–1).
    pub call_share: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub label: String,
    pub price: f64,
    pub move_pct: f64,
    pub pnl: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exposure {
    pub underlying: String,
    pub spot: Option<f64>,
    pub positions: usize,
    pub market_value: f64,
    pub unrealized: f64,
    /// Share-equivalent delta.
    pub delta_shares: f64,
    /// Delta shares × spot; P&L for a 1% move ≈ `delta_dollars / 100`.
    pub delta_dollars: f64,
    /// Change in share-equivalent delta per $1 move.
    pub gamma_shares: f64,
    /// Dollars per calendar day.
    pub theta_day: f64,
    /// Dollars per 1 vol point.
    pub vega: f64,
    /// Beta and R² against the book's benchmark.
    pub beta: Option<f64>,
    pub r2: Option<f64>,
    /// Daily return volatility of the underlying.
    pub daily_vol: Option<f64>,
    /// Standalone 1σ daily P&L of this underlying's positions, $.
    pub daily_sigma: Option<f64>,
    /// Share of the book's daily variance from this underlying (can be negative for hedges).
    pub risk_share: Option<f64>,
    /// `delta_dollars × beta`.
    pub benchmark_equivalent: Option<f64>,
    /// The underlying's own ±2σ day, fully repriced.
    pub shocks: Vec<Scenario>,
    pub next_earnings: Option<crate::provider::Earnings>,
    /// P&L at GEX levels and ±1σ (1 week).
    pub scenarios: Vec<Scenario>,
    /// P&L across fixed percentage moves.
    pub ladder: Vec<Scenario>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Warn,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flag {
    pub severity: Severity,
    pub position: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Totals {
    pub market_value: f64,
    pub unrealized: f64,
    pub delta_dollars: f64,
    pub theta_day: f64,
    pub vega: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskReport {
    pub as_of: Option<String>,
    pub account: Option<String>,
    pub net_liquidation: Option<f64>,
    pub currency: Option<String>,
    /// Freshness of the option data used (worst case across underlyings).
    pub freshness: Option<Freshness>,
    pub exposures: Vec<Exposure>,
    pub totals: Totals,
    /// Sizing limits and how much of each the book uses (needs net liquidation and FX).
    pub budget: Option<Budget>,
    pub directional: Directional,
    pub book_risk: Option<BookRisk>,
    pub market: Option<MarketContext>,
    pub flags: Vec<Flag>,
}

const LADDER: [f64; 7] = [-10.0, -5.0, -2.0, 0.0, 2.0, 5.0, 10.0];

fn find_contract<'a>(chain: Option<&'a ChainSnapshot>, p: &Position) -> Option<&'a OptionContract> {
    chain?.contracts.iter().find(|c| {
        Some(c.expiry) == p.expiry && Some(c.right) == p.right && p.strike.is_some_and(|k| (k - c.strike).abs() < 1e-6)
    })
}

/// A position ready for repricing.
struct Priced<'a> {
    pos: &'a Position,
    mult: f64,
    /// Option pricing inputs: right, strike, years to expiry, iv.
    option: Option<(Right, f64, f64, f64)>,
    mark: Option<f64>,
}

impl Priced<'_> {
    fn value(&self, s: f64, r: f64) -> f64 {
        match self.option {
            Some((right, k, t, iv)) => bs::value(right, s, k, t, r, iv),
            None => s,
        }
    }
}

/// `fx` is base-currency units per USD (1.0 for USD accounts); `None` if unknown.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    file: &PortfolioFile,
    data: &BTreeMap<String, UnderlyingData>,
    now: DateTime<Utc>,
    r: f64,
    fx: Option<f64>,
    sizing_cfg: &SizingConfig,
    market: Option<MarketContext>,
    factors: &FactorInput,
) -> RiskReport {
    let mut by_under: BTreeMap<String, Vec<&Position>> = BTreeMap::new();
    for p in &file.positions {
        by_under.entry(p.symbol.to_ascii_uppercase()).or_default().push(p);
    }

    let mut exposures = Vec::new();
    let mut flags = Vec::new();
    let mut freshness: Option<Freshness> = None;
    let mut long_premium = 0.0;
    let (mut long_calls, mut long_puts) = (0.0, 0.0);
    // Per exposure: spot and repriceable positions, kept for book-level shocks.
    let mut books: Vec<(f64, Vec<Priced>)> = Vec::new();

    for (sym, positions) in &by_under {
        let d = data.get(sym);
        let chain = d.and_then(|d| d.chain);
        let gex = d.and_then(|d| d.gex);
        let spot = chain.map(|c| c.spot).or(d.and_then(|d| d.quote));
        if let Some(c) = chain {
            if freshness.is_none_or(|f| c.freshness.quotes_as_of < f.quotes_as_of) {
                freshness = Some(c.freshness);
            }
        }

        let mut priced = Vec::new();
        for p in positions {
            let mult = p.multiplier();
            match p.sec_type {
                SecType::Stock => priced.push(Priced { pos: p, mult, option: None, mark: spot }),
                SecType::Option => {
                    let c = find_contract(chain, p);
                    let (Some(right), Some(k), Some(exp)) = (p.right, p.strike, p.expiry) else {
                        flags.push(flag(Severity::Warn, p, "option position is missing right/strike/expiry".into()));
                        continue;
                    };
                    let Some(c) = c.filter(|c| c.iv > 0.0) else {
                        flags.push(flag(
                            Severity::Warn,
                            p,
                            "no market data for this contract; excluded from Greeks".into(),
                        ));
                        continue;
                    };
                    let t = bs::years_to_expiry(exp, now);
                    priced.push(Priced { pos: p, mult, option: Some((right, k, t, c.iv)), mark: Some(c.mid()) });
                }
            }
        }

        let mut e = Exposure {
            underlying: sym.clone(),
            spot,
            positions: positions.len(),
            market_value: 0.0,
            unrealized: 0.0,
            delta_shares: 0.0,
            delta_dollars: 0.0,
            gamma_shares: 0.0,
            theta_day: 0.0,
            vega: 0.0,
            beta: None,
            r2: None,
            daily_vol: None,
            daily_sigma: None,
            risk_share: None,
            benchmark_equivalent: None,
            shocks: Vec::new(),
            next_earnings: d.and_then(|d| d.earnings.clone()),
            scenarios: Vec::new(),
            ladder: Vec::new(),
        };
        if let Some(s) = spot {
            for pp in &priced {
                let q = pp.pos.qty * pp.mult;
                if let Some(m) = pp.mark {
                    if let (Some((right, ..)), true) = (pp.option, q > 0.0) {
                        long_premium += m * q;
                        match right {
                            Right::Call => long_calls += m * q,
                            Right::Put => long_puts += m * q,
                        }
                    }
                    e.market_value += m * q;
                    e.unrealized += (m - pp.pos.avg_cost) * q;
                }
                match pp.option {
                    Some((right, k, t, iv)) => {
                        let g = bs::greeks(right, s, k, t, r, 0.0, iv);
                        e.delta_shares += g.delta * q;
                        e.gamma_shares += g.gamma * q;
                        e.theta_day += g.theta * q;
                        e.vega += g.vega * q;
                    }
                    None => e.delta_shares += q,
                }
            }
            e.delta_dollars = e.delta_shares * s;

            let pnl_at = |x: f64| -> f64 {
                priced.iter().map(|pp| (pp.value(x, r) - pp.value(s, r)) * pp.pos.qty * pp.mult).sum()
            };
            let scen =
                |label: String, x: f64| Scenario { label, price: x, move_pct: (x / s - 1.0) * 100.0, pnl: pnl_at(x) };
            if let Some(g) = gex {
                for (label, lvl) in [("put wall", g.put_wall), ("zero gamma", g.zero_gamma), ("call wall", g.call_wall)]
                {
                    if let Some(x) = lvl {
                        e.scenarios.push(scen(label.into(), x));
                    }
                }
            }
            if let Some(iv) = chain.and_then(|c| c.iv30) {
                let m = s * iv * (7.0f64 / 365.0).sqrt();
                e.scenarios.push(scen("-1σ week".into(), s - m));
                e.scenarios.push(scen("+1σ week".into(), s + m));
            }
            e.scenarios.sort_by(|a, b| a.price.total_cmp(&b.price));
            e.ladder = LADDER.iter().map(|pct| scen(format!("{pct:+.0}%"), s * (1.0 + pct / 100.0))).collect();
        }

        flags.extend(position_flags(positions, spot, now));
        flags.extend(earnings_flags(positions, d.and_then(|d| d.earnings.as_ref()), now));
        books.push((spot.unwrap_or(0.0), if spot.is_some() { priced } else { Vec::new() }));
        exposures.push(e);
    }

    if freshness.is_some_and(|f| f.quotes_stale) {
        flags.push(Flag {
            severity: Severity::Info,
            position: "all".into(),
            message: "option quotes are from the last close; marks and Greeks move at the open".into(),
        });
    }
    let totals = Totals {
        market_value: exposures.iter().map(|e| e.market_value).sum(),
        unrealized: exposures.iter().map(|e| e.unrealized).sum(),
        delta_dollars: exposures.iter().map(|e| e.delta_dollars).sum(),
        theta_day: exposures.iter().map(|e| e.theta_day).sum(),
        vega: exposures.iter().map(|e| e.vega).sum(),
    };
    let budget = match (file.net_liquidation, fx) {
        (Some(nlv), Some(fx)) if fx > 0.0 => {
            let input = sizing::BookInput {
                equity: nlv / fx,
                cash: file.cash.unwrap_or(0.0) / fx,
                excess_liquidity: file.excess_liquidity.map(|x| x / fx),
                underlyings: exposures.iter().map(|e| e.underlying.clone()).collect(),
                long_premium,
                capital_at_work: exposures.iter().map(|e| e.market_value.max(0.0)).sum(),
            };
            let b = sizing::budget(input, file.currency.clone().unwrap_or_else(|| "USD".into()), fx, sizing_cfg);
            flags.extend(budget_flags(&b));
            Some(b)
        }
        _ => None,
    };
    let histories: Vec<Option<&[Bar]>> =
        exposures.iter().map(|e| data.get(&e.underlying).and_then(|d| d.history)).collect();
    let book_risk = book_risk(&mut exposures, &books, &histories, factors, r, budget.as_ref().map(|b| b.equity));
    let bench_eq: f64 = exposures.iter().map(|e| e.benchmark_equivalent.unwrap_or(e.delta_dollars)).sum();
    let directional = Directional {
        net_delta_dollars: exposures.iter().map(|e| e.delta_dollars).sum(),
        benchmark: book_risk.as_ref().map(|b| b.benchmark.clone()),
        benchmark_equivalent: bench_eq,
        benchmark_equivalent_pct: budget.as_ref().map(|b| bench_eq / b.equity.max(1.0) * 100.0),
        pnl_per_benchmark_pct: bench_eq / 100.0,
        long_call_premium: long_calls,
        long_put_premium: long_puts,
        call_share: (long_calls + long_puts > 0.0).then(|| long_calls / (long_calls + long_puts)),
    };
    if let Some(br) = &book_risk {
        flags.extend(concentration_flags(&exposures, br));
    }
    flags.extend(directional_flags(&directional, market.as_ref()));
    flags.sort_by_key(|f| f.severity);

    RiskReport {
        as_of: file.as_of.clone(),
        account: file.account.clone(),
        net_liquidation: file.net_liquidation,
        currency: file.currency.clone(),
        freshness,
        exposures,
        totals,
        budget,
        directional,
        book_risk,
        market,
        flags,
    }
}

fn reprice(priced: &[Priced], s: f64, x: f64, r: f64) -> f64 {
    priced.iter().map(|pp| (pp.value(x, r) - pp.value(s, r)) * pp.pos.qty * pp.mult).sum()
}

/// Covariance-based daily risk; fills per-exposure beta, R², volatility and risk share.
fn book_risk(
    exposures: &mut [Exposure],
    books: &[(f64, Vec<Priced>)],
    histories: &[Option<&[Bar]>],
    f: &FactorInput,
    r: f64,
    equity: Option<f64>,
) -> Option<BookRisk> {
    const MIN_OBS: usize = 60;
    let idx: Vec<usize> =
        (0..exposures.len()).filter(|&i| histories[i].is_some() && exposures[i].spot.is_some()).collect();
    if idx.is_empty() || f.benchmarks.is_empty() {
        return None;
    }
    let mut series: Vec<_> = idx.iter().map(|&i| factor::log_returns(histories[i].unwrap())).collect();
    series.extend(f.benchmarks.iter().map(|(_, b)| factor::log_returns(b)));
    let rets = factor::align(&series, f.lookback);
    let obs = rets.first().map_or(0, |r| r.len());
    if obs < MIN_OBS {
        return None;
    }
    let k = idx.len();
    let (names, benches) = rets.split_at(k);
    let d: Vec<f64> = idx.iter().map(|&i| exposures[i].delta_dollars).collect();
    let abs_total: f64 = d.iter().map(|x| x.abs()).sum::<f64>().max(1e-9);

    let fits: Vec<Vec<factor::Fit>> =
        names.iter().map(|n| benches.iter().map(|b| factor::fit(n, b)).collect()).collect();
    let weighted: Vec<BenchmarkFit> = f
        .benchmarks
        .iter()
        .enumerate()
        .map(|(j, (name, _))| BenchmarkFit {
            benchmark: name.clone(),
            weighted_r2: (0..k).map(|i| d[i].abs() * fits[i][j].r2).sum::<f64>() / abs_total,
        })
        .collect();
    let chosen = f
        .force
        .as_ref()
        .and_then(|name| f.benchmarks.iter().position(|(b, _)| b.eq_ignore_ascii_case(name)))
        .unwrap_or_else(|| {
            (0..weighted.len()).max_by(|&a, &b| weighted[a].weighted_r2.total_cmp(&weighted[b].weighted_r2)).unwrap()
        });

    let cov = factor::covariance_matrix(names);
    let total_var = factor::quadratic(&d, &cov).max(0.0);
    let daily_sigma = total_var.sqrt();
    let bench_vol = factor::cov(&benches[chosen], &benches[chosen]).sqrt();
    let bench_eq: f64 = (0..k).map(|i| d[i] * fits[i][chosen].beta).sum();
    let market_sigma = bench_eq.abs() * bench_vol;
    let standalone: f64 = (0..k).map(|i| d[i].abs() * cov[i][i].sqrt()).sum();

    for (n, &i) in idx.iter().enumerate() {
        let e = &mut exposures[i];
        let fit = fits[n][chosen];
        let vol = cov[n][n].sqrt();
        let row: f64 = cov[n].iter().zip(&d).map(|(c, dj)| c * dj).sum();
        e.beta = Some(fit.beta);
        e.r2 = Some(fit.r2);
        e.daily_vol = Some(vol);
        e.daily_sigma = Some(d[n].abs() * vol);
        e.risk_share = (total_var > 0.0).then(|| d[n] * row / total_var);
        e.benchmark_equivalent = Some(e.delta_dollars * fit.beta);
        let (s, priced) = (&books[i].0, &books[i].1);
        e.shocks = [-2.0, 2.0]
            .iter()
            .map(|z| {
                let x = s * (z * vol).exp();
                Scenario {
                    label: format!("{z:+.0}σ day"),
                    price: x,
                    move_pct: (x / s - 1.0) * 100.0,
                    pnl: reprice(priced, *s, x, r),
                }
            })
            .collect();
    }

    let (bench_name, bench_bars) = &f.benchmarks[chosen];
    let bench_px = bench_bars.last().map_or(0.0, |b| b.c);
    let market_shocks = [-5.0, -3.0, -1.0, 1.0, 3.0, 5.0]
        .iter()
        .map(|m| {
            let pnl = (0..exposures.len())
                .filter(|&i| exposures[i].spot.is_some())
                .map(|i| {
                    let s = books[i].0;
                    let beta = exposures[i].beta.unwrap_or(1.0);
                    reprice(&books[i].1, s, s * (1.0 + beta * m / 100.0), r)
                })
                .sum();
            Scenario { label: format!("{bench_name} {m:+.0}%"), price: bench_px * (1.0 + m / 100.0), move_pct: *m, pnl }
        })
        .collect();

    Some(BookRisk {
        benchmark: bench_name.clone(),
        fits: weighted,
        observations: obs,
        daily_sigma,
        daily_sigma_pct: equity.map(|e| daily_sigma / e.max(1.0) * 100.0),
        market_sigma,
        specific_sigma: (total_var - market_sigma * market_sigma).max(0.0).sqrt(),
        market_share: if total_var > 0.0 { (market_sigma * market_sigma / total_var).min(1.0) } else { 0.0 },
        standalone_sigma: standalone,
        diversification: if standalone > 0.0 { 1.0 - daily_sigma / standalone } else { 0.0 },
        market_shocks,
    })
}

/// Flag when one underlying drives most of the book's daily risk.
fn concentration_flags(exposures: &[Exposure], br: &BookRisk) -> Vec<Flag> {
    let with_share: Vec<&Exposure> = exposures.iter().filter(|e| e.risk_share.is_some()).collect();
    if with_share.len() < 2 {
        return Vec::new();
    }
    with_share
        .iter()
        .filter(|e| e.risk_share.unwrap_or(0.0) >= 0.5)
        .map(|e| Flag {
            severity: Severity::Info,
            position: e.underlying.clone(),
            message: format!(
                "drives {:.0}% of the book's daily risk (book 1σ ±${:.0}, {:.0}% of it company-specific)",
                e.risk_share.unwrap_or(0.0) * 100.0,
                br.daily_sigma,
                (1.0 - br.market_share) * 100.0
            ),
        })
        .collect()
}

/// Flag a one-sided book when market conditions lean against it.
fn directional_flags(d: &Directional, m: Option<&MarketContext>) -> Vec<Flag> {
    let mut out = Vec::new();
    let book = |severity, message| Flag { severity, position: "book".into(), message };
    if let (Some(share), Some(m)) = (d.call_share, m) {
        let long_side = if share >= 0.8 {
            Some(("calls", share, &m.risk_off))
        } else if share <= 0.2 {
            Some(("puts", 1.0 - share, &m.risk_on))
        } else {
            None
        };
        if let Some((side, pct, against)) = long_side {
            if !against.is_empty() {
                out.push(book(
                    if against.len() >= 2 { Severity::High } else { Severity::Warn },
                    format!("{:.0}% of long premium is {side} while {}", pct * 100.0, against.join("; ")),
                ));
            }
        }
    }
    out
}

fn budget_flags(b: &Budget) -> Vec<Flag> {
    let mut out = Vec::new();
    let book = |severity, message| Flag { severity, position: "book".into(), message };
    if b.floor_headroom < 0.0 {
        out.push(book(
            Severity::High,
            format!(
                "excess liquidity {:.0}% of equity is below the {:.0}% floor: no new positions, consider reducing",
                b.excess_liquidity_pct, b.rules.liquidity_floor_pct
            ),
        ));
    } else if b.buffer_headroom < 0.0 {
        out.push(book(
            Severity::Warn,
            format!("cash {:.0}% of equity is below the {:.0}% buffer", b.cash_pct, b.rules.cash_buffer_pct),
        ));
    }
    if let (Some(h), Some(p)) = (b.premium_headroom, b.rules.premium_budget_pct) {
        if h < 0.0 {
            out.push(book(
                Severity::Warn,
                format!("long premium {:.0}% of equity is over the {p:.0}% budget", b.long_premium_pct),
            ));
        }
    }
    if b.slots_used > b.rules.slots {
        out.push(book(
            Severity::Warn,
            format!("{} underlyings open vs {} slots ({} tier)", b.slots_used, b.rules.slots, b.rules.tier),
        ));
    }
    out
}

/// Options that expire after an earnings report carry gap and IV-crush risk through it.
fn earnings_flags(positions: &[&Position], e: Option<&crate::provider::Earnings>, now: DateTime<Utc>) -> Vec<Flag> {
    let Some(e) = e else { return Vec::new() };
    let days = bs::dte(e.date, now);
    if days < 0 {
        return Vec::new();
    }
    positions
        .iter()
        .filter(|p| p.sec_type == SecType::Option && p.expiry.is_some_and(|x| x >= e.date))
        .map(|p| {
            let severity = if days <= 7 {
                Severity::High
            } else if days <= 30 {
                Severity::Warn
            } else {
                Severity::Info
            };
            flag(
                severity,
                p,
                format!(
                    "earnings {}{} in {days}d, before expiry: gap and IV-crush risk",
                    e.date.format("%m-%d"),
                    if e.estimated { " (est.)" } else { "" }
                ),
            )
        })
        .collect()
}

fn flag(severity: Severity, p: &Position, message: String) -> Flag {
    Flag { severity, position: p.label(), message }
}

fn position_flags(positions: &[&Position], spot: Option<f64>, now: DateTime<Utc>) -> Vec<Flag> {
    let mut out = Vec::new();
    for p in positions {
        let (SecType::Option, Some(right), Some(k), Some(exp)) = (p.sec_type, p.right, p.strike, p.expiry) else {
            continue;
        };
        let dte = bs::dte(exp, now);
        let moneyness = spot.map(|s| {
            let otm = match right {
                Right::Call => (k / s - 1.0) * 100.0,
                Right::Put => (1.0 - k / s) * 100.0,
            };
            if otm > 0.0 {
                format!("OTM by {otm:.1}%")
            } else {
                format!("ITM by {:.1}%", -otm)
            }
        });
        let m = moneyness.clone().unwrap_or_default();
        if dte <= 1 {
            let when = if dte <= 0 { "expires today" } else { "expires tomorrow" };
            out.push(flag(Severity::High, p, format!("{when}, {m}")));
        } else if dte <= 7 {
            out.push(flag(Severity::Warn, p, format!("expires in {dte}d, {m}")));
        }

        if p.qty < 0.0 {
            // Covered if a long of the same right with at least the same size and expiry exists.
            let covered_by_options: f64 = positions
                .iter()
                .filter(|o| o.qty > 0.0 && o.right == Some(right) && o.expiry.is_some_and(|e| e >= exp))
                .map(|o| o.qty)
                .sum();
            let shares: f64 = positions.iter().filter(|o| o.sec_type == SecType::Stock).map(|o| o.qty).sum();
            let covered = covered_by_options >= -p.qty || (right == Right::Call && shares >= -p.qty * p.multiplier());
            if !covered {
                let notional = k * -p.qty * p.multiplier();
                let msg = match right {
                    Right::Call => "naked short call: undefined risk".to_string(),
                    Right::Put => format!("naked short put: assignment notional ${notional:.0}"),
                };
                out.push(flag(Severity::High, p, msg));
            }
            if dte <= 21 && dte > 7 {
                out.push(flag(Severity::Warn, p, format!("short option inside 21 DTE ({dte}d): gamma risk rising")));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::legs::tests::synthetic_chain;

    #[test]
    fn long_call_scenarios() {
        let now = Utc::now();
        let chain = synthetic_chain(now);
        let exp = chain.expirations()[1];
        let file = PortfolioFile {
            positions: vec![Position {
                symbol: "TEST".into(),
                sec_type: SecType::Option,
                right: Some(Right::Call),
                strike: Some(100.0),
                expiry: Some(exp),
                qty: 2.0,
                avg_cost: 1.0,
                multiplier: None,
            }],
            ..Default::default()
        };
        let data = BTreeMap::from([(
            "TEST".to_string(),
            UnderlyingData { chain: Some(&chain), gex: None, quote: None, history: None, earnings: None },
        )]);
        let factors = FactorInput { benchmarks: vec![], force: None, lookback: 252 };
        let r = compute(&file, &data, now, 0.04, None, &SizingConfig::default(), None, &factors);
        assert!(r.book_risk.is_none());
        assert_eq!(r.directional.call_share, Some(1.0));
        let e = &r.exposures[0];
        assert!(e.delta_shares > 50.0 && e.delta_shares < 150.0, "{}", e.delta_shares);
        assert!(e.theta_day < 0.0);
        let at = |label: &str| e.ladder.iter().find(|s| s.label == label).unwrap().pnl;
        assert_eq!(at("+0%"), 0.0);
        assert!(at("+10%") > at("+5%") && at("+5%") > 0.0);
        assert!(at("-10%") < 0.0 && at("-10%") > -e.market_value - 1e-6);
        assert!(r.flags.iter().all(|f| f.severity != Severity::High));
    }
}

#[cfg(test)]
mod directional_tests {
    use super::*;

    fn market(off: usize, on: usize) -> MarketContext {
        MarketContext {
            spy: 700.0,
            spy_change_pct: 0.0,
            spy_trend: "mixed".into(),
            spy_rsi: None,
            spy_above_50d: None,
            spy_above_200d: None,
            spy_zero_gamma: None,
            spy_call_wall: None,
            spy_put_wall: None,
            vix: None,
            vix_change_pct: None,
            risk_off: (0..off).map(|i| format!("off{i}")).collect(),
            risk_on: (0..on).map(|i| format!("on{i}")).collect(),
        }
    }

    #[test]
    fn call_heavy_book_against_risk_off() {
        let d = Directional { call_share: Some(0.95), ..Default::default() };
        assert_eq!(directional_flags(&d, Some(&market(2, 0)))[0].severity, Severity::High);
        assert_eq!(directional_flags(&d, Some(&market(1, 3)))[0].severity, Severity::Warn);
        assert!(directional_flags(&d, Some(&market(0, 3))).is_empty());
        // Balanced books are not flagged for direction.
        let d = Directional { call_share: Some(0.5), ..Default::default() };
        assert!(directional_flags(&d, Some(&market(3, 0))).is_empty());
    }
}
