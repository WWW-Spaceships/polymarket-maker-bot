//! Database row types for all tables.

use serde::Serialize;
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbMarket {
    pub id: i64,
    pub condition_id: String,
    pub asset: String,
    pub timeframe: String,
    pub start_time: f64,
    pub end_time: f64,
    pub resolved: bool,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbFeature {
    pub id: i64,
    pub market_id: i64,
    pub timestamp: f64,
    pub pm_mid_price: Option<f64>,
    pub pm_best_bid: Option<f64>,
    pub pm_best_ask: Option<f64>,
    pub pm_spread: Option<f64>,
    pub pm_top_bid_size: Option<f64>,
    pub pm_top_ask_size: Option<f64>,
    pub external_price: Option<f64>,
    pub external_return_5s: Option<f64>,
    pub external_return_10s: Option<f64>,
    pub pm_implied_prob: Option<f64>,
    pub price_dislocation: Option<f64>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbTrade {
    pub id: i64,
    pub market_id: i64,
    pub condition_id: String,
    pub side: String,
    pub entry_price: f64,
    pub size: f64,
    pub entry_time: f64,
    pub exit_price: Option<f64>,
    pub exit_time: Option<f64>,
    pub pnl: Option<f64>,
    pub edge_at_entry: Option<f64>,
    pub direction: Option<String>,
    pub dislocation_bps: Option<f64>,
    pub tier: Option<i32>,
    pub order_id: Option<String>,
    pub token_id: Option<String>,
    pub signal_type: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbResolution {
    pub id: i64,
    pub market_id: i64,
    pub condition_id: String,
    pub outcome: String,
    pub final_price: f64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbSignal {
    pub id: i64,
    pub market_id: Option<i64>,
    pub timestamp: f64,
    pub binance_price: Option<f64>,
    pub coinbase_price: Option<f64>,
    pub binance_delta_1s: Option<f64>,
    pub coinbase_delta_1s: Option<f64>,
    pub binance_delta_3s: Option<f64>,
    pub coinbase_delta_3s: Option<f64>,
    pub poly_mid: Option<f64>,
    pub seconds_left: Option<f64>,
    pub direction: Option<String>,
    pub signal_strength: Option<f64>,
    pub traded: Option<bool>,
    pub result: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbEvent {
    pub id: i64,
    pub ts: f64,
    pub event_type: String,
    pub market_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbMakerOrder {
    pub id: i64,
    pub order_id: String,
    pub market_id: i64,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub order_type: String,
    pub status: String,
    pub posted_at: f64,
    pub filled_at: Option<f64>,
    pub cancelled_at: Option<f64>,
    pub fill_price: Option<f64>,
    pub fill_size: Option<f64>,
    pub cancel_reason: Option<String>,
    pub strategy: String,
    pub quote_cycle_id: Option<i64>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbRebate {
    pub id: i64,
    pub date: chrono::NaiveDate,
    pub amount_usdc: f64,
    pub volume_usdc: Option<f64>,
    pub recorded_at: f64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DbInventory {
    pub id: i64,
    pub token_id: String,
    pub condition_id: String,
    pub side: String,
    pub shares: f64,
    pub avg_entry_price: f64,
    pub updated_at: f64,
}
