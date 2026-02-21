pub mod binance;
pub mod coinbase;
pub mod delta;
pub mod polymarket;
pub mod types;

use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::info;

use crate::config::FeedConfig;
use crate::events::bus::EventBus;
use types::{FeedStatus, FeedUpdate, OracleSnapshot, OrderBook};

/// Coordinates all market data feeds.
pub struct FeedHub {
    pub binance_btc: Arc<binance::BinanceFeed>,
    pub binance_eth: Arc<binance::BinanceFeed>,
    pub coinbase_btc: Arc<coinbase::CoinbaseFeed>,
    pub coinbase_eth: Arc<coinbase::CoinbaseFeed>,
    pub polymarket: Arc<polymarket::PolymarketFeed>,
    event_bus: Arc<EventBus>,
}

impl FeedHub {
    pub fn new(config: &FeedConfig, event_bus: Arc<EventBus>) -> Self {
        let (update_tx, _) = broadcast::channel::<FeedUpdate>(4096);

        Self {
            binance_btc: Arc::new(binance::BinanceFeed::new("btc", &config.binance_ws_url, update_tx.clone())),
            binance_eth: Arc::new(binance::BinanceFeed::new("eth", &config.binance_ws_url, update_tx.clone())),
            coinbase_btc: Arc::new(coinbase::CoinbaseFeed::new("BTC-USD", &config.coinbase_ws_url, update_tx.clone())),
            coinbase_eth: Arc::new(coinbase::CoinbaseFeed::new("ETH-USD", &config.coinbase_ws_url, update_tx.clone())),
            polymarket: Arc::new(polymarket::PolymarketFeed::new(&config.polymarket_ws_url, update_tx)),
            event_bus,
        }
    }

    /// Spawn all feed tasks.
    pub async fn start(&self) {
        info!("starting all market data feeds");

        let bn_btc = self.binance_btc.clone();
        tokio::spawn(async move { bn_btc.run().await });

        let bn_eth = self.binance_eth.clone();
        tokio::spawn(async move { bn_eth.run().await });

        let cb_btc = self.coinbase_btc.clone();
        tokio::spawn(async move { cb_btc.run().await });

        let cb_eth = self.coinbase_eth.clone();
        tokio::spawn(async move { cb_eth.run().await });

        let pm = self.polymarket.clone();
        tokio::spawn(async move { pm.run().await });

        info!("all feeds spawned");
    }

    /// Get aggregated oracle snapshot for a given asset.
    pub fn oracle_snapshot(&self, asset: &str) -> OracleSnapshot {
        let (bn, cb) = match asset.to_lowercase().as_str() {
            "btc" => (&self.binance_btc, &self.coinbase_btc),
            "eth" => (&self.binance_eth, &self.coinbase_eth),
            _ => (&self.binance_btc, &self.coinbase_btc),
        };

        OracleSnapshot {
            binance_price: bn.get_price(),
            coinbase_price: cb.get_price(),
            binance_delta_1s: bn.get_delta(1.0),
            binance_delta_3s: bn.get_delta(3.0),
            binance_delta_60s: bn.get_delta(60.0),
            coinbase_delta_1s: cb.get_delta(1.0),
            coinbase_delta_3s: cb.get_delta(3.0),
            coinbase_delta_60s: cb.get_delta(60.0),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        }
    }

    /// Get orderbook for a specific token.
    pub fn get_book(&self, token_id: &str) -> Option<OrderBook> {
        self.polymarket.get_book(token_id)
    }

    /// Subscribe to orderbook updates for a market's tokens.
    pub async fn subscribe_market(&self, yes_token_id: &str, no_token_id: &str) {
        self.polymarket
            .subscribe(vec![yes_token_id.to_string(), no_token_id.to_string()])
            .await;
    }

    /// Unsubscribe from a market's tokens.
    pub async fn unsubscribe_market(&self, yes_token_id: &str, no_token_id: &str) {
        self.polymarket
            .unsubscribe(vec![yes_token_id.to_string(), no_token_id.to_string()])
            .await;
    }

    /// Get feed health status.
    pub fn feed_status(&self) -> FeedStatus {
        FeedStatus {
            binance_connected: self.binance_btc.is_connected(),
            coinbase_connected: self.coinbase_btc.is_connected(),
            binance_last_tick_ms: self.binance_btc.get_heartbeat_age_ms(),
            coinbase_last_tick_ms: self.coinbase_btc.get_heartbeat_age_ms(),
        }
    }
}
