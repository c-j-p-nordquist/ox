---
name: ox
description: Use the `ox` options CLI — above all `ox pick SYM`, which prices a trade plan (B1/B2/SL/T1/T2, the user's levels or default ones) as if the stock moves at random and ranks option contracts by how much better than chance the plan must be for each to break even. It never says whether to trade. Use whenever the user types a ticker to "pick", asks what a plan costs or which options fit it, asks about a ticker's options, gamma/GEX, expected move or levels, wants their book reviewed, or wants positions loaded into ox.
---

# Using `ox` as an analyst

`ox` is a terminal options tool. Every analysis command has `--json`; always use it (the plain
output is a summary for humans). Data is free and **delayed ~15 min** (CBOE chains, Yahoo bars)
unless the user's config says otherwise; option quotes outside market hours are from the last
close. Say so when timing matters.

Start the daemon first so each call answers in milliseconds: `ox daemon start` (idempotent).
If `ox` is not on PATH, tell the user how to install it (see the README) rather than guessing.

## `ox pick SYM` — the main job

ox does the arithmetic of a plan; the reason to trade is the user's. **ox assumes no edge.** An
audit in November 2026 pooled 59 liquid tickers: ox's level sources held no more often than random
levels, and its setups averaged 0R. One stock's history (about 20 independent trades) cannot show
an edge either way. So ox no longer says ENTER or NO TRADE. It says what a plan is worth if the
stock moves at random, which is the bar the user's own idea has to clear.

### What ox computes (and how to read it)

- `state`: `at_entry` (the leading plan's trigger has fired and price is at its entry), `waiting`
  (it has not; `why_not_now` says what is missing), `no_plan` (no levels near price). This is
  where the plan stands, **not advice**. Never present `at_entry` as a signal to buy.
- `setups[]`, leading one first (the user's levels when given, else the nearest pullback): `name`,
  `status`, `trigger` (checkable: "a daily bar closes back above 230.08"), `b1`/`b2` (scale-in
  entries), `sl`, `t1`/`t2`, each with `basis` (the level's sources or the rule). `alerts[]` are
  the prices to watch. The levels are conventional places for entries, stops and targets; they
  are not evidence that price will turn there.
- `p_t1_random` per setup: how often T1 comes before the stop if the stock moves at random, from
  where the plan is entered (spot when `at_entry`, else the average entry). The plan averages 0R
  at random before costs.
- `backtest` per setup is description, not proof: `signals` (the plan replayed at past signals), `any_day`
  (the same plan entered on any day: the stock's drift), `trend_only`, `effective_n`. Do not call
  a positive `signals.expectancy_r` an edge.
- `calibration`: per-source hold rates on this stock against control levels. Noise; ignore it.
- `earnings_move` (when a report is ahead): `date` (`estimated` = projected), `implied_move_pct`
  (1σ report move priced by the options), `past[]` reaction-day moves and `past_rms_pct`. The
  contract simulation jumps by `sim_move_pct` after the report and drops IV by that event variance.
  Estimated dates can be a week off: confirm against the company's announcement.
- `regime`: trend score and signals, ATR, realized vol, EWMA vol forecast, IV vs forecast RV
  (`vol_pricing` cheap/fair/rich), IV rank once 60+ days are recorded.
- `contracts` (always priced for the leading plan): Monte Carlo of the plan on zero-drift paths at
  forecast realized vol; `p_t1_no_edge` is the share of paths where T1 comes first. Per pick:
  - `ev_no_edge`: expected P&L if the stock moves at random, after spread and commission.
  - `ev_if_t1` / `ev_if_not`: the same paths split by whether T1 came before the stop.
  - `breakeven_p_t1`: how often T1 must come first for the structure to break even.
  - `edge_needed`: `breakeven_p_t1` minus `p_t1_no_edge`, the ranking key (lowest first). Shares
    need 0. An option structure that needs +20 points means the user must be right 20 points
    more often than chance just to get their money back.
  - `edge_needed` ≤ 0 means it pays at random, which rests on ox's realized-vol forecast being
    right (not audited). Say so; do not call it free money.
  - Also `net_at_entry`/`limit_at_entry` (priced at the trigger price if waiting), `p_profit`,
    P&L at SL/T1/T2/flat, `exit_plan`, `why`, `notes`, `live_quotes`, `sizing`.

### Your part

1. `ox daemon start`, then `ox pick SYM --json`. Pass the user's levels and direction with
   `--entry/--stop/--t1/--t2/--bias`; without them ox prices a default plan from nearby levels.
2. **What ox cannot see**: news and catalysts, the macro calendar (FOMC, CPI, jobs) inside the
   holding period, sector moves, and the user's other holdings unless a portfolio is loaded. Keep
   this layer separate from ox's numbers. If it contradicts the plan, say so and re-run with
   other levels.
3. **Live prices**, if you have a broker or market-data connection: take live bid/ask for the top
   picks' legs and push them in, then re-run `ox pick SYM --json` (picks with `live_quotes: true`
   are priced on them; live quotes expire after 30 minutes):
   `echo '{"symbol":"NVDA","source":"broker","spot":233.9,"quotes":[{"occ":"NVDA261218C00250000","bid":10.1,"ask":10.4}]}' | ox ingest quotes -`
   (or `{"expiry":"2026-12-18","right":"C","strike":250,...}` per quote). Without live prices, say
   that contract prices are delayed.
4. **Write it up**:
   - The odds first: how often T1 comes before the stop at random, and what the best structure
     needs (`breakeven_p_t1`, `edge_needed`). Put it as the question it is: "you need to be right
     about 64% of the time; chance gives 42%. What makes this better than chance?"
   - The plan: B1/B2/SL/T1/T2 with R:R, and for `waiting` the trigger in plain words.
   - 1–3 contracts: what each costs at random, what it makes if T1 comes first and loses if not,
     limit price, sizing, exit plan. Prefer the lowest `edge_needed` unless the user wants another
     shape.
   - Your qualitative layer, clearly labelled as yours.
   - Never say ox recommends or confirms a trade. It does not.
   - These are statistics from past prices, not advice; the user decides and places any order.

## Other commands

| Need | Command |
|---|---|
| One-shot summary | `ox overview SYM --json` — spot, IV30, per-expiry ATM IV / straddle / 1σ move / max pain / P/C, GEX levels |
| Gamma levels | `ox gex SYM [--dte N] [--rows 40] --json` — `zero_gamma`, `call_wall`, `put_wall`, per-strike GEX, `profile` |
| GEX heatmap | `ox gex SYM --map [--dte N] --json` — `gex` (the report above), `grid` (strikes ±25% × `expiries`; `call_gex`/`put_gex`/`call_oi`/`put_oi` as `[expiry][strike]`, net = call + put), `read` (where spot sits vs zero gamma, how much gamma rolls off within 7 days, the largest nodes). `ox serve` opens it as a browser page for the user, with the `ox pick` plan drawn on it |
| Chain for one expiry | `ox chain SYM --dte N` or `--exp YYYY-MM-DD` `[--strikes 15] --json` |
| Quotes | `ox quote SYM ... --json` |
| Support / resistance | `ox levels SYM --json` — confluence levels with `strength` and `sources`, plus session levels and gap |
| Earnings | `ox earnings SYM ... --json` — next date, `estimated` until the company confirms |
| Pre-market brief | `ox brief --json` — market backdrop, per symbol (benchmarks, positions, watchlist) move vs expected move, nearest S1/R1, IV30, earnings. With a portfolio: the book `review`, `changes` since the last brief, `events` (reports ahead) and `catalyst_due`. `--notes F --html F` writes a phone-friendly page |
| Watchlist | `ox watchlist --json`; `ox watchlist import -` (JSON or plain tickers); `ox watchlist clear` |
| Portfolio | `ox portfolio --json`; `ox portfolio import -` (schema below) |
| Portfolio risk | `ox risk --json` — backdrop, `book_risk` (1σ daily P&L, market vs company-specific), `directional`, per-underlying Greeks and β, `budget`, flags |
| Book review | `ox review --json` — see below |
| Plan + contracts | `ox pick SYM [--bias long|short] [--entry P --stop P --t1 P --t2 P] [--max-risk USD] [--verbose] --json` |
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

## Reviewing the book: hold or close

`ox review --json` simulates each holding's contracts through the leading plan for its direction,
on random paths, against closing them now. Sunk cost is ignored.

- `holdings[]`: `action` HOLD / CLOSE with `reasons`; `hold` has `ev_no_edge` (expected change in
  value from holding if the stock moves at random), `p_better`, `horizon_end`, `theta_day`,
  `close_friction`.
  - CLOSE: holding is expected to lose > 5% of value (min $10) by the horizon at random.
  - HOLD means holding is cheap, not that it is right: ask for the user's thesis.

Present these as proposals: a CLOSE is still the user's call.

## Levels, sessions and events

- Charts include pre-market and after-hours (04:00–20:00 ET); VWAP is anchored at the 09:30 open.
- `ox levels` strength is relative (0–100) within the symbol; independent kinds of evidence
  agreeing beat one kind repeating. Quote the `sources` when using a level.
- `ox pick` warns when earnings are within 5 days and notes picks that hold through one.
- The daemon records a daily snapshot (IV30, GEX levels, OI) for the watchlist, portfolio and
  benchmarks after each close; IV rank needs 60+ recorded days to count.
