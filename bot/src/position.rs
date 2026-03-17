use dashmap::DashMap;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::types::{Address, Decimal, U256};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
                self.total_bought += size;
                if self.net_size >= Decimal::ZERO {
                    // Buy increasing long: weighted average entry
                    let new_cost = self.avg_entry_price * self.net_size + price * size;
                    self.net_size += size;
                    if self.net_size.is_zero() {
                        self.avg_entry_price = Decimal::ZERO;
                    } else {
                        self.avg_entry_price = new_cost / self.net_size;
                    }
                } else {
                    // Buy reducing/flipping short
                    let short_abs = self.net_size.abs();
                    let cover_qty = size.min(short_abs);
                    // Realize PnL on covered portion (short profits when price drops)
                    self.realized_pnl += cover_qty * (self.avg_entry_price - price);
                    self.net_size += size;
                    if self.net_size > Decimal::ZERO {
                        // Flipped to long — remainder starts a new position
                        self.avg_entry_price = price;
                    } else if self.net_size.is_zero() {
                        self.avg_entry_price = Decimal::ZERO;
                    }
                    // If still short, avg_entry_price stays the same
                }
            }
            Side::Sell => {
                self.total_sold += size;
                if self.net_size <= Decimal::ZERO {
                    // Sell increasing short: weighted average entry using abs size
                    let abs_size = self.net_size.abs();
                    let new_cost = self.avg_entry_price * abs_size + price * size;
                    self.net_size -= size;
                    let new_abs = self.net_size.abs();
                    if new_abs.is_zero() {
                        self.avg_entry_price = Decimal::ZERO;
                    } else {
                        self.avg_entry_price = new_cost / new_abs;
                    }
                } else {
                    // Sell reducing/flipping long
                    let sell_qty = size.min(self.net_size);
                    // Realize PnL on sold portion
                    self.realized_pnl += sell_qty * (price - self.avg_entry_price);
                    self.net_size -= size;
                    if self.net_size < Decimal::ZERO {
                        // Flipped to short — remainder starts a new position
                        self.avg_entry_price = price;
                    } else if self.net_size.is_zero() {
                        self.avg_entry_price = Decimal::ZERO;
                    }
                    // If still long, avg_entry_price stays the same
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
    total_realized_pnl: std::sync::atomic::AtomicI64,
    /// File path for persisting cumulative PnL across restarts
    pnl_file: Option<std::path::PathBuf>,
    /// File path for persisting positions across restarts
    positions_file: Option<std::path::PathBuf>,
}

impl PositionTracker {
    /// Create a new tracker that persists PnL and positions to disk.
    pub fn new() -> Self {
        let pnl_file = std::path::PathBuf::from("pnl_total.txt");
        let positions_file = std::path::PathBuf::from("positions.json");
        let initial_pnl = Self::load_pnl(&pnl_file);
        let positions = Self::load_positions(&positions_file);
        Self {
            positions,
            total_realized_pnl: std::sync::atomic::AtomicI64::new(initial_pnl),
            pnl_file: Some(pnl_file),
            positions_file: Some(positions_file),
        }
    }

    /// Create a tracker without file persistence (for tests).
    #[cfg(test)]
    pub fn new_ephemeral() -> Self {
        Self {
            positions: DashMap::new(),
            total_realized_pnl: std::sync::atomic::AtomicI64::new(0),
            pnl_file: None,
            positions_file: None,
        }
    }

    fn load_pnl(path: &std::path::Path) -> i64 {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0)
    }

    fn load_positions(path: &std::path::Path) -> DashMap<U256, Position> {
        let map = DashMap::new();
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(positions) = serde_json::from_str::<Vec<Position>>(&data) {
                let count = positions.len();
                for pos in positions {
                    if !pos.net_size.is_zero() {
                        map.insert(pos.token_id, pos);
                    }
                }
                if count > 0 {
                    tracing::info!(loaded = map.len(), "restored positions from disk");
                }
            }
        }
        map
    }

    fn persist_pnl(&self) {
        if let Some(ref path) = self.pnl_file {
            let micros = self.total_realized_pnl.load(std::sync::atomic::Ordering::Relaxed);
            if let Err(e) = std::fs::write(path, micros.to_string()) {
                tracing::warn!(error = %e, path = %path.display(), "failed to persist PnL to disk");
            }
        }
    }

    fn persist_positions(&self) {
        if let Some(ref path) = self.positions_file {
            let positions: Vec<Position> = self.positions.iter().map(|e| e.value().clone()).collect();
            match serde_json::to_string(&positions) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(path, json) {
                        tracing::warn!(error = %e, path = %path.display(), "failed to persist positions to disk");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to serialize positions");
                }
            }
        }
    }

    pub fn record_fill(&self, token_id: U256, side: Side, size: Decimal, price: Decimal) {
        // Scope the DashMap entry guard so it's dropped before we persist
        let pnl_delta = {
            let mut entry = self
                .positions
                .entry(token_id)
                .or_insert_with(|| Position::new(token_id));
            let old_pnl = entry.realized_pnl;
            entry.record_fill(side, size, price);
            entry.realized_pnl - old_pnl
        }; // entry guard dropped here

        // Track total PnL in microdollars for atomicity
        if let Ok(micros) = (pnl_delta * dec!(1_000_000)).trunc().to_string().parse::<i64>() {
            self.total_realized_pnl
                .fetch_add(micros, std::sync::atomic::Ordering::Relaxed);
            self.persist_pnl();
        }
        self.persist_positions();
    }

    pub fn get_position(&self, token_id: &U256) -> Option<Position> {
        self.positions.get(token_id).map(|p| p.clone())
    }

    pub fn has_position(&self, token_id: &U256) -> bool {
        self.positions.contains_key(token_id)
    }

    /// Remove a position entirely (e.g. after on-chain redemption).
    pub fn remove_position(&self, token_id: &U256) {
        if self.positions.remove(token_id).is_some() {
            tracing::info!(token_id = %token_id, "position removed (redeemed)");
            self.persist_positions();
        }
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
            .total_realized_pnl
            .load(std::sync::atomic::Ordering::Relaxed);
        Decimal::new(micros, 6)
    }
}

/// Fetch open positions from the Polymarket data API and seed the tracker.
///
/// Queries both the EOA and proxy wallet addresses to catch positions from
/// before and after GnosisSafe routing was enabled. Deduplicates by token_id.
pub async fn sync_positions_from_api(
    wallets: &[Address],
    tracker: &PositionTracker,
) -> anyhow::Result<usize> {
    let client = polymarket_client_sdk::data::Client::default();
    let mut count = 0;

    for wallet in wallets {
        let req = PositionsRequest::builder()
            .user(*wallet)
            .limit(500)?
            .size_threshold(dec!(0.01))
            .build();

        let api_positions = client.positions(&req).await?;

        for p in &api_positions {
            if p.size.is_zero() {
                continue;
            }
            // Skip expired markets — they just eat exposure limit
            if p.end_date.is_some_and(|d| d < chrono::Utc::now().date_naive()) {
                tracing::debug!(
                    token_id = %p.asset,
                    end_date = ?p.end_date,
                    title = %p.title,
                    "skipping expired position"
                );
                continue;
            }
            // Skip if already synced from another wallet (dedup by token_id)
            if tracker.positions.contains_key(&p.asset) {
                continue;
            }
            let pos = Position {
                token_id: p.asset,
                net_size: p.size,
                avg_entry_price: p.avg_price,
                realized_pnl: Decimal::ZERO, // realized PnL resets each session
                total_bought: p.total_bought,
                total_sold: (p.total_bought - p.size).max(Decimal::ZERO),
            };
            tracing::debug!(
                token_id = %p.asset,
                size = %p.size,
                avg_price = %p.avg_price,
                wallet = %wallet,
                title = %p.title,
                "synced position"
            );
            tracker.positions.insert(p.asset, pos);
            count += 1;
        }
    }

    // Persist the synced positions to disk
    tracker.persist_positions();

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn token() -> U256 {
        U256::from(1u64)
    }

    #[test]
    fn buy_increasing_long() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        assert_eq!(pos.net_size, dec!(10));
        assert_eq!(pos.avg_entry_price, dec!(0.50));
        assert_eq!(pos.realized_pnl, dec!(0));

        // Buy more at higher price -> weighted avg
        pos.record_fill(Side::Buy, dec!(10), dec!(0.60));
        assert_eq!(pos.net_size, dec!(20));
        assert_eq!(pos.avg_entry_price, dec!(0.55));
        assert_eq!(pos.realized_pnl, dec!(0));
    }

    #[test]
    fn sell_reducing_long() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        // Sell half at profit
        pos.record_fill(Side::Sell, dec!(5), dec!(0.60));
        assert_eq!(pos.net_size, dec!(5));
        assert_eq!(pos.avg_entry_price, dec!(0.50)); // unchanged
        assert_eq!(pos.realized_pnl, dec!(0.50)); // 5 * (0.60 - 0.50)
    }

    #[test]
    fn sell_closing_long_exactly() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        assert_eq!(pos.net_size, dec!(0));
        assert_eq!(pos.avg_entry_price, dec!(0)); // reset
        assert_eq!(pos.realized_pnl, dec!(1.0)); // 10 * 0.10
    }

    #[test]
    fn sell_flipping_long_to_short() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        // Sell 15 -> realize PnL on 10 (the long), then 5 short at 0.60
        pos.record_fill(Side::Sell, dec!(15), dec!(0.60));
        assert_eq!(pos.net_size, dec!(-5));
        assert_eq!(pos.avg_entry_price, dec!(0.60)); // new short entry
        assert_eq!(pos.realized_pnl, dec!(1.0)); // 10 * (0.60 - 0.50)
    }

    #[test]
    fn sell_increasing_short() {
        let mut pos = Position::new(token());
        // Enter short
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        assert_eq!(pos.net_size, dec!(-10));
        assert_eq!(pos.avg_entry_price, dec!(0.60));
        assert_eq!(pos.realized_pnl, dec!(0));

        // Sell more at lower price -> weighted avg
        pos.record_fill(Side::Sell, dec!(10), dec!(0.50));
        assert_eq!(pos.net_size, dec!(-20));
        assert_eq!(pos.avg_entry_price, dec!(0.55));
        assert_eq!(pos.realized_pnl, dec!(0));
    }

    #[test]
    fn buy_reducing_short() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        // Cover half at lower price (profit)
        pos.record_fill(Side::Buy, dec!(5), dec!(0.50));
        assert_eq!(pos.net_size, dec!(-5));
        assert_eq!(pos.avg_entry_price, dec!(0.60)); // unchanged
        assert_eq!(pos.realized_pnl, dec!(0.50)); // 5 * (0.60 - 0.50)
    }

    #[test]
    fn buy_closing_short_exactly() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        assert_eq!(pos.net_size, dec!(0));
        assert_eq!(pos.avg_entry_price, dec!(0)); // reset
        assert_eq!(pos.realized_pnl, dec!(1.0)); // 10 * (0.60 - 0.50)
    }

    #[test]
    fn buy_flipping_short_to_long() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        // Buy 15 -> cover 10 (realize PnL), then 5 long at 0.50
        pos.record_fill(Side::Buy, dec!(15), dec!(0.50));
        assert_eq!(pos.net_size, dec!(5));
        assert_eq!(pos.avg_entry_price, dec!(0.50)); // new long entry
        assert_eq!(pos.realized_pnl, dec!(1.0)); // 10 * (0.60 - 0.50)
    }

    #[test]
    fn cumulative_pnl_multiple_trades() {
        let mut pos = Position::new(token());
        // Long trade: buy 10 @ 0.50, sell 10 @ 0.60 -> PnL +1.00
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        assert_eq!(pos.realized_pnl, dec!(1.0));

        // Short trade: sell 10 @ 0.60, buy 10 @ 0.50 -> PnL +1.00
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        assert_eq!(pos.realized_pnl, dec!(2.0));
    }

    #[test]
    fn unrealized_pnl_long() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Buy, dec!(10), dec!(0.50));
        assert_eq!(pos.unrealized_pnl(dec!(0.60)), dec!(1.0)); // +$1
        assert_eq!(pos.unrealized_pnl(dec!(0.40)), dec!(-1.0)); // -$1
    }

    #[test]
    fn unrealized_pnl_short() {
        let mut pos = Position::new(token());
        pos.record_fill(Side::Sell, dec!(10), dec!(0.60));
        // Short at 0.60, mark at 0.50 -> net_size(-10) * (0.50 - 0.60) = +1.0
        assert_eq!(pos.unrealized_pnl(dec!(0.50)), dec!(1.0));
        // Short at 0.60, mark at 0.70 -> net_size(-10) * (0.70 - 0.60) = -1.0
        assert_eq!(pos.unrealized_pnl(dec!(0.70)), dec!(-1.0));
    }
}
