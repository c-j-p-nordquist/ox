mod analytics;
mod brief;
mod brief_html;
mod cli;
mod client;
mod config;
mod daemon;
mod engine;
mod fmt;
mod history;
mod ingest;
mod model;
mod pick_view;
mod portfolio;
mod protocol;
mod provider;
mod serve;
mod skills;
mod watchlist;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};

use crate::client::Client;
use crate::config::Config;

#[derive(Parser)]
#[command(
    name = "ox",
    version,
    about = "Options contract picker: `ox pick SYMBOL` builds a trade plan and the contracts that fit it."
)]
struct Cli {
    /// Bypass the daemon and fetch directly.
    #[arg(long, global = true)]
    local: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Price a trade plan for a symbol as if the stock moves at random.
    ///
    /// Takes your levels (--entry, --stop, --t1, --t2) or builds a pullback/breakout plan from
    /// the nearest support/resistance, then shows how often T1 comes before the stop by chance
    /// and, per option structure, how much better than chance the plan must be to break even.
    /// It does not say whether to trade: no plan is assumed to have an edge.
    Pick {
        symbol: String,
        /// Force a direction instead of following the trend.
        #[arg(long, value_enum)]
        bias: Option<analytics::pick::setup::Direction>,
        /// Your entry price (adds a plan from your levels; the rest are filled in).
        #[arg(long)]
        entry: Option<f64>,
        /// Your stop price.
        #[arg(long)]
        stop: Option<f64>,
        /// Your first target.
        #[arg(long)]
        t1: Option<f64>,
        /// Your second target.
        #[arg(long)]
        t2: Option<f64>,
        /// Maximum loss per 1-lot in dollars.
        #[arg(long)]
        max_risk: Option<f64>,
        /// Contracts to list.
        #[arg(long, default_value_t = 6)]
        limit: usize,
        /// Minimum open interest per leg.
        #[arg(long, default_value_t = 100.0)]
        min_oi: f64,
        /// Maximum bid/ask spread per leg as a fraction of mid.
        #[arg(long, default_value_t = 0.12)]
        max_spread: f64,
        /// Print the full report: thesis, every setup's backtest, all contracts, zone sources
        /// and the per-source calibration table.
        #[arg(long, short)]
        verbose: bool,
        #[arg(long)]
        json: bool,
    },
    /// Review the book: HOLD or CLOSE per holding.
    ///
    /// Each holding is simulated on random paths against closing it now: CLOSE when holding is
    /// expected to lose a material part of its value unless the stock moves the right way.
    Review {
        #[arg(long)]
        json: bool,
    },
    /// Pre-market brief: backdrop, risk, and per symbol move, levels, expected move, earnings.
    Brief {
        #[arg(long)]
        json: bool,
        /// Write a self-contained HTML page (phone-friendly) to this path.
        #[arg(long)]
        html: Option<String>,
        /// Notes (e.g. a written read) to include at the top; `-` for stdin.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Support/resistance by confluence plus session levels (prior day, pre-market, overnight, gap).
    Levels {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// One-screen summary: IV, expected moves, GEX levels, put/call, max pain.
    Overview {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// Gamma exposure by strike with zero-gamma flip and call/put walls.
    Gex {
        symbol: String,
        /// Only include expirations up to this many days out.
        #[arg(long)]
        dte: Option<u32>,
        /// Number of strikes around spot to show.
        #[arg(long, default_value_t = 30)]
        rows: usize,
        /// Heatmap by strike and expiration, with a read of where gamma is concentrated.
        #[arg(long)]
        map: bool,
        #[arg(long)]
        json: bool,
    },
    /// Open the GEX heatmap in a browser (local web page; the picker's plan drawn on it).
    Serve {
        #[arg(long, default_value_t = 8642)]
        port: u16,
        /// Don't open a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Option chain for one expiration.
    Chain {
        symbol: String,
        /// Expiration date (YYYY-MM-DD); the first on or after it is used.
        #[arg(long)]
        exp: Option<NaiveDate>,
        /// First expiration at least this many days out.
        #[arg(long)]
        dte: Option<u32>,
        /// Strikes on each side of the money (0 = all).
        #[arg(long, default_value_t = 12)]
        strikes: usize,
        #[arg(long)]
        json: bool,
    },
    /// Last price and change.
    Quote {
        #[arg(required = true)]
        symbols: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Next earnings dates.
    Earnings {
        #[arg(required = true)]
        symbols: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Portfolio snapshot marked to market.
    Portfolio {
        #[command(subcommand)]
        action: Option<PortfolioCmd>,
        #[arg(long)]
        json: bool,
    },
    /// Portfolio risk: Greeks per underlying, P&L at GEX levels and moves, and flags.
    Risk {
        #[arg(long)]
        json: bool,
    },
    /// Show or replace the watchlist (scanned by review and brief).
    Watchlist {
        #[command(subcommand)]
        action: Option<WatchlistCmd>,
        #[arg(long)]
        json: bool,
    },
    /// Push external data into ox.
    Ingest {
        #[command(subcommand)]
        what: IngestCmd,
    },
    /// Manage the background data daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
    /// Show or create the config file.
    Config {
        #[command(subcommand)]
        action: Option<ConfigCmd>,
    },
    /// Agent skills that teach Claude (or another agent) to use ox.
    Skill {
        #[command(subcommand)]
        action: SkillCmd,
    },
    /// Daily history: IV30 (with IV rank), GEX levels, expected move per session.
    #[command(hide = true)]
    History {
        symbol: String,
        #[arg(long, default_value_t = 30)]
        days: usize,
        #[arg(long)]
        json: bool,
    },
    /// Record today's history snapshot now (default: watchlist, portfolio, benchmarks).
    #[command(hide = true)]
    Collect { symbols: Vec<String> },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// List the bundled skills.
    List,
    /// Print a skill's SKILL.md, e.g. to upload it to claude.ai.
    Show { name: String },
    /// Copy skills into ~/.claude/skills (default: ox).
    Install {
        /// Skills to install (see `ox skill list`).
        names: Vec<String>,
        /// Install into this skills directory instead.
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Replace skill files that differ from this version.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum PortfolioCmd {
    /// Replace the stored portfolio with a JSON file (`-` for stdin).
    Import { file: String },
}

#[derive(Subcommand)]
enum IngestCmd {
    /// Overnight-session bars: `{symbol, bars: [{t,o,h,l,c,v}]}` or an array of them; `-` for stdin.
    Overnight { file: String },
    /// Live option quotes (e.g. from your broker) that `pick` uses for 30 minutes:
    /// `{symbol, spot?, quotes: [{occ | expiry+right+strike, bid, ask}]}`; `-` for stdin.
    Quotes { file: String },
}

#[derive(Subcommand)]
enum WatchlistCmd {
    /// Replace it from JSON (`{"name", "source", "symbols": [...]}`) or plain tickers; `-` for stdin.
    Import { file: String },
    /// Remove the synced watchlist and fall back to config.toml.
    Clear,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start in the background.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Show whether it is running.
    Status,
    /// Run in the foreground.
    Run,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Write a default config file if none exists.
    Init,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Cli::parse();
    let cfg = Config::load()?;
    let cmd = args.cmd;
    let connect = || Client::connect(&cfg, args.local);
    match cmd {
        Cmd::Quote { symbols, json } => cli::quote(&connect().await?, symbols, json).await,
        Cmd::Chain { symbol, exp, dte, strikes, json } => {
            cli::chain(&connect().await?, symbol, exp, dte, strikes, json).await
        }
        Cmd::Gex { symbol, dte, rows, map: true, json } => {
            cli::gex_map(&connect().await?, symbol, dte, rows, json).await
        }
        Cmd::Gex { symbol, dte, rows, map: false, json } => cli::gex(&connect().await?, symbol, dte, rows, json).await,
        Cmd::Serve { port, no_open } => serve::run(connect().await?, cfg.clone(), port, !no_open).await,
        Cmd::Overview { symbol, json } => cli::overview(&connect().await?, symbol, json).await,
        Cmd::Portfolio { action: Some(PortfolioCmd::Import { file }), .. } => cli::portfolio_import(&file),
        Cmd::Portfolio { action: None, json } => cli::portfolio_show(&connect().await?, json).await,
        Cmd::Risk { json } => cli::risk(&connect().await?, json).await,
        Cmd::Pick { symbol, bias, entry, stop, t1, t2, max_risk, limit, min_oi, max_spread, verbose, json } => {
            let params = analytics::pick::PickParams {
                bias,
                entry,
                stop,
                t1,
                t2,
                contracts: analytics::pick::contracts::ContractParams {
                    limit,
                    min_oi,
                    max_spread,
                    max_risk,
                    commission: cfg.costs.commission_per_contract,
                },
            };
            cli::pick(&connect().await?, symbol, params, verbose, json).await
        }
        Cmd::Review { json } => cli::review(&connect().await?, json).await,
        Cmd::Daemon { action } => match action {
            DaemonCmd::Run => daemon::run(cfg).await,
            DaemonCmd::Start => {
                let s = daemon::start().await?;
                println!("oxd running (pid {}, up {}s)", s.pid, s.uptime_secs);
                Ok(())
            }
            DaemonCmd::Stop => {
                println!("{}", if daemon::stop().await? { "oxd stopped" } else { "oxd was not running" });
                Ok(())
            }
            DaemonCmd::Status => {
                match daemon::status().await {
                    Some(s) => println!(
                        "oxd {} running (pid {}, up {}s, providers {}), cached chains: {}",
                        s.version,
                        s.pid,
                        s.uptime_secs,
                        s.providers,
                        if s.cached_chains.is_empty() { "none".into() } else { s.cached_chains.join(", ") }
                    ),
                    None => println!("oxd not running"),
                }
                Ok(())
            }
        },
        Cmd::History { symbol, days, json } => cli::history(&connect().await?, symbol, days, json).await,
        Cmd::Collect { symbols } => cli::collect(&connect().await?, symbols).await,
        Cmd::Brief { json, html, notes } => cli::brief(&connect().await?, json, html, notes).await,
        Cmd::Levels { symbol, json } => cli::levels(&connect().await?, symbol, json).await,
        Cmd::Earnings { symbols, json } => cli::earnings(&connect().await?, symbols, json).await,
        Cmd::Ingest { what: IngestCmd::Overnight { file } } => cli::ingest_overnight(&file),
        Cmd::Ingest { what: IngestCmd::Quotes { file } } => {
            for (sym, n) in ingest::save_quotes(&cli::read_input(&file)?)? {
                println!("{sym}: {n} live option quotes (used by pick for 30 minutes)");
            }
            Ok(())
        }
        Cmd::Watchlist { action: Some(WatchlistCmd::Import { file }), .. } => cli::watchlist_import(&file),
        Cmd::Watchlist { action: Some(WatchlistCmd::Clear), .. } => {
            let _ = std::fs::remove_file(watchlist::path());
            println!("synced watchlist removed; using config.toml");
            Ok(())
        }
        Cmd::Watchlist { action: None, json } => cli::watchlist_show(&connect().await?, &cfg, json).await,
        Cmd::Skill { action: SkillCmd::List } => {
            for (name, about, _) in skills::SKILLS {
                println!("{name:<8} {about}");
            }
            Ok(())
        }
        Cmd::Skill { action: SkillCmd::Show { name } } => {
            print!("{}", skills::get(&name)?);
            Ok(())
        }
        Cmd::Skill { action: SkillCmd::Install { names, dir, force } } => {
            let names = if names.is_empty() { vec!["ox".to_string()] } else { names };
            let dir = dir.unwrap_or_else(skills::default_dir);
            let written = skills::install(&names, &dir, force)?;
            if written.is_empty() {
                println!("already up to date in {}", dir.display());
            }
            for p in written {
                println!("wrote {}", p.display());
            }
            Ok(())
        }
        Cmd::Config { action } => {
            match action {
                Some(ConfigCmd::Init) => println!("{}", Config::write_default()?.display()),
                Some(ConfigCmd::Path) => println!("{}", config::config_path().display()),
                None => print!("{}", toml::to_string_pretty(&cfg)?),
            }
            Ok(())
        }
    }
}
