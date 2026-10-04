---
name: ox
description: Use the `ox` options CLI — above all `ox pick SYM`, which builds a backtested trade plan (B1/B2/SL/T1/T2, ENTER / WAIT with triggers / NO TRADE) and ranks the option contracts that fit it. Use whenever the user types a ticker to "pick", asks for a plan or the best options for a ticker, asks about a ticker's options, gamma/GEX, expected move or levels, wants their book reviewed, or wants positions loaded into ox.
---

# Using `ox` as an analyst

`ox` is a terminal options tool. Every analysis command has `--json`; always use it (the plain
output is a summary for humans). Data is free and **delayed ~15 min** (CBOE chains, Yahoo bars)
unless the user's config says otherwise; option quotes outside market hours are from the last
close. Say so when timing matters.

Start the daemon first so each call answers in milliseconds: `ox daemon start` (idempotent).
If `ox` is not on PATH, tell the user how to install it (see the README) rather than guessing.

## `ox pick SYM` — the main job

ox does the statistics; you add judgment, live prices if the user has a broker connection, and
the write-up.

### What ox computes (and how to read it)

- `verdict`: `enter` (a setup with a measured edge has triggered), `wait` (one has an edge but its
  trigger has not fired — `headline` says exactly what to wait for), `no_trade` (no setup has a
  measured edge; `why_not_now` says why). NO TRADE is a normal, frequent answer. Do not argue the
  user into a trade the numbers don't support.
- `setups[]`, leading one first: `name`, `status`, `trigger` (checkable: "a daily bar closes back
  above 230.08"), `b1`/`b2` (scale-in entries), `sl`, `t1`/`t2`, each with `basis` (the zone's
  sources or the rule) — quote these when explaining. `alerts[]` are the prices to watch.
  `status` says only whether the trigger fired; `has_edge` says whether it is worth trading.
- Zones: support/resistance from swing pivots, fib retracements/extensions, MAs, volume profile,
  anchored VWAPs, prior week/month, plus GEX walls/zero gamma. Each source has a `lift`: how often
  that kind of level held on **this stock** vs a random level (1.0 = no better than random; options
  levels get a fixed 1.1 and are marked "not backtested"). `ox pick SYM --verbose` prints the table.
- `backtest` per setup: the exact plan (half at T1, stop to breakeven, rest at T2, time stop)
  replayed at every past signal. Three controls: `no_edge_p_t1` (coin flip with these distances),
  `trend_only` (enter any day in the same trend) and `any_day` (enter any day at all = the stock's
  drift). `edge_source` splits the result into drift + trend filter + level timing.
- `has_edge` needs ≥ 8 signals, shrunk expectancy > 0.05R, t ≥ 1.65 on the *effective*
  (non-overlapping) sample, **and** `t_excess` ≥ 1.65: the result must beat its `control` by a
  margin unlikely to be luck. The control is `any_day` for a plain trend entry and the better of
  `trend_only`/`any_day` for level setups. `excess_r` is the margin. `robust` = both t's also clear
  the Šidák bar for the number of setups tested; `has_edge` without `robust` is tentative — say so.
- `earnings_move` (when a report is ahead): `date` (`estimated` = projected), `implied_move_pct`
  (1σ report move priced by the options), `past[]` reaction-day moves and `past_rms_pct`. The
  contract simulation jumps by `sim_move_pct` after the report and drops IV by that event variance.
  Estimated dates can be a week off: confirm against the company's announcement.
- `backtest.through_earnings` / `without_earnings`: past signals split by whether a report fell
  inside the trade.
- `regime`: trend score and signals, ATR, realized vol, EWMA vol forecast, IV vs forecast RV
  (`vol_pricing` cheap/fair/rich), IV rank once 60+ days are recorded.
- `contracts` (for the leading setup; `--contracts` forces it on NO TRADE): Monte Carlo of the plan
  with forecast realized vol and a drift calibrated to the backtest's P(T1 first). Per pick:
  `net_at_entry`/`limit_at_entry` (priced at the trigger price if waiting), `ev`, `ev_on_risk` (rank
  key), `ev_no_edge` (what it costs if the setup is worthless), `p_profit`, P&L at SL/T1/T2/flat,
  `exit_plan`, `why`, `notes`, `live_quotes`, `sizing` (when a portfolio is loaded).

### Your part

1. `ox daemon start`, then `ox pick SYM --json`. If the user gave levels or a direction, pass
   `--entry/--stop/--t1/--t2/--bias` (adds a "your levels" plan, backtested the same way).
2. **What ox cannot see**: news and catalysts, the macro calendar (FOMC, CPI, jobs) inside the
   holding period, sector moves, and the user's other holdings unless a portfolio is loaded. Keep
   this layer separate from ox's numbers. If it contradicts the plan, say so and re-run with
   overrides or downgrade the verdict — explain which and why. Never upgrade NO TRADE to ENTER on
   narrative alone.
3. **Live prices**, if you have a broker or market-data connection: take live bid/ask for the top
   picks' legs and push them in, then re-run `ox pick SYM --json` (picks with `live_quotes: true`
   are priced on them; live quotes expire after 30 minutes):
   `echo '{"symbol":"NVDA","source":"broker","spot":233.9,"quotes":[{"occ":"NVDA261218C00250000","bid":10.1,"ask":10.4}]}' | ox ingest quotes -`
   (or `{"expiry":"2026-12-18","right":"C","strike":250,...}` per quote). Without live prices, say
   that contract prices are delayed.
4. **Write it up**:
   - Verdict and headline first. For WAIT: the trigger in plain words ("not now — if NVDA trades
     down to 213 (fib 0.382 + 100d SMA) and a daily bar closes back above it"), plus why not now.
     For NO TRADE: the reason (no edge / drift only / could be luck) and what would change it.
   - The thesis: ox's `thesis` lines, then your qualitative layer, clearly labelled.
   - The plan: B1/B2/SL/T1/T2 with `basis` and R:R; the backtest line with its controls.
   - 1–3 contracts: why this structure for this plan (vol pricing, where the short strike sits vs
     T1/T2, theta if flat, `ev_no_edge`), limit price, sizing, exit plan.
   - These are statistics from past prices, not advice; the user decides and places any order.

## Other commands

| Need | Command |
|---|---|
| One-shot summary | `ox overview SYM --json` — spot, IV30, per-expiry ATM IV / straddle / 1σ move / max pain / P/C, GEX levels |
| Gamma levels | `ox gex SYM [--dte N] [--rows 40] --json` — `zero_gamma`, `call_wall`, `put_wall`, per-strike GEX, `profile` |
| Chain for one expiry | `ox chain SYM --dte N` or `--exp YYYY-MM-DD` `[--strikes 15] --json` |
| Quotes | `ox quote SYM ... --json` |
| Support / resistance | `ox levels SYM --json` — confluence levels with `strength` and `sources`, plus session levels and gap |
| Earnings | `ox earnings SYM ... --json` — next date, `estimated` until the company confirms |
| Pre-market brief | `ox brief --json` — market backdrop, per symbol (benchmarks, positions, watchlist) move vs expected move, nearest S1/R1, IV30, earnings. With a portfolio: the book `review`, `changes` since the last brief, `events` (reports ahead) and `catalyst_due`. `--notes F --html F` writes a phone-friendly page |
| Watchlist | `ox watchlist --json`; `ox watchlist import -` (JSON or plain tickers); `ox watchlist clear` |
| Portfolio | `ox portfolio --json`; `ox portfolio import -` (schema below) |
| Portfolio risk | `ox risk --json` — backdrop, `book_risk` (1σ daily P&L, market vs company-specific), `directional`, per-underlying Greeks and β, `budget`, flags |
| Book review | `ox review [SYM ...] --json` — see below |
| Plan + contracts | `ox pick SYM [--bias long|short] [--entry P --stop P --t1 P --t2 P] [--max-risk USD] [--contracts] [--verbose] --json` |
| Live option quotes in | `ox ingest quotes -` — bid/ask that `pick` uses for 30 minutes |
| Overnight bars in | `ox ingest overnight -` — `[{"symbol","source","bars":[{"t","o","h","l","c","v"}]}]`; `ox levels` adds the overnight range for 24 hours |
| Config | `ox config` (show), `ox config init`, `ox config path` |

Interpretation notes:
- GEX is $ delta change per 1% move under the dealer-long-calls/short-puts convention. Spot above
  `zero_gamma` → dealers dampen moves; below → moves extend.
- `expected_move` is 1σ from ATM IV (`S·σ·√T`); `straddle` is the market price of the ATM straddle.

## Loading a portfolio

ox has no broker login. Positions come in as a snapshot, from a broker export, a script, or you
reading the user's broker through a connector:

```json
{
  "as_of": "2026-09-28T14:05:00Z",
  "net_liquidation": 25000.0,
  "cash": 8000.0,
  "excess_liquidity": 7500.0,
  "currency": "USD",
  "positions": [
    {"symbol": "NVDA", "sec_type": "option", "right": "C", "strike": 230, "expiry": "2026-10-16", "qty": 2, "avg_cost": 4.10},
    {"symbol": "AAPL", "sec_type": "stock", "qty": 10, "avg_cost": 320.50}
  ]
}
```

- `symbol` is the **underlying** ticker; `qty` is signed (negative = short).
- `avg_cost` is **per share** (an option bought at $410 per contract is 4.10).
- `net_liquidation`, `cash`, `excess_liquidity` (optional) and `currency` are in the account's base
  currency; ox converts to USD.
- Pipe it into `ox portfolio import -`, then check `ox portfolio --json` for anything that failed
  to mark (`mark: null`).

## Sizing (ox does the arithmetic, you do the judgment)

With a portfolio loaded, `budget` (in `ox risk` and `ox pick`) uses net liquidation = 100%: equity
tier → slots and a per-trade target/cap, a 20% cash buffer (soft) and a 10% excess-liquidity floor
(hard). All overridable in `[sizing]` in the config (`ox config path`). Each pick's `sizing` gives
`contracts` (at the target), `max_contracts` (hard limits), the `binding` limit,
`dips_into_buffer`, and `blocked` when nothing fits.
- Never exceed `max_contracts`. Say why when you suggest a different size.
- `blocked` is an answer: say which limit and what would free room, or a smaller structure.
- If `dips_into_buffer` is true, say so explicitly.

## Reviewing the book: hold, close, roll, replace

`ox review --json` judges each holding as if it were a fresh trade: `pick` in the holding's
direction, then the held contracts simulated through that plan against closing them now. Sunk cost
is ignored.

- `holdings[]`: `action` HOLD / CLOSE / ROLL with `reasons`; `hold` has `ev_plan`, `ev_no_edge`,
  `horizon_end`, `theta_day`, `close_friction`. The decision uses `ev_plan` only when the direction
  has a measured edge, else `ev_no_edge`.
  - CLOSE: no measured edge and holding is expected to lose > 5% of value (min $10) by the horizon.
  - ROLL: the edge exists but these contracts lose under it; `roll_to` is the best contract.
  - HOLD without an edge means holding is cheap, not that it is right: ask for the user's thesis.
- `candidates[]`: watchlist names (or the symbols given) with ENTER/WAIT verdicts, sized.
- `replacements[]`: ENTER candidates the book can't fund, paired with the holding whose close
  funds them for the largest expected `gain`; `gain_no_edge` is the same if the new setup is
  worthless.

Present these as proposals: a CLOSE is still the user's call.

## Levels, sessions and events

- Charts include pre-market and after-hours (04:00–20:00 ET); VWAP is anchored at the 09:30 open.
- `ox levels` strength is relative (0–100) within the symbol; independent kinds of evidence
  agreeing beat one kind repeating. Quote the `sources` when using a level.
- `ox pick` blocks fresh entries within 5 days of earnings and notes picks that hold through one.
- The daemon records a daily snapshot (IV30, GEX levels, OI) for the watchlist, portfolio and
  benchmarks after each close; IV rank needs 60+ recorded days to count.
