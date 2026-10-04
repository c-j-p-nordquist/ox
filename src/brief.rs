//! Pre-market brief: everything worth knowing before the open, in one call. Market backdrop,
//! book risk, and per symbol (benchmarks, positions, watchlist) the pre-market move, nearest
//! levels, today's expected move, IV, and upcoming earnings.
//!
//! It also carries the book review (`ox review`), what changed in it since the previous day's
//! brief, upcoming reports with their implied moves, and `catalyst_due`: the symbols that need a
//! catalyst check (news, confirmed dates, macro) today, so the research runs where it matters
//! instead of on everything every day.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::{NaiveDate, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::analytics::market::{self, Session};
use crate::analytics::pick::Verdict;
use crate::analytics::review::{Action, ReviewReport};
use crate::analytics::risk::{MarketContext, RiskReport};
use crate::analytics::{self, levels};
use crate::engine::Engine;
use crate::history::IvRank;
use crate::provider::Earnings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Index,
    Position,
    Watchlist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nearest {
    pub price: f64,
    pub distance_pct: f64,
    pub strength: f64,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefSymbol {
    pub symbol: String,
    pub group: Group,
    pub price: Option<f64>,
    pub prev_close: Option<f64>,
    /// Move vs the prior regular close (pre-market move before the open).
    pub change_pct: Option<f64>,
    pub session: Option<Session>,
    pub realtime: bool,
    pub resistances: Vec<Nearest>,
    pub supports: Vec<Nearest>,
    pub session_levels: Option<levels::SessionLevels>,
    /// One-standard-deviation move to the nearest expiry, $ and %.
    pub expected_move: Option<f64>,
    pub expected_move_pct: Option<f64>,
    pub expected_move_expiry: Option<NaiveDate>,
    pub iv30: Option<f64>,
    pub iv_rank: Option<IvRank>,
    pub zero_gamma: Option<f64>,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub earnings: Option<Earnings>,
    pub earnings_in_days: Option<i64>,
    /// Move as a multiple of today's expected move (how unusual the gap is).
    pub move_vs_expected: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Brief {
    pub generated_at: i64,
    pub date: NaiveDate,
    pub session: Session,
    pub market: Option<MarketContext>,
    pub risk: Option<RiskReport>,
    pub symbols: Vec<BriefSymbol>,
    /// Symbols ordered by absolute move vs expected move (largest first).
    pub movers: Vec<String>,
    /// Symbols reporting earnings within the next 7 days.
    pub earnings_soon: Vec<String>,
    /// `ox review`: holdings, candidates with an edge, replacements.
    #[serde(default)]
    pub review: Option<ReviewReport>,
    /// What changed in the review since the previous day's brief.
    #[serde(default)]
    pub changes: Vec<String>,
    /// Reports ahead for positions and candidates (next 30 days), soonest first.
    #[serde(default)]
    pub events: Vec<EventRow>,
    /// Symbols that need a catalyst check today, and why.
    #[serde(default)]
    pub catalyst_due: Vec<Due>,
    /// Written summary added by whoever publishes the brief (e.g. Claude).
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub symbol: String,
    /// "position" or "candidate".
    pub role: String,
    pub date: NaiveDate,
    pub days: i64,
    pub estimated: bool,
    pub implied_move_pct: Option<f64>,
    pub past_rms_pct: Option<f64>,
    /// Other positions reporting within a day of this one.
    pub cluster: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Due {
    pub symbol: String,
    pub reasons: Vec<String>,
}

/// The review as of a day, to diff against the next day's.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub date: Option<NaiveDate>,
    /// Holding → action; candidate → verdict.
    pub actions: std::collections::BTreeMap<String, String>,
    pub verdicts: std::collections::BTreeMap<String, String>,
}

/// Last two days' snapshots: the brief diffs today's review against the latest earlier day.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    current: Snapshot,
    previous: Snapshot,
}

fn state_path() -> std::path::PathBuf {
    crate::config::config_dir().join("brief_state.json")
}

fn label<T: Serialize>(x: &T) -> String {
    serde_json::to_value(x).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

pub fn snapshot(r: &ReviewReport, date: NaiveDate) -> Snapshot {
    Snapshot {
        date: Some(date),
        actions: r.holdings.iter().map(|h| (h.underlying.clone(), label(&h.action))).collect(),
        verdicts: r.candidates.iter().map(|c| (c.symbol.clone(), label(&c.verdict))).collect(),
    }
}

/// Changes from `prev` to `now`, in words.
pub fn changes(prev: &Snapshot, now: &Snapshot) -> Vec<String> {
    if prev.date.is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (sym, a) in &now.actions {
        match prev.actions.get(sym) {
            Some(p) if p != a => {
                out.push(format!("{sym}: review now says {} (was {})", a.to_uppercase(), p.to_uppercase()))
            }
            None => out.push(format!("{sym}: new holding, review says {}", a.to_uppercase())),
            _ => {}
        }
    }
    for sym in prev.actions.keys().filter(|s| !now.actions.contains_key(*s)) {
        out.push(format!("{sym}: no longer held"));
    }
    for (sym, v) in &now.verdicts {
        match prev.verdicts.get(sym) {
            Some(p) if p != v => out.push(format!(
                "{sym}: {} (was {})",
                v.to_uppercase().replace('_', " "),
                p.to_uppercase().replace('_', " ")
            )),
            None => out.push(format!("{sym}: new candidate, {}", v.to_uppercase().replace('_', " "))),
            _ => {}
        }
    }
    for sym in prev.verdicts.keys().filter(|s| !now.verdicts.contains_key(*s) && !now.actions.contains_key(*s)) {
        out.push(format!("{sym}: dropped to NO TRADE ({} before)", prev.verdicts[sym].to_uppercase()));
    }
    out
}

/// Reports in the next 30 days for holdings and candidates, with clusters among holdings.
pub fn events(r: &ReviewReport, today: NaiveDate) -> Vec<EventRow> {
    let mut rows: Vec<EventRow> = Vec::new();
    let held = r.holdings.iter().map(|h| (h.underlying.as_str(), "position", h.earnings_move.as_ref()));
    let cands = r.candidates.iter().map(|c| (c.symbol.as_str(), "candidate", c.earnings_move.as_ref()));
    for (sym, role, m) in held.chain(cands) {
        let Some(m) = m else { continue };
        let days = (m.date - today).num_days();
        if !(0..=30).contains(&days) {
            continue;
        }
        rows.push(EventRow {
            symbol: sym.to_string(),
            role: role.into(),
            date: m.date,
            days,
            estimated: m.estimated,
            implied_move_pct: m.implied_move_pct,
            past_rms_pct: m.past_rms_pct,
            cluster: Vec::new(),
        });
    }
    let positions: Vec<(String, NaiveDate)> =
        rows.iter().filter(|e| e.role == "position").map(|e| (e.symbol.clone(), e.date)).collect();
    for e in rows.iter_mut().filter(|e| e.role == "position") {
        e.cluster = positions
            .iter()
            .filter(|(s, d)| *s != e.symbol && (*d - e.date).num_days().abs() <= 1)
            .map(|(s, _)| s.clone())
            .collect();
    }
    rows.sort_by_key(|e| (e.date, e.symbol.clone()));
    rows
}

/// Which symbols need a catalyst check today. A check is due when a position reports within a
/// week (or has an unconfirmed date within two), when the review's call on a holding changed,
/// for anything in a proposed replacement or newly ENTER, and for every holding on Mondays.
pub fn catalyst_due(r: &ReviewReport, ev: &[EventRow], prev: &Snapshot, today: NaiveDate) -> Vec<Due> {
    use chrono::Datelike;
    let mut due: Vec<Due> = Vec::new();
    let mut add = |sym: &str, why: String| match due.iter_mut().find(|d| d.symbol == sym) {
        Some(d) => d.reasons.push(why),
        None => due.push(Due { symbol: sym.to_string(), reasons: vec![why] }),
    };
    for e in ev.iter().filter(|e| e.role == "position") {
        if e.days <= 7 {
            add(
                &e.symbol,
                format!("reports {} ({} days){}", e.date, e.days, if e.estimated { ", date unconfirmed" } else { "" }),
            );
        } else if e.estimated && e.days <= 14 {
            add(&e.symbol, format!("estimated report date {} ({} days): confirm it", e.date, e.days));
        }
        if !e.cluster.is_empty() && e.days <= 14 {
            add(&e.symbol, format!("reports within a day of {}", e.cluster.join(", ")));
        }
    }
    for h in &r.holdings {
        let a = label(&h.action);
        if prev.date.is_some() && prev.actions.get(&h.underlying).is_some_and(|p| *p != a) {
            add(&h.underlying, format!("review changed to {}", a.to_uppercase()));
        } else if h.action != Action::Hold && prev.actions.get(&h.underlying).is_none_or(|p| *p != a) {
            add(&h.underlying, format!("review says {}", a.to_uppercase()));
        }
    }
    for x in &r.replacements {
        add(&x.close, format!("proposed to close for {}", x.open));
        add(&x.open, format!("proposed to replace {}", x.close));
    }
    for c in r.candidates.iter().filter(|c| c.verdict == Verdict::Enter) {
        if prev.verdicts.get(&c.symbol).is_none_or(|v| v != "enter") {
            add(&c.symbol, "new ENTER candidate".into());
        }
    }
    if today.weekday() == chrono::Weekday::Mon {
        for h in &r.holdings {
            add(&h.underlying, "weekly book check".into());
        }
    }
    due
}

impl Engine {
    pub async fn brief(self: &Arc<Self>) -> Result<Brief> {
        let now = Utc::now();
        let today = now.with_timezone(&New_York).date_naive();

        let positions: Vec<String> = crate::portfolio::load()
            .ok()
            .flatten()
            .map(|p| p.positions.iter().map(|x| x.symbol.to_ascii_uppercase()).collect())
            .unwrap_or_default();
        let (watch, _) = crate::watchlist::effective(&self.config().watchlist);
        let mut order: Vec<(String, Group)> = Vec::new();
        let mut seen = HashSet::new();
        for (list, g) in [
            (self.config().risk.benchmarks.clone(), Group::Index),
            (positions, Group::Position),
            (watch, Group::Watchlist),
        ] {
            for s in list {
                let s = s.to_ascii_uppercase();
                if seen.insert(s.clone()) {
                    order.push((s, g));
                }
            }
        }

        let market = {
            let me = self.clone();
            tokio::spawn(async move { me.market_context().await.ok() })
        };
        let risk = {
            let me = self.clone();
            tokio::spawn(async move { me.risk().await.ok().flatten() })
        };

        // Per-symbol work is network-bound; keep a handful in flight to stay polite to sources.
        let gate = Arc::new(Semaphore::new(6));
        let mut set = JoinSet::new();
        for (i, (sym, group)) in order.iter().cloned().enumerate() {
            let me = self.clone();
            let gate = gate.clone();
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                (i, me.brief_symbol(&sym, group, today).await)
            });
        }
        let mut rows: Vec<(usize, BriefSymbol)> = Vec::new();
        while let Some(r) = set.join_next().await {
            rows.push(r?);
        }
        rows.sort_by_key(|(i, _)| *i);
        let symbols: Vec<BriefSymbol> = rows.into_iter().map(|(_, s)| s).collect();

        let mut movers: Vec<&BriefSymbol> = symbols.iter().filter(|s| s.change_pct.is_some()).collect();
        let key = |s: &BriefSymbol| s.move_vs_expected.or(s.change_pct.map(|c| c / 2.0)).unwrap_or(0.0).abs();
        movers.sort_by(|a, b| key(b).total_cmp(&key(a)));
        // The book review, diffed against the previous day's.
        let review = self.review(Vec::new()).await.ok();
        let mut state: State =
            std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let (mut changes_v, mut events_v, mut due_v) = (Vec::new(), Vec::new(), Vec::new());
        if let Some(r) = &review {
            let snap = snapshot(r, today);
            if state.current.date.is_some_and(|d| d < today) {
                state.previous = std::mem::take(&mut state.current);
            }
            let prev = state.previous.clone();
            changes_v = changes(&prev, &snap);
            events_v = events(r, today);
            due_v = catalyst_due(r, &events_v, &prev, today);
            state.current = snap;
            if let Ok(b) = serde_json::to_vec_pretty(&state) {
                let _ = std::fs::write(state_path(), b);
            }
        }

        let earnings_soon = symbols
            .iter()
            .filter(|s| s.earnings_in_days.is_some_and(|d| (0..=7).contains(&d)))
            .map(|s| s.symbol.clone())
            .collect();

        Ok(Brief {
            generated_at: now.timestamp(),
            date: today,
            session: market::session(now),
            market: market.await.ok().flatten(),
            risk: risk.await.ok().flatten(),
            movers: movers.iter().take(8).map(|s| s.symbol.clone()).collect(),
            earnings_soon,
            symbols,
            review,
            changes: changes_v,
            events: events_v,
            catalyst_due: due_v,
            notes: None,
        })
    }

    async fn brief_symbol(self: &Arc<Self>, sym: &str, group: Group, today: NaiveDate) -> BriefSymbol {
        let (quote, lv, chain, earnings) =
            tokio::join!(self.live_quote(sym), self.levels(sym), self.chain(sym), self.earnings(sym));
        let quote = quote.ok();
        let lv = lv.ok();
        let chain = chain.ok();
        let gex = if chain.is_some() { self.gex(sym, None).await.ok() } else { None };
        let now = Utc::now();

        // Expected move to the nearest expiry that is not today.
        let em = chain.as_ref().and_then(|c| {
            let exp = analytics::pick_expiry(c, now, None, Some(1))?;
            let st = analytics::expiry_stats(c, now, exp);
            Some((st.expected_move?, exp))
        });
        let iv_rank = self.store().ok().and_then(|s| crate::history::iv_rank(&s.lock().unwrap(), sym).ok().flatten());

        let price = quote.as_ref().map(|q| q.last).or(lv.as_ref().map(|l| l.spot));
        let prev_close = lv.as_ref().and_then(|l| l.session.prev_close).or(quote.as_ref().map(|q| q.prev_close));
        let change_pct = match (price, prev_close) {
            (Some(p), Some(pc)) if pc > 0.0 => Some((p / pc - 1.0) * 100.0),
            _ => None,
        };
        let em_pct = match (em, price) {
            (Some((m, _)), Some(p)) if p > 0.0 => Some(m / p * 100.0),
            _ => None,
        };
        let nearest = |v: &[levels::Level]| -> Vec<Nearest> {
            v.iter()
                .take(2)
                .map(|l| Nearest {
                    price: l.price,
                    distance_pct: l.distance_pct,
                    strength: l.strength,
                    sources: l.sources.clone(),
                })
                .collect()
        };
        BriefSymbol {
            symbol: sym.to_string(),
            group,
            price,
            prev_close,
            change_pct,
            session: quote.as_ref().and_then(|q| q.session),
            realtime: quote.as_ref().is_some_and(|q| !q.delayed),
            resistances: lv.as_ref().map(|l| nearest(&l.resistances)).unwrap_or_default(),
            supports: lv.as_ref().map(|l| nearest(&l.supports)).unwrap_or_default(),
            session_levels: lv.as_ref().map(|l| l.session.clone()),
            expected_move: em.map(|(m, _)| m),
            expected_move_pct: em_pct,
            expected_move_expiry: em.map(|(_, e)| e),
            iv30: chain.as_ref().and_then(|c| c.iv30),
            iv_rank,
            zero_gamma: gex.as_ref().and_then(|g| g.zero_gamma),
            call_wall: gex.as_ref().and_then(|g| g.call_wall),
            put_wall: gex.as_ref().and_then(|g| g.put_wall),
            earnings_in_days: earnings.as_ref().map(|e| (e.date - today).num_days()),
            earnings,
            move_vs_expected: match (change_pct, em_pct) {
                (Some(c), Some(e)) if e > 0.0 => Some(c / e),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::pick::earnings::EarningsMove;
    use crate::analytics::pick::setup::Direction;
    use crate::analytics::review::{Candidate, Holding};

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn em(date: NaiveDate, estimated: bool) -> EarningsMove {
        EarningsMove {
            date,
            estimated,
            implied_move_pct: Some(6.0),
            implied_from: None,
            past: vec![],
            past_rms_pct: None,
            sim_move_pct: None,
        }
    }

    fn holding(sym: &str, action: Action, e: Option<EarningsMove>) -> Holding {
        Holding {
            underlying: sym.into(),
            positions: vec![],
            direction: Direction::Long,
            spot: 100.0,
            market_value: 1000.0,
            unrealized: 0.0,
            min_dte: None,
            verdict: Verdict::NoTrade,
            headline: String::new(),
            setup: None,
            has_edge: false,
            earnings_move: e,
            hold: None,
            action,
            reasons: vec![],
            roll_to: None,
        }
    }

    fn report(h: Vec<Holding>, c: Vec<Candidate>) -> ReviewReport {
        ReviewReport {
            as_of: 0,
            budget: None,
            holdings: h,
            candidates: c,
            replacements: vec![],
            no_trade: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn diffs_and_due_list() {
        let today = d(10, 21); // a Wednesday
        let yesterday = report(vec![holding("GOOG", Action::Hold, None), holding("META", Action::Hold, None)], vec![]);
        let prev = snapshot(&yesterday, d(10, 20));
        let cand = Candidate {
            symbol: "NFLX".into(),
            verdict: Verdict::Enter,
            headline: String::new(),
            setup: None,
            robust: false,
            earnings_move: None,
            pick: None,
        };
        let r = report(
            vec![
                holding("GOOG", Action::Close, Some(em(d(10, 28), true))),
                holding("META", Action::Hold, Some(em(d(10, 28), false))),
            ],
            vec![cand],
        );
        let now = snapshot(&r, today);
        let ch = changes(&prev, &now);
        assert!(ch.iter().any(|c| c.contains("GOOG: review now says CLOSE (was HOLD)")), "{ch:?}");
        assert!(ch.iter().any(|c| c.contains("NFLX: new candidate, ENTER")), "{ch:?}");
        let ev = events(&r, today);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].cluster, vec!["META".to_string()]);
        let due = catalyst_due(&r, &ev, &prev, today);
        let goog = due.iter().find(|x| x.symbol == "GOOG").unwrap();
        assert!(goog.reasons.iter().any(|r| r.contains("reports 2026-10-28 (7 days), date unconfirmed")), "{goog:?}");
        assert!(goog.reasons.iter().any(|r| r.contains("review changed to CLOSE")), "{goog:?}");
        assert!(due.iter().any(|x| x.symbol == "NFLX"));
        // A quiet day: nothing due, nothing changed.
        let quiet = report(vec![holding("GOOG", Action::Hold, None)], vec![]);
        let s = snapshot(&quiet, today);
        let p = Snapshot { date: Some(d(10, 20)), ..s.clone() };
        assert!(changes(&p, &s).is_empty());
        assert!(catalyst_due(&quiet, &[], &p, today).is_empty());
        // Mondays: every holding gets the weekly check.
        assert_eq!(catalyst_due(&quiet, &[], &p, d(10, 19)).len(), 1);
    }
}
