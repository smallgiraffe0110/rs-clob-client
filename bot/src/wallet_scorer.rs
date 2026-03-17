use std::path::Path;
use std::sync::Arc;

use polymarket_client_sdk::data::types::request::{
    ClosedPositionsRequest, TraderLeaderboardRequest,
};
use polymarket_client_sdk::data::types::{LeaderboardCategory, LeaderboardOrderBy, TimePeriod};
use polymarket_client_sdk::types::{Address, Decimal};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::config::CopyTraderConfig;

const DATA_DIR: &str = "data";
const SCORED_FILE: &str = "data/scored_wallets.json";
const MAX_CLOSED_PAGES: i32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredWallet {
    pub address: Address,
    pub username: Option<String>,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub total_pnl: Decimal,
    pub trade_count: usize,
    pub score: f64,
}

#[derive(Serialize, Deserialize)]
struct ScoredWalletsFile {
    scored_at: i64,
    wallets: Vec<ScoredWallet>,
}

pub struct WalletScorer {
    data_client: polymarket_client_sdk::data::Client,
    config: CopyTraderConfig,
    pub scored: Arc<RwLock<Vec<ScoredWallet>>>,
}

impl WalletScorer {
    pub fn new(config: CopyTraderConfig, scored: Arc<RwLock<Vec<ScoredWallet>>>) -> Self {
        Self {
            data_client: polymarket_client_sdk::data::Client::default(),
            config,
            scored,
        }
    }

    pub async fn run(&self) {
        info!("wallet scorer starting");

        // Load previous results from disk so copy tracker has data immediately
        // Use cached wallets for up to 1 hour across restarts (scorer will refresh in background)
        if let Some(wallets) = Self::load_from_disk(3600) {
            info!(count = wallets.len(), "loaded scored wallets from disk");
            *self.scored.write().await = wallets;
        }

        let interval =
            tokio::time::Duration::from_secs(self.config.scorer_interval_secs);

        loop {
            self.score_candidates().await;
            tokio::time::sleep(interval).await;
        }
    }

    /// Score a single candidate wallet by fetching closed positions and computing metrics.
    async fn score_candidate(
        &self,
        address: Address,
        username: Option<String>,
    ) -> Option<ScoredWallet> {
        // Paginate closed positions with a cap
        let mut all_closed = Vec::new();
        let mut offset = 0i32;
        let max_offset = MAX_CLOSED_PAGES * 50;

        while let Ok(b) = ClosedPositionsRequest::builder()
            .user(address)
            .limit(50)
        {
            let req = match b.offset(offset) {
                Ok(b) => b.build(),
                Err(_) => break,
            };

            // Retry with backoff on rate limit (429)
            let mut attempt = 0u32;
            let positions_result = loop {
                match self.data_client.closed_positions(&req).await {
                    Ok(positions) => break Some(positions),
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("429") && attempt < 3 {
                            attempt += 1;
                            let wait = tokio::time::Duration::from_millis(1000 * 2u64.pow(attempt));
                            debug!(address = %address, attempt, wait_ms = wait.as_millis(), "rate limited, backing off");
                            tokio::time::sleep(wait).await;
                        } else {
                            debug!(address = %address, error = %e, "failed to fetch closed positions");
                            break None;
                        }
                    }
                }
            };

            match positions_result {
                Some(positions) => {
                    let count = positions.len();
                    all_closed.extend(positions);
                    if count < 50 {
                        break; // Last page
                    }
                    offset += 50;
                    if offset >= max_offset {
                        debug!(
                            address = %address,
                            positions = all_closed.len(),
                            "pagination cap reached"
                        );
                        break;
                    }
                    // Rate limit between pagination calls
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
                None => break,
            }
        }

        if all_closed.len() < self.config.min_closed_trades {
            debug!(
                address = %address,
                trades = all_closed.len(),
                min = self.config.min_closed_trades,
                "skipping candidate: insufficient trade history"
            );
            return None;
        }

        // Compute metrics
        let mut wins = 0usize;
        let mut losses = 0usize;
        let mut sum_positive = Decimal::ZERO;
        let mut sum_negative = Decimal::ZERO;
        let mut total_pnl = Decimal::ZERO;

        for pos in &all_closed {
            total_pnl += pos.realized_pnl;
            if pos.realized_pnl > Decimal::ZERO {
                wins += 1;
                sum_positive += pos.realized_pnl;
            } else if pos.realized_pnl < Decimal::ZERO {
                losses += 1;
                sum_negative += pos.realized_pnl; // negative value
            }
            // realized_pnl == 0 counts as neither win nor loss
        }

        let trade_count = wins + losses;
        if trade_count == 0 {
            return None;
        }

        // Hard filter: reject wallets with negative total PnL
        if total_pnl <= Decimal::ZERO {
            debug!(
                address = %address,
                total_pnl = %total_pnl,
                "skipping candidate: negative total PnL"
            );
            return None;
        }

        // Hard filter: reject wallets with 100% win rate and 0 losses.
        // These are typically market-maker bots or redemption arb accounts,
        // not genuine predictive traders we want to copy.
        if losses == 0 {
            debug!(
                address = %address,
                wins = wins,
                "skipping candidate: zero losses (likely bot/arb account)"
            );
            return None;
        }

        let win_rate = wins as f64 / trade_count as f64;

        // profit_factor = sum(positive_pnl) / abs(sum(negative_pnl))
        let profit_factor = if sum_negative < Decimal::ZERO {
            let pf = sum_positive / sum_negative.abs();
            pf.try_into().unwrap_or(0.0f64)
        } else {
            // No losses — perfect profit factor, cap at 10
            10.0
        };

        // Avg PnL per trade — how much real money per position
        let avg_pnl_per_trade: f64 = (total_pnl / Decimal::from(trade_count as u64))
            .try_into()
            .unwrap_or(0.0);

        // Composite score — favor consistency and skill over raw whale size:
        //   win_rate       * 0.20  (diminishing returns above 80% — very high WR = range grinder)
        //   profit_factor  * 0.30  (risk-adjusted: how much winners exceed losers)
        //   avg_pnl/trade  * 0.25  (edge per trade, not total capital deployed)
        //   trade_count    * 0.15  (statistical significance — more trades = more reliable)
        //   total_pnl      * 0.10  (tiebreaker, capped low so whales don't dominate)
        let total_pnl_f64: f64 = total_pnl.try_into().unwrap_or(0.0);
        let total_pnl_component = (total_pnl_f64 / 500.0).clamp(0.0, 1.0);
        let avg_pnl_component = (avg_pnl_per_trade / 5.0).clamp(0.0, 1.0);
        // Diminishing returns on win rate above 80% — crypto range grinders
        // hit 95%+ WR which shouldn't give them a big scoring edge
        let wr_component = if win_rate > 0.80 {
            0.80 + (win_rate - 0.80) * 0.25 // 95% WR → 0.8375 instead of 0.95
        } else {
            win_rate
        };
        let score = wr_component * 0.20
            + (profit_factor / 5.0).min(1.0) * 0.30
            + avg_pnl_component * 0.25
            + (trade_count as f64 / 100.0).min(1.0) * 0.15
            + total_pnl_component * 0.10;

        Some(ScoredWallet {
            address,
            username,
            win_rate,
            profit_factor,
            total_pnl,
            trade_count,
            score,
        })
    }

    fn parse_category(s: &str) -> LeaderboardCategory {
        match s.to_uppercase().as_str() {
            "POLITICS" => LeaderboardCategory::Politics,
            "SPORTS" => LeaderboardCategory::Sports,
            "CRYPTO" => LeaderboardCategory::Crypto,
            "CULTURE" => LeaderboardCategory::Culture,
            _ => LeaderboardCategory::Overall,
        }
    }

    async fn score_candidates(&self) {
        // Build category list: use multi-category if set, else fall back to single
        let categories: Vec<LeaderboardCategory> =
            if !self.config.auto_discover_categories.is_empty() {
                self.config
                    .auto_discover_categories
                    .iter()
                    .map(|s| Self::parse_category(s))
                    .collect()
            } else {
                vec![Self::parse_category(&self.config.auto_discover_category)]
            };

        // Fetch candidates from each category and deduplicate
        let mut seen = std::collections::HashSet::new();
        let mut all_candidates = Vec::new();

        for category in &categories {
            let Ok(req) = TraderLeaderboardRequest::builder()
                .category(*category)
                .time_period(TimePeriod::Week)
                .order_by(LeaderboardOrderBy::Pnl)
                .limit(50)
            else {
                warn!(?category, "failed to build leaderboard request");
                continue;
            };
            let req = req.build();

            match self.data_client.leaderboard(&req).await {
                Ok(entries) => {
                    info!(category = ?category, count = entries.len(), "fetched leaderboard");
                    for entry in entries {
                        if seen.insert(entry.proxy_wallet) {
                            all_candidates.push(entry);
                        }
                    }
                }
                Err(e) => {
                    warn!(category = ?category, error = %e, "failed to fetch leaderboard");
                }
            }
        }

        let candidates = all_candidates;
        if candidates.is_empty() {
            warn!("no candidates found across all categories");
            return;
        }
        info!(total = candidates.len(), categories = categories.len(), "scoring candidate wallets");

        // Score candidates sequentially to respect API rate limits
        let mut scored_wallets: Vec<ScoredWallet> = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            if let Some(sw) = self.score_candidate(c.proxy_wallet, c.user_name.clone()).await {
                scored_wallets.push(sw);
            }
            // Throttle between candidates to avoid 429s
            if i + 1 < candidates.len() {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
        }

        // Sort by score descending
        scored_wallets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Keep top N
        let keep = self.config.auto_discover_count as usize;
        scored_wallets.truncate(keep);

        info!(
            qualified = scored_wallets.len(),
            "wallet scoring complete"
        );

        for (i, w) in scored_wallets.iter().enumerate() {
            info!(
                rank = i + 1,
                address = %w.address,
                username = ?w.username,
                total_pnl = %w.total_pnl,
                win_rate = format!("{:.1}%", w.win_rate * 100.0),
                profit_factor = format!("{:.2}", w.profit_factor),
                trades = w.trade_count,
                score = format!("{:.3}", w.score),
                "scored wallet"
            );
        }

        // Write to shared state and persist to disk
        Self::save_to_disk(&scored_wallets);
        *self.scored.write().await = scored_wallets;
    }

    fn save_to_disk(wallets: &[ScoredWallet]) {
        if let Err(e) = std::fs::create_dir_all(DATA_DIR) {
            warn!(error = %e, "failed to create data directory");
            return;
        }
        let file = ScoredWalletsFile {
            scored_at: chrono::Utc::now().timestamp(),
            wallets: wallets.to_vec(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(e) = std::fs::write(SCORED_FILE, json) {
                    warn!(error = %e, "failed to write scored wallets to disk");
                } else {
                    info!(path = SCORED_FILE, "saved scored wallets to disk");
                }
            }
            Err(e) => warn!(error = %e, "failed to serialize scored wallets"),
        }
    }

    fn load_from_disk(max_age_secs: u64) -> Option<Vec<ScoredWallet>> {
        let path = Path::new(SCORED_FILE);
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(path) {
            Ok(json) => match serde_json::from_str::<ScoredWalletsFile>(&json) {
                Ok(file) => {
                    let now = chrono::Utc::now().timestamp();
                    let age = now - file.scored_at;
                    if age > max_age_secs as i64 {
                        warn!(
                            age_secs = age,
                            max_age_secs = max_age_secs,
                            "scored wallets on disk are stale, ignoring"
                        );
                        return None;
                    }
                    Some(file.wallets)
                }
                Err(e) => {
                    warn!(error = %e, "failed to parse scored wallets from disk");
                    None
                }
            },
            Err(e) => {
                warn!(error = %e, "failed to read scored wallets from disk");
                None
            }
        }
    }
}
