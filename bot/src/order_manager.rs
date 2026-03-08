use std::sync::Arc;

use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use dashmap::DashMap;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::types::{OrderType, Side};
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::types::{Decimal, U256};
use tracing::{error, info, warn};

use crate::strategy::StrategyAction;

#[derive(Debug, Clone)]
pub struct LiveOrder {
    pub order_id: String,
    pub token_id: U256,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
}

pub struct OrderManager {
    client: Arc<Client<Authenticated<Normal>>>,
    signer: Arc<PrivateKeySigner>,
    live_orders: DashMap<String, LiveOrder>,
    /// Maps token_id -> set of order_ids
    orders_by_token: DashMap<U256, Vec<String>>,
    dry_run: bool,
}

impl OrderManager {
    pub fn new(
        client: Arc<Client<Authenticated<Normal>>>,
        signer: Arc<PrivateKeySigner>,
        dry_run: bool,
    ) -> Self {
        Self {
            client,
            signer,
            live_orders: DashMap::new(),
            orders_by_token: DashMap::new(),
            dry_run,
        }
    }

    pub async fn execute(&self, actions: Vec<StrategyAction>) -> Result<()> {
        for action in actions {
            if let Err(e) = self.execute_one(action).await {
                error!(error = %e, "failed to execute action");
            }
        }
        Ok(())
    }

    async fn execute_one(&self, action: StrategyAction) -> Result<()> {
        match action {
            StrategyAction::PlaceOrder {
                token_id,
                side,
                price,
                size,
                taker,
            } => {
                self.place_order(token_id, side, price, size, taker).await?;
            }
            StrategyAction::CancelOrder { order_id } => {
                self.cancel_order(&order_id).await?;
            }
            StrategyAction::CancelAllForToken { token_id } => {
                self.cancel_all_for_token(&token_id).await?;
            }
        }
        Ok(())
    }

    async fn place_order(
        &self,
        token_id: U256,
        side: Side,
        price: Decimal,
        size: Decimal,
        taker: bool,
    ) -> Result<()> {
        let side_str = match side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
            _ => "UNKNOWN",
        };

        let order_type = if taker { OrderType::FAK } else { OrderType::GTC };
        let order_type_str = if taker { "FAK" } else { "GTC" };

        if self.dry_run {
            info!(
                dry_run = true,
                token_id = %token_id,
                side = side_str,
                price = %price,
                size = %size,
                order_type = order_type_str,
                "would place order"
            );
            return Ok(());
        }

        info!(
            token_id = %token_id,
            side = side_str,
            price = %price,
            size = %size,
            order_type = order_type_str,
            "placing order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(side)
            .price(price)
            .size(size)
            .order_type(order_type)
            .post_only(!taker)
            .build()
            .await
            .context("building order")?;

        let signed = self
            .client
            .sign(&*self.signer, signable)
            .await
            .context("signing order")?;

        let response = self
            .client
            .post_order(signed)
            .await
            .context("posting order")?;

        if response.success {
            let order_id = response.order_id.clone();
            info!(order_id = %order_id, "order placed successfully");

            let live = LiveOrder {
                order_id: order_id.clone(),
                token_id,
                side,
                price,
                size,
            };
            self.live_orders.insert(order_id.clone(), live);
            self.orders_by_token
                .entry(token_id)
                .or_default()
                .push(order_id);
        } else {
            warn!(
                order_id = %response.order_id,
                error = ?response.error_msg,
                "order rejected"
            );
        }

        Ok(())
    }

    async fn cancel_order(&self, order_id: &str) -> Result<()> {
        if self.dry_run {
            info!(dry_run = true, order_id = %order_id, "would cancel order");
            self.remove_order(order_id);
            return Ok(());
        }

        info!(order_id = %order_id, "canceling order");
        match self.client.cancel_order(order_id).await {
            Ok(resp) => {
                if !resp.canceled.is_empty() {
                    info!(order_id = %order_id, "order canceled");
                }
                if let Some(reason) = resp.not_canceled.get(order_id) {
                    warn!(order_id = %order_id, reason = %reason, "order not canceled");
                }
            }
            Err(e) => {
                warn!(order_id = %order_id, error = %e, "cancel request failed");
            }
        }
        self.remove_order(order_id);
        Ok(())
    }

    async fn cancel_all_for_token(&self, token_id: &U256) -> Result<()> {
        let order_ids: Vec<String> = self
            .orders_by_token
            .get(token_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        if order_ids.is_empty() {
            return Ok(());
        }

        if self.dry_run {
            info!(
                dry_run = true,
                token_id = %token_id,
                count = order_ids.len(),
                "would cancel all orders for token"
            );
            for id in &order_ids {
                self.remove_order(id);
            }
            return Ok(());
        }

        let id_refs: Vec<&str> = order_ids.iter().map(|s| s.as_str()).collect();
        info!(
            token_id = %token_id,
            count = id_refs.len(),
            "canceling all orders for token"
        );

        match self.client.cancel_orders(&id_refs).await {
            Ok(resp) => {
                info!(
                    canceled = resp.canceled.len(),
                    not_canceled = resp.not_canceled.len(),
                    "batch cancel complete"
                );
            }
            Err(e) => {
                warn!(token_id = %token_id, error = %e, "batch cancel failed");
            }
        }

        for id in &order_ids {
            self.remove_order(id);
        }
        Ok(())
    }

    fn remove_order(&self, order_id: &str) {
        if let Some((_, order)) = self.live_orders.remove(order_id) {
            if let Some(mut ids) = self.orders_by_token.get_mut(&order.token_id) {
                ids.retain(|id| id != order_id);
            }
        }
    }

    pub fn live_orders_for_token(&self, token_id: &U256) -> Vec<LiveOrder> {
        self.orders_by_token
            .get(token_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.live_orders.get(id).map(|o| o.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn live_order_ids_for_token(&self, token_id: &U256) -> Vec<String> {
        self.orders_by_token
            .get(token_id)
            .map(|ids| ids.clone())
            .unwrap_or_default()
    }

    pub fn remove_order_by_id(&self, order_id: &str) {
        self.remove_order(order_id);
    }
}
