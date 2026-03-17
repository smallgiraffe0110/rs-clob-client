pub mod market_maker;

use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{Decimal, U256};

use crate::market_state::LocalBook;
use crate::position::Position;

/// Actions a strategy can emit — the engine/order manager executes them.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants are part of the strategy API; not all are emitted yet
pub enum StrategyAction {
    PlaceOrder {
        token_id: U256,
        side: Side,
        price: Decimal,
        size: Decimal,
        taker: bool,
    },
    CancelOrder {
        order_id: String,
    },
    CancelAllForToken {
        token_id: U256,
    },
}

/// Pure decision interface. Implementations must not perform IO.
pub trait Strategy: Send + Sync {
    /// Called on every orderbook update for a subscribed token.
    fn on_book_update(
        &self,
        book: &LocalBook,
        position: Option<&Position>,
        live_order_ids: &[String],
    ) -> Vec<StrategyAction>;

    /// Called on each fill (trade executed for our account).
    fn on_fill(
        &self,
        token_id: &U256,
        side: Side,
        size: Decimal,
        price: Decimal,
        position: &Position,
    ) -> Vec<StrategyAction>;

    /// Called on periodic tick timer.
    fn on_tick(
        &self,
        book: &LocalBook,
        position: Option<&Position>,
        live_order_ids: &[String],
    ) -> Vec<StrategyAction>;
}
