//! Automatic redemption of resolved Polymarket conditional tokens.
//!
//! Periodically checks for positions where `redeemable = true`, verifies
//! on-chain ERC-1155 balance, and calls `redeemPositions` to convert
//! winning tokens back to USDC.e.

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::types::address;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use crate::order_manager::OrderManager;
use crate::position::PositionTracker;
use crate::strategy::StrategyAction;

sol! {
    #[sol(rpc)]
    interface IERC1155 {
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IConditionalTokens {
        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;
    }

    #[sol(rpc)]
    interface INegRiskAdapter {
        function redeemPositions(
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;
    }
}

const CTF_ADDRESS: Address = address!("4D97DCd97eC945f40cF65F87097ACe5EA0476045");
const USDCE_ADDRESS: Address = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
const NEG_RISK_ADAPTER: Address = address!("d91E80cF2E7be2e162c6513ceD06f1dD0dA35296");

pub struct Redeemer {
    signer: Arc<PrivateKeySigner>,
    positions: Arc<PositionTracker>,
    order_manager: Arc<OrderManager>,
    wallets: Vec<Address>,
    interval_secs: u64,
}

impl Redeemer {
    pub fn new(
        signer: Arc<PrivateKeySigner>,
        positions: Arc<PositionTracker>,
        order_manager: Arc<OrderManager>,
        wallets: Vec<Address>,
        interval_secs: u64,
    ) -> Self {
        Self {
            signer,
            positions,
            order_manager,
            wallets,
            interval_secs,
        }
    }

    pub async fn run(&self) {
        info!(
            interval_secs = self.interval_secs,
            wallets = self.wallets.len(),
            "auto-redeemer started"
        );

        let interval = tokio::time::Duration::from_secs(self.interval_secs);
        // Wait a bit before first check to let the bot settle
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

        loop {
            if let Err(e) = self.check_and_redeem().await {
                warn!(error = %e, "redemption cycle failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn check_and_redeem(&self) -> anyhow::Result<()> {
        let data_client = polymarket_client_sdk::data::Client::default();

        // Collect all redeemable positions across wallets
        struct MarketInfo {
            title: String,
            negative_risk: bool,
            token_ids: Vec<U256>,
        }

        // Near-certainty positions to sell on CLOB ($0.99+, not yet resolved)
        struct NearCertaintySell {
            token_id: U256,
            cur_price: Decimal,
            our_size: Decimal,
            title: String,
        }

        let mut redeemable_markets: HashMap<FixedBytes<32>, MarketInfo> = HashMap::new();
        let mut near_certainty_sells: Vec<NearCertaintySell> = Vec::new();

        for wallet in &self.wallets {
            let req = PositionsRequest::builder()
                .user(*wallet)
                .limit(500)?
                .size_threshold(dec!(0.01))
                .build();

            let positions = match data_client.positions(&req).await {
                Ok(p) => p,
                Err(e) => {
                    debug!(wallet = %wallet, error = %e, "failed to fetch positions for redemption check");
                    continue;
                }
            };

            for pos in &positions {
                if pos.redeemable {
                    let entry = redeemable_markets
                        .entry(pos.condition_id)
                        .or_insert_with(|| MarketInfo {
                            title: pos.title.clone(),
                            negative_risk: pos.negative_risk,
                            token_ids: Vec::new(),
                        });
                    if !entry.token_ids.contains(&pos.asset) {
                        entry.token_ids.push(pos.asset);
                    }
                    continue;
                }

                // Sell positions trading at $0.99+ even if not officially resolved.
                // At this price there's <1 cent upside — better to lock in profit now.
                if pos.cur_price >= dec!(0.99) {
                    let our_size = self.positions.net_size(&pos.asset);
                    if our_size > Decimal::ZERO
                        && !near_certainty_sells.iter().any(|s| s.token_id == pos.asset)
                    {
                        near_certainty_sells.push(NearCertaintySell {
                            token_id: pos.asset,
                            cur_price: pos.cur_price,
                            our_size,
                            title: pos.title.clone(),
                        });
                    }
                }
            }
        }

        // Sell near-certainty positions on the CLOB
        if !near_certainty_sells.is_empty() {
            info!(
                count = near_certainty_sells.len(),
                "found positions at $0.99+ — selling before resolution"
            );
            for sell in &near_certainty_sells {
                let price = (sell.cur_price - dec!(0.01)).max(dec!(0.01)).round_dp(2);
                info!(
                    title = %sell.title,
                    cur_price = %sell.cur_price,
                    size = %sell.our_size,
                    sell_price = %price,
                    "auto-selling near-certainty position"
                );
                let action = StrategyAction::PlaceOrder {
                    token_id: sell.token_id,
                    side: Side::Sell,
                    price,
                    size: sell.our_size,
                    taker: true,
                };
                if let Err(e) = self.order_manager.execute(vec![action]).await {
                    warn!(title = %sell.title, error = %e, "failed to sell near-certainty position");
                }
            }
        }

        if redeemable_markets.is_empty() {
            if near_certainty_sells.is_empty() {
                debug!("no redeemable or near-certainty markets found");
            }
            return Ok(());
        }

        info!(
            count = redeemable_markets.len(),
            "found redeemable markets, checking on-chain balances"
        );

        // Set up provider with wallet for on-chain transactions
        let wallet = alloy::network::EthereumWallet::from((*self.signer).clone());
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http("https://polygon-bor-rpc.publicnode.com".parse()?);

        let wallet_address = self.signer.address();

        // Check POL balance for gas
        let pol_balance = provider.get_balance(wallet_address).await?;
        if pol_balance == U256::ZERO {
            warn!("no POL for gas — skipping redemption cycle");
            return Ok(());
        }

        let ctf = IERC1155::new(CTF_ADDRESS, &provider);
        let ctf_contract = IConditionalTokens::new(CTF_ADDRESS, &provider);
        let neg_risk_contract = INegRiskAdapter::new(NEG_RISK_ADAPTER, &provider);

        let index_sets = vec![U256::from(1), U256::from(2)];
        let mut redeemed = 0u32;

        for (condition_id, info) in &redeemable_markets {
            // Check on-chain balance for each token
            let mut has_balance = false;
            for token_id in &info.token_ids {
                let bal = ctf.balanceOf(wallet_address, *token_id).call().await?;
                if bal > U256::ZERO {
                    has_balance = true;
                    break;
                }
            }

            if !has_balance {
                debug!(title = %info.title, "skipping redemption: zero on-chain balance");
                continue;
            }

            info!(title = %info.title, neg_risk = info.negative_risk, "redeeming resolved market");

            let result = if info.negative_risk {
                neg_risk_contract
                    .redeemPositions(*condition_id, index_sets.clone())
                    .send()
                    .await
            } else {
                let parent_collection_id = FixedBytes::<32>::ZERO;
                ctf_contract
                    .redeemPositions(USDCE_ADDRESS, parent_collection_id, *condition_id, index_sets.clone())
                    .send()
                    .await
            };

            match result {
                Ok(pending) => match pending.get_receipt().await {
                    Ok(receipt) => {
                        info!(
                            title = %info.title,
                            tx = %receipt.transaction_hash,
                            "redeemed successfully"
                        );
                        redeemed += 1;

                        // Remove redeemed positions from tracker so exposure is freed
                        for token_id in &info.token_ids {
                            self.positions.remove_position(token_id);
                        }
                    }
                    Err(e) => {
                        warn!(title = %info.title, error = %e, "redemption tx failed");
                    }
                },
                Err(e) => {
                    let err_str = format!("{e}");
                    if err_str.contains("revert") {
                        debug!(title = %info.title, "redemption reverted (may not be fully resolved yet)");
                    } else {
                        warn!(title = %info.title, error = %e, "redemption send failed");
                    }
                }
            }
        }

        if redeemed > 0 {
            // Check final USDC.e balance
            let usdce = IERC20::new(USDCE_ADDRESS, &provider);
            let balance = usdce.balanceOf(wallet_address).call().await?;
            let balance_f64 = balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
            info!(
                redeemed_count = redeemed,
                usdce_balance = format!("${:.2}", balance_f64),
                "redemption cycle complete"
            );
        }

        Ok(())
    }
}
