//! Binance WebSocket trade stream for real-time price data.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::connect_async;
use tracing::{debug, error, info, warn};

use super::delta::DeltaComputer;
use super::types::{DeltaResult, FeedSource, FeedUpdate, OracleTick};

/// Binance trade message (partial).
#[derive(Deserialize)]
struct BinanceTrade {
    /// Trade price as string
    p: String,
    /// Trade time (milliseconds since epoch)
    #[serde(rename = "T")]
    trade_time: u64,
}

/// Binance WebSocket trade stream for a single asset.
pub struct BinanceFeed {
    asset: String,
    ws_url: String,
    delta_computer: Arc<RwLock<DeltaComputer>>,
    latest_price: Arc<RwLock<f64>>,
    connected: Arc<AtomicBool>,
    heartbeat_ts: Arc<RwLock<f64>>,
    update_tx: broadcast::Sender<FeedUpdate>,
}

impl BinanceFeed {
    pub fn new(asset: &str, base_ws_url: &str, update_tx: broadcast::Sender<FeedUpdate>) -> Self {
        Self {
            asset: asset.to_lowercase(),
            ws_url: format!("{}/{}usdt@trade", base_ws_url, asset.to_lowercase()),
            delta_computer: Arc::new(RwLock::new(DeltaComputer::new(120.0))),
            latest_price: Arc::new(RwLock::new(0.0)),
            connected: Arc::new(AtomicBool::new(false)),
            heartbeat_ts: Arc::new(RwLock::new(0.0)),
            update_tx,
        }
    }

    /// Main run loop — connects, reads messages, reconnects on failure.
    pub async fn run(&self) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            info!(asset = %self.asset, url = %self.ws_url, "connecting to binance WS");

            match connect_async(&self.ws_url).await {
                Ok((ws_stream, _response)) => {
                    self.connected.store(true, Ordering::Relaxed);
                    backoff_secs = 1;
                    info!(asset = %self.asset, "binance WS connected");

                    let (mut _write, mut read) = ws_stream.split();

                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(tungstenite::Message::Text(text)) => {
                                self.handle_message(&text);
                            }
                            Ok(tungstenite::Message::Ping(_data)) => {
                                debug!(asset = %self.asset, "binance ping received");
                                // tungstenite auto-responds with pong
                            }
                            Ok(tungstenite::Message::Close(_)) => {
                                warn!(asset = %self.asset, "binance WS closed by server");
                                break;
                            }
                            Err(e) => {
                                error!(asset = %self.asset, error = %e, "binance WS error");
                                break;
                            }
                            _ => {}
                        }
                    }

                    self.connected.store(false, Ordering::Relaxed);
                    warn!(asset = %self.asset, "binance WS disconnected");
                    let _ = self.update_tx.send(FeedUpdate::Disconnected(FeedSource::Binance));
                }
                Err(e) => {
                    error!(
                        asset = %self.asset,
                        error = %e,
                        backoff_secs = backoff_secs,
                        "binance WS connection failed"
                    );
                }
            }

            sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(max_backoff);
        }
    }

    fn handle_message(&self, text: &str) {
        let trade: BinanceTrade = match serde_json::from_str(text) {
            Ok(t) => t,
            Err(_) => return,
        };

        let price: f64 = match trade.p.parse() {
            Ok(p) => p,
            Err(_) => return,
        };

        let timestamp = trade.trade_time as f64 / 1000.0;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        *self.latest_price.write() = price;
        *self.heartbeat_ts.write() = now;
        self.delta_computer.write().add_tick(timestamp, price);

        let _ = self.update_tx.send(FeedUpdate::OracleTick(OracleTick {
            price,
            timestamp,
            source: FeedSource::Binance,
        }));
    }

    pub fn get_price(&self) -> f64 {
        *self.latest_price.read()
    }

    pub fn get_delta(&self, window_seconds: f64) -> DeltaResult {
        self.delta_computer.read().compute_delta(window_seconds)
    }

    pub fn get_heartbeat_age_ms(&self) -> f64 {
        let hb = *self.heartbeat_ts.read();
        if hb <= 0.0 {
            return -1.0;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        (now - hb) * 1000.0
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

use tokio_tungstenite::tungstenite;
