# Polymarket Copy-Trading Bot

## What This Is

A Rust-based copy-trading bot for Polymarket that:
- Discovers and scores top traders from the Polymarket leaderboard
- Copies their positions with configurable scaling
- Provides a real-time web dashboard at `localhost:3030`
- Supports dry-run mode (simulated fills) and live trading

## Project Structure

```
bot/
├── src/
│   ├── main.rs              # Entry point, wires up all components
│   ├── engine.rs             # Core trading loop: book updates, ticks, fills, risk checks
│   ├── copy_tracker.rs       # Discovers leader positions, generates copy orders, stop-loss
│   ├── wallet_scorer.rs      # Scores wallets from leaderboard (win rate, avg PnL, profit factor)
│   ├── position.rs           # Position tracking with realized + unrealized PnL
│   ├── market_state.rs       # Order book state + synthetic mark prices from copy tracker
│   ├── dashboard.rs          # Axum WebSocket server, sends Snapshot + streaming updates
│   ├── dashboard_html.rs     # Single-file HTML/CSS/JS dashboard (embedded as const &str)
│   ├── config.rs             # Config structs (parsed from config.toml)
│   ├── risk.rs               # Risk checks: exposure limits, position limits, loss budget
│   ├── order_manager.rs      # Tracks live orders
│   └── strategy/
│       ├── mod.rs            # Strategy trait + StrategyAction enum
│       └── market_maker.rs   # Market-making strategy (used for active_tokens)
├── config.toml               # Bot configuration (risk limits, copy trader settings)
├── .env.example              # API credential template
├── DEPLOY.md                 # Mac Mini deployment guide
└── Cargo.toml                # Dependencies
```

## Key Architecture

### Data Flow
```
Polymarket WS → Engine (book updates) → Strategy → StrategyActions → Risk Check → Execute/Simulate
Copy Tracker (polls API) → EngineEvents → Engine → Simulate fills
All updates → broadcast::Sender<DashboardUpdate> → WebSocket clients (dashboard)
```

### Dashboard Updates (WebSocket messages)
- `Snapshot` — full state on connect (positions, books, PnL, exposure)
- `TickSummary` — periodic PnL (realized + unrealized) and exposure
- `PositionUpdate` — per-position mark-to-market updates every tick
- `LeaderUpdate` — scored leaders + tracked copy targets
- `LeaderTrade` — individual leader trades detected
- `CopyEvent` — stop-loss and price-guard alerts
- `Trade`, `OrderEvent`, `BookSnapshot` — market-making activity

### Mark-to-Market PnL
- Copy tracker feeds `cur_price` from Polymarket API into `MarketState.update_mark_price()`
- Engine's `handle_tick()` computes unrealized PnL using midpoint prices
- `daily_pnl = realized + unrealized` — updates every tick
- Each position broadcasts its combined PnL to the dashboard every tick

### Wallet Scoring Formula
```
score = avg_pnl_component * 0.30      // $5 avg profit/trade = max
      + win_rate * 0.25
      + profit_factor_component * 0.25 // profit_factor/5, capped at 1.0
      + volume_component * 0.20       // trade_count/100, capped at 1.0
```

### Dashboard (dashboard_html.rs)
- Single HTML file embedded as a Rust string constant
- PnL chart with exposure overlay, time axis, high/low markers
- `pnlHistory` and `exposureHistory` store `{t: timestamp, v: value}` objects
- Positions table shows combined realized + unrealized PnL
- Panel header: "PnL & Exposure"

## Build & Run

```bash
# Development
cargo build
cargo run

# Production
cargo build --release
./target/release/polymarket-bot
```

Requires `.env` with Polymarket API credentials (see `.env.example`).
Config is read from `config.toml` in the working directory.

## Deployment

See `DEPLOY.md` for full Mac Mini deployment instructions with Cloudflare Tunnel.

Target deployment: `hunterearls.dev/polymarket` (or `bot.hunterearls.dev`)
Backend runs on Mac Mini ("openclaw"), exposed via Cloudflare Tunnel.

## Important Notes

- `OrderBookLevel` is `#[non_exhaustive]` — must use `.builder().price(x).size(y).build()`, not struct literals
- Copy-traded tokens don't have real order book subscriptions — prices come from the copy tracker's API polling via `MarketState.update_mark_price()` (synthetic books with `timestamp == 0`)
- The `build_snapshot()` in `dashboard.rs` shows ALL positions with non-zero size (not just `active_tokens`)
- Risk config is in `config.toml` under `[risk]`
- Dry run mode simulates fills instantly (BUY at ask, SELL at bid) in `engine.rs`
