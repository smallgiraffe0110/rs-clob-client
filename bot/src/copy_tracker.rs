use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::time::Instant;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::data::types::request::{PositionsRequest, TradesRequest};
use polymarket_client_sdk::types::{Address, B256, Decimal, U256};
use rust_decimal_macros::dec;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, error, info, trace, warn};

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
    /// Number of distinct leaders holding this token
    leader_count: usize,
    /// Condition ID (shared between both sides of a binary market)
    condition_id: B256,
}

pub struct CopyTracker {
    data_client: polymarket_client_sdk::data::Client,
    positions: Arc<PositionTracker>,
    market_state: Arc<MarketState>,
    config: CopyTraderConfig,
    engine_tx: mpsc::Sender<EngineEvent>,
    dashboard_tx: broadcast::Sender<DashboardUpdate>,
    /// token_id -> (target_size, last_known_price, condition_id) for exit detection
    tracked_tokens: DashMap<U256, (Decimal, Decimal, B256)>,
    /// Cooldown: token_id -> instant when we last exited (sold). Prevents churn re-entry.
    exit_cooldowns: DashMap<U256, Instant>,
    /// Sticky exits: token_id -> consecutive polls where leaders were absent
    absent_polls: DashMap<U256, u32>,
    /// Scored wallets from WalletScorer (replaces self-managed leader discovery)
    scored_wallets: Arc<RwLock<Vec<ScoredWallet>>>,
    /// Configured leader addresses (always followed regardless of scoring)
    configured_leaders: Vec<Address>,
    /// Last trade poll timestamp per leader address
    last_trade_poll: DashMap<Address, i64>,
    /// Tokens that triggered stop-loss — never re-enter these in this session
    stop_loss_blacklist: DashMap<U256, ()>,
    /// Condition-level cooldown: condition_id -> instant when we last exited ANY side.
    /// Prevents flip-flopping (selling Yes then immediately buying No of same market).
    condition_cooldowns: DashMap<B256, Instant>,
    /// Initial bankroll for dynamic position sizing (0 = use fixed max_target_size)
    initial_bankroll: Decimal,
    /// Max exposure as fraction of bankroll (from risk config)
    max_exposure_pct: Decimal,
    /// Poll counter for warmup mode
    poll_count: AtomicU32,
}

impl CopyTracker {
    pub fn new(
        positions: Arc<PositionTracker>,
        market_state: Arc<MarketState>,
        config: CopyTraderConfig,
        engine_tx: mpsc::Sender<EngineEvent>,
        dashboard_tx: broadcast::Sender<DashboardUpdate>,
        scored_wallets: Arc<RwLock<Vec<ScoredWallet>>>,
        initial_bankroll: Decimal,
        max_exposure_pct: Decimal,
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
            exit_cooldowns: DashMap::new(),
            absent_polls: DashMap::new(),
            scored_wallets,
            configured_leaders,
            last_trade_poll: DashMap::new(),
            stop_loss_blacklist: DashMap::new(),
            condition_cooldowns: DashMap::new(),
            initial_bankroll,
            max_exposure_pct,
            poll_count: AtomicU32::new(0),
        }
    }

    fn is_on_cooldown(&self, token_id: &U256, duration: tokio::time::Duration) -> bool {
        self.exit_cooldowns
            .get(token_id)
            .map(|t| t.elapsed() < duration)
            .unwrap_or(false)
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
                        let days_remaining = pos.end_date
                            .map(|d| (d - today).num_days())
                            .unwrap_or(0); // treat missing end_date as same-day

                        // Skip expired/resolved markets
                        if days_remaining < 0 {
                            continue;
                        }

                        // Skip markets too far from resolution
                        if days_remaining > self.config.max_days_to_resolution {
                            debug!(title = %pos.title, days = days_remaining, "skipping: too far from resolution");
                            continue;
                        }

                        // Skip same-day / imminent markets (coin flips with no edge)
                        if days_remaining < self.config.min_days_to_resolution {
                            debug!(title = %pos.title, days = days_remaining, "skipping: too close to resolution");
                            continue;
                        }

                        // Skip extreme longshots and near-certainties by price
                        if pos.cur_price < self.config.min_cur_price {
                            debug!(title = %pos.title, price = %pos.cur_price, "skipping: price below min_cur_price");
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
                                        leader_count: 0,
                                        condition_id: pos.condition_id,
                                    },
                                )
                            });
                        entry.0 += scaled * weight;
                        entry.1 += weight;
                        entry.2.leader_count += 1;
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

        // Deduplicate: for each condition (market), keep only the side with stronger
        // leader consensus. This prevents holding both YES and NO of the same market.
        {
            // Group token_ids by condition_id, tracking weighted_sum for each
            let mut condition_sides: HashMap<B256, Vec<(U256, Decimal)>> = HashMap::new();
            for (token_id, (weighted_sum, _total_weight, meta)) in &targets {
                condition_sides
                    .entry(meta.condition_id)
                    .or_default()
                    .push((*token_id, *weighted_sum));
            }
            // For conditions with multiple sides, remove the weaker one(s)
            for (_cond_id, sides) in &condition_sides {
                if sides.len() <= 1 {
                    continue;
                }
                // Find the side with the highest weighted consensus
                let best = sides
                    .iter()
                    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|s| s.0);
                for (token_id, _ws) in sides {
                    if Some(*token_id) != best {
                        let title = targets.get(token_id).map(|t| t.2.title.clone()).unwrap_or_default();
                        debug!(token = %token_id, title = %title, "dropping weaker side of market (stronger consensus on other outcome)");
                        targets.remove(token_id);
                    }
                }
            }
        }

        // Also prevent buying the opposite side of any market we already hold
        {
            let condition_ids_held: std::collections::HashSet<B256> = targets
                .iter()
                .filter(|(token_id, _)| self.positions.net_size(token_id) > Decimal::ZERO)
                .map(|(_, (_, _, meta))| meta.condition_id)
                .collect();

            let mut to_drop = Vec::new();
            for (token_id, (_, _, meta)) in &targets {
                // If we already hold a position under this condition_id and this isn't the one we hold
                if condition_ids_held.contains(&meta.condition_id)
                    && self.positions.net_size(token_id).is_zero()
                {
                    debug!(
                        token = %token_id,
                        title = %meta.title,
                        "skipping opposite side — already hold a position in this market"
                    );
                    to_drop.push(*token_id);
                }
            }
            for token_id in to_drop {
                targets.remove(&token_id);
            }
        }

        // Feed current prices into market state for mark-to-market PnL
        for (token_id, (_ws, _tw, meta)) in &targets {
            self.market_state.update_mark_price(*token_id, meta.cur_price);
        }

        // Compute deltas and build actions
        let mut actions = Vec::new();
        let mut tracked_token_infos = Vec::new();

        let cooldown_duration = tokio::time::Duration::from_secs(self.config.exit_cooldown_secs);

        // Stop loss check: scan positions before normal target processing
        // Collect (token_id, condition_id) pairs to set cooldowns on
        let mut pending_cooldowns: Vec<(U256, B256)> = Vec::new();
        for (token_id, (_weighted_sum, _total_weight, meta)) in &targets {
            if self.is_on_cooldown(token_id, cooldown_duration) {
                continue;
            }
            if let Some(pos) = self.positions.get_position(token_id)
                && pos.net_size > Decimal::ZERO && pos.avg_entry_price > Decimal::ZERO
            {
                let cur_price = meta.cur_price;
                let pnl_pct = (cur_price - pos.avg_entry_price) / pos.avg_entry_price;
                if pnl_pct < -self.config.stop_loss_pct {
                    let slippage = (cur_price * self.config.max_slippage_pct).round_dp(2).max(dec!(0.01));
                    let price = (cur_price - slippage)
                        .max(dec!(0.01))
                        .round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id: *token_id,
                        side: Side::Sell,
                        price,
                        size: pos.net_size,
                        taker: true,
                    });
                    pending_cooldowns.push((*token_id, meta.condition_id));
                    self.stop_loss_blacklist.insert(*token_id, ());
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "STOP_LOSS".into(),
                        token_title: meta.title.clone(),
                        details: format!(
                            "Sold {} @ {} (entry {} loss {:.1}%) [blacklisted]",
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
                        "stop loss triggered — blacklisted for session"
                    );
                }
            }
        }

        // Take-profit check: sell when price reaches near-certainty
        for (token_id, (_ws, _tw, meta)) in &targets {
            if self.is_on_cooldown(token_id, cooldown_duration) || pending_cooldowns.iter().any(|(t, _)| t == token_id) {
                continue;
            }
            if let Some(pos) = self.positions.get_position(token_id)
                && pos.net_size > Decimal::ZERO
            {
                let cur_price = meta.cur_price;
                let is_near_expiry = meta.days_remaining <= self.config.near_expiry_days;
                if cur_price >= self.config.take_profit_price && is_near_expiry {
                    trace!(token = %token_id, cur_price = %cur_price, days = meta.days_remaining,
                           title = %meta.title, "suppressing take-profit for near-expiry (ride to resolution)");
                }
                if cur_price >= self.config.take_profit_price && !is_near_expiry {
                    let slippage = (cur_price * self.config.max_slippage_pct).round_dp(2).max(dec!(0.01));
                    let price = (cur_price - slippage).max(dec!(0.01)).round_dp(2);
                    let profit_pct = if pos.avg_entry_price > Decimal::ZERO {
                        ((cur_price - pos.avg_entry_price) / pos.avg_entry_price * dec!(100))
                            .round_dp(1)
                    } else {
                        Decimal::ZERO
                    };
                    actions.push(StrategyAction::PlaceOrder {
                        token_id: *token_id,
                        side: Side::Sell,
                        price,
                        size: pos.net_size,
                        taker: true,
                    });
                    pending_cooldowns.push((*token_id, meta.condition_id)); // reuse stop-out tracking to prevent re-entry
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "TAKE_PROFIT".into(),
                        token_title: meta.title.clone(),
                        details: format!(
                            "Sold {} @ {} (entry {} profit +{:.1}%)",
                            pos.net_size, price, pos.avg_entry_price, profit_pct
                        ),
                    });
                    info!(
                        token = %token_id,
                        cur_price = %cur_price,
                        avg_entry = %pos.avg_entry_price,
                        profit_pct = %profit_pct,
                        title = %meta.title,
                        "take profit triggered"
                    );
                }
            }
        }

        // Orphaned position protection: check stop-loss and take-profit for positions
        // the bot holds but leaders have already exited. These are NOT in `targets`,
        // so the loops above never see them. Without this, orphaned positions have no
        // stop-loss protection until the sticky-exit mechanism fires (3+ polls).
        {
            let target_token_ids: std::collections::HashSet<U256> =
                targets.keys().copied().collect();

            for pos in self.positions.all_positions() {
                let token_id = pos.token_id;

                // Skip if already covered by the targets-based loops above
                if target_token_ids.contains(&token_id) {
                    continue;
                }

                // Skip if no meaningful position
                if pos.net_size <= Decimal::ZERO || pos.avg_entry_price <= Decimal::ZERO {
                    continue;
                }

                // Skip if already on cooldown or pending exit this cycle
                if self.is_on_cooldown(&token_id, cooldown_duration)
                    || pending_cooldowns.iter().any(|(t, _)| t == &token_id)
                {
                    continue;
                }

                // Get the current mark price from MarketState (if available)
                let cur_price = match self.market_state.get_book(&token_id)
                    .and_then(|book| book.midpoint())
                {
                    Some(p) if p > Decimal::ZERO => p,
                    _ => continue, // No price data — can't evaluate, skip
                };

                // Look up condition_id from tracked_tokens (set when we first entered)
                let condition_id = self.tracked_tokens
                    .get(&token_id)
                    .map(|entry| entry.value().2)
                    .unwrap_or_default();

                let pnl_pct = (cur_price - pos.avg_entry_price) / pos.avg_entry_price;

                // Stop-loss check
                if pnl_pct < -self.config.stop_loss_pct {
                    let slippage = (cur_price * self.config.max_slippage_pct)
                        .round_dp(2)
                        .max(dec!(0.01));
                    let price = (cur_price - slippage).max(dec!(0.01)).round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id,
                        side: Side::Sell,
                        price,
                        size: pos.net_size,
                        taker: true,
                    });
                    pending_cooldowns.push((token_id, condition_id));
                    self.stop_loss_blacklist.insert(token_id, ());
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "STOP_LOSS".into(),
                        token_title: format!("orphaned:{:.8}", token_id),
                        details: format!(
                            "Sold {} @ {} (entry {} loss {:.1}%) [orphaned+blacklisted]",
                            pos.net_size, price, pos.avg_entry_price,
                            pnl_pct * dec!(100)
                        ),
                    });
                    info!(
                        token = %token_id,
                        cur_price = %cur_price,
                        avg_entry = %pos.avg_entry_price,
                        loss_pct = %pnl_pct,
                        "orphaned stop loss triggered — leaders already exited, blacklisted"
                    );
                    continue; // Don't also check take-profit
                }

                // Take-profit check (no near-expiry logic since we don't have days_remaining
                // for orphaned positions — just exit at the take-profit price)
                if cur_price >= self.config.take_profit_price {
                    let slippage = (cur_price * self.config.max_slippage_pct)
                        .round_dp(2)
                        .max(dec!(0.01));
                    let price = (cur_price - slippage).max(dec!(0.01)).round_dp(2);
                    let profit_pct = ((cur_price - pos.avg_entry_price)
                        / pos.avg_entry_price
                        * dec!(100))
                    .round_dp(1);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id,
                        side: Side::Sell,
                        price,
                        size: pos.net_size,
                        taker: true,
                    });
                    pending_cooldowns.push((token_id, condition_id));
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "TAKE_PROFIT".into(),
                        token_title: format!("orphaned:{:.8}", token_id),
                        details: format!(
                            "Sold {} @ {} (entry {} profit +{:.1}%) [orphaned]",
                            pos.net_size, price, pos.avg_entry_price, profit_pct
                        ),
                    });
                    info!(
                        token = %token_id,
                        cur_price = %cur_price,
                        avg_entry = %pos.avg_entry_price,
                        profit_pct = %profit_pct,
                        "orphaned take profit triggered — leaders already exited"
                    );
                }
            }
        }

        // Dust cleanup: sell positions worth less than min_position_usd to free capital.
        // These are too small to matter but eat exposure budget.
        let min_value = self.config.min_position_usd;
        for (token_id, (_ws, _tw, meta)) in &targets {
            if self.is_on_cooldown(token_id, cooldown_duration)
                || pending_cooldowns.iter().any(|(t, _)| t == token_id)
            {
                continue;
            }
            if let Some(pos) = self.positions.get_position(token_id)
                && pos.net_size > Decimal::ZERO
            {
                let position_value = pos.net_size * meta.cur_price;
                if position_value < min_value && position_value > dec!(0.50) {
                    let slippage = (meta.cur_price * self.config.max_slippage_pct)
                        .round_dp(2)
                        .max(dec!(0.01));
                    let price = (meta.cur_price - slippage).max(dec!(0.01)).round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id: *token_id,
                        side: Side::Sell,
                        price,
                        size: pos.net_size,
                        taker: true,
                    });
                    pending_cooldowns.push((*token_id, meta.condition_id));
                    info!(
                        token = %token_id,
                        value = %position_value.round_dp(2),
                        min = %min_value,
                        title = %meta.title,
                        "dust cleanup — selling small position to free capital"
                    );
                }
            }
        }

        // Compute effective max target size (scales with bankroll if configured)
        let effective_max_target = if self.initial_bankroll > Decimal::ZERO {
            let bankroll = (self.initial_bankroll + self.positions.daily_pnl())
                .max(Decimal::ZERO);
            bankroll * self.config.max_target_size_pct
        } else {
            self.config.max_target_size
        };

        // Pre-compute remaining exposure budget to avoid spamming BUY orders
        // that the risk manager will just veto.
        let current_exposure = self.positions.total_exposure();
        let max_exposure = if self.initial_bankroll > Decimal::ZERO {
            let bankroll = (self.initial_bankroll + self.positions.daily_pnl())
                .max(Decimal::ZERO);
            bankroll * self.max_exposure_pct
        } else {
            // Fallback: use a large number so this check doesn't block
            Decimal::MAX
        };
        let mut remaining_budget = max_exposure - current_exposure;
        let mut budget_exhausted_logged = false;

        for (token_id, (weighted_sum, total_weight, meta)) in &targets {
            // Skip tokens on cooldown (recently exited) or pending exit this cycle
            if self.is_on_cooldown(token_id, cooldown_duration) || pending_cooldowns.iter().any(|(t, _)| t == token_id) {
                continue;
            }

            // Skip if ANY side of this market (condition) was recently exited — prevents
            // flip-flopping (e.g. selling Yes then immediately buying No of same market)
            if self.condition_cooldowns.get(&meta.condition_id)
                .map(|t| t.elapsed() < cooldown_duration)
                .unwrap_or(false)
            {
                debug!(
                    token = %token_id,
                    title = %meta.title,
                    "skipping: condition-level cooldown (recently exited other side)"
                );
                continue;
            }

            // Skip tokens that were stop-lossed this session (prevents re-entry churn)
            if self.stop_loss_blacklist.contains_key(token_id) {
                continue;
            }

            let mut target_avg = if *total_weight > Decimal::ZERO {
                *weighted_sum / *total_weight
            } else {
                Decimal::ZERO
            };

            // Cap target to fit within risk limits (dynamic or fixed)
            if target_avg > effective_max_target {
                target_avg = effective_max_target;
            }

            let our_size = self.positions.net_size(token_id);

            // Consensus filter: require minimum leader agreement for NEW entries
            // (always allow exits/reductions for positions we already hold)
            if our_size.is_zero() && (meta.leader_count as i64) < self.config.min_leaders_for_entry {
                debug!(
                    title = %meta.title,
                    leaders = meta.leader_count,
                    min = self.config.min_leaders_for_entry,
                    "skipping: insufficient leader consensus"
                );
                continue;
            }

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
                    leader_count: meta.leader_count,
                });
                continue;
            }

            if delta > Decimal::ZERO {
                // Budget guard: skip new entries when exposure cap is nearly reached
                let order_exposure = delta * cur_price;
                if our_size.is_zero() && order_exposure > remaining_budget {
                    if !budget_exhausted_logged {
                        info!(
                            exposure = %current_exposure.round_dp(2),
                            limit = %max_exposure.round_dp(2),
                            remaining = %remaining_budget.round_dp(2),
                            "exposure budget exhausted — skipping new entries this cycle"
                        );
                        budget_exhausted_logged = true;
                    }
                    continue;
                }

                // Compute weighted average leader entry price
                let leader_entry = if meta.leader_entry_weight > Decimal::ZERO {
                    meta.leader_avg_entry / meta.leader_entry_weight
                } else {
                    cur_price
                };
                let is_near_expiry = meta.days_remaining <= self.config.near_expiry_days;
                let effective_max_price = if is_near_expiry {
                    self.config.near_expiry_max_entry_price
                } else {
                    self.config.max_entry_price
                };
                let effective_drift = if is_near_expiry {
                    self.config.near_expiry_max_entry_drift
                } else {
                    self.config.max_entry_drift
                };
                let max_allowed = (leader_entry + effective_drift).min(effective_max_price);

                // Price guard: skip if current price drifted too far above leader's entry
                if cur_price > max_allowed {
                    let _ = self.dashboard_tx.send(DashboardUpdate::CopyEvent {
                        event_type: "PRICE_GUARD".into(),
                        token_title: meta.title.clone(),
                        details: format!(
                            "Skipped buy @ {} (leader entry {} + drift {} = max {}){}",
                            cur_price, leader_entry.round_dp(2),
                            effective_drift, max_allowed.round_dp(2),
                            if is_near_expiry { " [near-expiry]" } else { "" }
                        ),
                    });
                    info!(
                        token = %token_id,
                        cur_price = %cur_price,
                        leader_entry = %leader_entry.round_dp(2),
                        max_allowed = %max_allowed.round_dp(2),
                        is_near_expiry = is_near_expiry,
                        title = %meta.title,
                        "skipping buy: price too far above leader entry"
                    );
                } else {
                    // Need to buy
                    let slippage = (cur_price * self.config.max_slippage_pct).round_dp(2).max(dec!(0.01));
                    let price = (cur_price + slippage)
                        .min(dec!(0.99))
                        .round_dp(2);
                    actions.push(StrategyAction::PlaceOrder {
                        token_id: *token_id,
                        side: Side::Buy,
                        price,
                        size: delta.round_dp(2),
                        taker: true,
                    });
                    // Deduct from remaining budget so batch-level tracking is accurate
                    remaining_budget -= order_exposure;
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
                let slippage = (cur_price * self.config.max_slippage_pct).round_dp(2).max(dec!(0.01));
                let price = (cur_price - slippage)
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
            self.tracked_tokens.insert(*token_id, (target_avg, cur_price, meta.condition_id));

            tracked_token_infos.push(TrackedToken {
                token_id: token_id.to_string(),
                title: meta.title.clone(),
                target_size: target_avg.round_dp(2).to_string(),
                our_size: our_size.round_dp(2).to_string(),
                leader_price: cur_price.to_string(),
                delta: delta.round_dp(2).to_string(),
                days_remaining: format_days_remaining(meta.days_remaining),
                leader_count: meta.leader_count,
            });
        }

        // Sticky exit detection: only close positions after leaders are absent
        // for N consecutive polls (prevents churn from flickering leader data)
        let target_keys: std::collections::HashSet<U256> =
            targets.keys().copied().collect();
        let mut to_remove = Vec::new();

        for entry in self.tracked_tokens.iter() {
            let token_id = *entry.key();
            if target_keys.contains(&token_id) {
                // Leaders still present — reset absent counter
                self.absent_polls.remove(&token_id);
            } else {
                // Leaders absent this poll — increment counter
                let mut absent = self.absent_polls.entry(token_id).or_insert(0);
                *absent += 1;
                let count = *absent;

                if count >= self.config.exit_absent_polls {
                    let our_size = self.positions.net_size(&token_id);
                    if our_size > Decimal::ZERO {
                        let last_price = entry.value().1;
                        let slippage = (last_price * self.config.max_slippage_pct).round_dp(2).max(dec!(0.01));
                        let price = (last_price - slippage)
                            .max(dec!(0.01))
                            .round_dp(2);
                        actions.push(StrategyAction::PlaceOrder {
                            token_id,
                            side: Side::Sell,
                            price,
                            size: our_size,
                            taker: true,
                        });
                        let cond_id = entry.value().2;
                        pending_cooldowns.push((token_id, cond_id));
                        info!(
                            token = %token_id,
                            size = %our_size,
                            price = %price,
                            absent_polls = count,
                            "exit detection: leaders abandoned position"
                        );
                    }
                    to_remove.push(token_id);
                } else {
                    debug!(
                        token = %token_id,
                        absent_polls = count,
                        required = self.config.exit_absent_polls,
                        "leaders absent, waiting before exit"
                    );
                }
            }
        }
        for token_id in to_remove {
            self.tracked_tokens.remove(&token_id);
            self.absent_polls.remove(&token_id);
        }

        // Set cooldowns BEFORE sending to engine — prevents re-entry even if send fails
        if !pending_cooldowns.is_empty() {
            let now = Instant::now();
            for (token_id, cond_id) in &pending_cooldowns {
                self.exit_cooldowns.insert(*token_id, now);
                // Also set condition-level cooldown — prevents buying the OTHER side
                self.condition_cooldowns.insert(*cond_id, now);
            }
        }

        // Warmup mode: log proposed trades but don't execute
        let poll_num = self.poll_count.fetch_add(1, Ordering::Relaxed) + 1;
        let warmup = self.config.warmup_polls;
        let in_warmup = warmup > 0 && poll_num <= warmup;

        if !actions.is_empty() && in_warmup {
            info!(
                poll = poll_num,
                warmup_remaining = warmup - poll_num,
                count = actions.len(),
                "WARMUP: observing (not executing) proposed trades"
            );
            for action in &actions {
                if let StrategyAction::PlaceOrder { token_id, side, price, size, .. } = action {
                    let side_str = match side {
                        Side::Buy => "BUY",
                        Side::Sell => "SELL",
                        _ => "?",
                    };
                    info!(
                        warmup = true,
                        side = side_str,
                        price = %price,
                        size = %size,
                        token = %token_id,
                        "would place order"
                    );
                }
            }
        } else if !actions.is_empty() {
            if poll_num == warmup + 1 && warmup > 0 {
                info!("warmup complete — now executing trades live");
            }
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
                // Don't advance timestamp on failure — retry next poll to avoid dropping trades
                return;
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
