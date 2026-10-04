//! Data sources. Each capability is a separate trait so different providers can be mixed:
//! e.g. free CBOE chains with a real-time quote feed. Add a provider by implementing the
//! traits it supports and registering it in [`build`].

pub mod cboe;
pub mod nasdaq;
pub mod yahoo;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use async_trait::async_trait;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::model::{Bar, ChainSnapshot, Quote, Timeframe};

#[async_trait]
pub trait ChainSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn chain(&self, symbol: &str) -> Result<ChainSnapshot>;
}

#[async_trait]
pub trait BarSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn bars(&self, symbol: &str, tf: Timeframe) -> Result<Vec<Bar>>;
}

#[async_trait]
pub trait QuoteSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn quote(&self, symbol: &str) -> Result<Quote>;

    /// Many quotes at once. Providers with a batch endpoint override this; symbols that
    /// fail are left out.
    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        let mut out = Vec::with_capacity(symbols.len());
        for s in symbols {
            if let Ok(q) = self.quote(s).await {
                out.push(q);
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Earnings {
    pub symbol: String,
    pub date: NaiveDate,
    /// Projected from past reporting dates rather than announced.
    pub estimated: bool,
    /// Past report dates the source knows, newest first (usually the last four quarters).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub past: Vec<NaiveDate>,
}

#[async_trait]
pub trait EventSource: Send + Sync {
    fn name(&self) -> &'static str;
    /// Next earnings date, or `None` for instruments without earnings (ETFs, indices).
    async fn next_earnings(&self, symbol: &str) -> Result<Option<Earnings>>;
}

pub struct Sources {
    pub chains: Arc<dyn ChainSource>,
    pub bars: Arc<dyn BarSource>,
    pub quotes: Arc<dyn QuoteSource>,
    /// Real-time single-symbol quotes for the symbol in focus.
    pub live: Option<Arc<dyn QuoteSource>>,
    pub events: Option<Arc<dyn EventSource>>,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ox/0.1")
        .gzip(true)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("http client")
}

pub fn build(p: &crate::config::Providers) -> Result<Sources> {
    let (chains, bars, quotes) = (p.chains.as_str(), p.bars.as_str(), p.quotes.as_str());
    let http = http_client();
    let yahoo = Arc::new(yahoo::Yahoo::new(http.clone()));
    let nasdaq = Arc::new(nasdaq::Nasdaq::new(http.clone()));
    let cboe = Arc::new(cboe::Cboe::new(http));
    let chains: Arc<dyn ChainSource> = match chains {
        "cboe" => cboe,
        other => bail!("unknown chain provider {other:?} (available: cboe)"),
    };
    let bars: Arc<dyn BarSource> = match bars {
        "yahoo" => yahoo.clone(),
        other => bail!("unknown bar provider {other:?} (available: yahoo)"),
    };
    let quotes: Arc<dyn QuoteSource> = match quotes {
        "yahoo" => yahoo,
        other => bail!("unknown quote provider {other:?} (available: yahoo)"),
    };
    let live: Option<Arc<dyn QuoteSource>> = match p.live_quotes.as_str() {
        "nasdaq" => Some(nasdaq.clone()),
        "none" | "" => None,
        other => bail!("unknown live quote provider {other:?} (available: nasdaq, none)"),
    };
    let events: Option<Arc<dyn EventSource>> = match p.events.as_str() {
        "nasdaq" => Some(nasdaq),
        "none" | "" => None,
        other => bail!("unknown events provider {other:?} (available: nasdaq, none)"),
    };
    Ok(Sources { chains, bars, quotes, live, events })
}

/// Indices are namespaced differently by each provider.
pub fn is_index(symbol: &str) -> bool {
    matches!(symbol, "SPX" | "NDX" | "RUT" | "VIX" | "XSP" | "DJX" | "OEX" | "XEO")
}
