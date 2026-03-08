use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BotConfig {
    pub general: GeneralConfig,
    pub risk: RiskConfig,
    pub market_maker: MarketMakerConfig,
    pub market_selection: MarketSelectionConfig,
    #[serde(default)]
    pub copy_trader: CopyTraderConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralConfig {
    pub dry_run: bool,
    pub clob_url: String,
    pub ws_url: String,
    pub gamma_url: String,
    pub use_server_time: bool,
    pub tick_interval_ms: u64,
    #[serde(default = "default_dashboard_port")]
    pub dashboard_port: u16,
}

fn default_dashboard_port() -> u16 {
    3030
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub max_position_size: Decimal,
    pub max_open_orders: usize,
    pub max_total_exposure_usd: Decimal,
    pub daily_loss_limit_usd: Decimal,
    pub min_balance_usd: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketMakerConfig {
    pub half_spread: Decimal,
    pub min_edge: Decimal,
    pub requote_threshold: Decimal,
    pub order_size: Decimal,
    pub num_levels: usize,
    pub level_spacing: Decimal,
    pub inventory_skew_factor: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketSelectionConfig {
    pub token_ids: Vec<String>,
    pub condition_ids: Vec<String>,
    pub min_liquidity: Decimal,
    pub min_volume_24h: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CopyTraderConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub leaders: Vec<String>,
    #[serde(default = "default_auto_discover")]
    pub auto_discover: bool,
    #[serde(default = "default_discover_count")]
    pub auto_discover_count: i32,
    #[serde(default = "default_discover_category")]
    pub auto_discover_category: String,
    #[serde(default = "default_min_closed_trades")]
    pub min_closed_trades: usize,
    #[serde(default = "default_scorer_interval")]
    pub scorer_interval_secs: u64,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_scale")]
    pub scale_factor: Decimal,
    #[serde(default = "default_min_position")]
    pub min_position_usd: Decimal,
    #[serde(default = "default_slippage")]
    pub max_slippage: Decimal,
    #[serde(default = "default_stop_loss_pct")]
    pub stop_loss_pct: Decimal,
    #[serde(default = "default_max_entry_price")]
    pub max_entry_price: Decimal,
    #[serde(default = "default_max_entry_drift")]
    pub max_entry_drift: Decimal,
    #[serde(default = "default_max_days_to_resolution")]
    pub max_days_to_resolution: i64,
    #[serde(default)]
    pub exclude_title_keywords: Vec<String>,
}

impl Default for CopyTraderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            leaders: Vec::new(),
            auto_discover: true,
            auto_discover_count: 20,
            auto_discover_category: "OVERALL".into(),
            min_closed_trades: 10,
            scorer_interval_secs: 600,
            poll_interval_secs: 30,
            scale_factor: rust_decimal_macros::dec!(0.1),
            min_position_usd: rust_decimal_macros::dec!(5.0),
            max_slippage: rust_decimal_macros::dec!(0.02),
            stop_loss_pct: rust_decimal_macros::dec!(0.20),
            max_entry_price: rust_decimal_macros::dec!(0.97),
            max_entry_drift: rust_decimal_macros::dec!(0.05),
            max_days_to_resolution: 4,
            exclude_title_keywords: Vec::new(),
        }
    }
}

fn default_auto_discover() -> bool {
    true
}
fn default_discover_count() -> i32 {
    20
}
fn default_min_closed_trades() -> usize {
    10
}
fn default_scorer_interval() -> u64 {
    600
}
fn default_discover_category() -> String {
    "OVERALL".into()
}
fn default_poll_interval() -> u64 {
    10
}
fn default_scale() -> Decimal {
    rust_decimal_macros::dec!(0.1)
}
fn default_min_position() -> Decimal {
    rust_decimal_macros::dec!(5.0)
}
fn default_slippage() -> Decimal {
    rust_decimal_macros::dec!(0.02)
}
fn default_stop_loss_pct() -> Decimal {
    rust_decimal_macros::dec!(0.20)
}
fn default_max_entry_price() -> Decimal {
    rust_decimal_macros::dec!(0.97)
}
fn default_max_entry_drift() -> Decimal {
    rust_decimal_macros::dec!(0.05)
}
fn default_max_days_to_resolution() -> i64 {
    4
}

impl BotConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
    }
}
