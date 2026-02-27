//! Risk management — position limits, exposure caps, circuit breaker.

use crate::config::RiskConfig;
use crate::events::bus::{BotEvent, EventBus};
use crate::strategy::pricing::MarketInventory;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Current portfolio risk state.
#[derive(Debug, Clone, Serialize)]
pub struct RiskState {
    pub total_exposure_usd: f64,
    pub daily_pnl: f64,
    pub active_markets: usize,
    pub halted: bool,
}

/// Risk manager enforcing position limits and circuit breakers.
pub struct RiskManager {
    config: RiskConfig,
    halted: AtomicBool,
    daily_pnl: RwLock<f64>,
    total_exposure: RwLock<f64>,
    active_market_count: RwLock<usize>,
    event_bus: Arc<EventBus>,
}

impl RiskManager {
    pub fn new(config: RiskConfig, event_bus: Arc<EventBus>) -> Self {
        Self {
            config,
            halted: AtomicBool::new(false),
            daily_pnl: RwLock::new(0.0),
            total_exposure: RwLock::new(0.0),
            active_market_count: RwLock::new(0),
            event_bus,
        }
    }

    /// Check if we can post a new order given current inventory.
    pub fn can_post_order(
        &self,
        side: &str,
        _token: &str,
        size: f64,
        inventory: &MarketInventory,
    ) -> bool {
        if self.is_halted() {
            return false;
        }

        if size > self.config.max_order_size {
            return false;
        }

        if size < self.config.min_order_size {
            return false;
        }

        // Check per-side position limits
        match side {
            "BUY" => {
                // Buying YES increases yes_shares
                if inventory.yes_shares + size > self.config.max_yes_position {
                    return false;
                }
            }
            "SELL" => {
                // Selling YES reduces yes_shares (need shares to sell)
                // For maker, selling means providing asks
            }
            _ => {}
        }

        // Check portfolio exposure
        let exposure = *self.total_exposure.read();
        if exposure + size * 0.5 > self.config.max_total_exposure_usd {
            return false;
        }

        // Check concurrent market limit
        let markets = *self.active_market_count.read();
        if markets >= self.config.max_concurrent_markets {
            return false;
        }

        true
    }

    /// Check inventory imbalance.
    pub fn check_inventory_imbalance(&self, inventory: &MarketInventory) -> bool {
        inventory.net_yes().abs() <= self.config.max_inventory_imbalance
    }

    /// Get maximum order size given current inventory.
    pub fn get_max_order_size(&self, side: &str, inventory: &MarketInventory) -> f64 {
        let remaining = match side {
            "BUY" => self.config.max_yes_position - inventory.yes_shares,
            "SELL" => self.config.max_no_position - inventory.no_shares,
            _ => self.config.max_order_size,
        };
        remaining.min(self.config.max_order_size).max(0.0)
    }

    /// Update daily PnL and check circuit breaker.
    pub fn update_daily_pnl(&self, pnl_change: f64) {
        let mut dpnl = self.daily_pnl.write();
        *dpnl += pnl_change;
        if *dpnl < -self.config.max_daily_loss_usd {
            let was_halted = self.halted.swap(true, Ordering::Relaxed);
            tracing::error!(
                daily_pnl = *dpnl,
                limit = -self.config.max_daily_loss_usd,
                "CIRCUIT BREAKER TRIPPED — halting all quoting"
            );
            if !was_halted {
                self.event_bus.publish(BotEvent::CircuitBreaker {
                    daily_pnl: *dpnl,
                });
            }
        }

        // Publish risk alert if approaching limit (>75% of max loss)
        let threshold = -self.config.max_daily_loss_usd * 0.75;
        if *dpnl < threshold && *dpnl >= -self.config.max_daily_loss_usd {
            let exposure = *self.total_exposure.read();
            self.event_bus.publish(BotEvent::RiskAlert {
                message: format!("Daily PnL at {:.1}% of limit", (*dpnl / -self.config.max_daily_loss_usd) * 100.0),
                daily_pnl: *dpnl,
                exposure_usd: exposure,
            });
        }
    }

    /// Update total exposure.
    pub fn set_total_exposure(&self, exposure: f64) {
        *self.total_exposure.write() = exposure;
    }

    /// Update active market count.
    pub fn set_active_market_count(&self, count: usize) {
        *self.active_market_count.write() = count;
    }

    /// Check if trading is halted.
    pub fn is_halted(&self) -> bool {
        self.halted.load(Ordering::Relaxed)
    }

    /// Get current risk state snapshot.
    pub fn state(&self) -> RiskState {
        RiskState {
            total_exposure_usd: *self.total_exposure.read(),
            daily_pnl: *self.daily_pnl.read(),
            active_markets: *self.active_market_count.read(),
            halted: self.is_halted(),
        }
    }

    /// Reset daily PnL (call at start of new trading day).
    pub fn reset_daily(&self) {
        *self.daily_pnl.write() = 0.0;
        self.halted.store(false, Ordering::Relaxed);
    }
}
