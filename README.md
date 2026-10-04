# ox

**A data-driven answer to "should I trade this, and how?"** Give ox a ticker. It reads the trend,
finds the support and resistance that has actually held on that stock, builds a trade plan,
backtests that exact plan against controls, and answers **ENTER**, **WAIT** (with the exact
trigger) or **NO TRADE**. When there is a trade, it ranks the option contracts that fit the plan.

NO TRADE is a real, frequent answer: an edge has to beat simply holding the stock, on enough
independent trades that it is unlikely to be luck.

```text
NVDA  233.95   strong uptrend · IV 29% vs 33% realized (cheap) · above zero γ 202.82
ATR 5.91 (2.5%) · earnings Nov 18 (est.) in 45d

 NO TRADE  no setup has a measured edge right now
  • trend entry now: +0.65R is no better than entering on any day (+0.75R): it is the stock's past
    drift, not a signal
  • pullback long: +0.10R could be luck: t = 0.51 on ~50 independent trades (needs 1.65)
  ! option quotes are from the last close

Closest plan  trend entry now · 1R = 25.90 risk/share · R:R 1.5 to T1, 2.5 to T2 · time stop 60d
     298.71   +27.7%     T2    2.5R projection
     272.81   +16.6%     T1    1.5R projection
     235.11    +0.5%  R        call wall + swing high + prior month high/low
▸    233.95              B1    now
     230.08    -1.7%  S        put wall + prior week high/low + swing high
     221.11    -5.5%  S        3m volume POC + prior week high/low
     217.70    -6.9%  S        swing high + 50d SMA
     208.05   -11.1%     SL    0.5 ATR beyond fib 0.382 + 100d SMA +2 more

--verbose for the backtests, evidence and every contract · --contracts to price the plan anyway
Statistics from past prices, not investment advice. Past edges can stop working.
```

Free data, no account, no broker login. Readable in a terminal and built to be driven by an AI
agent: every command has `--json`, and ox ships with skills for Claude.

## Install

macOS and Linux:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/c-j-p-nordquist/ox/releases/latest/download/ox-installer.sh | sh
```

This installs `ox` (and `ox-update`, which upgrades it) into `~/.local/bin`. Or build from source
with a Rust toolchain: `cargo install --git https://github.com/c-j-p-nordquist/ox`.

## Quick start

```bash
ox pick NVDA            # verdict, why, and the plan against the levels
ox pick NVDA -v         # the full report: every setup's backtest, every contract, level sources
ox pick NVDA --json     # everything, for scripts and agents
ox daemon start         # optional: keeps data warm so every call answers in milliseconds
```

The first `pick` for a symbol downloads five years of daily bars and calibrates its levels,
which takes a few seconds; later calls are fast.

## Use it with Claude

```bash
ox skill install            # teaches Claude Code to run ox and read its output
ox skill install ox-ibkr    # optional: sync, verify and stage trades with the IBKR connector
```

Then ask Claude things like "pick NVDA" or "review my book". ox does the statistics; Claude adds
what ox can't see (news, catalysts, macro) and keeps it separate from the numbers. For claude.ai,
`ox skill show ox > SKILL.md` gives the file to upload.

## How `ox pick` decides

1. **Regime.** Trend score in [-1, 1] from seven time-series momentum / moving-average conditions;
   realized vol (Yang-Zhang), an EWMA forecast mean-reverting to the one-year level, and implied
   vs forecast realized vol (cheap / fair / rich).
2. **Zones with measured weights.** Swing pivots, Fibonacci retracements (0.382–0.786) and
   extensions (1.272, 1.618), 20/50/100/200-day averages, volume-profile POC and value area,
   VWAPs anchored at the last major swing, prior week/month extremes, plus GEX walls and zero
   gamma. Each kind of level is scored on the stock's own five-year history: when price touched
   it, did it bounce one ATR before closing half an ATR through? The same test on control levels
   at random offsets gives the base rate, so a source's *lift* is how much better than random it
   has been. The pass is walk-forward (no look-ahead). `--verbose` prints the table.
3. **Plans.** With the trend: a pullback into the nearest qualifying zone, a deeper pullback into
   the strongest one, a breakout through the zone ahead, and a trend entry now; without a trend,
   range fades both ways. Each has B1/B2 (scale-in), SL beyond the level that would prove it wrong,
   and T1/T2 at the next zones (or R-multiple projections when no measured zone lies ahead).
4. **Backtest of the exact plan.** Half off at T1, stop to breakeven, rest at T2, time stop —
   replayed at every past signal with the zones known on that day. Compared with three controls:
   a driftless coin flip, entering any day in the same trend, and entering on any day at all
   (the stock's drift). The result is split into drift + trend filter + level timing.
5. **Verdict.** A setup has an edge with ≥ 8 signals, positive expectancy after shrinking toward
   zero by the *effective* (non-overlapping) sample, a one-sided t ≥ 1.65, and beating the
   any-day control. "Robust" additionally clears the Šidák-corrected bar for the number of setups
   tested. ENTER if a setup with an edge has triggered and waiting hasn't historically paid more;
   WAIT with the trigger if one has an edge but hasn't triggered; otherwise NO TRADE, with why.
6. **Contracts.** Long calls/puts, debit spreads with the short strike at the plan's targets,
   butterflies centered on T1/T2, and credit spreads with the short strike beyond the stop. Each is
   run through a Monte Carlo of the plan itself: forecast realized vol, a drift calibrated so the
   chance of T1 before the stop matches the backtest, the same scale-out rules, Black-Scholes marks
   with sticky-strike IV (lower after earnings), half the spread and commission each way. Ranked by
   expected P&L on max loss, shown next to the expected P&L if the setup has no edge.

Your own levels can be given (`--entry --stop --t1 --t2`, `--bias long|short`); they are
backtested the same way. Live broker quotes can be pushed in (`ox ingest quotes -`) and replace
the delayed ones for 30 minutes. With a portfolio loaded, picks are sized against the book.

What ox does not judge: news, catalysts and macro. Those are for you (or your agent) to add on top.

## Other commands

Every command accepts `--json`.

```bash
ox levels NVDA          # support/resistance by confluence + prior day, pre-market, overnight, gap
ox overview SPY         # IV30, expected move per expiry, GEX levels, P/C, max pain
ox gex SPY --dte 7      # GEX by strike, zero gamma, walls
ox chain NVDA --dte 14  # chain for the first expiry ≥ 14 days out
ox quote SPY QQQ IWM
ox earnings NVDA GOOG   # next earnings dates (estimated until the company confirms)
ox brief                # pre-market brief: backdrop, and per symbol move, levels, expected move
ox watchlist import -   # replace the watchlist (JSON or plain tickers, from stdin)
```

With a portfolio loaded:

```bash
ox portfolio import pos.json   # load positions (schema below; `-` for stdin)
ox portfolio                   # marked to market
ox risk                        # Greeks, 1σ daily P&L (market vs company-specific), flags
ox review                      # HOLD / CLOSE / ROLL per holding, candidates, replacements
```

## Portfolio

ox never logs in to a broker. You load a snapshot, by hand, from a broker export, or through an
agent with a broker connector:

```json
{
  "net_liquidation": 25000,
  "cash": 8000,
  "currency": "USD",
  "positions": [
    {"symbol": "NVDA", "sec_type": "option", "right": "C", "strike": 230, "expiry": "2026-10-16", "qty": 2, "avg_cost": 4.10},
    {"symbol": "AAPL", "sec_type": "stock", "qty": 10, "avg_cost": 320.5}
  ]
}
```

`symbol` is the underlying, `qty` is signed (negative = short), and `avg_cost` is per share (an
option bought for $410 is 4.10). Account values can be in any currency; ox converts to USD.
`excess_liquidity` (optional) is your broker's available-funds figure.

With a portfolio, `pick` sizes each contract against the book: net liquidation is 100%, a number
of concurrent positions and a per-trade target scale with account size, a 20% cash buffer is
kept, and a 10% excess-liquidity floor is never sized through.

## Configuration

`ox config init` writes the defaults to `~/.config/ox/config.toml` (or `$XDG_CONFIG_HOME/ox`):

```toml
watchlist = ["SPY", "QQQ", "NVDA"]
risk_free_rate = 0.04

[costs]
commission_per_contract = 0.65   # your broker's per-contract option commission

[sizing]
# slots = 4                  # concurrent positions (default scales with account size)
# option_target_pct = 7.0    # per-trade target, % of net liquidation
# option_cap_pct = 10.0      # per-trade hard cap
cash_buffer_pct = 20.0       # soft
liquidity_floor_pct = 10.0   # hard
# premium_budget_pct = 30.0  # optional cap on total long premium

[risk]
benchmarks = ["SPY", "QQQ"]
lookback_days = 252

[providers]
chains = "cboe"
bars = "yahoo"
quotes = "yahoo"
```

## Data and its limits

| Data | Default source | Notes |
|---|---|---|
| Option chains (IV, Greeks, OI) | CBOE delayed quotes | ~15 min delay; US-listed equities, ETFs and indices |
| Daily and intraday bars, quotes | Yahoo Finance | delayed per exchange rules |
| Real-time quote, earnings dates | Nasdaq | the symbol in focus only |

These are free public endpoints, fetched directly from your machine, for personal use. They are
not official APIs: they can change, rate-limit or disappear, and ox will need an update when they
do (`ox-update`). Option quotes outside market hours are from the last close; ox says so, and you
can push live quotes from your broker with `ox ingest quotes`.

Providers sit behind small traits (`src/provider/mod.rs`), so a licensed or real-time source can
be added without touching the analytics. Contributions welcome.

## How the numbers are computed

- **GEX** = Γ × OI × 100 × S² × 1%: dollars of dealer delta per 1% move, under the common
  convention that dealers are long calls and short puts. Gamma is recomputed from each contract's
  IV with Black-Scholes, so it is consistent across providers.
- **Zero gamma** is where net GEX changes sign as spot is swept ±15%, holding each contract's IV
  fixed (sticky-strike). **Call / put walls** are the strikes with the largest call gamma at or
  above spot and put gamma at or below it.
- **Expected move** is S × ATM IV × √T (one standard deviation); the ATM straddle is shown next to it.
- **Portfolio risk** uses the holdings' daily-return covariance (one year by default), weighted by
  position delta, split into the part the best-fitting benchmark explains and the company-specific
  rest.

## Disclaimer

ox is a statistics tool, not investment advice. Backtests describe the past; edges stop working,
data can be wrong or stale, and options can lose their entire value. You are responsible for your
own trades. The software is provided as is, without warranty (see [LICENSE](LICENSE)).

## License

MIT
