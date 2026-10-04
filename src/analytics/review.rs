//! `ox review`: should each holding stay, and is something better waiting for its capital?
//!
//! Every holding is judged like a fresh trade: `pick` in the holding's direction, then the held
//! contracts are simulated through that plan against closing them now ([`HoldEval`]). Sunk cost
//! does not enter; only what holding is expected to add from here.
//!
//! - HOLD: the direction has a measured edge and these contracts capture it, or there is no edge
//!   but holding costs little in expectation (the user's own thesis is then the only reason).
//! - CLOSE: no measured edge, and holding is expected to lose value (theta and IV above the
//!   realized-vol forecast) by the plan's horizon.
//! - ROLL: the edge is there but these contracts don't capture it (e.g. they expire before the
//!   plan's targets are due); the best-fitting contract is attached.
//!
//! Replacements pair an ENTER candidate that the budget blocks (slots, liquidity floor) with the
//! weakest holding whose closing would fund it, and only when the swap is expected to pay after
//! friction.

use serde::{Deserialize, Serialize};

use super::pick::contracts::{HoldEval, Pick};
use super::pick::earnings::EarningsMove;
use super::pick::setup::Direction;
use super::pick::Verdict;
use super::sizing::{self, Budget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Hold,
    Close,
    Roll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub underlying: String,
    /// Position labels, e.g. `MU 26-10-16 1200C`.
    pub positions: Vec<String>,
    pub direction: Direction,
    pub spot: f64,
    /// Market value at mid and unrealized P&L, USD.
    pub market_value: f64,
    pub unrealized: f64,
    pub min_dte: Option<i64>,
    /// `pick` in the holding's direction.
    pub verdict: Verdict,
    pub headline: String,
    pub setup: Option<String>,
    pub has_edge: bool,
    /// The next report, if one is ahead (date, implied and past moves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earnings_move: Option<EarningsMove>,
    pub hold: Option<HoldEval>,
    pub action: Action,
    pub reasons: Vec<String>,
    /// For ROLL: the best contract for the same plan, sized as if this holding were closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roll_to: Option<Pick>,
}

impl Holding {
    /// Expected value of holding that the decision uses: the plan's drift only with a measured
    /// edge, zero drift otherwise.
    pub fn hold_ev(&self) -> f64 {
        self.hold.as_ref().map_or(0.0, |h| if self.has_edge { h.ev_plan } else { h.ev_no_edge })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub symbol: String,
    pub verdict: Verdict,
    pub headline: String,
    pub setup: Option<String>,
    pub robust: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earnings_move: Option<EarningsMove>,
    /// Best contract by EV on risk, sized against the current book.
    pub pick: Option<Pick>,
}

impl Candidate {
    pub fn fits(&self) -> bool {
        self.pick.as_ref().and_then(|p| p.sizing.as_ref()).is_some_and(|s| s.contracts > 0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replacement {
    /// Holding to close.
    pub close: String,
    pub close_positions: Vec<String>,
    pub close_action: Action,
    /// Cash the close frees (mid), and what crossing the spread costs, USD.
    pub proceeds: f64,
    pub close_friction: f64,
    /// Expected value given up by not holding (see [`Holding::hold_ev`]).
    pub hold_ev: f64,
    pub open: String,
    /// The candidate's pick, sized against the book after the close.
    pub open_pick: Pick,
    pub lots: u32,
    /// New position's EV (after its round-trip friction) minus the holding's EV and the close
    /// friction: what the swap is expected to add under the new plan's measured edge.
    pub gain: f64,
    /// The same if the new setup turns out to have no edge.
    pub gain_no_edge: f64,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub as_of: i64,
    pub budget: Option<Budget>,
    pub holdings: Vec<Holding>,
    /// Scanned symbols with a measured edge (ENTER first, then WAIT).
    pub candidates: Vec<Candidate>,
    pub replacements: Vec<Replacement>,
    /// Symbols scanned and found NO TRADE, and failures.
    pub no_trade: Vec<String>,
    pub notes: Vec<String>,
}

/// Holding a position without an edge "costs little" below this loss in expectation.
fn material(value: f64) -> f64 {
    (0.05 * value.abs()).max(10.0)
}

/// HOLD / CLOSE / ROLL for a holding, with reasons. `fresh_best` is the best contract `pick` finds
/// for the same plan today, if any.
pub fn decide(h: &Holding, fresh_best: Option<&Pick>, days_to_t1: f64) -> (Action, Vec<String>) {
    let Some(e) = &h.hold else {
        return (Action::Hold, vec!["contracts not found in the chain: cannot simulate holding".into()]);
    };
    let mut why = Vec::new();
    let horizon = format!("to {} ({} trading days)", e.horizon_end, e.horizon_days);
    if h.has_edge {
        if e.ev_plan > 0.0 {
            why.push(format!(
                "{}: measured edge; holding these contracts is worth {:+.0} expected {horizon}",
                h.headline, e.ev_plan
            ));
            return (Action::Hold, why);
        }
        why.push(format!(
            "the {} plan has an edge but these contracts lose {:+.0} expected {horizon}",
            h.setup.as_deref().unwrap_or("leading"),
            e.ev_plan
        ));
        if (e.horizon_days as f64) < days_to_t1 {
            why.push(format!("they expire before T1 is typically reached (~{days_to_t1:.0} trading days)"));
        }
        if let Some(p) = fresh_best.filter(|p| p.ev > 0.0) {
            why.push(format!("a {} {} fits the plan (EV {:+.0} per lot)", p.structure.label(), p.expiry, p.ev));
            return (Action::Roll, why);
        }
        why.push("no better contract for the plan either: close".into());
        return (Action::Close, why);
    }
    let reason = match h.verdict {
        Verdict::NoTrade => "no setup has a measured edge in this direction".to_string(),
        _ => h.headline.clone(),
    };
    if e.ev_no_edge < -material(e.value_now) {
        why.push(format!(
            "{reason}; without one, holding is expected to lose {:.0} {horizon} (theta {:+.0}/day, IV vs forecast realized vol)",
            -e.ev_no_edge, e.theta_day
        ));
        why.push("hold only for a reason ox cannot see (catalyst, news) and say which".into());
        (Action::Close, why)
    } else {
        why.push(format!(
            "{reason}, but holding costs little in expectation ({:+.0} {horizon}): it rests on your own thesis",
            e.ev_no_edge
        ));
        (Action::Hold, why)
    }
}

/// Budget as if `h` were closed at mid: its slot freed, proceeds back in cash and excess liquidity.
pub fn after_closing(b: &Budget, h: &Holding) -> Budget {
    let mut b = b.clone();
    let proceeds = h.market_value;
    b.underlyings.retain(|u| !u.eq_ignore_ascii_case(&h.underlying));
    b.slots_used = b.underlyings.len() as u32;
    b.cash += proceeds;
    b.excess_liquidity += proceeds;
    b.long_premium = (b.long_premium - proceeds.max(0.0)).max(0.0);
    b.capital_at_work = (b.capital_at_work - proceeds.max(0.0)).max(0.0);
    let e = b.equity.max(1.0);
    let pct = |x: f64| x / e * 100.0;
    b.buffer_headroom = b.cash - e * b.rules.cash_buffer_pct / 100.0;
    b.floor_headroom = b.excess_liquidity - e * b.rules.liquidity_floor_pct / 100.0;
    b.premium_headroom = b.rules.premium_budget_pct.map(|p| e * p / 100.0 - b.long_premium);
    b.cash_pct = pct(b.cash);
    b.excess_liquidity_pct = pct(b.excess_liquidity);
    b.long_premium_pct = pct(b.long_premium);
    b.capital_at_work_pct = pct(b.capital_at_work);
    b
}

/// For each ENTER candidate the book cannot fund, the holding whose close funds it with the
/// largest expected gain (if any is positive). Holdings marked CLOSE are tried first.
pub fn replacements(holdings: &[Holding], candidates: &[Candidate], b: &Budget) -> Vec<Replacement> {
    let mut out = Vec::new();
    for c in candidates.iter().filter(|c| c.verdict == Verdict::Enter && !c.fits()) {
        let Some(pick) = &c.pick else { continue };
        let mut best: Option<Replacement> = None;
        for h in holdings.iter().filter(|h| !h.underlying.eq_ignore_ascii_case(&c.symbol) && h.hold.is_some()) {
            let freed = after_closing(b, h);
            let s = sizing::size_lots(pick.max_loss, pick.net_at_entry > 0.0, &c.symbol, &freed, 0.0);
            if s.contracts == 0 {
                continue;
            }
            let e = h.hold.as_ref().unwrap();
            let lots = s.contracts as f64;
            let gain = pick.ev * lots - h.hold_ev() - e.close_friction;
            let gain_no_edge = pick.ev_no_edge * lots - e.ev_no_edge - e.close_friction;
            if gain <= 0.0 {
                continue;
            }
            let better = best
                .as_ref()
                .is_none_or(|r| (h.action == Action::Close, gain) > (r.close_action == Action::Close, r.gain));
            if better {
                let mut open_pick = pick.clone();
                open_pick.sizing = Some(s.clone());
                let why = format!(
                    "{} has {}; closing {} ({}) frees ${:.0} and the slot; {} lot(s) of {} {} expected {:+.0} vs {:+.0} from holding, {:.0} to close; if the new setup has no edge: {:+.0}",
                    c.symbol,
                    if c.robust { "a robust edge" } else { "a tentative edge" },
                    h.underlying,
                    match h.action {
                        Action::Close => "no edge, costs to hold",
                        Action::Roll => "contracts don't fit its plan",
                        Action::Hold => "the weakest holding that funds it",
                    },
                    h.market_value,
                    s.contracts,
                    c.symbol,
                    pick.structure.label(),
                    pick.ev * lots,
                    h.hold_ev(),
                    e.close_friction,
                    gain_no_edge
                );
                best = Some(Replacement {
                    close: h.underlying.clone(),
                    close_positions: h.positions.clone(),
                    close_action: h.action,
                    proceeds: h.market_value,
                    close_friction: e.close_friction,
                    hold_ev: h.hold_ev(),
                    open: c.symbol.clone(),
                    open_pick,
                    lots: s.contracts,
                    gain,
                    gain_no_edge,
                    why,
                });
            }
        }
        out.extend(best);
    }
    out.sort_by(|a, b| b.gain.total_cmp(&a.gain));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::pick::contracts::Structure;
    use crate::analytics::sizing::{budget, BookInput, SizingConfig};
    use chrono::NaiveDate;

    fn eval(value: f64, ev_plan: f64, ev_no_edge: f64, days: usize) -> HoldEval {
        HoldEval {
            value_now: value,
            ev_plan,
            ev_no_edge,
            p_better: 0.4,
            horizon_days: days,
            horizon_end: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            theta_day: -10.0,
            close_friction: 8.0,
        }
    }

    fn holding(sym: &str, value: f64, has_edge: bool, e: HoldEval) -> Holding {
        Holding {
            underlying: sym.into(),
            positions: vec![format!("{sym} C")],
            direction: Direction::Long,
            spot: 100.0,
            market_value: value,
            unrealized: 0.0,
            min_dte: Some(40),
            verdict: if has_edge { Verdict::Enter } else { Verdict::NoTrade },
            headline: String::new(),
            setup: Some("pullback long".into()),
            has_edge,
            earnings_move: None,
            hold: Some(e),
            action: Action::Hold,
            reasons: vec![],
            roll_to: None,
        }
    }

    fn pick(max_loss: f64, ev: f64) -> Pick {
        Pick {
            structure: Structure::LongCall,
            expiry: NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
            dte: 70,
            legs: vec![],
            ratios: vec![1],
            net_mid_now: max_loss / 100.0,
            net_at_entry: max_loss / 100.0,
            limit_at_entry: max_loss / 100.0,
            max_loss,
            max_gain: None,
            ev,
            ev_on_risk: ev / max_loss,
            ev_no_edge: -0.1 * max_loss,
            p_profit: 0.4,
            pnl_at_sl: 0.0,
            pnl_at_t1: 0.0,
            pnl_at_t2: 0.0,
            pnl_flat: 0.0,
            delta: 50.0,
            theta_day: -5.0,
            vega: 10.0,
            friction: 10.0,
            min_oi: 1000.0,
            worst_spread_pct: 0.02,
            live_quotes: false,
            exit_plan: String::new(),
            why: String::new(),
            notes: vec![],
            sizing: None,
        }
    }

    #[test]
    fn decisions() {
        let h = holding("A", 1000.0, false, eval(1000.0, 50.0, -200.0, 30));
        assert_eq!(decide(&h, None, 20.0).0, Action::Close);
        let h = holding("A", 1000.0, false, eval(1000.0, 50.0, -20.0, 30));
        assert_eq!(decide(&h, None, 20.0).0, Action::Hold);
        let h = holding("A", 1000.0, true, eval(1000.0, 120.0, -200.0, 30));
        assert_eq!(decide(&h, None, 20.0).0, Action::Hold);
        // Edge, but the contracts expire before T1: roll when a fitting contract exists.
        let h = holding("A", 1000.0, true, eval(1000.0, -80.0, -200.0, 8));
        let (a, why) = decide(&h, Some(&pick(900.0, 150.0)), 20.0);
        assert_eq!(a, Action::Roll);
        assert!(why.iter().any(|w| w.contains("expire before T1")), "{why:?}");
        assert_eq!(decide(&h, None, 20.0).0, Action::Close);
    }

    #[test]
    fn replaces_the_weakest_holding_that_funds_it() {
        // $5k book, 3 slots full, $500 above the floor.
        let input = BookInput {
            equity: 5000.0,
            cash: 1000.0,
            excess_liquidity: None,
            underlyings: vec!["A".into(), "B".into(), "C".into()],
            long_premium: 4000.0,
            capital_at_work: 4000.0,
        };
        let b = budget(input, "USD".into(), 1.0, &SizingConfig::default());
        let mut a = holding("A", 1500.0, false, eval(1500.0, 0.0, -300.0, 30));
        a.action = Action::Close;
        let bh = holding("B", 1500.0, true, eval(1500.0, 200.0, -100.0, 30));
        let c = holding("C", 1000.0, false, eval(1000.0, 0.0, -5.0, 30));
        let cand = Candidate {
            symbol: "N".into(),
            verdict: Verdict::Enter,
            headline: String::new(),
            setup: None,
            robust: true,
            earnings_move: None,
            pick: Some(pick(600.0, 180.0)),
        };
        let r = replacements(&[a, bh, c], std::slice::from_ref(&cand), &b);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].close, "A");
        assert!(r[0].gain > 0.0 && r[0].lots >= 1, "{:?}", r[0].why);
        // Nothing worth giving up: no replacement.
        let strong = holding("A", 1500.0, true, eval(1500.0, 900.0, 0.0, 30));
        assert!(replacements(&[strong], &[cand], &b).is_empty());
    }
}
