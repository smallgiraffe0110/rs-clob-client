use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::clob::ws::types::response::{
    BookUpdate, OrderMessage, OrderMessageType, TradeMessage,
};
use polymarket_client_sdk::types::U256;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::dashboard::{DashboardUpdate, PriceLevel};
use crate::market_state::MarketState;
use crate::order_manager::OrderManager;
use crate::position::PositionTracker;
use crate::risk::RiskManager;
use crate::strategy::{Strategy, StrategyAction};

#[derive(Debug)]
pub enum EngineEvent {
    BookUpdate(BookUpdate),
    TradeConfirmed(TradeMessage),
    OrderUpdate(OrderMessage),
    Tick,
    CopyActions(Vec<crate::strategy::StrategyAction>),
    Shutdown,
}

pub struct Engine {
    market_state: Arc<MarketState>,
    order_manager: Arc<OrderManager>,
    positions: Arc<PositionTracker>,
    risk: RiskManager,
    strategy: Box<dyn Strategy>,
    rx: mpsc::Receiver<EngineEvent>,
    /// Token IDs we're actively quoting
    active_tokens: Vec<U256>,
    dashboard_tx: broadcast::Sender<DashboardUpdate>,
    dry_run: bool,
    /// Tracks trade IDs we've already processed to deduplicate WS replays.
    seen_trade_ids: HashSet<String>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        market_state: Arc<MarketState>,
        order_manager: Arc<OrderManager>,
        positions: Arc<PositionTracker>,
        risk: RiskManager,
        strategy: Box<dyn Strategy>,
        rx: mpsc::Receiver<EngineEvent>,
        active_tokens: Vec<U256>,
        dashboard_tx: broadcast::Sender<DashboardUpdate>,
        dry_run: bool,
    ) -> Self {
        Self {
            market_state,
            order_manager,
            positions,
            risk,
            strategy,
            rx,
            active_tokens,
            dashboard_tx,
            dry_run,
            seen_trade_ids: HashSet::new(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        info!(tokens = self.active_tokens.len(), "engine started");

        while let Some(event) = self.rx.recv().await {
            match event {
                EngineEvent::BookUpdate(update) => {
                    self.handle_book_update(update).await;
                }
                EngineEvent::TradeConfirmed(trade) => {
                    // The WS user stream replays the same fill multiple
                    // times (matched → mined → confirmed). Only process
                    // each trade_id once to avoid inflating positions.
                    if !self.seen_trade_ids.insert(trade.id.clone()) {
                        debug!(
                            trade_id = %trade.id,
                            status = ?trade.status,
                            "duplicate fill ignored"
                        );
                        continue;
                    }

                    // Prevent unbounded memory growth: when the set gets
                    // large, clear it. Old IDs won't reappear.
                    if self.seen_trade_ids.len() > 1000 {
                        self.seen_trade_ids.clear();
                    }

                    self.handle_trade(trade).await;
                }
                EngineEvent::OrderUpdate(order) => {
                    self.handle_order_update(order);
                }
                EngineEvent::Tick => {
                    self.handle_tick().await;
                }
                EngineEvent::CopyActions(actions) => {
                    self.execute_with_risk(actions).await;
                }
                EngineEvent::Shutdown => {
                    info!("engine shutting down");
                    break;
                }
            }
        }

        info!("engine stopped");
        Ok(())
    }

    async fn handle_book_update(&self, update: BookUpdate) {
        let token_id = update.asset_id;
        self.market_state.update(&update);

        let book = match self.market_state.get_book(&token_id) {
            Some(b) => b,
            None => return,
        };

        let _ = self.dashboard_tx.send(DashboardUpdate::BookSnapshot {
            token_id: token_id.to_string(),
            bids: book
                .bids
                .iter()
                .map(|l| PriceLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            asks: book
                .asks
                .iter()
                .map(|l| PriceLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            midpoint: book.midpoint().map(|m| m.to_string()),
            spread: book.spread().map(|s| s.to_string()),
        });

        let position = self.positions.get_position(&token_id);
        let live_ids = self.order_manager.live_order_ids_for_token(&token_id);

        let actions =
            self.strategy
                .on_book_update(&book, position.as_ref(), &live_ids);

        self.execute_with_risk(actions).await;
    }

    async fn handle_trade(&self, trade: TradeMessage) {
        let token_id = trade.asset_id;
        let side = trade.side;
        let size = trade.size;
        let price = trade.price;

        let side_str = match side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
            _ => "UNKNOWN",
        };

        // Only process fills for tokens the bot manages: either already in
        // positions (from disk or prior fills) or with a live order in the
        // order manager.  This prevents manual/external trades on the same
        // proxy wallet from contaminating position tracking.
        let known = self.positions.has_position(&token_id)
            || !self.order_manager.live_order_ids_for_token(&token_id).is_empty();
        if !known {
            debug!(
                token_id = %token_id,
                side = side_str,
                size = %size,
                price = %price,
                "ignoring external fill (not a bot-managed token)"
            );
            return;
        }

        info!(
            token_id = %token_id,
            side = side_str,
            size = %size,
            price = %price,
            "fill received"
        );

        let _ = self.dashboard_tx.send(DashboardUpdate::Trade {
            token_id: token_id.to_string(),
            side: side_str.to_string(),
            size: size.to_string(),
            price: price.to_string(),
        });

        self.positions.record_fill(token_id, side, size, price);

        if let Some(pos) = self.positions.get_position(&token_id) {
            info!(
                token_id = %token_id,
                net_size = %pos.net_size,
                avg_entry = %pos.avg_entry_price,
                realized_pnl = %pos.realized_pnl,
                "position updated"
            );

            let mark = self.market_state.get_book(&token_id).and_then(|b| b.midpoint()).unwrap_or(price);
            let _ = self.dashboard_tx.send(DashboardUpdate::PositionUpdate {
                token_id: token_id.to_string(),
                net_size: pos.net_size.to_string(),
                avg_entry_price: pos.avg_entry_price.to_string(),
                realized_pnl: pos.realized_pnl.to_string(),
                unrealized_pnl: pos.unrealized_pnl(mark).to_string(),
            });

            let actions = self.strategy.on_fill(&token_id, side, size, price, &pos);
            self.execute_with_risk(actions).await;
        }
    }

    fn handle_order_update(&self, order: OrderMessage) {
        // Track cancellations and removals
        if let Some(ref msg_type) = order.msg_type {
            info!(
                order_id = %order.id,
                msg_type = ?msg_type,
                side = ?order.side,
                price = %order.price,
                "order update"
            );

            let _ = self.dashboard_tx.send(DashboardUpdate::OrderEvent {
                order_id: order.id.clone(),
                token_id: order.asset_id.to_string(),
                side: format!("{}", order.side),
                price: order.price.to_string(),
                event_type: match msg_type {
                    OrderMessageType::Placement => "PLACED".to_string(),
                    OrderMessageType::Update => "UPDATED".to_string(),
                    OrderMessageType::Cancellation => "CANCELED".to_string(),
                    _ => "UNKNOWN".to_string(),
                },
            });
        }

        // If an order was canceled externally, remove from our tracking
        if matches!(order.msg_type, Some(OrderMessageType::Cancellation)) {
            self.order_manager.remove_order_by_id(&order.id);
        }
    }

    async fn handle_tick(&self) {
        for token_id in &self.active_tokens {
            let book = match self.market_state.get_book(token_id) {
                Some(b) => b,
                None => continue,
            };

            let position = self.positions.get_position(token_id);
            let live_ids = self.order_manager.live_order_ids_for_token(token_id);

            let actions =
                self.strategy
                    .on_tick(&book, position.as_ref(), &live_ids);

            self.execute_with_risk(actions).await;
        }

        // Periodic PnL log (realized + unrealized)
        let realized = self.positions.daily_pnl();
        let unrealized = self.positions.total_unrealized_pnl(|token_id| {
            self.market_state.get_book(token_id).and_then(|b| b.midpoint())
        });
        let daily_pnl = realized + unrealized;
        let exposure = self.positions.total_exposure();
        info!(daily_pnl = %daily_pnl, realized = %realized, unrealized = %unrealized, exposure = %exposure, "tick summary");

        let _ = self.dashboard_tx.send(DashboardUpdate::TickSummary {
            total_pnl: daily_pnl.to_string(),
            total_exposure: exposure.to_string(),
        });

        // Broadcast mark-to-market position updates so the UI stays current
        for pos in self.positions.all_positions() {
            if pos.net_size.is_zero() {
                continue;
            }
            let mark = self.market_state.get_book(&pos.token_id).and_then(|b| b.midpoint()).unwrap_or(pos.avg_entry_price);
            let _ = self.dashboard_tx.send(DashboardUpdate::PositionUpdate {
                token_id: pos.token_id.to_string(),
                net_size: pos.net_size.to_string(),
                avg_entry_price: pos.avg_entry_price.to_string(),
                realized_pnl: pos.realized_pnl.to_string(),
                unrealized_pnl: pos.unrealized_pnl(mark).to_string(),
            });
        }
    }

    async fn execute_with_risk(&self, actions: Vec<StrategyAction>) {
        if self.dry_run {
            // In dry-run mode, check risk and simulate fills one-by-one
            // so each fill updates positions before the next risk check.
            for action in actions {
                if let StrategyAction::PlaceOrder {
                    token_id,
                    side,
                    price,
                    size,
                    ..
                } = &action
                {
                    let exposure = *price * *size;
                    if let Some(veto) = self.risk.check_order(
                        token_id,
                        *side,
                        *size,
                        exposure,
                        &self.positions,
                    ) {
                        warn!(veto = %veto, "risk check failed, skipping order");
                        continue;
                    }

                    let side_str = match side {
                        Side::Buy => "BUY",
                        Side::Sell => "SELL",
                        _ => "UNKNOWN",
                    };

                    // Simulate the fill at the current market price rather than the
                    // limit price (which includes slippage). This gives realistic
                    // paper PnL — in live trading, taker orders fill at the book's
                    // price, not our padded limit.
                    let fill_price = self
                        .market_state
                        .get_book(token_id)
                        .and_then(|b| b.midpoint())
                        .unwrap_or(*price);

                    info!(
                        dry_run = true,
                        token_id = %token_id,
                        side = side_str,
                        limit_price = %price,
                        fill_price = %fill_price,
                        size = %size,
                        "simulated fill"
                    );
                    self.positions.record_fill(*token_id, *side, *size, fill_price);

                    let _ = self.dashboard_tx.send(DashboardUpdate::Trade {
                        token_id: token_id.to_string(),
                        side: side_str.to_string(),
                        size: size.to_string(),
                        price: fill_price.to_string(),
                    });

                    if let Some(pos) = self.positions.get_position(token_id) {
                        let mark = self.market_state.get_book(token_id).and_then(|b| b.midpoint()).unwrap_or(fill_price);
                        let _ =
                            self.dashboard_tx
                                .send(DashboardUpdate::PositionUpdate {
                                    token_id: token_id.to_string(),
                                    net_size: pos.net_size.to_string(),
                                    avg_entry_price: pos.avg_entry_price.to_string(),
                                    realized_pnl: pos.realized_pnl.to_string(),
                                    unrealized_pnl: pos.unrealized_pnl(mark).to_string(),
                                });
                    }
                }
            }
            return;
        }

        // Live mode: risk check each order, tracking pending exposure from
        // orders already approved in this batch (fills are async, so the
        // position tracker won't reflect them until later).
        let mut approved = Vec::new();
        let mut pending_exposure = rust_decimal::Decimal::ZERO;

        for action in actions {
            match &action {
                StrategyAction::PlaceOrder {
                    token_id,
                    side,
                    price,
                    size,
                    taker: _,
                } => {
                    let exposure = *price * *size;
                    if let Some(veto) = self.risk.check_order_with_pending(
                        token_id,
                        *side,
                        *size,
                        exposure,
                        &self.positions,
                        pending_exposure,
                    ) {
                        warn!(veto = %veto, "risk check failed, skipping order");
                        continue;
                    }
                    // Track this order's exposure so subsequent checks in
                    // the same batch see the cumulative total.
                    if matches!(side, Side::Buy) {
                        pending_exposure += exposure;
                    }
                    approved.push(action);
                }
                // Cancels always pass risk
                _ => approved.push(action),
            }
        }

        if !approved.is_empty()
            && let Err(e) = self.order_manager.execute(approved).await
        {
            error!(error = %e, "order execution error");
        }
    }
}
