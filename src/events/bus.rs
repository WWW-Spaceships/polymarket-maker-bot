//! Internal event broadcast — tokio::broadcast channel for cross-component events.

use serde::Serialize;
use tokio::sync::broadcast;

/// Bot-wide events for alerting, logging, and monitoring.
#[derive(Debug, Clone, Serialize)]
pub enum BotEvent {
    /// An order was posted to the CLOB.
    OrderPosted {
        order_id: String,
        token_id: String,
        side: String,
        price: f64,
        size: f64,
        strategy: String,
    },
    /// An order was filled.
    OrderFilled {
        order_id: String,
        fill_price: f64,
        fill_size: f64,
        fully_filled: bool,
    },
    /// An order was cancelled.
    OrderCancelled {
        order_id: String,
        reason: String,
    },
    /// New market discovered and tracked.
    MarketDiscovered {
        condition_id: String,
        asset: String,
        timeframe: String,
    },
    /// Market resolved.
    MarketResolved {
        condition_id: String,
        outcome: String,
    },
    /// Risk alert (approaching limits).
    RiskAlert {
        message: String,
        daily_pnl: f64,
        exposure_usd: f64,
    },
    /// Circuit breaker tripped — all quoting halted.
    CircuitBreaker {
        daily_pnl: f64,
    },
    /// Feed disconnected.
    FeedDisconnected {
        source: String,
    },
    /// Feed reconnected.
    FeedReconnected {
        source: String,
    },
    /// Article strategy fired.
    ArticleFired {
        condition_id: String,
        direction: String,
        price: f64,
        size: f64,
        conviction: f64,
    },
    /// Daily PnL summary.
    DailySummary {
        total_pnl: f64,
        trades_count: u64,
        win_rate: f64,
        rebates_usdc: f64,
    },
    /// Negative-vig opportunity detected (YES_ask + NO_ask < $1.00).
    NegVigDetected {
        condition_id: String,
        asset: String,
        timeframe: String,
        yes_ask: f64,
        no_ask: f64,
        combined: f64,
        edge_cents: f64,
        yes_depth: f64,
        no_depth: f64,
        secs_left: f64,
    },
}

/// Central event bus for broadcasting events to all subscribers.
pub struct EventBus {
    tx: broadcast::Sender<BotEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all subscribers.
    pub fn publish(&self, event: BotEvent) {
        // Ignore error if no subscribers
        let _ = self.tx.send(event);
    }

    /// Subscribe to events.
    pub fn subscribe(&self) -> broadcast::Receiver<BotEvent> {
        self.tx.subscribe()
    }

    /// Get current subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
