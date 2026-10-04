//! Local daily history (SQLite): one summary row per symbol and session, per-contract open
//! interest, and the day's support/resistance levels. It gives open-interest changes, an own
//! IV rank, and a record to check levels against later.
//!
//! A snapshot is dated by the session its option quotes reflect (the last close outside
//! regular hours). Open interest in a snapshot is as of the start of that session, so the
//! change between two snapshots is the positioning added or closed in the earlier session.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::analytics::gex::GexReport;
use crate::analytics::levels::LevelsReport;
use crate::analytics::Overview;
use crate::model::ChainSnapshot;

/// Contracts further out than this are not stored per contract.
const MAX_CONTRACT_DTE: i64 = 120;

pub fn path() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("ox").join("history.sqlite")
}

pub fn open() -> Result<Connection> {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let conn = Connection::open(&p).with_context(|| format!("opening {}", p.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS snapshots (
             date TEXT NOT NULL, symbol TEXT NOT NULL, spot REAL, iv30 REAL,
             front_expiry TEXT, front_atm_iv REAL, front_expected_move REAL, front_max_pain REAL,
             gex REAL, zero_gamma REAL, call_wall REAL, put_wall REAL,
             put_call_oi REAL, put_call_volume REAL, call_volume REAL, put_volume REAL,
             recorded_at INTEGER NOT NULL, PRIMARY KEY (date, symbol));
         CREATE TABLE IF NOT EXISTS contracts (
             date TEXT NOT NULL, symbol TEXT NOT NULL, occ TEXT NOT NULL, expiry TEXT NOT NULL,
             right TEXT NOT NULL, strike REAL NOT NULL, open_interest REAL, volume REAL, iv REAL, mid REAL,
             PRIMARY KEY (date, occ));
         CREATE INDEX IF NOT EXISTS contracts_symbol_date ON contracts (symbol, date);
         CREATE TABLE IF NOT EXISTS levels (
             date TEXT NOT NULL, symbol TEXT NOT NULL, kind TEXT NOT NULL, price REAL NOT NULL,
             strength REAL, sources TEXT);
         CREATE INDEX IF NOT EXISTS levels_symbol_date ON levels (symbol, date);",
    )?;
    Ok(conn)
}

/// The session date a chain's option quotes belong to.
pub fn session_date(chain: &ChainSnapshot) -> NaiveDate {
    Utc.timestamp_opt(chain.freshness.quotes_as_of, 0)
        .single()
        .map(|t| t.with_timezone(&New_York).date_naive())
        .unwrap_or_else(|| Utc::now().with_timezone(&New_York).date_naive())
}

pub fn has_snapshot(conn: &Connection, date: NaiveDate, symbol: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM snapshots WHERE date = ?1 AND symbol = ?2", params![date.to_string(), symbol], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

pub fn record(
    conn: &mut Connection,
    chain: &ChainSnapshot,
    gex: &GexReport,
    overview: &Overview,
    levels: Option<&LevelsReport>,
) -> Result<NaiveDate> {
    let date = session_date(chain);
    let d = date.to_string();
    let sym = chain.underlying.as_str();
    let front = overview.expirations.first();
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT OR REPLACE INTO snapshots VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            d,
            sym,
            chain.spot,
            chain.iv30,
            front.map(|e| e.date.to_string()),
            front.and_then(|e| e.atm_iv),
            front.and_then(|e| e.expected_move),
            front.and_then(|e| e.max_pain),
            gex.total_gex,
            gex.zero_gamma,
            gex.call_wall,
            gex.put_wall,
            overview.put_call_oi,
            overview.put_call_volume,
            overview.total_call_volume,
            overview.total_put_volume,
            Utc::now().timestamp(),
        ],
    )?;
    tx.execute("DELETE FROM contracts WHERE date = ?1 AND symbol = ?2", params![d, sym])?;
    {
        let mut ins = tx.prepare("INSERT OR REPLACE INTO contracts VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?;
        for c in &chain.contracts {
            let dte = (c.expiry - date).num_days();
            if !(0..=MAX_CONTRACT_DTE).contains(&dte) || (c.open_interest <= 0.0 && c.volume <= 0.0) {
                continue;
            }
            ins.execute(params![
                d,
                sym,
                c.symbol,
                c.expiry.to_string(),
                c.right.letter().to_string(),
                c.strike,
                c.open_interest,
                c.volume,
                c.iv,
                c.mid()
            ])?;
        }
    }
    if let Some(l) = levels {
        tx.execute("DELETE FROM levels WHERE date = ?1 AND symbol = ?2", params![d, sym])?;
        let mut ins = tx.prepare("INSERT INTO levels VALUES (?1,?2,?3,?4,?5,?6)")?;
        for (kind, list) in [("S", &l.supports), ("R", &l.resistances)] {
            for x in list {
                ins.execute(params![d, sym, kind, x.price, x.strength, x.sources.join(", ")])?;
            }
        }
    }
    tx.commit()?;
    Ok(date)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayRow {
    pub date: NaiveDate,
    pub spot: Option<f64>,
    pub iv30: Option<f64>,
    pub front_atm_iv: Option<f64>,
    pub front_expected_move: Option<f64>,
    pub gex: Option<f64>,
    pub zero_gamma: Option<f64>,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub put_call_oi: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IvRank {
    pub current: f64,
    /// Where current IV30 sits between the stored min and max (0–100).
    pub rank: f64,
    /// Share of stored days with lower IV30 (0–100).
    pub percentile: f64,
    pub days: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryReport {
    pub symbol: String,
    pub rows: Vec<DayRow>,
    pub iv_rank: Option<IvRank>,
}

pub fn history(conn: &Connection, symbol: &str, days: usize) -> Result<HistoryReport> {
    let mut st = conn.prepare(
        "SELECT date, spot, iv30, front_atm_iv, front_expected_move, gex, zero_gamma, call_wall, put_wall, put_call_oi
         FROM snapshots WHERE symbol = ?1 ORDER BY date DESC LIMIT ?2",
    )?;
    let mut rows: Vec<DayRow> = st
        .query_map(params![symbol, days as i64], |r| {
            Ok(DayRow {
                date: r.get::<_, String>(0)?.parse().unwrap_or_default(),
                spot: r.get(1)?,
                iv30: r.get(2)?,
                front_atm_iv: r.get(3)?,
                front_expected_move: r.get(4)?,
                gex: r.get(5)?,
                zero_gamma: r.get(6)?,
                call_wall: r.get(7)?,
                put_wall: r.get(8)?,
                put_call_oi: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.reverse();
    Ok(HistoryReport { symbol: symbol.to_string(), iv_rank: iv_rank(conn, symbol)?, rows })
}

/// IV rank from every stored IV30 for the symbol (needs at least 5 days).
pub fn iv_rank(conn: &Connection, symbol: &str) -> Result<Option<IvRank>> {
    let mut st = conn.prepare("SELECT iv30 FROM snapshots WHERE symbol = ?1 AND iv30 IS NOT NULL ORDER BY date")?;
    let ivs: Vec<f64> = st.query_map(params![symbol], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let Some(&current) = ivs.last() else { return Ok(None) };
    if ivs.len() < 5 {
        return Ok(None);
    }
    let (lo, hi) = ivs.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
    let below = ivs.iter().filter(|x| **x < current).count();
    Ok(Some(IvRank {
        current,
        rank: if hi > lo { (current - lo) / (hi - lo) * 100.0 } else { 50.0 },
        percentile: below as f64 / ivs.len() as f64 * 100.0,
        days: ivs.len(),
    }))
}
