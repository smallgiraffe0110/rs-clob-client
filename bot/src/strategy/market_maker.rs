use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;

use crate::config::MarketMakerConfig;
use crate::market_state::LocalBook;
use crate::position::Position;
use crate::strategy::{Strategy, StrategyAction};

pub struct MarketMakerStrategy {
    config: MarketMakerConfig,
    /// Last midpoint we quoted around, per token.
    last_mid: dashmap::DashMap<U256, Decimal>,
}

impl MarketMakerStrategy {
    pub fn new(config: MarketMakerConfig) -> Self {
        Self {
            config,
            last_mid: dashmap::DashMap::new(),
        }
    }

    fn compute_quotes(
        &self,
        book: &LocalBook,
        position: Option<&Position>,
    ) -> Vec<StrategyAction> {
        let Some(mid) = book.midpoint() else {
            return vec![];
        };

        let spread = book.spread().unwrap_or(Decimal::ZERO);
        if spread < self.config.min_edge {
            return vec![];
        }

        let inventory = position.map(|p| p.net_size).unwrap_or(Decimal::ZERO);
        let skew = inventory * self.config.inventory_skew_factor;

        let mut actions = Vec::new();

        for i in 0..self.config.num_levels {
            let level_offset =
                Decimal::from(i as u32) * self.config.level_spacing;

            // Bid: lower if long (skew > 0), higher if short
            let bid_price = mid - self.config.half_spread - level_offset - skew;
            // Ask: higher if short (skew < 0), lower if long
            let ask_price = mid + self.config.half_spread + level_offset - skew;

            // Clamp to valid range [0.01, 0.99]
            let bid_price = bid_price.max(dec!(0.01));
            let ask_price = ask_price.min(dec!(0.99));

            // Round to 2 decimal places (hundredths tick size)
            let bid_price = bid_price.round_dp(2);
            let ask_price = ask_price.round_dp(2);

            if bid_price < ask_price {
                actions.push(StrategyAction::PlaceOrder {
                    token_id: book.asset_id,
                    side: Side::Buy,
                    price: bid_price,
                    size: self.config.order_size,
                    taker: false,
                });
                actions.push(StrategyAction::PlaceOrder {
                    token_id: book.asset_id,
                    side: Side::Sell,
                    price: ask_price,
                    size: self.config.order_size,
                    taker: false,
                });
            }
        }

        actions
    }

    fn should_requote(&self, book: &LocalBook) -> bool {
        let Some(mid) = book.midpoint() else {
            return false;
        };
        match self.last_mid.get(&book.asset_id) {
            Some(prev) => (mid - *prev).abs() >= self.config.requote_threshold,
            None => true, // First quote
        }
    }

    fn record_mid(&self, book: &LocalBook) {
        if let Some(mid) = book.midpoint() {
            self.last_mid.insert(book.asset_id, mid);
        }
    }
}

impl Strategy for MarketMakerStrategy {
    fn on_book_update(
        &self,
        book: &LocalBook,
        position: Option<&Position>,
        live_order_ids: &[String],
    ) -> Vec<StrategyAction> {
        if !self.should_requote(book) {
            return vec![];
        }

        let mut actions = Vec::new();

        // Cancel existing orders before re-quoting
        if !live_order_ids.is_empty() {
            actions.push(StrategyAction::CancelAllForToken {
                token_id: book.asset_id,
            });
        }

        actions.extend(self.compute_quotes(book, position));
        self.record_mid(book);

        actions
    }

    fn on_fill(
        &self,
        token_id: &U256,
        _side: Side,
        _size: Decimal,
        _price: Decimal,
        _position: &Position,
    ) -> Vec<StrategyAction> {
        // After a fill, invalidate last mid so next book update triggers a requote
        self.last_mid.remove(token_id);
        vec![]
    }

    fn on_tick(
        &self,
        book: &LocalBook,
        position: Option<&Position>,
        live_order_ids: &[String],
    ) -> Vec<StrategyAction> {
        // On tick, only re-quote if we have no live orders (they may have been
        // canceled server-side or expired)
        if live_order_ids.is_empty() {
            let actions = self.compute_quotes(book, position);
            if !actions.is_empty() {
                self.record_mid(book);
            }
            return actions;
        }
        vec![]
    }
}
