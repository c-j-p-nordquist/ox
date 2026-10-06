//! Gamma exposure (GEX) by strike, key levels and the zero-gamma flip.
//!
//! Convention (the common "naive" dealer model): dealers are long calls and short puts
//! relative to customers, so call gamma is counted positive and put gamma negative.
//! Values are dollars of delta change per 1% move in the underlying:
//! `gamma * OI * 100 * S^2 * 0.01`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::bs;
use crate::model::{ChainSnapshot, OptionContract, Right};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GexStrike {
    pub strike: f64,
    pub call_gex: f64,
    pub put_gex: f64,
    pub net_gex: f64,
    pub call_oi: f64,
    pub put_oi: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProfilePoint {
    pub spot: f64,
    pub net_gex: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GexReport {
    pub underlying: String,
    pub spot: f64,
    pub as_of: i64,
    #[serde(flatten)]
    pub freshness: super::market::Freshness,
    /// Only expirations with DTE <= this were included (`None` = all).
    pub max_dte: Option<u32>,
    pub expirations: usize,
    pub total_gex: f64,
    pub call_gex: f64,
    pub put_gex: f64,
    /// Spot level where net dealer gamma changes sign (nearest to current spot).
    pub zero_gamma: Option<f64>,
    /// Strike at or above spot with the largest call gamma (resistance).
    pub call_wall: Option<f64>,
    /// Strike at or below spot with the largest put gamma (support).
    pub put_wall: Option<f64>,
    /// Strike with the largest absolute net gamma.
    pub max_abs_strike: Option<f64>,
    pub strikes: Vec<GexStrike>,
    /// Net GEX recomputed across hypothetical spot prices (sticky-strike IV).
    pub profile: Vec<ProfilePoint>,
}

struct Leg {
    k: f64,
    t: f64,
    iv: f64,
    /// +OI for calls, -OI for puts.
    signed_oi: f64,
}

const PROFILE_POINTS: usize = 81;
const PROFILE_RANGE: f64 = 0.15;

pub fn compute(chain: &ChainSnapshot, now: DateTime<Utc>, r: f64, max_dte: Option<u32>) -> GexReport {
    let s = chain.spot;
    let mut legs = Vec::with_capacity(chain.contracts.len());
    let mut by_strike: BTreeMap<u64, GexStrike> = BTreeMap::new();
    let mut expirations = std::collections::HashSet::new();

    for c in &chain.contracts {
        let Some((t, gex)) = contract_gex(c, s, now, r, max_dte) else { continue };
        expirations.insert(c.expiry);
        let sign = if c.right == Right::Call { 1.0 } else { -1.0 };

        let e = by_strike.entry((c.strike * 1000.0).round() as u64).or_insert(GexStrike {
            strike: c.strike,
            call_gex: 0.0,
            put_gex: 0.0,
            net_gex: 0.0,
            call_oi: 0.0,
            put_oi: 0.0,
        });
        match c.right {
            Right::Call => {
                e.call_gex += gex;
                e.call_oi += c.open_interest;
            }
            Right::Put => {
                e.put_gex += gex;
                e.put_oi += c.open_interest;
            }
        }
        e.net_gex += gex;
        legs.push(Leg { k: c.strike, t, iv: c.iv, signed_oi: sign * c.open_interest });
    }

    let strikes: Vec<GexStrike> = by_strike.into_values().collect();
    let call_gex: f64 = strikes.iter().map(|x| x.call_gex).sum();
    let put_gex: f64 = strikes.iter().map(|x| x.put_gex).sum();
    let argmax = |f: &dyn Fn(&GexStrike) -> f64| {
        strikes.iter().filter(|x| f(x) > 0.0).max_by(|a, b| f(a).total_cmp(&f(b))).map(|x| x.strike)
    };
    let call_wall = argmax(&|x| if x.strike >= s { x.call_gex } else { 0.0 });
    let put_wall = argmax(&|x| if x.strike <= s { -x.put_gex } else { 0.0 });
    let max_abs_strike = argmax(&|x| x.net_gex.abs());

    let profile = profile(&legs, s, r);
    let zero_gamma = zero_crossing(&profile, s);

    GexReport {
        underlying: chain.underlying.clone(),
        spot: s,
        as_of: chain.as_of,
        freshness: chain.freshness,
        max_dte,
        expirations: expirations.len(),
        total_gex: call_gex + put_gex,
        call_gex,
        put_gex,
        zero_gamma,
        call_wall,
        put_wall,
        max_abs_strike,
        strikes,
        profile,
    }
}

/// One contract's dealer GEX at spot `s` (and its time to expiry), or `None` when it is
/// excluded: no open interest, implausible IV, expired, or beyond `max_dte`.
fn contract_gex(c: &OptionContract, s: f64, now: DateTime<Utc>, r: f64, max_dte: Option<u32>) -> Option<(f64, f64)> {
    if c.open_interest <= 0.0 || !(0.005..5.0).contains(&c.iv) {
        return None;
    }
    let dte = bs::dte(c.expiry, now);
    if dte < 0 || max_dte.is_some_and(|m| dte > m as i64) {
        return None;
    }
    let t = bs::years_to_expiry(c.expiry, now);
    let sign = if c.right == Right::Call { 1.0 } else { -1.0 };
    Some((t, sign * bs::gamma(s, c.strike, t, r, 0.0, c.iv) * c.open_interest * 100.0 * s * s * 0.01))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridExpiry {
    pub expiry: NaiveDate,
    pub dte: i64,
    pub net_gex: f64,
    pub call_oi: f64,
    pub put_oi: f64,
}

/// GEX and open interest by strike and expiration (the heatmap). Net GEX is `call + put`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GexGrid {
    /// Strikes within the requested range of spot, ascending.
    pub strikes: Vec<f64>,
    pub expiries: Vec<GridExpiry>,
    /// `[expiry][strike]`, in the orders above.
    pub call_gex: Vec<Vec<f64>>,
    pub put_gex: Vec<Vec<f64>>,
    pub call_oi: Vec<Vec<f64>>,
    pub put_oi: Vec<Vec<f64>>,
}

/// Strike x expiration grid for strikes within `range` (a fraction, e.g. 0.2) of spot.
pub fn grid(chain: &ChainSnapshot, now: DateTime<Utc>, r: f64, max_dte: Option<u32>, range: f64) -> GexGrid {
    let s = chain.spot;
    let in_range = |k: f64| (k - s).abs() <= s * range;
    let key = |k: f64| (k * 1000.0).round() as u64;
    let mut strikes: Vec<f64> = chain.contracts.iter().map(|c| c.strike).filter(|k| in_range(*k)).collect();
    strikes.sort_by(f64::total_cmp);
    strikes.dedup_by_key(|k| key(*k));
    let col: BTreeMap<u64, usize> = strikes.iter().enumerate().map(|(i, k)| (key(*k), i)).collect();

    let mut rows: BTreeMap<NaiveDate, [Vec<f64>; 4]> = BTreeMap::new();
    let mut totals: BTreeMap<NaiveDate, (f64, f64, f64)> = BTreeMap::new();
    for c in &chain.contracts {
        let Some((_, gex)) = contract_gex(c, s, now, r, max_dte) else { continue };
        let tot = totals.entry(c.expiry).or_default();
        tot.0 += gex;
        let call = c.right == Right::Call;
        if call {
            tot.1 += c.open_interest;
        } else {
            tot.2 += c.open_interest;
        }
        let Some(&i) = col.get(&key(c.strike)) else { continue };
        let row = rows.entry(c.expiry).or_insert_with(|| std::array::from_fn(|_| vec![0.0; strikes.len()]));
        let (g, oi) = if call { (0, 2) } else { (1, 3) };
        row[g][i] += gex;
        row[oi][i] += c.open_interest;
    }

    let mut out = GexGrid {
        strikes,
        expiries: Vec::new(),
        call_gex: Vec::new(),
        put_gex: Vec::new(),
        call_oi: Vec::new(),
        put_oi: Vec::new(),
    };
    for (expiry, (net_gex, call_oi, put_oi)) in totals {
        let [cg, pg, co, po] =
            rows.remove(&expiry).unwrap_or_else(|| std::array::from_fn(|_| vec![0.0; out.strikes.len()]));
        out.expiries.push(GridExpiry { expiry, dte: bs::dte(expiry, now), net_gex, call_oi, put_oi });
        out.call_gex.push(cg);
        out.put_gex.push(pg);
        out.call_oi.push(co);
        out.put_oi.push(po);
    }
    out
}

/// The heatmap view: the strike report, the strike x expiration grid, and a plain read of both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GexMap {
    pub gex: GexReport,
    pub grid: GexGrid,
    /// Descriptive lines, each traceable to a number above. Nothing here is backtested.
    pub read: Vec<String>,
}

/// Where spot sits against the levels, where gamma is concentrated, and the biggest nodes.
pub fn read(g: &GexReport, grid: &GexGrid) -> Vec<String> {
    let s = g.spot;
    let rel = |x: f64| format!("{:+.1}%", (x / s - 1.0) * 100.0);
    let mut out = Vec::new();

    out.push(match g.zero_gamma {
        Some(z) => format!(
            "spot {s:.2} is {} zero gamma {z:.2} ({}): net dealer gamma is {} here under the standard \
             dealer-sign model — usually read as hedging that {} moves (not backtested on this stock)",
            if s >= z { "above" } else { "below" },
            rel(z),
            if s >= z { "positive" } else { "negative" },
            if s >= z { "dampens" } else { "amplifies" },
        ),
        None => format!("no zero-gamma crossing within ±{:.0}% of spot", PROFILE_RANGE * 100.0),
    });
    match (g.call_wall, g.put_wall) {
        (None, None) => {}
        (cw, pw) => out.push(format!(
            "call wall {} · put wall {}",
            cw.map_or("–".into(), |x| format!("{x} ({})", rel(x))),
            pw.map_or("–".into(), |x| format!("{x} ({})", rel(x)))
        )),
    }

    let gross: Vec<f64> = (0..grid.expiries.len())
        .map(|e| grid.call_gex[e].iter().chain(&grid.put_gex[e]).map(|x| x.abs()).sum())
        .collect();
    let total: f64 = gross.iter().sum();
    if total > 0.0 {
        let week: f64 = grid.expiries.iter().zip(&gross).filter(|(x, _)| x.dte <= 7).map(|(_, g)| g).sum();
        if let Some((e, top)) = grid.expiries.iter().zip(&gross).max_by(|a, b| a.1.total_cmp(b.1)) {
            out.push(format!(
                "{:.0}% of gross gamma near spot expires within 7 days; the largest expiration is {} \
                 ({} DTE, {:.0}%) — the map can change materially after it rolls off",
                week / total * 100.0,
                e.expiry.format("%b %-d"),
                e.dte,
                top / total * 100.0
            ));
        }

        let mut nodes: Vec<(usize, usize, f64)> = (0..grid.expiries.len())
            .flat_map(|e| (0..grid.strikes.len()).map(move |k| (e, k)))
            .map(|(e, k)| (e, k, grid.call_gex[e][k] + grid.put_gex[e][k]))
            .collect();
        nodes.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()));
        let list: Vec<String> = nodes
            .iter()
            .take(3)
            .filter(|n| n.2 != 0.0)
            .map(|&(e, k, v)| {
                format!(
                    "{} {} ({}{:.0}% of gross)",
                    grid.strikes[k],
                    grid.expiries[e].expiry.format("%b %-d"),
                    if v > 0.0 { "+" } else { "−" },
                    v.abs() / total * 100.0
                )
            })
            .collect();
        if !list.is_empty() {
            out.push(format!("largest nodes: {}", list.join(", ")));
        }
    }
    out.push(
        "open interest is as of the start of the session (it updates overnight); gamma uses current spot and IV".into(),
    );
    out
}

fn profile(legs: &[Leg], s: f64, r: f64) -> Vec<ProfilePoint> {
    (0..PROFILE_POINTS)
        .map(|i| {
            let x = s * (1.0 - PROFILE_RANGE + 2.0 * PROFILE_RANGE * i as f64 / (PROFILE_POINTS - 1) as f64);
            let net: f64 = legs.iter().map(|l| l.signed_oi * bs::gamma(x, l.k, l.t, r, 0.0, l.iv)).sum::<f64>()
                * 100.0
                * x
                * x
                * 0.01;
            ProfilePoint { spot: x, net_gex: net }
        })
        .collect()
}

/// Linear-interpolated sign change of the profile closest to `spot`.
fn zero_crossing(profile: &[ProfilePoint], spot: f64) -> Option<f64> {
    profile
        .windows(2)
        .filter(|w| w[0].net_gex.signum() != w[1].net_gex.signum() && w[0].net_gex != 0.0)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            a.spot + (b.spot - a.spot) * (-a.net_gex / (b.net_gex - a.net_gex))
        })
        .min_by(|a, b| (a - spot).abs().total_cmp(&(b - spot).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn contract(expiry: NaiveDate, right: Right, strike: f64, oi: f64) -> OptionContract {
        OptionContract {
            symbol: String::new(),
            root: "T".into(),
            expiry,
            right,
            strike,
            bid: 0.0,
            ask: 0.0,
            last: 0.0,
            iv: 0.3,
            delta: 0.0,
            gamma: 0.0,
            theta: 0.0,
            vega: 0.0,
            open_interest: oi,
            volume: 0.0,
            prev_close: 0.0,
        }
    }

    #[test]
    fn grid_matches_strike_totals() {
        let now = Utc.with_ymd_and_hms(2026, 10, 5, 15, 0, 0).unwrap();
        let (e1, e2) = (NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        let chain = ChainSnapshot {
            underlying: "T".into(),
            spot: 100.0,
            prev_close: 100.0,
            iv30: None,
            as_of: now.timestamp(),
            source: "test".into(),
            delayed: true,
            freshness: crate::analytics::market::Freshness::assess(now.timestamp(), now),
            regular_close: None,
            contracts: vec![
                contract(e1, Right::Call, 105.0, 1000.0),
                contract(e1, Right::Put, 95.0, 800.0),
                contract(e2, Right::Call, 105.0, 500.0),
                contract(e2, Right::Put, 200.0, 500.0), // outside the range: totals only
            ],
        };
        let g = grid(&chain, now, 0.04, None, 0.2);
        assert_eq!(g.strikes, vec![95.0, 105.0]);
        assert_eq!(g.expiries.iter().map(|e| e.expiry).collect::<Vec<_>>(), vec![e1, e2]);
        assert_eq!(g.call_oi[0], vec![0.0, 1000.0]);
        assert_eq!(g.put_oi[0], vec![800.0, 0.0]);
        assert_eq!(g.expiries[1].put_oi, 500.0);
        assert!(g.call_gex[0][1] > 0.0 && g.put_gex[0][0] < 0.0);

        // Summed over expirations, each strike matches the strike report.
        let report = compute(&chain, now, 0.04, None);
        for (k, s) in g.strikes.iter().enumerate() {
            let row = report.strikes.iter().find(|x| x.strike == *s).unwrap();
            let net: f64 = (0..g.expiries.len()).map(|e| g.call_gex[e][k] + g.put_gex[e][k]).sum();
            assert!((net - row.net_gex).abs() < 1e-6 * row.net_gex.abs().max(1.0));
        }
        assert!(read(&report, &g).iter().any(|l| l.starts_with("largest nodes: ")));
    }

    #[test]
    fn crossing_interpolates() {
        let p = [
            ProfilePoint { spot: 90.0, net_gex: -10.0 },
            ProfilePoint { spot: 100.0, net_gex: 10.0 },
            ProfilePoint { spot: 110.0, net_gex: 20.0 },
        ];
        assert_eq!(zero_crossing(&p, 100.0), Some(95.0));
        assert_eq!(zero_crossing(&p[1..], 100.0), None);
    }
}
