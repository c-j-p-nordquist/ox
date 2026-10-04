---
name: ox-ibkr
description: Connect `ox` to Interactive Brokers through the IBKR connector — sync the IBKR portfolio and a watchlist into ox, verify ox's contract picks with live IBKR quotes, push overnight bars, set price alerts at ox's trigger levels, and stage (never place) orders as IBKR order instructions. Use alongside the `ox` skill whenever the user has the IBKR connector and asks to sync, verify, alert on, or stage a trade from `ox pick` or `ox review`.
---

# ox + Interactive Brokers

This skill is the bridge between the `ox` CLI (see the `ox` skill) and the IBKR connector. ox
never logs in to IBKR: you read IBKR and push the data into ox, and you stage orders in IBKR from
ox's picks. The user reviews and submits every order themselves.

## Sync the portfolio

1. Read positions and balances: `get_account_positions`, `get_account_summary`.
2. Map to ox's portfolio schema (see the `ox` skill) and pipe into `ox portfolio import -`:
   - `symbol` is the underlying; parse `contract_description` like `GOOG Oct16'26 300 PUT` into
     symbol / expiry / strike / right.
   - `avg_cost` per share: the connector's `average_price` is already per share for options
     (check: `(market_price - average_price) * 100 * qty ≈ unrealized_pnl`), so copy it as is.
   - `net_liquidation`, `cash` (`total_cash_value`), `excess_liquidity` and `currency` (the
     account's base currency) from `get_account_summary`.
   - Futures and other instruments only as `stock` rows if they have a Yahoo-quotable symbol;
     otherwise skip them and say so.
3. Confirm with `ox portfolio --json`; report anything with `mark: null`.

Re-sync before `ox risk`, `ox review` or a sized `ox pick` if the snapshot is older than today or
the user has traded since.

## Sync a watchlist

1. `get_watchlists`; ask the user once which list mirrors ox's watchlist if it isn't obvious.
2. `get_watchlist(id)` → take each `contract_description` as the ticker (skip anything that isn't
   a plain ticker, and say so).
3. Pipe `{"name": "<list>", "source": "ibkr", "as_of": "<date>", "symbols": [...]}` into
   `ox watchlist import -`, keeping IBKR's order.

## Verify a pick with live quotes

ox's chains are delayed (before the open, option quotes are from the last close). Before
recommending contracts:

1. `search_contracts(query=SYM)` → the row with the exact `symbol` and OPT in sections →
   `underlying_contract_id`.
2. `get_option_parameters(underlying_contract_id)` → the expiration `id` whose `date` matches the
   pick's expiry (YYYYMMDD) and whose `trading_class` equals the symbol (e.g. `NVDA`, not `2NVDA`).
3. `get_option_data(expiration_id, min_strike, max_strike)` bounded around the legs → per strike
   `call_contract_id_ex` / `put_contract_id_ex`.
4. `get_price_snapshot(contract_id, market_data_names=["bid_ask","option_midpoint_iv","top_status"])`
   per leg; for the underlying also `implied_volatility_percentile` (IV rank context; values are
   fractions, 0.03 = 3rd percentile).
   - `top-status`: `REALTIME` = live; `FROZEN` = last close (midpoint IV comes back
     `isValid: false`). Frozen quotes match what ox already has, so re-check after the open.
5. Push the live bid/ask and the live underlying as `spot`:
   `echo '{"symbol":"NVDA","source":"ibkr","spot":233.9,"quotes":[{"occ":"NVDA261218C00250000","bid":10.1,"ask":10.4}]}' | ox ingest quotes -`
   then re-run `ox pick SYM --json`. If the ranking or verdict changed, say so.

## Overnight session

ox has no overnight data of its own. Before a pre-market read:

1. `get_price_history(contract_id, security_type="STK", exchange="OVERNIGHT", outside_rth=true,
   step="FIFTEEN_MINS" or "ONE_HOUR", period="TWO_DAYS")`.
2. Convert `time` (ISO, UTC) to unix seconds and pipe
   `[{"symbol": "NVDA", "source": "ibkr-overnight", "bars": [{"t","o","h","l","c","v"}, ...]}]`
   into `ox ingest overnight -`. `ox levels SYM` then includes the overnight range for 24 hours.

## Alerts for WAIT verdicts

WAIT verdicts are not staged. Offer IBKR price alerts at the setup's `alerts[]` prices
(`create_alert`), and re-run `ox pick` when one fires: triggers need a daily close, so check near
the close.

## Stage, don't place

1. Show a confirmation block: account, legs, side, quantity, LIMIT price (from the pick's
   `limit_at_entry`, refreshed with live quotes), time in force, max loss, and `sizing` from ox.
2. Only after an explicit yes:
   - Single leg: `create_order_instruction(contract_id_ex=<leg>, side, quantity, order_type="LIMIT", limit_price, time_in_force="DAY")`.
   - Spread: `get_combo_identifier(legs=[{contract_id_ex, size: +1|-1}, ...])` → pass its
     `contract_id_ex` to `create_order_instruction`. Debit spreads: BUY at the net debit; for credit
     spreads follow the combo's `description` for sign conventions and confirm with the user.
3. An instruction is **not a live order**: give the user the returned URL to review and submit in
   IBKR. After they submit, re-sync the portfolio.
