//! Gamma exposure (GEX) by strike, key levels and the zero-gamma flip.
//!
//! Convention (the common "naive" dealer model): dealers are long calls and short puts
//! relative to customers, so call gamma is counted positive and put gamma negative.
//! Values are dollars of delta change per 1% move in the underlying:
//! `gamma * OI * 100 * S^2 * 0.01`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::bs;
use crate::model::{ChainSnapshot, Right};

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
        if c.open_interest <= 0.0 || !(0.005..5.0).contains(&c.iv) {
            continue;
        }
        let dte = bs::dte(c.expiry, now);
        if dte < 0 || max_dte.is_some_and(|m| dte > m as i64) {
            continue;
        }
        expirations.insert(c.expiry);
        let t = bs::years_to_expiry(c.expiry, now);
        let sign = if c.right == Right::Call { 1.0 } else { -1.0 };
        let g = bs::gamma(s, c.strike, t, r, 0.0, c.iv);
        let gex = sign * g * c.open_interest * 100.0 * s * s * 0.01;

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
