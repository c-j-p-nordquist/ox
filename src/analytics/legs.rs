//! Option legs shared by the contract ranking and the position review.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::model::Right;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leg {
    /// OCC symbol, e.g. `NVDA261016C00235000`.
    pub symbol: String,
    pub side: Side,
    pub right: Right,
    pub strike: f64,
    pub expiry: NaiveDate,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub iv: f64,
    pub delta: f64,
    pub open_interest: f64,
    pub volume: f64,
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::{DateTime, Duration, Utc};
    use chrono_tz::America::New_York;

    use crate::analytics::bs;
    use crate::analytics::market::{Freshness, Session};
    use crate::model::{ChainSnapshot, OptionContract, Right};

    /// Flat-vol synthetic chain priced with Black-Scholes.
    pub fn synthetic_chain(now: DateTime<Utc>) -> ChainSnapshot {
        let spot = 100.0;
        let today = now.with_timezone(&New_York).date_naive();
        let mut contracts = Vec::new();
        for days in [7, 21, 35] {
            let expiry = today + Duration::days(days);
            let t = bs::years_to_expiry(expiry, now);
            for k in (70..=130).step_by(5) {
                let k = k as f64;
                for right in [Right::Call, Right::Put] {
                    let g = bs::greeks(right, spot, k, t, 0.04, 0.0, 0.3);
                    contracts.push(OptionContract {
                        symbol: format!("TEST{}{}{:08}", expiry.format("%y%m%d"), right.letter(), (k * 1000.0) as u64),
                        root: "TEST".into(),
                        expiry,
                        right,
                        strike: k,
                        bid: (g.price - 0.02).max(0.01),
                        ask: g.price + 0.02,
                        last: g.price,
                        iv: 0.3,
                        delta: g.delta,
                        gamma: g.gamma,
                        theta: g.theta,
                        vega: g.vega,
                        open_interest: 1000.0,
                        volume: 100.0,
                        prev_close: g.price,
                    });
                }
            }
        }
        ChainSnapshot {
            underlying: "TEST".into(),
            spot,
            prev_close: spot,
            iv30: Some(0.3),
            as_of: now.timestamp(),
            source: "test".into(),
            delayed: false,
            freshness: Freshness {
                session: Session::Regular,
                quotes_stale: false,
                quotes_as_of: now.timestamp(),
                spot_as_of: now.timestamp(),
            },
            regular_close: Some(spot),
            contracts,
        }
    }
}
