//! Axum HTTP dashboard server.

use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use tracing::info;

use crate::config::WebConfig;
use crate::executor::order_manager::OrderManager;
use crate::feeds::FeedHub;
use crate::position::tracker::PositionTracker;

use super::routes;

/// Shared state for all web routes.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub position_tracker: Arc<PositionTracker>,
    pub order_manager: Arc<OrderManager>,
    pub feed_hub: Arc<FeedHub>,
}

/// Axum web server for the dashboard.
pub struct WebServer {
    config: WebConfig,
    state: AppState,
}

impl WebServer {
    pub fn new(
        config: WebConfig,
        db: PgPool,
        position_tracker: Arc<PositionTracker>,
        order_manager: Arc<OrderManager>,
        feed_hub: Arc<FeedHub>,
    ) -> Self {
        Self {
            config,
            state: AppState {
                db,
                position_tracker,
                order_manager,
                feed_hub,
            },
        }
    }

    /// Start the HTTP server.
    pub async fn start(self) -> anyhow::Result<()> {
        let app = Router::new()
            .merge(routes::api_routes())
            .with_state(self.state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.config.port));
        info!(port = self.config.port, "web dashboard starting");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}
