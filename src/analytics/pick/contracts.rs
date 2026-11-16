//! Contracts for a plan, ranked by simulating the plan itself.
//!
//! Price paths start at the plan's entry and move at random: no drift beyond the risk-free rate
//! the options are priced with (so a fairly priced call or put averages zero), forecast *realized*
//! volatility, so the difference between implied and realized vol shows up in the results. Each
//! path is managed like the plan: out on a daily close beyond the stop, half off at T1 with the
//! stop to breakeven, the rest at T2, everything at the time stop. Every candidate is marked to Black-Scholes at those exit points
//! (sticky-strike IV, lower after an earnings date), paying half the bid/ask spread and commission
//! each way.
//!
//! No plan is assumed to have an edge (see [`super::setup`]), so nothing is simulated "as if the
//! plan works". Instead each structure gets its expected result at random, the same paths split
//! by whether T1 came before the stop, and from those the share of the time T1 must come first
//! for it to break even. That share minus the random-walk rate is the edge the trader has to
//! bring; the ranking prefers the structure that needs the least.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use super::setup::Setup;
use crate::analytics::bs;
use crate::analytics::legs::{Leg, Side};
use crate::model::{ChainSnapshot, OptionContract, Right};

const PATHS: usize = 3000;
/// Calendar days per trading day.
const CAL_PER_TRADING: f64 = 365.0 / 252.0;
/// IV multiplier once an earnings date has passed, when the report's own move is unknown.
const EARNINGS_CRUSH: f64 = 0.85;

/// An earnings report inside the simulation: the date and its 1σ log move (implied by the
/// options, else the stock's past reactions). With a move, paths jump by it on the first close
/// after the date and each contract's IV afterwards loses exactly that event variance; without
/// one, IV just drops by [`EARNINGS_CRUSH`].
#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub date: NaiveDate,
    pub sd: Option<f64>,
}

impl Event {
    /// IV left in a contract once the report has passed.
    fn iv_after(&self, iv: f64, expiry: NaiveDate, now: DateTime<Utc>) -> f64 {
        match self.sd {
            Some(sd) => {
                let t = bs::years_to_expiry(expiry, now);
                let var = iv * iv * t;
                ((var - sd * sd).max(0.25 * var) / t).sqrt()
            }
            None => iv * EARNINGS_CRUSH,
        }
    }

    /// Simulated day (1-based) whose close first includes the report: the session after the
    /// report date (most large caps report after the close), counted in weekdays.
    fn day(&self, today: NaiveDate, hold: usize) -> Option<usize> {
        use chrono::Datelike;
        let sessions = (1..=(self.date - today).num_days())
            .filter(|&k| (today + Duration::days(k)).weekday().number_from_monday() <= 5)
            .count();
        Some(sessions + 1).filter(|&k| k <= hold)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractParams {
    pub limit: usize,
    pub min_oi: f64,
    /// Maximum bid/ask spread per leg as a fraction of mid.
    pub max_spread: f64,
    /// Maximum loss per 1-lot in dollars.
    pub max_risk: Option<f64>,
    /// Broker commission per option contract, charged on entry and exit.
    #[serde(default = "default_commission")]
    pub commission: f64,
}

fn default_commission() -> f64 {
    crate::config::Costs::default().commission_per_contract
}

impl Default for ContractParams {
    /// The CLI defaults.
    fn default() -> Self {
        Self { limit: 6, min_oi: 100.0, max_spread: 0.12, max_risk: None, commission: default_commission() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Structure {
    LongCall,
    LongPut,
    CallDebitSpread,
    PutDebitSpread,
    PutCreditSpread,
    CallCreditSpread,
    CallButterfly,
    PutButterfly,
}

impl Structure {
    pub fn label(self) -> &'static str {
        match self {
            Structure::LongCall => "long call",
            Structure::LongPut => "long put",
            Structure::CallDebitSpread => "call debit spread",
            Structure::PutDebitSpread => "put debit spread",
            Structure::PutCreditSpread => "put credit spread",
            Structure::CallCreditSpread => "call credit spread",
            Structure::CallButterfly => "call butterfly",
            Structure::PutButterfly => "put butterfly",
        }
    }

    fn is_credit(self) -> bool {
        matches!(self, Structure::PutCreditSpread | Structure::CallCreditSpread)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pick {
    pub structure: Structure,
    pub expiry: NaiveDate,
    pub dte: i64,
    pub legs: Vec<Leg>,
    /// Quantity per leg for one unit (a butterfly sells 2 of the body).
    pub ratios: Vec<u32>,
    /// Net per share at today's mid: positive = debit.
    pub net_mid_now: f64,
    /// Estimated net per share when the trigger fires at the plan entry (same IV, today's date).
    pub net_at_entry: f64,
    /// Limit price to use for the entry order (per share): mid at entry.
    pub limit_at_entry: f64,
    pub max_loss: f64,
    pub max_gain: Option<f64>,
    /// Expected P&L per unit if the stock moves at random (no drift beyond the risk-free rate),
    /// after friction.
    pub ev_no_edge: f64,
    /// The random paths split by outcome: mean P&L when T1 came before the stop, and when it
    /// did not.
    pub ev_if_t1: f64,
    pub ev_if_not: f64,
    /// Share of the time T1 must come before the stop for this structure to break even.
    pub breakeven_p_t1: f64,
    /// `breakeven_p_t1` minus the random-walk rate: how much better than chance the plan has to
    /// be. Negative when the structure pays at random. The ranking key, lowest first.
    pub edge_needed: f64,
    /// Share of random paths that end with a profit.
    pub p_profit: f64,
    /// P&L per unit at the plan's levels on the days they are expected.
    pub pnl_at_sl: f64,
    pub pnl_at_t1: f64,
    pub pnl_at_t2: f64,
    /// If the underlying sits at the entry until the time stop.
    pub pnl_flat: f64,
    pub delta: f64,
    pub theta_day: f64,
    pub vega: f64,
    pub friction: f64,
    pub min_oi: f64,
    pub worst_spread_pct: f64,
    /// Every leg priced from live broker quotes (otherwise delayed/last close).
    pub live_quotes: bool,
    pub exit_plan: String,
    pub why: String,
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizing: Option<crate::analytics::sizing::Sizing>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractsReport {
    /// Price the simulation starts from (the plan's average entry, or spot when entering now).
    pub entry: f64,
    /// Forecast realized vol used for the paths.
    pub sim_vol: f64,
    /// Share of random paths on which T1 comes before the stop.
    pub p_t1_no_edge: f64,
    /// Average price the stop fills at on the random paths. The stop is a daily close beyond SL,
    /// so this is past SL: the loss when stopped is more than 1R.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_stop_fill: Option<f64>,
    pub time_stop: NaiveDate,
    pub min_expiry: NaiveDate,
    pub expirations: Vec<NaiveDate>,
    pub evaluated: usize,
    pub picks: Vec<Pick>,
    pub warnings: Vec<String>,
}

/// Small deterministic PRNG (SplitMix64) with Box-Muller normals.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn normals(&mut self, n: usize) -> Vec<f64> {
        let mut v = Vec::with_capacity(n + 1);
        while v.len() < n {
            let (u1, u2) = (self.uniform(), self.uniform());
            let r = (-2.0 * u1.ln()).sqrt();
            v.push(r * (std::f64::consts::TAU * u2).cos());
            v.push(r * (std::f64::consts::TAU * u2).sin());
        }
        v.truncate(n);
        v
    }
}

/// An exit of `frac` of the position on trading day `day` at underlying `price`.
#[derive(Debug, Clone, Copy)]
struct Exit {
    day: usize,
    price: f64,
    frac: f64,
}

#[derive(Clone, Copy)]
struct Plan {
    d: f64,
    entry: f64,
    sl: f64,
    t1: f64,
    t2: f64,
    hold: usize,
    /// Earnings jump: (day, 1σ log move). The path's last shock (index `hold`) drives it.
    jump: Option<(usize, f64)>,
}

/// Shocks per path: one per day plus one for an earnings jump. Paths come in mirrored pairs
/// (antithetic), so the sample has no net direction of its own: without that, 3000 paths carry
/// about a point of noise in the edge a structure needs, in proportion to its delta.
fn shocks(n: usize, hold: usize) -> Vec<Vec<f64>> {
    let mut rng = Rng(0x0A11_CE5E_ED00_0001);
    let mut out = Vec::with_capacity(n + 1);
    while out.len() < n {
        let z = rng.normals(hold + 1);
        out.push(z.iter().map(|x| -x).collect());
        out.push(z);
    }
    out.truncate(n);
    out
}

/// Manage one path like the plan, on daily closes.
///
/// The stop is the plan's own rule, a daily close beyond SL, so it is checked on the close and
/// fills there: past the stop, by about 0.58 of a day's move on average. The targets and the
/// breakeven stop after T1 are resting orders that fill on a touch at their level; daily closes
/// miss intraday touches, so those levels are moved inward (Broadie–Glasserman) to stand in for
/// continuous monitoring.
fn manage(p: &Plan, shocks: &[f64], mu: f64, sigma_d: f64, out: &mut Vec<Exit>) -> bool {
    out.clear();
    let shift = (0.5826 * sigma_d).exp();
    let (be, t1, t2) = if p.d > 0.0 {
        (p.entry * shift, p.t1 / shift, p.t2 / shift)
    } else {
        (p.entry / shift, p.t1 * shift, p.t2 * shift)
    };
    let mut s = p.entry;
    let mut half = false;
    let mut hit_t1 = false;
    for (k, z) in shocks.iter().enumerate().take(p.hold) {
        s *= (mu - 0.5 * sigma_d * sigma_d + sigma_d * z).exp();
        let day = k + 1;
        let mut gapped = false;
        if let Some((_, sd)) = p.jump.filter(|(jd, _)| *jd == day) {
            s *= (sd * shocks[p.hold] - 0.5 * sd * sd).exp();
            gapped = true;
        }
        // A report gaps through levels: orders fill at the price after the gap, not at the level.
        let fill = |level: f64| if gapped { s } else { level };
        if half && p.d * (s - be) <= 0.0 {
            out.push(Exit { day, price: fill(p.entry), frac: 0.5 });
            return hit_t1;
        }
        if !half && p.d * (s - p.sl) <= 0.0 {
            out.push(Exit { day, price: s, frac: 1.0 });
            return hit_t1;
        }
        if !half && p.d * (s - t1) >= 0.0 {
            half = true;
            hit_t1 = true;
            out.push(Exit { day, price: fill(p.t1), frac: 0.5 });
        }
        if half && p.d * (s - t2) >= 0.0 {
            out.push(Exit { day, price: fill(p.t2), frac: 0.5 });
            return hit_t1;
        }
    }
    out.push(Exit { day: p.hold, price: s, frac: if half { 0.5 } else { 1.0 } });
    hit_t1
}

/// Simulated exits per path for a given drift, and whether each path reached T1 before the stop.
fn simulate(p: &Plan, shocks: &[Vec<f64>], mu: f64, sigma_d: f64) -> (Vec<Vec<Exit>>, Vec<bool>) {
    let mut hit = Vec::with_capacity(shocks.len());
    let mut all = Vec::with_capacity(shocks.len());
    let mut buf = Vec::new();
    for z in shocks {
        hit.push(manage(p, z, mu, sigma_d, &mut buf));
        all.push(buf.clone());
    }
    (all, hit)
}

/// Mean fill of the paths that ended on the stop (the whole position out at or beyond SL).
fn avg_stop_fill(p: &Plan, paths: &[Vec<Exit>]) -> Option<f64> {
    let fills: Vec<f64> = paths
        .iter()
        .filter(|x| x.len() == 1 && x[0].frac == 1.0 && p.d * (x[0].price - p.sl) <= 0.0)
        .map(|x| x[0].price)
        .collect();
    (!fills.is_empty()).then(|| fills.iter().sum::<f64>() / fills.len() as f64)
}

fn share(hit: &[bool]) -> f64 {
    hit.iter().filter(|h| **h).count() as f64 / hit.len().max(1) as f64
}

struct Cand {
    structure: Structure,
    legs: Vec<(usize, Side, u32)>,
}

fn trading_to_date(today: NaiveDate, days: f64) -> NaiveDate {
    today + Duration::days((days * CAL_PER_TRADING).ceil() as i64)
}

fn at_close(date: NaiveDate) -> DateTime<Utc> {
    use chrono::TimeZone;
    New_York
        .from_local_datetime(&date.and_hms_opt(16, 0, 0).unwrap())
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap()
}

/// The plan as simulated: from spot when it is at its entry, else from its average entry.
fn plan_of(setup: &Setup, spot: f64, earnings: Option<Event>, today: NaiveDate) -> Plan {
    let entering_now = setup.status == super::setup::Status::Enter;
    let hold = setup.backtest.hold_days;
    Plan {
        d: setup.direction.d(),
        entry: if entering_now { spot } else { setup.avg_entry },
        sl: setup.sl.price,
        t1: setup.t1.price,
        t2: setup.t2.price,
        hold,
        jump: earnings.and_then(|e| Some((e.day(today, hold)?, e.sd?))),
    }
}

/// Share of random paths on which T1 comes before the stop within the plan's time stop: the same
/// paths [`rank`] prices contracts on, so a report shows one number for it.
pub fn random_hit_rate(
    setup: &Setup,
    spot: f64,
    sim_vol: f64,
    earnings: Option<Event>,
    now: DateTime<Utc>,
    r: f64,
) -> f64 {
    let plan = plan_of(setup, spot, earnings, now.with_timezone(&New_York).date_naive());
    share(&simulate(&plan, &shocks(PATHS, plan.hold), daily_drift(r), sim_vol / 252f64.sqrt()).1)
}

/// Drift per trading day of a stock that moves at random. Options are priced with the rate `r`,
/// which puts the forward above spot; a stock with no drift at all would make every call lose and
/// every put gain by the interest on its strike, tilting long plans against short ones. Drifting
/// at `r` is the no-edge case: a fairly priced option then averages zero.
fn daily_drift(r: f64) -> f64 {
    r / 252.0
}

/// IV that reprices `c`'s mid with the underlying at `quote_spot`, the price its quote was taken
/// at; the provider's IV when the mid is outside what the model can price.
fn quoted_iv(c: &OptionContract, quote_spot: f64, now: DateTime<Utc>, r: f64) -> f64 {
    // Without a two-sided quote the "mid" is the last trade, from an unknown time and price.
    if !two_sided(c) {
        return c.iv;
    }
    bs::implied_vol(c.right, c.mid(), quote_spot, c.strike, bs::years_to_expiry(c.expiry, now), r, 0.0)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(c.iv)
}

fn two_sided(c: &OptionContract) -> bool {
    c.bid > 0.0 && c.ask > 0.0
}

#[allow(clippy::too_many_arguments)]
pub fn rank(
    chain: &ChainSnapshot,
    setup: &Setup,
    spot: f64,
    sim_vol: f64,
    earnings: Option<Event>,
    p: &ContractParams,
    now: DateTime<Utc>,
    r: f64,
    live: &std::collections::HashSet<String>,
) -> ContractsReport {
    let d = setup.direction.d();
    let today = now.with_timezone(&New_York).date_naive();
    let entering_now = setup.status == super::setup::Status::Enter;
    // The plan's levels are prices; only the entry moves when entering now.
    let plan = plan_of(setup, spot, earnings, today);
    let entry = plan.entry;
    let time_stop = trading_to_date(today, plan.hold as f64);
    // Expiries must outlive the time stop by a week, so no leg is in its last-week decay.
    let min_expiry = time_stop + Duration::days(7);
    let max_expiry = min_expiry + Duration::days(((plan.hold as f64 * CAL_PER_TRADING) as i64).max(45));
    let expirations: Vec<NaiveDate> =
        chain.expirations().into_iter().filter(|e| *e >= min_expiry && *e <= max_expiry).take(4).collect();
    let mut warnings = Vec::new();
    if expirations.is_empty() {
        warnings.push(format!("no expiration between {min_expiry} and {max_expiry} in the chain"));
    }

    let liquid = |c: &OptionContract| {
        c.bid > 0.0
            && c.ask > 0.0
            && c.iv > 0.0
            && c.open_interest >= p.min_oi
            && ((c.ask - c.bid) / c.mid() <= p.max_spread || c.ask - c.bid <= 0.05)
    };
    let contracts: Vec<&OptionContract> =
        chain.contracts.iter().filter(|c| expirations.contains(&c.expiry) && liquid(c)).collect();
    // Quotes can be older than `spot` (delayed, or from the last close while spot is pre/post
    // market). Each contract is modelled from the price its quote was taken at, with the IV that
    // quote implies there, so the model agrees with the market before anything is simulated.
    let quote_spot = |c: &OptionContract| if live.contains(&c.symbol) { chain.spot } else { chain.quote_spot() };
    let ivs: Vec<f64> = contracts.iter().map(|c| quoted_iv(c, quote_spot(c), now, r)).collect();
    if !expirations.is_empty() && !chain.contracts.iter().any(|c| expirations.contains(&c.expiry) && two_sided(c)) {
        warnings.push(
            "no option in these expirations has a two-sided quote (bid and ask are empty before the open): contracts cannot be priced until the session opens or live quotes are ingested (`ox ingest quotes`)"
                .into(),
        );
    }
    let mut cands: Vec<Cand> = Vec::new();
    // Strike distances below are in units of the plan's risk per share.
    let unit = setup.risk_per_share.max(1e-9);
    for exp in &expirations {
        let side = |right: Right| -> Vec<usize> {
            let mut v: Vec<usize> =
                (0..contracts.len()).filter(|&i| contracts[i].expiry == *exp && contracts[i].right == right).collect();
            v.sort_by(|&a, &b| contracts[a].strike.total_cmp(&contracts[b].strike));
            v.dedup_by(|a, b| (contracts[*a].strike - contracts[*b].strike).abs() < 1e-9);
            v
        };
        let (dir_right, credit_right) = if d > 0.0 { (Right::Call, Right::Put) } else { (Right::Put, Right::Call) };
        let mut dl = side(dir_right);
        if d < 0.0 {
            dl.reverse(); // ITM → OTM in the trade's direction
        }
        let k = |i: usize| contracts[i].strike;
        let delta_at_entry = |i: usize| {
            let c = contracts[i];
            bs::greeks(c.right, entry, c.strike, bs::years_to_expiry(c.expiry, now), r, 0.0, ivs[i]).delta.abs()
        };
        let (single, debit_spread, fly) = if d > 0.0 {
            (Structure::LongCall, Structure::CallDebitSpread, Structure::CallButterfly)
        } else {
            (Structure::LongPut, Structure::PutDebitSpread, Structure::PutButterfly)
        };
        for (a, &long) in dl.iter().enumerate() {
            let del = delta_at_entry(long);
            if (0.25..=0.85).contains(&del) {
                cands.push(Cand { structure: single, legs: vec![(long, Side::Buy, 1)] });
            }
            if (0.35..=0.85).contains(&del) {
                // Short strike around T1 or T2 (where the plan takes profit), or anything between.
                for &short in dl.iter().skip(a + 1) {
                    let ks = k(short);
                    if d * (ks - plan.t1) < -0.75 * unit || d * (ks - plan.t2) > 0.5 * unit {
                        continue;
                    }
                    cands.push(Cand {
                        structure: debit_spread,
                        legs: vec![(long, Side::Buy, 1), (short, Side::Sell, 1)],
                    });
                }
            }
        }
        // Butterflies centered on T1 and T2: the payoff peaks where the plan expects to be.
        for target in [plan.t1, plan.t2] {
            let Some(c) = dl.iter().position(|&i| {
                (k(i) - target).abs() == dl.iter().map(|&j| (k(j) - target).abs()).fold(f64::MAX, f64::min)
            }) else {
                continue;
            };
            for w in 1..=4 {
                if c >= w && c + w < dl.len() {
                    cands.push(Cand {
                        structure: fly,
                        legs: vec![(dl[c - w], Side::Buy, 1), (dl[c], Side::Sell, 2), (dl[c + w], Side::Buy, 1)],
                    });
                }
            }
        }
        // Credit spreads on the other side: short strike at or beyond the stop, where the plan is
        // already wrong, so the credit is kept unless the thesis fails.
        let mut cl = side(credit_right);
        if credit_right == Right::Put {
            cl.reverse(); // near-the-money first
        }
        let credit = if d > 0.0 { Structure::PutCreditSpread } else { Structure::CallCreditSpread };
        for (a, &short) in cl.iter().enumerate() {
            let ks = k(short);
            let beyond_stop = d * (plan.sl - ks); // ≥ 0: strike past the stop
            if !(-0.5 * unit..=2.0 * unit).contains(&beyond_stop) {
                continue;
            }
            // Wings no wider than the plan's risk: past that the spread risks many times the
            // credit (and illiquid strikes in between can leave a gap of hundreds of dollars).
            for &long in cl.iter().skip(a + 1).take(4).filter(|&&l| (k(l) - ks).abs() <= unit) {
                cands.push(Cand { structure: credit, legs: vec![(short, Side::Sell, 1), (long, Side::Buy, 1)] });
            }
        }
    }

    // The stock moving at random: no drift beyond the rate the options are priced with.
    let sigma_d = sim_vol / 252f64.sqrt();
    let shocks = shocks(PATHS, plan.hold);
    let (paths, hit) = simulate(&plan, &shocks, daily_drift(r), sigma_d);
    let p0 = share(&hit);
    let avg_stop_fill = avg_stop_fill(&plan, &paths);
    if let Some(fill) = avg_stop_fill {
        let r_lost = (entry - fill) / (entry - plan.sl);
        if r_lost > 1.1 {
            warnings.push(format!(
                "the stop is a daily close beyond {:.2}, which fills past it: at {fill:.2} on average at random, a loss of {r_lost:.1}R rather than 1R",
                plan.sl
            ));
        }
    }

    // Timestamps per simulated day and per contract expiry, so pricing in the loop is pure math.
    const YEAR: f64 = 365.0 * 86_400.0;
    let days: Vec<(NaiveDate, f64)> = (0..=plan.hold)
        .map(|k| {
            let date = trading_to_date(today, k as f64);
            (date, if k == 0 { now.timestamp() as f64 } else { at_close(date).timestamp() as f64 })
        })
        .collect();
    let expiry_ts: Vec<f64> = contracts.iter().map(|c| at_close(c.expiry).timestamp() as f64).collect();
    let iv_after: Vec<f64> =
        contracts.iter().zip(&ivs).map(|(c, &iv)| earnings.map_or(iv, |e| e.iv_after(iv, c.expiry, now))).collect();
    let value_at = |i: usize, s: f64, day: usize| -> f64 {
        let c = contracts[i];
        let (date, ts) = days[day.min(plan.hold)];
        let iv = match earnings {
            Some(e) if date > e.date && c.expiry > e.date && today <= e.date => iv_after[i],
            _ => ivs[i],
        };
        bs::value(c.right, s, c.strike, (expiry_ts[i] - ts) / YEAR, r, iv)
    };
    let half_spread = |i: usize| (contracts[i].ask - contracts[i].bid) / 2.0;

    let evaluated = cands.len();
    let mut picks: Vec<Pick> = Vec::new();
    for cand in &cands {
        let sign = |s: Side| if s == Side::Buy { 1.0 } else { -1.0 };
        let qty = |q: u32| q as f64;
        // Entry: the quoted mid adjusted by the model's change from the price the quotes were
        // taken at to the entry price.
        let net_mid_now: f64 = cand.legs.iter().map(|&(i, s, q)| sign(s) * qty(q) * contracts[i].mid()).sum();
        let model = |s: f64| -> f64 { cand.legs.iter().map(|&(i, sd, q)| sign(sd) * qty(q) * value_at(i, s, 0)).sum() };
        let model_at_quote: f64 =
            cand.legs.iter().map(|&(i, sd, q)| sign(sd) * qty(q) * value_at(i, quote_spot(contracts[i]), 0)).sum();
        // The model's own entry value is the cost basis of every simulated exit: where the model
        // cannot match a quote, the gap between them must not show up as profit or loss.
        let model_entry = model(entry);
        let net_at_entry = net_mid_now + model_entry - model_at_quote;
        let contracts_n: f64 = cand.legs.iter().map(|&(_, _, q)| qty(q)).sum();
        let spread_cost: f64 = cand.legs.iter().map(|&(i, _, q)| qty(q) * half_spread(i)).sum::<f64>() * 100.0;
        let friction = 2.0 * spread_cost + 2.0 * contracts_n * p.commission;

        let strikes: Vec<f64> = cand.legs.iter().map(|&(i, _, _)| contracts[i].strike).collect();
        let (max_loss, max_gain) = match cand.structure {
            Structure::LongCall | Structure::LongPut => (net_at_entry * 100.0, None),
            Structure::CallDebitSpread | Structure::PutDebitSpread => {
                let w = (strikes[0] - strikes[1]).abs();
                (net_at_entry * 100.0, Some((w - net_at_entry) * 100.0))
            }
            Structure::PutCreditSpread | Structure::CallCreditSpread => {
                let w = (strikes[0] - strikes[1]).abs();
                ((w + net_at_entry) * 100.0, Some(-net_at_entry * 100.0))
            }
            Structure::CallButterfly | Structure::PutButterfly => {
                let w = (strikes[1] - strikes[0]).abs();
                (net_at_entry * 100.0, Some((w - net_at_entry) * 100.0))
            }
        };
        if max_loss <= 0.0 || max_gain.is_some_and(|g| g <= 0.0) || p.max_risk.is_some_and(|m| max_loss > m) {
            continue;
        }
        let exit_value = |e: &Exit| -> f64 {
            cand.legs.iter().map(|&(i, sd, q)| sign(sd) * qty(q) * value_at(i, e.price, e.day)).sum()
        };
        let path_pnl = |exits: &[Exit]| -> f64 {
            exits.iter().map(|e| e.frac * (exit_value(e) - model_entry) * 100.0).sum::<f64>() - friction
        };
        let pnls: Vec<f64> = paths.iter().map(|x| path_pnl(x)).collect();
        let ev_no_edge = pnls.iter().sum::<f64>() / pnls.len() as f64;
        let p_profit = pnls.iter().filter(|x| **x > 0.0).count() as f64 / pnls.len() as f64;
        let mean_where = |want: bool| -> Option<f64> {
            let v: Vec<f64> = pnls.iter().zip(&hit).filter(|(_, h)| **h == want).map(|(x, _)| *x).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        };
        let (ev_if_t1, ev_if_not) = (mean_where(true).unwrap_or(0.0), mean_where(false).unwrap_or(0.0));
        // A structure that loses money even when T1 comes first is never the pick.
        if ev_if_t1 <= 0.0 {
            continue;
        }
        let breakeven_p_t1 = breakeven(ev_if_t1, ev_if_not);
        let scen = |price: f64, day: f64| {
            (exit_value(&Exit { day: day.round() as usize, price, frac: 1.0 }) - model_entry) * 100.0 - friction
        };
        let half_t1 = (setup.days_to_t1 / 2.0).max(1.0);
        let t_now = |i: usize| bs::years_to_expiry(contracts[i].expiry, now);
        let (mut delta, mut theta, mut vega) = (0.0, 0.0, 0.0);
        for &(i, sd, q) in &cand.legs {
            let c = contracts[i];
            let g = bs::greeks(c.right, entry, c.strike, t_now(i), r, 0.0, ivs[i]);
            delta += sign(sd) * qty(q) * g.delta * 100.0;
            theta += sign(sd) * qty(q) * g.theta * 100.0;
            vega += sign(sd) * qty(q) * g.vega * 100.0;
        }
        let legs: Vec<Leg> = cand
            .legs
            .iter()
            .map(|&(i, s, _)| {
                let c = contracts[i];
                Leg {
                    symbol: c.symbol.clone(),
                    side: s,
                    right: c.right,
                    strike: c.strike,
                    expiry: c.expiry,
                    bid: c.bid,
                    ask: c.ask,
                    mid: c.mid(),
                    iv: ivs[i],
                    delta: c.delta,
                    open_interest: c.open_interest,
                    volume: c.volume,
                }
            })
            .collect();
        let expiry = legs[0].expiry;
        picks.push(Pick {
            structure: cand.structure,
            expiry,
            dte: bs::dte(expiry, now),
            ratios: cand.legs.iter().map(|&(_, _, q)| q).collect(),
            min_oi: legs.iter().map(|l| l.open_interest).fold(f64::MAX, f64::min),
            worst_spread_pct: legs.iter().map(|l| (l.ask - l.bid) / l.mid.max(0.01)).fold(0.0, f64::max),
            live_quotes: legs.iter().all(|l| live.contains(&l.symbol)),
            legs,
            net_mid_now,
            net_at_entry,
            limit_at_entry: (net_at_entry * 100.0).round() / 100.0,
            max_loss,
            max_gain,
            ev_no_edge,
            ev_if_t1,
            ev_if_not,
            breakeven_p_t1,
            edge_needed: breakeven_p_t1 - p0,
            p_profit,
            pnl_at_sl: scen(plan.sl, half_t1),
            pnl_at_t1: scen(plan.t1, setup.days_to_t1),
            pnl_at_t2: scen(plan.t2, setup.days_to_t2),
            pnl_flat: scen(entry, plan.hold as f64),
            delta,
            theta_day: theta,
            vega,
            friction,
            exit_plan: String::new(),
            why: String::new(),
            notes: Vec::new(),
            sizing: None,
        });
    }
    // Least edge needed first; among equals, the one that costs least at random per dollar risked.
    let key = |a: &Pick, b: &Pick| {
        a.edge_needed
            .total_cmp(&b.edge_needed)
            .then((b.ev_no_edge / b.max_loss).total_cmp(&(a.ev_no_edge / a.max_loss)))
    };
    picks.sort_by(key);

    // Variety: the best of each structure first, then the next best overall.
    let mut out: Vec<Pick> = Vec::new();
    for pk in &picks {
        if out.len() < p.limit && !out.iter().any(|x| x.structure == pk.structure) {
            out.push(pk.clone());
        }
    }
    for pk in &picks {
        if out.len() >= p.limit {
            break;
        }
        let dup = out.iter().any(|x| x.legs.iter().map(|l| &l.symbol).eq(pk.legs.iter().map(|l| &l.symbol)));
        if !dup && out.iter().filter(|x| x.structure == pk.structure).count() < 2 {
            out.push(pk.clone());
        }
    }
    out.sort_by(key);
    for pk in &mut out {
        annotate(pk, setup, &plan, time_stop, earnings, sim_vol, now);
    }
    if !live.is_empty() {
        let n = out.iter().filter(|p| p.live_quotes).count();
        warnings.push(format!(
            "{n} of {} picks priced from live broker quotes ({} contracts ingested)",
            out.len(),
            live.len()
        ));
    } else if chain.freshness.quotes_stale {
        warnings.push(format!(
            "option quotes are from the {} close: refresh with live quotes (`ox ingest quotes`) before ordering",
            crate::fmt::time_et(chain.freshness.quotes_as_of)
        ));
    }
    if !entering_now {
        warnings.push(format!(
            "prices are estimated for when the underlying reaches {entry:.2} with today's IVs; each day of waiting costs the theta shown"
        ));
    }

    ContractsReport {
        entry,
        sim_vol,
        p_t1_no_edge: p0,
        avg_stop_fill,
        time_stop,
        min_expiry,
        expirations,
        evaluated,
        picks: out,
        warnings,
    }
}

/// One leg of a position already held: a chain contract, or the stock itself (`None`), with its
/// signed size in underlying units (contracts × multiplier, or shares).
pub struct HeldLeg<'a> {
    pub contract: Option<&'a OptionContract>,
    pub units: f64,
}

/// Holding a position through the plan versus closing it now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldEval {
    /// Value of the position now at mid, $.
    pub value_now: f64,
    /// Expected change in value from holding through the plan's exits instead of closing now, $,
    /// if the stock moves at random (no drift beyond the risk-free rate). Both pay the exit spread once, so it nets out.
    pub ev_no_edge: f64,
    /// Share of random paths that end above today's value.
    pub p_better: f64,
    /// Trading days simulated: the plan's time stop or the first expiry, whichever comes first.
    pub horizon_days: usize,
    pub horizon_end: NaiveDate,
    pub theta_day: f64,
    /// Half the bid/ask spread plus commission to close now, $.
    pub close_friction: f64,
}

/// Simulate a held position under the plan's management from today's spot (same random paths
/// and pricing as [`rank`]), capped at the first expiry. `quote_spot` is the underlying price the
/// option quotes were taken at ([`ChainSnapshot::quote_spot`]): when spot has moved since, that
/// move has already happened to the position and is not part of what holding adds. `sim_vol`
/// gives the realized-vol forecast for a horizon in trading days.
#[allow(clippy::too_many_arguments)]
pub fn hold_value(
    legs: &[HeldLeg],
    setup: &Setup,
    spot: f64,
    quote_spot: f64,
    sim_vol: impl Fn(usize) -> f64,
    earnings: Option<Event>,
    now: DateTime<Utc>,
    r: f64,
    commission: f64,
) -> HoldEval {
    let today = now.with_timezone(&New_York).date_naive();
    let first_expiry = legs.iter().filter_map(|l| l.contract.map(|c| c.expiry)).min();
    let to_expiry =
        first_expiry.map_or(usize::MAX, |e| ((e - today).num_days().max(0) as f64 / CAL_PER_TRADING).floor() as usize);
    let hold = setup.backtest.hold_days.min(to_expiry).max(1);
    let jump = earnings.and_then(|e| Some((e.day(today, hold)?, e.sd?)));
    let plan = Plan {
        d: setup.direction.d(),
        entry: spot,
        sl: setup.sl.price,
        t1: setup.t1.price,
        t2: setup.t2.price,
        hold,
        jump,
    };
    let sigma_d = sim_vol(hold) / 252f64.sqrt();
    let shocks = shocks(PATHS, plan.hold);
    let (paths, _) = simulate(&plan, &shocks, daily_drift(r), sigma_d);

    let ivs: Vec<f64> = legs.iter().map(|l| l.contract.map_or(0.0, |c| quoted_iv(c, quote_spot, now, r))).collect();
    let leg_value = |k: usize, s: f64, day: usize| -> f64 {
        let Some(c) = legs[k].contract else { return s };
        let date = trading_to_date(today, day as f64);
        let ts = if day == 0 { now } else { at_close(date) };
        let iv = match earnings {
            Some(e) if date > e.date && c.expiry > e.date && today <= e.date => e.iv_after(ivs[k], c.expiry, now),
            _ => ivs[k],
        };
        let t = (at_close(c.expiry) - ts).num_seconds() as f64 / (365.0 * 86_400.0);
        bs::value(c.right, s, c.strike, t, r, iv)
    };
    let model = |s: f64, day: usize| -> f64 { (0..legs.len()).map(|k| legs[k].units * leg_value(k, s, day)).sum() };
    // The model's value at today's spot is what closing now is compared against. The reported
    // value is each quoted mid carried from the quotes' price to spot by the model (the model
    // value itself where there is no two-sided quote).
    let model_now = model(spot, 0);
    let value_now: f64 = (0..legs.len())
        .map(|k| {
            let at_spot = leg_value(k, spot, 0);
            let quoted = legs[k].contract.filter(|c| two_sided(c)).map(|c| c.mid() - leg_value(k, quote_spot, 0));
            legs[k].units * (at_spot + quoted.unwrap_or(0.0))
        })
        .sum();
    let path_value = |exits: &[Exit]| -> f64 { exits.iter().map(|e| e.frac * model(e.price, e.day)).sum() };
    let changes: Vec<f64> = paths.iter().map(|x| path_value(x) - model_now).collect();
    let ev_no_edge = changes.iter().sum::<f64>() / changes.len() as f64;
    let mut theta_day = 0.0;
    let mut close_friction = 0.0;
    for (l, &iv) in legs.iter().zip(&ivs) {
        if let Some(c) = l.contract {
            theta_day +=
                l.units * bs::greeks(c.right, spot, c.strike, bs::years_to_expiry(c.expiry, now), r, 0.0, iv).theta;
            close_friction += l.units.abs() * (c.ask - c.bid) / 2.0 + (l.units.abs() / 100.0) * commission;
        }
    }
    HoldEval {
        value_now,
        ev_no_edge,
        p_better: changes.iter().filter(|x| **x > 0.0).count() as f64 / changes.len() as f64,
        horizon_days: hold,
        horizon_end: trading_to_date(today, hold as f64).min(first_expiry.unwrap_or(NaiveDate::MAX)),
        theta_day,
        close_friction,
    }
}

/// Share of the time the winning branch must happen for the two branch means to net to zero.
/// 0 when the structure earns even on the losing branch, 1 when it cannot break even.
fn breakeven(ev_if_t1: f64, ev_if_not: f64) -> f64 {
    if ev_if_not >= 0.0 {
        0.0
    } else if ev_if_t1 <= 0.0 {
        1.0
    } else {
        -ev_if_not / (ev_if_t1 - ev_if_not)
    }
}

fn annotate(
    pk: &mut Pick,
    setup: &Setup,
    plan: &Plan,
    time_stop: NaiveDate,
    earnings: Option<Event>,
    sim_vol: f64,
    now: DateTime<Utc>,
) {
    let px = |x: f64| format!("{x:.2}");
    let close_word = if plan.d > 0.0 { "below" } else { "above" };
    pk.exit_plan = format!(
        "close half when the underlying reaches T1 {} and the rest at T2 {}; after T1, exit the rest at entry {}; close everything on a daily close {close_word} {} or on {time_stop}",
        px(plan.t1),
        px(plan.t2),
        px(plan.entry),
        px(plan.sl)
    );
    let buy_iv = pk.legs.iter().filter(|l| l.side == Side::Buy).map(|l| l.iv).fold(0.0, f64::max);
    let sell_iv = pk.legs.iter().filter(|l| l.side == Side::Sell).map(|l| l.iv).fold(0.0, f64::max);
    pk.why = match pk.structure {
        Structure::LongCall | Structure::LongPut => format!(
            "simplest expression; uncapped past T2; pays {:.0}% IV vs {:.0}% forecast realized",
            buy_iv * 100.0,
            sim_vol * 100.0
        ),
        Structure::CallDebitSpread | Structure::PutDebitSpread => format!(
            "short strike {} sits at the plan's profit zone, selling {:.0}% IV to cut cost; gains capped beyond it",
            pk.legs[1].strike,
            sell_iv * 100.0
        ),
        Structure::CallButterfly | Structure::PutButterfly => format!(
            "peaks at {} where the plan expects to exit; cheap, but needs price near the body late in the trade",
            pk.legs[1].strike
        ),
        Structure::PutCreditSpread | Structure::CallCreditSpread => format!(
            "short strike {} is at/beyond the stop: keeps the credit unless the thesis fails; benefits from time and from IV ({:.0}%) above realized",
            pk.legs[0].strike,
            sell_iv * 100.0
        ),
    };
    if pk.ev_no_edge < -0.15 * pk.max_loss {
        pk.notes.push(format!("costly at random: {:+.0} expected with no edge", pk.ev_no_edge));
    }
    if pk.edge_needed <= 0.0 {
        pk.notes.push(format!(
            "pays at random only if realized volatility comes in at the {:.0}% forecast, which ox has not verified",
            sim_vol * 100.0
        ));
    }
    if pk.structure.is_credit() && pk.max_loss > 3.0 * pk.max_gain.unwrap_or(0.0) {
        pk.notes.push("risks more than 3× the credit".into());
    }
    if let Some(e) = earnings {
        if pk.expiry > e.date && e.date <= time_stop {
            let leg = &pk.legs[0];
            let after = e.iv_after(leg.iv, leg.expiry, now);
            pk.notes.push(match e.sd {
                Some(sd) => format!(
                    "holds through earnings {}: simulated with a ±{:.1}% report move; {} IV {:.0}% → {:.0}% after",
                    e.date.format("%m-%d"),
                    sd * 100.0,
                    leg.strike,
                    leg.iv * 100.0,
                    after * 100.0
                ),
                None => format!("holds through earnings {}: gap risk not modelled (no move estimate), IV assumed to drop {:.0}% after", e.date.format("%m-%d"), (1.0 - EARNINGS_CRUSH) * 100.0),
            });
        }
    }
    if pk.worst_spread_pct > 0.08 {
        pk.notes.push(format!("wide market: worst leg {:.0}% of mid", pk.worst_spread_pct * 100.0));
    }
    if pk.friction > 0.1 * pk.max_loss {
        pk.notes.push(format!("friction ${:.0} is {:.0}% of the risk", pk.friction, pk.friction / pk.max_loss * 100.0));
    }
    let _ = setup;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan { d: 1.0, entry: 100.0, sl: 95.0, t1: 110.0, t2: 120.0, hold: 40, jump: None }
    }

    #[test]
    fn breakeven_rate() {
        // Win 300, lose 100: one win pays for three losses.
        assert!((breakeven(300.0, -100.0) - 0.25).abs() < 1e-12);
        assert_eq!(breakeven(50.0, 10.0), 0.0);
        assert_eq!(breakeven(-5.0, -100.0), 1.0);
    }

    #[test]
    fn random_walk_hit_rate_matches_the_distances() {
        // Driftless: T1 before the stop about stop / (stop + t1) of the time, where the stop's
        // distance is to where a stop on the close fills: past SL by 0.58 of a day's move.
        let p = Plan { hold: 250, ..plan() };
        let sigma_d = 0.4 / 252f64.sqrt();
        let (paths, hit) = simulate(&p, &super::shocks(20_000, p.hold), 0.0, sigma_d);
        let past = 0.5826 * sigma_d * p.sl;
        let got = share(&hit);
        assert!((got - (5.0 + past) / (15.0 + past)).abs() < 0.02, "{got}");
        let fill = avg_stop_fill(&p, &paths).unwrap();
        assert!((fill - (p.sl - past)).abs() < 0.15, "{fill} vs {}", p.sl - past);
    }

    #[test]
    fn earnings_jump_widens_outcomes() {
        // No barriers within reach: the time-stop price carries the whole distribution.
        let base = Plan { d: 1.0, entry: 100.0, sl: 1.0, t1: 1e9, t2: 2e9, hold: 20, jump: None };
        let jumpy = Plan { jump: Some((5, 0.10)), ..base };
        let shocks = super::shocks(4000, base.hold);
        let sd = 0.01;
        let spread = |p: &Plan| {
            let (paths, _) = simulate(p, &shocks, 0.0, sd);
            let x: Vec<f64> = paths.iter().map(|e| e.last().unwrap().price.ln()).collect();
            let m = x.iter().sum::<f64>() / x.len() as f64;
            (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
        };
        let (a, b) = (spread(&base), spread(&jumpy));
        // √(20·0.01²) ≈ 0.045 without; √(0.045² + 0.1²) ≈ 0.11 with the report.
        assert!((a - 0.045).abs() < 0.005 && (b - 0.11).abs() < 0.01, "{a} {b}");
        let e = Event { date: NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(), sd: Some(0.1) };
        // Mon 5 Jan → report Fri 9 Jan: Tue–Fri are sessions 1–4, Mon 12 is the reaction (5).
        assert_eq!(e.day(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), 20), Some(5));
        assert_eq!(e.day(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), 4), None);
    }

    #[test]
    fn exits_sum_to_whole_position() {
        let p = plan();
        let mut rng = Rng(11);
        let shocks: Vec<Vec<f64>> = (0..500).map(|_| rng.normals(p.hold + 1)).collect();
        let (paths, _) = simulate(&p, &shocks, 0.001, 0.025);
        for ex in paths {
            let total: f64 = ex.iter().map(|e| e.frac).sum();
            assert!((total - 1.0).abs() < 1e-9, "{ex:?}");
        }
    }

    fn contract(strike: f64, iv: f64, expiry: NaiveDate) -> OptionContract {
        OptionContract {
            symbol: "X".into(),
            root: "X".into(),
            prev_close: 0.0,
            expiry,
            strike,
            right: Right::Call,
            bid: 0.0,
            ask: 0.0,
            last: 0.0,
            volume: 0.0,
            open_interest: 1000.0,
            iv,
            delta: 0.5,
            gamma: 0.0,
            theta: 0.0,
            vega: 0.0,
        }
    }

    #[test]
    fn holding_pays_for_rich_iv_without_edge() {
        use super::super::setup::{Backtest, Direction, Outcomes, PlanLevel, SetupKind, Status};
        let now = at_close(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        let lvl = |p: f64| PlanLevel { price: p, basis: String::new(), zone: None };
        let bt = Backtest {
            signals: Outcomes::default(),
            first_signal: None,
            last_signal: None,
            no_edge_p_t1: 0.33,
            trend_only: Outcomes::default(),
            any_day: Outcomes::default(),
            effective_n: 0.0,
            hold_days: 30,
            through_earnings: Outcomes::default(),
            without_earnings: Outcomes::default(),
        };
        let setup = Setup {
            kind: SetupKind::TrendNow,
            direction: Direction::Long,
            status: Status::Enter,
            name: String::new(),
            trigger: String::new(),
            b1: lvl(100.0),
            b2: None,
            sl: lvl(90.0),
            t1: lvl(120.0),
            t2: lvl(130.0),
            avg_entry: 100.0,
            risk_per_share: 10.0,
            rr_t1: 2.0,
            rr_t2: 3.0,
            p_t1_random: 0.33,
            obstacles: vec![],
            backtest: bt,
            days_to_t1: 20.0,
            days_to_t2: 25.0,
            why: vec![],
            why_not_now: vec![],
            alerts: vec![],
        };
        let exp = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
        let price = |iv: f64| bs::value(Right::Call, 100.0, 105.0, bs::years_to_expiry(exp, now), 0.04, iv);
        // Paid 50% IV while realized runs at 30%: holding loses in expectation; the reverse gains.
        let mut rich = contract(105.0, 0.5, exp);
        (rich.bid, rich.ask) = (price(0.5), price(0.5));
        let h = hold_value(
            &[HeldLeg { contract: Some(&rich), units: 100.0 }],
            &setup,
            100.0,
            100.0,
            |_| 0.3,
            None,
            now,
            0.04,
            0.65,
        );
        assert!(h.ev_no_edge < 0.0, "{h:?}");
        let mut cheap = contract(105.0, 0.2, exp);
        (cheap.bid, cheap.ask) = (price(0.2), price(0.2));
        let h = hold_value(
            &[HeldLeg { contract: Some(&cheap), units: 100.0 }],
            &setup,
            100.0,
            100.0,
            |_| 0.4,
            None,
            now,
            0.04,
            0.65,
        );
        assert!(h.ev_no_edge > 0.0, "{h:?}");
        assert_eq!(h.horizon_days, 30);
    }

    #[test]
    fn normals_are_standard() {
        let v = Rng(3).normals(20000);
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let s = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        assert!(m.abs() < 0.03 && (s - 1.0).abs() < 0.03, "{m} {s}");
    }
}

/// What the simulation must get right whatever the plan: a fairly priced contract needs no edge,
/// and nothing but the plan and the prices paid moves the result.
#[cfg(test)]
mod consistency {
    use super::super::setup::{Backtest, Direction, Outcomes, PlanLevel, Setup, SetupKind, Status};
    use super::*;
    use crate::analytics::market::{Freshness, Session};

    fn setup(dir: Direction, sl: f64, t1: f64, t2: f64) -> Setup {
        let lvl = |p: f64| PlanLevel { price: p, basis: String::new(), zone: None };
        let bt = Backtest {
            signals: Outcomes::default(),
            first_signal: None,
            last_signal: None,
            no_edge_p_t1: 0.33,
            trend_only: Outcomes::default(),
            any_day: Outcomes::default(),
            effective_n: 0.0,
            hold_days: 30,
            through_earnings: Outcomes::default(),
            without_earnings: Outcomes::default(),
        };
        Setup {
            kind: SetupKind::TrendNow,
            direction: dir,
            status: Status::Enter,
            name: String::new(),
            trigger: String::new(),
            b1: lvl(100.0),
            b2: None,
            sl: lvl(sl),
            t1: lvl(t1),
            t2: lvl(t2),
            avg_entry: 100.0,
            risk_per_share: (100.0 - sl).abs(),
            rr_t1: 2.0,
            rr_t2: 4.0,
            p_t1_random: 0.33,
            obstacles: vec![],
            backtest: bt,
            days_to_t1: 10.0,
            days_to_t2: 20.0,
            why: vec![],
            why_not_now: vec![],
            alerts: vec![],
        }
    }
    fn long() -> Setup {
        setup(Direction::Long, 95.0, 110.0, 120.0)
    }
    fn short() -> Setup {
        setup(Direction::Short, 105.0, 90.0, 80.0)
    }
    fn now() -> DateTime<Utc> {
        at_close(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap())
    }
    fn expiry() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap()
    }

    /// Strikes 60–140 whose quotes are exact model prices at `quoted_at` and 40% IV (no interest),
    /// while the provider's IV field says `iv_field`.
    fn chain(quoted_at: f64, iv_field: f64) -> ChainSnapshot {
        chain_at_rate(quoted_at, iv_field, 0.0)
    }

    fn chain_at_rate(quoted_at: f64, iv_field: f64, r: f64) -> ChainSnapshot {
        let t = bs::years_to_expiry(expiry(), now());
        let mut contracts = Vec::new();
        for i in 0..=32 {
            let k = 60.0 + 2.5 * i as f64;
            for right in [Right::Call, Right::Put] {
                let px = bs::greeks(right, quoted_at, k, t, r, 0.0, 0.40).price;
                contracts.push(OptionContract {
                    symbol: format!("X{k}{right:?}"),
                    root: "X".into(),
                    prev_close: 0.0,
                    expiry: expiry(),
                    strike: k,
                    right,
                    bid: px,
                    ask: px,
                    last: px,
                    volume: 0.0,
                    open_interest: 1000.0,
                    iv: iv_field,
                    delta: 0.5,
                    gamma: 0.0,
                    theta: 0.0,
                    vega: 0.0,
                });
            }
        }
        ChainSnapshot {
            underlying: "X".into(),
            spot: quoted_at,
            prev_close: quoted_at,
            iv30: None,
            as_of: 0,
            source: String::new(),
            delayed: true,
            freshness: Freshness { session: Session::Closed, quotes_stale: false, quotes_as_of: 0, spot_as_of: 0 },
            regular_close: None,
            contracts,
        }
    }

    /// Every structure, at 40% realized vol, no interest and no costs.
    fn picks(setup: &Setup, chain: &ChainSnapshot, spot: f64) -> ContractsReport {
        picks_at_rate(setup, chain, spot, 0.0)
    }

    fn picks_at_rate(setup: &Setup, chain: &ChainSnapshot, spot: f64, r: f64) -> ContractsReport {
        let p = ContractParams { limit: 400, min_oi: 1.0, max_spread: 1.0, max_risk: None, commission: 0.0 };
        rank(chain, setup, spot, 0.40, None, &p, now(), r, &Default::default())
    }

    /// Expected result within 1.5% of the risk, and where the result turns on T1 against the stop,
    /// within two points of edge. (The broken versions of these cases were off by 6 to 12 points.)
    /// Simulation noise is about a point; structures whose result barely depends on which level
    /// comes first (butterflies, credit spreads) turn a dollar of it into a point of edge.
    fn assert_no_edge(rep: &ContractsReport) {
        assert!(rep.picks.len() >= 6, "{} picks", rep.picks.len());
        for p in &rep.picks {
            let turns_on_t1 = p.ev_if_t1 - p.ev_if_not > 0.5 * p.max_loss;
            assert!(
                p.ev_no_edge.abs() < 0.015 * p.max_loss.max(100.0) && (!turns_on_t1 || p.edge_needed.abs() < 0.02),
                "{} {:?}: {:+.1} expected on {:.0} risked, edge {:+.2}pt",
                p.structure.label(),
                p.legs.iter().map(|l| l.strike).collect::<Vec<_>>(),
                p.ev_no_edge,
                p.max_loss,
                p.edge_needed * 100.0
            );
        }
    }

    #[test]
    fn a_fairly_priced_chain_needs_no_edge() {
        assert_no_edge(&picks(&long(), &chain(100.0, 0.40), 100.0));
        assert_no_edge(&picks(&short(), &chain(100.0, 0.40), 100.0));
    }

    #[test]
    fn interest_does_not_favor_puts_over_calls() {
        // With a 4% rate the forward is above spot. A stock with no drift at all would make calls
        // lose and puts gain; drifting at the rate, neither does.
        let ch = chain_at_rate(100.0, 0.40, 0.04);
        assert_no_edge(&picks_at_rate(&long(), &ch, 100.0, 0.04));
        assert_no_edge(&picks_at_rate(&short(), &ch, 100.0, 0.04));
    }

    #[test]
    fn the_providers_iv_does_not_move_the_result() {
        // Quotes are worth 40% IV whatever the provider's own IV field says.
        assert_no_edge(&picks(&long(), &chain(100.0, 0.43), 100.0));
        assert_no_edge(&picks(&short(), &chain(100.0, 0.36), 100.0));
    }

    #[test]
    fn a_move_since_the_quotes_is_not_edge() {
        // Quotes from the close at 100; spot is 102 before the open.
        let mut stale = chain(100.0, 0.40);
        stale.freshness.quotes_stale = true;
        stale.regular_close = Some(100.0);
        stale.spot = 101.0;
        let rep = picks(&long(), &stale, 102.0);
        assert_no_edge(&rep);
        // And the entry is priced as it would be quoted with the underlying at 102.
        let fresh = picks(&long(), &chain(102.0, 0.40), 102.0);
        for (a, b) in rep.picks.iter().zip(&fresh.picks) {
            assert_eq!(a.legs[0].symbol, b.legs[0].symbol);
            assert!((a.net_at_entry - b.net_at_entry).abs() < 0.01, "{} vs {}", a.net_at_entry, b.net_at_entry);
        }
    }

    #[test]
    fn the_stock_itself_averages_nothing_through_a_report() {
        for d in [1.0, -1.0] {
            let p = Plan {
                d,
                entry: 100.0,
                sl: 100.0 - 5.0 * d,
                t1: 100.0 + 10.0 * d,
                t2: 100.0 + 20.0 * d,
                hold: 30,
                jump: Some((5, 0.10)),
            };
            let (paths, _) = simulate(&p, &shocks(40_000, p.hold), 0.0, 0.40 / 252f64.sqrt());
            let pnl = |x: &Vec<Exit>| x.iter().map(|e| e.frac * d * (e.price - 100.0)).sum::<f64>();
            let mean_r = paths.iter().map(pnl).sum::<f64>() / paths.len() as f64 / 5.0;
            assert!(mean_r.abs() < 0.03, "{mean_r:+.3}R");
            // The gap can end a trade beyond its stop.
            let worst = paths.iter().map(pnl).fold(f64::MAX, f64::min) / 5.0;
            assert!(worst < -1.5, "{worst}");
        }
    }

    #[test]
    fn holding_is_not_charged_for_a_move_that_already_happened() {
        // 100 strike call marked at the close with the stock at 100; the stock is 96 before the open.
        let mut ch = chain(100.0, 0.43);
        ch.freshness.quotes_stale = true;
        ch.regular_close = Some(100.0);
        let c = ch.contracts.iter().find(|c| c.strike == 100.0 && c.right == Right::Call).unwrap();
        let s = setup(Direction::Long, 91.0, 106.0, 116.0);
        let h = hold_value(
            &[HeldLeg { contract: Some(c), units: 100.0 }],
            &s,
            96.0,
            ch.quote_spot(),
            |_| 0.40,
            None,
            now(),
            0.0,
            0.0,
        );
        let worth = 100.0 * bs::value(Right::Call, 96.0, 100.0, bs::years_to_expiry(expiry(), now()), 0.0, 0.40);
        assert!((h.value_now - worth).abs() < 1.0, "{} vs {worth}", h.value_now);
        assert!(h.ev_no_edge.abs() < 0.02 * worth, "{:+.1} on {worth:.0}", h.ev_no_edge);
    }

    #[test]
    fn one_random_hit_rate_per_plan() {
        let rep = picks(&long(), &chain(100.0, 0.40), 100.0);
        assert_eq!(random_hit_rate(&long(), 100.0, 0.40, None, now(), 0.0), rep.p_t1_no_edge);
    }
}
