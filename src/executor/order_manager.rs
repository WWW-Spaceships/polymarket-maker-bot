//! Order lifecycle management — post, cancel, reconcile maker orders.
//!
//! Uses HMAC-authenticated REST calls to the Polymarket CLOB API.
//! Tracks live orders in a DashMap for O(1) lookup by the hot loop.

use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::config::AuthConfig;
use crate::db::queries;
use crate::events::bus::{BotEvent, EventBus};

/// A live order tracked in memory.
#[derive(Debug, Clone, Serialize)]
pub struct LiveOrder {
    pub order_id: String,
    pub token_id: String,
    pub condition_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub strategy: String,
    pub posted_at: f64,
    pub market_db_id: i64,
}

/// Response from the CLOB order placement endpoint.
#[derive(Debug, Deserialize)]
pub struct OrderResponse {
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    pub success: Option<bool>,
    #[serde(rename = "errorMsg")]
    pub error_msg: Option<String>,
    pub status: Option<String>,
}

/// Response from the CLOB cancel endpoint.
#[derive(Debug, Deserialize)]
pub struct CancelResponse {
    pub canceled: Option<Vec<String>>,
    pub not_canceled: Option<serde_json::Value>,
}

/// Manages the full lifecycle of maker orders.
pub struct OrderManager {
    client: Client,
    base_url: String,
    auth: AuthConfig,
    db: PgPool,
    event_bus: Arc<EventBus>,
    /// In-memory tracking of all live (posted, not yet filled/cancelled) orders.
    pub live_orders: Arc<DashMap<String, LiveOrder>>,
}

impl OrderManager {
    pub fn new(base_url: String, auth: AuthConfig, db: PgPool, event_bus: Arc<EventBus>) -> Self {
        Self {
            client: Client::new(),
            base_url,
            auth,
            db,
            event_bus,
            live_orders: Arc::new(DashMap::new()),
        }
    }

    /// Post a GTC limit order to the CLOB.
    pub async fn post_order(
        &self,
        token_id: &str,
        condition_id: &str,
        side: &str,
        price: f64,
        size: f64,
        strategy: &str,
        market_db_id: i64,
        paper_mode: bool,
    ) -> anyhow::Result<Option<String>> {
        let now = now_ts();

        if paper_mode {
            let fake_id = format!("paper-{}-{}", token_id.chars().take(8).collect::<String>(), now as u64);
            info!(
                order_id = %fake_id,
                token_id,
                side,
                price,
                size,
                strategy,
                "[PAPER] would post order"
            );

            // Record in DB
            let _ = queries::insert_maker_order(
                &self.db,
                &fake_id,
                market_db_id,
                condition_id,
                token_id,
                side,
                price,
                size,
                "GTC",
                now,
                strategy,
            )
            .await;

            self.live_orders.insert(
                fake_id.clone(),
                LiveOrder {
                    order_id: fake_id.clone(),
                    token_id: token_id.to_string(),
                    condition_id: condition_id.to_string(),
                    side: side.to_string(),
                    price,
                    size,
                    strategy: strategy.to_string(),
                    posted_at: now,
                    market_db_id,
                },
            );

            self.event_bus.publish(BotEvent::OrderPosted {
                order_id: fake_id.clone(),
                token_id: token_id.to_string(),
                side: side.to_string(),
                price,
                size,
                strategy: strategy.to_string(),
            });

            return Ok(Some(fake_id));
        }

        // Build the order payload
        let payload = serde_json::json!({
            "tokenID": token_id,
            "price": format!("{:.2}", price),
            "size": format!("{:.1}", size),
            "side": side,
            "type": "GTC",
            "feeRateBps": "0",
        });

        let url = format!("{}/order", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("POLY-ADDRESS", &self.auth.funder_address)
            .header("POLY-SIGNATURE", &self.sign_request("POST", "/order", &payload))
            .header("POLY-TIMESTAMP", now.to_string())
            .header("POLY-API-KEY", &self.auth.clob_api_key)
            .header("POLY-PASSPHRASE", &self.auth.clob_api_passphrase)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            error!(
                status = %status,
                body = %body,
                token_id,
                side,
                price,
                size,
                "CLOB order POST failed"
            );
            return Err(anyhow::anyhow!("order post failed: {} {}", status, body));
        }

        let resp: OrderResponse = serde_json::from_str(&body)?;

        if let Some(ref order_id) = resp.order_id {
            info!(
                order_id,
                token_id,
                side,
                price,
                size,
                strategy,
                "order posted"
            );

            // Record in DB
            let _ = queries::insert_maker_order(
                &self.db,
                order_id,
                market_db_id,
                condition_id,
                token_id,
                side,
                price,
                size,
                "GTC",
                now,
                strategy,
            )
            .await;

            // Track in memory
            self.live_orders.insert(
                order_id.clone(),
                LiveOrder {
                    order_id: order_id.clone(),
                    token_id: token_id.to_string(),
                    condition_id: condition_id.to_string(),
                    side: side.to_string(),
                    price,
                    size,
                    strategy: strategy.to_string(),
                    posted_at: now,
                    market_db_id,
                },
            );

            self.event_bus.publish(BotEvent::OrderPosted {
                order_id: order_id.clone(),
                token_id: token_id.to_string(),
                side: side.to_string(),
                price,
                size,
                strategy: strategy.to_string(),
            });

            Ok(Some(order_id.clone()))
        } else {
            warn!(
                error = resp.error_msg.as_deref().unwrap_or("unknown"),
                token_id,
                side,
                "order post returned no order_id"
            );
            Ok(None)
        }
    }

    /// Cancel a single order by ID.
    pub async fn cancel_order(&self, order_id: &str, paper_mode: bool) -> anyhow::Result<()> {
        debug!(order_id, "cancelling order");

        // Remove from live tracking
        self.live_orders.remove(order_id);

        // Update DB status
        let _ = queries::update_maker_order_status(&self.db, order_id, "cancelled", None, None).await;

        self.event_bus.publish(BotEvent::OrderCancelled {
            order_id: order_id.to_string(),
            reason: if paper_mode { "paper_cancel".to_string() } else { "cancel_replace".to_string() },
        });

        if paper_mode {
            info!(order_id, "[PAPER] would cancel order");
            return Ok(());
        }

        let url = format!("{}/order/{}", self.base_url, order_id);

        let now = now_ts();
        let response = self
            .client
            .delete(&url)
            .header("POLY-ADDRESS", &self.auth.funder_address)
            .header("POLY-SIGNATURE", &self.sign_request("DELETE", &format!("/order/{}", order_id), &serde_json::Value::Null))
            .header("POLY-TIMESTAMP", now.to_string())
            .header("POLY-API-KEY", &self.auth.clob_api_key)
            .header("POLY-PASSPHRASE", &self.auth.clob_api_passphrase)
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await?;
            warn!(order_id, body = %body, "cancel request failed (may already be filled)");
        }

        Ok(())
    }

    /// Cancel all orders for a specific market (by condition_id).
    pub async fn cancel_market_orders(
        &self,
        condition_id: &str,
        paper_mode: bool,
    ) -> anyhow::Result<usize> {
        let mut cancelled = 0;
        let to_cancel: Vec<String> = self
            .live_orders
            .iter()
            .filter(|entry| entry.value().condition_id == condition_id)
            .map(|entry| entry.key().clone())
            .collect();

        for order_id in &to_cancel {
            if let Err(e) = self.cancel_order(order_id, paper_mode).await {
                warn!(order_id, error = %e, "failed to cancel order");
            } else {
                cancelled += 1;
            }
        }

        if !to_cancel.is_empty() {
            info!(condition_id, cancelled, total = to_cancel.len(), "cancelled market orders");
        }

        Ok(cancelled)
    }

    /// Cancel ALL live orders (used during shutdown or circuit breaker).
    pub async fn cancel_all(&self) -> anyhow::Result<()> {
        let all_ids: Vec<String> = self.live_orders.iter().map(|e| e.key().clone()).collect();

        if all_ids.is_empty() {
            info!("no live orders to cancel");
            return Ok(());
        }

        info!(count = all_ids.len(), "cancelling all live orders");

        // Try the batch cancel endpoint first
        let url = format!("{}/cancel-all", self.base_url);
        let now = now_ts();

        let response = self
            .client
            .delete(&url)
            .header("POLY-ADDRESS", &self.auth.funder_address)
            .header("POLY-SIGNATURE", &self.sign_request("DELETE", "/cancel-all", &serde_json::Value::Null))
            .header("POLY-TIMESTAMP", now.to_string())
            .header("POLY-API-KEY", &self.auth.clob_api_key)
            .header("POLY-PASSPHRASE", &self.auth.clob_api_passphrase)
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                info!("batch cancel-all succeeded");
            }
            _ => {
                warn!("batch cancel-all failed, falling back to individual cancels");
                for order_id in &all_ids {
                    let _ = self.cancel_order(order_id, false).await;
                }
            }
        }

        // Clear all from tracking
        self.live_orders.clear();

        // Update DB for any remaining
        for order_id in &all_ids {
            let _ = queries::update_maker_order_status(&self.db, order_id, "cancelled", None, None).await;
        }

        Ok(())
    }

    /// Get all live orders for a given condition_id.
    pub fn get_market_orders(&self, condition_id: &str) -> Vec<LiveOrder> {
        self.live_orders
            .iter()
            .filter(|entry| entry.value().condition_id == condition_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get count of live orders.
    pub fn live_order_count(&self) -> usize {
        self.live_orders.len()
    }

    /// Handle a fill notification (from WS user channel or polling).
    pub async fn on_fill(
        &self,
        order_id: &str,
        fill_price: f64,
        fill_size: f64,
        fully_filled: bool,
    ) {
        info!(
            order_id,
            fill_price,
            fill_size,
            fully_filled,
            "order filled"
        );

        self.event_bus.publish(BotEvent::OrderFilled {
            order_id: order_id.to_string(),
            fill_price,
            fill_size,
            fully_filled,
        });

        let status = if fully_filled {
            "filled"
        } else {
            "partially_filled"
        };

        if fully_filled {
            self.live_orders.remove(order_id);
        }

        let _ = queries::update_maker_order_status(
            &self.db,
            order_id,
            status,
            Some(fill_price),
            Some(fill_size),
        )
        .await;
    }

    /// Reconcile live orders against the CLOB (call periodically).
    pub async fn reconcile(&self) -> anyhow::Result<()> {
        let url = format!("{}/orders", self.base_url);
        let now = now_ts();

        let response = self
            .client
            .get(&url)
            .header("POLY-ADDRESS", &self.auth.funder_address)
            .header("POLY-SIGNATURE", &self.sign_request("GET", "/orders", &serde_json::Value::Null))
            .header("POLY-TIMESTAMP", now.to_string())
            .header("POLY-API-KEY", &self.auth.clob_api_key)
            .header("POLY-PASSPHRASE", &self.auth.clob_api_passphrase)
            .send()
            .await?;

        if !response.status().is_success() {
            warn!("reconcile: failed to fetch open orders from CLOB");
            return Ok(());
        }

        let body: serde_json::Value = response.json().await?;
        let clob_order_ids: std::collections::HashSet<String> = body
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|o| o.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();

        // Find orders we think are live but CLOB doesn't know about (filled or expired)
        let stale: Vec<String> = self
            .live_orders
            .iter()
            .filter(|e| !e.key().starts_with("paper-") && !clob_order_ids.contains(e.key().as_str()))
            .map(|e| e.key().clone())
            .collect();

        for order_id in &stale {
            warn!(order_id, "stale order removed during reconciliation");
            self.live_orders.remove(order_id);
            let _ = queries::update_maker_order_status(&self.db, order_id, "expired", None, None).await;
        }

        if !stale.is_empty() {
            info!(removed = stale.len(), "reconciliation cleaned up stale orders");
        }

        Ok(())
    }

    /// HMAC signature for CLOB REST API authentication.
    /// In production, this should use the CLOB API secret with HMAC-SHA256.
    fn sign_request(&self, method: &str, path: &str, body: &serde_json::Value) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let timestamp = now_ts().to_string();
        let body_str = if body.is_null() {
            String::new()
        } else {
            serde_json::to_string(body).unwrap_or_default()
        };

        let message = format!("{}{}{}{}", timestamp, method, path, body_str);

        let secret_bytes = base64_decode(&self.auth.clob_api_secret);
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret_bytes)
            .expect("HMAC key can be any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();

        base64_encode(&result.into_bytes())
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn base64_decode(input: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .unwrap_or_default()
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}
