//! Polymarket Maker Bot — Entry Point
//!
//! Loads configuration, initializes all subsystems, and runs the main event loop.
//! Handles graceful shutdown on SIGINT/SIGTERM.

mod config;
mod db;
mod discovery;
mod error;
mod events;
mod executor;
mod feeds;
mod fees;
mod logging;
mod position;
mod strategy;
mod telegram;
mod web;

use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::pool;
use crate::events::bus::EventBus;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file (ignore if missing)
    let _ = dotenvy::dotenv();

    // Load configuration
    let config = Config::load()?;

    // Initialize logging
    logging::structured::init_logging(&config.logging);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        trading_enabled = config.trading.enabled,
        paper_mode = config.trading.paper_mode,
        "polymarket-maker-bot starting"
    );

    // Initialize database
    let db_pool = pool::create_pool(&config.database.url).await?;
    pool::run_migrations(&db_pool).await?;
    info!("database connected and migrations applied");

    // Initialize event bus
    let event_bus = Arc::new(EventBus::new(1024));

    // Initialize feeds
    let feed_hub = Arc::new(feeds::FeedHub::new(&config.feeds, event_bus.clone()));

    // Initialize position tracker
    let position_tracker = Arc::new(position::tracker::PositionTracker::new(db_pool.clone()));

    // Initialize risk manager
    let risk_manager = Arc::new(strategy::risk::RiskManager::new(config.risk.clone()));

    // Initialize fee calculator (awareness only — makers pay 0)
    let _fee_calc = fees::calculator::FeeCalculator::new();

    // Initialize pricing model
    let pricer = Arc::new(strategy::pricing::QuotePricer::new(
        config.strategy.clone(),
        None, // bayesian weights loaded from DB later
    ));

    // Initialize article strategy
    let article = Arc::new(strategy::article::ArticleStrategy::new(
        config.article.clone(),
    ));

    // Initialize strategy engine
    let strategy_engine = Arc::new(strategy::engine::StrategyEngine::new(
        pricer.clone(),
        article.clone(),
        risk_manager.clone(),
    ));

    // Initialize order manager
    let order_manager = Arc::new(executor::order_manager::OrderManager::new(
        config.clob.base_url.clone(),
        config.auth.clone(),
        db_pool.clone(),
    ));

    // Initialize market discovery
    let discovery = Arc::new(discovery::gamma::GammaDiscovery::new(
        config.discovery.clone(),
        config.clob.gamma_base_url.clone(),
        db_pool.clone(),
    ));

    // Spawn feed tasks
    let feed_hub_clone = feed_hub.clone();
    let _feed_handle = tokio::spawn(async move {
        feed_hub_clone.start().await;
    });

    // Spawn discovery loop
    let discovery_clone = discovery.clone();
    let feed_hub_for_disc = feed_hub.clone();
    let strategy_engine_disc = strategy_engine.clone();
    let order_mgr_disc = order_manager.clone();
    let risk_disc = risk_manager.clone();
    let pos_disc = position_tracker.clone();
    let db_disc = db_pool.clone();
    let config_disc = config.clone();
    let _discovery_handle = tokio::spawn(async move {
        discovery::gamma::run_discovery_loop(
            discovery_clone,
            feed_hub_for_disc,
            strategy_engine_disc,
            order_mgr_disc,
            risk_disc,
            pos_disc,
            db_disc,
            config_disc,
        )
        .await;
    });

    // Spawn Telegram bot (if configured)
    let _telegram_handle = if config.telegram.bot_token.is_some() {
        let tg = telegram::bot::TelegramBot::new(
            config.telegram.clone(),
            event_bus.subscribe(),
        );
        Some(tokio::spawn(async move {
            if let Err(e) = tg.run().await {
                error!(error = %e, "telegram bot error");
            }
        }))
    } else {
        None
    };

    // Spawn web dashboard (if enabled)
    let _web_handle = if config.web.enabled {
        let web_server = web::server::WebServer::new(
            config.web.clone(),
            db_pool.clone(),
            position_tracker.clone(),
            order_manager.clone(),
            feed_hub.clone(),
        );
        Some(tokio::spawn(async move {
            if let Err(e) = web_server.start().await {
                error!(error = %e, "web server error");
            }
        }))
    } else {
        None
    };

    info!("all subsystems started, waiting for shutdown signal");

    // Wait for shutdown signal
    let shutdown = async {
        let ctrl_c = signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => { info!("received SIGINT"); }
            _ = sigterm.recv() => { info!("received SIGTERM"); }
        }
    };

    shutdown.await;

    // Graceful shutdown
    warn!("shutting down — cancelling all orders");
    if let Err(e) = order_manager.cancel_all().await {
        error!(error = %e, "failed to cancel orders during shutdown");
    }

    info!("shutdown complete");
    Ok(())
}
