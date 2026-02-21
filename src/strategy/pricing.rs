//! QuotePricer — computes bid/ask quotes for two-sided market making.

use crate::config::StrategyConfig;
use crate::feeds::types::{OracleSnapshot, OrderBook};
use serde::Serialize;

/// A set of quotes for both YES and NO tokens.
#[derive(Debug, Clone, Serialize)]
pub struct QuoteSet {
    pub yes_bid: f64,
    pub yes_ask: f64,
    pub no_bid: f64,
    pub no_ask: f64,
    pub yes_bid_size: f64,
    pub yes_ask_size: f64,
    pub no_bid_size: f64,
    pub no_ask_size: f64,
}

/// Per-market inventory state.
#[derive(Debug, Clone, Default)]
pub struct MarketInventory {
    pub yes_shares: f64,
    pub no_shares: f64,
}

impl MarketInventory {
    pub fn net_yes(&self) -> f64 {
        self.yes_shares - self.no_shares
    }
}

/// Quote pricing model for two-sided market making.
pub struct QuotePricer {
    config: StrategyConfig,
    bayesian_weights: Option<Vec<f64>>,
}

impl QuotePricer {
    pub fn new(config: StrategyConfig, bayesian_weights: Option<Vec<f64>>) -> Self {
        Self {
            config,
            bayesian_weights,
        }
    }

    /// Compute bid/ask quotes for YES and NO tokens.
    pub fn compute_quotes(
        &self,
        oracle: &OracleSnapshot,
        book: &OrderBook,
        secs_left: f64,
        inventory: &MarketInventory,
        max_position: f64,
    ) -> QuoteSet {
        // 1. Estimate fair value P(YES wins)
        let fair_value = self.estimate_fair_value(oracle, book, secs_left);

        // 2. Half-spread: function of volatility and config minimum
        let vol = oracle.binance_delta_3s.delta_bps.abs();
        let half_spread = f64::max(
            self.config.min_half_spread,
            self.config.volatility_coefficient * vol / 10_000.0,
        );

        // 3. Inventory skew — adjust prices to reduce directional exposure
        let max_pos = if max_position > 0.0 { max_position } else { 100.0 };
        let skew = self.config.inventory_skew_factor * inventory.net_yes() / max_pos;

        // 4. Compute 4 quotes
        let yes_bid = quantize_down(fair_value - half_spread - skew, book.tick_size);
        let yes_ask = quantize_up(fair_value + half_spread - skew, book.tick_size);
        let no_bid = quantize_down((1.0 - fair_value) - half_spread + skew, book.tick_size);
        let no_ask = quantize_up((1.0 - fair_value) + half_spread + skew, book.tick_size);

        // 5. Clamp all to valid range
        let size = self.config.quote_size;

        QuoteSet {
            yes_bid: yes_bid.clamp(0.01, 0.99),
            yes_ask: yes_ask.clamp(0.01, 0.99),
            no_bid: no_bid.clamp(0.01, 0.99),
            no_ask: no_ask.clamp(0.01, 0.99),
            yes_bid_size: size,
            yes_ask_size: size,
            no_bid_size: size,
            no_ask_size: size,
        }
    }

    /// Estimate P(YES wins) from oracle + book state.
    fn estimate_fair_value(
        &self,
        oracle: &OracleSnapshot,
        book: &OrderBook,
        _secs_left: f64,
    ) -> f64 {
        // If we have Bayesian weights, use them
        if let Some(ref _weights) = self.bayesian_weights {
            // TODO: Port full Bayesian inference from Python
            // For now, use a simple oracle-adjusted mid
        }

        // Simple model: use PM mid adjusted by oracle delta
        let pm_mid = book.mid();
        let bn_d1 = oracle.binance_delta_1s.delta_bps;

        // Oracle adjustment: if Binance shows BTC going up, shift fair value up
        let oracle_adj = bn_d1 / 10_000.0 * 0.5; // conservative 50% pass-through

        (pm_mid + oracle_adj).clamp(0.01, 0.99)
    }
}

/// Round price down to tick boundary.
fn quantize_down(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 {
        return price;
    }
    (price / tick).floor() * tick
}

/// Round price up to tick boundary.
fn quantize_up(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 {
        return price;
    }
    (price / tick).ceil() * tick
}
