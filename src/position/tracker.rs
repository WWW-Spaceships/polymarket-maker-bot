//! Position tracking — per-token inventory management.

use dashmap::DashMap;
use serde::Serialize;
use sqlx::PgPool;
use tracing::{debug, info};

use crate::db::queries;
use crate::strategy::pricing::MarketInventory;

/// Tracks per-token position (shares held).
#[derive(Debug, Clone, Serialize)]
pub struct TokenPosition {
    pub token_id: String,
    pub condition_id: String,
    pub side: String,
    pub shares: f64,
    pub avg_entry_price: f64,
}

/// Per-condition-id inventory pair (YES + NO shares).
#[derive(Debug, Clone)]
struct ConditionInventory {
    yes_token_id: String,
    no_token_id: String,
    yes_shares: f64,
    no_shares: f64,
}

/// Manages real-time position state.
pub struct PositionTracker {
    /// Token-level positions keyed by token_id.
    positions: DashMap<String, TokenPosition>,
    /// Condition-level inventory keyed by condition_id.
    inventories: DashMap<String, ConditionInventory>,
    db: PgPool,
}

impl PositionTracker {
    pub fn new(db: PgPool) -> Self {
        Self {
            positions: DashMap::new(),
            inventories: DashMap::new(),
            db,
        }
    }

    /// Load inventory state from DB on startup.
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let rows = queries::get_all_inventory(&self.db).await?;
        for inv in rows {
            self.positions.insert(
                inv.token_id.clone(),
                TokenPosition {
                    token_id: inv.token_id,
                    condition_id: inv.condition_id,
                    side: inv.side,
                    shares: inv.shares,
                    avg_entry_price: inv.avg_entry_price,
                },
            );
        }
        info!(count = self.positions.len(), "loaded positions from DB");
        Ok(())
    }

    /// Record a fill — updates inventory in memory and DB.
    pub async fn on_fill(
        &self,
        token_id: &str,
        condition_id: &str,
        side: &str,
        fill_price: f64,
        fill_size: f64,
    ) {
        let now = now_ts();

        let mut entry = self.positions.entry(token_id.to_string()).or_insert_with(|| {
            TokenPosition {
                token_id: token_id.to_string(),
                condition_id: condition_id.to_string(),
                side: side.to_string(),
                shares: 0.0,
                avg_entry_price: 0.0,
            }
        });

        let pos = entry.value_mut();

        // Update average entry price (weighted average)
        if side == "BUY" {
            let total_cost = pos.avg_entry_price * pos.shares + fill_price * fill_size;
            pos.shares += fill_size;
            if pos.shares > 0.0 {
                pos.avg_entry_price = total_cost / pos.shares;
            }
        } else {
            // SELL reduces position
            pos.shares = (pos.shares - fill_size).max(0.0);
            // avg_entry_price stays the same on sells
        }

        debug!(
            token_id,
            condition_id,
            side,
            fill_price,
            fill_size,
            new_shares = pos.shares,
            "position updated"
        );

        // Persist to DB
        let _ = queries::upsert_inventory(
            &self.db,
            token_id,
            condition_id,
            side,
            pos.shares,
            pos.avg_entry_price,
            now,
        )
        .await;
    }

    /// Get MarketInventory for a condition_id (used by strategy engine).
    pub fn get_inventory(&self, condition_id: &str) -> MarketInventory {
        let mut yes_shares = 0.0;
        let mut no_shares = 0.0;

        for entry in self.positions.iter() {
            let pos = entry.value();
            if pos.condition_id == condition_id {
                // Determine if this is YES or NO based on stored side context
                // Convention: first token is YES, second is NO
                // We track by individual token fills, but the side tells us direction
                if pos.side == "BUY" {
                    // This token was bought — it could be YES or NO token
                    // We need a way to map token_id → YES/NO
                    // For now, use a simple heuristic based on shares > 0
                    yes_shares += pos.shares;
                } else {
                    no_shares += pos.shares;
                }
            }
        }

        MarketInventory {
            yes_shares,
            no_shares,
        }
    }

    /// Get total portfolio exposure in USD (sum of all positions * avg_entry_price).
    pub fn get_total_exposure_usd(&self) -> f64 {
        self.positions
            .iter()
            .map(|entry| entry.value().shares * entry.value().avg_entry_price)
            .sum()
    }

    /// Get all current positions.
    pub fn all_positions(&self) -> Vec<TokenPosition> {
        self.positions.iter().map(|e| e.value().clone()).collect()
    }

    /// Get position count (tokens with shares > 0).
    pub fn position_count(&self) -> usize {
        self.positions
            .iter()
            .filter(|e| e.value().shares > 0.0)
            .count()
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
