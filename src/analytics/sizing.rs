//! Position sizing from the actual book. Net liquidation is 100%. Limits:
//! - slots: concurrent underlyings, by equity tier;
//! - per-trade target and cap (% of equity; the cap also bounds the total in one underlying);
//! - cash buffer (soft, default 20%): the target size keeps cash above it;
//! - excess-liquidity floor (hard, default 10%): never sized through, since below it brokers start
//!   margin calls and liquidations;
//! - optional long-premium budget (off by default).
//!
//! All overridable in `[sizing]` in config.toml. This is arithmetic only — whether a trade deserves
//! a slot at all is a judgment made before sizing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SizingConfig {
    /// Concurrent positions (distinct underlyings). Default by equity tier.
    pub slots: Option<u32>,
    /// Target capital per option trade, % of equity. Default by tier.
    pub option_target_pct: Option<f64>,
    /// Hard cap per option trade (and per underlying), % of equity. Default by tier.
    pub option_cap_pct: Option<f64>,
    /// Cash the target size keeps uncommitted, % of equity. Default 20.
    pub cash_buffer_pct: Option<f64>,
    /// Excess liquidity never sized through, % of equity. Default 10.
    pub liquidity_floor_pct: Option<f64>,
    /// Optional cap on total long premium, % of equity. Default: none.
    pub premium_budget_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rules {
    pub tier: String,
    pub slots: u32,
    pub option_target_pct: f64,
    pub option_cap_pct: f64,
    pub cash_buffer_pct: f64,
    pub liquidity_floor_pct: f64,
    pub premium_budget_pct: Option<f64>,
}

pub fn rules(equity: f64, cfg: &SizingConfig) -> Rules {
    let (tier, slots, target, cap) = match equity {
        e if e < 7_500.0 => ("~$5k", 3, 8.0, 15.0),
        e if e < 12_500.0 => ("~$10k", 4, 7.0, 10.0),
        e if e < 25_000.0 => ("~$15-25k", 5, 5.0, 8.0),
        _ => ("$25k+", 6, 2.0, 4.0),
    };
    Rules {
        tier: tier.into(),
        slots: cfg.slots.unwrap_or(slots),
        option_target_pct: cfg.option_target_pct.unwrap_or(target),
        option_cap_pct: cfg.option_cap_pct.unwrap_or(cap),
        cash_buffer_pct: cfg.cash_buffer_pct.unwrap_or(20.0),
        liquidity_floor_pct: cfg.liquidity_floor_pct.unwrap_or(10.0),
        premium_budget_pct: cfg.premium_budget_pct,
    }
}

/// The book's current use of its limits, in USD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub rules: Rules,
    pub base_currency: String,
    /// Base-currency units per USD (e.g. SEK per USD); 1 for USD accounts.
    pub fx_rate: f64,
    pub equity: f64,
    pub cash: f64,
    /// Broker excess liquidity; equals cash when the snapshot does not include it.
    pub excess_liquidity: f64,
    pub underlyings: Vec<String>,
    pub slots_used: u32,
    /// Current value of long options (the premium that can still be lost).
    pub long_premium: f64,
    pub long_premium_pct: f64,
    /// Net market value per underlying (options + stock), floored at zero, summed.
    pub capital_at_work: f64,
    pub capital_at_work_pct: f64,
    pub cash_pct: f64,
    pub excess_liquidity_pct: f64,
    /// Dollars that can be committed before cash reaches the buffer (negative = already below).
    pub buffer_headroom: f64,
    /// Dollars that can be committed before excess liquidity reaches the floor.
    pub floor_headroom: f64,
    /// Room under the optional premium budget.
    pub premium_headroom: Option<f64>,
    pub per_trade_target: f64,
    pub per_trade_cap: f64,
}

pub struct BookInput {
    pub equity: f64,
    pub cash: f64,
    pub excess_liquidity: Option<f64>,
    pub underlyings: Vec<String>,
    pub long_premium: f64,
    pub capital_at_work: f64,
}

pub fn budget(input: BookInput, base_currency: String, fx_rate: f64, cfg: &SizingConfig) -> Budget {
    let r = rules(input.equity, cfg);
    let e = input.equity.max(1.0);
    let pct = |x: f64| x / e * 100.0;
    let excess = input.excess_liquidity.unwrap_or(input.cash);
    Budget {
        per_trade_target: e * r.option_target_pct / 100.0,
        per_trade_cap: e * r.option_cap_pct / 100.0,
        buffer_headroom: input.cash - e * r.cash_buffer_pct / 100.0,
        floor_headroom: excess - e * r.liquidity_floor_pct / 100.0,
        premium_headroom: r.premium_budget_pct.map(|p| e * p / 100.0 - input.long_premium),
        slots_used: input.underlyings.len() as u32,
        long_premium_pct: pct(input.long_premium),
        capital_at_work_pct: pct(input.capital_at_work),
        cash_pct: pct(input.cash),
        excess_liquidity_pct: pct(excess),
        excess_liquidity: excess,
        underlyings: input.underlyings,
        long_premium: input.long_premium,
        capital_at_work: input.capital_at_work,
        equity: input.equity,
        cash: input.cash,
        base_currency,
        fx_rate,
        rules: r,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sizing {
    /// Contracts (lots) at the per-trade target, keeping the cash buffer.
    pub contracts: u32,
    /// Most contracts allowed by the hard limits (per-trade cap, liquidity floor).
    pub max_contracts: u32,
    pub capital_per_lot: f64,
    /// Capital at work for `contracts` lots, $ and % of equity.
    pub capital: f64,
    pub capital_pct: f64,
    /// The limit that decided `contracts`.
    pub binding: String,
    /// Why the trade does not fit, if `max_contracts == 0`.
    pub blocked: Option<String>,
    pub cash_after_pct: f64,
    pub excess_liquidity_after_pct: f64,
    /// `contracts` takes cash below the buffer (allowed only because the floor still holds).
    pub dips_into_buffer: bool,
    /// Adds to an existing position's slot rather than opening a new one.
    pub same_underlying: bool,
}

/// Size a structure whose 1-lot max loss is `per_lot` dollars.
pub fn size_lots(per_lot: f64, is_debit: bool, underlying: &str, b: &Budget, existing: f64) -> Sizing {
    let same = b.underlyings.iter().any(|u| u.eq_ignore_ascii_case(underlying));
    let underlying_room = b.per_trade_cap - existing;
    let premium = b.premium_headroom.filter(|_| is_debit).map(|h| ("premium budget", h));

    let soft: Vec<(&str, f64)> = [
        Some(("per-trade target", b.per_trade_target)),
        Some(("per-underlying cap", underlying_room)),
        Some(("cash buffer", b.buffer_headroom)),
        premium,
    ]
    .into_iter()
    .flatten()
    .collect();
    let hard: Vec<(&str, f64)> = [
        Some(("per-trade cap", b.per_trade_cap)),
        Some(("per-underlying cap", underlying_room)),
        Some(("liquidity floor", b.floor_headroom)),
        premium,
    ]
    .into_iter()
    .flatten()
    .collect();
    let tightest = |limits: &[(&str, f64)]| -> (String, f64) {
        let (n, d) = limits.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        (n.to_string(), d)
    };
    let lots = |dollars: f64| if per_lot > 0.0 { (dollars.max(0.0) / per_lot).floor() as u32 } else { 0 };

    let (soft_name, soft_room) = tightest(&soft);
    let (hard_name, hard_room) = tightest(&hard);
    let slot_blocked = !same && b.slots_used >= b.rules.slots;
    let target_n = lots(soft_room);
    let max_n = if slot_blocked { 0 } else { lots(hard_room) };
    // One lot is allowed when soft limits are too tight but the hard limits still fit it.
    let contracts = if max_n == 0 {
        0
    } else if target_n == 0 {
        1
    } else {
        target_n.min(max_n)
    };

    let blocked = if slot_blocked {
        Some(format!("all {} slots in use ({}); close one first", b.rules.slots, b.underlyings.join(", ")))
    } else if max_n == 0 {
        Some(if hard_room <= 0.0 {
            format!("{hard_name} already reached ({hard_room:+.0} left)")
        } else {
            format!("1 lot needs ${per_lot:.0} but only ${hard_room:.0} fits the {hard_name}")
        })
    } else {
        None
    };

    let capital = contracts as f64 * per_lot;
    let e = b.equity.max(1.0);
    let cash_after = b.cash - capital;
    Sizing {
        contracts,
        max_contracts: max_n,
        capital_per_lot: per_lot,
        capital,
        capital_pct: capital / e * 100.0,
        binding: if contracts == 1 && target_n == 0 { format!("1 lot despite {soft_name}") } else { soft_name },
        blocked,
        cash_after_pct: cash_after / e * 100.0,
        excess_liquidity_after_pct: (b.excess_liquidity - capital) / e * 100.0,
        dips_into_buffer: contracts > 0 && cash_after < e * b.rules.cash_buffer_pct / 100.0,
        same_underlying: same,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn book(cash: f64, underlyings: &[&str]) -> Budget {
        let input = BookInput {
            equity: 10_000.0,
            cash,
            excess_liquidity: None,
            underlyings: underlyings.iter().map(|s| s.to_string()).collect(),
            long_premium: 10_000.0 - cash,
            capital_at_work: 10_000.0 - cash,
        };
        budget(input, "USD".into(), 1.0, &SizingConfig::default())
    }

    #[test]
    fn sizes_to_target_without_premium_cap() {
        // $10k tier: target 7% = $700, cap 10% = $1000. 60% already in premium is fine now.
        let s = size_lots(180.0, true, "NVDA", &book(4_000.0, &["GOOG"]), 0.0);
        assert_eq!(s.contracts, 3);
        assert_eq!(s.max_contracts, 5);
        assert!(s.blocked.is_none() && !s.dips_into_buffer);
    }

    #[test]
    fn buffer_is_soft_floor_is_hard() {
        // Cash $2200: $200 above the 20% buffer.
        let s = size_lots(180.0, true, "NVDA", &book(2_200.0, &["GOOG"]), 0.0);
        assert_eq!((s.contracts, s.max_contracts), (1, 5));
        assert!(!s.dips_into_buffer);
        // Cash $2100: the target can't keep the buffer, but one lot fits above the floor.
        let s = size_lots(180.0, true, "NVDA", &book(2_100.0, &["GOOG"]), 0.0);
        assert_eq!(s.contracts, 1);
        assert!(s.dips_into_buffer && s.binding.contains("cash buffer"));
        // Cash $1100: only $100 above the 10% floor, less than one lot.
        let s = size_lots(180.0, true, "NVDA", &book(1_100.0, &["GOOG"]), 0.0);
        assert_eq!(s.contracts, 0);
        assert!(s.blocked.unwrap().contains("liquidity floor"));
    }

    #[test]
    fn optional_premium_budget() {
        let cfg = SizingConfig { premium_budget_pct: Some(30.0), ..Default::default() };
        let input = BookInput {
            equity: 10_000.0,
            cash: 6_000.0,
            excess_liquidity: None,
            underlyings: vec!["GOOG".into()],
            long_premium: 2_900.0,
            capital_at_work: 2_900.0,
        };
        let s = size_lots(180.0, true, "NVDA", &budget(input, "USD".into(), 1.0, &cfg), 0.0);
        assert!(s.blocked.unwrap().contains("premium budget"));
    }

    #[test]
    fn slots_block_new_names_only() {
        let b = book(6_000.0, &["A", "B", "C", "D"]);
        assert!(size_lots(180.0, true, "NVDA", &b, 0.0).blocked.unwrap().contains("slots"));
        let s = size_lots(180.0, true, "A", &b, 200.0);
        assert!(s.same_underlying && s.contracts > 0);
    }
}
