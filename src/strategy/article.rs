//! Article endgame strategy — T-10s before 5-min market close.
//!
//! At 10 seconds before close, BTC direction is ~85-93% determined.
//! Post a single maker order on the winning side at 90-95c.

use crate::config::ArticleConfig;
use crate::feeds::types::{OracleSnapshot, OrderBook};
use serde::Serialize;

/// An article strategy order decision.
#[derive(Debug, Clone, Serialize)]
pub struct ArticleOrder {
    pub direction: String,
    pub price: f64,
    pub size: f64,
    pub conviction: f64,
}

/// T-10s endgame strategy for 5-minute BTC markets.
pub struct ArticleStrategy {
    config: ArticleConfig,
}

impl ArticleStrategy {
    pub fn new(config: ArticleConfig) -> Self {
        Self { config }
    }

    /// Evaluate whether to fire the article strategy.
    /// Returns Some(ArticleOrder) if conditions are met, None otherwise.
    pub fn evaluate(
        &self,
        oracle: &OracleSnapshot,
        book: &OrderBook,
        secs_left: f64,
    ) -> Option<ArticleOrder> {
        if !self.config.enabled {
            return None;
        }

        // Only fire in the article zone
        if secs_left > self.config.trigger_seconds_before_close
            || secs_left < self.config.min_seconds_before_close
        {
            return None;
        }

        // 60-second directional signal
        let bn_d60 = &oracle.binance_delta_60s;
        let cb_d60 = &oracle.coinbase_delta_60s;

        if !bn_d60.available || !cb_d60.available {
            return None;
        }

        let bn_val = bn_d60.delta_bps;
        let cb_val = cb_d60.delta_bps;

        // Direction must agree
        if bn_val.signum() != cb_val.signum() {
            return None;
        }

        // Magnitude thresholds
        if bn_val.abs() < self.config.min_bn_delta_60s_bps {
            return None;
        }
        if cb_val.abs() < self.config.min_cb_delta_60s_bps {
            return None;
        }

        let direction = if bn_val > 0.0 { "UP" } else { "DOWN" };

        // PM mid must confirm trend
        let pm_mid = book.mid();
        let trending_mid = if direction == "UP" { pm_mid } else { 1.0 - pm_mid };
        if trending_mid < self.config.min_trending_mid {
            return None;
        }

        // Entry price: best ask of winning side, capped
        let entry_price = if direction == "UP" {
            book.best_ask().min(self.config.entry_price_cap)
        } else {
            // For "DOWN", we buy the NO token
            // The NO token's price ≈ 1 - YES_mid, so its ask is around there
            // We cap at entry_price_cap
            (1.0 - book.best_bid()).min(self.config.entry_price_cap)
        };

        // Floor at reasonable price
        let entry_price = entry_price.max(0.85);

        Some(ArticleOrder {
            direction: direction.to_string(),
            price: entry_price,
            size: self.config.article_size,
            conviction: trending_mid,
        })
    }
}
