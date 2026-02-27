//! Strategy engine — per-market state machine.

use std::sync::Arc;

use serde::Serialize;

use crate::feeds::types::{OracleSnapshot, OrderBook};
use crate::strategy::article::{ArticleOrder, ArticleStrategy};
use crate::strategy::pricing::{MarketInventory, QuotePricer, QuoteSet};
use crate::strategy::risk::RiskManager;

/// Market lifecycle states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MarketState {
    Discovered,
    /// Phase 0: monitoring books for neg-vig, no order placement.
    Monitoring,
    /// Legacy states (kept for future phases)
    Qualifying,
    Quoting,
    ArticleZone,
    Settlement,
    Withdrawn,
}

/// Context for a single active market.
#[derive(Debug, Clone, Serialize)]
pub struct MarketContext {
    pub condition_id: String,
    pub asset: String,
    pub timeframe: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub state: MarketState,
    pub neg_risk: bool,
    pub tick_size: f64,
    pub min_order_size: f64,
    pub db_market_id: Option<i64>,
}

impl MarketContext {
    pub fn seconds_until_close(&self) -> f64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        (self.end_time - now).max(0.0)
    }

    pub fn is_5min(&self) -> bool {
        self.timeframe == "5min"
    }
}

/// Action the strategy engine wants the executor to take.
#[derive(Debug)]
pub enum StrategyAction {
    /// Post/update two-sided quotes.
    PostQuotes(QuoteSet),
    /// Post an article endgame order.
    PostArticle(ArticleOrder),
    /// Cancel all orders for this market.
    CancelAll,
    /// Do nothing — no change needed.
    Hold,
    /// Withdraw from this market entirely.
    Withdraw,
}

/// Drives market-level strategy decisions.
pub struct StrategyEngine {
    pricer: Arc<QuotePricer>,
    article: Arc<ArticleStrategy>,
    risk: Arc<RiskManager>,
}

impl StrategyEngine {
    pub fn new(
        pricer: Arc<QuotePricer>,
        article: Arc<ArticleStrategy>,
        risk: Arc<RiskManager>,
    ) -> Self {
        Self {
            pricer,
            article,
            risk,
        }
    }

    /// Process a market and decide what action to take.
    ///
    /// PHASE 0: Monitor-only mode. All quoting/article logic is disabled.
    /// The cancel/replace loop handles neg-vig monitoring directly.
    /// This engine only manages state transitions (Discovered → Monitoring → Settlement).
    pub fn process_market(
        &self,
        market: &mut MarketContext,
        _oracle: &OracleSnapshot,
        _yes_book: &OrderBook,
        _inventory: &MarketInventory,
    ) -> StrategyAction {
        let secs_left = market.seconds_until_close();

        // Risk check
        if self.risk.is_halted() {
            market.state = MarketState::Withdrawn;
            return StrategyAction::CancelAll;
        }

        match &market.state {
            MarketState::Discovered => {
                market.state = MarketState::Monitoring;
                StrategyAction::Hold
            }
            MarketState::Monitoring => {
                if secs_left < 2.0 {
                    market.state = MarketState::Settlement;
                    return StrategyAction::CancelAll;
                }
                // Phase 0: just monitor, no orders
                StrategyAction::Hold
            }
            // Legacy states — treat as monitoring
            MarketState::Qualifying | MarketState::Quoting | MarketState::ArticleZone => {
                market.state = MarketState::Monitoring;
                StrategyAction::Hold
            }
            MarketState::Settlement => StrategyAction::Hold,
            MarketState::Withdrawn => {
                if secs_left > 30.0 {
                    market.state = MarketState::Monitoring;
                }
                StrategyAction::Hold
            }
        }
    }
}
