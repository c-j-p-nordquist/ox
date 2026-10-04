//! CBOE public delayed quotes (~15 min delay). Full chain with IV, Greeks and open
//! interest in a single request; free and needs no account.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::NaiveDateTime;
use serde::Deserialize;

use super::{is_index, ChainSource};
use crate::analytics::market::Freshness;
use crate::model::{parse_occ, ChainSnapshot, OptionContract};

const BASE: &str = "https://cdn-api.cboe.com/api/global/delayed_quotes/options";

pub struct Cboe {
    http: reqwest::Client,
}

impl Cboe {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

#[derive(Deserialize)]
struct Envelope {
    timestamp: Option<String>,
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    #[serde(default)]
    current_price: Option<f64>,
    #[serde(default)]
    close: Option<f64>,
    #[serde(default)]
    prev_day_close: Option<f64>,
    #[serde(default)]
    price_change: Option<f64>,
    #[serde(default)]
    iv30: Option<f64>,
    #[serde(default)]
    options: Vec<RawOption>,
}

#[derive(Deserialize)]
struct RawOption {
    option: String,
    #[serde(default)]
    bid: Option<f64>,
    #[serde(default)]
    ask: Option<f64>,
    #[serde(default)]
    iv: Option<f64>,
    #[serde(default)]
    open_interest: Option<f64>,
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    delta: Option<f64>,
    #[serde(default)]
    gamma: Option<f64>,
    #[serde(default)]
    theta: Option<f64>,
    #[serde(default)]
    vega: Option<f64>,
    #[serde(default)]
    last_trade_price: Option<f64>,
    #[serde(default)]
    prev_day_close: Option<f64>,
}

pub fn parse(underlying: &str, body: &[u8]) -> Result<ChainSnapshot> {
    let env: Envelope = serde_json::from_slice(body).context("decoding CBOE chain")?;
    let d = env.data;
    let spot = d.current_price.or(d.close).filter(|p| *p > 0.0).context("CBOE response has no underlying price")?;
    let as_of = env
        .timestamp
        .as_deref()
        .and_then(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S").ok())
        // CBOE timestamps are UTC.
        .map(|t| t.and_utc().timestamp())
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let contracts = d
        .options
        .into_iter()
        .filter_map(|o| {
            let (root, expiry, right, strike) = parse_occ(&o.option)?;
            Some(OptionContract {
                symbol: o.option,
                root,
                expiry,
                right,
                strike,
                bid: o.bid.unwrap_or(0.0),
                ask: o.ask.unwrap_or(0.0),
                last: o.last_trade_price.unwrap_or(0.0),
                iv: o.iv.unwrap_or(0.0),
                delta: o.delta.unwrap_or(0.0),
                gamma: o.gamma.unwrap_or(0.0),
                theta: o.theta.unwrap_or(0.0),
                vega: o.vega.unwrap_or(0.0),
                open_interest: o.open_interest.unwrap_or(0.0),
                volume: o.volume.unwrap_or(0.0),
                prev_close: o.prev_day_close.unwrap_or(0.0),
            })
        })
        .collect();
    Ok(ChainSnapshot {
        underlying: underlying.to_string(),
        spot,
        // `prev_day_close` rolls to today's close after hours; `price_change` stays correct.
        prev_close: d.price_change.map(|c| spot - c).or(d.prev_day_close).unwrap_or(spot),
        iv30: d.iv30.map(|v| v / 100.0),
        as_of,
        source: "cboe".into(),
        delayed: true,
        freshness: Freshness::assess(as_of, chrono::Utc::now()),
        regular_close: d.close,
        contracts,
    })
}

#[async_trait]
impl ChainSource for Cboe {
    fn name(&self) -> &'static str {
        "cboe"
    }

    async fn chain(&self, symbol: &str) -> Result<ChainSnapshot> {
        let sym = symbol.to_ascii_uppercase();
        let path = if is_index(&sym) { format!("_{sym}") } else { sym.clone() };
        let resp = self.http.get(format!("{BASE}/{path}.json")).send().await.context("requesting CBOE chain")?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN || resp.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("CBOE has no option chain for {sym}");
        }
        let body = resp.error_for_status()?.bytes().await?;
        tokio::task::spawn_blocking(move || parse(&sym, &body)).await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample() {
        let body = br#"{"timestamp":"2026-09-28 06:17:08","data":{"current_price":771.35,"prev_day_close":767.18,"iv30":12.275,
            "options":[{"option":"SPY261016C00780000","bid":5.1,"ask":5.2,"iv":0.13,"open_interest":1200,"volume":300,
            "delta":0.35,"gamma":0.02,"theta":-0.2,"vega":0.7,"last_trade_price":5.2,"prev_day_close":5.0},
            {"option":"bad","bid":1}]}}"#;
        let c = parse("SPY", body).unwrap();
        assert_eq!(c.spot, 771.35);
        assert_eq!(c.iv30, Some(0.12275));
        assert_eq!(c.contracts.len(), 1);
        assert_eq!(c.contracts[0].strike, 780.0);
        assert_eq!(c.as_of, 1790576228);
    }
}
