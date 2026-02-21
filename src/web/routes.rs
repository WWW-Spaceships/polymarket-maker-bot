//! HTTP route handlers for the web dashboard.

use axum::{
    extract::State,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::db::queries;

use super::server::AppState;

/// Build all API routes.
pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/positions", get(positions))
        .route("/api/orders", get(orders))
        .route("/api/trades", get(trades))
        .route("/api/signals", get(signals))
        .route("/api/feeds", get(feeds))
        .route("/health", get(health))
}

/// GET /api/status — overall bot status.
async fn status(State(state): State<AppState>) -> Json<Value> {
    let feed_status = state.feed_hub.feed_status();
    let live_orders = state.order_manager.live_order_count();
    let positions = state.position_tracker.position_count();
    let exposure = state.position_tracker.get_total_exposure_usd();

    Json(json!({
        "status": "running",
        "live_orders": live_orders,
        "open_positions": positions,
        "total_exposure_usd": exposure,
        "feeds": feed_status,
    }))
}

/// GET /api/positions — current inventory.
async fn positions(State(state): State<AppState>) -> Json<Value> {
    let pos = state.position_tracker.all_positions();
    Json(json!({ "positions": pos }))
}

/// GET /api/orders — live orders.
async fn orders(State(state): State<AppState>) -> Json<Value> {
    let live: Vec<_> = state
        .order_manager
        .live_orders
        .iter()
        .map(|e| e.value().clone())
        .collect();
    Json(json!({ "live_orders": live }))
}

/// GET /api/trades — recent trades from DB.
async fn trades(State(state): State<AppState>) -> Json<Value> {
    match queries::get_recent_trades(&state.db, 50).await {
        Ok(rows) => Json(json!({ "trades": rows })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /api/signals — recent signals from DB.
async fn signals(State(state): State<AppState>) -> Json<Value> {
    match queries::get_recent_signals(&state.db, 50).await {
        Ok(rows) => Json(json!({ "signals": rows })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /api/feeds — feed health status.
async fn feeds(State(state): State<AppState>) -> Json<Value> {
    let status = state.feed_hub.feed_status();
    Json(json!({ "feeds": status }))
}

/// GET /health — simple health check.
async fn health() -> &'static str {
    "ok"
}
