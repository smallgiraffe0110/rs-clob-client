use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::{DashMap, DashSet};
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::data::types::request::{PositionsRequest, TradesRequest};
use polymarket_client_sdk::types::{Address, Decimal, U256};
use rust_decimal_macros::dec;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::config::CopyTraderConfig;
use crate::dashboard::{DashboardUpdate, LeaderInfo, TrackedToken};
use crate::engine::EngineEvent;
use crate::market_state::MarketState;
use crate::position::PositionTracker;
use crate::strategy::StrategyAction;
use crate::wallet_scorer::ScoredWallet;

/// Metadata about each token we're tracking from leaders.
#[derive(Debug, Clone)]
struct TokenMeta {
    title: String,
    cur_price: Decimal,
    days_remaining: i64,
    /// Weighted average of leaders' entry prices
    leader_avg_entry: Decimal,
    leader_entry_weight: Decimal,
}

pub struct CopyTracker {
    data_client: polymarket_client_sdk::data::Client,
    positions: Arc<PositionTracker>,
    market_state: Arc<MarketState>,
    config: CopyTraderConfig,
    engine_tx: mpsc::Sender<EngineEvent>,
    dashboard_tx: broadcast::Sender<DashboardUpdate>,
    /// token_id -> (target_size, last_known_price) for exit detection
    tracked_tokens: DashMap<U256, (Decimal, Decimal)>,
    /// Tokens we've stopped out of — prevent re-entry until leader exits
    stopped_out: DashSet<U256>,
    /// Scored wallets from WalletScorer (replaces self-managed leader discovery)
    scored_wallets: Arc<RwLock<Vec<ScoredWallet>>>,
    /// Configured leader addresses (always followed regardless of scoring)
    configured_leaders: Vec<Address>,
    /// Last trade poll timestamp per leader address
    last_trade_poll: DashMap<Address, i64>,
}

impl CopyTracker {
    pub fn new(
        positions: Arc<PositionTracker>,
        market_state: Arc<MarketState>,
        config: CopyTraderConfig,
        engine_tx: mpsc::Sender<EngineEvent>,
        dashboard_tx: broadcast::Sender<DashboardUpdate>,
        scored_wallets: Arc<RwLock<Vec<ScoredWallet>>>,
    ) -> Self {
        // Parse configured leader addresses up front
        let configured_leaders: Vec<Address> = config
            .leaders
            .iter()
            .filter_map(|addr_str| match addr_str.parse::<Address>() {
                Ok(addr) => Some(addr),
                Err(e) => {
                    warn!(addr = %addr_str, error = %e, "invalid leader address, skipping");
                    None
                }
            })
            .collect();

        Self {
            data_client: polymarket_client_sdk::data::Client::default(),
            positions,
            market_state,
            config,
            engine_tx,
            dashboard_tx,
            tracked_tokens: DashMap::new(),
            stopped_out: DashSet::new(),
            scored_wallets,
            configured_leaders,
            last_trade_poll: DashMap::new(),
        }
    }

    pub async fn run(&self) {
        info!("copy tracker starting");

        let interval = tokio::time::Duration::from_secs(self.config.poll_interval_secs);

        loop {
            let leaders = self.get_leaders().await;
            if leaders.is_empty() {
                warn!("no leaders to follow (scorer may still be running), waiting...");
                tokio::time::sleep(interval).await;
                continue;
            }

            self.poll_and_act(&leaders).await;

            tokio::time::sleep(interval).await;
        }
    }

    /// Build the leader list from scored wallets + configured addresses.
    async fn get_leaders(&self) -> Vec<(Address, Option<ScoredWallet>)> {
        let scored = self.scored_wallets.read().await;

        let mut seen = std::collections::HashSet::new();
        let mut leaders = Vec::new();

        // Scored wallets first (they have metadata)
        for sw in scored.iter() {
            if seen.insert(sw.address) {
                leaders.push((sw.address, Some(sw.clone())));
            }
        }

        // Then configured leaders (no score data)
        for &addr in &self.configured_leaders {
            if seen.insert(addr) {
                leaders.push((addr, None));
            }
        }

        leaders
    }

    async fn poll_and_act(&self, leaders: &[(Address, Option<ScoredWallet>)]) {
        // Aggregate target positions across all leaders (score-weighted)
        // token_id -> (weighted_sum, total_weight, meta)
        let mut targets: HashMap<U256, (Decimal, Decimal, TokenMeta)> = HashMap::new();
        let mut leader_infos = Vec::new();

        for (leader_addr, scored) in leaders {
            let Ok(req) = PositionsRequest::builder()
                .user(*leader_addr)
                .limit(500)
            else {
                continue;
            };
            let req = req.size_threshold(dec!(0.1)).build();

            match self.data_client.positions(&req).await {
                Ok(positions) => {
                    let (username, win_rate_str, score_str) = match scored {
                        Some(sw) => (
                            sw.username.clone().unwrap_or_else(|| format!("{:.8}", leader_addr)),
                            format!("{:.1}%", sw.win_rate * 100.0),
                            format!("{:.3}", sw.score),
                        ),
                        None => (
                            format!("{:.8}", leader_addr),
                            "-".to_string(),
                            "-".to_string(),
                        ),
                    };
                    let total_pnl: Decimal = positions.iter().map(|p| p.cash_pnl).sum();

                    leader_infos.push(LeaderInfo {
                        address: format!("{leader_addr}"),
                        username,
                        pnl: total_pnl.to_string(),
                        num_positions: positions.len(),
                        win_rate: win_rate_str,
                        score: score_str,
                    });

                    debug!(
                        leader = %leader_addr,
                        positions = positions.len(),
                        "fetched leader positions"
                    );

                    let today = Utc::now().date_naive();

                    for pos in &positions {
                        let days_remaining = (pos.end_date - today).num_days();

                        // Skip expired/resolved markets
                        if days_remaining < 0 {
                            continue;
                        }

                        // Skip markets too far from resolution
                        if days_remaining > self.config.max_days_to_resolution {
                            debug!(title = %pos.title, days = days_remaining, "skipping: too far from resolution");
                            continue;
                        }

                        // Skip excluded keywords
                        if self.config.exclude_title_keywords.iter().any(|kw| {
                            pos.title.to_lowercase().contains(&kw.to_lowercase())
                        }) {
                            debug!(title = %pos.title, "skipping: excluded keyword");
                            continue;
                        }

                        let scaled = pos.size * self.config.scale_factor;
                        let weight = scored
                            .as_ref()
                            .map(|sw| Decimal::try_from(sw.score).unwrap_or(dec!(1)))
                            .unwrap_or(dec!(1));
                        let entry = targets
                            .entry(pos.asset)
                            .or_insert_with(|| {
                                (
                                    Decimal::ZERO,
                                    Decimal::ZERO,
                                    TokenMeta {
                                        title: pos.title.clone(),
                                        cur_price: pos.cur_price,
                                        days_remaining,
                                        leader_avg_entry: Decimal::ZERO,
                                        leader_entry_weight: Decimal::ZERO,
                                    },
                                )
                            });
                        entry.0 += scaled * weight;
                        entry.1 += weight;
                        // Accumulate weighted leader entry price
                        entry.2.leader_avg_entry += pos.avg_price * weight;
                        entry.2.leader_entry_weight += weight;
                        // Update price to latest seen
                        entry.2.cur_price = pos.cur_price;
                    }
                }
                Err(e) => {
                    warn!(leader = %leader_addr, error = %e, "failed to fetch positions");
                }
            }

            // Fetch recent trades for this leader
            self.poll_leader_trades(*leader_addr, leaders).await;
        }

        // Feed current prices into market state for mark-to-market PnL
        for (token_id, (_ws, _tw, meta)) in &targets {
            self.market_state.update_mark_price(*token_id, meta.cur_price);
        }

        // Compute deltas and build actions
        let mut actions = Vec::new();
        let mut tracked_token_infos = Vec::new();

        // Stop loss check: scan positions before normal target processing
        for (token_id, (_weighted_sum, _total_weight, meta)) in &targets {
            if self.stopped_out.contains(token_id) {
                continue;
            }
            if let Some(pos) = self.positions.get_position(token_id) {
                if pos.net_size > Decimal::ZERO && pos.avg_entry_price > Decimal::ZERO {
                    let cur_price = meta.cur_price;
                    let pnl_pct = (cur_price - pos.avg_entry_price) / pos.avg_entry_price;
                    if pnl_pct < -self.config.stop_loss_pct {
                        let price = (cur_price - self.config.max_slippage)
                            .max(dec!(0.01))
                            .round_dp(2);
                        actions.push(StrategyAction::PlaceOrder {
                            token_id: *token_id,
                            side: Side::Sell,
                            price,
                            size: pos.net_size,
                            taker: true,
                        });
                        self.stopped_out.insert(*token_id);
                        let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                            event_type: "STOP_LOSS".into(),
                            token_title: meta.title.clone(),
                            details: format!(
                                "Sold {} @ {} (entry {} loss {:.1}%)",
                                pos.net_size, price, pos.avg_entry_price,
                                pnl_pct * dec!(100)
                            ),
                        });
                        info!(
                            token = %token_id,
                            cur_price = %cur_price,
                            avg_entry = %pos.avg_entry_price,
                            loss_pct = %pnl_pct,
                            title = %meta.title,
                            "stop loss triggered"
                        );
                    }
                }
            }
        }

        for (token_id, (weighted_sum, total_weight, meta)) in &targets {
            // Skip tokens we've stopped out of
            if self.stopped_out.contains(token_id) {
                continue;
            }

            let target_avg = if *total_weight > Decimal::ZERO {
                *weighted_sum / *total_weight
            } else {
                Decimal::ZERO
            };

            let our_size = self.positions.net_size(token_id);
            let delta = target_avg - our_size;
            let cur_price = meta.cur_price;

            // Skip if position value is below threshold
            let delta_value = delta.abs() * cur_price;
            if delta_value < self.config.min_position_usd {
                // Still track it for dashboard
                tracked_token_infos.push(TrackedToken {
                    token_id: token_id.to_string(),
                    title: meta.title.clone(),
                    target_size: target_avg.round_dp(2).to_string(),
                    our_size: our_size.round_dp(2).to_string(),
                    leader_price: cur_price.to_string(),
                    delta: delta.round_dp(2).to_string(),
                    days_remaining: format_days_remaining(meta.days_remaining),
                });
                continue;
            }

            if delta > Decimal::ZERO {
                // Compute weighted average leader entry price
                let leader_entry = if meta.leader_entry_weight > Decimal::ZERO {
                    meta.leader_avg_entry / meta.leader_entry_weight
                } else {
                    cur_price
                };
                let max_allowed = (leader_entry + self.config.max_entry_drift)
                    .min(self.config.max_entry_price);

                // Price guard: skip if current price drifted too far above leader's entry
                if cur_price > max_allowed {
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "PRICE_GUARD".into(),
                        token_title: meta.title.clone(),
                        details: format!(
                            "Skipped buy @ {} (leader entry {} + drift {} = max {})",
                            cur_price, leader_entry.round_dp(2),
                            self.config.max_entry_drift, max_allowed.round_dp(2)
                        ),
                    });
                    info!(
                        token = %token_id,
                        cur_price = %cur_price,
                        leader_entry = %leader_entry.round_dp(2),
                        max_allowed = %max_allowed.round_dp(2),
                        title = %meta.title,
                        "skipping buy: price too far above leader entry"
                    );
                } else {
                    // Need to buy
                    let price = (cur_price + self.config.max_slippage)
                        .min(dec!(0.99))
                        .round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id: *token_id,
                        side: Side::Buy,
                        price,
                        size: delta.round_dp(2),
                        taker: true,
                    });
                    info!(
                        token = %token_id,
                        side = "BUY",
                        size = %delta.round_dp(2),
                        price = %price,
                        title = %meta.title,
                        "copy action"
                    );
                }
            } else if delta < Decimal::ZERO {
                // Need to sell
                let price = (cur_price - self.config.max_slippage)
                    .max(dec!(0.01))
                    .round_dp(2);
                actions.push(StrategyAction::PlaceOrder {
                    token_id: *token_id,
                    side: Side::Sell,
                    price,
                    size: delta.abs().round_dp(2),
                    taker: true,
                });
                info!(
                    token = %token_id,
                    side = "SELL",
                    size = %delta.abs().round_dp(2),
                    price = %price,
                    title = %meta.title,
                    "copy action"
                );
            }

            // Update tracked tokens with target size and last known price
            self.tracked_tokens.insert(*token_id, (target_avg, cur_price));

            tracked_token_infos.push(TrackedToken {
                token_id: token_id.to_string(),
                title: meta.title.clone(),
                target_size: target_avg.round_dp(2).to_string(),
                our_size: our_size.round_dp(2).to_string(),
                leader_price: cur_price.to_string(),
                delta: delta.round_dp(2).to_string(),
                days_remaining: format_days_remaining(meta.days_remaining),
            });
        }

        // Exit detection: close positions leaders have abandoned
        let target_keys: std::collections::HashSet<U256> =
            targets.keys().copied().collect();
        let mut to_remove = Vec::new();

        for entry in self.tracked_tokens.iter() {
            let token_id = *entry.key();
            if !target_keys.contains(&token_id) {
                let our_size = self.positions.net_size(&token_id);
                if our_size > Decimal::ZERO {
                    // Leaders have exited — sell at last known price minus slippage
                    let last_price = entry.value().1;
                    let price = (last_price - self.config.max_slippage)
                        .max(dec!(0.01))
                        .round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id,
                        side: Side::Sell,
                        price,
                        size: our_size,
                        taker: true,
                    });
                    info!(
                        token = %token_id,
                        size = %our_size,
                        price = %price,
                        "exit detection: leaders abandoned position"
                    );
                }
                to_remove.push(token_id);
            }
        }
        for token_id in to_remove {
            self.tracked_tokens.remove(&token_id);
            // Leader exited = fresh slate, allow re-entry if they come back
            self.stopped_out.remove(&token_id);
        }

        // Send actions to engine
        if !actions.is_empty() {
            info!(count = actions.len(), "sending copy actions to engine");
            if let Err(e) = self.engine_tx.send(EngineEvent::CopyActions(actions)).await {
                error!(error = %e, "failed to send copy actions");
            }
        }

        // Send dashboard update
        let _ = self.dashboard_tx.send(DashboardUpdate::LeaderUpdate {
            leaders: leader_infos,
            tracked_tokens: tracked_token_infos,
        });
    }

    /// Fetch recent trades for a leader and broadcast them to the dashboard.
    async fn poll_leader_trades(
        &self,
        leader_addr: Address,
        leaders: &[(Address, Option<ScoredWallet>)],
    ) {
        let now_ts = Utc::now().timestamp();

        // Get the last poll timestamp (default to 60s ago on first poll)
        let since = self
            .last_trade_poll
            .get(&leader_addr)
            .map(|v| *v)
            .unwrap_or(now_ts - 60);

        let req = TradesRequest::builder()
            .user(leader_addr)
            .build();

        match self.data_client.trades(&req).await {
            Ok(trades) => {
                let scored_wallet = leaders
                    .iter()
                    .find(|(addr, _)| *addr == leader_addr)
                    .and_then(|(_, sw)| sw.as_ref());

                let leader_name = scored_wallet
                    .and_then(|sw| sw.username.clone())
                    .unwrap_or_else(|| format!("{:.8}", leader_addr));

                let leader_score = scored_wallet
                    .map(|sw| format!("{:.3}", sw.score))
                    .unwrap_or_else(|| "-".to_string());

                for trade in &trades {
                    // Only emit trades newer than our last poll
                    if trade.timestamp <= since {
                        continue;
                    }

                    let ts = DateTime::from_timestamp(trade.timestamp, 0)
                        .map(|dt| dt.format("%H:%M:%S").to_string())
                        .unwrap_or_else(|| trade.timestamp.to_string());

                    let _ = self.dashboard_tx.send(DashboardUpdate::LeaderTrade {
                        leader_address: format!("{leader_addr}"),
                        leader_name: leader_name.clone(),
                        leader_score: leader_score.clone(),
                        token_title: trade.title.clone(),
                        side: format!("{}", trade.side),
                        size: trade.size.to_string(),
                        price: trade.price.to_string(),
                        timestamp: ts,
                    });
                }
            }
            Err(e) => {
                debug!(leader = %leader_addr, error = %e, "failed to fetch leader trades");
            }
        }

        self.last_trade_poll.insert(leader_addr, now_ts);
    }
}

fn format_days_remaining(days: i64) -> String {
    if days <= 0 {
        "< 1d".into()
    } else {
        format!("{days}d")
    }
}
