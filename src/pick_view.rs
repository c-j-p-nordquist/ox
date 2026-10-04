//! The default `ox pick` view: the verdict, why, and the plan against the levels on one screen.
//! `--verbose` prints the full report (`cli::pick`), `--json` the data.

use std::fmt::Write;

use chrono::{DateTime, Datelike};

use crate::analytics::legs::Side;
use crate::analytics::pick::contracts::Pick;
use crate::analytics::pick::setup::{PlanLevel, Setup, SetupKind};
use crate::analytics::pick::zones::Zone;
use crate::analytics::pick::{PickReport, Verdict};
use crate::cli::{bold, by_sign, dim, green, paint, red, sizing_line, yellow};
use crate::fmt::{price, strike};

/// Widest description in the level ladder.
const DESC: usize = 56;
/// Wrap width for prose lines.
const WIDTH: usize = 100;

/// Keep whole " + "-joined sources while they fit in `n` chars; count the rest.
fn fit_sources(s: &str, n: usize) -> String {
    let parts: Vec<&str> = s.split(" + ").collect();
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        let more = parts.len() - i - 1;
        let next = if out.is_empty() { p.to_string() } else { format!("{out} + {p}") };
        let tail = if more > 0 { format!(" +{more} more") } else { String::new() };
        if !out.is_empty() && next.chars().count() + tail.chars().count() > n {
            return format!("{out} +{} more", parts.len() - i);
        }
        out = next;
    }
    out
}

/// Word-wrap `text` to `WIDTH`, starting after `first` and indenting continuations by `indent`.
fn wrap(first: &str, text: &str, indent: usize) -> String {
    let mut out = first.to_string();
    let mut col = indent;
    for (i, w) in text.split(' ').enumerate() {
        let len = w.chars().count();
        if i > 0 && col + 1 + len > WIDTH {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            col = indent;
        } else if i > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(w);
        col += len;
    }
    out
}

/// Zone descriptions without the exact price of each source: "call wall + swing high".
fn short_desc(z: &Zone) -> String {
    let mut seen = std::collections::HashSet::new();
    z.sources.iter().filter(|s| seen.insert(s.kind)).map(|s| strip_price(&s.label)).collect::<Vec<_>>().join(" + ")
}

fn strip_price(label: &str) -> String {
    match label.rsplit_once(' ') {
        Some((head, tail)) if tail.parse::<f64>().is_ok() => head.to_string(),
        _ => label.to_string(),
    }
}

/// A plan level's basis without source prices or the explanation after the colon:
/// "beyond fib 0.382 213.35 + 100d SMA 213.87 by 0.5 ATR" → "0.5 ATR beyond fib 0.382 + 100d SMA".
fn short_basis(basis: &str) -> String {
    let head = basis.split(':').next().unwrap_or(basis);
    let strip = |x: &str| x.split(" + ").map(strip_price).collect::<Vec<_>>().join(" + ");
    match head.strip_prefix("beyond ").and_then(|x| x.rsplit_once(" by ")) {
        Some((what, by)) => format!("{by} beyond {}", strip(what)),
        None => strip(head),
    }
}

/// One-line evidence for a setup: sample, expectancy, and the margin over its control.
fn evidence(s: &Setup) -> String {
    let bt = &s.backtest;
    let control = if bt.control == "trend_only" { "entering on any day in the trend" } else { "entering on any day" };
    format!(
        "{} past signals (~{:.0} independent) averaged {:+.2}R, {:+.2}R more than {control} (t {:.1})",
        bt.signals.trades, bt.effective_n, bt.signals.expectancy_r, bt.excess_r, bt.t_excess
    )
}

fn edge_tag(s: &Setup) -> String {
    match (s.robust, s.has_edge) {
        (true, _) => green("robust edge"),
        (false, true) => yellow("tentative edge"),
        _ => red("no edge"),
    }
}

pub fn render(r: &PickReport) -> String {
    let mut o = String::new();
    header(&mut o, r);
    verdict(&mut o, r);
    if let Some(lead) = r.setups.first() {
        ladder(&mut o, r, lead);
    }
    if let Some(c) = &r.contracts {
        contracts(&mut o, r, c);
    }
    if !r.replace.is_empty() {
        let _ = writeln!(o, "\n{}", bold("The book can't fund this; worth swapping out"));
        for x in &r.replace {
            o.push_str(&crate::cli::replacement_text(x));
        }
    }
    let mut more = vec!["--verbose for the backtests, evidence and every contract"];
    if r.contracts.is_none() && !r.setups.is_empty() {
        more.push("--contracts to price the plan anyway");
    }
    let _ = writeln!(o, "\n{}", dim(&more.join(" · ")));
    let _ = writeln!(o, "{}", dim("Statistics from past prices, not investment advice. Past edges can stop working."));
    o
}

fn header(o: &mut String, r: &PickReport) {
    let g = &r.regime;
    let trend = match g.trend_score {
        x if x >= 0.3 => green(&g.trend),
        x if x <= -0.3 => red(&g.trend),
        _ => g.trend.clone(),
    };
    let mut facts = vec![trend];
    if let Some(iv) = g.iv30 {
        let rv = g.rv_forecast_30d.map_or(String::new(), |rv| format!(" vs {:.0}% realized", rv * 100.0));
        facts.push(format!("IV {:.0}%{rv} ({})", iv * 100.0, g.vol_pricing));
    }
    if let Some(z) = r.gamma.as_ref().and_then(|x| x.zero_gamma) {
        facts.push(format!("{} zero γ {}", if r.spot >= z { "above" } else { "below" }, price(z)));
    }
    let _ = writeln!(o, "{}  {}   {}", bold(&r.symbol), bold(&price(r.spot)), facts.join(dim(" · ").as_str()));

    let mut sub = vec![format!("ATR {} ({:.1}%)", price(g.atr), g.atr_pct)];
    if let Some(e) = &r.earnings {
        let today = DateTime::from_timestamp(r.as_of, 0).map_or(e.date, |t| t.date_naive());
        let days = (e.date - today).num_days();
        let when = format!(
            "earnings {} {}{} in {days}d",
            e.date.format("%b"),
            e.date.day(),
            if e.estimated { " (est.)" } else { "" }
        );
        sub.push(if days <= 14 { yellow(&when) } else { when });
    }
    let _ = writeln!(o, "{}", dim(&sub.join(" · ")));
}

fn verdict(o: &mut String, r: &PickReport) {
    let badge = match r.verdict {
        Verdict::Enter => paint("1;7;32", " ENTER "),
        Verdict::Wait => paint("1;7;33", " WAIT "),
        Verdict::NoTrade => paint("1;7;31", " NO TRADE "),
    };
    let lead = r.setups.first();
    let pad = " ".repeat(match r.verdict {
        Verdict::Enter => 8,
        Verdict::Wait => 7,
        Verdict::NoTrade => 11,
    });
    let _ = writeln!(o);
    match (r.verdict, lead) {
        (Verdict::NoTrade, Some(_)) => {
            let _ = writeln!(o, "{badge} no setup has a measured edge right now");
        }
        (Verdict::NoTrade, None) => {
            let _ = writeln!(o, "{badge} no proven levels near {} to build a plan from", price(r.spot));
        }
        (_, Some(s)) => {
            let at = if r.verdict == Verdict::Enter && matches!(s.kind, SetupKind::TrendNow | SetupKind::TrendUnderZone)
            {
                " at market"
            } else {
                ""
            };
            let _ = writeln!(o, "{badge} {}{at}  {}", bold(&s.name), edge_tag(s));
            match (r.verdict, at.is_empty()) {
                (Verdict::Wait, _) => {
                    let _ = writeln!(o, "{pad}wait for: {}", s.trigger);
                }
                (_, true) => {
                    let _ = writeln!(o, "{pad}triggered: {}", s.trigger);
                }
                _ => {}
            }
        }
        (_, None) => {}
    }

    // Why: the evidence for a trade, or each setup's reason for not being one.
    let mut why: Vec<String> = Vec::new();
    match (r.verdict, lead) {
        (Verdict::NoTrade, _) => {
            if r.bias.is_none() {
                why.push(format!("no trend to lean on ({}): only fades at the range edges qualify", r.regime.trend));
            }
            for s in r.setups.iter().take(3) {
                if let Some(w) = s.why_not_now.last() {
                    why.push(format!("{}: {w}", bold(&s.name)));
                }
            }
        }
        (_, Some(s)) => {
            why.push(evidence(s));
            if !s.edge_source.is_empty() {
                why.push(format!("edge source: {}", s.edge_source));
            }
            let prefix = format!("{}: ", s.name);
            why.extend(r.why_not_now.iter().filter_map(|w| w.strip_prefix(&prefix)).map(String::from));
            why.extend(
                s.why.iter().filter(|w| w.starts_with("earnings") || w.starts_with("past signals held")).cloned(),
            );
        }
        _ => {}
    }
    for w in why {
        let _ = writeln!(o, "{}", wrap(&format!("  {} ", dim("•")), &w, 4));
    }
    if r.verdict == Verdict::NoTrade && r.setups.len() > 3 {
        let _ = writeln!(o, "  {}", dim(&format!("+{} more setups in --verbose", r.setups.len() - 3)));
    }
    for w in &r.warnings {
        let _ = writeln!(o, "  {} {w}", yellow("!"));
    }
}

/// The lead plan drawn against the zones, top to bottom, with the spot marked.
fn ladder(o: &mut String, r: &PickReport, s: &Setup) {
    struct Row {
        price: f64,
        zone: Option<(bool, bool)>, // (is resistance, proven)
        desc: String,
        tags: Vec<&'static str>,
        spot: bool,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (z, res) in r.resistances.iter().take(3).map(|z| (z, true)).chain(r.supports.iter().take(3).map(|z| (z, false)))
    {
        rows.push(Row {
            price: z.price,
            zone: Some((res, z.proven() > 0)),
            desc: short_desc(z),
            tags: vec![],
            spot: false,
        });
    }
    rows.push(Row { price: r.spot, zone: None, desc: String::new(), tags: vec![], spot: true });
    let plan: [(&'static str, Option<&PlanLevel>); 5] =
        [("B1", Some(&s.b1)), ("B2", s.b2.as_ref()), ("SL", Some(&s.sl)), ("T1", Some(&s.t1)), ("T2", Some(&s.t2))];
    for (tag, l) in plan.into_iter().filter_map(|(t, l)| l.map(|l| (t, l))) {
        let near = |p: f64| (p - l.price).abs() < 0.01;
        match rows.iter_mut().find(|row| near(row.price)) {
            Some(row) => row.tags.push(tag),
            None => {
                let desc = l.zone.as_ref().map_or_else(|| short_basis(&l.basis), short_desc);
                let zone = l.zone.as_ref().map(|z| (z.price > r.spot, z.proven() > 0));
                rows.push(Row { price: l.price, zone, desc, tags: vec![tag], spot: false });
            }
        }
    }
    rows.sort_by(|a, b| b.price.total_cmp(&a.price));
    let any_proven = rows.iter().any(|r| matches!(r.zone, Some((_, true))));

    let risk = s.risk_per_share;
    let title = if r.verdict == Verdict::NoTrade { "Closest plan" } else { "Plan" };
    let _ = writeln!(
        o,
        "\n{}  {}",
        bold(title),
        dim(&format!(
            "{} · 1R = {} risk/share · R:R {:.1} to T1, {:.1} to T2 · time stop {}d",
            s.name,
            price(risk),
            s.rr_t1,
            s.rr_t2,
            s.backtest.hold_days
        ))
    );
    for row in rows {
        let tags = row.tags.join(" ");
        if row.spot {
            let _ = writeln!(
                o,
                "{} {}  {:>7}     {} {}",
                bold("▸"),
                bold(&format!("{:>9}", price(row.price))),
                "",
                yellow(&format!("{tags:<5}")),
                dim("now")
            );
            continue;
        }
        let pct = (row.price / r.spot - 1.0) * 100.0;
        let mark = match row.zone {
            Some((true, proven)) => red(if proven { "R★" } else { "R " }),
            Some((false, proven)) => green(if proven { "S★" } else { "S " }),
            None => "  ".into(),
        };
        let _ = writeln!(
            o,
            "  {:>9}  {}  {mark} {} {}",
            price(row.price),
            dim(&format!("{pct:>+6.1}%")),
            yellow(&format!("{tags:<5}")),
            dim(&fit_sources(&row.desc, DESC))
        );
    }
    if any_proven {
        let _ = writeln!(o, "  {}", dim("★ a level type that has held more often than control levels on this stock"));
    }
}

fn legs(p: &Pick) -> String {
    p.legs
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
        .collect::<Vec<_>>()
        .join(" ")
}

fn contracts(o: &mut String, r: &PickReport, c: &crate::analytics::pick::contracts::ContractsReport) {
    let lead = r.setups.first().map_or("", |s| s.name.as_str());
    let live = c.picks.first().is_some_and(|p| p.live_quotes);
    let _ = writeln!(
        o,
        "\n{}  {}",
        bold("Contracts"),
        dim(&format!(
            "for {lead} · {} · per contract, after spread and commission",
            if live { "live quotes" } else { "delayed quotes" }
        ))
    );
    for w in &c.warnings {
        let _ = writeln!(o, "  {} {w}", yellow("!"));
    }
    if c.picks.is_empty() {
        let _ = writeln!(o, "  {}", dim(&format!("no contracts passed the filters ({} evaluated)", c.evaluated)));
        return;
    }
    let _ = writeln!(
        o,
        "{}",
        dim(&format!(
            "  {:>1}  {:<18} {:<6} {:<14} {:>8} {:>8} {:>7} {:>9} {:>5}",
            "#", "structure", "exp", "legs", "price", "max loss", "EV", "no-edge", "P>0"
        ))
    );
    for (i, p) in c.picks.iter().take(3).enumerate() {
        let net =
            if p.net_at_entry >= 0.0 { format!("{:.2}", p.net_at_entry) } else { format!("{:.2}cr", -p.net_at_entry) };
        let _ = writeln!(
            o,
            "  {}  {:<18} {:<6} {:<14} {:>8} {:>8.0} {} {} {:>4.0}%",
            i + 1,
            p.structure.label(),
            p.expiry.format("%b %-d").to_string(),
            legs(p),
            net,
            p.max_loss,
            by_sign(p.ev, &format!("{:>+7.0}", p.ev)),
            by_sign(p.ev_no_edge, &format!("{:>+9.0}", p.ev_no_edge)),
            p.p_profit * 100.0
        );
        let _ = writeln!(o, "     {}", dim(p.why.split(';').next().unwrap_or(&p.why)));
    }
    if c.picks.len() > 3 {
        let _ = writeln!(o, "  {}", dim(&format!("+{} more in --verbose", c.picks.len() - 3)));
    }
    let p = &c.picks[0];
    if let Some(s) = &p.sizing {
        let _ = writeln!(o, "  #1 {}", sizing_line(s));
    }
    let _ = writeln!(o, "{}", wrap(&format!("  {} ", dim("exit:")), &p.exit_plan, 8));
    let _ = writeln!(
        o,
        "  {}",
        dim("EV uses the plan's backtested hit rate; no-edge is the same contract if the stock moves randomly")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_source_prices() {
        assert_eq!(strip_price("call wall 235.00"), "call wall");
        assert_eq!(strip_price("fib 0.382 213.35"), "fib 0.382");
        assert_eq!(strip_price("prior week high/low"), "prior week high/low");
    }

    #[test]
    fn shortens_plan_basis() {
        assert_eq!(
            short_basis("beyond fib 0.382 213.35 + 100d SMA 213.87 by 0.5 ATR"),
            "0.5 ATR beyond fib 0.382 + 100d SMA"
        );
        assert_eq!(short_basis("1.5R projection: no zone ahead with a measured hold rate"), "1.5R projection");
        assert_eq!(
            short_basis("beyond the B1 zone by 0.5 ATR: a close there means the level failed"),
            "0.5 ATR beyond the B1 zone"
        );
    }

    #[test]
    fn fits_whole_sources() {
        assert_eq!(fit_sources("call wall + swing high", 40), "call wall + swing high");
        assert_eq!(fit_sources("call wall + swing high + prior month high/low", 34), "call wall + swing high +1 more");
        assert_eq!(fit_sources("a very long single source name", 10), "a very long single source name");
    }

    #[test]
    fn wraps_with_hanging_indent() {
        let w = wrap("- ", &"word ".repeat(30), 2);
        assert!(w.lines().all(|l| l.chars().count() <= WIDTH));
        assert!(w.lines().skip(1).all(|l| l.starts_with("  word")));
    }

    /// Renders a saved `ox pick --json` report: `OX_PICK_JSON=file cargo test render_file -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn render_file() {
        let path = std::env::var("OX_PICK_JSON").expect("OX_PICK_JSON");
        let r: PickReport = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        println!("{}", render(&r));
    }
}
