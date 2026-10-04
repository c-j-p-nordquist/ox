//! Yahoo Finance chart API: free OHLCV bars and quotes (delayed for most venues).

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{BarSource, QuoteSource};
use crate::model::{Bar, Quote, Timeframe};

const BASE: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const SPARK: &str = "https://query1.finance.yahoo.com/v8/finance/spark";
/// Symbols per spark request.
const SPARK_BATCH: usize = 20;

pub struct Yahoo {
    http: reqwest::Client,
}

impl Yahoo {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn chart(&self, symbol: &str, interval: &str, range: &str, prepost: bool) -> Result<ChartResult> {
        let url = format!("{BASE}/{}?interval={interval}&range={range}&includePrePost={prepost}", yahoo_symbol(symbol));
        let resp: ChartResponse = self
            .http
            .get(url)
            .send()
            .await
            .context("requesting Yahoo chart")?
            .json()
            .await
            .context("decoding Yahoo chart")?;
        if let Some(err) = resp.chart.error {
            bail!("Yahoo: {}", err.description.unwrap_or_else(|| "unknown error".into()));
        }
        resp.chart.result.and_then(|mut r| r.pop()).with_context(|| format!("no Yahoo data for {symbol}"))
    }
}

fn yahoo_symbol(symbol: &str) -> String {
    match symbol.to_ascii_uppercase().as_str() {
        "SPX" => "^GSPC".into(),
        "NDX" => "^NDX".into(),
        "RUT" => "^RUT".into(),
        "VIX" => "^VIX".into(),
        "XSP" => "^XSP".into(),
        "DJX" => "^DJI".into(),
        s => s.replace('.', "-"),
    }
}

#[derive(Deserialize)]
struct ChartResponse {
    chart: Chart,
}

#[derive(Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
    error: Option<YahooError>,
}

#[derive(Deserialize)]
struct YahooError {
    description: Option<String>,
}

#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    #[serde(default)]
    timestamp: Vec<i64>,
    indicators: Indicators,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    regular_market_price: Option<f64>,
    previous_close: Option<f64>,
    chart_previous_close: Option<f64>,
    regular_market_day_high: Option<f64>,
    regular_market_day_low: Option<f64>,
    regular_market_volume: Option<f64>,
    regular_market_time: Option<i64>,
}

#[derive(Deserialize)]
struct Indicators {
    #[serde(default)]
    quote: Vec<Ohlcv>,
}

#[derive(Deserialize, Default)]
struct Ohlcv {
    #[serde(default)]
    open: Vec<Option<f64>>,
    #[serde(default)]
    high: Vec<Option<f64>>,
    #[serde(default)]
    low: Vec<Option<f64>>,
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    volume: Vec<Option<f64>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spark {
    #[serde(default)]
    timestamp: Vec<i64>,
    #[serde(default)]
    close: Vec<Option<f64>>,
    previous_close: Option<f64>,
    chart_previous_close: Option<f64>,
}

fn params(tf: Timeframe) -> (&'static str, &'static str) {
    match tf {
        Timeframe::M1 => ("1m", "2d"),
        Timeframe::M5 => ("5m", "5d"),
        Timeframe::M15 => ("15m", "1mo"),
        Timeframe::H1 => ("60m", "6mo"),
        Timeframe::D1 => ("1d", "5y"),
    }
}

#[async_trait]
impl BarSource for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    async fn bars(&self, symbol: &str, tf: Timeframe) -> Result<Vec<Bar>> {
        let (interval, range) = params(tf);
        // Intraday bars include pre- and post-market (04:00–20:00 ET).
        let r = self.chart(symbol, interval, range, tf.is_intraday()).await?;
        let q = r.indicators.quote.into_iter().next().unwrap_or_default();
        let bars = r
            .timestamp
            .iter()
            .enumerate()
            .filter_map(|(i, &t)| {
                let get = |v: &Vec<Option<f64>>| v.get(i).copied().flatten();
                Some(Bar {
                    t,
                    o: get(&q.open)?,
                    h: get(&q.high)?,
                    l: get(&q.low)?,
                    c: get(&q.close)?,
                    v: get(&q.volume).unwrap_or(0.0),
                })
            })
            .collect();
        Ok(bars)
    }
}

#[async_trait]
impl QuoteSource for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    async fn quote(&self, symbol: &str) -> Result<Quote> {
        let m = self.chart(symbol, "1d", "1d", false).await?.meta;
        let last = m.regular_market_price.with_context(|| format!("no price for {symbol}"))?;
        let prev = m.previous_close.or(m.chart_previous_close).unwrap_or(last);
        Ok(Quote {
            symbol: symbol.to_ascii_uppercase(),
            last,
            prev_close: prev,
            change: last - prev,
            change_pct: if prev > 0.0 { (last / prev - 1.0) * 100.0 } else { 0.0 },
            high: m.regular_market_day_high,
            low: m.regular_market_day_low,
            volume: m.regular_market_volume,
            time: m.regular_market_time.unwrap_or(0),
            delayed: true,
            source: "yahoo".into(),
            session: None,
        })
    }

    /// One request per 20 symbols via the spark endpoint.
    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        let mut out = Vec::with_capacity(symbols.len());
        for chunk in symbols.chunks(SPARK_BATCH) {
            let wanted: Vec<String> = chunk.iter().map(|s| yahoo_symbol(s)).collect();
            let url = format!("{SPARK}?symbols={}&range=1d&interval=1d", wanted.join(","));
            let resp: std::collections::HashMap<String, Spark> = self
                .http
                .get(url)
                .send()
                .await
                .context("requesting Yahoo spark")?
                .json()
                .await
                .context("decoding Yahoo spark")?;
            for (orig, ysym) in chunk.iter().zip(&wanted) {
                let Some(sp) = resp.get(ysym) else { continue };
                let Some(last) = sp.close.iter().rev().flatten().next().copied() else { continue };
                let prev = sp.previous_close.or(sp.chart_previous_close).unwrap_or(last);
                out.push(Quote {
                    symbol: orig.to_ascii_uppercase(),
                    last,
                    prev_close: prev,
                    change: last - prev,
                    change_pct: if prev > 0.0 { (last / prev - 1.0) * 100.0 } else { 0.0 },
                    high: None,
                    low: None,
                    volume: None,
                    time: sp.timestamp.last().copied().unwrap_or(0),
                    delayed: true,
                    source: "yahoo".into(),
                    session: None,
                });
            }
        }
        Ok(out)
    }
}
