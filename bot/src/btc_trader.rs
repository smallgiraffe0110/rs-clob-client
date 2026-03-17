//! BTC 5-minute market auto-trader for Polymarket.
//!
//! Strategy:
//!   - Markets are "Bitcoin Up or Down" with slug `btc-updown-5m-{window_start_ts}`
//!   - Resolution: Chainlink BTC/USD Data Streams price at window close vs open
//!   - If close >= open → "Up" wins, else "Down" wins
//!
//! How we make money:
//!   - Binance price leads Chainlink by ~27s — we see BTC moves before the oracle
//!   - We wait 60-90s into the window for a clear directional signal
//!   - When Chainlink has ALREADY moved from strike, the direction is established
//!   - Market prices lag because most traders react slowly
//!   - We buy the winning side before the market catches up
//!
//! Key rules:
//!   1. Wait for signal: don't enter in the first 30 seconds
//!   2. Use Chainlink as truth: it's the resolution oracle, Binance is early warning
//!   3. Both sources must agree on direction (or Binance alone if Chainlink is stale)
//!   4. Try FAK first (instant fill), fall back to GTC on book (no post_only)
//!   5. Only score bets with confirmed fills (FAK success or position tracker check)
//!
//! Latency optimizations:
//!   - Binance WebSocket stream (~10ms) instead of HTTP polling (~500ms)
//!   - Pre-cached order book (refreshed every 2s, ready when signal fires)
//!   - Chainlink polled every 2s instead of 5s

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use alloy::primitives::{Address, I256};
use alloy::providers::ProviderBuilder;
use alloy::sol;
use dashmap::DashMap;
use futures::StreamExt;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::BtcTraderConfig;
use crate::order_manager::OrderManager;
use crate::position::PositionTracker;

const WINDOW_SECS: u64 = 300; // 5 minutes

// ── Data sources ─────────────────────────────────────────────────────
/// Combined Binance Futures WS: aggTrade (order flow) + markPrice (funding) + liquidations
const BINANCE_FUTURES_WS: &str =
    "wss://fstream.binance.com/stream?streams=btcusdt@aggTrade/btcusdt@markPrice@1s/!forceOrder@arr";
const BINANCE_PRICE_URL: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
const CHAINLINK_BTC_USD: &str = "0xc907E116054Ad103354f2D350FD2514433D57F6f";
const CHAINLINK_DECIMALS: f64 = 1e8;
const POLYGON_RPC: &str = "https://polygon-bor-rpc.publicnode.com";
const GAMMA_API: &str = "https://gamma-api.polymarket.com";

/// Minimum seconds before speculative pre-bid (Binance leads, get cheap shares early).
const SPECULATIVE_ENTRY_SECS: u64 = 20;
/// Minimum seconds before confirmed full-size entry.
const CONFIRMED_ENTRY_SECS: u64 = 60;
/// Minimum Binance move (USD) to trigger speculative pre-bid. Lower bar than confirmed.
const SPECULATIVE_MIN_MOVE_USD: f64 = 15.0;

/// Minimum BTC price move for confirmed entry (scaled by time remaining).
/// Speculative pre-bids get cheap shares early; confirmed is the fallback.
const MIN_MOVE_EARLY_USD: f64 = 30.0;
const MIN_MOVE_LATE_USD: f64 = 15.0;
/// Boundary: entries with more than this many seconds left use the early (larger) threshold.
const EARLY_ENTRY_SECS_LEFT: u64 = 180;

sol! {
    #[sol(rpc)]
    interface IChainlinkAggregator {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

/// A live Polymarket BTC 5-min market.
#[derive(Debug, Clone)]
struct LiveMarket {
    token_id_up: U256,
    token_id_down: U256,
    up_price: f64,
    down_price: f64,
    window_start: u64,
}

#[derive(Debug, Clone)]
struct BtcBet {
    side: String,        // "UP" or "DOWN"
    price_paid: f64,     // price per share
    size_usd: f64,       // USD spent
    token_id: U256,      // token we bet on (for fill tracking)
    filled: bool,        // FAK confirmed fill vs GTC pending
    // ── Entry-time snapshot for trade history ──
    entry_secs_in: u64,
    window_start: u64,
    strike: f64,
    binance_at_entry: f64,
    chainlink_at_entry: f64,
    binance_move: f64,
    chainlink_move: f64,
    model_prob: f64,
    edge: f64,
    ann_vol: f64,
    order_flow_pct: f64,     // net flow / total volume at entry
    funding_rate: f64,
    liq_pressure: f64,
    speculative: bool,       // was this a speculative pre-bid or confirmed?
    market_bid: Option<f64>, // best bid on our token at entry
    market_ask: Option<f64>, // best ask on our token at entry
}

/// Complete trade record written to disk after resolution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TradeRecord {
    // Identity
    window_start: u64,
    timestamp: String,       // ISO8601 for readability
    side: String,
    // Entry
    entry_secs_in: u64,
    speculative: bool,
    price_paid: f64,
    size_usd: f64,
    strike: f64,
    // Prices at entry
    binance_at_entry: f64,
    chainlink_at_entry: f64,
    binance_move_at_entry: f64,
    chainlink_move_at_entry: f64,
    spread_ema: f64,
    // Model at entry
    model_prob: f64,
    edge: f64,
    ann_vol: f64,
    // Signals at entry
    order_flow_pct: f64,
    funding_rate: f64,
    liq_pressure: f64,
    // Book at entry
    market_bid: Option<f64>,
    market_ask: Option<f64>,
    // Resolution
    chainlink_at_close: f64,
    final_move: f64,          // chainlink_close - strike
    won: bool,
    pnl: f64,
    mid_exit: bool,           // did we exit mid-window?
    // Derived (for quick analysis)
    move_reversed: bool,      // did direction flip between entry and close?
    peak_move_for_us: f64,    // max favorable move we saw (0 if not tracked)
}

const TRADES_PATH: &str = "data/btc_trades.jsonl";

impl TradeRecord {
    fn append_to_disk(&self) {
        let _ = std::fs::create_dir_all("data");
        if let Ok(json) = serde_json::to_string(self) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(TRADES_PATH)
            {
                let _ = writeln!(f, "{}", json);
            }
        }
    }

    fn load_all() -> Vec<TradeRecord> {
        std::fs::read_to_string(TRADES_PATH)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// Adaptive parameters derived from trade history.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct HistoryInsights {
    /// Average price paid on winning speculative trades (best achievable entry).
    avg_winning_spec_price: Option<f64>,
    /// Average price paid on winning confirmed trades.
    avg_winning_conf_price: Option<f64>,
    /// Win rate for speculative entries.
    spec_win_rate: Option<f64>,
    /// Win rate for confirmed entries.
    conf_win_rate: Option<f64>,
    /// Average entry secs_in for winners.
    avg_winning_entry_secs: Option<f64>,
    /// When flow agreed with our direction, win rate.
    flow_agree_win_rate: Option<f64>,
    /// When flow disagreed, win rate.
    flow_disagree_win_rate: Option<f64>,
    /// Average |binance_move| at entry for winners vs losers.
    avg_win_move: Option<f64>,
    avg_loss_move: Option<f64>,
    /// Optimal min_move threshold (midpoint between avg loss move and avg win move).
    suggested_min_move: Option<f64>,
    /// How often direction reversed between entry and close.
    reversal_rate: Option<f64>,
    /// Total trades analyzed.
    trade_count: usize,
}

impl HistoryInsights {
    fn from_trades(trades: &[TradeRecord]) -> Self {
        if trades.is_empty() {
            return Self {
                avg_winning_spec_price: None, avg_winning_conf_price: None,
                spec_win_rate: None, conf_win_rate: None,
                avg_winning_entry_secs: None,
                flow_agree_win_rate: None, flow_disagree_win_rate: None,
                avg_win_move: None, avg_loss_move: None, suggested_min_move: None,
                reversal_rate: None, trade_count: 0,
            };
        }

        let winners: Vec<_> = trades.iter().filter(|t| t.won).collect();
        let losers: Vec<_> = trades.iter().filter(|t| !t.won).collect();

        let spec_trades: Vec<_> = trades.iter().filter(|t| t.speculative).collect();
        let conf_trades: Vec<_> = trades.iter().filter(|t| !t.speculative).collect();

        let avg_winning_spec_price = {
            let ws: Vec<_> = winners.iter().filter(|t| t.speculative).collect();
            if ws.is_empty() { None } else { Some(ws.iter().map(|t| t.price_paid).sum::<f64>() / ws.len() as f64) }
        };
        let avg_winning_conf_price = {
            let wc: Vec<_> = winners.iter().filter(|t| !t.speculative).collect();
            if wc.is_empty() { None } else { Some(wc.iter().map(|t| t.price_paid).sum::<f64>() / wc.len() as f64) }
        };
        let spec_win_rate = if spec_trades.is_empty() { None } else {
            Some(spec_trades.iter().filter(|t| t.won).count() as f64 / spec_trades.len() as f64)
        };
        let conf_win_rate = if conf_trades.is_empty() { None } else {
            Some(conf_trades.iter().filter(|t| t.won).count() as f64 / conf_trades.len() as f64)
        };
        let avg_winning_entry_secs = if winners.is_empty() { None } else {
            Some(winners.iter().map(|t| t.entry_secs_in as f64).sum::<f64>() / winners.len() as f64)
        };

        // Flow analysis: did order_flow direction agree with our side?
        let (mut flow_agree_wins, mut flow_agree_total) = (0usize, 0usize);
        let (mut flow_disagree_wins, mut flow_disagree_total) = (0usize, 0usize);
        for t in trades {
            let flow_bullish = t.order_flow_pct > 0.0;
            let we_bullish = t.side == "UP";
            if flow_bullish == we_bullish {
                flow_agree_total += 1;
                if t.won { flow_agree_wins += 1; }
            } else {
                flow_disagree_total += 1;
                if t.won { flow_disagree_wins += 1; }
            }
        }
        let flow_agree_win_rate = if flow_agree_total > 0 { Some(flow_agree_wins as f64 / flow_agree_total as f64) } else { None };
        let flow_disagree_win_rate = if flow_disagree_total > 0 { Some(flow_disagree_wins as f64 / flow_disagree_total as f64) } else { None };

        let avg_win_move = if winners.is_empty() { None } else {
            Some(winners.iter().map(|t| t.binance_move_at_entry.abs()).sum::<f64>() / winners.len() as f64)
        };
        let avg_loss_move = if losers.is_empty() { None } else {
            Some(losers.iter().map(|t| t.binance_move_at_entry.abs()).sum::<f64>() / losers.len() as f64)
        };
        let suggested_min_move = match (avg_win_move, avg_loss_move) {
            (Some(w), Some(l)) => Some((w + l) / 2.0), // midpoint as threshold
            _ => None,
        };

        let reversals = trades.iter().filter(|t| t.move_reversed).count();
        let reversal_rate = Some(reversals as f64 / trades.len() as f64);

        Self {
            avg_winning_spec_price, avg_winning_conf_price,
            spec_win_rate, conf_win_rate,
            avg_winning_entry_secs,
            flow_agree_win_rate, flow_disagree_win_rate,
            avg_win_move, avg_loss_move, suggested_min_move,
            reversal_rate, trade_count: trades.len(),
        }
    }
}

/// Cached order book snapshot for a token.
#[derive(Debug, Clone, Default)]
struct CachedBook {
    best_bid: Option<f64>,
    best_ask: Option<f64>,
}

/// Real-time market signals from Binance Futures WebSocket.
/// All fields are shared via watch channels for zero-copy reads.
#[derive(Debug, Clone, Default)]
struct MarketSignals {
    /// Net buy pressure: positive = buyers dominating, negative = sellers.
    /// Computed as (buy_volume - sell_volume) over a rolling window.
    order_flow: f64,
    /// Total volume in the rolling window (for detecting spikes).
    volume: f64,
    /// Current funding rate (positive = longs pay shorts = bullish bias).
    funding_rate: f64,
    /// Recent liquidation pressure: positive = longs liquidated (bearish), negative = shorts liquidated (bullish).
    liq_pressure: f64,
}

const BTC_STATS_PATH: &str = "data/btc_stats.json";

/// Persisted stats that survive restarts so auto-scaling keeps working.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedStats {
    cumulative_pnl: f64,
    wins: u32,
    losses: u32,
    skips: u32,
}

impl PersistedStats {
    fn load() -> Self {
        std::fs::read_to_string(BTC_STATS_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(PersistedStats { cumulative_pnl: 0.0, wins: 0, losses: 0, skips: 0 })
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all("data");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(BTC_STATS_PATH, json);
        }
    }
}

pub struct BtcTrader {
    config: BtcTraderConfig,
    order_manager: Arc<OrderManager>,
    #[allow(dead_code)]
    positions: Arc<PositionTracker>,
    http: reqwest::Client,
    /// Binance price via WebSocket (watch channel for latest)
    binance_rx: watch::Receiver<f64>,
    /// Market signals (order flow, liquidations, funding rate)
    signals_rx: watch::Receiver<MarketSignals>,
    /// Binance price samples for volatility
    price_samples: VecDeque<f64>,
    /// Log returns for annualized vol
    log_returns: VecDeque<f64>,
    /// Chainlink price + last update timestamp
    chainlink_price: f64,
    chainlink_updated_at: u64,
    last_chainlink_poll: u64,
    /// Strike price = Chainlink price at window open
    strike: f64,
    strike_window: u64,
    /// Binance price captured at window open (for spread tracking)
    binance_at_open: f64,
    /// Current market from gamma API
    current_market: Option<LiveMarket>,
    last_market_fetch: u64,
    /// Prevent double trading per window
    traded_windows: DashMap<u64, ()>,
    /// Current-window exposure only (resets each window since 5-min markets resolve)
    exposure: Decimal,
    /// Track the last bet for win/loss checking
    last_bet: Option<BtcBet>,
    /// Running P&L and record for the session
    session_pnl: f64,
    session_wins: u32,
    session_losses: u32,
    session_skips: u32,
    /// Rolling Binance-Chainlink spread (EMA over windows, more stable than single snapshot)
    spread_ema: f64,
    /// Pre-cached order books for UP and DOWN tokens (refreshed every 2s)
    cached_book_up: CachedBook,
    cached_book_down: CachedBook,
    last_book_fetch: u64,
    /// Speculative pre-bid: order placed early at cheap price before signal confirms.
    /// (order_id, side "UP"/"DOWN", token_id, window_start, price)
    speculative_order: Option<(String, String, U256, u64, f64)>,
    /// Track how many speculative orders placed this window (prevent re-placing after cancel)
    spec_window: u64,
    spec_placed: bool,
    /// Pending async speculative order placement result (non-blocking).
    pending_order: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
    /// Pending async confirmed order placement result (non-blocking).
    pending_confirmed: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
    /// Tracks windows where we already exited mid-window (prevent double-sell)
    exited_windows: DashMap<u64, ()>,
    /// Adaptive insights from trade history.
    history: HistoryInsights,
}

impl BtcTrader {
    pub fn new(
        config: BtcTraderConfig,
        order_manager: Arc<OrderManager>,
        positions: Arc<PositionTracker>,
    ) -> Self {
        // Create watch channels — WebSocket writer, trader reader
        let (binance_tx, binance_rx) = watch::channel(0.0_f64);
        let (signals_tx, signals_rx) = watch::channel(MarketSignals::default());

        // Spawn Binance Futures combined WebSocket (price + order flow + funding + liquidations)
        tokio::spawn(binance_futures_ws(binance_tx, signals_tx));

        // Load persisted stats so auto-scaling survives restarts
        let stats = PersistedStats::load();
        if stats.cumulative_pnl != 0.0 {
            info!(
                pnl = format!("${:+.2}", stats.cumulative_pnl),
                record = format!("{}-{} (skip {})", stats.wins, stats.losses, stats.skips),
                "restored BTC trader stats from disk"
            );
        }

        Self {
            config,
            order_manager,
            positions,
            http: reqwest::Client::new(),
            binance_rx,
            signals_rx,
            price_samples: VecDeque::with_capacity(600),
            log_returns: VecDeque::with_capacity(300),
            chainlink_price: 0.0,
            chainlink_updated_at: 0,
            last_chainlink_poll: 0,
            strike: 0.0,
            strike_window: 0,
            binance_at_open: 0.0,
            current_market: None,
            last_market_fetch: 0,
            traded_windows: DashMap::new(),
            exposure: dec!(0),
            last_bet: None,
            session_pnl: stats.cumulative_pnl,
            session_wins: stats.wins,
            session_losses: stats.losses,
            session_skips: stats.skips,
            spread_ema: 0.0,
            cached_book_up: CachedBook::default(),
            cached_book_down: CachedBook::default(),
            last_book_fetch: 0,
            speculative_order: None,
            spec_window: 0,
            spec_placed: false,
            pending_order: None,
            pending_confirmed: None,
            exited_windows: DashMap::new(),
            history: {
                let trades = TradeRecord::load_all();
                let insights = HistoryInsights::from_trades(&trades);
                if insights.trade_count > 0 {
                    info!(
                        trades = insights.trade_count,
                        spec_wr = format!("{}", insights.spec_win_rate.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or("-".into())),
                        conf_wr = format!("{}", insights.conf_win_rate.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or("-".into())),
                        avg_win_move = format!("{}", insights.avg_win_move.map(|m| format!("${:.0}", m)).unwrap_or("-".into())),
                        avg_loss_move = format!("{}", insights.avg_loss_move.map(|m| format!("${:.0}", m)).unwrap_or("-".into())),
                        reversal_rate = format!("{}", insights.reversal_rate.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or("-".into())),
                        flow_agree_wr = format!("{}", insights.flow_agree_win_rate.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or("-".into())),
                        "loaded trade history insights"
                    );
                }
                insights
            },
        }
    }

    /// Dynamic bet size based on cumulative profit.
    /// Base $20, add $5 for every $20 of cumulative profit. Cap at $50.
    /// Only risk more when the profits justify it.
    fn current_bet_size(&self) -> Decimal {
        let base = self.config.bet_size_usd;
        if self.session_pnl <= 0.0 {
            return base;
        }
        let profit_tiers = (self.session_pnl / 20.0).floor() as u32;
        let bump = Decimal::from(profit_tiers.min(6)) * dec!(5);
        (base + bump).min(dec!(50))
    }

    fn save_stats(&self) {
        PersistedStats {
            cumulative_pnl: self.session_pnl,
            wins: self.session_wins,
            losses: self.session_losses,
            skips: self.session_skips,
        }.save();
    }

    pub async fn run(mut self) {
        info!(
            min_prob = format!("{:.0}%", self.config.min_probability * 100.0),
            min_edge = format!("{:.0}%", self.config.min_edge * 100.0),
            bet_size = %self.config.bet_size_usd,
            "BTC 5-min trader started (WebSocket mode)"
        );

        // Wait for first Binance price from WebSocket (up to 5s, then fall back to HTTP)
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            if *self.binance_rx.borrow() > 0.0 {
                info!("Binance WebSocket connected");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!("Binance WebSocket not ready, seeding from HTTP");
                if let Ok(p) = self.fetch_binance_price().await {
                    // Seed the vol buffer
                    self.price_samples.push_back(p);
                }
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        // Main loop: 200ms tick for signal evaluation.
        // Binance price comes from WebSocket (near-zero latency).
        // This tick drives Chainlink polling, vol updates, and trade evaluation.
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(200));
        let mut last_log_secs: u64 = 0;

        loop {
            interval.tick().await;

            // Read latest Binance price from WebSocket (non-blocking)
            let binance_price = *self.binance_rx.borrow();
            if binance_price <= 0.0 {
                continue; // WebSocket not yet connected
            }

            // Kill switch: stop BTC trading if cumulative loss exceeds $25
            // (currently at +$12, so this gives ~$37 of room = 7 consecutive $5 losses)
            if self.session_pnl < -25.0 {
                warn!(
                    pnl = format!("${:+.2}", self.session_pnl),
                    "BTC TRADER KILL SWITCH — cumulative loss > $25, shutting down"
                );
                break;
            }

            let now = now_secs();
            let now_ms = now_millis();
            let window_start = now - (now % WINDOW_SECS);

            // 0. Check for pending order completion (non-blocking)
            if let Some(ref mut rx) = self.pending_order {
                match rx.try_recv() {
                    Ok(Ok(order_id)) => {
                        // Order placed successfully — update speculative_order with real ID
                        if let Some((ref mut oid, _, _, _, _)) = self.speculative_order {
                            if oid == "pending" {
                                info!(order_id = %order_id, "speculative order placed (async)");
                                *oid = order_id;
                            }
                        }
                        self.pending_order = None;
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "speculative order failed (async)");
                        if let Some((ref mut oid, _, _, _, _)) = self.speculative_order {
                            if oid == "pending" {
                                *oid = "failed".to_string();
                            }
                        }
                        self.pending_order = None;
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                        // Still in flight — keep waiting
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        // Sender dropped (task panicked)
                        warn!("speculative order task dropped");
                        if let Some((ref mut oid, _, _, _, _)) = self.speculative_order {
                            if oid == "pending" {
                                *oid = "failed".to_string();
                            }
                        }
                        self.pending_order = None;
                    }
                }
            }

            // 0b. Check for pending confirmed order completion (non-blocking)
            if let Some(ref mut rx) = self.pending_confirmed {
                match rx.try_recv() {
                    Ok(Ok(order_id)) => {
                        info!(order_id = %order_id, "confirmed order placed (async)");
                        self.pending_confirmed = None;
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "confirmed order failed (async)");
                        self.session_skips += 1;
                        self.pending_confirmed = None;
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        warn!("confirmed order task dropped");
                        self.pending_confirmed = None;
                    }
                }
            }

            // 1. Chainlink price (every 2s for fresher oracle data)
            if now - self.last_chainlink_poll >= 2 {
                match self.fetch_chainlink_price().await {
                    Ok((price, updated_at)) => {
                        self.chainlink_price = price;
                        self.chainlink_updated_at = updated_at;
                    }
                    Err(e) => {
                        warn!(error = %e, "Chainlink fetch failed");
                    }
                }
                self.last_chainlink_poll = now;
            }

            // 2. Window rotation: set strike, check previous bet result
            if window_start != self.strike_window && self.chainlink_price > 0.0 {
                self.rotate_window(binance_price, window_start).await;
            }

            // 3. Update vol samples (throttle to ~2/sec to avoid bloating buffers)
            if self.price_samples.back().is_none_or(|_| now_ms % 500 < 200) {
                if let Some(&prev) = self.price_samples.back() {
                    if prev > 0.0 {
                        self.log_returns.push_back((binance_price / prev).ln());
                        if self.log_returns.len() > 300 {
                            self.log_returns.pop_front();
                        }
                    }
                }
                self.price_samples.push_back(binance_price);
                if self.price_samples.len() > 600 {
                    self.price_samples.pop_front();
                }
            }

            // 4. Fetch market by predicted slug (every 10s or when missing)
            let need_fetch = self.current_market.as_ref().is_none_or(|m| m.window_start != window_start)
                || now - self.last_market_fetch >= 10;
            if need_fetch && now - self.last_market_fetch >= 2 {
                match self.fetch_market_by_slug(window_start).await {
                    Ok(Some(market)) => {
                        debug!(
                            window = window_start,
                            up = format!("{:.3}", market.up_price),
                            down = format!("{:.3}", market.down_price),
                            "found live BTC 5-min market"
                        );
                        self.current_market = Some(market);
                    }
                    Ok(None) => {
                        debug!(window = window_start, "no BTC 5-min market for this window");
                    }
                    Err(e) => {
                        warn!(error = %e, "gamma API fetch failed");
                    }
                }
                self.last_market_fetch = now;
            }

            // 5. Pre-cache order books (every 2s so they're ready when signal fires)
            if now - self.last_book_fetch >= 2 {
                if let Some(m) = &self.current_market {
                    let (bid_up, ask_up) = self.fetch_book(&m.token_id_up).await;
                    let (bid_dn, ask_dn) = self.fetch_book(&m.token_id_down).await;
                    self.cached_book_up = CachedBook { best_bid: bid_up, best_ask: ask_up };
                    self.cached_book_down = CachedBook { best_bid: bid_dn, best_ask: ask_dn };
                }
                self.last_book_fetch = now;
            }

            // 6. Log price status every 30s (deduplicated)
            if self.chainlink_price > 0.0 && now % 30 == 0 && now != last_log_secs {
                last_log_secs = now;
                let secs_into = now - window_start;
                let spread = binance_price - self.chainlink_price;
                let move_from_strike = self.chainlink_price - self.strike;
                let age = now.saturating_sub(self.chainlink_updated_at);
                info!(
                    binance = format!("${:.2}", binance_price),
                    chainlink = format!("${:.2}", self.chainlink_price),
                    strike = format!("${:.2}", self.strike),
                    move_usd = format!("{:+.2}", move_from_strike),
                    spread = format!("{:+.2}", spread),
                    oracle_age = format!("{}s", age),
                    secs_in = secs_into,
                    record = format!("{}-{}", self.session_wins, self.session_losses),
                    flow = format!("{:+.1}", {
                        let s = self.signals_rx.borrow().clone();
                        if s.volume > 0.0 { s.order_flow / s.volume * 100.0 } else { 0.0 }
                    }),
                    funding = format!("{:+.4}%", self.signals_rx.borrow().funding_rate * 100.0),
                    "price status"
                );
            }

            // 7. Evaluate trade (or check for mid-window exit if already in)
            if self.strike > 0.0
                && self.log_returns.len() >= self.config.vol_min_samples
                && self.current_market.is_some()
            {
                self.evaluate_and_trade(binance_price, now, window_start).await;
                // Mid-window exit disabled: sells always fail on thin books,
                // and 5-min markets bounce too much. Just ride to resolution.
                // Max loss = bet_size ($5) either way.
            }
        }
    }

    /// Handle window rotation: score previous bet, reset state, set new strike.
    async fn rotate_window(&mut self, binance_price: f64, window_start: u64) {
        // Check previous bet result
        if let Some(bet) = self.last_bet.take() {
            // For GTC orders that weren't instantly filled, check if we actually got a position
            let actually_filled = if bet.filled {
                true // FAK was confirmed filled
            } else {
                // GTC was pending — check position tracker for evidence of fill
                let has_pos = self.positions.net_size(&bet.token_id) > Decimal::ZERO;
                if !has_pos {
                    info!(
                        side = %bet.side,
                        "GTC order never filled — skipping result"
                    );
                    self.session_skips += 1;
                }
                has_pos
            };

            if actually_filled {
                let mid_exit = self.exited_windows.contains_key(&self.strike_window);
                let up_won = self.chainlink_price >= bet.strike;
                let we_won = (bet.side == "UP" && up_won) || (bet.side == "DOWN" && !up_won);
                let final_move = self.chainlink_price - bet.strike;
                let entry_direction_up = bet.side == "UP";
                let close_direction_up = final_move > 0.0;
                let move_reversed = entry_direction_up != close_direction_up;

                let pnl = if mid_exit {
                    self.session_losses += 1;
                    let remaining = self.positions.net_size(&bet.token_id);
                    let p = if remaining <= Decimal::ZERO {
                        -(bet.size_usd * 0.3)
                    } else {
                        -bet.size_usd
                    };
                    self.session_pnl += p;
                    info!(
                        result = "LOSS (mid-exit) ✗",
                        side = %bet.side,
                        pnl = format!("${:+.2}", p),
                        session_pnl = format!("${:+.2}", self.session_pnl),
                        record = format!("{}-{}", self.session_wins, self.session_losses),
                        "BTC 5-MIN RESULT"
                    );
                    p
                } else {
                    let payout = if we_won { bet.size_usd / bet.price_paid } else { 0.0 };
                    let p = payout - bet.size_usd;
                    self.session_pnl += p;
                    if we_won { self.session_wins += 1; } else { self.session_losses += 1; }
                    let move_usd = final_move.abs();
                    let next_bet = self.current_bet_size();
                    info!(
                        result = if we_won { "WIN ✓" } else { "LOSS ✗" },
                        side = %bet.side,
                        pnl = format!("${:+.2}", p),
                        session_pnl = format!("${:+.2}", self.session_pnl),
                        next_bet = %next_bet,
                        record = format!("{}-{}", self.session_wins, self.session_losses),
                        close = format!("${:.2}", self.chainlink_price),
                        strike = format!("${:.2}", bet.strike),
                        move_usd = format!("${:.0}", move_usd),
                        "BTC 5-MIN RESULT"
                    );
                    p
                };

                // Write full trade record to disk
                let record = TradeRecord {
                    window_start: bet.window_start,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    side: bet.side.clone(),
                    entry_secs_in: bet.entry_secs_in,
                    speculative: bet.speculative,
                    price_paid: bet.price_paid,
                    size_usd: bet.size_usd,
                    strike: bet.strike,
                    binance_at_entry: bet.binance_at_entry,
                    chainlink_at_entry: bet.chainlink_at_entry,
                    binance_move_at_entry: bet.binance_move,
                    chainlink_move_at_entry: bet.chainlink_move,
                    spread_ema: self.spread_ema,
                    model_prob: bet.model_prob,
                    edge: bet.edge,
                    ann_vol: bet.ann_vol,
                    order_flow_pct: bet.order_flow_pct,
                    funding_rate: bet.funding_rate,
                    liq_pressure: bet.liq_pressure,
                    market_bid: bet.market_bid,
                    market_ask: bet.market_ask,
                    chainlink_at_close: self.chainlink_price,
                    final_move,
                    won: we_won && !mid_exit,
                    pnl,
                    mid_exit,
                    move_reversed,
                    peak_move_for_us: 0.0, // TODO: track intra-window peak
                };
                record.append_to_disk();
                info!(
                    reversed = move_reversed,
                    spec = bet.speculative,
                    entry_secs = bet.entry_secs_in,
                    bn_move = format!("${:+.0}", bet.binance_move),
                    final_move = format!("${:+.0}", final_move),
                    "trade record saved"
                );

                self.save_stats();

                // Refresh history insights every time we add a trade
                let trades = TradeRecord::load_all();
                self.history = HistoryInsights::from_trades(&trades);
            }
        }

        // Cancel any speculative order from previous window ON THE EXCHANGE
        if let Some((order_id, side, _, _, _)) = self.speculative_order.take() {
            if order_id != "failed" {
                info!(order_id = %order_id, side = %side, "canceling stale speculative order on exchange (window rotated)");
                if let Err(e) = self.order_manager.execute_strict(
                    crate::strategy::StrategyAction::CancelOrder { order_id: order_id.clone() }
                ).await {
                    warn!(order_id = %order_id, error = %e, "failed to cancel stale speculative on exchange");
                }
            }
        }

        // Reset exposure for new window
        self.exposure = dec!(0);
        self.exited_windows.retain(|k, _| *k >= window_start.saturating_sub(WINDOW_SECS * 10));

        // Update spread EMA (exponential moving average, alpha=0.3 for smoothing)
        if binance_price > 0.0 && self.chainlink_price > 0.0 {
            let current_spread = binance_price - self.chainlink_price;
            if self.spread_ema == 0.0 {
                self.spread_ema = current_spread;
            } else {
                self.spread_ema = 0.3 * current_spread + 0.7 * self.spread_ema;
            }
        }

        // Set new strike from Chainlink
        self.strike = self.chainlink_price;
        self.strike_window = window_start;
        self.binance_at_open = binance_price;
        self.current_market = None;

        info!(
            strike = format!("${:.2}", self.strike),
            spread_ema = format!("{:+.2}", self.spread_ema),
            window = window_start,
            session_pnl = format!("${:+.2}", self.session_pnl),
            record = format!("{}-{} (skip {})", self.session_wins, self.session_losses, self.session_skips),
            "new window"
        );
    }

    async fn fetch_binance_price(&self) -> anyhow::Result<f64> {
        #[derive(serde::Deserialize)]
        struct R { price: String }
        let r: R = self.http.get(BINANCE_PRICE_URL).send().await?.json().await?;
        Ok(r.price.parse()?)
    }

    async fn fetch_chainlink_price(&self) -> anyhow::Result<(f64, u64)> {
        let provider = ProviderBuilder::new()
            .connect_http(POLYGON_RPC.parse()?);
        let feed: Address = CHAINLINK_BTC_USD.parse()?;
        let agg = IChainlinkAggregator::new(feed, &provider);
        let result = agg.latestRoundData().call().await?;
        let answer: I256 = result.answer;
        let updated_at: u64 = result.updatedAt.to::<u64>();
        let price = answer.to_string().parse::<f64>().unwrap_or(0.0) / CHAINLINK_DECIMALS;
        Ok((price, updated_at))
    }

    /// Fetch the BTC 5-min market for a given window by its predictable slug.
    async fn fetch_market_by_slug(&self, window_start: u64) -> anyhow::Result<Option<LiveMarket>> {
        let slug = format!("btc-updown-5m-{window_start}");
        let url = format!("{GAMMA_API}/events/slug/{slug}");

        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            debug!(slug = %slug, status = %status, "gamma API returned non-200");
            return Ok(None);
        }

        let body: serde_json::Value = resp.json().await?;

        if body.get("closed") == Some(&serde_json::Value::Bool(true)) {
            return Ok(None);
        }

        let markets = match body.get("markets").and_then(|m| m.as_array()) {
            Some(m) if !m.is_empty() => m,
            _ => return Ok(None),
        };

        let market = &markets[0];

        if market.get("closed") == Some(&serde_json::Value::Bool(true)) {
            return Ok(None);
        }

        // clobTokenIds and outcomePrices are JSON-encoded strings
        let token_ids: Vec<String> = match market.get("clobTokenIds") {
            Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => return Ok(None),
        };
        if token_ids.len() < 2 {
            return Ok(None);
        }

        let token_up = match U256::from_str(&token_ids[0]) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        let token_down = match U256::from_str(&token_ids[1]) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };

        let prices: Vec<String> = match market.get("outcomePrices") {
            Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => return Ok(None),
        };
        let up_price: f64 = prices.first().and_then(|s| s.parse().ok()).unwrap_or(0.5);
        let down_price: f64 = prices.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.5);

        Ok(Some(LiveMarket {
            token_id_up: token_up,
            token_id_down: token_down,
            up_price,
            down_price,
            window_start,
        }))
    }

    /// Fetch the CLOB order book to find actual best bid/ask for a token.
    /// Returns (best_bid, best_ask) as Option<f64> each.
    async fn fetch_book(&self, token_id: &U256) -> (Option<f64>, Option<f64>) {
        let url = format!("https://clob.polymarket.com/book?token_id={token_id}");
        let resp = match self.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => return (None, None),
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return (None, None),
        };

        let best_bid = body.get("bids")
            .and_then(|b| b.as_array())
            .and_then(|arr| arr.last()) // bids sorted ascending, last = highest
            .and_then(|b| b.get("price"))
            .and_then(|p| p.as_str())
            .and_then(|s| s.parse::<f64>().ok());

        let best_ask = body.get("asks")
            .and_then(|a| a.as_array())
            .and_then(|arr| arr.first()) // asks sorted ascending, first = lowest
            .and_then(|a| a.get("price"))
            .and_then(|p| p.as_str())
            .and_then(|s| s.parse::<f64>().ok());

        (best_bid, best_ask)
    }

    async fn evaluate_and_trade(&mut self, binance_price: f64, now: u64, window_start: u64) {
        let secs_into = now - window_start;
        let secs_left = WINDOW_SECS.saturating_sub(secs_into);

        // Already committed this window (speculative filled or confirmed entry placed)?
        if self.traded_windows.contains_key(&window_start) {
            return;
        }

        // Too early for anything
        if secs_into < SPECULATIVE_ENTRY_SECS {
            return;
        }

        // Don't enter after entry window
        if secs_into > self.config.entry_window_secs {
            return;
        }

        // Exposure limit
        if self.exposure >= self.config.max_exposure_usd {
            return;
        }

        let market = match &self.current_market {
            Some(m) if m.window_start == window_start => m.clone(),
            _ => return,
        };

        // ── Compute direction from price sources ──
        // Strike is set from Chainlink at window open, so compare Chainlink directly.
        // For Binance (early signal), normalize by comparing to Binance's OWN price at open,
        // not the Chainlink strike — this avoids the spread bias that made us always go UP.
        let chainlink_move = self.chainlink_price - self.strike;
        let binance_move = binance_price - self.binance_at_open; // raw Binance delta, no spread adjustment
        let abs_chainlink_move = chainlink_move.abs();
        let abs_binance_move = binance_move.abs();
        let chainlink_up = chainlink_move > 0.0;
        let binance_up = binance_move > 0.0;
        let chainlink_stale = abs_chainlink_move < 5.0;

        // ── No speculative stage: wait for confirmed signal at 60s+ ──
        if secs_into < CONFIRMED_ENTRY_SECS {
            return;
        }

        // Direction check with full requirements (both sources must agree if Chainlink active)
        let direction_up = if chainlink_stale {
            if abs_binance_move < MIN_MOVE_LATE_USD {
                return;
            }
            binance_up
        } else {
            if chainlink_up != binance_up {
                return;
            }
            chainlink_up
        };

        // Scaled move threshold for confirmed entry
        let min_move = if secs_left >= EARLY_ENTRY_SECS_LEFT {
            MIN_MOVE_EARLY_USD
        } else {
            let frac = secs_left as f64 / EARLY_ENTRY_SECS_LEFT as f64;
            MIN_MOVE_LATE_USD + (MIN_MOVE_EARLY_USD - MIN_MOVE_LATE_USD) * frac
        };

        if abs_chainlink_move < min_move && abs_binance_move < min_move {
            return;
        }

        // ── Volatility cushion — reject coin-flip zones ──
        let ann_vol = self.calc_annualized_vol();
        if ann_vol > 0.0 {
            let t_remaining_yr = secs_left as f64 / (365.25 * 24.0 * 3600.0);
            let sigma_remaining = self.chainlink_price * ann_vol * t_remaining_yr.sqrt();
            let best_move = abs_chainlink_move.max(abs_binance_move);
            if best_move < 1.0 * sigma_remaining {
                if secs_into % 10 == 0 && now_millis() % 1000 < 200 {
                    debug!(
                        move_usd = format!("${:.0}", best_move),
                        sigma = format!("${:.0}", sigma_remaining),
                        ratio = format!("{:.1}x", best_move / sigma_remaining),
                        secs_left = secs_left,
                        "coin-flip zone — skipping confirmed entry"
                    );
                }
                return;
            }
        }

        // ── Model probability check ──
        // When Chainlink is stale, estimate where it will update to using Binance - spread_ema
        // (spread_ema = Binance - Chainlink, so Chainlink ≈ Binance - spread_ema)
        let estimated_chainlink = binance_price - self.spread_ema;
        let model_price = if chainlink_stale { estimated_chainlink } else { self.chainlink_price };
        let p_up_raw = prob_above(model_price, self.strike, ann_vol, secs_left as f64);

        // Pure model — no signal adjustments (flow/funding/liq add noise, not edge)
        let signals = self.signals_rx.borrow().clone();
        let flow_pct = if signals.volume > 0.0 {
            signals.order_flow / signals.volume
        } else {
            0.0
        };

        let p_up = p_up_raw.clamp(0.01, 0.99);
        let p_down = 1.0 - p_up;

        let (side_label, token_id, _market_price, model_prob) = if direction_up {
            ("UP", market.token_id_up, market.up_price, p_up)
        } else {
            ("DOWN", market.token_id_down, market.down_price, p_down)
        };

        if model_prob < self.config.min_probability {
            return;
        }

        // ── Execution ──
        // Edge check is the price guard: max we'll pay = model_prob - min_edge.
        // e.g. model 90% → max $0.85, model 70% → max $0.65.
        // FAK if ask is within our edge, otherwise GTC at best_bid+1c.
        let our_cached = if direction_up { &self.cached_book_up } else { &self.cached_book_down };
        let (best_bid, best_ask) = (our_cached.best_bid, our_cached.best_ask);
        let max_willing = model_prob - self.config.min_edge;

        let (buy_price, use_fak) = if let Some(ask) = best_ask {
            if ask <= max_willing {
                // Ask is within our edge — take it instantly via FAK
                let p = Decimal::from_f64_retain(ask).unwrap_or(dec!(0.50));
                (p.round_dp(2), true)
            } else if let Some(bid) = best_bid {
                // Ask too expensive — sit on top of book
                let penny_above = Decimal::from_f64_retain(bid + 0.01)
                    .unwrap_or(dec!(0.50))
                    .round_dp(2);
                let max_willing_dec = Decimal::from_f64_retain(max_willing)
                    .unwrap_or(dec!(0.50))
                    .round_dp(2);
                let capped = penny_above.min(max_willing_dec);
                if capped.to_string().parse::<f64>().unwrap_or(1.0) > max_willing {
                    return; // even top-of-book is above our max willing
                }
                (capped, false)
            } else {
                return; // no book data, skip
            }
        } else if let Some(bid) = best_bid {
            let penny_above = Decimal::from_f64_retain(bid + 0.01)
                .unwrap_or(dec!(0.50))
                .round_dp(2);
            let max_willing_dec = Decimal::from_f64_retain(max_willing)
                .unwrap_or(dec!(0.50))
                .round_dp(2);
            let capped = penny_above.min(max_willing_dec);
            if capped.to_string().parse::<f64>().unwrap_or(1.0) > max_willing {
                return;
            }
            (capped, false)
        } else {
            return; // no book at all
        };

        let buy_price_f64: f64 = buy_price.to_string().parse().unwrap_or(0.50);
        let edge = model_prob - buy_price_f64;
        if edge < self.config.min_edge {
            return;
        }

        // Flat sizing — testing mode, no conviction scaling
        let bet_size = self.current_bet_size();
        let size = (bet_size / buy_price).round_dp(0);
        if size <= dec!(0) {
            return;
        }

        let size_usd: f64 = bet_size.to_string().parse().unwrap_or(30.0);

        info!(
            side = side_label,
            binance = format!("${:.2}", binance_price),
            chainlink = format!("${:.2}", self.chainlink_price),
            strike = format!("${:.2}", self.strike),
            cl_move = format!("${:+.0}", chainlink_move),
            bn_move = format!("${:+.0}", binance_move),
            model = format!("{:.0}%", model_prob * 100.0),
            edge = format!("{:.1}%", edge * 100.0),
            buy_price = %buy_price,
            bet_size = %bet_size,
            mode = if use_fak { "FAK" } else { "GTC" },
            size = %size,
            flow = format!("{:+.0}%", flow_pct * 100.0),
            secs_in = secs_into,
            secs_left = secs_left,
            ">>> CONFIRMED BTC TRADE <<<"
        );

        // Mark window as traded immediately to prevent re-entry while order is in flight
        self.traded_windows.insert(window_start, ());
        self.traded_windows.retain(|k, _| *k >= window_start.saturating_sub(WINDOW_SECS * 10));
        self.exposure += bet_size;
        self.last_bet = Some(BtcBet {
            side: side_label.to_string(),
            price_paid: buy_price_f64,
            size_usd,
            token_id,
            filled: use_fak, // FAK = assumed filled, GTC = pending
            entry_secs_in: secs_into,
            window_start,
            strike: self.strike,
            binance_at_entry: binance_price,
            chainlink_at_entry: self.chainlink_price,
            binance_move,
            chainlink_move,
            model_prob,
            edge,
            ann_vol,
            order_flow_pct: flow_pct,
            funding_rate: signals.funding_rate,
            liq_pressure: signals.liq_pressure,
            speculative: false,
            market_bid: our_cached.best_bid,
            market_ask: our_cached.best_ask,
        });

        // Non-blocking: spawn order placement so the main loop keeps monitoring prices.
        let om = self.order_manager.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mode_label = if use_fak { "FAK" } else { "GTC" };
        let side_str = side_label.to_string();
        let bp = buy_price;
        let bs = bet_size;
        if use_fak {
            tokio::spawn(async move {
                let fak_action = crate::strategy::StrategyAction::PlaceOrder {
                    token_id,
                    side: Side::Buy,
                    price: bp,
                    size,
                    taker: true,
                };
                let result = om.execute_strict(fak_action).await;
                match &result {
                    Ok(()) => tracing::info!("FAK FILLED — ${} on {} at {} (instant)", bs, side_str, bp),
                    Err(e) => tracing::warn!(error = %e, "FAK failed"),
                }
                let _ = tx.send(result.map(|()| "fak-filled".to_string()).map_err(|e| e.to_string()));
            });
        } else {
            tokio::spawn(async move {
                let result = om.place_gtc_crossing(token_id, Side::Buy, bp, size).await;
                match &result {
                    Ok(oid) => tracing::info!(order_id = %oid, "GTC BID — ${} on {} at {} (confirmed)", bs, side_str, bp),
                    Err(e) => tracing::warn!(error = %e, "GTC failed"),
                }
                let _ = tx.send(result.map_err(|e| e.to_string()));
            });
        }
        info!(mode = mode_label, "confirmed order dispatched (async) — main loop continues");
        self.pending_confirmed = Some(rx);
    }

    /// Mid-window exit: if we hold a position and Binance has reversed through strike
    /// against us, sell to cut losses instead of riding to $0.
    async fn check_mid_window_exit(&mut self, binance_price: f64, now: u64, window_start: u64) {
        // Must have an active bet this window
        let bet = match &self.last_bet {
            Some(b) => b,
            None => return,
        };

        // Don't exit in the first 90 seconds — give the trade time to develop
        let secs_into = now - window_start;
        if secs_into < 90 {
            return;
        }

        // Already exited this window
        if self.exited_windows.contains_key(&window_start) {
            return;
        }

        // Check if we actually hold the position
        let held_size = self.positions.net_size(&bet.token_id);
        if held_size <= Decimal::ZERO {
            return;
        }

        // Estimate where Chainlink will land: Binance - spread_ema ≈ Chainlink
        let estimated_chainlink = binance_price - self.spread_ema;
        let move_from_strike = estimated_chainlink - self.strike;

        // Has direction reversed against us?
        let reversed = match bet.side.as_str() {
            "UP" => move_from_strike < -15.0,   // BTC $15+ below strike → UP loses
            "DOWN" => move_from_strike > 15.0,   // BTC $15+ above strike → DOWN loses
            _ => return,
        };

        if !reversed {
            return;
        }

        // Get sell price — best bid on our token
        let our_cached = if bet.side == "UP" { &self.cached_book_up } else { &self.cached_book_down };
        let sell_price = match our_cached.best_bid {
            Some(bid) if bid > 0.05 => {
                // Sell at best bid (taker sell)
                Decimal::from_f64_retain(bid).unwrap_or(dec!(0.30)).round_dp(2)
            }
            _ => {
                // No bid or bid too low — not worth selling at dust
                return;
            }
        };

        let loss_if_hold = bet.size_usd; // lose entire position at resolution
        let recovery = (held_size * sell_price).to_string().parse::<f64>().unwrap_or(0.0);
        let loss_if_exit = bet.size_usd - recovery;

        info!(
            side = %bet.side,
            move_usd = format!("${:+.0}", move_from_strike),
            sell_price = %sell_price,
            held = %held_size,
            recovery = format!("${:.2}", recovery),
            loss_hold = format!("${:.2}", loss_if_hold),
            loss_exit = format!("${:.2}", loss_if_exit),
            saved = format!("${:.2}", loss_if_hold - loss_if_exit),
            secs_in = secs_into,
            ">>> MID-WINDOW EXIT — direction reversed, cutting losses <<<"
        );

        // Mark exited immediately so we don't double-sell on next tick
        self.exited_windows.insert(window_start, ());

        // Non-blocking: spawn sell so main loop keeps monitoring
        let om = self.order_manager.clone();
        let token = bet.token_id;
        let sp = sell_price;
        let hs = held_size;
        let saved = loss_if_hold - loss_if_exit;
        tokio::spawn(async move {
            let sell_action = crate::strategy::StrategyAction::PlaceOrder {
                token_id: token,
                side: Side::Sell,
                price: sp,
                size: hs,
                taker: true,
            };
            match om.execute_strict(sell_action).await {
                Ok(()) => {
                    tracing::info!(
                        sell_price = %sp,
                        shares = %hs,
                        "mid-window exit FILLED — saved ${:.2} vs holding to resolution", saved
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mid-window exit FAK failed — trying GTC");
                    match om.place_gtc_crossing(token, Side::Sell, sp, hs).await {
                        Ok(order_id) => {
                            tracing::info!(order_id = %order_id, "mid-window exit GTC placed at {}", sp);
                        }
                        Err(e2) => {
                            tracing::warn!(error = %e2, "mid-window exit GTC also failed — stuck holding");
                        }
                    }
                }
            }
        });
    }

    fn calc_annualized_vol(&self) -> f64 {
        if self.log_returns.len() < 10 {
            return 0.0;
        }
        let n = self.log_returns.len() as f64;
        let mean = self.log_returns.iter().sum::<f64>() / n;
        let var = self.log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        // Vol samples are ~2/sec (throttled in main loop)
        let samples_per_sec = 2.0;
        let samples_per_year = samples_per_sec * 365.25 * 24.0 * 3600.0;
        (var * samples_per_year).sqrt()
    }
}

/// Combined Binance Futures WebSocket: price + order flow + funding rate + liquidations.
/// Single connection delivers all signals with ~10ms latency.
async fn binance_futures_ws(price_tx: watch::Sender<f64>, signals_tx: watch::Sender<MarketSignals>) {
    // Rolling window accumulators (reset each 5-min window)
    let mut buy_vol: f64 = 0.0;
    let mut sell_vol: f64 = 0.0;
    let mut liq_long_usd: f64 = 0.0;
    let mut liq_short_usd: f64 = 0.0;
    let mut funding_rate: f64 = 0.0;
    let mut last_window: u64 = 0;

    loop {
        match tokio_tungstenite::connect_async(BINANCE_FUTURES_WS).await {
            Ok((ws_stream, _)) => {
                info!("Binance Futures WebSocket connected (aggTrade + markPrice + liquidations)");
                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    let text = match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => t,
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            warn!("Binance Futures WS closed");
                            break;
                        }
                        Err(e) => {
                            warn!(error = %e, "Binance Futures WS error");
                            break;
                        }
                        _ => continue,
                    };

                    // Combined stream wraps each event: {"stream":"...","data":{...}}
                    let envelope: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let stream = envelope.get("stream").and_then(|s| s.as_str()).unwrap_or("");
                    let data = match envelope.get("data") {
                        Some(d) => d,
                        None => continue,
                    };

                    // Reset accumulators on window boundary
                    let now = now_secs();
                    let window = now - (now % 300);
                    if window != last_window {
                        buy_vol = 0.0;
                        sell_vol = 0.0;
                        liq_long_usd = 0.0;
                        liq_short_usd = 0.0;
                        last_window = window;
                    }

                    if stream.contains("aggTrade") {
                        // aggTrade: price + order flow (m=true → taker sell, m=false → taker buy)
                        if let (Some(price_s), Some(qty_s), Some(is_maker)) = (
                            data.get("p").and_then(|v| v.as_str()),
                            data.get("q").and_then(|v| v.as_str()),
                            data.get("m").and_then(|v| v.as_bool()),
                        ) {
                            if let (Ok(price), Ok(qty)) = (price_s.parse::<f64>(), qty_s.parse::<f64>()) {
                                let _ = price_tx.send(price);
                                let usd_vol = price * qty;
                                if is_maker {
                                    // Buyer is maker → taker is selling
                                    sell_vol += usd_vol;
                                } else {
                                    // Buyer is taker → active buying
                                    buy_vol += usd_vol;
                                }
                            }
                        }
                    } else if stream.contains("markPrice") {
                        // markPrice: live funding rate
                        if let Some(r) = data.get("r").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                            funding_rate = r;
                        }
                    } else if stream.contains("forceOrder") {
                        // Liquidation event
                        if let Some(order) = data.get("o") {
                            let side = order.get("S").and_then(|v| v.as_str()).unwrap_or("");
                            let qty: f64 = order.get("q").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                            let price: f64 = order.get("p").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                            let usd = qty * price;
                            let symbol = order.get("s").and_then(|v| v.as_str()).unwrap_or("");
                            if symbol == "BTCUSDT" {
                                if side == "SELL" {
                                    // Long liquidated (forced sell) → bearish
                                    liq_long_usd += usd;
                                } else {
                                    // Short liquidated (forced buy) → bullish
                                    liq_short_usd += usd;
                                }
                            }
                        }
                    }

                    // Update signals (throttle to avoid excessive updates)
                    let total_vol = buy_vol + sell_vol;
                    let net_flow = buy_vol - sell_vol;
                    // Normalize liq pressure: positive = net longs liquidated (bearish)
                    let net_liq = if liq_long_usd + liq_short_usd > 0.0 {
                        (liq_long_usd - liq_short_usd) / (liq_long_usd + liq_short_usd)
                    } else {
                        0.0
                    };

                    let _ = signals_tx.send(MarketSignals {
                        order_flow: net_flow,
                        volume: total_vol,
                        funding_rate,
                        liq_pressure: net_liq,
                    });
                }
            }
            Err(e) => {
                warn!(error = %e, "Binance Futures WS connect failed");
            }
        }

        warn!("Binance Futures WS reconnecting in 2s...");
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// GBM probability that price finishes above strike.
fn prob_above(current: f64, strike: f64, vol: f64, t_left_secs: f64) -> f64 {
    if vol <= 0.0 || t_left_secs <= 0.0 {
        return if current >= strike { 1.0 } else { 0.0 };
    }
    let t_yr = t_left_secs / (365.25 * 24.0 * 3600.0);
    let sigma_t = vol * t_yr.sqrt();
    if sigma_t == 0.0 {
        return if current >= strike { 1.0 } else { 0.0 };
    }
    let d = ((current / strike).ln() + 0.5 * vol * vol * t_yr) / sigma_t;
    norm_cdf(d)
}

/// Normal CDF (Abramowitz & Stegun, max error ~1.5e-7).
fn norm_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x_abs = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * x_abs);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x_abs * x_abs).exp();
    0.5 * (1.0 + sign * y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_norm_cdf() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((norm_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((norm_cdf(-1.96) - 0.025).abs() < 1e-3);
    }

    #[test]
    fn test_prob_above() {
        let p = prob_above(84000.0, 84000.0, 0.5, 300.0);
        assert!((p - 0.5).abs() < 0.05);
        assert!(prob_above(85000.0, 84000.0, 0.5, 300.0) > 0.6);
        assert!(prob_above(83000.0, 84000.0, 0.5, 300.0) < 0.4);
    }

    #[test]
    fn test_slug_prediction() {
        let ts: u64 = 1773593400;
        assert_eq!(ts % 300, 0);
        let slug = format!("btc-updown-5m-{ts}");
        assert_eq!(slug, "btc-updown-5m-1773593400");
    }
}
