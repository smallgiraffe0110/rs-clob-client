use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use polymarket_client_sdk::types::U256;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::info;

use crate::config::RiskConfig;
use crate::market_state::MarketState;
use crate::order_manager::OrderManager;
use crate::position::PositionTracker;

// ---------------------------------------------------------------------------
// Update messages broadcast from Engine -> WebSocket clients
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardUpdate {
    BookSnapshot {
        token_id: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        midpoint: Option<String>,
        spread: Option<String>,
    },
    Trade {
        token_id: String,
        side: String,
        size: String,
        price: String,
    },
    OrderEvent {
        order_id: String,
        token_id: String,
        side: String,
        price: String,
        event_type: String,
    },
    PositionUpdate {
        token_id: String,
        net_size: String,
        avg_entry_price: String,
        realized_pnl: String,
        unrealized_pnl: String,
    },
    TickSummary {
        daily_pnl: String,
        total_exposure: String,
    },
    LeaderUpdate {
        leaders: Vec<LeaderInfo>,
        tracked_tokens: Vec<TrackedToken>,
    },
    LeaderTrade {
        leader_address: String,
        leader_name: String,
        leader_score: String,
        token_title: String,
        side: String,
        size: String,
        price: String,
        timestamp: String,
    },
    CopyEvent {
        event_type: String,
        token_title: String,
        details: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderInfo {
    pub address: String,
    pub username: String,
    pub pnl: String,
    pub num_positions: usize,
    pub win_rate: String,
    pub score: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackedToken {
    pub token_id: String,
    pub title: String,
    pub target_size: String,
    pub our_size: String,
    pub leader_price: String,
    pub delta: String,
    pub days_remaining: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

// ---------------------------------------------------------------------------
// Full snapshot (sent on WebSocket connect + GET /api/snapshot)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct FullSnapshot {
    #[serde(rename = "type")]
    kind: String,
    dry_run: bool,
    daily_pnl: String,
    total_exposure: String,
    max_exposure: String,
    daily_loss_limit: String,
    active_tokens: Vec<String>,
    books: Vec<BookData>,
    positions: Vec<PositionData>,
    orders: Vec<OrderData>,
}

#[derive(Debug, Clone, Serialize)]
struct BookData {
    token_id: String,
    bids: Vec<PriceLevel>,
    asks: Vec<PriceLevel>,
    midpoint: Option<String>,
    spread: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PositionData {
    token_id: String,
    net_size: String,
    avg_entry_price: String,
    realized_pnl: String,
    total_bought: String,
    total_sold: String,
}

#[derive(Debug, Clone, Serialize)]
struct OrderData {
    order_id: String,
    token_id: String,
    side: String,
    price: String,
    size: String,
}

// ---------------------------------------------------------------------------
// Shared state for axum handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DashboardState {
    market_state: Arc<MarketState>,
    order_manager: Arc<OrderManager>,
    positions: Arc<PositionTracker>,
    dashboard_tx: broadcast::Sender<DashboardUpdate>,
    active_tokens: Vec<U256>,
    dry_run: bool,
    risk_config: RiskConfig,
}

impl DashboardState {
    pub fn new(
        market_state: Arc<MarketState>,
        order_manager: Arc<OrderManager>,
        positions: Arc<PositionTracker>,
        dashboard_tx: broadcast::Sender<DashboardUpdate>,
        active_tokens: Vec<U256>,
        dry_run: bool,
        risk_config: RiskConfig,
    ) -> Self {
        Self {
            market_state,
            order_manager,
            positions,
            dashboard_tx,
            active_tokens,
            dry_run,
            risk_config,
        }
    }

    fn build_snapshot(&self) -> FullSnapshot {
        let mut books = Vec::new();
        for token_id in &self.active_tokens {
            if let Some(book) = self.market_state.get_book(token_id) {
                books.push(BookData {
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
            }
        }

        let positions: Vec<PositionData> = self
            .positions
            .all_positions()
            .into_iter()
            .filter(|pos| !pos.net_size.is_zero())
            .map(|pos| PositionData {
                token_id: pos.token_id.to_string(),
                net_size: pos.net_size.to_string(),
                avg_entry_price: pos.avg_entry_price.to_string(),
                realized_pnl: pos.realized_pnl.to_string(),
                total_bought: pos.total_bought.to_string(),
                total_sold: pos.total_sold.to_string(),
            })
            .collect();

        let mut orders = Vec::new();
        for token_id in &self.active_tokens {
            for order in self.order_manager.live_orders_for_token(token_id) {
                orders.push(OrderData {
                    order_id: order.order_id,
                    token_id: order.token_id.to_string(),
                    side: format!("{}", order.side),
                    price: order.price.to_string(),
                    size: order.size.to_string(),
                });
            }
        }

        FullSnapshot {
            kind: "Snapshot".into(),
            dry_run: self.dry_run,
            daily_pnl: self.positions.daily_pnl().to_string(),
            total_exposure: self.positions.total_exposure().to_string(),
            max_exposure: self.risk_config.max_total_exposure_usd.to_string(),
            daily_loss_limit: self.risk_config.daily_loss_limit_usd.to_string(),
            active_tokens: self.active_tokens.iter().map(|t| t.to_string()).collect(),
            books,
            positions,
            orders,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn index_handler() -> Html<&'static str> {
    Html(crate::dashboard_html::HTML)
}

async fn snapshot_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    Json(state.build_snapshot())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: DashboardState) {
    // Send full snapshot on connect
    let snapshot = state.build_snapshot();
    let Ok(json) = serde_json::to_string(&snapshot) else {
        return;
    };
    if socket.send(Message::Text(json.into())).await.is_err() {
        return;
    }

    // Stream broadcast updates
    let mut rx = state.dashboard_tx.subscribe();

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(update) => {
                        let Ok(json) = serde_json::to_string(&update) else { continue };
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

pub async fn start(port: u16, state: DashboardState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/snapshot", get(snapshot_handler))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!("dashboard at http://localhost:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}
