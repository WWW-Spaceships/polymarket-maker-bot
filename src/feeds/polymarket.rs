//! Polymarket WebSocket orderbook stream.
//!
//! Protocol: wss://ws-subscriptions-clob.polymarket.com/ws/market
//! - Connect, then send subscribe messages for each asset_id
//! - Server sends "book" snapshots and "price_change" L2 deltas
//! - Must respond to server pings; also send our own keepalive pings

use std::sync::Arc;

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration, interval};
use tokio_tungstenite::{connect_async, tungstenite};
use tracing::{debug, error, info, warn};

use super::types::{BookLevel, FeedUpdate, OrderBook};

#[derive(Deserialize, Debug)]
struct WsMessage {
    event_type: Option<String>,
    asset_id: Option<String>,
    #[allow(dead_code)]
    market: Option<String>,
    bids: Option<Vec<WsBookLevel>>,
    asks: Option<Vec<WsBookLevel>>,
    #[allow(dead_code)]
    timestamp: Option<String>,
    price_changes: Option<Vec<WsPriceChange>>,
}

#[derive(Deserialize, Debug)]
struct WsBookLevel {
    price: String,
    size: String,
}

#[derive(Deserialize, Debug)]
struct WsPriceChange {
    asset_id: Option<String>,
    price: Option<String>,
    size: Option<String>,
    side: Option<String>,
}

/// Polymarket orderbook WebSocket feed.
pub struct PolymarketFeed {
    ws_url: String,
    books: Arc<DashMap<String, OrderBook>>,
    subscribed_tokens: Arc<RwLock<Vec<String>>>,
    update_tx: broadcast::Sender<FeedUpdate>,
}

impl PolymarketFeed {
    pub fn new(ws_url: &str, update_tx: broadcast::Sender<FeedUpdate>) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            books: Arc::new(DashMap::new()),
            subscribed_tokens: Arc::new(RwLock::new(Vec::new())),
            update_tx,
        }
    }

    pub async fn run(&self) {
        let mut backoff_secs = 1u64;
        let max_backoff = 60u64;

        loop {
            // Don't connect if nothing to subscribe to — just wait
            let tokens = self.subscribed_tokens.read().clone();
            if tokens.is_empty() {
                debug!("polymarket WS: no tokens subscribed, waiting...");
                sleep(Duration::from_secs(5)).await;
                continue;
            }

            info!(url = %self.ws_url, token_count = tokens.len(), "connecting to polymarket WS");

            match connect_async(&self.ws_url).await {
                Ok((ws_stream, _)) => {
                    backoff_secs = 1;
                    info!("polymarket WS connected");

                    let (mut write, mut read) = ws_stream.split();

                    // Subscribe to all tracked tokens
                    for token_id in &tokens {
                        let sub_msg = serde_json::json!({
                            "auth": {},
                            "markets": [token_id],
                            "assets_ids": [token_id],
                            "type": "market"
                        });
                        if let Err(e) = write.send(tungstenite::Message::Text(sub_msg.to_string())).await {
                            warn!(error = %e, "failed to send subscribe message");
                            break;
                        }
                        debug!(token_id, "subscribed to PM orderbook");
                    }

                    // Keepalive ping interval
                    let mut ping_interval = interval(Duration::from_secs(10));
                    // Skip the immediate first tick
                    ping_interval.tick().await;

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(tungstenite::Message::Text(text))) => {
                                        self.handle_message(&text);
                                    }
                                    Some(Ok(tungstenite::Message::Ping(data))) => {
                                        debug!("polymarket ping received");
                                        let _ = write.send(tungstenite::Message::Pong(data)).await;
                                    }
                                    Some(Ok(tungstenite::Message::Close(_))) => {
                                        warn!("polymarket WS closed by server");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        error!(error = %e, "polymarket WS error");
                                        break;
                                    }
                                    None => {
                                        warn!("polymarket WS stream ended");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            _ = ping_interval.tick() => {
                                // Send WebSocket-level ping frame (not text)
                                if let Err(e) = write.send(tungstenite::Message::Ping(vec![])).await {
                                    warn!(error = %e, "failed to send WS ping");
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, backoff_secs, "polymarket WS connection failed");
                }
            }

            sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(max_backoff);
        }
    }

    fn handle_message(&self, text: &str) {
        // Some servers send literal text pong
        if text == "PONG" || text == "pong" {
            return;
        }

        // Try to parse as array first (PM sometimes sends arrays)
        if text.starts_with('[') {
            if let Ok(msgs) = serde_json::from_str::<Vec<WsMessage>>(text) {
                for msg in msgs {
                    self.process_message(msg);
                }
                return;
            }
        }

        let msg: WsMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, text_len = text.len(), "failed to parse PM WS message");
                return;
            }
        };

        self.process_message(msg);
    }

    fn process_message(&self, msg: WsMessage) {
        let event_type = msg.event_type.as_deref().unwrap_or("");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        match event_type {
            "book" => {
                if let Some(asset_id) = &msg.asset_id {
                    let bids: Vec<BookLevel> = msg
                        .bids
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|l| {
                            Some(BookLevel {
                                price: l.price.parse().ok()?,
                                size: l.size.parse().ok()?,
                            })
                        })
                        .collect();

                    let asks: Vec<BookLevel> = msg
                        .asks
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|l| {
                            Some(BookLevel {
                                price: l.price.parse().ok()?,
                                size: l.size.parse().ok()?,
                            })
                        })
                        .collect();

                    debug!(
                        asset_id,
                        bid_levels = bids.len(),
                        ask_levels = asks.len(),
                        "PM book snapshot"
                    );

                    let book = OrderBook {
                        token_id: asset_id.clone(),
                        bids,
                        asks,
                        timestamp: now,
                        tick_size: 0.01,
                        min_order_size: 5.0,
                    };

                    let _ = self.update_tx.send(FeedUpdate::BookSnapshot(book.clone()));
                    self.books.insert(asset_id.clone(), book);
                }
            }
            "price_change" => {
                if let Some(changes) = msg.price_changes {
                    for change in changes {
                        let asset_id = match change.asset_id {
                            Some(id) => id,
                            None => continue,
                        };
                        let price: f64 = match change.price.and_then(|p| p.parse().ok()) {
                            Some(p) => p,
                            None => continue,
                        };
                        let size: f64 = match change.size.and_then(|s| s.parse().ok()) {
                            Some(s) => s,
                            None => continue,
                        };
                        let side = change.side.unwrap_or_default();

                        if let Some(mut book) = self.books.get_mut(&asset_id) {
                            let levels = if side == "BUY" {
                                &mut book.bids
                            } else {
                                &mut book.asks
                            };

                            if size == 0.0 {
                                levels.retain(|l| (l.price - price).abs() > 0.00001);
                            } else {
                                if let Some(lvl) = levels.iter_mut().find(|l| (l.price - price).abs() < 0.00001) {
                                    lvl.size = size;
                                } else {
                                    levels.push(BookLevel { price, size });
                                    if side == "BUY" {
                                        levels.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
                                    } else {
                                        levels.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
                                    }
                                }
                            }
                            book.timestamp = now;
                        }

                        let _ = self.update_tx.send(FeedUpdate::BookDelta {
                            token_id: asset_id,
                            price,
                            size,
                            side,
                        });
                    }
                }
            }
            _ => {
                debug!(event_type, "unknown PM WS event type");
            }
        }
    }

    /// Subscribe to token orderbook updates.
    pub async fn subscribe(&self, token_ids: Vec<String>) {
        let mut tokens = self.subscribed_tokens.write();
        for tid in &token_ids {
            if !tokens.contains(tid) {
                tokens.push(tid.clone());
            }
        }
        info!(tokens = ?token_ids, "queued polymarket token subscriptions");
    }

    /// Unsubscribe from token orderbook updates.
    pub async fn unsubscribe(&self, token_ids: Vec<String>) {
        let mut tokens = self.subscribed_tokens.write();
        tokens.retain(|t| !token_ids.contains(t));
        for tid in &token_ids {
            self.books.remove(tid);
        }
        info!(tokens = ?token_ids, "unsubscribed from polymarket tokens");
    }

    /// Get current orderbook for a token.
    pub fn get_book(&self, token_id: &str) -> Option<OrderBook> {
        self.books.get(token_id).map(|b| b.clone())
    }
}
