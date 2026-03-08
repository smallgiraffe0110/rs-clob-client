use dashmap::DashMap;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;

#[derive(Debug, Clone)]
pub struct Position {
    pub token_id: U256,
    pub net_size: Decimal,
    pub avg_entry_price: Decimal,
    pub realized_pnl: Decimal,
    pub total_bought: Decimal,
    pub total_sold: Decimal,
}

impl Position {
    pub fn new(token_id: U256) -> Self {
        Self {
            token_id,
            net_size: Decimal::ZERO,
            avg_entry_price: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            total_bought: Decimal::ZERO,
            total_sold: Decimal::ZERO,
        }
    }

    pub fn record_fill(&mut self, side: Side, size: Decimal, price: Decimal) {
        match side {
            Side::Buy => {
                let new_cost = self.avg_entry_price * self.net_size + price * size;
                self.net_size += size;
                if self.net_size > Decimal::ZERO {
                    self.avg_entry_price = new_cost / self.net_size;
                }
                self.total_bought += size;
            }
            Side::Sell => {
                if self.net_size > Decimal::ZERO {
                    // Realize PnL on the portion we're selling
                    let sell_size = size.min(self.net_size);
                    self.realized_pnl += sell_size * (price - self.avg_entry_price);
                }
                self.net_size -= size;
                self.total_sold += size;
                // If position flipped short, reset avg entry
                if self.net_size < Decimal::ZERO {
                    self.avg_entry_price = price;
                }
            }
            _ => {}
        }
    }

    pub fn exposure(&self) -> Decimal {
        self.net_size.abs() * self.avg_entry_price
    }

    /// Unrealized PnL given a current market price.
    pub fn unrealized_pnl(&self, mark_price: Decimal) -> Decimal {
        if self.net_size.is_zero() {
            return Decimal::ZERO;
        }
        self.net_size * (mark_price - self.avg_entry_price)
    }
}

pub struct PositionTracker {
    positions: DashMap<U256, Position>,
    daily_realized_pnl: std::sync::atomic::AtomicI64,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            positions: DashMap::new(),
            daily_realized_pnl: std::sync::atomic::AtomicI64::new(0),
        }
    }

    pub fn record_fill(&self, token_id: U256, side: Side, size: Decimal, price: Decimal) {
        let mut entry = self
            .positions
            .entry(token_id)
            .or_insert_with(|| Position::new(token_id));
        let old_pnl = entry.realized_pnl;
        entry.record_fill(side, size, price);
        let pnl_delta = entry.realized_pnl - old_pnl;

        // Track daily PnL in microdollars for atomicity
        if let Some(micros) = (pnl_delta * dec!(1_000_000)).trunc().to_string().parse::<i64>().ok() {
            self.daily_realized_pnl
                .fetch_add(micros, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn get_position(&self, token_id: &U256) -> Option<Position> {
        self.positions.get(token_id).map(|p| p.clone())
    }

    pub fn net_size(&self, token_id: &U256) -> Decimal {
        self.positions
            .get(token_id)
            .map(|p| p.net_size)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn total_exposure(&self) -> Decimal {
        self.positions
            .iter()
            .map(|entry| entry.value().exposure())
            .sum()
    }

    pub fn all_positions(&self) -> Vec<Position> {
        self.positions.iter().map(|entry| entry.value().clone()).collect()
    }

    /// Sum of unrealized PnL across all positions using a price lookup.
    pub fn total_unrealized_pnl(&self, price_fn: impl Fn(&U256) -> Option<Decimal>) -> Decimal {
        self.positions
            .iter()
            .map(|entry| {
                let pos = entry.value();
                match price_fn(entry.key()) {
                    Some(price) => pos.unrealized_pnl(price),
                    None => Decimal::ZERO,
                }
            })
            .sum()
    }

    pub fn daily_pnl(&self) -> Decimal {
        let micros = self
            .daily_realized_pnl
            .load(std::sync::atomic::Ordering::Relaxed);
        Decimal::new(micros, 6)
    }
}
