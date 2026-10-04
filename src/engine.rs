//! The engine owns providers and caches, and answers [`Request`]s. It runs in-process for
//! one-shot commands, or inside the daemon where caches stay warm across invocations.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::task::JoinSet;

use crate::analytics::{self, bs, gex, indicators, levels, pick, review, risk, sizing};
use crate::config::Config;
use crate::model::{Bar, ChainSnapshot, Quote, Timeframe};
use crate::portfolio::{self, PortfolioView, PositionRow, SecType};
use crate::protocol::{Request, Response, Status};
use crate::provider::{self, Sources};

struct Slot<V> {
    value: Mutex<Option<(Instant, Arc<V>)>>,
    fetching: tokio::sync::Mutex<()>,
}

/// TTL cache with per-key fetch de-duplication and stale-on-error fallback.
pub struct Cache<K, V> {
    ttl: Duration,
    slots: Mutex<HashMap<K, Arc<Slot<V>>>>,
}

impl<K: Eq + Hash + Clone, V> Cache<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, slots: Mutex::new(HashMap::new()) }
    }

    fn slot(&self, key: &K) -> Arc<Slot<V>> {
        self.slots
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Slot { value: Mutex::new(None), fetching: tokio::sync::Mutex::new(()) }))
            .clone()
    }

    fn fresh(&self, slot: &Slot<V>) -> Option<Arc<V>> {
        slot.value.lock().unwrap().as_ref().filter(|(t, _)| t.elapsed() < self.ttl).map(|(_, v)| v.clone())
    }

    pub async fn get<F, Fut>(&self, key: &K, fetch: F) -> Result<Arc<V>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V>>,
    {
        let slot = self.slot(key);
        if let Some(v) = self.fresh(&slot) {
            return Ok(v);
        }
        let _guard = slot.fetching.lock().await;
        if let Some(v) = self.fresh(&slot) {
            return Ok(v);
        }
        match fetch().await {
            Ok(v) => {
                let v = Arc::new(v);
                *slot.value.lock().unwrap() = Some((Instant::now(), v.clone()));
                Ok(v)
            }
            Err(e) => slot.value.lock().unwrap().as_ref().map(|(_, v)| v.clone()).ok_or(e),
        }
    }

    /// Refetch in the background without blocking readers, who keep seeing the old value.
    pub async fn refresh<F, Fut>(&self, key: &K, fetch: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V>>,
    {
        let slot = self.slot(key);
        let Ok(_guard) = slot.fetching.try_lock() else { return };
        if let Ok(v) = fetch().await {
            *slot.value.lock().unwrap() = Some((Instant::now(), Arc::new(v)));
        }
    }

    /// Store a value fetched elsewhere (e.g. as part of a batch).
    pub fn insert(&self, key: &K, value: V) {
        *self.slot(key).value.lock().unwrap() = Some((Instant::now(), Arc::new(value)));
    }

    /// The cached value regardless of age.
    pub fn stale(&self, key: &K) -> Option<Arc<V>> {
        let slot = self.slots.lock().unwrap().get(key).cloned()?;
        let v = slot.value.lock().unwrap();
        v.as_ref().map(|(_, v)| v.clone())
    }

    /// The cached value if it is still within the TTL.
    pub fn peek_fresh(&self, key: &K) -> Option<Arc<V>> {
        let slot = self.slots.lock().unwrap().get(key).cloned()?;
        self.fresh(&slot)
    }

    pub fn age(&self, key: &K) -> Option<Duration> {
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(key)?;
        let v = slot.value.lock().unwrap();
        v.as_ref().map(|(t, _)| t.elapsed())
    }

    /// Keys that currently hold a value.
    pub fn keys(&self) -> Vec<K> {
        self.slots
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.value.lock().unwrap().is_some())
            .map(|(k, _)| k.clone())
            .collect()
    }
}

type GexKey = (String, Option<u32>);
/// GEX reports memoized against the chain snapshot they were computed from.
type GexMemo = HashMap<GexKey, (Arc<ChainSnapshot>, Arc<gex::GexReport>)>;
/// Per symbol: (bar count, last bar time, calibration).
type CalibrationMemo = HashMap<String, (usize, i64, Arc<pick::zones::Calibration>)>;

/// What a client asked for recently; the daemon keeps these warm.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Chain,
    Bars(Timeframe),
    Quote,
}

pub struct Engine {
    cfg: Config,
    sources: Sources,
    chains: Cache<String, ChainSnapshot>,
    bars: Cache<(String, Timeframe), Vec<Bar>>,
    quotes: Cache<String, Quote>,
    live: Cache<String, Quote>,
    earnings: Cache<String, Option<provider::Earnings>>,
    gex: Mutex<GexMemo>,
    /// Level calibrations memoized against the daily bars they came from (count, last bar time).
    calibrations: Mutex<CalibrationMemo>,
    history: Option<Mutex<rusqlite::Connection>>,
    /// (session date, symbol) pairs that failed to collect, so they aren't retried in a loop.
    collect_failed: Mutex<std::collections::HashSet<(chrono::NaiveDate, String)>>,
    collecting: std::sync::atomic::AtomicBool,
    /// Last time each chain / bar series was requested; drives daemon prefetching.
    touched: Mutex<HashMap<(String, Kind), Instant>>,
    started: Instant,
}

impl Engine {
    /// Contract filters at their defaults, with the configured commission.
    fn contract_params(&self) -> pick::contracts::ContractParams {
        pick::contracts::ContractParams { commission: self.cfg.costs.commission_per_contract, ..Default::default() }
    }

    pub fn new(cfg: Config) -> Result<Arc<Self>> {
        let sources = provider::build(&cfg.providers)?;
        Ok(Arc::new(Self {
            chains: Cache::new(Duration::from_secs(cfg.cache.chain_secs)),
            bars: Cache::new(Duration::from_secs(cfg.cache.bars_secs)),
            quotes: Cache::new(Duration::from_secs(cfg.cache.quote_secs)),
            live: Cache::new(Duration::from_secs(3)),
            earnings: Cache::new(Duration::from_secs(12 * 3600)),
            gex: Mutex::new(HashMap::new()),
            calibrations: Mutex::new(HashMap::new()),
            touched: Mutex::new(HashMap::new()),
            history: match crate::history::open() {
                Ok(c) => Some(Mutex::new(c)),
                Err(e) => {
                    eprintln!("history store unavailable: {e:#}");
                    None
                }
            },
            collect_failed: Mutex::new(Default::default()),
            collecting: std::sync::atomic::AtomicBool::new(false),
            started: Instant::now(),
            sources,
            cfg,
        }))
    }

    fn touch(&self, symbol: &str, kind: Kind) {
        self.touched.lock().unwrap().insert((symbol.to_string(), kind), Instant::now());
    }

    pub async fn chain(&self, symbol: &str) -> Result<Arc<ChainSnapshot>> {
        let sym = symbol.to_ascii_uppercase();
        self.touch(&sym, Kind::Chain);
        let src = self.sources.chains.clone();
        let r = self.cfg.risk_free_rate;
        self.chains.get(&sym, || fetch_chain(src, sym.clone(), r)).await
    }

    pub async fn bars(&self, symbol: &str, tf: Timeframe) -> Result<Arc<Vec<Bar>>> {
        let sym = symbol.to_ascii_uppercase();
        self.touch(&sym, Kind::Bars(tf));
        let src = self.sources.bars.clone();
        self.bars.get(&(sym.clone(), tf), || async move { src.bars(&sym, tf).await }).await
    }

    pub async fn quote(&self, symbol: &str) -> Result<Arc<Quote>> {
        let sym = symbol.to_ascii_uppercase();
        self.touch(&sym, Kind::Quote);
        let src = self.sources.quotes.clone();
        let s = sym.clone();
        self.quotes.get(&sym, || async move { src.quote(&s).await }).await
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub(crate) fn store(&self) -> Result<&Mutex<rusqlite::Connection>> {
        self.history.as_ref().context("history store unavailable")
    }

    /// Symbols the daemon records daily: watchlist, portfolio underlyings and risk benchmarks.
    pub(crate) fn collection_set(&self) -> Vec<String> {
        let (mut syms, _) = crate::watchlist::effective(&self.cfg.watchlist);
        if let Ok(Some(p)) = portfolio::load() {
            syms.extend(p.positions.iter().map(|x| x.symbol.to_ascii_uppercase()));
        }
        syms.extend(self.cfg.risk.benchmarks.iter().map(|s| s.to_ascii_uppercase()));
        let mut seen = std::collections::HashSet::new();
        syms.retain(|s| seen.insert(s.clone()));
        syms
    }

    /// Record one symbol's snapshot for the session its option quotes reflect.
    pub async fn collect(self: &Arc<Self>, symbol: &str) -> Result<chrono::NaiveDate> {
        let mut chain = self.chain(symbol).await?;
        // Outside regular hours the live spot is pre/post-market while option quotes are from the
        // close: record the session as it closed, with GEX recomputed at the closing price.
        let gex = match chain.regular_close.filter(|_| chain.freshness.quotes_stale) {
            Some(close) => {
                let mut c = (*chain).clone();
                c.spot = close;
                let r = self.cfg.risk_free_rate;
                let c = Arc::new(c);
                let cc = c.clone();
                chain = c;
                Arc::new(tokio::task::spawn_blocking(move || gex::compute(&cc, Utc::now(), r, None)).await?)
            }
            None => self.gex(symbol, None).await?,
        };
        let overview = analytics::overview(&chain, &gex, Utc::now(), 8);
        let levels = self.levels(symbol).await.ok();
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = me.store()?.lock().unwrap();
            crate::history::record(&mut conn, &chain, &gex, &overview, levels.as_ref())
        })
        .await?
    }

    /// Outside regular hours, record any symbol missing a snapshot for the last session.
    async fn collect_due(self: &Arc<Self>) {
        use crate::analytics::market::{last_options_close, session, Session};
        let now = Utc::now();
        let close = last_options_close(now);
        if session(now) == Session::Regular || now < close + chrono::Duration::minutes(15) || self.history.is_none() {
            return;
        }
        use std::sync::atomic::Ordering;
        if self.collecting.swap(true, Ordering::AcqRel) {
            return; // a previous run is still going
        }
        let date = close.with_timezone(&chrono_tz::America::New_York).date_naive();
        for sym in self.collection_set() {
            let done = {
                let conn = self.history.as_ref().unwrap().lock().unwrap();
                crate::history::has_snapshot(&conn, date, &sym).unwrap_or(false)
            };
            if done || self.collect_failed.lock().unwrap().contains(&(date, sym.clone())) {
                continue;
            }
            match self.collect(&sym).await {
                Ok(d) => eprintln!("history: recorded {sym} for {d}"),
                Err(e) => {
                    eprintln!("history: {sym} skipped: {e:#}");
                    self.collect_failed.lock().unwrap().insert((date, sym));
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.collecting.store(false, Ordering::Release);
    }

    /// Real-time quote from the live source, falling back to the regular quote source.
    pub async fn live_quote(&self, symbol: &str) -> Result<Arc<Quote>> {
        let sym = symbol.to_ascii_uppercase();
        let Some(src) = self.sources.live.clone() else { return self.quote(&sym).await };
        let s = sym.clone();
        match self.live.get(&sym, || async move { src.quote(&s).await }).await {
            Ok(q) => Ok(q),
            Err(_) => self.quote(&sym).await,
        }
    }

    /// Next earnings date (cached for hours; `None` for ETFs, indices, or without a source).
    pub async fn earnings(&self, symbol: &str) -> Option<provider::Earnings> {
        let src = self.sources.events.clone()?;
        let sym = symbol.to_ascii_uppercase();
        let s = sym.clone();
        self.earnings.get(&sym, || async move { src.next_earnings(&s).await }).await.ok().and_then(|e| (*e).clone())
    }

    /// Session levels and support/resistance by confluence.
    pub async fn levels(self: &Arc<Self>, symbol: &str) -> Result<levels::LevelsReport> {
        let sym = symbol.to_ascii_uppercase();
        let (daily, intraday, chain, live) = tokio::join!(
            self.bars(&sym, Timeframe::D1),
            self.bars(&sym, Timeframe::M5),
            self.chain(&sym),
            self.live_quote(&sym)
        );
        let daily = daily?;
        let intraday = intraday.unwrap_or_default();
        let chain = chain.ok();
        let gex = if chain.is_some() { self.gex(&sym, None).await.ok() } else { None };
        let now = Utc::now();
        let spot = live
            .ok()
            .map(|q| q.last)
            .or(chain.as_ref().map(|c| c.spot))
            .or(intraday.last().map(|b| b.c))
            .or(daily.last().map(|b| b.c))
            .context("no price")?;
        let max_pain = chain.as_ref().and_then(|c| {
            let exp = analytics::pick_expiry(c, now, None, None)?;
            analytics::expiry_stats(c, now, exp).max_pain
        });
        let opt = levels::OptionLevels {
            call_wall: gex.as_ref().and_then(|g| g.call_wall),
            put_wall: gex.as_ref().and_then(|g| g.put_wall),
            zero_gamma: gex.as_ref().and_then(|g| g.zero_gamma),
            max_pain,
        };
        let overnight = crate::ingest::load_overnight(&sym);
        tokio::task::spawn_blocking(move || levels::compute(&sym, spot, &daily, &intraday, &overnight, &opt, now))
            .await
            .map_err(Into::into)
    }

    /// Quotes for many symbols: fresh ones from cache, the rest in one batch request.
    pub async fn quotes(&self, symbols: &[String]) -> Vec<Quote> {
        let syms: Vec<String> = symbols.iter().map(|s| s.to_ascii_uppercase()).collect();
        for s in &syms {
            self.touch(s, Kind::Quote);
        }
        let missing: Vec<String> = syms.iter().filter(|s| self.quotes.peek_fresh(s).is_none()).cloned().collect();
        if !missing.is_empty() {
            if let Ok(fetched) = self.sources.quotes.quotes(&missing).await {
                for q in fetched {
                    self.quotes.insert(&q.symbol.clone(), q);
                }
            }
        }
        syms.iter()
            .filter_map(|s| self.quotes.peek_fresh(s).or_else(|| self.quotes.stale(s)))
            .map(|q| (*q).clone())
            .collect()
    }

    pub async fn gex(&self, symbol: &str, max_dte: Option<u32>) -> Result<Arc<gex::GexReport>> {
        let chain = self.chain(symbol).await?;
        let key = (chain.underlying.clone(), max_dte);
        if let Some((snap, report)) = self.gex.lock().unwrap().get(&key) {
            if Arc::ptr_eq(snap, &chain) {
                return Ok(report.clone());
            }
        }
        let r = self.cfg.risk_free_rate;
        let c = chain.clone();
        let report = Arc::new(tokio::task::spawn_blocking(move || gex::compute(&c, Utc::now(), r, max_dte)).await?);
        self.gex.lock().unwrap().insert(key, (chain, report.clone()));
        Ok(report)
    }

    /// Walk-forward level calibration for a symbol's daily bars, memoized per bar set.
    async fn calibration(&self, sym: &str, daily: &Arc<Vec<Bar>>) -> Result<Arc<pick::zones::Calibration>> {
        let key = (daily.len(), daily.last().map_or(0, |b| b.t));
        if let Some((n, t, c)) = self.calibrations.lock().unwrap().get(sym) {
            if (*n, *t) == key {
                return Ok(c.clone());
            }
        }
        let bars = daily.clone();
        let cal = Arc::new(tokio::task::spawn_blocking(move || pick::calibrate(&bars)).await?);
        self.calibrations.lock().unwrap().insert(sym.to_string(), (key.0, key.1, cal.clone()));
        Ok(cal)
    }

    pub async fn pick(self: &Arc<Self>, symbol: &str, params: pick::PickParams) -> Result<pick::PickReport> {
        let sym = symbol.to_ascii_uppercase();
        let mut report = self.pick_sized(&sym, params).await?;
        // ENTER but the book cannot fund it: say which holding to swap out, if any is worth it.
        let blocked = report.contracts.as_ref().is_some_and(|c| {
            !c.picks.is_empty() && c.picks.iter().all(|p| p.sizing.as_ref().is_some_and(|s| s.contracts == 0))
        });
        if report.verdict == pick::Verdict::Enter && blocked {
            if let (Some(b), Some(c)) = (report.budget.clone(), report.contracts.as_ref()) {
                let holdings = self.holdings(&b).await.unwrap_or_default();
                let best = c.picks.first().cloned();
                let cand = review::Candidate {
                    symbol: sym.clone(),
                    verdict: report.verdict,
                    headline: report.headline.clone(),
                    setup: report.setups.first().map(|s| s.name.clone()),
                    robust: report.setups.first().is_some_and(|s| s.robust),
                    earnings_move: report.earnings_move.clone(),
                    pick: best,
                };
                report.replace = review::replacements(&holdings, &[cand], &b);
            }
        }
        Ok(report)
    }

    /// `pick`, with its contracts sized against the book.
    async fn pick_sized(self: &Arc<Self>, symbol: &str, params: pick::PickParams) -> Result<pick::PickReport> {
        let sym = symbol.to_ascii_uppercase();
        let (daily, chain, live) =
            tokio::join!(self.bars(&sym, Timeframe::D1), self.chain(&sym), self.live_quote(&sym));
        let daily = daily?;
        let mut chain = chain.ok();
        let gex = if chain.is_some() { self.gex(&sym, None).await.ok() } else { None };
        let earnings = self.earnings(&sym).await;
        // Live quotes pushed in from a broker replace the delayed ones for the contracts they cover.
        let live_quotes = crate::ingest::load_quotes(&sym);
        let mut live_symbols = Vec::new();
        if let (Some(lq), Some(c)) = (&live_quotes, &chain) {
            let mut c2 = (**c).clone();
            live_symbols = crate::ingest::apply_quotes(&mut c2, lq);
            analytics::enrich(&mut c2, Utc::now(), self.cfg.risk_free_rate);
            chain = Some(Arc::new(c2));
        }
        let live_spot = live_quotes.as_ref().and_then(|q| q.spot);
        let iv_rank = match self.store() {
            Ok(store) => {
                let conn = store.lock().unwrap();
                crate::history::iv_rank(&conn, &sym).ok().flatten()
            }
            Err(_) => None,
        };
        let spot = live_spot
            .or(live.ok().map(|q| q.last))
            .or(chain.as_ref().map(|c| c.spot))
            .or(daily.last().map(|b| b.c))
            .context("no price")?;
        let cal = self.calibration(&sym, &daily).await?;
        let live_set: std::collections::HashSet<String> = live_symbols.iter().cloned().collect();
        let r = self.cfg.risk_free_rate;
        let s2 = sym.clone();
        let mut report = tokio::task::spawn_blocking(move || {
            let input = pick::Input {
                symbol: &s2,
                daily: &daily,
                cal: &cal,
                spot,
                chain: chain.as_deref(),
                gex: gex.as_deref(),
                earnings: earnings.as_ref(),
                iv_rank: iv_rank.as_ref(),
                now: Utc::now(),
                r,
                live: &live_set,
            };
            pick::pick(&input, &params)
        })
        .await??;
        // Size picks against the real book when a portfolio is loaded.
        if let Some(risk) = self.risk().await.ok().flatten() {
            if let Some(b) = risk.budget {
                let existing =
                    risk.exposures.iter().find(|e| e.underlying == sym).map_or(0.0, |e| e.market_value.max(0.0));
                if let Some(c) = &mut report.contracts {
                    for pk in &mut c.picks {
                        pk.sizing = Some(sizing::size_lots(pk.max_loss, pk.net_at_entry > 0.0, &sym, &b, existing));
                    }
                }
                report.budget = Some(b);
            }
        }
        Ok(report)
    }

    /// Each holding judged as a fresh trade: `pick` in its direction, and its contracts simulated
    /// through that plan against closing now.
    async fn holdings(self: &Arc<Self>, b: &sizing::Budget) -> Result<Vec<review::Holding>> {
        let Some(file) = portfolio::load()? else { return Ok(Vec::new()) };
        let mut groups: Vec<(String, Vec<portfolio::Position>)> = Vec::new();
        for p in file.positions.iter().filter(|p| p.qty != 0.0) {
            let sym = p.symbol.to_ascii_uppercase();
            match groups.iter_mut().find(|(s, _)| *s == sym) {
                Some((_, v)) => v.push(p.clone()),
                None => groups.push((sym, vec![p.clone()])),
            }
        }
        let mut set = JoinSet::new();
        for (i, (sym, positions)) in groups.into_iter().enumerate() {
            let (me, b) = (self.clone(), b.clone());
            set.spawn(async move { (i, me.holding(&sym, positions, &b).await) });
        }
        let mut out = Vec::new();
        while let Some(r) = set.join_next().await {
            if let (i, Ok(h)) = r? {
                out.push((i, h));
            }
        }
        out.sort_by_key(|(i, _)| *i);
        Ok(out.into_iter().map(|(_, h)| h).collect())
    }

    async fn holding(
        self: &Arc<Self>,
        sym: &str,
        positions: Vec<portfolio::Position>,
        b: &sizing::Budget,
    ) -> Result<review::Holding> {
        let chain = self.chain(sym).await.ok();
        let now = Utc::now();
        let find = |p: &portfolio::Position| {
            chain.as_ref().and_then(|ch| {
                ch.contracts.iter().find(|c| {
                    Some(c.expiry) == p.expiry
                        && Some(c.right) == p.right
                        && p.strike.is_some_and(|k| (k - c.strike).abs() < 1e-6)
                })
            })
        };
        // Direction from the net delta of the group.
        let delta: f64 = positions
            .iter()
            .map(|p| {
                let d = match p.sec_type {
                    SecType::Stock => 1.0,
                    SecType::Option => find(p).map_or(0.0, |c| c.delta),
                };
                d * p.qty * p.multiplier()
            })
            .sum();
        let direction = if delta >= 0.0 { pick::setup::Direction::Long } else { pick::setup::Direction::Short };
        let params = pick::PickParams {
            bias: Some(direction),
            entry: None,
            stop: None,
            t1: None,
            t2: None,
            force_contracts: true,
            contracts: self.contract_params(),
        };
        let report = self.pick_sized(sym, params).await?;
        let spot = report.spot;
        let mut mv = 0.0;
        let mut unrealized = 0.0;
        let mut all_found = true;
        for p in &positions {
            let mark = match p.sec_type {
                SecType::Stock => Some(spot),
                SecType::Option => find(p).map(|c| c.mid()),
            };
            all_found &= mark.is_some();
            if let Some(m) = mark {
                mv += m * p.qty * p.multiplier();
                unrealized += (m - p.avg_cost) * p.qty * p.multiplier();
            }
        }
        let setup = report.setups.first().cloned();
        let hold = match (&setup, all_found) {
            (Some(st), true) => {
                let regime = report.regime.clone();
                let earnings = report
                    .earnings_move
                    .as_ref()
                    .map(|m| pick::contracts::Event { date: m.date, sd: m.sim_move_pct.map(|x| x / 100.0) });
                let (st, chain, positions) = (st.clone(), chain.clone(), positions.clone());
                let r = self.cfg.risk_free_rate;
                let commission = self.cfg.costs.commission_per_contract;
                Some(
                    tokio::task::spawn_blocking(move || {
                        let legs: Vec<pick::contracts::HeldLeg> = positions
                            .iter()
                            .map(|p| pick::contracts::HeldLeg {
                                contract: match p.sec_type {
                                    SecType::Stock => None,
                                    SecType::Option => chain.as_ref().and_then(|ch| {
                                        ch.contracts.iter().find(|c| {
                                            Some(c.expiry) == p.expiry
                                                && Some(c.right) == p.right
                                                && p.strike.is_some_and(|k| (k - c.strike).abs() < 1e-6)
                                        })
                                    }),
                                },
                                units: p.qty * p.multiplier(),
                            })
                            .collect();
                        pick::contracts::hold_value(
                            &legs,
                            &st,
                            spot,
                            |h| pick::sim_vol(&regime, h),
                            earnings,
                            now,
                            r,
                            commission,
                        )
                    })
                    .await?,
                )
            }
            _ => None,
        };
        let mut h = review::Holding {
            underlying: sym.to_string(),
            positions: positions.iter().map(|p| p.label()).collect(),
            direction,
            spot,
            market_value: mv,
            unrealized,
            min_dte: positions.iter().filter_map(|p| p.expiry).map(|e| bs::dte(e, now)).min(),
            verdict: report.verdict,
            headline: report.headline.clone(),
            setup: setup.as_ref().map(|s| s.name.clone()),
            has_edge: setup.as_ref().is_some_and(|s| s.has_edge),
            earnings_move: report.earnings_move.clone(),
            hold,
            action: review::Action::Hold,
            reasons: Vec::new(),
            roll_to: None,
        };
        let fresh = report.contracts.as_ref().and_then(|c| c.picks.first());
        let (action, reasons) = review::decide(&h, fresh, setup.as_ref().map_or(0.0, |s| s.days_to_t1));
        if action == review::Action::Roll {
            h.roll_to = fresh.cloned().map(|mut p| {
                p.sizing =
                    Some(sizing::size_lots(p.max_loss, p.net_at_entry > 0.0, sym, &review::after_closing(b, &h), 0.0));
                p
            });
        }
        h.action = action;
        h.reasons = reasons;
        Ok(h)
    }

    /// Holdings (HOLD / CLOSE / ROLL), candidates with an edge among `scan` (default: the
    /// watchlist), and replacements where a blocked candidate beats a holding.
    pub async fn review(self: &Arc<Self>, scan: Vec<String>) -> Result<review::ReviewReport> {
        let risk = self.risk().await?.context("no portfolio: import one with `ox portfolio import`")?;
        let b = risk.budget.clone().context("portfolio has no net liquidation: cannot size")?;
        let holdings = self.holdings(&b).await?;
        let held: std::collections::HashSet<String> = holdings.iter().map(|h| h.underlying.clone()).collect();
        let scan: Vec<String> = if scan.is_empty() { crate::watchlist::effective(&self.cfg.watchlist).0 } else { scan };
        let mut seen = std::collections::HashSet::new();
        let scan: Vec<String> = scan
            .into_iter()
            .map(|s| s.to_ascii_uppercase())
            .filter(|s| !held.contains(s) && seen.insert(s.clone()))
            .collect();
        let mut set = JoinSet::new();
        for sym in scan {
            let me = self.clone();
            set.spawn(async move {
                let r = me
                    .pick_sized(
                        &sym,
                        pick::PickParams {
                            bias: None,
                            entry: None,
                            stop: None,
                            t1: None,
                            t2: None,
                            force_contracts: false,
                            contracts: me.contract_params(),
                        },
                    )
                    .await;
                (sym, r)
            });
        }
        let (mut candidates, mut no_trade) = (Vec::new(), Vec::new());
        while let Some(r) = set.join_next().await {
            let (sym, r) = r?;
            match r {
                Ok(rep) if rep.verdict != pick::Verdict::NoTrade => candidates.push(review::Candidate {
                    symbol: sym,
                    verdict: rep.verdict,
                    headline: rep.headline.clone(),
                    setup: rep.setups.first().map(|s| s.name.clone()),
                    robust: rep.setups.first().is_some_and(|s| s.robust),
                    earnings_move: rep.earnings_move.clone(),
                    pick: rep.contracts.and_then(|c| c.picks.into_iter().next()),
                }),
                Ok(_) => no_trade.push(sym),
                Err(e) => no_trade.push(format!("{sym} (failed: {e:#})")),
            }
        }
        candidates.sort_by(|a, b| {
            (b.verdict == pick::Verdict::Enter)
                .cmp(&(a.verdict == pick::Verdict::Enter))
                .then(b.robust.cmp(&a.robust))
                .then(a.symbol.cmp(&b.symbol))
        });
        no_trade.sort();
        let replacements = review::replacements(&holdings, &candidates, &b);
        let mut notes = vec![
            "hold EV is what holding adds from here vs closing at mid now (sunk cost ignored); it uses the plan's drift only when the direction has a measured edge, zero drift otherwise".to_string(),
        ];
        if risk.freshness.as_ref().is_some_and(|f| f.quotes_stale) {
            notes.push("option quotes are from the last close: re-check with live quotes before acting".into());
        }
        Ok(review::ReviewReport {
            as_of: Utc::now().timestamp(),
            budget: Some(b),
            holdings,
            candidates,
            replacements,
            no_trade,
            notes,
        })
    }

    pub async fn handle(self: &Arc<Self>, req: Request) -> Response {
        match self.dispatch(req).await {
            Ok(r) => r,
            Err(e) => Response::Error(format!("{e:#}")),
        }
    }

    async fn dispatch(self: &Arc<Self>, req: Request) -> Result<Response> {
        let now = Utc::now();
        Ok(match req {
            Request::Ping => Response::Pong(Status {
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
                uptime_secs: self.started.elapsed().as_secs(),
                providers: format!(
                    "{}/{}/{}, live {}, events {}",
                    self.sources.chains.name(),
                    self.sources.bars.name(),
                    self.sources.quotes.name(),
                    self.sources.live.as_ref().map_or("none", |s| s.name()),
                    self.sources.events.as_ref().map_or("none", |s| s.name())
                ),
                cached_chains: self.chains.keys(),
            }),
            Request::Quotes { symbols } => Response::Quotes(self.quotes(&symbols).await),
            Request::LiveQuote { symbol } => Response::Quote((*self.live_quote(&symbol).await?).clone()),
            Request::Earnings { symbols } => {
                let mut out = Vec::new();
                for s in symbols {
                    if let Some(e) = self.earnings(&s).await {
                        out.push(e);
                    }
                }
                Response::Earnings(out)
            }
            Request::Levels { symbol } => Response::Levels(self.levels(&symbol).await?),
            Request::Brief => Response::Brief(Box::new(self.brief().await?)),
            Request::History { symbol, days } => {
                let sym = symbol.to_ascii_uppercase();
                let me = self.clone();
                Response::History(
                    tokio::task::spawn_blocking(move || {
                        crate::history::history(&me.store()?.lock().unwrap(), &sym, days)
                    })
                    .await??,
                )
            }
            Request::Collect { symbols } => {
                let syms = if symbols.is_empty() { self.collection_set() } else { symbols };
                let mut out = Vec::new();
                for s in syms {
                    let s = s.to_ascii_uppercase();
                    let outcome = match self.collect(&s).await {
                        Ok(date) => format!("recorded {date}"),
                        Err(e) => format!("skipped: {e:#}"),
                    };
                    out.push((s, outcome));
                }
                Response::Collected(out)
            }
            Request::Chain { symbol, expiry, min_dte, strikes } => {
                let chain = self.chain(&symbol).await?;
                let exp = analytics::pick_expiry(&chain, now, expiry, min_dte).context("no expirations available")?;
                Response::Chain(analytics::chain_view(&chain, now, exp, strikes))
            }
            Request::Gex { symbol, max_dte } => Response::Gex((*self.gex(&symbol, max_dte).await?).clone()),
            Request::Overview { symbol } => {
                let chain = self.chain(&symbol).await?;
                let g = self.gex(&symbol, None).await?;
                Response::Overview(analytics::overview(&chain, &g, now, 8))
            }
            Request::Portfolio => Response::Portfolio(self.portfolio().await?),
            Request::Risk => Response::Risk(self.risk().await?),
            Request::Pick { symbol, params } => Response::Pick(Box::new(self.pick(&symbol, params).await?)),
            Request::Review { scan } => Response::Review(Box::new(self.review(scan).await?)),
            Request::Shutdown => Response::Ok,
        })
    }

    async fn portfolio(self: &Arc<Self>) -> Result<Option<PortfolioView>> {
        let Some(file) = portfolio::load()? else { return Ok(None) };
        let now = Utc::now();

        // Fetch every underlying's chain (options) or quote (stock) concurrently.
        let mut set = JoinSet::new();
        let mut seen = std::collections::HashSet::new();
        for p in &file.positions {
            let key = (p.symbol.to_ascii_uppercase(), p.sec_type);
            if seen.insert(key.clone()) {
                let me = self.clone();
                set.spawn(async move {
                    let (chain, quote) = match key.1 {
                        SecType::Option => (me.chain(&key.0).await.ok(), None),
                        SecType::Stock => (None, me.quote(&key.0).await.ok()),
                    };
                    (key, chain, quote)
                });
            }
        }
        let mut chains = HashMap::new();
        let mut quotes = HashMap::new();
        while let Some(r) = set.join_next().await {
            let (key, chain, quote) = r?;
            if let Some(c) = chain {
                chains.insert(key.0.clone(), c);
            }
            if let Some(q) = quote {
                quotes.insert(key.0, q);
            }
        }

        let rows: Vec<PositionRow> = file
            .positions
            .iter()
            .map(|p| {
                let sym = p.symbol.to_ascii_uppercase();
                let mult = p.multiplier();
                let (mark, under, delta, dte, iv) = match p.sec_type {
                    SecType::Stock => {
                        let q = quotes.get(&sym);
                        (q.map(|q| q.last), q.map(|q| q.last), Some(1.0), None, None)
                    }
                    SecType::Option => {
                        let chain = chains.get(&sym);
                        let c = chain.and_then(|ch| {
                            ch.contracts.iter().find(|c| {
                                Some(c.expiry) == p.expiry
                                    && Some(c.right) == p.right
                                    && p.strike.is_some_and(|k| (k - c.strike).abs() < 1e-6)
                            })
                        });
                        (
                            c.map(|c| c.mid()),
                            chain.map(|ch| ch.spot),
                            c.map(|c| c.delta),
                            p.expiry.map(|e| bs::dte(e, now)),
                            c.map(|c| c.iv),
                        )
                    }
                };
                let mv = mark.map(|m| m * p.qty * mult);
                let pnl = mark.map(|m| (m - p.avg_cost) * p.qty * mult);
                let cost = p.avg_cost * p.qty.abs() * mult;
                PositionRow {
                    label: p.label(),
                    position: p.clone(),
                    mark,
                    underlying_price: under,
                    market_value: mv,
                    unrealized_pnl: pnl,
                    unrealized_pct: pnl.filter(|_| cost > 0.0).map(|x| x / cost * 100.0),
                    delta_shares: delta.map(|d| d * p.qty * mult),
                    dte,
                    iv,
                }
            })
            .collect();
        Ok(Some(PortfolioView {
            as_of: file.as_of,
            account: file.account,
            net_liquidation: file.net_liquidation,
            cash: file.cash,
            currency: file.currency,
            total_market_value: rows.iter().filter_map(|r| r.market_value).sum(),
            total_unrealized: rows.iter().filter_map(|r| r.unrealized_pnl).sum(),
            rows,
        }))
    }

    pub(crate) async fn risk(self: &Arc<Self>) -> Result<Option<risk::RiskReport>> {
        let Some(file) = portfolio::load()? else { return Ok(None) };
        let mut symbols: Vec<String> = file.positions.iter().map(|p| p.symbol.to_ascii_uppercase()).collect();
        symbols.sort();
        symbols.dedup();

        let market = {
            let me = self.clone();
            tokio::spawn(async move { me.market_context().await.ok() })
        };
        let mut benchmarks = Vec::new();
        for b in &self.cfg.risk.benchmarks {
            if let Ok(bars) = self.bars(b, Timeframe::D1).await {
                benchmarks.push((b.to_ascii_uppercase(), bars));
            }
        }
        let mut set = JoinSet::new();
        for sym in symbols {
            let me = self.clone();
            set.spawn(async move {
                let chain = me.chain(&sym).await.ok();
                let gex = if chain.is_some() { me.gex(&sym, None).await.ok() } else { None };
                let quote = if chain.is_none() { me.quote(&sym).await.ok().map(|q| q.last) } else { None };
                let history = me.bars(&sym, Timeframe::D1).await.ok();
                let earnings = me.earnings(&sym).await;
                (sym, chain, gex, quote, history, earnings)
            });
        }
        let mut fetched = Vec::new();
        while let Some(r) = set.join_next().await {
            fetched.push(r?);
        }
        // Account values are in the base currency; positions and sizing are in USD.
        let fx = match file.currency.as_deref().map(str::to_ascii_uppercase) {
            None => Some(1.0),
            Some(c) if c == "USD" => Some(1.0),
            Some(c) => self.quote(&format!("USD{c}=X")).await.ok().map(|q| q.last),
        };
        let market = market.await.ok().flatten();
        let r = self.cfg.risk_free_rate;
        let sizing_cfg = self.cfg.sizing.clone();
        let risk_cfg = self.cfg.risk.clone();
        let report = tokio::task::spawn_blocking(move || {
            let data = fetched
                .iter()
                .map(|(sym, chain, gex, quote, history, earnings)| {
                    let d = risk::UnderlyingData {
                        chain: chain.as_deref(),
                        gex: gex.as_deref(),
                        quote: *quote,
                        history: history.as_ref().map(|h| h.as_slice()),
                        earnings: earnings.clone(),
                    };
                    (sym.clone(), d)
                })
                .collect();
            let factors = risk::FactorInput {
                benchmarks: benchmarks.iter().map(|(n, b)| (n.clone(), b.as_slice())).collect(),
                force: risk_cfg.benchmark.clone(),
                lookback: risk_cfg.lookback_days,
            };
            risk::compute(&file, &data, Utc::now(), r, fx, &sizing_cfg, market, &factors)
        })
        .await?;
        Ok(Some(report))
    }

    /// SPY trend and gamma regime plus VIX: the backdrop a book is judged against.
    pub(crate) async fn market_context(self: &Arc<Self>) -> Result<risk::MarketContext> {
        let bars = self.bars("SPY", Timeframe::D1).await?;
        let g = self.gex("SPY", None).await.ok();
        let q = self.quote("SPY").await.ok();
        let vix = self.quote("^VIX").await.ok();
        let snap = indicators::snapshot(&bars, false).context("no SPY history")?;
        let spot = g.as_ref().map(|g| g.spot).unwrap_or(snap.close);
        // The chain's previous close is the last regular close (Yahoo's lags a session pre-market).
        let chain_prev = self.chain("SPY").await.ok().map(|c| c.prev_close).filter(|p| *p > 0.0);
        let prev = chain_prev.or(q.as_ref().map(|q| q.prev_close).filter(|p| *p > 0.0)).unwrap_or(snap.close);
        let chg = (spot / prev - 1.0) * 100.0;
        let above = |x: Option<f64>| x.map(|v| spot > v);
        let (above_50, above_200) = (above(snap.ema50), above(snap.sma200));
        let (vix_last, vix_chg) = (vix.as_ref().map(|v| v.last), vix.as_ref().map(|v| v.change_pct));
        let zg = g.as_ref().and_then(|g| g.zero_gamma);

        let mut off = Vec::new();
        let mut on = Vec::new();
        match zg {
            Some(z) if spot < z => {
                off.push(format!("SPY {spot:.2} is below zero gamma {z:.2}: dealer hedging amplifies moves"))
            }
            Some(z) => on.push(format!("SPY {spot:.2} is above zero gamma {z:.2}: dealer hedging damps moves")),
            None => {}
        }
        match snap.trend {
            "down" => off.push("SPY daily trend is down (close < EMA9 < EMA21)".into()),
            "up" => on.push("SPY daily trend is up (close > EMA9 > EMA21)".into()),
            _ => {}
        }
        match above_50 {
            Some(false) => off.push("SPY is below its 50-day EMA".into()),
            Some(true) => on.push("SPY is above its 50-day EMA".into()),
            None => {}
        }
        if let Some(v) = vix_last {
            if v >= 20.0 {
                off.push(format!("VIX is elevated at {v:.1}"));
            } else if v < 15.0 {
                on.push(format!("VIX is low at {v:.1}"));
            }
        }
        if let Some(c) = vix_chg.filter(|c| *c >= 10.0) {
            off.push(format!("VIX is up {c:.0}% today"));
        }
        if chg <= -1.0 {
            off.push(format!("SPY is down {chg:.1}% today"));
        } else if chg >= 1.0 {
            on.push(format!("SPY is up {chg:.1}% today"));
        }

        Ok(risk::MarketContext {
            spy: spot,
            spy_change_pct: chg,
            spy_trend: snap.trend.to_string(),
            spy_rsi: snap.rsi14,
            spy_above_50d: above_50,
            spy_above_200d: above_200,
            spy_zero_gamma: zg,
            spy_call_wall: g.as_ref().and_then(|g| g.call_wall),
            spy_put_wall: g.as_ref().and_then(|g| g.put_wall),
            vix: vix_last,
            vix_change_pct: vix_chg,
            risk_off: off,
            risk_on: on,
        })
    }

    /// Daemon background loop: keep recently used data fresh so requests never wait on the network.
    pub async fn prefetch_loop(self: Arc<Self>) {
        const KEEP_WARM: Duration = Duration::from_secs(15 * 60);
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            ticks += 1;
            // Every 5 minutes, make sure the last session is recorded in the history store.
            if ticks % 150 == 1 {
                let me = self.clone();
                tokio::spawn(async move { me.collect_due().await });
            }
            let active: Vec<(String, Kind)> = {
                let mut t = self.touched.lock().unwrap();
                t.retain(|_, at| at.elapsed() < KEEP_WARM);
                t.keys().cloned().collect()
            };
            let due = |age: Option<Duration>, ttl_secs: u64| {
                age.is_some_and(|a| a > Duration::from_secs(ttl_secs).mul_f32(0.75))
            };
            let mut due_quotes = Vec::new();
            for (sym, kind) in active {
                let me = self.clone();
                match kind {
                    Kind::Chain if due(self.chains.age(&sym), self.cfg.cache.chain_secs) => {
                        tokio::spawn(async move {
                            let src = me.sources.chains.clone();
                            let r = me.cfg.risk_free_rate;
                            me.chains.refresh(&sym, || fetch_chain(src, sym.clone(), r)).await;
                        });
                    }
                    Kind::Bars(tf) if due(self.bars.age(&(sym.clone(), tf)), self.cfg.cache.bars_secs) => {
                        tokio::spawn(async move {
                            let src = me.sources.bars.clone();
                            let key = (sym.clone(), tf);
                            me.bars.refresh(&key, || async move { src.bars(&sym, tf).await }).await;
                        });
                    }
                    Kind::Quote if due(self.quotes.age(&sym), self.cfg.cache.quote_secs) => due_quotes.push(sym),
                    _ => {}
                }
            }
            // All due quotes in one batch request.
            if !due_quotes.is_empty() {
                let me = self.clone();
                tokio::spawn(async move {
                    if let Ok(fetched) = me.sources.quotes.quotes(&due_quotes).await {
                        for q in fetched {
                            me.quotes.insert(&q.symbol.clone(), q);
                        }
                    }
                });
            }
        }
    }
}

/// Chain requests in flight at once: CBOE answers 429 when a review or brief asks for dozens.
static CHAIN_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

async fn fetch_chain(src: Arc<dyn provider::ChainSource>, sym: String, r: f64) -> Result<ChainSnapshot> {
    let mut chain = {
        let _permit = CHAIN_GATE.acquire().await?;
        let mut wait = Duration::from_secs(2);
        let mut attempt = 0;
        loop {
            match src.chain(&sym).await {
                Ok(c) => break c,
                // Rate limited: back off and retry a few times before giving up.
                Err(e) if attempt < 3 && format!("{e:#}").contains("429") => {
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                    wait *= 3;
                }
                Err(e) => return Err(e),
            }
        }
    };
    tokio::task::spawn_blocking(move || {
        analytics::enrich(&mut chain, Utc::now(), r);
        chain
    })
    .await
    .map_err(Into::into)
}
