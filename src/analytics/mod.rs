//! Derived views over a chain snapshot: chain tables, expected moves, unusual activity.

pub mod bs;
pub mod factor;
pub mod gex;
pub mod indicators;
pub mod legs;
pub mod levels;
pub mod market;
pub mod pick;
pub mod review;
pub mod risk;
pub mod sizing;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{ChainSnapshot, OptionContract, Right};
use market::Freshness;

/// Fill in Greeks for providers that only supply IV (or only prices).
pub fn enrich(chain: &mut ChainSnapshot, now: DateTime<Utc>, r: f64) {
    let s = chain.spot;
    for c in &mut chain.contracts {
        let t = bs::years_to_expiry(c.expiry, now);
        if c.iv <= 0.0 {
            if let Some(iv) = bs::implied_vol(c.right, c.mid(), s, c.strike, t, r, 0.0) {
                c.iv = iv;
            }
        }
        if c.delta == 0.0 && c.gamma == 0.0 && c.iv > 0.0 {
            let g = bs::greeks(c.right, s, c.strike, t, r, 0.0, c.iv);
            c.delta = g.delta;
            c.gamma = g.gamma;
            c.theta = g.theta;
            c.vega = g.vega;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryInfo {
    pub date: NaiveDate,
    pub dte: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainRow {
    pub strike: f64,
    pub call: Option<OptionContract>,
    pub put: Option<OptionContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainView {
    pub underlying: String,
    pub spot: f64,
    pub as_of: i64,
    pub delayed: bool,
    #[serde(flatten)]
    pub freshness: Freshness,
    pub source: String,
    pub expirations: Vec<ExpiryInfo>,
    pub expiry: NaiveDate,
    pub dte: i64,
    pub rows: Vec<ChainRow>,
}

/// Pick an expiry: exact date, else first with DTE >= `min_dte`, else the nearest.
pub fn pick_expiry(
    chain: &ChainSnapshot,
    now: DateTime<Utc>,
    date: Option<NaiveDate>,
    min_dte: Option<u32>,
) -> Option<NaiveDate> {
    let exps: Vec<NaiveDate> = chain.expirations().into_iter().filter(|e| bs::dte(*e, now) >= 0).collect();
    if let Some(d) = date {
        return exps.iter().copied().find(|e| *e >= d);
    }
    let min = min_dte.unwrap_or(0) as i64;
    exps.iter().copied().find(|e| bs::dte(*e, now) >= min).or_else(|| exps.last().copied())
}

pub fn chain_view(
    chain: &ChainSnapshot,
    now: DateTime<Utc>,
    expiry: NaiveDate,
    strikes_each_side: Option<usize>,
) -> ChainView {
    let mut rows: Vec<ChainRow> = Vec::new();
    let mut legs: Vec<&OptionContract> = chain.contracts.iter().filter(|c| c.expiry == expiry).collect();
    legs.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    for c in legs {
        if rows.last().is_none_or(|r| (r.strike - c.strike).abs() > 1e-9) {
            rows.push(ChainRow { strike: c.strike, call: None, put: None });
        }
        let row = rows.last_mut().unwrap();
        // Weeklies and monthlies can share a strike (SPX vs SPXW); keep the more liquid one.
        let slot = if c.right == Right::Call { &mut row.call } else { &mut row.put };
        if slot.as_ref().is_none_or(|x| x.open_interest + x.volume < c.open_interest + c.volume) {
            *slot = Some(c.clone());
        }
    }
    if let Some(n) = strikes_each_side {
        let atm = atm_index(&rows, chain.spot);
        let lo = atm.saturating_sub(n);
        let hi = (atm + n + 1).min(rows.len());
        rows = rows.drain(lo..hi).collect();
    }
    ChainView {
        underlying: chain.underlying.clone(),
        spot: chain.spot,
        as_of: chain.as_of,
        delayed: chain.delayed,
        freshness: chain.freshness,
        source: chain.source.clone(),
        expirations: chain
            .expirations()
            .into_iter()
            .map(|date| ExpiryInfo { date, dte: bs::dte(date, now) })
            .filter(|e| e.dte >= 0)
            .collect(),
        expiry,
        dte: bs::dte(expiry, now),
        rows,
    }
}

pub fn atm_index(rows: &[ChainRow], spot: f64) -> usize {
    rows.iter()
        .enumerate()
        .min_by(|a, b| (a.1.strike - spot).abs().total_cmp(&(b.1.strike - spot).abs()))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryStats {
    pub date: NaiveDate,
    pub dte: i64,
    pub atm_strike: f64,
    pub atm_iv: Option<f64>,
    /// ATM straddle mid price.
    pub straddle: Option<f64>,
    /// One-standard-deviation move from ATM IV: `S * iv * sqrt(T)`.
    pub expected_move: Option<f64>,
    pub call_volume: f64,
    pub put_volume: f64,
    pub call_oi: f64,
    pub put_oi: f64,
    pub put_call_oi: Option<f64>,
    pub max_pain: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub underlying: String,
    pub spot: f64,
    pub prev_close: f64,
    pub change_pct: f64,
    pub iv30: Option<f64>,
    pub as_of: i64,
    pub delayed: bool,
    #[serde(flatten)]
    pub freshness: Freshness,
    pub source: String,
    pub total_call_volume: f64,
    pub total_put_volume: f64,
    pub put_call_volume: Option<f64>,
    pub put_call_oi: Option<f64>,
    pub gex_total: f64,
    pub zero_gamma: Option<f64>,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub expirations: Vec<ExpiryStats>,
}

pub fn expiry_stats(chain: &ChainSnapshot, now: DateTime<Utc>, expiry: NaiveDate) -> ExpiryStats {
    let view = chain_view(chain, now, expiry, None);
    let s = chain.spot;
    let t = bs::years_to_expiry(expiry, now);
    let atm = view.rows.get(atm_index(&view.rows, s));
    let atm_iv = atm.and_then(|r| {
        let ivs: Vec<f64> = [&r.call, &r.put].into_iter().flatten().map(|c| c.iv).filter(|v| *v > 0.0).collect();
        (!ivs.is_empty()).then(|| ivs.iter().sum::<f64>() / ivs.len() as f64)
    });
    let straddle = atm
        .and_then(|r| Some((r.call.as_ref()?.mid(), r.put.as_ref()?.mid())))
        .filter(|(c, p)| *c > 0.0 && *p > 0.0)
        .map(|(c, p)| c + p);
    let sum = |right: Right, f: fn(&OptionContract) -> f64| -> f64 {
        view.rows
            .iter()
            .filter_map(|r| if right == Right::Call { r.call.as_ref() } else { r.put.as_ref() })
            .map(f)
            .sum()
    };
    let (call_oi, put_oi) = (sum(Right::Call, |c| c.open_interest), sum(Right::Put, |c| c.open_interest));
    ExpiryStats {
        date: expiry,
        dte: view.dte,
        atm_strike: atm.map(|r| r.strike).unwrap_or(s),
        atm_iv,
        straddle,
        expected_move: atm_iv.map(|iv| s * iv * t.sqrt()),
        call_volume: sum(Right::Call, |c| c.volume),
        put_volume: sum(Right::Put, |c| c.volume),
        call_oi,
        put_oi,
        put_call_oi: (call_oi > 0.0).then(|| put_oi / call_oi),
        max_pain: max_pain(&view.rows),
    }
}

/// Strike at which total intrinsic value owed to option holders is smallest.
fn max_pain(rows: &[ChainRow]) -> Option<f64> {
    rows.iter()
        .map(|settle| {
            let pain: f64 = rows
                .iter()
                .map(|r| {
                    let c = r.call.as_ref().map_or(0.0, |c| c.open_interest * (settle.strike - r.strike).max(0.0));
                    let p = r.put.as_ref().map_or(0.0, |p| p.open_interest * (r.strike - settle.strike).max(0.0));
                    c + p
                })
                .sum();
            (settle.strike, pain)
        })
        .filter(|(_, p)| *p > 0.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

pub fn overview(chain: &ChainSnapshot, gex: &gex::GexReport, now: DateTime<Utc>, max_expirations: usize) -> Overview {
    let (mut cv, mut pv, mut coi, mut poi) = (0.0, 0.0, 0.0, 0.0);
    for c in &chain.contracts {
        match c.right {
            Right::Call => {
                cv += c.volume;
                coi += c.open_interest;
            }
            Right::Put => {
                pv += c.volume;
                poi += c.open_interest;
            }
        }
    }
    let expirations = chain
        .expirations()
        .into_iter()
        .filter(|e| bs::dte(*e, now) >= 0)
        .take(max_expirations)
        .map(|e| expiry_stats(chain, now, e))
        .collect();
    Overview {
        underlying: chain.underlying.clone(),
        spot: chain.spot,
        prev_close: chain.prev_close,
        change_pct: if chain.prev_close > 0.0 { (chain.spot / chain.prev_close - 1.0) * 100.0 } else { 0.0 },
        iv30: chain.iv30,
        as_of: chain.as_of,
        delayed: chain.delayed,
        freshness: chain.freshness,
        source: chain.source.clone(),
        total_call_volume: cv,
        total_put_volume: pv,
        put_call_volume: (cv > 0.0).then(|| pv / cv),
        put_call_oi: (coi > 0.0).then(|| poi / coi),
        gex_total: gex.total_gex,
        zero_gamma: gex.zero_gamma,
        call_wall: gex.call_wall,
        put_wall: gex.put_wall,
        expirations,
    }
}
