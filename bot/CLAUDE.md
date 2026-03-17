# Polymarket Trading Bot

## What This Is

A Rust-based trading bot for Polymarket with three strategies:
1. **Copy Trader** — discovers top traders from leaderboard, copies their positions with configurable scaling
2. **BTC 5-min Trader** — auto-trades "Bitcoin Up or Down" 5-minute binary markets using Binance/Chainlink price feeds
3. **Market Maker** — simple market-making on configured tokens (currently unused)

Dashboard at `localhost:3030` (WebSocket-based, real-time).

## Project Structure

```
bot/
├── src/
│   ├── main.rs              # Entry point, wires up all components
│   ├── engine.rs             # Core trading loop: book updates, ticks, fills, risk checks
│   ├── btc_trader.rs         # BTC 5-min auto-trader (Binance WS + Chainlink oracle)
│   ├── copy_tracker.rs       # Discovers leader positions, generates copy orders, stop-loss
│   ├── wallet_scorer.rs      # Scores wallets from leaderboard (win rate, avg PnL, profit factor)
│   ├── position.rs           # Position tracking with realized + unrealized PnL
│   ├── market_state.rs       # Order book state + synthetic mark prices from copy tracker
│   ├── dashboard.rs          # Axum WebSocket server, sends Snapshot + streaming updates
│   ├── dashboard_html.rs     # Single-file HTML/CSS/JS dashboard (embedded as const &str)
│   ├── config.rs             # Config structs (parsed from config.toml)
│   ├── risk.rs               # Risk checks: exposure limits, position limits, loss budget
│   ├── order_manager.rs      # Order execution: FAK, GTC, GTC-crossing
│   ├── redeemer.rs           # Auto-redeems resolved conditional tokens for USDC.e
│   └── strategy/
│       ├── mod.rs            # Strategy trait + StrategyAction enum
│       └── market_maker.rs   # Market-making strategy (used for active_tokens)
├── src/bin/                  # Utility binaries
│   ├── approve.rs            # Approve USDC.e + Conditional Tokens on Polymarket contracts
│   ├── check_positions.rs    # Query data API for open positions
│   ├── check_onchain.rs      # Check actual ERC-1155 balances on-chain
│   ├── redeem.rs             # Manually redeem resolved conditional tokens
│   ├── swap_usdc.rs          # Swap native USDC to USDC.e via Uniswap V3
│   ├── send_usdc.rs          # Send USDC to another address
│   ├── transfer_to_proxy.rs  # Transfer tokens to proxy wallet
│   ├── fund_proxy.rs         # Fund proxy wallet with MATIC
│   └── derive_keys.rs        # Derive keys from mnemonic
├── data/
│   ├── btc_stats.json        # BTC trader cumulative PnL, wins, losses, skips
│   ├── btc_trades.jsonl      # Per-trade log for BTC trader
│   └── scored_wallets.json   # Cached wallet scores (1-hour validity)
├── config.toml               # Bot configuration (all settings)
├── positions.json            # Persisted positions across restarts
├── pnl_total.txt             # Cumulative realized PnL
├── .env.example              # API credential template
├── DEPLOY.md                 # Mac Mini deployment guide
└── Cargo.toml                # Dependencies
```

## Build & Run

```bash
export PATH="$HOME/.cargo/bin:$PATH"  # required on this machine

# Development
cargo build && cargo run

# Production
cargo build --release
nohup ./target/release/polymarket-bot > bot.log 2>&1 &
```

Requires `.env` with Polymarket API credentials (see `.env.example`).
Config is read from `config.toml` in the working directory.

## Deployment

See `DEPLOY.md` for Mac Mini deployment with Cloudflare Tunnel.
Runs on Mac Mini ("openclaw"), dashboard exposed via Cloudflare Tunnel.

To deploy changes:
```bash
cargo build --release
kill $(pgrep polymarket-bot)
nohup ./target/release/polymarket-bot > bot.log 2>&1 &
```

---

## BTC 5-Min Trader (`btc_trader.rs`)

### How It Works
- Polymarket has "Bitcoin Up or Down" markets every 5 minutes
- Resolution: Chainlink BTC/USD Data Streams price at window close vs open
- If close >= open, "Up" wins ($1.00), else "Down" wins ($1.00). Loser = $0.00.
- **Our edge**: Binance price leads Chainlink by ~27 seconds

### Strategy (Pure Model)
1. Wait until 60+ seconds into the 5-min window (no early entries)
2. Poll Chainlink oracle on Polygon for current BTC/USD price
3. Compute `p_up = prob_above(chainlink_price, strike, annualized_vol, secs_left)` (Black-Scholes style)
4. If `p_up >= min_probability` (70%) AND `edge >= min_edge` (8%), enter
5. No signal adjustments — pure model output only (flow/funding/liq were tested, added noise not edge)
6. No mid-window exits — sells always fail on thin books, and positions can recover from reversals
7. Max loss per trade = bet_size ($5 in test mode)

### Pricing Logic
- `max_willing = model_prob - min_edge` — the most we'll pay
- If ask <= max_willing: FAK (fill-and-kill) instant fill
- Otherwise: GTC bid at best_bid + $0.01, capped at max_willing
- No hard price cap — edge check IS the price guard
- Example: model 90% → max buy $0.82. Model 70% → max buy $0.62.

### Key Design Decisions (Learned the Hard Way)
- **No speculative stage**: Was entering at 20s with Binance-only signal. Caused $110 blowup from multiple orders stacking in one window. Removed entirely.
- **No mid-window exit**: FAK and GTC sells both fail on thin 5-min books. 5 of 6 mid-exits failed. Position gets marked as exited but actually isn't, preventing recovery. Just ride to resolution.
- **No signal adjustments**: Flow, funding, liquidations were tested. They fight each other and cause false confidence. Pure `prob_above()` is cleaner.
- **Non-blocking order placement**: All CLOB operations use `tokio::spawn` with oneshot channels. The CLOB API takes 1-20 seconds; blocking would miss price updates.
- **Kill switch**: Session breaks if cumulative PnL drops below -$25.

### Config (`[btc_trader]` in config.toml)
```toml
bet_size_usd = 5.0          # per-trade size (test mode)
min_probability = 0.70      # model must say ≥70%
min_edge = 0.08             # model_prob - buy_price must be ≥8%
max_exposure_usd = 50.0     # total BTC trader exposure cap
```

### Data Files
- `data/btc_stats.json` — cumulative: `{cumulative_pnl, wins, losses, skips}`
- `data/btc_trades.jsonl` — per-trade log with timestamps, prices, outcomes

### Monitoring
```bash
# Watch for trade signals and results
grep "CONFIRMED BTC TRADE\|won\|lost\|GTC order never filled\|KILL" bot.log | tail -20

# Check current stats
cat data/btc_stats.json

# Check if orders are filling (0% fill rate = price cap too low or book too thin)
grep "GTC order never filled" bot.log | wc -l
```

### Common Issues
- **0% fill rate**: Price cap too low or books too thin. The edge check (`max_willing = model_prob - min_edge`) determines max price. If asks are always above this, lower min_edge or accept fewer fills.
- **Order stacking**: If multiple orders fire in same window, check `traded_windows` dedup logic.
- **Chainlink stale**: Oracle age > 30s is logged. Bot uses Binance-only direction when Chainlink is stale.

---

## Copy Trader (`copy_tracker.rs`)

### How It Works
1. Wallet scorer discovers top 20 traders from Polymarket leaderboards (4 categories)
2. Copy tracker polls their positions every 15 seconds
3. When 2+ leaders hold the same token, bot enters with `scale_factor * leader_size`
4. Stop-loss at 25%, take-profit at $0.92 (suppressed for near-expiry markets)
5. Sticky exits: leaders must be absent 12 consecutive polls (3 min) before bot exits

### Key Filters
- `max_entry_price = 0.85` — skip near-certainties (no upside)
- `max_entry_drift = 0.08` — max price above leader's entry
- `max_days_to_resolution = 7` — near-term markets only
- `min_leaders_for_entry = 2` — consensus required
- `exclude_title_keywords` — blocks crypto range, sports spreads, esports sub-games
- Near-expiry relaxation (≤2 days): relaxed price/drift guards

### Anti-Churn Logic
- Groups targets by `condition_id`, keeps only dominant side per market
- Won't buy opposite side of a held position
- 30-min cooldown after exiting before re-entry (both sides)

---

## Core Architecture

### Data Flow
```
Polymarket WS → Engine (book updates) → Strategy → StrategyActions → Risk Check → Execute/Simulate
Copy Tracker (polls API) → EngineEvents → Engine → Simulate fills
BTC Trader (independent loop) → OrderManager → CLOB API
All updates → broadcast::Sender<DashboardUpdate> → WebSocket clients (dashboard)
```

### Order Execution (`order_manager.rs`)
- `execute_strict()` — FAK (fill-and-kill) taker order
- `place_gtc_crossing()` — GTC order that can cross the spread (no post_only flag)
- All orders go through Polymarket CLOB API with signed messages

### Position Tracking (`position.rs`)
- Tracks realized + unrealized PnL per position
- Persisted to `positions.json` across restarts
- Duplicate fill dedup via HashSet on `trade.id` (WebSocket replays fills 3-4x)

### Risk Manager (`risk.rs`)
- Position size limits (absolute + % of bankroll)
- Total exposure cap
- Daily loss limit (stops all trading)

### Dashboard Updates (WebSocket messages)
- `Snapshot` — full state on connect
- `TickSummary` — periodic PnL and exposure
- `PositionUpdate` — per-position mark-to-market
- `LeaderUpdate` — scored leaders + copy targets
- `CopyEvent` — stop-loss and price-guard alerts

---

## Wallet & Contract Info

- **Signature type**: `gnosis_safe` — trades go through proxy wallet, visible on Polymarket UI
- **USDC.e**: `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` (Polygon)
- **Conditional Tokens (ERC-1155)**: `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045`
- **CTF Exchange**: `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E`
- **Neg Risk CTF Exchange**: `0xC5d563A36AE78145C45a50134d48A1215220f80a`
- **Chainlink BTC/USD (Polygon)**: `0xc907E116054Ad103354f2D350FD2514433D57F6f`

---

## Important Gotchas

- `OrderBookLevel` is `#[non_exhaustive]` — must use `.builder().price(x).size(y).build()`, not struct literals
- Copy-traded tokens don't have real order book subscriptions — prices come from API polling via `MarketState.update_mark_price()` (synthetic books with `timestamp == 0`)
- Must call `ring::default_provider().install_default()` before any TLS/WS connections (in `main.rs`)
- The CLOB API can take 1-20 seconds per call — never `.await` in the hot loop, always `tokio::spawn`
- BTC 5-min market books are extremely thin — GTC orders may never fill
- `positions.json` and `pnl_total.txt` persist across restarts — reset them to `[]` and `0` to start fresh
