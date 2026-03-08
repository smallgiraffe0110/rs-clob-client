use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
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
        if let Some(wallets) = Self::load_from_disk(self.config.scorer_interval_secs * 2) {
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

        loop {
            let req = match ClosedPositionsRequest::builder()
                .user(address)
                .limit(50)
            {
                Ok(b) => match b.offset(offset) {
                    Ok(b) => b.build(),
                    Err(_) => break,
                },
                Err(_) => break,
            };

            match self.data_client.closed_positions(&req).await {
                Ok(positions) => {
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
                }
                Err(e) => {
                    debug!(
                        address = %address,
                        error = %e,
                        "failed to fetch closed positions"
                    );
                    break;
                }
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

        // Composite score:
        //   avg_pnl * 0.30 + win_rate * 0.25 + profit_factor * 0.25 + volume * 0.20
        // avg_pnl_component: $5 avg profit per trade = max score
        let avg_pnl_component = (avg_pnl_per_trade / 5.0).clamp(0.0, 1.0);
        let score = avg_pnl_component * 0.3
            + win_rate * 0.25
            + (profit_factor / 5.0).min(1.0) * 0.25
            + (trade_count as f64 / 100.0).min(1.0) * 0.2;

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

    async fn score_candidates(&self) {
        let category = match self.config.auto_discover_category.to_uppercase().as_str() {
            "POLITICS" => LeaderboardCategory::Politics,
            "SPORTS" => LeaderboardCategory::Sports,
            "CRYPTO" => LeaderboardCategory::Crypto,
            "CULTURE" => LeaderboardCategory::Culture,
            _ => LeaderboardCategory::Overall,
        };

        // Fetch top 50 candidates by PnL (weekly) as candidate pool
        let Ok(req) = TraderLeaderboardRequest::builder()
            .category(category)
            .time_period(TimePeriod::Week)
            .order_by(LeaderboardOrderBy::Pnl)
            .limit(50)
        else {
            warn!("failed to build leaderboard request");
            return;
        };
        let req = req.build();

        let candidates = match self.data_client.leaderboard(&req).await {
            Ok(entries) => {
                info!(count = entries.len(), "scoring candidate wallets");
                entries
            }
            Err(e) => {
                warn!(error = %e, "failed to fetch leaderboard for scoring");
                return;
            }
        };

        // Score candidates concurrently (up to 5 at a time)
        let candidate_inputs: Vec<_> = candidates
            .iter()
            .map(|c| (c.proxy_wallet, c.user_name.clone()))
            .collect();

        let mut scored_wallets: Vec<ScoredWallet> = stream::iter(candidate_inputs)
            .map(|(addr, name)| self.score_candidate(addr, name))
            .buffer_unordered(5)
            .filter_map(|opt| async { opt })
            .collect()
            .await;

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
