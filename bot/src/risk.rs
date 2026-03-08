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

    pub fn check_order(
        &self,
        token_id: &U256,
        additional_size: Decimal,
        additional_exposure: Decimal,
        positions: &PositionTracker,
    ) -> Option<RiskVeto> {
        // Check position size limit
        let current_pos = positions.net_size(token_id).abs();
        if current_pos + additional_size > self.config.max_position_size {
            return Some(RiskVeto::MaxPositionSize {
                token_id: *token_id,
                current: current_pos,
                limit: self.config.max_position_size,
            });
        }

        // Check total exposure
        let total_exp = positions.total_exposure();
        if total_exp + additional_exposure > self.config.max_total_exposure_usd {
            return Some(RiskVeto::MaxTotalExposure {
                current: total_exp,
                limit: self.config.max_total_exposure_usd,
            });
        }

        // Check daily loss limit
        let daily_pnl = positions.daily_pnl();
        if daily_pnl < -self.config.daily_loss_limit_usd {
            return Some(RiskVeto::DailyLossLimit {
                current_pnl: daily_pnl,
                limit: self.config.daily_loss_limit_usd,
            });
        }

        None
    }
}
