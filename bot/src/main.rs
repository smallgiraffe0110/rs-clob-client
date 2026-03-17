mod btc_trader;
mod config;
mod copy_tracker;
mod dashboard;
mod dashboard_html;
mod engine;
mod market_state;
mod order_manager;
mod position;
mod redeemer;
mod risk;
mod strategy;
mod wallet_scorer;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, bail};
use futures::StreamExt as _;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::clob::ws::WsMessage;
use polymarket_client_sdk::types::U256;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{error, info, warn};

use crate::config::BotConfig;
use crate::engine::{Engine, EngineEvent};
use crate::market_state::MarketState;
use crate::order_manager::OrderManager;
use crate::position::PositionTracker;
use crate::risk::RiskManager;
use crate::strategy::market_maker::MarketMakerStrategy;

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider before any TLS connections (WebSockets)
    let _ = rustls::crypto::ring::default_provider().install_default();

    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,polymarket_bot=debug,polymarket_client_sdk::clob::client=off,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off"
                    .into()
            }),
        )
        .init();

    // Load config
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let config = BotConfig::load(&config_path)?;

    info!(
        dry_run = config.general.dry_run,
        clob_url = %config.general.clob_url,
        "loaded config"
    );

    // Parse token IDs from config
    let token_ids: Vec<U256> = config
        .market_selection
        .token_ids
        .iter()
        .map(|s| U256::from_str(s).context("parsing token_id"))
        .collect::<Result<Vec<_>>>()?;

    if token_ids.is_empty() && !config.copy_trader.enabled {
        bail!("no token_ids configured and copy_trader not enabled — set market_selection.token_ids in config.toml");
    }

    if !token_ids.is_empty() {
        info!(count = token_ids.len(), "subscribing to tokens");
    }

    // Create signer
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .context("POLYMARKET_PRIVATE_KEY env var required")?;
    let signer: PrivateKeySigner = private_key
        .parse::<PrivateKeySigner>()
        .context("invalid private key")?
        .with_chain_id(Some(POLYGON));
    let signer = Arc::new(signer);

    // Create authenticated CLOB client
    let clob_config = polymarket_client_sdk::clob::Config::builder()
        .use_server_time(config.general.use_server_time)
        .build();

    let signature_type = match config.general.signature_type.as_str() {
        "proxy" => Some(SignatureType::Proxy),
        "gnosis_safe" | "safe" => Some(SignatureType::GnosisSafe),
        _ => None, // "eoa" or default — no signature_type set
    };

    let mut auth_builder = polymarket_client_sdk::clob::Client::new(
        &config.general.clob_url,
        clob_config,
    )?
    .authentication_builder(&*signer);

    if let Some(sig_type) = signature_type {
        auth_builder = auth_builder.signature_type(sig_type);
        info!(signature_type = %config.general.signature_type, "using proxy wallet routing");
    }

    let clob_client = auth_builder
        .authenticate()
        .await
        .context("CLOB authentication failed")?;

    let clob_client = Arc::new(clob_client);
    info!("authenticated with CLOB API");

    // Create shared state
    let market_state = Arc::new(MarketState::new());

    // Sync positions from Polymarket data API instead of stale local file.
    // Query both EOA and proxy wallet to catch positions from before/after
    // GnosisSafe routing was enabled.
    let positions = Arc::new(PositionTracker::new());
    {
        // Only sync EOA positions (old pre-proxy trades like Oscar bets).
        // Bot-created proxy trades are tracked through WebSocket fills.
        let eoa = signer.address();
        let wallets = vec![eoa];
        match position::sync_positions_from_api(&wallets, &positions).await {
            Ok(count) => {
                if count > 0 {
                    info!(count, "synced positions from Polymarket API");
                } else {
                    info!("no open positions found on Polymarket API");
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to sync positions from API, starting fresh");
            }
        }
    }
    let order_manager = Arc::new(OrderManager::new(
        Arc::clone(&clob_client),
        Arc::clone(&signer),
        config.general.dry_run,
    ));
    let risk = RiskManager::new(config.risk.clone());
    let strategy = Box::new(MarketMakerStrategy::new(config.market_maker.clone()));

    // Engine channel
    let (tx, rx) = mpsc::channel::<EngineEvent>(1024);

    // Dashboard broadcast channel
    let (dashboard_tx, _) = broadcast::channel::<dashboard::DashboardUpdate>(256);

    // Create engine
    let mut engine = Engine::new(
        Arc::clone(&market_state),
        Arc::clone(&order_manager),
        Arc::clone(&positions),
        risk,
        strategy,
        rx,
        token_ids.clone(),
        dashboard_tx.clone(),
        config.general.dry_run,
    );

    // Spawn engine task
    let engine_task = tokio::spawn(async move {
        if let Err(e) = engine.run().await {
            error!(error = %e, "engine error");
        }
    });

    // Clone dashboard_tx before moving into DashboardState
    let dashboard_tx_copy = dashboard_tx.clone();

    // Spawn dashboard web server
    let dashboard_state = dashboard::DashboardState::new(
        Arc::clone(&market_state),
        Arc::clone(&order_manager),
        Arc::clone(&positions),
        dashboard_tx,
        token_ids.clone(),
        config.general.dry_run,
        config.risk.clone(),
    );
    let dashboard_port = config.general.dashboard_port;
    let dashboard_bind = config.general.dashboard_bind.clone();
    let dashboard_task = tokio::spawn(async move {
        if let Err(e) = dashboard::start(&dashboard_bind, dashboard_port, dashboard_state).await {
            error!(error = %e, "dashboard error");
        }
    });

    // Wallet scorer + copy tracker (if enabled)
    let (scorer_task, copy_task) = if config.copy_trader.enabled {
        let scored_wallets = Arc::new(RwLock::new(Vec::new()));

        // Only spawn wallet scorer when auto_discover is enabled
        let scorer_handle = if config.copy_trader.auto_discover {
            let scorer = wallet_scorer::WalletScorer::new(
                config.copy_trader.clone(),
                Arc::clone(&scored_wallets),
            );
            info!("wallet scorer enabled (auto_discover=true)");
            Some(tokio::spawn(async move { scorer.run().await }))
        } else {
            info!("wallet scorer disabled (auto_discover=false)");
            None
        };

        // Spawn copy tracker with shared scored wallets
        let copy_tracker = copy_tracker::CopyTracker::new(
            Arc::clone(&positions),
            Arc::clone(&market_state),
            config.copy_trader.clone(),
            tx.clone(),
            dashboard_tx_copy,
            Arc::clone(&scored_wallets),
            config.risk.initial_bankroll,
            config.risk.max_exposure_pct,
        );
        info!("copy trader enabled");
        let copy_handle = tokio::spawn(async move { copy_tracker.run().await });

        (scorer_handle, Some(copy_handle))
    } else {
        (None, None)
    };

    // Auto-redeemer: periodically redeem resolved markets for USDC.e
    // Also auto-sells positions trading at $0.99+ (near-certainty, no upside left)
    let proxy_addr: alloy::primitives::Address =
        "0x473a68e67807ddb2d62e7703f0ca2fd76903addc"
            .parse()
            .expect("valid proxy address");
    let redeemer = redeemer::Redeemer::new(
        Arc::clone(&signer),
        Arc::clone(&positions),
        Arc::clone(&order_manager),
        vec![signer.address(), proxy_addr],
        300, // check every 5 minutes
    );
    let redeemer_task = tokio::spawn(async move { redeemer.run().await });

    // BTC 5-minute trader (if enabled)
    let btc_task = if config.btc_trader.enabled {
        let btc = btc_trader::BtcTrader::new(
            config.btc_trader.clone(),
            Arc::clone(&order_manager),
            Arc::clone(&positions),
        );
        info!("BTC 5-min trader enabled");
        Some(tokio::spawn(async move { btc.run().await }))
    } else {
        None
    };

    // Orderbook WS (unauthenticated — orderbook is public data)
    let tx_book = tx.clone();
    let book_token_ids = token_ids.clone();

    let book_task = tokio::spawn(async move {
        if book_token_ids.is_empty() {
            info!("no market-maker tokens configured, skipping orderbook WS");
            // Keep task alive so join doesn't exit early
            std::future::pending::<()>().await;
            return;
        }
        let mut backoff_secs = 1u64;
        loop {
            let ws_book = polymarket_client_sdk::clob::ws::Client::default();
            let stream = match ws_book.subscribe_orderbook(book_token_ids.clone()) {
                Ok(s) => {
                    info!("orderbook WS connected");
                    backoff_secs = 1; // reset on success
                    s
                }
                Err(e) => {
                    warn!(error = %e, backoff = backoff_secs, "failed to subscribe to orderbook, retrying");
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };
            let mut stream = std::pin::pin!(stream);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(update) => {
                        if tx_book.send(EngineEvent::BookUpdate(update)).await.is_err() {
                            info!("engine channel closed, orderbook task exiting");
                            return;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "orderbook stream error");
                    }
                }
            }
            warn!(backoff = backoff_secs, "orderbook stream ended, reconnecting");
            tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(60);
        }
    });

    // User events WS (authenticated — requires API credentials)
    let tx_user = tx.clone();
    let signer_addr = signer.address();

    let user_task = tokio::spawn(async move {
        // Parse credentials once (deterministic, no need to redo on reconnect)
        let (key, secret, passphrase) = match (
            std::env::var("POLYMARKET_API_KEY"),
            std::env::var("POLYMARKET_API_SECRET"),
            std::env::var("POLYMARKET_API_PASSPHRASE"),
        ) {
            (Ok(k), Ok(s), Ok(p)) => (k, s, p),
            _ => {
                warn!("no API credentials — user events disabled");
                std::future::pending::<()>().await;
                return;
            }
        };

        let api_key = match uuid::Uuid::parse_str(&key) {
            Ok(k) => k,
            Err(e) => {
                error!(error = %e, "invalid POLYMARKET_API_KEY");
                return;
            }
        };

        let mut backoff_secs = 1u64;
        loop {
            let credentials = polymarket_client_sdk::auth::Credentials::new(
                api_key,
                secret.clone(),
                passphrase.clone(),
            );

            let ws_user = polymarket_client_sdk::clob::ws::Client::default();
            let ws_user = match ws_user.authenticate(credentials, signer_addr) {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, backoff = backoff_secs, "WS authentication failed, retrying");
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };

            let stream = match ws_user.subscribe_user_events(vec![]) {
                Ok(s) => {
                    info!("authenticated WebSocket for user events");
                    backoff_secs = 1; // reset on success
                    s
                }
                Err(e) => {
                    warn!(error = %e, backoff = backoff_secs, "failed to subscribe to user events, retrying");
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };
            let mut stream = std::pin::pin!(stream);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(WsMessage::Trade(trade)) => {
                        if tx_user
                            .send(EngineEvent::TradeConfirmed(trade))
                            .await
                            .is_err()
                        {
                            info!("engine channel closed, user events task exiting");
                            return;
                        }
                    }
                    Ok(WsMessage::Order(order)) => {
                        if tx_user
                            .send(EngineEvent::OrderUpdate(order))
                            .await
                            .is_err()
                        {
                            info!("engine channel closed, user events task exiting");
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!(error = %e, "user events stream error");
                    }
                }
            }
            warn!(backoff = backoff_secs, "user events stream ended, reconnecting");
            tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(60);
        }
    });

    // Tick timer
    let tx_tick = tx.clone();
    let tick_ms = config.general.tick_interval_ms;
    let tick_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(tick_ms));
        loop {
            interval.tick().await;
            if tx_tick.send(EngineEvent::Tick).await.is_err() {
                break;
            }
        }
    });

    // Wait for shutdown signal
    info!("bot running — press Ctrl+C to stop");
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl+c")?;

    info!("shutdown signal received");
    let _ = tx.send(EngineEvent::Shutdown).await;

    let _ = engine_task.await;
    book_task.abort();
    user_task.abort();
    tick_task.abort();
    dashboard_task.abort();
    redeemer_task.abort();
    if let Some(task) = copy_task {
        task.abort();
    }
    if let Some(task) = scorer_task {
        task.abort();
    }
    if let Some(task) = btc_task {
        task.abort();
    }

    info!("bot stopped");
    Ok(())
}
