//! One-shot commands. Every command supports `--json` for scripts and for Claude.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;

use crate::analytics::atm_index;
use crate::analytics::legs::Side;
use crate::analytics::market::Freshness;
use crate::analytics::risk::{Scenario, Severity};
use crate::client::Client;
use crate::expect;
use crate::fmt::{self, big, opt_num, pct, price, signed_big, strike};
use crate::portfolio::{self, PortfolioFile};
use crate::protocol::{Request, Response};

fn color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

pub(crate) fn paint(code: &str, s: &str) -> String {
    if color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub(crate) fn green(s: &str) -> String {
    paint("32", s)
}
pub(crate) fn red(s: &str) -> String {
    paint("31", s)
}
pub(crate) fn yellow(s: &str) -> String {
    paint("33", s)
}
pub(crate) fn dim(s: &str) -> String {
    paint("2", s)
}
pub(crate) fn bold(s: &str) -> String {
    paint("1", s)
}
pub(crate) fn by_sign(x: f64, s: &str) -> String {
    if x >= 0.0 {
        green(s)
    } else {
        red(s)
    }
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn source_line(source: &str, delayed: bool, f: &Freshness) -> String {
    let base = dim(&format!(
        "{source}{} · spot {} ({})",
        if delayed { " delayed" } else { "" },
        fmt::time_et(f.spot_as_of),
        f.session.label()
    ));
    if f.quotes_stale {
        format!("{base} {}", paint("33", &format!("· option quotes from {} close", fmt::time_et(f.quotes_as_of))))
    } else {
        base
    }
}

pub async fn quote(client: &Client, symbols: Vec<String>, json: bool) -> Result<()> {
    let quotes = expect!(client.call(Request::Quotes { symbols }).await?, Response::Quotes)?;
    if json {
        return print_json(&quotes);
    }
    for q in quotes {
        println!(
            "{:<6} {:>10}  {}  {}",
            bold(&q.symbol),
            price(q.last),
            by_sign(q.change, &format!("{:>+8.2} {:>8}", q.change, pct(q.change_pct))),
            dim(&format!("{} · {}", q.source, fmt::time_et(q.time)))
        );
    }
    Ok(())
}

pub async fn chain(
    client: &Client,
    symbol: String,
    expiry: Option<NaiveDate>,
    dte: Option<u32>,
    strikes: usize,
    json: bool,
) -> Result<()> {
    let req = Request::Chain { symbol, expiry, min_dte: dte, strikes: (strikes > 0).then_some(strikes) };
    let v = expect!(client.call(req).await?, Response::Chain)?;
    if json {
        return print_json(&v);
    }
    println!(
        "{} {}  exp {} ({}d)   {}",
        bold(&v.underlying),
        price(v.spot),
        v.expiry,
        v.dte,
        source_line(&v.source, v.delayed, &v.freshness)
    );
    println!(
        "{}",
        dim(&format!(
            "{:>7} {:>7} {:>6} {:>6} {:>7} {:>7} │ {:^8} │ {:<7} {:<7} {:<6} {:<6} {:<7} {:<7}",
            "OI", "Vol", "IV", "Δ", "Bid", "Ask", "Strike", "Bid", "Ask", "Δ", "IV", "Vol", "OI"
        ))
    );
    let atm = atm_index(&v.rows, v.spot);
    for (i, r) in v.rows.iter().enumerate() {
        let c = r.call.as_ref();
        let p = r.put.as_ref();
        let f = |x: Option<f64>, w: usize, d: usize| {
            x.map(|x| format!("{x:>w$.d$}")).unwrap_or_else(|| format!("{:>w$}", "–"))
        };
        let calls = format!(
            "{} {} {} {} {} {}",
            f(c.map(|c| c.open_interest), 7, 0),
            f(c.map(|c| c.volume), 7, 0),
            f(c.map(|c| c.iv * 100.0), 6, 1),
            f(c.map(|c| c.delta), 6, 2),
            f(c.map(|c| c.bid), 7, 2),
            f(c.map(|c| c.ask), 7, 2)
        );
        let puts = format!(
            "{} {} {} {} {} {}",
            f(p.map(|c| c.bid), 7, 2),
            f(p.map(|c| c.ask), 7, 2),
            f(p.map(|c| c.delta), 6, 2),
            f(p.map(|c| c.iv * 100.0), 6, 1),
            f(p.map(|c| c.volume), 7, 0),
            f(p.map(|c| c.open_interest), 7, 0)
        );
        let k = format!("{:^8}", strike(r.strike));
        let k = if i == atm { paint("1;7", &k) } else { bold(&k) };
        let calls = if r.strike < v.spot { paint("33", &calls) } else { calls };
        let puts = if r.strike > v.spot { paint("33", &puts) } else { puts };
        println!("{calls} │ {k} │ {puts}");
    }
    let exps: Vec<String> =
        v.expirations.iter().take(12).map(|e| format!("{}({}d)", e.date.format("%m-%d"), e.dte)).collect();
    println!("{}", dim(&format!("expirations: {}", exps.join(" "))));
    Ok(())
}

pub async fn gex(client: &Client, symbol: String, max_dte: Option<u32>, rows: usize, json: bool) -> Result<()> {
    let mut g = expect!(client.call(Request::Gex { symbol, max_dte }).await?, Response::Gex)?;
    // Keep output focused on strikes around spot.
    let center = g.strikes.iter().position(|s| s.strike >= g.spot).unwrap_or(g.strikes.len());
    let half = rows / 2;
    let lo = center.saturating_sub(half);
    let hi = (center + half).min(g.strikes.len());
    g.strikes = g.strikes.drain(lo..hi).collect();
    if json {
        return print_json(&g);
    }
    println!(
        "{} {}  GEX {} per 1%  ({} expirations{})   {}",
        bold(&g.underlying),
        price(g.spot),
        by_sign(g.total_gex, &signed_big(g.total_gex)),
        g.expirations,
        max_dte.map(|d| format!(", ≤{d} DTE")).unwrap_or_default(),
        source_line("cboe", true, &g.freshness)
    );
    println!(
        "zero gamma {}   call wall {}   put wall {}",
        bold(&opt_num(g.zero_gamma, price)),
        green(&opt_num(g.call_wall, strike)),
        red(&opt_num(g.put_wall, strike))
    );
    let max = g.strikes.iter().map(|s| s.net_gex.abs()).fold(0.0, f64::max).max(1.0);
    const W: usize = 30;
    for s in g.strikes.iter().rev() {
        let n = ((s.net_gex.abs() / max) * W as f64).round() as usize;
        let (left, right) = if s.net_gex < 0.0 {
            (format!("{:>W$}", "█".repeat(n)), " ".repeat(W))
        } else {
            (" ".repeat(W), format!("{:<W$}", "█".repeat(n)))
        };
        let mut tags = Vec::new();
        if Some(s.strike) == g.call_wall {
            tags.push("CALL WALL");
        }
        if Some(s.strike) == g.put_wall {
            tags.push("PUT WALL");
        }
        let near_spot = (s.strike - g.spot).abs()
            <= g.strikes.windows(2).map(|w| w[1].strike - w[0].strike).fold(f64::MAX, f64::min) / 2.0;
        if near_spot {
            tags.push("◀ SPOT");
        }
        let k = format!("{:>8}", strike(s.strike));
        println!(
            "{} {}│{} {:>8} {}",
            if near_spot { paint("1;7", &k) } else { k },
            red(&left),
            green(&right),
            signed_big(s.net_gex),
            bold(&tags.join(" "))
        );
    }
    Ok(())
}

pub async fn overview(client: &Client, symbol: String, json: bool) -> Result<()> {
    let o = expect!(client.call(Request::Overview { symbol }).await?, Response::Overview)?;
    if json {
        return print_json(&o);
    }
    println!(
        "{} {} {}  IV30 {}   {}",
        bold(&o.underlying),
        price(o.spot),
        by_sign(o.change_pct, &pct(o.change_pct)),
        opt_num(o.iv30, |v| format!("{:.1}%", v * 100.0)),
        source_line(&o.source, o.delayed, &o.freshness)
    );
    println!(
        "GEX {}  zero γ {}  call wall {}  put wall {}   P/C vol {}  P/C OI {}",
        by_sign(o.gex_total, &signed_big(o.gex_total)),
        opt_num(o.zero_gamma, price),
        green(&opt_num(o.call_wall, strike)),
        red(&opt_num(o.put_wall, strike)),
        opt_num(o.put_call_volume, |x| format!("{x:.2}")),
        opt_num(o.put_call_oi, |x| format!("{x:.2}"))
    );
    println!(
        "{}",
        dim(&format!(
            "{:<11} {:>4} {:>8} {:>6} {:>9} {:>14} {:>9} {:>9} {:>6} {:>9}",
            "expiry", "dte", "ATM", "IV", "straddle", "exp. move ±", "call vol", "put vol", "P/C OI", "max pain"
        ))
    );
    for e in &o.expirations {
        let em = e.expected_move.map(|m| format!("{} ({:.1}%)", price(m), m / o.spot * 100.0));
        println!(
            "{:<11} {:>4} {:>8} {:>6} {:>9} {:>14} {:>9} {:>9} {:>6} {:>9}",
            e.date.to_string(),
            e.dte,
            strike(e.atm_strike),
            opt_num(e.atm_iv, |v| format!("{:.1}%", v * 100.0)),
            opt_num(e.straddle, |x| if o.freshness.quotes_stale { format!("{}*", price(x)) } else { price(x) }),
            em.unwrap_or_else(|| "–".into()),
            big(e.call_volume),
            big(e.put_volume),
            opt_num(e.put_call_oi, |x| format!("{x:.2}")),
            opt_num(e.max_pain, strike)
        );
    }
    if o.freshness.quotes_stale {
        println!(
            "{}",
            paint(
                "33",
                &format!(
                    "* straddles priced at the {} close, before today's time decay",
                    fmt::time_et(o.freshness.quotes_as_of)
                )
            )
        );
    }
    Ok(())
}

pub async fn portfolio_show(client: &Client, json: bool) -> Result<()> {
    let p = expect!(client.call(Request::Portfolio).await?, Response::Portfolio)?;
    if json {
        return print_json(&p);
    }
    let Some(p) = p else {
        println!("No portfolio loaded. Import one with `ox portfolio import <file.json>` (schema: see README).");
        return Ok(());
    };
    let cur = p.currency.as_deref().unwrap_or("");
    println!(
        "{}  net liq {} {cur}  cash {} {cur}  {}",
        bold(p.account.as_deref().unwrap_or("portfolio")),
        opt_num(p.net_liquidation, big),
        opt_num(p.cash, big),
        dim(&format!("snapshot {}", p.as_of.as_deref().unwrap_or("?")))
    );
    println!(
        "{}",
        dim(&format!(
            "{:<24} {:>7} {:>9} {:>9} {:>10} {:>11} {:>8} {:>8} {:>5}",
            "position", "qty", "avg", "mark", "value", "P&L", "P&L%", "Δ shs", "dte"
        ))
    );
    for r in &p.rows {
        let pnl = r.unrealized_pnl.unwrap_or(0.0);
        println!(
            "{:<24} {:>7} {:>9} {:>9} {:>10} {:>11} {:>8} {:>8} {:>5}",
            r.label,
            r.position.qty,
            price(r.position.avg_cost),
            opt_num(r.mark, price),
            opt_num(r.market_value, big),
            by_sign(pnl, &format!("{:>11}", opt_num(r.unrealized_pnl, signed_big))),
            opt_num(r.unrealized_pct, |x| format!("{x:+.1}%")),
            opt_num(r.delta_shares, |x| format!("{x:.0}")),
            r.dte.map(|d| d.to_string()).unwrap_or_default()
        );
    }
    println!(
        "positions (USD): value {}   unrealized {}",
        big(p.total_market_value),
        by_sign(p.total_unrealized, &signed_big(p.total_unrealized))
    );
    Ok(())
}

pub fn portfolio_import(file: &str) -> Result<()> {
    let data = if file == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
    };
    let p: PortfolioFile = serde_json::from_str(&data).context("portfolio JSON does not match the schema")?;
    let n = p.positions.len();
    let path = portfolio::save(&p)?;
    println!("imported {n} positions → {}", path.display());
    Ok(())
}

pub async fn risk(client: &Client, json: bool) -> Result<()> {
    let r = expect!(client.call(Request::Risk).await?, Response::Risk)?;
    if json {
        return print_json(&r);
    }
    let Some(r) = r else {
        println!("No portfolio loaded. Import one with `ox portfolio import <file.json>`.");
        return Ok(());
    };
    let t = &r.totals;
    println!(
        "{}  net liq {} {}   positions (USD): value {}  unrealized {}  Δ$ {}  θ {}/day  vega {:+.0}/pt",
        bold(r.account.as_deref().unwrap_or("portfolio")),
        opt_num(r.net_liquidation, big),
        r.currency.as_deref().unwrap_or(""),
        big(t.market_value),
        by_sign(t.unrealized, &signed_big(t.unrealized)),
        signed_big(t.delta_dollars),
        by_sign(t.theta_day, &format!("{:+.0}", t.theta_day)),
        t.vega
    );
    if let Some(f) = &r.freshness {
        println!("{}", source_line("cboe", true, f));
    }
    if let Some(b) = &r.budget {
        println!("{}", budget_line(b));
    }
    if let Some(m) = &r.market {
        println!(
            "{} SPY {} ({}) trend {} · {} 50d · {} 200d · zero γ {} · VIX {} ({})",
            dim("market"),
            price(m.spy),
            by_sign(m.spy_change_pct, &pct(m.spy_change_pct)),
            m.spy_trend,
            match m.spy_above_50d {
                Some(true) => "above",
                Some(false) => "below",
                None => "?",
            },
            match m.spy_above_200d {
                Some(true) => "above",
                Some(false) => "below",
                None => "?",
            },
            opt_num(m.spy_zero_gamma, price),
            opt_num(m.vix, |v| format!("{v:.1}")),
            opt_num(m.vix_change_pct, pct)
        );
    }
    let d = &r.directional;
    let bench = d.benchmark.as_deref().unwrap_or("index");
    println!(
        "{} {bench}-equivalent {}{} · 1% {bench} ≈ {} · long premium: calls {} / puts {}{}",
        dim("direction"),
        signed_big(d.benchmark_equivalent),
        opt_num(d.benchmark_equivalent_pct, |p| format!(" ({:.1}× equity)", p / 100.0)),
        by_sign(d.pnl_per_benchmark_pct, &format!("{:+.0}", d.pnl_per_benchmark_pct)),
        big(d.long_call_premium),
        big(d.long_put_premium),
        opt_num(d.call_share, |s| format!(" ({:.0}% calls)", s * 100.0))
    );
    if let Some(b) = &r.book_risk {
        let fits: Vec<String> = b.fits.iter().map(|f| format!("{} {:.2}", f.benchmark, f.weighted_r2)).collect();
        println!(
            "{} 1σ day ±${:.0}{} · {} explains {:.0}% (±${:.0}), company-specific ±${:.0} · diversification saves {:.0}% vs ±${:.0} standalone · fit R² {} · {} days",
            dim("daily risk"),
            b.daily_sigma,
            opt_num(b.daily_sigma_pct, |p| format!(" ({p:.1}% of equity)")),
            b.benchmark,
            b.market_share * 100.0,
            b.market_sigma,
            b.specific_sigma,
            b.diversification * 100.0,
            b.standalone_sigma,
            fits.join(", "),
            b.observations
        );
        let shocks: Vec<String> = b
            .market_shocks
            .iter()
            .map(|s| format!("{} {}", dim(&s.label), by_sign(s.pnl, &format!("{:+.0}", s.pnl))))
            .collect();
        println!("{} {}", dim("market shocks"), shocks.join("  "));
    }
    for f in &r.flags {
        let tag = match f.severity {
            Severity::High => paint("1;31", "HIGH"),
            Severity::Warn => paint("33", "WARN"),
            Severity::Info => dim("info"),
        };
        println!("  {tag}  {:<24} {}", f.position, f.message);
    }
    for e in &r.exposures {
        println!();
        println!(
            "{} {}  {} pos   β {} R² {}   vol {}/day   risk {}   Δ {:+.0} sh ({})   Γ {:+.2} sh/$   θ {}/day   vega {:+.0}/pt   value {}  P&L {}",
            bold(&e.underlying),
            opt_num(e.spot, price),
            e.positions,
            opt_num(e.beta, |b| format!("{b:.2}")),
            opt_num(e.r2, |x| format!("{x:.2}")),
            opt_num(e.daily_vol, |v| format!("{:.1}%", v * 100.0)),
            opt_num(e.risk_share, |x| format!("{:.0}%", x * 100.0)),
            e.delta_shares,
            signed_big(e.delta_dollars),
            e.gamma_shares,
            by_sign(e.theta_day, &format!("{:+.0}", e.theta_day)),
            e.vega,
            big(e.market_value),
            by_sign(e.unrealized, &signed_big(e.unrealized)),
        );
        let row = |s: &[Scenario]| {
            s.iter()
                .map(|x| {
                    format!(
                        "{} {} ({:+.1}%) {}",
                        x.label,
                        price(x.price),
                        x.move_pct,
                        by_sign(x.pnl, &format!("{:+.0}", x.pnl))
                    )
                })
                .collect::<Vec<_>>()
                .join(dim("  │  ").as_str())
        };
        if let Some(x) = &e.next_earnings {
            let today = chrono::Utc::now().with_timezone(&chrono_tz::America::New_York).date_naive();
            println!(
                "  {} {} (in {}d{})",
                dim("earnings"),
                x.date,
                (x.date - today).num_days(),
                if x.estimated { ", estimated" } else { "" }
            );
        }
        if !e.scenarios.is_empty() {
            println!("  {} {}", dim("levels"), row(&e.scenarios));
        }
        if !e.shocks.is_empty() {
            println!("  {} {}", dim("own day"), row(&e.shocks));
        }
        if !e.ladder.is_empty() {
            let ladder: Vec<String> = e
                .ladder
                .iter()
                .map(|x| format!("{} {}", dim(&x.label), by_sign(x.pnl, &format!("{:+.0}", x.pnl))))
                .collect();
            println!("  {} {}", dim("moves "), ladder.join("  "));
        }
    }
    Ok(())
}

fn budget_line(b: &crate::analytics::sizing::Budget) -> String {
    let r = &b.rules;
    let state = |room: f64| if room < 0.0 { paint("33", "below") } else { green("ok") };
    let premium = match r.premium_budget_pct {
        Some(p) => format!("{:.0}%/{p:.0}%", b.long_premium_pct),
        None => format!("{:.0}%", b.long_premium_pct),
    };
    format!(
        "{} equity ${:.0} ({} tier{}) · slots {}/{} · long premium {} · cash {:.0}% (buffer {:.0}%) {} · excess liq {:.0}% (floor {:.0}%) {} · deployable ${:.0} to buffer, ${:.0} to floor · per trade {:.0}% target, {:.0}% cap",
        dim("budget"),
        b.equity,
        r.tier,
        if b.base_currency != "USD" { format!(", {} @ {:.2}", b.base_currency, b.fx_rate) } else { String::new() },
        b.slots_used,
        r.slots,
        premium,
        b.cash_pct,
        r.cash_buffer_pct,
        state(b.buffer_headroom),
        b.excess_liquidity_pct,
        r.liquidity_floor_pct,
        state(b.floor_headroom),
        b.buffer_headroom.max(0.0),
        b.floor_headroom.max(0.0),
        r.option_target_pct,
        r.option_cap_pct
    )
}

/// Parse a horizon like `2w`, `10d`, `1m`, `14` or a date `2026-10-16` into days from today.
pub fn watchlist_import(file: &str) -> Result<()> {
    let data = if file == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
    };
    let w = crate::watchlist::parse(&data)?;
    let n = w.symbols.len();
    let path = crate::watchlist::save(&w)?;
    println!("imported {n} symbols{} → {}", w.name.map(|n| format!(" ({n})")).unwrap_or_default(), path.display());
    Ok(())
}

pub async fn watchlist_show(client: &Client, cfg: &crate::config::Config, json: bool) -> Result<()> {
    let (symbols, name) = crate::watchlist::effective(&cfg.watchlist);
    let quotes = expect!(client.call(Request::Quotes { symbols: symbols.clone() }).await?, Response::Quotes)?;
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            name: Option<String>,
            symbols: &'a [String],
            quotes: Vec<crate::model::Quote>,
        }
        return print_json(&Out { name, symbols: &symbols, quotes });
    }
    println!(
        "{} {}",
        bold(name.as_deref().unwrap_or("watchlist")),
        dim(&format!("({} symbols{})", symbols.len(), if name.is_some() { ", synced" } else { ", from config.toml" }))
    );
    for s in &symbols {
        match quotes.iter().find(|q| &q.symbol == s) {
            Some(q) => println!(
                "{:<6} {:>10}  {}",
                bold(s),
                price(q.last),
                by_sign(q.change, &format!("{:>+8.2} {:>8}", q.change, pct(q.change_pct)))
            ),
            None => println!("{:<6} {:>10}", bold(s), dim("no quote")),
        }
    }
    Ok(())
}

pub fn read_input(file: &str) -> Result<String> {
    if file == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        Ok(s)
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))
    }
}

pub fn ingest_overnight(file: &str) -> Result<()> {
    for (sym, n) in crate::ingest::save_overnight(&read_input(file)?)? {
        println!("{sym}: {n} overnight bars");
    }
    Ok(())
}

pub async fn earnings(client: &Client, symbols: Vec<String>, json: bool) -> Result<()> {
    let e = expect!(client.call(Request::Earnings { symbols: symbols.clone() }).await?, Response::Earnings)?;
    if json {
        return print_json(&e);
    }
    let today = chrono::Utc::now().with_timezone(&chrono_tz::America::New_York).date_naive();
    for s in symbols {
        match e.iter().find(|x| x.symbol.eq_ignore_ascii_case(&s)) {
            Some(x) => println!(
                "{:<6} {}  in {:>3}d{}",
                bold(&x.symbol),
                x.date,
                (x.date - today).num_days(),
                if x.estimated { dim(" (estimated)") } else { String::new() }
            ),
            None => println!("{:<6} {}", bold(&s.to_ascii_uppercase()), dim("no earnings date (ETF/index or unknown)")),
        }
    }
    Ok(())
}

pub async fn levels(client: &Client, symbol: String, json: bool) -> Result<()> {
    let r = expect!(client.call(Request::Levels { symbol }).await?, Response::Levels)?;
    if json {
        return print_json(&r);
    }
    let s = &r.session;
    println!("{} {}  ATR(14) {}", bold(&r.symbol), price(r.spot), opt_num(r.atr, price));
    println!(
        "{} prior day H {} L {} C {} · pre-market H {} L {} · overnight H {} L {} · prior week H {} L {} · gap {}",
        dim("session"),
        opt_num(s.prev_high, price),
        opt_num(s.prev_low, price),
        opt_num(s.prev_close, price),
        opt_num(s.premarket_high, price),
        opt_num(s.premarket_low, price),
        opt_num(s.overnight_high, price),
        opt_num(s.overnight_low, price),
        opt_num(s.prev_week_high, price),
        opt_num(s.prev_week_low, price),
        opt_num(s.gap_pct, |g| by_sign(g, &format!("{g:+.2}%")))
    );
    let row = |tag: &str, l: &crate::analytics::levels::Level, color: fn(&str) -> String| {
        let bar = "█".repeat(((l.strength / 10.0).round() as usize).max(1));
        println!(
            "  {} {:>9} {:>+6.2}%  {:<10} {}",
            color(tag),
            price(l.price),
            l.distance_pct,
            color(&bar),
            dim(&l.sources.join(", "))
        );
    };
    for (i, l) in r.resistances.iter().enumerate().rev() {
        row(&format!("R{}", i + 1), l, green);
    }
    println!("  {} {:>9}", bold("──"), bold(&price(r.spot)));
    for (i, l) in r.supports.iter().enumerate() {
        row(&format!("S{}", i + 1), l, red);
    }
    Ok(())
}

pub async fn collect(client: &Client, symbols: Vec<String>) -> Result<()> {
    let out = expect!(client.call(Request::Collect { symbols }).await?, Response::Collected)?;
    for (sym, outcome) in out {
        println!("{:<6} {}", bold(&sym), if outcome.starts_with("recorded") { green(&outcome) } else { dim(&outcome) });
    }
    println!("{}", dim(&format!("store: {}", crate::history::path().display())));
    Ok(())
}

pub async fn history(client: &Client, symbol: String, days: usize, json: bool) -> Result<()> {
    let h = expect!(client.call(Request::History { symbol, days }).await?, Response::History)?;
    if json {
        return print_json(&h);
    }
    match &h.iv_rank {
        Some(r) => println!(
            "{}  IV30 {:.1}%  IV rank {:.0}  percentile {:.0}  ({} days stored)",
            bold(&h.symbol),
            r.current * 100.0,
            r.rank,
            r.percentile,
            r.days
        ),
        None => {
            println!("{}  {}", bold(&h.symbol), dim(&format!("IV rank needs 5+ stored days ({} so far)", h.rows.len())))
        }
    }
    println!(
        "{}",
        dim(&format!(
            "{:<10} {:>9} {:>6} {:>7} {:>9} {:>8} {:>9} {:>8} {:>8} {:>6}",
            "date", "spot", "IV30", "ATM IV", "exp.move", "GEX", "zero γ", "call W", "put W", "P/C"
        ))
    );
    for r in &h.rows {
        println!(
            "{:<10} {:>9} {:>6} {:>7} {:>9} {:>8} {:>9} {:>8} {:>8} {:>6}",
            r.date.to_string(),
            opt_num(r.spot, price),
            opt_num(r.iv30, |v| format!("{:.1}%", v * 100.0)),
            opt_num(r.front_atm_iv, |v| format!("{:.1}%", v * 100.0)),
            opt_num(r.front_expected_move, |m| format!("±{}", price(m))),
            opt_num(r.gex, signed_big),
            opt_num(r.zero_gamma, price),
            opt_num(r.call_wall, strike),
            opt_num(r.put_wall, strike),
            opt_num(r.put_call_oi, |x| format!("{x:.2}"))
        );
    }
    if h.rows.is_empty() {
        println!("{}", dim("nothing stored yet: the daemon records after each close, or run `ox collect`"));
    }
    Ok(())
}

pub async fn brief(client: &Client, json: bool, html: Option<String>, notes: Option<String>) -> Result<()> {
    let mut b = *expect!(client.call(Request::Brief).await?, Response::Brief)?;
    if let Some(n) = notes {
        b.notes = Some(read_input(&n)?);
    }
    if let Some(path) = html {
        std::fs::write(&path, crate::brief_html::render(&b)?)?;
        println!("wrote {path}");
        return Ok(());
    }
    if json {
        return print_json(&b);
    }
    if let Some(m) = &b.market {
        println!(
            "{} SPY {} ({}) trend {} · zero γ {} · VIX {} ({})",
            bold("market"),
            price(m.spy),
            by_sign(m.spy_change_pct, &pct(m.spy_change_pct)),
            m.spy_trend,
            opt_num(m.spy_zero_gamma, price),
            opt_num(m.vix, |v| format!("{v:.1}")),
            opt_num(m.vix_change_pct, pct)
        );
        for f in &m.risk_off {
            println!("  {} {f}", red("▼"));
        }
        for f in &m.risk_on {
            println!("  {} {f}", green("▲"));
        }
    }
    if let Some(r) = &b.risk {
        for f in r.flags.iter().filter(|f| f.severity != Severity::Info) {
            println!("  {} {} {}", paint("33", "!"), bold(&f.position), f.message);
        }
    }
    println!(
        "{}",
        dim(&format!(
            "{:<6} {:>9} {:>7} {:>6} {:>10} {:>10} {:>10} {:>7} {:>9}",
            "", "price", "move", "×EM", "S1", "R1", "exp.move", "IV30", "earnings"
        ))
    );
    for s in &b.symbols {
        println!(
            "{:<6} {:>9} {} {:>6} {:>10} {:>10} {:>10} {:>7} {:>9}",
            bold(&s.symbol),
            opt_num(s.price, price),
            by_sign(s.change_pct.unwrap_or(0.0), &format!("{:>7}", opt_num(s.change_pct, |c| format!("{c:+.2}%")))),
            opt_num(s.move_vs_expected, |x| format!("{x:+.1}×")),
            opt_num(s.supports.first().map(|l| l.price), price),
            opt_num(s.resistances.first().map(|l| l.price), price),
            opt_num(s.expected_move_pct, |e| format!("±{e:.1}%")),
            opt_num(s.iv30, |v| format!("{:.0}%", v * 100.0)),
            s.earnings_in_days.map(|d| format!("{d}d")).unwrap_or_default()
        );
    }
    if let Some(r) = &b.review {
        println!("\n{}", bold("Book review"));
        for h in &r.holdings {
            println!(
                "  {:<5} {:<5} {}",
                format!("{:?}", h.action).to_uppercase(),
                h.underlying,
                dim(h.reasons.first().map_or("", |s| s.as_str()))
            );
        }
        for x in &r.replacements {
            replacement_line(x);
        }
    }
    for c in &b.changes {
        println!("  {} {c}", paint("33", "Δ"));
    }
    if !b.events.is_empty() {
        println!("\n{}", bold("Reports ahead"));
        for e in &b.events {
            println!(
                "  {:<5} {} ({}d{}) {} implied {} past {}{}",
                e.symbol,
                e.date,
                e.days,
                if e.estimated { ", est." } else { "" },
                dim(&e.role),
                opt_num(e.implied_move_pct, |x| format!("±{x:.1}%")),
                opt_num(e.past_rms_pct, |x| format!("±{x:.1}%")),
                if e.cluster.is_empty() { String::new() } else { format!(" · with {}", e.cluster.join(", ")) }
            );
        }
    }
    if !b.catalyst_due.is_empty() {
        println!("\n{}", bold("Catalyst check due"));
        for d in &b.catalyst_due {
            println!("  {:<5} {}", d.symbol, d.reasons.join("; "));
        }
    }
    Ok(())
}

pub(crate) fn sizing_line(s: &crate::analytics::sizing::Sizing) -> String {
    match &s.blocked {
        Some(why) => paint("33", &format!("size: 0 — {why}")),
        None => format!(
            "size: {} lot{} = ${:.0} ({:.1}% of equity, {}) · max {} · cash after {:.0}%{}{}",
            s.contracts,
            if s.contracts == 1 { "" } else { "s" },
            s.capital,
            s.capital_pct,
            s.binding,
            s.max_contracts,
            s.cash_after_pct,
            if s.dips_into_buffer { paint("33", " (below buffer)") } else { String::new() },
            if s.same_underlying { " · adds to existing slot" } else { "" }
        ),
    }
}

pub async fn pick(
    client: &Client,
    symbol: String,
    params: crate::analytics::pick::PickParams,
    verbose: bool,
    json: bool,
) -> Result<()> {
    use crate::analytics::pick::setup::Status;
    use crate::analytics::pick::Verdict;
    let r = *expect!(client.call(Request::Pick { symbol, params }).await?, Response::Pick)?;
    if json {
        return print_json(&r);
    }
    if !verbose {
        print!("{}", crate::pick_view::render(&r));
        return Ok(());
    }
    let g = &r.regime;
    let gamma =
        r.gamma.as_ref().and_then(|x| x.zero_gamma).map_or(String::new(), |z| {
            format!(" · {} zero γ {}", if r.spot >= z { "above" } else { "below" }, price(z))
        });
    println!(
        "{} {} · ATR {} ({:.1}%) · {} ({:+.2}) · IV {} vs RV {} {}{}",
        bold(&r.symbol),
        price(r.spot),
        price(g.atr),
        g.atr_pct,
        g.trend,
        g.trend_score,
        opt_num(g.iv30, |v| format!("{:.0}%", v * 100.0)),
        opt_num(g.rv_forecast_30d, |v| format!("{:.0}%", v * 100.0)),
        g.vol_pricing,
        gamma
    );
    let head = match r.verdict {
        Verdict::Enter => green(&r.headline),
        Verdict::Wait => paint("33", &r.headline),
        Verdict::NoTrade => red(&r.headline),
    };
    println!("\n{}", bold(&head));
    for w in &r.warnings {
        println!("  {} {w}", paint("33", "!"));
    }
    println!("\n{}", bold("Thesis"));
    for t in &r.thesis {
        println!("  • {t}");
    }
    if !r.why_not_now.is_empty() {
        println!("\n{}", bold("Why not now"));
        for t in &r.why_not_now {
            println!("  • {t}");
        }
    }

    println!("\n{}", bold("Setups"));
    for (i, s) in r.setups.iter().enumerate() {
        // ENTER/wait only for a setup with an edge; otherwise just whether its trigger has fired.
        let status = match (s.status, s.has_edge) {
            (Status::Enter, true) => green("ENTER"),
            (Status::Wait, true) => paint("33", "wait"),
            (Status::Enter, false) => dim("triggered"),
            (Status::Wait, false) => dim("not triggered"),
        };
        let edge = match (s.robust, s.has_edge) {
            (true, _) => green("edge, robust"),
            (false, true) => paint("33", "edge, tentative"),
            _ => red("no edge"),
        };
        println!(" {} {}  [{status}] [{edge}]  score {:+.2}R", i + 1, bold(&s.name), s.score);
        println!("   {} {}", dim("trigger"), s.trigger);
        let lvl = |tag: &str, l: &crate::analytics::pick::setup::PlanLevel, extra: String| {
            println!("   {:<3} {:>9}  {}{}", tag, price(l.price), dim(&l.basis), extra);
        };
        lvl("B1", &s.b1, String::new());
        if let Some(b2) = &s.b2 {
            lvl("B2", b2, String::new());
        }
        lvl("SL", &s.sl, String::new());
        lvl("T1", &s.t1, format!("  R:R {:.1}", s.rr_t1));
        lvl("T2", &s.t2, format!("  R:R {:.1}", s.rr_t2));
        let bt = &s.backtest;
        let pc = |x: f64| format!("{:.0}%", x * 100.0);
        println!(
            "   {} {} signals (~{:.0} independent){} · T1 first {} (no-edge {}) · T2 {} · stopped {} · {:+.2}R ({:+.2}R shrunk, t {:.1}) · {:+.2}R over {} (t {:.1}) · trend-only {:+.2}R, any day {:+.2}R · ~{:.0}d to T1, ~{:.0}d to T2 · time stop {}d",
            dim("history"),
            bt.signals.trades,
            bt.effective_n,
            bt.first_signal.map_or(String::new(), |d| format!(" since {d}")),
            pc(bt.signals.p_t1),
            pc(bt.no_edge_p_t1),
            pc(bt.signals.p_t2),
            pc(bt.signals.p_stop),
            bt.signals.expectancy_r,
            bt.expectancy_r_shrunk,
            bt.t_stat,
            bt.excess_r,
            bt.control.replace('_', "-"),
            bt.t_excess,
            bt.trend_only.expectancy_r,
            bt.any_day.expectancy_r,
            s.days_to_t1,
            s.days_to_t2,
            bt.hold_days
        );
        if i == 0 {
            for w in s.why.iter().filter(|w| !w.starts_with("history:") && !w.starts_with("trend:")) {
                println!("   {} {}", dim("·"), dim(w));
            }
        }
        if !s.alerts.is_empty() {
            let a: Vec<String> = s.alerts.iter().map(|a| price(a.price)).collect();
            println!("   {} {}", dim("alerts"), a.join(", "));
        }
    }

    if let Some(c) = &r.contracts {
        let lead = r.setups.first().map_or("", |s| s.name.as_str());
        println!(
            "\n{} {}",
            bold(&format!("Contracts for {lead}")),
            dim(&format!(
                "entry {} · sim vol {:.0}% · drift set to P(T1 first) {:.0}% · time stop {} · expiries ≥ {}",
                price(c.entry),
                c.sim_vol * 100.0,
                c.target_p_t1 * 100.0,
                c.time_stop,
                c.min_expiry
            ))
        );
        for w in &c.warnings {
            println!("  {} {w}", paint("33", "!"));
        }
        if let Some(b) = &r.budget {
            println!("{}", budget_line(b));
        }
        println!(
            "{}",
            dim(&format!(
                "{:>2} {:<18} {:<6} {:<24} {:>7} {:>8} {:>7} {:>6} {:>8} {:>5} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6}",
                "#",
                "structure",
                "exp",
                "legs",
                "limit",
                "max loss",
                "EV",
                "EV/R",
                "no-edge",
                "P>0",
                "@SL",
                "@T1",
                "@T2",
                "flat",
                "Δ",
                "θ/day"
            ))
        );
        for (i, p) in c.picks.iter().enumerate() {
            let legs: Vec<String> = p
                .legs
                .iter()
                .zip(&p.ratios)
                .map(|(l, q)| {
                    format!(
                        "{}{}{}{}",
                        if l.side == Side::Buy { "+" } else { "-" },
                        if *q > 1 { format!("{q}×") } else { String::new() },
                        strike(l.strike),
                        l.right.letter()
                    )
                })
                .collect();
            let net = if p.net_at_entry >= 0.0 {
                format!("{:.2}", p.net_at_entry)
            } else {
                format!("{:.2}cr", -p.net_at_entry)
            };
            let sgn = |x: f64| by_sign(x, &format!("{x:>+7.0}"));
            println!(
                "{:>2} {:<18} {:<6} {:<24} {:>7} {:>8.0} {} {:>6.2} {} {:>4.0}% {} {} {} {} {:>+6.0} {:>+6.1}",
                i + 1,
                p.structure.label(),
                p.expiry.format("%m-%d").to_string(),
                legs.join(" "),
                net,
                p.max_loss,
                sgn(p.ev),
                p.ev_on_risk,
                by_sign(p.ev_no_edge, &format!("{:>+8.0}", p.ev_no_edge)),
                p.p_profit * 100.0,
                sgn(p.pnl_at_sl),
                sgn(p.pnl_at_t1),
                sgn(p.pnl_at_t2),
                sgn(p.pnl_flat),
                p.delta,
                p.theta_day
            );
            let src = if p.live_quotes { "live quotes" } else { "delayed quotes" };
            let mut notes =
                vec![p.why.clone(), format!("{src} · friction ${:.0} · min OI {}", p.friction, big(p.min_oi))];
            notes.extend(p.notes.iter().cloned());
            println!("   {}", dim(&notes.join(" · ")));
            if let Some(s) = &p.sizing {
                println!("   {}", sizing_line(s));
            }
        }
        if let Some(p) = c.picks.first() {
            println!("   {} {}", dim("exit plan:"), p.exit_plan);
        }
        if c.picks.is_empty() {
            println!("{}", dim(&format!("no contracts passed the filters ({} evaluated)", c.evaluated)));
        } else {
            println!(
                "{}",
                dim(&format!("{} structures evaluated · $ per unit · EV after spread and commission", c.evaluated))
            );
        }
    }
    if !r.replace.is_empty() {
        println!("\n{}", bold("The book can't fund this; worth swapping out"));
        for x in &r.replace {
            replacement_line(x);
        }
    }

    println!("\n{}", bold("Zones"));
    let zone = |z: &crate::analytics::pick::zones::Zone, tag: &str| {
        let proven = z.proven();
        println!(
            "  {} {:>9} {:>+5.1} ATR  strength {:.2}{}  {}",
            tag,
            price(z.price),
            (z.price - r.spot) / g.atr,
            z.strength,
            if proven > 0 { format!(" ({proven} proven)") } else { String::new() },
            dim(&z.describe())
        );
    };
    for z in r.resistances.iter().rev() {
        zone(z, &red("R"));
    }
    println!("  {} {:>9}", dim("·"), bold(&price(r.spot)));
    for z in &r.supports {
        zone(z, &green("S"));
    }
    {
        println!(
            "\n{} {}",
            bold("Level calibration"),
            dim(&format!(
                "{} daily bars from {}",
                r.history_days,
                r.history_from.map_or("?".into(), |d| d.to_string())
            ))
        );
        println!(
            "{}",
            dim(&format!(
                "  {:<28} {:<10} {:>7} {:>6} {:>6} {:>6}",
                "source", "side", "touches", "held", "base", "lift"
            ))
        );
        for k in &r.calibration {
            println!(
                "  {:<28} {:<10} {:>7} {:>6} {:>5.0}% {}",
                k.source,
                format!("{:?}", k.side).to_lowercase(),
                k.touches,
                opt_num(k.hold_rate, |h| format!("{:.0}%", h * 100.0)),
                k.base_rate * 100.0,
                by_sign(k.lift - 1.0, &format!("{:>6.2}", k.lift))
            );
        }
    }
    Ok(())
}

fn replacement_line(x: &crate::analytics::review::Replacement) {
    print!("{}", replacement_text(x));
}

pub(crate) fn replacement_text(x: &crate::analytics::review::Replacement) -> String {
    format!(
        "  close {} ({}) → open {} {} {} ×{}  gain {} ({} if no edge)\n    {}\n",
        bold(&x.close),
        x.close_positions.join(", "),
        bold(&x.open),
        x.open_pick.structure.label(),
        x.open_pick.expiry.format("%m-%d"),
        x.lots,
        by_sign(x.gain, &format!("{:+.0}", x.gain)),
        by_sign(x.gain_no_edge, &format!("{:+.0}", x.gain_no_edge)),
        dim(&x.why)
    )
}

pub async fn review(client: &Client, scan: Vec<String>, json: bool) -> Result<()> {
    use crate::analytics::review::Action;
    let r = *expect!(client.call(Request::Review { scan }).await?, Response::Review)?;
    if json {
        return print_json(&r);
    }
    if let Some(b) = &r.budget {
        println!("{}", budget_line(b));
    }
    println!("\n{}", bold("Holdings"));
    for h in &r.holdings {
        let action = match h.action {
            Action::Hold => green("HOLD"),
            Action::Close => red("CLOSE"),
            Action::Roll => paint("33", "ROLL"),
        };
        let ev = h.hold.as_ref().map_or("—".to_string(), |e| {
            format!(
                "hold EV {} (no edge {}) to {}",
                by_sign(h.hold_ev(), &format!("{:+.0}", h.hold_ev())),
                format_args!("{:+.0}", e.ev_no_edge),
                e.horizon_end
            )
        });
        println!(
            "  {:<5} {} {}  value {:.0} ({:+.0})  {}",
            action,
            bold(&h.underlying),
            dim(&h.positions.join(", ")),
            h.market_value,
            h.unrealized,
            ev
        );
        for w in &h.reasons {
            println!("        {}", dim(w));
        }
        if let Some(p) = &h.roll_to {
            let legs: Vec<String> = p
                .legs
                .iter()
                .map(|l| {
                    format!("{}{}{}", if l.side == Side::Buy { "+" } else { "-" }, strike(l.strike), l.right.letter())
                })
                .collect();
            println!(
                "        roll to {} {} {} @ {:.2}, EV {:+.0}/lot",
                p.structure.label(),
                p.expiry,
                legs.join(" "),
                p.limit_at_entry,
                p.ev
            );
        }
    }
    println!("\n{}", bold("Candidates with a measured edge"));
    if r.candidates.is_empty() {
        println!("  {}", dim("none"));
    }
    for c in &r.candidates {
        let fit = match c.pick.as_ref().and_then(|p| p.sizing.as_ref()) {
            Some(s) if s.contracts > 0 => green(&format!("fits {} lot(s)", s.contracts)),
            Some(s) => red(s.blocked.as_deref().unwrap_or("blocked")),
            None => dim("no contract"),
        };
        println!("  {} {}  {}", bold(&c.symbol), c.headline, fit);
    }
    if !r.replacements.is_empty() {
        println!("\n{}", bold("Replacements"));
        for x in &r.replacements {
            replacement_line(x);
        }
    }
    if !r.no_trade.is_empty() {
        println!("\n{} {}", dim("no trade:"), dim(&r.no_trade.join(" ")));
    }
    for n in &r.notes {
        println!("{} {}", dim("·"), dim(n));
    }
    Ok(())
}
