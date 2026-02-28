//! Polymarket Maker Bot — Entry Point
//!
//! Loads configuration, initializes all subsystems, and runs the main event loop.
//! Handles graceful shutdown on SIGINT/SIGTERM.
//! Health endpoint starts immediately so Railway doesn't restart-loop.

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
async fn main() {
    // Print immediately so Railway logs show SOMETHING
    eprintln!("polymarket-maker-bot process started");

    // Load .env file (ignore if missing)
    let _ = dotenvy::dotenv();

    // Debug: print env var presence
    eprintln!(
        "env check: DATABASE_URL={}, PRIVATE_KEY={}, CLOB_API_KEY={}, CLOB_SECRET={}, CLOB_PASSPHRASE={}, PORT={}",
        if std::env::var("DATABASE_URL").is_ok() { "set" } else { "MISSING" },
        if std::env::var("PRIVATE_KEY").is_ok() { "set" } else { "MISSING" },
        if std::env::var("CLOB_API_KEY").is_ok() { "set" } else { "MISSING" },
        if std::env::var("CLOB_SECRET").is_ok() { "set" } else { "MISSING" },
        if std::env::var("CLOB_PASSPHRASE").is_ok() { "set" } else { "MISSING" },
        std::env::var("PORT").unwrap_or_else(|_| "MISSING".into()),
    );

    // Load configuration
    let config = match Config::load() {
        Ok(c) => {
            eprintln!("config loaded OK");
            c
        }
        Err(e) => {
            eprintln!("FATAL: config load failed: {e}");
            wait_for_shutdown().await;
            return;
        }
    };

    // Initialize logging
    logging::structured::init_logging(&config.logging);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        trading_enabled = config.trading.enabled,
        paper_mode = config.trading.paper_mode,
        "polymarket-maker-bot starting"
    );

    // Initialize database (retry a few times — Railway PG may still be booting)
    let db_pool = match connect_db_with_retry(&config.database.url, 5).await {
        Some(p) => p,
        None => {
            error!("database connection failed after retries — check DATABASE_URL");
            wait_for_shutdown().await;
            return;
        }
    };

    match pool::run_migrations(&db_pool).await {
        Ok(_) => info!("database migrations applied"),
        Err(e) => {
            error!(error = %e, "database migrations failed");
            wait_for_shutdown().await;
            return;
        }
    }

    // Initialize event bus
    let event_bus = Arc::new(EventBus::new(1024));

    // Initialize feeds
    let feed_hub = Arc::new(feeds::FeedHub::new(&config.feeds, event_bus.clone()));

    // Initialize position tracker
    let position_tracker = Arc::new(position::tracker::PositionTracker::new(db_pool.clone()));

    // Initialize risk manager
    let risk_manager = Arc::new(strategy::risk::RiskManager::new(config.risk.clone(), event_bus.clone()));

    // Initialize fee calculator (awareness only — makers pay 0)
    let _fee_calc = fees::calculator::FeeCalculator::new();

    // Initialize pricing model
    let pricer = Arc::new(strategy::pricing::QuotePricer::new(
        config.strategy.clone(),
        None,
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

    // Initialize order manager (now async — authenticates with CLOB via SDK)
    let order_manager = Arc::new(
        executor::order_manager::OrderManager::new(
            &config.clob.base_url,
            &config.auth,
            db_pool.clone(),
            event_bus.clone(),
        )
        .await
        .expect("failed to initialize order manager — check CLOB credentials"),
    );

    // Initialize market discovery
    let discovery = Arc::new(discovery::gamma::GammaDiscovery::new(
        config.discovery.clone(),
        config.clob.gamma_base_url.clone(),
        db_pool.clone(),
    ));

    // Spawn feed tasks
    let feed_hub_clone = feed_hub.clone();
    tokio::spawn(async move {
        feed_hub_clone.start().await;
    });

    // Spawn discovery loop
    let discovery_clone = discovery.clone();
    let feed_hub_for_disc = feed_hub.clone();
    let strategy_engine_disc = strategy_engine.clone();
    let order_mgr_disc = order_manager.clone();
    let risk_disc = risk_manager.clone();
    let pos_disc = position_tracker.clone();
    let event_bus_disc = event_bus.clone();
    let db_disc = db_pool.clone();
    let config_disc = config.clone();
    tokio::spawn(async move {
        discovery::gamma::run_discovery_loop(
            discovery_clone,
            feed_hub_for_disc,
            strategy_engine_disc,
            order_mgr_disc,
            risk_disc,
            pos_disc,
            event_bus_disc,
            db_disc,
            config_disc,
        )
        .await;
    });

    // Spawn Telegram bot (if configured)
    if config.telegram.bot_token.is_some() {
        let tg_state = Arc::new(telegram::bot::TelegramState {
            position_tracker: position_tracker.clone(),
            order_manager: order_manager.clone(),
            feed_hub: feed_hub.clone(),
            discovery: discovery.clone(),
            risk_manager: risk_manager.clone(),
        });
        let tg = telegram::bot::TelegramBot::new(
            config.telegram.clone(),
            event_bus.subscribe(),
            tg_state,
        );
        tokio::spawn(async move {
            if let Err(e) = tg.run().await {
                error!(error = %e, "telegram bot error");
            }
        });
    }

    // Spawn full web dashboard (on a different port won't conflict with health server)
    // The health server on PORT handles Railway, dashboard runs alongside
    if config.web.enabled {
        let web_server = web::server::WebServer::new(
            config.web.clone(),
            db_pool.clone(),
            position_tracker.clone(),
            order_manager.clone(),
            feed_hub.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = web_server.start().await {
                error!(error = %e, "web server error");
            }
        });
    }

    info!("all subsystems started, waiting for shutdown signal");

    // Wait for shutdown signal
    wait_for_shutdown().await;

    // Graceful shutdown
    warn!("shutting down — cancelling all orders");
    if let Err(e) = order_manager.cancel_all().await {
        error!(error = %e, "failed to cancel orders during shutdown");
    }

    info!("shutdown complete");
}

/// Minimal /health server — starts instantly for Railway health checks.
async fn run_health_server(port: u16) {
    use axum::{routing::get, Router};

    let app = Router::new()
        .route("/health", get(|| async { "ok" }));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    eprintln!("health server listening on port {port}");

    if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
        let _ = axum::serve(listener, app).await;
    }
}

/// Try connecting to Postgres with retries (Railway PG may take a moment).
async fn connect_db_with_retry(url: &str, max_attempts: u32) -> Option<sqlx::PgPool> {
    for attempt in 1..=max_attempts {
        match pool::create_pool(url).await {
            Ok(p) => {
                info!(attempt, "database connected");
                return Some(p);
            }
            Err(e) => {
                warn!(attempt, max_attempts, error = %e, "database connection failed, retrying...");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    None
}

/// Wait for SIGINT or SIGTERM.
async fn wait_for_shutdown() {
    let ctrl_c = signal::ctrl_c();
    #[cfg(unix)]
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    tokio::select! {
        _ = ctrl_c => { info!("received SIGINT"); }
        _ = sigterm.recv() => { info!("received SIGTERM"); }
    }
}
