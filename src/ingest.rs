//! Data pushed in from outside ox (e.g. overnight-session bars from a broker), kept in the
//! cache directory and used when fresh.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::state_dir;
use crate::model::Bar;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OvernightBars {
    pub symbol: String,
    #[serde(default)]
    pub source: Option<String>,
    pub bars: Vec<Bar>,
}

fn dir() -> PathBuf {
    state_dir().join("overnight")
}

pub fn save_overnight(input: &str) -> Result<Vec<(String, usize)>> {
    // Accept one object or an array of them.
    let sets: Vec<OvernightBars> = match serde_json::from_str::<Vec<OvernightBars>>(input) {
        Ok(v) => v,
        Err(_) => {
            vec![serde_json::from_str(input).context("expected {symbol, bars: [{t,o,h,l,c,v}]} or an array of them")?]
        }
    };
    std::fs::create_dir_all(dir())?;
    let mut out = Vec::new();
    for mut s in sets {
        s.symbol = s.symbol.to_ascii_uppercase();
        s.bars.sort_by_key(|b| b.t);
        std::fs::write(dir().join(format!("{}.json", s.symbol)), serde_json::to_vec(&s)?)?;
        out.push((s.symbol, s.bars.len()));
    }
    Ok(out)
}

/// Overnight bars for `symbol` if they were ingested within the last day.
pub fn load_overnight(symbol: &str) -> Vec<Bar> {
    let path = dir().join(format!("{}.json", symbol.to_ascii_uppercase()));
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 24 * 3600));
    if !fresh {
        return Vec::new();
    }
    std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<OvernightBars>(&b).ok())
        .map(|s| s.bars)
        .unwrap_or_default()
}

/// Live option quotes pushed in (e.g. from a broker); they override the delayed chain for 30 minutes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveQuotes {
    pub symbol: String,
    #[serde(default)]
    pub source: Option<String>,
    /// Live underlying price, when known.
    #[serde(default)]
    pub spot: Option<f64>,
    pub quotes: Vec<LiveQuote>,
    /// Unix seconds when ingested (set by ox).
    #[serde(default)]
    pub ingested_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveQuote {
    /// OCC symbol (`NVDA261218C00250000`), or give `expiry` + `right` + `strike`.
    #[serde(default)]
    pub occ: Option<String>,
    #[serde(default)]
    pub expiry: Option<chrono::NaiveDate>,
    #[serde(default)]
    pub right: Option<crate::model::Right>,
    #[serde(default)]
    pub strike: Option<f64>,
    pub bid: f64,
    pub ask: f64,
}

const LIVE_TTL_SECS: i64 = 30 * 60;

fn quotes_dir() -> PathBuf {
    state_dir().join("quotes")
}

pub fn save_quotes(input: &str) -> Result<Vec<(String, usize)>> {
    let sets: Vec<LiveQuotes> = match serde_json::from_str::<Vec<LiveQuotes>>(input) {
        Ok(v) => v,
        Err(_) => vec![serde_json::from_str(input).context(
            "expected {symbol, spot?, quotes: [{occ | expiry+right+strike, bid, ask}]} or an array of them",
        )?],
    };
    std::fs::create_dir_all(quotes_dir())?;
    let mut out = Vec::new();
    for mut s in sets {
        s.symbol = s.symbol.to_ascii_uppercase();
        s.ingested_at = chrono::Utc::now().timestamp();
        std::fs::write(quotes_dir().join(format!("{}.json", s.symbol)), serde_json::to_vec(&s)?)?;
        out.push((s.symbol, s.quotes.len()));
    }
    Ok(out)
}

/// Live quotes for `symbol` ingested within the last 30 minutes.
pub fn load_quotes(symbol: &str) -> Option<LiveQuotes> {
    let path = quotes_dir().join(format!("{}.json", symbol.to_ascii_uppercase()));
    let q: LiveQuotes = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (chrono::Utc::now().timestamp() - q.ingested_at < LIVE_TTL_SECS).then_some(q)
}

/// Overwrite matching contracts' bid/ask (and spot) with live quotes; IV and Greeks are cleared so
/// the caller re-derives them from the new mid. Returns the OCC symbols that were updated.
pub fn apply_quotes(chain: &mut crate::model::ChainSnapshot, live: &LiveQuotes) -> Vec<String> {
    if let Some(s) = live.spot.filter(|s| *s > 0.0) {
        chain.spot = s;
    }
    let mut hit = Vec::new();
    for q in &live.quotes {
        if q.bid <= 0.0 || q.ask < q.bid {
            continue;
        }
        let c = chain.contracts.iter_mut().find(|c| match &q.occ {
            Some(occ) => c.symbol.eq_ignore_ascii_case(occ.trim()),
            None => {
                Some(c.expiry) == q.expiry
                    && Some(c.right) == q.right
                    && q.strike.is_some_and(|k| (k - c.strike).abs() < 1e-6)
            }
        });
        if let Some(c) = c {
            c.bid = q.bid;
            c.ask = q.ask;
            c.iv = 0.0;
            c.delta = 0.0;
            c.gamma = 0.0;
            hit.push(c.symbol.clone());
        }
    }
    hit
}
