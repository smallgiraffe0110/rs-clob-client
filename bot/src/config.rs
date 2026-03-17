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
    #[serde(default)]
    pub btc_trader: BtcTraderConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralConfig {
    pub dry_run: bool,
    pub clob_url: String,
    pub use_server_time: bool,
    pub tick_interval_ms: u64,
    #[serde(default = "default_dashboard_port")]
    pub dashboard_port: u16,
    #[serde(default = "default_dashboard_bind")]
    pub dashboard_bind: String,
    /// Signature type for order signing: "eoa", "proxy", or "gnosis_safe"
    #[serde(default = "default_signature_type")]
    pub signature_type: String,
}

fn default_signature_type() -> String {
    "eoa".into()
}

fn default_dashboard_port() -> u16 {
    3030
}

fn default_max_exposure_pct() -> Decimal {
    rust_decimal_macros::dec!(0.80)
}
fn default_max_position_size_pct() -> Decimal {
    rust_decimal_macros::dec!(0.10)
}
fn default_daily_loss_limit_pct() -> Decimal {
    rust_decimal_macros::dec!(0.15)
}

fn default_dashboard_bind() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub max_position_size: Decimal,
    pub max_total_exposure_usd: Decimal,
    pub daily_loss_limit_usd: Decimal,
    #[serde(default)]
    pub initial_bankroll: Decimal,
    #[serde(default = "default_max_exposure_pct")]
    pub max_exposure_pct: Decimal,
    #[serde(default = "default_max_position_size_pct")]
    pub max_position_size_pct: Decimal,
    #[serde(default = "default_daily_loss_limit_pct")]
    pub daily_loss_limit_pct: Decimal,
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
#[allow(dead_code)] // Config schema fields — parsed from config.toml, used for future market selection
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
    #[serde(default)]
    pub auto_discover_categories: Vec<String>,
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
    #[serde(default = "default_slippage", alias = "max_slippage")]
    pub max_slippage_pct: Decimal,
    #[serde(default = "default_stop_loss_pct")]
    pub stop_loss_pct: Decimal,
    #[serde(default = "default_take_profit_price")]
    pub take_profit_price: Decimal,
    #[serde(default = "default_max_entry_price")]
    pub max_entry_price: Decimal,
    #[serde(default = "default_max_entry_drift")]
    pub max_entry_drift: Decimal,
    #[serde(default = "default_max_days_to_resolution")]
    pub max_days_to_resolution: i64,
    #[serde(default = "default_min_days_to_resolution")]
    pub min_days_to_resolution: i64,
    #[serde(default = "default_max_target_size")]
    pub max_target_size: Decimal,
    #[serde(default = "default_max_target_size_pct")]
    pub max_target_size_pct: Decimal,
    #[serde(default = "default_min_cur_price")]
    pub min_cur_price: Decimal,
    #[serde(default = "default_min_leaders")]
    pub min_leaders_for_entry: i64,
    #[serde(default = "default_exit_cooldown_secs")]
    pub exit_cooldown_secs: u64,
    #[serde(default = "default_exit_absent_polls")]
    pub exit_absent_polls: u32,
    #[serde(default)]
    pub warmup_polls: u32,
    #[serde(default = "default_near_expiry_days")]
    pub near_expiry_days: i64,
    #[serde(default = "default_near_expiry_max_entry_price")]
    pub near_expiry_max_entry_price: Decimal,
    #[serde(default = "default_near_expiry_max_entry_drift")]
    pub near_expiry_max_entry_drift: Decimal,
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
            auto_discover_categories: Vec::new(),
            min_closed_trades: 10,
            scorer_interval_secs: 600,
            poll_interval_secs: 30,
            scale_factor: rust_decimal_macros::dec!(0.1),
            min_position_usd: rust_decimal_macros::dec!(5.0),
            max_slippage_pct: rust_decimal_macros::dec!(0.03),
            stop_loss_pct: rust_decimal_macros::dec!(0.20),
            take_profit_price: rust_decimal_macros::dec!(0.93),
            max_entry_price: rust_decimal_macros::dec!(0.97),
            max_entry_drift: rust_decimal_macros::dec!(0.05),
            max_days_to_resolution: 4,
            min_days_to_resolution: 1,
            max_target_size: rust_decimal_macros::dec!(25.0),
            max_target_size_pct: rust_decimal_macros::dec!(0.06),
            min_cur_price: rust_decimal_macros::dec!(0.15),
            min_leaders_for_entry: 3,
            exit_cooldown_secs: 1800,
            exit_absent_polls: 3,
            warmup_polls: 0,
            near_expiry_days: 2,
            near_expiry_max_entry_price: rust_decimal_macros::dec!(0.97),
            near_expiry_max_entry_drift: rust_decimal_macros::dec!(0.40),
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
    30
}
fn default_scale() -> Decimal {
    rust_decimal_macros::dec!(0.1)
}
fn default_min_position() -> Decimal {
    rust_decimal_macros::dec!(5.0)
}
fn default_slippage() -> Decimal {
    rust_decimal_macros::dec!(0.03)
}
fn default_stop_loss_pct() -> Decimal {
    rust_decimal_macros::dec!(0.20)
}
fn default_take_profit_price() -> Decimal {
    rust_decimal_macros::dec!(0.93)
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
fn default_min_days_to_resolution() -> i64 {
    1
}
fn default_max_target_size() -> Decimal {
    rust_decimal_macros::dec!(25.0)
}
fn default_max_target_size_pct() -> Decimal {
    rust_decimal_macros::dec!(0.06)
}
fn default_min_cur_price() -> Decimal {
    rust_decimal_macros::dec!(0.15)
}
fn default_min_leaders() -> i64 {
    3
}
fn default_exit_cooldown_secs() -> u64 {
    1800
}
fn default_exit_absent_polls() -> u32 {
    3
}
fn default_near_expiry_days() -> i64 {
    2
}
fn default_near_expiry_max_entry_price() -> Decimal {
    rust_decimal_macros::dec!(0.97)
}
fn default_near_expiry_max_entry_drift() -> Decimal {
    rust_decimal_macros::dec!(0.40)
}

#[derive(Debug, Clone, Deserialize)]
pub struct BtcTraderConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_btc_bet_size")]
    pub bet_size_usd: Decimal,
    #[serde(default = "default_btc_min_probability")]
    pub min_probability: f64,
    #[serde(default = "default_btc_min_edge")]
    pub min_edge: f64,
    #[serde(default = "default_btc_max_exposure")]
    pub max_exposure_usd: Decimal,
    #[serde(default = "default_btc_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_btc_market_search_secs")]
    pub market_search_interval_secs: u64,
    #[serde(default = "default_btc_entry_window_secs")]
    pub entry_window_secs: u64,
    #[serde(default = "default_btc_vol_samples")]
    pub vol_min_samples: usize,
}

impl Default for BtcTraderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bet_size_usd: rust_decimal_macros::dec!(7.0),
            min_probability: 0.68,
            min_edge: 0.05,
            max_exposure_usd: rust_decimal_macros::dec!(50.0),
            poll_interval_ms: 500,
            market_search_interval_secs: 120,
            entry_window_secs: 180,
            vol_min_samples: 30,
        }
    }
}

fn default_btc_bet_size() -> Decimal { rust_decimal_macros::dec!(7.0) }
fn default_btc_min_probability() -> f64 { 0.68 }
fn default_btc_min_edge() -> f64 { 0.05 }
fn default_btc_max_exposure() -> Decimal { rust_decimal_macros::dec!(50.0) }
fn default_btc_poll_interval_ms() -> u64 { 500 }
fn default_btc_market_search_secs() -> u64 { 120 }
fn default_btc_entry_window_secs() -> u64 { 180 }
fn default_btc_vol_samples() -> usize { 30 }

impl BotConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
    }
}
