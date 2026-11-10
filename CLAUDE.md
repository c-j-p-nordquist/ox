# ox — options contract picker

Rust (edition 2021), tokio. Single binary `ox`. The product is `ox pick SYM`: a trade plan priced
as if the stock moves at random, and the contracts ranked by how much better than chance the plan
must be for each to break even. It never says whether to trade. Claude is the main user.

## Architecture

- `provider/` — data sources behind three traits (`ChainSource`, `BarSource`, `QuoteSource`) so
  providers can be mixed. Free defaults: `cboe` (delayed chains with IV/Greeks/OI) and `yahoo`
  (bars, quotes). A new real-time provider = implement the traits + register in `provider::build`.
- `engine.rs` — TTL caches with fetch de-duplication, answers `protocol::Request`. Runs in-process
  or inside the daemon, whose `prefetch_loop` keeps recently used data warm.
- `daemon.rs` / `client.rs` — Unix socket in the cache dir (`~/Library/Caches/ox/oxd.sock` on macOS), newline-delimited
  JSON. `Client::connect` uses the daemon when it answers a ping, else runs locally.
- `analytics/pick/` — the picker: `regime` (trend, vol forecast), `zones` (level sources +
  walk-forward hold rates vs control levels), `setup` (plans + their history as description),
  `contracts` (zero-drift Monte Carlo of the plan per structure: break-even hit rate and edge
  needed; and of a held position vs closing it), `mod` (plan state, situation).
- `analytics/review.rs` — `ox review`: each holding HOLD or CLOSE on what holding costs at random.
- `analytics/` — Black-Scholes (`bs`), GEX + zero-gamma profile (`gex`), indicators, chain views,
  expected moves, max pain. Pure functions; unit-tested.
- `cli.rs` — one-shot commands; each supports `--json`. `pick_view.rs` is the default `ox pick`
  view (`--verbose` is the full report in `cli.rs`).
- `skills/` — the agent skills (`ox`, `ox-ibkr`), embedded in the binary by `skills.rs` for
  `ox skill install`; `.claude/skills/*` symlink to them. Keep them in step with the `--json` output.
- Releases: cargo-dist (`dist-workspace.toml`, `.github/workflows/release.yml`). Push a `v*` tag
  to build macOS/Linux binaries and the `ox-installer.sh` curl installer.

## Rules

- Statistical honesty is the product. ox assumes no edge: a November 2026 audit over 59 tickers
  found the level sources no better than control levels and the setups at 0R pooled, and one
  stock's ~20 independent trades cannot show an edge. Do not reintroduce a per-stock edge verdict;
  an edge claim would need a pooled, out-of-sample test. Heavy compute goes in `spawn_blocking` inside the engine and
  is memoized (the level calibration is cached per bar set).
- Every new view gets a `--json` form (Claude uses them; see `.claude/skills/ox`).
- Keep data provider-neutral: normalize into `model.rs` types. Nothing in the core assumes a
  broker; broker-specific glue lives in skills (e.g. `skills/ox-ibkr`).

## Commands

```bash
cargo build --release && cargo test && cargo clippy
./target/release/ox --local pick NVDA --verbose   # check the picker end to end
```
