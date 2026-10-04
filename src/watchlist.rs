//! Watchlist synced from outside (e.g. a broker watchlist), stored at
//! `~/.config/ox/watchlist.json`. It takes precedence over `watchlist` in config.toml.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::config_dir;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WatchlistFile {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    pub symbols: Vec<String>,
}

pub fn path() -> PathBuf {
    config_dir().join("watchlist.json")
}

pub fn load() -> Option<WatchlistFile> {
    let bytes = std::fs::read(path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Accept the JSON schema, or plain tickers separated by commas, spaces or newlines.
pub fn parse(input: &str) -> Result<WatchlistFile> {
    let trimmed = input.trim();
    let mut w = if trimmed.starts_with('{') {
        serde_json::from_str::<WatchlistFile>(trimmed).context("watchlist JSON does not match the schema")?
    } else {
        WatchlistFile {
            symbols: trimmed.split(|c: char| c == ',' || c.is_whitespace()).map(str::to_string).collect(),
            ..Default::default()
        }
    };
    let mut seen = std::collections::HashSet::new();
    w.symbols = w
        .symbols
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect();
    anyhow::ensure!(!w.symbols.is_empty(), "no symbols in watchlist input");
    Ok(w)
}

pub fn save(w: &WatchlistFile) -> Result<PathBuf> {
    std::fs::create_dir_all(config_dir())?;
    let path = path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(w)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The watchlist to use: the synced file if present, else the config default.
pub fn effective(config_default: &[String]) -> (Vec<String>, Option<String>) {
    match load() {
        Some(w) if !w.symbols.is_empty() => (w.symbols, w.name),
        _ => (config_default.iter().map(|s| s.to_ascii_uppercase()).collect(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_json() {
        assert_eq!(parse("msft, nvda\nMSFT  aapl").unwrap().symbols, ["MSFT", "NVDA", "AAPL"]);
        let w = parse(r#"{"name":"Stocks","source":"broker","symbols":["goog","meta"]}"#).unwrap();
        assert_eq!(w.name.as_deref(), Some("Stocks"));
        assert_eq!(w.symbols, ["GOOG", "META"]);
        assert!(parse("  ").is_err());
    }
}
