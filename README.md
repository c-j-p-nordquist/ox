# ox

**What does this trade cost if I'm wrong about it?** Give ox a ticker and, if you have them, your
levels. It lays out the plan (entry, stop, two targets), says how often the first target comes
before the stop if the stock simply moves at random, and ranks option contracts by how much better
than chance the plan must be for each to break even.

ox does not say whether to trade, and it assumes no plan has an edge. It used to backtest its own
setups and answer ENTER, WAIT or NO TRADE. An audit over 59 liquid tickers showed why it almost
always said NO TRADE: its support and resistance levels held no more often than random levels,
its setups averaged 0R, and one stock's history is too short to show an edge either way. What is
left is the part that is arithmetic: the odds of a plan at random, and what each way of expressing
it costs.

```text
NVDA  181.68   downtrend · IV 57% vs 49% realized (fair) · below zero γ 185.70
ATR 8.39 (4.7%) · earnings Nov 17 in 7d

 WAITING  rally short · short
  • waiting for: price trades into the B1 resistance zone (198.90) and a daily bar closes back below
    it
  • at random: T1 173.35 comes before the stop 213.20 27% of the time, for 0R on average before
    costs
  • this stock: the same plan entered on any day in its history got T1 first 14% of the time and
    averaged -0.65R (201 entries)
  • no edge is assumed: the reason to take the trade has to be your own
  • earnings 2026-11-17 falls inside the expected holding period: gap risk
  +2 more plans in --verbose

Plan  rally short · 1R = 10.30 risk/share · R:R 2.9 to T1, 5.1 to T2 · time stop 60d
     213.20   +17.4%    SL    0.5 ATR beyond the B2 zone
     206.91   +13.9%  R B2    200d SMA + 20d EMA + fib 0.5 +2 more
     198.90    +9.5%  R B1    fib 0.382 + call wall + swing high + swing low +1 more
     186.44    +2.6%  R       zero gamma + 1y volume POC + swing high
▸    181.68                    now
     173.35    -4.6%  S T1    1y value area edge + swing low
     150.22   -17.3%  S T2    fib ext 1.272 + put wall

Contracts  for rally short, if NVDA moves at random · delayed quotes · per contract, after spread and commission
  ! option quotes are from the Mon 16:15 ET close: refresh with live quotes (`ox ingest quotes`) before ordering
  ! prices are estimated for when the underlying reaches 202.91 with today's IVs; each day of waiting costs the theta shown
  #  structure          exp    legs              price max loss  random   if T1  if not  needs   edge
  1  put debit spread   Mar 19 +280P -160P       78.35     7835    -174   +1663    -840    34%   +7pt
     short strike 160 sits at the plan's profit zone, selling 49% IV to cut cost
  2  put debit spread   Mar 19 +280P -150P       80.29     8029    -192   +1803    -915    34%   +7pt
     short strike 150 sits at the plan's profit zone, selling 50% IV to cut cost
  3  long put           Mar 19 +280P             84.78     8478    -254   +2114   -1111    34%   +8pt
     simplest expression
  +3 more in --verbose
  exit: close half when the underlying reaches T1 173.35 and the rest at T2 150.22; after T1, exit
        the rest at entry 202.91; close everything on a daily close above 213.20 or on 2027-02-05
  random: expected result if the stock moves at random · needs: how often T1 must come before the
  stop to break even · edge: that minus the 27% it happens at random (shares need +0pt)

--verbose for every plan, its history and every contract
Odds if the stock moves at random, from past volatility. Not a recommendation: ox assumes no edge.
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
ox pick NVDA            # a default plan, its odds at random, and the contracts
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

## How `ox pick` works

1. **Regime.** Trend score in [-1, 1] from seven time-series momentum / moving-average conditions;
   realized vol (Yang-Zhang), an EWMA forecast mean-reverting to the one-year level, and implied
   vs forecast realized vol (cheap / fair / rich).
2. **Levels.** Swing pivots, Fibonacci retracements and extensions, 20/50/100/200-day averages,
   volume-profile POC and value area, VWAPs anchored at the last major swing, prior week/month
   extremes, plus GEX walls and zero gamma. They are conventional places to put entries, stops and
   targets. Measured against control levels they hold no more often than chance, so ox does not
   claim price will turn at them.
3. **Plans.** Your levels (`--entry --stop --t1 --t2`, `--bias long|short`) lead when given.
   Otherwise: a pullback into the nearest level, a deeper pullback, a breakout through the level
   ahead and an entry now; without a trend, range fades both ways. Each has B1/B2 (scale-in), SL,
   and T1/T2. The plan is AT ENTRY or WAITING for its trigger; that is its state, not advice.
4. **Odds at random.** With no drift, T1 comes before the stop (entry − SL) / (T1 − SL) of the
   time and the plan averages 0R before costs. The plan's history on this stock (past signals, and
   the same plan entered on any day) is shown as description, not as evidence of an edge.
5. **Contracts.** Long calls/puts, debit spreads with the short strike at the plan's targets,
   butterflies centered on T1/T2, and credit spreads with the short strike beyond the stop. Each is
   run through a Monte Carlo of the plan on zero-drift paths: forecast realized vol, half off at
   T1, stop to breakeven, rest at T2, time stop, Black-Scholes marks with sticky-strike IV (lower
   after earnings), half the spread and commission each way. Per structure: the expected result at
   random, the result when T1 comes first and when it does not, and from those how often T1 must
   come first to break even. That minus the random rate is the edge you have to bring; the ranking
   puts the structure that needs the least first. Shares need zero.

Live broker quotes can be pushed in (`ox ingest quotes -`) and replace the delayed ones for 30
minutes. With a portfolio loaded, picks are sized against the book.

```bash
ox portfolio import pos.json   # load positions (schema below; `-` for stdin)
ox portfolio                   # marked to market
ox risk                        # Greeks, 1σ daily P&L (market vs company-specific), flags
ox review                      # HOLD or CLOSE per holding: what holding costs at random
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

ox is an arithmetic tool, not investment advice. It assumes no edge; its volatility forecast can be wrong,
data can be wrong or stale, and options can lose their entire value. You are responsible for your
own trades. The software is provided as is, without warranty (see [LICENSE](LICENSE)).

## License

MIT
