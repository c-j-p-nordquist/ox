//! `ox review`: what does each holding cost to keep if the stock moves at random?
//!
//! No plan is assumed to have an edge (see [`super::pick::setup`]), so a holding is not judged
//! on whether its direction "has a setup". The held contracts are simulated through the leading
//! plan for the holding's direction on zero-drift paths, against closing them now
//! ([`HoldEval`]). Sunk cost does not enter; only what holding is expected to add from here.
//!
//! - HOLD: holding costs little at random. Whether to keep it rests on the user's own thesis.
//! - CLOSE: holding is expected to lose a material part of its value by the plan's horizon
//!   (theta, and IV above the realized-vol forecast) unless the stock moves the right way.

use serde::{Deserialize, Serialize};

use super::pick::contracts::HoldEval;
use super::pick::earnings::EarningsMove;
use super::pick::setup::Direction;
use super::sizing::Budget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Hold,
    Close,
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
    /// `pick` in the holding's direction: the plan the holding is simulated through.
    pub headline: String,
    pub setup: Option<String>,
    /// The next report, if one is ahead (date, implied and past moves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earnings_move: Option<EarningsMove>,
    pub hold: Option<HoldEval>,
    pub action: Action,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub as_of: i64,
    pub budget: Option<Budget>,
    pub holdings: Vec<Holding>,
    pub notes: Vec<String>,
}

/// Holding "costs little" below this expected loss.
fn material(value: f64) -> f64 {
    (0.05 * value.abs()).max(10.0)
}

/// HOLD / CLOSE for a holding, with reasons.
pub fn decide(h: &Holding) -> (Action, Vec<String>) {
    let Some(e) = &h.hold else {
        return (Action::Hold, vec!["contracts not found in the chain: cannot simulate holding".into()]);
    };
    let horizon = format!("to {} ({} trading days)", e.horizon_end, e.horizon_days);
    if e.ev_no_edge < -material(e.value_now) {
        let why = vec![
            format!(
                "if {} moves at random, holding is expected to lose {:.0} {horizon} (theta {:+.0}/day, IV vs forecast realized vol)",
                h.underlying, -e.ev_no_edge, e.theta_day
            ),
            "hold only for a reason of your own (catalyst, thesis) and say which".into(),
        ];
        (Action::Close, why)
    } else {
        let why = vec![format!(
            "holding costs little at random ({:+.0} {horizon}): whether to keep it rests on your own thesis",
            e.ev_no_edge
        )];
        (Action::Hold, why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn holding(value: f64, ev_no_edge: f64) -> Holding {
        Holding {
            underlying: "A".into(),
            positions: vec!["A C".into()],
            direction: Direction::Long,
            spot: 100.0,
            market_value: value,
            unrealized: 0.0,
            min_dte: Some(40),
            headline: String::new(),
            setup: Some("pullback long".into()),
            earnings_move: None,
            hold: Some(HoldEval {
                value_now: value,
                ev_no_edge,
                p_better: 0.4,
                horizon_days: 30,
                horizon_end: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                theta_day: -10.0,
                close_friction: 8.0,
            }),
            action: Action::Hold,
            reasons: vec![],
        }
    }

    #[test]
    fn decisions() {
        // Expected to lose a fifth of its value at random: close. A fiftieth: hold.
        assert_eq!(decide(&holding(1000.0, -200.0)).0, Action::Close);
        assert_eq!(decide(&holding(1000.0, -20.0)).0, Action::Hold);
        let mut h = holding(1000.0, -200.0);
        h.hold = None;
        assert_eq!(decide(&h).0, Action::Hold);
    }
}
