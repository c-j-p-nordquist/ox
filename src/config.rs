use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub watchlist: Vec<String>,
    /// Annualized risk-free rate used for Greeks and GEX.
    pub risk_free_rate: f64,
    pub providers: Providers,
    pub cache: CacheTtl,
    pub sizing: crate::analytics::sizing::SizingConfig,
    pub risk: RiskConfig,
    pub costs: Costs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Costs {
    /// Broker commission per option contract, charged on entry and on exit.
    pub commission_per_contract: f64,
}

impl Default for Costs {
    fn default() -> Self {
        Self { commission_per_contract: 0.65 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    /// Candidate benchmarks; the best-fitting one (exposure-weighted R²) is used.
    pub benchmarks: Vec<String>,
    /// Always use this benchmark instead.
    pub benchmark: Option<String>,
    /// Trading days of daily returns for betas and covariance.
    pub lookback_days: usize,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self { benchmarks: vec!["SPY".into(), "QQQ".into()], benchmark: None, lookback_days: 252 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Providers {
    pub chains: String,
    pub bars: String,
    pub quotes: String,
    /// Real-time quote for the symbol in focus ("none" to disable).
    pub live_quotes: String,
    /// Earnings dates ("none" to disable).
    pub events: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheTtl {
    pub chain_secs: u64,
    pub bars_secs: u64,
    pub quote_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watchlist: ["SPY", "QQQ", "IWM", "NVDA", "TSLA", "AAPL", "META", "AMD"].map(String::from).to_vec(),
            risk_free_rate: 0.04,
            providers: Providers::default(),
            cache: CacheTtl::default(),
            sizing: Default::default(),
            risk: RiskConfig::default(),
            costs: Costs::default(),
        }
    }
}

impl Default for Providers {
    fn default() -> Self {
        Self {
            chains: "cboe".into(),
            bars: "yahoo".into(),
            quotes: "yahoo".into(),
            live_quotes: "nasdaq".into(),
            events: "nasdaq".into(),
        }
    }
}

impl Default for CacheTtl {
    fn default() -> Self {
        Self { chain_secs: 30, bars_secs: 20, quote_secs: 5 }
    }
}

/// `$XDG_CONFIG_HOME/ox` or `~/.config/ox`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ox")
}

pub fn state_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("ox")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn write_default() -> Result<PathBuf> {
        let path = config_path();
        std::fs::create_dir_all(config_dir())?;
        if !path.exists() {
            std::fs::write(&path, toml::to_string_pretty(&Self::default())?)?;
        }
        Ok(path)
    }
}
