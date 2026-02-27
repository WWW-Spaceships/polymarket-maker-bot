//! Order lifecycle management — post, cancel, reconcile maker orders.
//!
//! Uses the official polymarket-client-sdk for EIP-712 order signing
//! and HMAC-authenticated REST calls to the Polymarket CLOB API.
//! Tracks live orders in a DashMap for O(1) lookup by the hot loop.

use dashmap::DashMap;
use serde::Serialize;
use sqlx::PgPool;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as _;
use polymarket_client_sdk::auth::{Credentials, Normal};
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::clob::client::AuthenticationBuilder;
use polymarket_client_sdk::clob::types::response::PostOrderResponse;
use polymarket_client_sdk::POLYGON;

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

/// Response from the CLOB order placement endpoint (legacy, kept for compat).
#[derive(Debug, serde::Deserialize)]
pub struct OrderResponse {
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    pub success: Option<bool>,
    #[serde(rename = "errorMsg")]
    pub error_msg: Option<String>,
    pub status: Option<String>,
}

/// Response from the CLOB cancel endpoint (legacy, kept for compat).
#[derive(Debug, serde::Deserialize)]
pub struct CancelResponse {
    pub canceled: Option<Vec<String>>,
    pub not_canceled: Option<serde_json::Value>,
}

/// Manages the full lifecycle of maker orders.
/// Uses the official Polymarket SDK for EIP-712 signed order placement.
pub struct OrderManager {
    /// The authenticated CLOB client from polymarket-client-sdk
    clob: Arc<ClobClient<polymarket_client_sdk::auth::state::Authenticated<Normal>>>,
    /// The wallet signer (needed for sign() calls)
    signer: Arc<PrivateKeySigner>,
    db: PgPool,
    event_bus: Arc<EventBus>,
    /// In-memory tracking of all live (posted, not yet filled/cancelled) orders.
    pub live_orders: Arc<DashMap<String, LiveOrder>>,
}

impl OrderManager {
    /// Create a new OrderManager with the official Polymarket SDK.
    /// This authenticates with the CLOB API using L1 EIP-712 headers.
    pub async fn new(
        base_url: &str,
        auth: &AuthConfig,
        db: PgPool,
        event_bus: Arc<EventBus>,
    ) -> anyhow::Result<Self> {
        // Create the wallet signer from private key
        let signer = PrivateKeySigner::from_str(&auth.private_key)
            .map_err(|e| anyhow::anyhow!("invalid private key: {}", e))?
            .with_chain_id(Some(POLYGON));

        info!(
            address = %signer.address(),
            "wallet signer initialized"
        );

        // Build the CLOB client
        let clob_client = ClobClient::new(base_url, ClobConfig::default())
            .map_err(|e| anyhow::anyhow!("failed to create CLOB client: {}", e))?;

        // Build credentials from env vars (API key, secret, passphrase)
        let api_key = uuid::Uuid::parse_str(&auth.clob_api_key)
            .map_err(|e| anyhow::anyhow!("invalid CLOB API key UUID: {}", e))?;
        let credentials = Credentials::new(
            api_key,
            auth.clob_api_secret.clone(),
            auth.clob_api_passphrase.clone(),
        );

        // Determine signature type
        let sig_type = match auth.signature_type {
            0 => SignatureType::Eoa,
            1 => SignatureType::Proxy,
            2 => SignatureType::GnosisSafe,
            _ => SignatureType::Eoa,
        };

        // Authenticate with existing credentials (no need to derive new ones)
        let mut auth_builder = clob_client
            .authentication_builder(&signer)
            .credentials(credentials)
            .signature_type(sig_type);

        // If using proxy/safe, set the funder address
        if sig_type != SignatureType::Eoa && !auth.funder_address.is_empty() {
            let funder_addr = auth.funder_address.parse()
                .map_err(|e| anyhow::anyhow!("invalid funder address: {}", e))?;
            auth_builder = auth_builder.funder(funder_addr);
        }

        let authenticated_client = auth_builder
            .authenticate()
            .await
            .map_err(|e| anyhow::anyhow!("CLOB authentication failed: {}", e))?;

        info!("CLOB client authenticated successfully");

        Ok(Self {
            clob: Arc::new(authenticated_client),
            signer: Arc::new(signer),
            db,
            event_bus,
            live_orders: Arc::new(DashMap::new()),
        })
    }

    /// Post a GTC limit order to the CLOB with proper EIP-712 signing.
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
            let fake_id = format!(
                "paper-{}-{}",
                token_id.chars().take(8).collect::<String>(),
                now as u64
            );
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

        // --- LIVE MODE: Build, sign, and post via official SDK ---

        // Parse token ID as U256
        let token_id_u256 = U256::from_str(token_id)
            .map_err(|e| anyhow::anyhow!("invalid token ID: {}", e))?;

        // Parse side
        let order_side = match side {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => return Err(anyhow::anyhow!("invalid side: {}", side)),
        };

        // Build the order using the SDK's builder (handles amounts, fees, tick sizes)
        let price_dec = rust_decimal::Decimal::try_from(price)
            .map_err(|e| anyhow::anyhow!("invalid price: {}", e))?;
        let size_dec = rust_decimal::Decimal::try_from(size)
            .map_err(|e| anyhow::anyhow!("invalid size: {}", e))?;

        // Truncate to proper precision
        use rust_decimal::prelude::*;
        let price_dec = price_dec.round_dp(2); // tick size is 0.01
        let size_dec = size_dec.round_dp(2); // lot size is 2 decimals

        let signable_order = self
            .clob
            .limit_order()
            .token_id(token_id_u256)
            .side(order_side)
            .price(price_dec)
            .size(size_dec)
            .order_type(OrderType::GTC)
            .build()
            .await
            .map_err(|e| anyhow::anyhow!("order build failed: {}", e))?;

        // Sign with EIP-712
        let signed_order = self
            .clob
            .sign(&*self.signer, signable_order)
            .await
            .map_err(|e| anyhow::anyhow!("order signing failed: {}", e))?;

        // Post to CLOB
        let response = self
            .clob
            .post_order(signed_order)
            .await
            .map_err(|e| anyhow::anyhow!("order post failed: {}", e))?;

        let order_id = response.order_id;

        info!(
            order_id = %order_id,
            token_id,
            side,
            price,
            size,
            strategy,
            "order posted via SDK"
        );

        // Record in DB
        let _ = queries::insert_maker_order(
            &self.db,
            &order_id,
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

        Ok(Some(order_id))
    }

    /// Cancel a single order by ID.
    pub async fn cancel_order(&self, order_id: &str, paper_mode: bool) -> anyhow::Result<()> {
        debug!(order_id, "cancelling order");

        // Remove from live tracking
        self.live_orders.remove(order_id);

        // Update DB status
        let _ =
            queries::update_maker_order_status(&self.db, order_id, "cancelled", None, None).await;

        self.event_bus.publish(BotEvent::OrderCancelled {
            order_id: order_id.to_string(),
            reason: if paper_mode {
                "paper_cancel".to_string()
            } else {
                "cancel_replace".to_string()
            },
        });

        if paper_mode {
            info!(order_id, "[PAPER] would cancel order");
            return Ok(());
        }

        // Cancel via SDK
        match self.clob.cancel_order(order_id).await {
            Ok(_) => {
                debug!(order_id, "order cancelled via SDK");
            }
            Err(e) => {
                warn!(order_id, error = %e, "cancel request failed (may already be filled)");
            }
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
            info!(
                condition_id,
                cancelled,
                total = to_cancel.len(),
                "cancelled market orders"
            );
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

        // Try the batch cancel endpoint first via SDK
        match self.clob.cancel_all_orders().await {
            Ok(_) => {
                info!("batch cancel-all succeeded via SDK");
            }
            Err(e) => {
                warn!(error = %e, "batch cancel-all failed, falling back to individual cancels");
                for order_id in &all_ids {
                    let _ = self.cancel_order(order_id, false).await;
                }
            }
        }

        // Clear all from tracking
        self.live_orders.clear();

        // Update DB for any remaining
        for order_id in &all_ids {
            let _ =
                queries::update_maker_order_status(&self.db, order_id, "cancelled", None, None)
                    .await;
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
        // Use SDK to fetch open orders
        let request = polymarket_client_sdk::clob::types::request::OrdersRequest::builder().build();
        let page = self
            .clob
            .orders(&request, None)
            .await
            .map_err(|e| anyhow::anyhow!("reconcile fetch failed: {}", e))?;

        let clob_order_ids: std::collections::HashSet<String> = page
            .data
            .iter()
            .map(|o| o.id.clone())
            .collect();

        // Find orders we think are live but CLOB doesn't know about
        let stale: Vec<String> = self
            .live_orders
            .iter()
            .filter(|e| {
                !e.key().starts_with("paper-") && !clob_order_ids.contains(e.key().as_str())
            })
            .map(|e| e.key().clone())
            .collect();

        for order_id in &stale {
            warn!(order_id, "stale order removed during reconciliation");
            self.live_orders.remove(order_id);
            let _ =
                queries::update_maker_order_status(&self.db, order_id, "expired", None, None)
                    .await;
        }

        if !stale.is_empty() {
            info!(
                removed = stale.len(),
                "reconciliation cleaned up stale orders"
            );
        }

        Ok(())
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
