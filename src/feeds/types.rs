//! Data types for the market data feed system.

use serde::{Deserialize, Serialize};

/// Source of a price tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedSource {
    Binance,
    Coinbase,
}

/// A single price tick from an exchange.
#[derive(Debug, Clone)]
pub struct OracleTick {
    pub price: f64,
    pub timestamp: f64,
    pub source: FeedSource,
}

/// Result of computing a delta over a time window.
#[derive(Debug, Clone)]
pub struct DeltaResult {
    pub delta_bps: f64,
    pub available: bool,
    pub sample_age_ms: f64,
    pub window_start_ts: f64,
    pub window_end_ts: f64,
}

impl DeltaResult {
    pub fn unavailable() -> Self {
        Self {
            delta_bps: 0.0,
            available: false,
            sample_age_ms: -1.0,
            window_start_ts: 0.0,
            window_end_ts: 0.0,
        }
    }
}

/// Current feed health status.
#[derive(Debug, Clone, Serialize)]
pub struct FeedStatus {
    pub binance_connected: bool,
    pub coinbase_connected: bool,
    pub binance_last_tick_ms: f64,
    pub coinbase_last_tick_ms: f64,
}

/// Aggregated oracle snapshot at a point in time.
#[derive(Debug, Clone)]
pub struct OracleSnapshot {
    pub binance_price: f64,
    pub coinbase_price: f64,
    pub binance_delta_1s: DeltaResult,
    pub binance_delta_3s: DeltaResult,
    pub binance_delta_60s: DeltaResult,
    pub coinbase_delta_1s: DeltaResult,
    pub coinbase_delta_3s: DeltaResult,
    pub coinbase_delta_60s: DeltaResult,
    pub timestamp: f64,
}

/// A single orderbook price level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// Polymarket orderbook for a single token.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub token_id: String,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub timestamp: f64,
    pub tick_size: f64,
    pub min_order_size: f64,
}

impl OrderBook {
    pub fn mid(&self) -> f64 {
        let bid = self.best_bid();
        let ask = self.best_ask();
        if bid > 0.0 && ask > 0.0 {
            (bid + ask) / 2.0
        } else if ask > 0.0 {
            ask
        } else if bid > 0.0 {
            bid
        } else {
            0.5
        }
    }

    pub fn best_bid(&self) -> f64 {
        self.bids.first().map(|l| l.price).unwrap_or(0.0)
    }

    pub fn best_ask(&self) -> f64 {
        self.asks.first().map(|l| l.price).unwrap_or(0.0)
    }

    pub fn spread(&self) -> f64 {
        let bid = self.best_bid();
        let ask = self.best_ask();
        if bid > 0.0 && ask > 0.0 {
            ask - bid
        } else {
            0.0
        }
    }

    pub fn top_bid_size(&self) -> f64 {
        self.bids.first().map(|l| l.size).unwrap_or(0.0)
    }

    pub fn top_ask_size(&self) -> f64 {
        self.asks.first().map(|l| l.size).unwrap_or(0.0)
    }

    pub fn empty(token_id: String) -> Self {
        Self {
            token_id,
            bids: vec![],
            asks: vec![],
            timestamp: 0.0,
            tick_size: 0.01,
            min_order_size: 5.0,
        }
    }
}

/// Feed update messages for internal event channels.
#[derive(Debug, Clone)]
pub enum FeedUpdate {
    OracleTick(OracleTick),
    BookSnapshot(OrderBook),
    BookDelta {
        token_id: String,
        price: f64,
        size: f64,
        side: String,
    },
    Disconnected(FeedSource),
}
