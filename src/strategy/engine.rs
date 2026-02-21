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
    pub fn process_market(
        &self,
        market: &mut MarketContext,
        oracle: &OracleSnapshot,
        yes_book: &OrderBook,
        inventory: &MarketInventory,
    ) -> StrategyAction {
        let secs_left = market.seconds_until_close();

        // Risk check
        if self.risk.is_halted() {
            market.state = MarketState::Withdrawn;
            return StrategyAction::CancelAll;
        }

        match &market.state {
            MarketState::Discovered => {
                // Transition to qualifying
                market.state = MarketState::Qualifying;
                StrategyAction::Hold
            }
            MarketState::Qualifying => {
                // Check if market qualifies for quoting
                if secs_left < 5.0 {
                    market.state = MarketState::Settlement;
                    return StrategyAction::CancelAll;
                }
                if yes_book.asks.len() < 2 || yes_book.bids.len() < 2 {
                    return StrategyAction::Hold; // Not enough book depth
                }
                if yes_book.spread() > 0.10 {
                    return StrategyAction::Hold; // Spread too wide
                }

                market.state = MarketState::Quoting;
                let quotes = self.pricer.compute_quotes(
                    oracle,
                    yes_book,
                    secs_left,
                    inventory,
                    self.risk.get_max_order_size("BUY", inventory),
                );
                StrategyAction::PostQuotes(quotes)
            }
            MarketState::Quoting => {
                // Check for state transitions
                if secs_left < 2.0 {
                    market.state = MarketState::Settlement;
                    return StrategyAction::CancelAll;
                }

                // Enter article zone for 5-min markets
                if market.is_5min() && secs_left <= 10.0 {
                    market.state = MarketState::ArticleZone;
                    if let Some(article_order) = self.article.evaluate(oracle, yes_book, secs_left)
                    {
                        return StrategyAction::PostArticle(article_order);
                    }
                    return StrategyAction::CancelAll;
                }

                // Check feed health
                if !oracle.binance_delta_1s.available {
                    market.state = MarketState::Withdrawn;
                    return StrategyAction::CancelAll;
                }

                // Normal quoting: compute new quotes
                let quotes = self.pricer.compute_quotes(
                    oracle,
                    yes_book,
                    secs_left,
                    inventory,
                    self.risk.get_max_order_size("BUY", inventory),
                );
                StrategyAction::PostQuotes(quotes)
            }
            MarketState::ArticleZone => {
                if secs_left < 2.0 {
                    market.state = MarketState::Settlement;
                    return StrategyAction::CancelAll;
                }
                // Already in article zone, hold position
                StrategyAction::Hold
            }
            MarketState::Settlement => StrategyAction::Hold,
            MarketState::Withdrawn => {
                // Check if we can re-enter
                if secs_left > 30.0 && oracle.binance_delta_1s.available {
                    market.state = MarketState::Qualifying;
                }
                StrategyAction::Hold
            }
        }
    }
}
