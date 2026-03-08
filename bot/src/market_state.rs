use dashmap::DashMap;
use polymarket_client_sdk::clob::ws::types::response::{BookUpdate, OrderBookLevel};
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;

#[derive(Debug, Clone)]
pub struct LocalBook {
    pub asset_id: U256,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: i64,
}

impl LocalBook {
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first().map(|l| l.price)
    }

    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first().map(|l| l.price)
    }

    pub fn midpoint(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / dec!(2)),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        }
    }
}

pub struct MarketState {
    books: DashMap<U256, LocalBook>,
}

impl MarketState {
    pub fn new() -> Self {
        Self {
            books: DashMap::new(),
        }
    }

    pub fn update(&self, update: &BookUpdate) {
        self.books.insert(
            update.asset_id,
            LocalBook {
                asset_id: update.asset_id,
                bids: update.bids.clone(),
                asks: update.asks.clone(),
                timestamp: update.timestamp,
            },
        );
    }

    pub fn get_book(&self, asset_id: &U256) -> Option<LocalBook> {
        self.books.get(asset_id).map(|b| b.clone())
    }

    fn make_level(price: Decimal) -> OrderBookLevel {
        OrderBookLevel::builder().price(price).size(dec!(0)).build()
    }

    /// Update mark price for a token. Only writes synthetic books (timestamp == 0);
    /// will not overwrite a real order book.
    pub fn update_mark_price(&self, asset_id: U256, price: Decimal) {
        let level = Self::make_level(price);
        self.books
            .entry(asset_id)
            .and_modify(|book| {
                if book.timestamp == 0 {
                    book.bids = vec![level.clone()];
                    book.asks = vec![level.clone()];
                }
            })
            .or_insert_with(|| LocalBook {
                asset_id,
                bids: vec![level.clone()],
                asks: vec![level],
                timestamp: 0,
            });
    }
}
