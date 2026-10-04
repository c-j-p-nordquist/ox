//! Portfolio snapshot pushed in from outside (a broker export, a script, or an AI agent with broker access),
//! stored at `~/.config/ox/portfolio.json` and marked to market with cached data.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::model::Right;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecType {
    #[default]
    Stock,
    Option,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// Underlying ticker, e.g. `NVDA`.
    pub symbol: String,
    #[serde(default)]
    pub sec_type: SecType,
    #[serde(default)]
    pub right: Option<Right>,
    #[serde(default)]
    pub strike: Option<f64>,
    #[serde(default)]
    pub expiry: Option<NaiveDate>,
    /// Signed quantity: shares or contracts; negative for short.
    pub qty: f64,
    /// Average cost per share (options: per-share premium, i.e. the per-contract cost / 100).
    pub avg_cost: f64,
    #[serde(default)]
    pub multiplier: Option<f64>,
}

impl Position {
    pub fn multiplier(&self) -> f64 {
        self.multiplier.unwrap_or(if self.sec_type == SecType::Option { 100.0 } else { 1.0 })
    }

    pub fn label(&self) -> String {
        match (self.sec_type, self.expiry, self.right, self.strike) {
            (SecType::Option, Some(e), Some(r), Some(k)) => {
                format!("{} {} {}{}", self.symbol, e.format("%y-%m-%d"), trim_num(k), r.letter())
            }
            _ => self.symbol.clone(),
        }
    }
}

fn trim_num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortfolioFile {
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub net_liquidation: Option<f64>,
    #[serde(default)]
    pub cash: Option<f64>,
    /// Excess liquidity / available funds as the broker reports it (account currency).
    #[serde(default)]
    pub excess_liquidity: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    pub positions: Vec<Position>,
}

pub fn path() -> PathBuf {
    config_dir().join("portfolio.json")
}

pub fn load() -> Result<Option<PortfolioFile>> {
    match std::fs::read(path()) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).context("parsing portfolio.json")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save(p: &PortfolioFile) -> Result<PathBuf> {
    std::fs::create_dir_all(config_dir())?;
    let path = path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(p)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionRow {
    pub label: String,
    pub position: Position,
    pub mark: Option<f64>,
    pub underlying_price: Option<f64>,
    pub market_value: Option<f64>,
    pub unrealized_pnl: Option<f64>,
    pub unrealized_pct: Option<f64>,
    /// Share-equivalent delta exposure.
    pub delta_shares: Option<f64>,
    pub dte: Option<i64>,
    pub iv: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioView {
    pub as_of: Option<String>,
    pub account: Option<String>,
    pub net_liquidation: Option<f64>,
    pub cash: Option<f64>,
    pub currency: Option<String>,
    pub rows: Vec<PositionRow>,
    pub total_market_value: f64,
    pub total_unrealized: f64,
}
