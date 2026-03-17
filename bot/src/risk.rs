use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{Decimal, U256};

use crate::config::RiskConfig;
use crate::position::PositionTracker;

#[derive(Debug, Clone)]
pub enum RiskVeto {
    MaxPositionSize { token_id: U256, current: Decimal, limit: Decimal },
    MaxTotalExposure { current: Decimal, limit: Decimal },
    DailyLossLimit { current_pnl: Decimal, limit: Decimal },
}

impl std::fmt::Display for RiskVeto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxPositionSize { token_id, current, limit } => {
                write!(f, "position size {current} exceeds limit {limit} for {token_id}")
            }
            Self::MaxTotalExposure { current, limit } => {
                write!(f, "total exposure {current} exceeds limit {limit}")
            }
            Self::DailyLossLimit { current_pnl, limit } => {
                write!(f, "daily PnL {current_pnl} exceeds loss limit -{limit}")
            }
        }
    }
}

pub struct RiskManager {
    config: RiskConfig,
}

impl RiskManager {
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// Compute effective risk limits, scaling with bankroll when initial_bankroll is set.
    fn effective_limits(&self, positions: &PositionTracker) -> (Decimal, Decimal, Decimal) {
        if self.config.initial_bankroll > Decimal::ZERO {
            let bankroll = (self.config.initial_bankroll + positions.daily_pnl())
                .max(Decimal::ZERO);
            (
                bankroll * self.config.max_position_size_pct,
                bankroll * self.config.max_exposure_pct,
                bankroll * self.config.daily_loss_limit_pct,
            )
        } else {
            (
                self.config.max_position_size,
                self.config.max_total_exposure_usd,
                self.config.daily_loss_limit_usd,
            )
        }
    }

    #[allow(dead_code)] // available for dashboard/logging use
    pub fn current_bankroll(&self, positions: &PositionTracker) -> Decimal {
        if self.config.initial_bankroll > Decimal::ZERO {
            (self.config.initial_bankroll + positions.daily_pnl()).max(Decimal::ZERO)
        } else {
            Decimal::ZERO
        }
    }

    pub fn check_order(
        &self,
        token_id: &U256,
        side: Side,
        additional_size: Decimal,
        additional_exposure: Decimal,
        positions: &PositionTracker,
    ) -> Option<RiskVeto> {
        self.check_order_with_pending(token_id, side, additional_size, additional_exposure, positions, Decimal::ZERO)
    }

    /// Like `check_order` but accounts for exposure from orders already approved
    /// in the current batch that haven't been filled yet.
    pub fn check_order_with_pending(
        &self,
        token_id: &U256,
        side: Side,
        additional_size: Decimal,
        additional_exposure: Decimal,
        positions: &PositionTracker,
        pending_exposure: Decimal,
    ) -> Option<RiskVeto> {
        let net_size = positions.net_size(token_id);
        let (max_pos, max_exp, max_loss) = self.effective_limits(positions);

        // Determine if this order increases risk
        let is_risk_increasing = match side {
            Side::Buy => net_size >= Decimal::ZERO,  // buying when flat/long = increasing
            Side::Sell => net_size <= Decimal::ZERO,  // selling when flat/short = increasing
            _ => true, // unknown side = conservative
        };

        // Position size and exposure limits only apply to risk-increasing orders
        if is_risk_increasing {
            let current_pos = net_size.abs();
            if current_pos + additional_size > max_pos {
                return Some(RiskVeto::MaxPositionSize {
                    token_id: *token_id,
                    current: current_pos,
                    limit: max_pos,
                });
            }

            let total_exp = positions.total_exposure() + pending_exposure;
            if total_exp + additional_exposure > max_exp {
                return Some(RiskVeto::MaxTotalExposure {
                    current: total_exp,
                    limit: max_exp,
                });
            }
        }

        // Daily loss limit blocks risk-INCREASING orders only.
        // Risk-reducing orders (exits/sells of held positions) must always be allowed
        // so the bot can stop-loss and exit positions even when the loss limit is hit.
        let daily_pnl = positions.daily_pnl();
        if daily_pnl < -max_loss && is_risk_increasing {
            return Some(RiskVeto::DailyLossLimit {
                current_pnl: daily_pnl,
                limit: max_loss,
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn config() -> RiskConfig {
        RiskConfig {
            max_position_size: dec!(100),
            max_total_exposure_usd: dec!(200),
            daily_loss_limit_usd: dec!(20),
            initial_bankroll: Decimal::ZERO, // use fixed limits for tests
            max_exposure_pct: dec!(0.80),
            max_position_size_pct: dec!(0.10),
            daily_loss_limit_pct: dec!(0.15),
        }
    }

    fn token() -> U256 {
        U256::from(1u64)
    }

    #[test]
    fn buy_when_flat_is_risk_increasing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        // Buying 150 when flat should be blocked (over 100 limit)
        let veto = risk.check_order(&token(), Side::Buy, dec!(150), dec!(75), &positions);
        assert!(matches!(veto, Some(RiskVeto::MaxPositionSize { .. })));
    }

    #[test]
    fn sell_when_long_is_risk_reducing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        // First build a long position of 90
        positions.record_fill(token(), Side::Buy, dec!(90), dec!(0.50));
        // Selling 50 when long = reducing, should pass even though pos is near limit
        let veto = risk.check_order(&token(), Side::Sell, dec!(50), dec!(25), &positions);
        assert!(veto.is_none());
    }

    #[test]
    fn sell_when_short_is_risk_increasing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        positions.record_fill(token(), Side::Sell, dec!(90), dec!(0.50));
        // Selling more when already short = increasing
        let veto = risk.check_order(&token(), Side::Sell, dec!(20), dec!(10), &positions);
        assert!(matches!(veto, Some(RiskVeto::MaxPositionSize { .. })));
    }

    #[test]
    fn buy_when_short_is_risk_reducing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        positions.record_fill(token(), Side::Sell, dec!(90), dec!(0.50));
        // Buying when short = reducing, should pass
        let veto = risk.check_order(&token(), Side::Buy, dec!(50), dec!(25), &positions);
        assert!(veto.is_none());
    }

    #[test]
    fn daily_loss_limit_blocks_risk_increasing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        // Create a loss that exceeds the limit
        positions.record_fill(token(), Side::Buy, dec!(100), dec!(0.50));
        positions.record_fill(token(), Side::Sell, dec!(100), dec!(0.29)); // -$21 loss
        // A risk-increasing order (new buy when flat) should be blocked
        let veto = risk.check_order(&token(), Side::Buy, dec!(10), dec!(5), &positions);
        assert!(matches!(veto, Some(RiskVeto::DailyLossLimit { .. })));
    }

    #[test]
    fn daily_loss_limit_allows_risk_reducing() {
        let risk = RiskManager::new(config());
        let positions = PositionTracker::new_ephemeral();
        // Create a loss that exceeds the limit
        positions.record_fill(token(), Side::Buy, dec!(100), dec!(0.50));
        positions.record_fill(token(), Side::Sell, dec!(100), dec!(0.29)); // -$21 loss
        // Risk-reducing order (selling a held position) should still be allowed
        positions.record_fill(token(), Side::Buy, dec!(10), dec!(0.50));
        let veto = risk.check_order(&token(), Side::Sell, dec!(5), dec!(2.5), &positions);
        assert!(veto.is_none(), "risk-reducing sells must be allowed even when daily loss limit is hit");
    }
}
