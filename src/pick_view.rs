//! The default `ox pick` view: where the plan stands, its odds at random, the plan against the
//! levels and the contracts, on one screen.
//! `--verbose` prints the full report (`cli::pick`), `--json` the data.

use std::fmt::Write;

use chrono::{DateTime, Datelike};

use crate::analytics::legs::Side;
use crate::analytics::pick::contracts::Pick;
use crate::analytics::pick::setup::{PlanLevel, Setup};
use crate::analytics::pick::zones::Zone;
use crate::analytics::pick::{PickReport, State};
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

pub fn render(r: &PickReport) -> String {
    let mut o = String::new();
    header(&mut o, r);
    state(&mut o, r);
    if let Some(lead) = r.setups.first() {
        ladder(&mut o, r, lead);
    }
    if let Some(c) = &r.contracts {
        contracts(&mut o, r, c);
    }
    let _ = writeln!(o, "\n{}", dim("--verbose for every plan, its history and every contract"));
    let _ = writeln!(
        o,
        "{}",
        dim("Odds if the stock moves at random, from past volatility. Not a recommendation: ox assumes no edge.")
    );
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

fn state(o: &mut String, r: &PickReport) {
    let badge = match r.state {
        State::AtEntry => paint("1;7;36", " AT ENTRY "),
        State::Waiting => paint("1;7;33", " WAITING "),
        State::NoPlan => paint("1;7;90", " NO PLAN "),
    };
    let _ = writeln!(o);
    let Some(s) = r.setups.first() else {
        let _ = writeln!(o, "{badge} no levels near {} to build a plan from", price(r.spot));
        return;
    };
    let side = if s.direction.d() > 0.0 { "long" } else { "short" };
    let _ = writeln!(o, "{badge} {} {}", bold(&s.name), dim(&format!("· {side}")));
    let bt = &s.backtest;
    let pc = |x: f64| format!("{:.0}%", x * 100.0);
    // The simulated rate (it includes the time stop) when contracts were priced, else the
    // distance ratio from where the plan is entered.
    let p0 = r.contracts.as_ref().map_or(s.p_t1_random, |c| c.p_t1_no_edge);
    let mut lines: Vec<String> = Vec::new();
    if r.state == State::Waiting {
        lines.push(format!("waiting for: {}", s.trigger));
    }
    lines.push(format!(
        "{}: T1 {} comes before the stop {} {} of the time, for 0R on average before costs",
        bold("at random"),
        price(s.t1.price),
        price(s.sl.price),
        pc(p0)
    ));
    if bt.any_day.trades >= 8 {
        lines.push(format!(
            "{}: the same plan entered on any day in its history got T1 first {} of the time and averaged {:+.2}R ({} entries)",
            bold("this stock"),
            pc(bt.any_day.p_t1),
            bt.any_day.expectancy_r,
            bt.any_day.trades
        ));
    }
    lines.push("no edge is assumed: the reason to take the trade has to be your own".into());
    lines.extend(s.why.iter().filter(|w| w.starts_with("earnings")).cloned());
    for w in lines {
        let _ = writeln!(o, "{}", wrap(&format!("  {} ", dim("•")), &w, 4));
    }
    if r.setups.len() > 1 {
        let n = r.setups.len() - 1;
        let _ = writeln!(o, "  {}", dim(&format!("+{n} more plan{} in --verbose", if n == 1 { "" } else { "s" })));
    }
    for w in &r.warnings {
        let _ = writeln!(o, "  {} {w}", yellow("!"));
    }
}

/// The lead plan drawn against the zones, top to bottom, with the spot marked.
fn ladder(o: &mut String, r: &PickReport, s: &Setup) {
    struct Row {
        price: f64,
        zone: Option<bool>, // is resistance
        desc: String,
        tags: Vec<&'static str>,
        spot: bool,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (z, res) in r.resistances.iter().take(3).map(|z| (z, true)).chain(r.supports.iter().take(3).map(|z| (z, false)))
    {
        rows.push(Row { price: z.price, zone: Some(res), desc: short_desc(z), tags: vec![], spot: false });
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
                let zone = l.zone.as_ref().map(|z| z.price > r.spot);
                rows.push(Row { price: l.price, zone, desc, tags: vec![tag], spot: false });
            }
        }
    }
    rows.sort_by(|a, b| b.price.total_cmp(&a.price));

    let risk = s.risk_per_share;
    let title = "Plan";
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
            Some(true) => red("R"),
            Some(false) => green("S"),
            None => " ".into(),
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
            "for {lead}, if {} moves at random · {} · per contract, after spread and commission",
            r.symbol,
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
            "  {:>1}  {:<18} {:<6} {:<14} {:>8} {:>8} {:>7} {:>7} {:>7} {:>6} {:>6}",
            "#", "structure", "exp", "legs", "price", "max loss", "random", "if T1", "if not", "needs", "edge"
        ))
    );
    for (i, p) in c.picks.iter().take(3).enumerate() {
        let net =
            if p.net_at_entry >= 0.0 { format!("{:.2}", p.net_at_entry) } else { format!("{:.2}cr", -p.net_at_entry) };
        let _ = writeln!(
            o,
            "  {}  {:<18} {:<6} {:<14} {:>8} {:>8.0} {} {} {} {:>5.0}% {}",
            i + 1,
            p.structure.label(),
            p.expiry.format("%b %-d").to_string(),
            legs(p),
            net,
            p.max_loss,
            by_sign(p.ev_no_edge, &format!("{:>+7.0}", p.ev_no_edge)),
            by_sign(p.ev_if_t1, &format!("{:>+7.0}", p.ev_if_t1)),
            by_sign(p.ev_if_not, &format!("{:>+7.0}", p.ev_if_not)),
            p.breakeven_p_t1 * 100.0,
            by_sign(-p.edge_needed, &format!("{:>+4.0}pt", p.edge_needed * 100.0))
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
    let legend = format!(
        "random: expected result if the stock moves at random · needs: how often T1 must come before the stop to break even · edge: that minus the {:.0}% it happens at random (shares need +0pt)",
        c.p_t1_no_edge * 100.0
    );
    let _ = writeln!(o, "{}", dim(&wrap("  ", &legend, 2)));
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
        assert_eq!(short_basis("1.5R projection: no level ahead"), "1.5R projection");
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
