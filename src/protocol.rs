//! Request/response types shared by the in-process engine and the daemon socket
//! (newline-delimited JSON).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::analytics::gex::{GexMap, GexReport};
use crate::analytics::levels::LevelsReport;
use crate::analytics::pick::{PickParams, PickReport};
use crate::analytics::risk::RiskReport;
use crate::analytics::{ChainView, Overview};
use crate::history::HistoryReport;
use crate::model::Quote;
use crate::portfolio::PortfolioView;
use crate::provider::Earnings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Quotes {
        symbols: Vec<String>,
    },
    /// Real-time quote for one symbol (falls back to the regular quote source).
    LiveQuote {
        symbol: String,
    },
    Earnings {
        symbols: Vec<String>,
    },
    Levels {
        symbol: String,
    },
    Brief,
    History {
        symbol: String,
        days: usize,
    },
    /// Record today's snapshot now (default set: watchlist, portfolio, benchmarks).
    Collect {
        symbols: Vec<String>,
    },
    Chain {
        symbol: String,
        expiry: Option<NaiveDate>,
        min_dte: Option<u32>,
        strikes: Option<usize>,
    },
    Gex {
        symbol: String,
        max_dte: Option<u32>,
    },
    /// GEX by strike and expiration (the heatmap), with a read of it.
    GexMap {
        symbol: String,
        max_dte: Option<u32>,
    },
    Overview {
        symbol: String,
    },
    Portfolio,
    Risk,
    Pick {
        symbol: String,
        params: PickParams,
    },
    /// Holdings judged as fresh trades, candidates from `scan` (default: watchlist), replacements.
    Review {
        scan: Vec<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    /// `chains/bars/quotes` provider names.
    pub providers: String,
    pub cached_chains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Pong(Status),
    Quotes(Vec<Quote>),
    Quote(Quote),
    Earnings(Vec<Earnings>),
    Levels(LevelsReport),
    Brief(Box<crate::brief::Brief>),
    History(HistoryReport),
    /// (symbol, outcome) per symbol.
    Collected(Vec<(String, String)>),
    Chain(ChainView),
    Gex(GexReport),
    GexMap(Box<GexMap>),
    Overview(Overview),
    Portfolio(Option<PortfolioView>),
    Risk(Option<RiskReport>),
    Pick(Box<PickReport>),
    Review(Box<crate::analytics::review::ReviewReport>),
    Ok,
    Error(String),
}
