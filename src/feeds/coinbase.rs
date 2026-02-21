//! Coinbase WebSocket match stream for real-time price data.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite};
use tracing::{debug, error, info, warn};

use super::delta::DeltaComputer;
use super::types::{DeltaResult, FeedSource, FeedUpdate, OracleTick};

#[derive(Deserialize)]
struct CoinbaseMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    price: Option<String>,
    #[serde(default)]
    time: Option<String>,
}

/// Coinbase WebSocket match stream for a single product.
pub struct CoinbaseFeed {
    product_id: String,
    ws_url: String,
    delta_computer: Arc<RwLock<DeltaComputer>>,
    latest_price: Arc<RwLock<f64>>,
    connected: Arc<AtomicBool>,
    heartbeat_ts: Arc<RwLock<f64>>,
    update_tx: broadcast::Sender<FeedUpdate>,
}

impl CoinbaseFeed {
    pub fn new(
        product_id: &str,
        ws_url: &str,
        update_tx: broadcast::Sender<FeedUpdate>,
    ) -> Self {
        Self {
            product_id: product_id.to_string(),
            ws_url: ws_url.to_string(),
            delta_computer: Arc::new(RwLock::new(DeltaComputer::new(120.0))),
            latest_price: Arc::new(RwLock::new(0.0)),
            connected: Arc::new(AtomicBool::new(false)),
            heartbeat_ts: Arc::new(RwLock::new(0.0)),
            update_tx,
        }
    }

    pub async fn run(&self) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            info!(product = %self.product_id, "connecting to coinbase WS");

            match connect_async(&self.ws_url).await {
                Ok((ws_stream, _)) => {
                    self.connected.store(true, Ordering::Relaxed);
                    backoff_secs = 1;
                    info!(product = %self.product_id, "coinbase WS connected");

                    let (mut write, mut read) = ws_stream.split();

                    // Send subscription
                    let subscribe = serde_json::json!({
                        "type": "subscribe",
                        "channels": [{
                            "name": "matches",
                            "product_ids": [self.product_id]
                        }]
                    });

                    if let Err(e) = write
                        .send(tungstenite::Message::Text(subscribe.to_string()))
                        .await
                    {
                        error!(error = %e, "failed to subscribe to coinbase");
                        self.connected.store(false, Ordering::Relaxed);
                        sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(max_backoff);
                        continue;
                    }

                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(tungstenite::Message::Text(text)) => {
                                self.handle_message(&text);
                            }
                            Ok(tungstenite::Message::Ping(_)) => {
                                debug!(product = %self.product_id, "coinbase ping");
                            }
                            Ok(tungstenite::Message::Close(_)) => {
                                warn!(product = %self.product_id, "coinbase WS closed");
                                break;
                            }
                            Err(e) => {
                                error!(product = %self.product_id, error = %e, "coinbase WS error");
                                break;
                            }
                            _ => {}
                        }
                    }

                    self.connected.store(false, Ordering::Relaxed);
                    let _ = self.update_tx.send(FeedUpdate::Disconnected(FeedSource::Coinbase));
                }
                Err(e) => {
                    error!(
                        product = %self.product_id,
                        error = %e,
                        backoff_secs,
                        "coinbase WS connection failed"
                    );
                }
            }

            sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(max_backoff);
        }
    }

    fn handle_message(&self, text: &str) {
        let msg: CoinbaseMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(_) => return,
        };

        // Only process match messages
        if msg.msg_type != "match" && msg.msg_type != "last_match" {
            return;
        }

        let price: f64 = match msg.price.as_deref().and_then(|p| p.parse().ok()) {
            Some(p) => p,
            None => return,
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        // Parse ISO timestamp if available, otherwise use wall clock
        let timestamp = msg
            .time
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_millis() as f64 / 1000.0)
            .unwrap_or(now);

        *self.latest_price.write() = price;
        *self.heartbeat_ts.write() = now;
        self.delta_computer.write().add_tick(timestamp, price);

        let _ = self.update_tx.send(FeedUpdate::OracleTick(OracleTick {
            price,
            timestamp,
            source: FeedSource::Coinbase,
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
