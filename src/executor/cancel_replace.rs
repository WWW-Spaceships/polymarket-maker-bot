//! Cancel/replace hot loop — the core trading cycle.
//!
//! Target: <100ms per cycle.
//! 1. Wait for feed update or 50ms timeout
//! 2. Snapshot oracle + PM book state
//! 3. Run strategy engine (compute quotes or article decision)
//! 4. Diff new quotes against outstanding orders
//! 5. Cancel stale orders (> 1 tick away)
//! 6. Post replacement orders
//! 7. Queue DB write (async, off hot path)

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::time;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::feeds::FeedHub;
use crate::position::tracker::PositionTracker;
use crate::strategy::engine::{MarketContext, StrategyAction, StrategyEngine};
use crate::strategy::risk::RiskManager;

use super::order_manager::OrderManager;

/// Run the cancel/replace loop for a single market.
/// This function runs until the market reaches Settlement or Withdrawn state
/// and should be spawned as a tokio task per active market.
pub async fn run_cancel_replace_loop(
    market: Arc<tokio::sync::Mutex<MarketContext>>,
    feed_hub: Arc<FeedHub>,
    strategy_engine: Arc<StrategyEngine>,
    order_manager: Arc<OrderManager>,
    _risk_manager: Arc<RiskManager>,
    position_tracker: Arc<PositionTracker>,
    config: Arc<Config>,
    _db: PgPool,
) {
    let cycle_interval = Duration::from_millis(50);

    {
        let cid = market.lock().await.condition_id.clone();
        info!(condition_id = %cid, "cancel/replace loop started");
    }

    loop {
        let cycle_start = Instant::now();

        // 1. Snapshot current market state
        let mut ctx = market.lock().await;

        // Check if market is done
        match &ctx.state {
            crate::strategy::engine::MarketState::Settlement
            | crate::strategy::engine::MarketState::Withdrawn => {
                info!(
                    condition_id = %ctx.condition_id,
                    state = ?ctx.state,
                    "market loop exiting"
                );
                // Cancel any remaining orders
                let _ = order_manager
                    .cancel_market_orders(&ctx.condition_id, config.trading.paper_mode)
                    .await;
                break;
            }
            _ => {}
        }

        // 2. Get oracle snapshot for this market's asset
        let oracle = feed_hub.oracle_snapshot(&ctx.asset);

        // 3. Get YES token orderbook
        let yes_book = feed_hub
            .get_book(&ctx.yes_token_id)
            .unwrap_or_else(|| crate::feeds::types::OrderBook::empty(ctx.yes_token_id.clone()));

        // 4. Get current inventory
        let inventory = position_tracker.get_inventory(&ctx.condition_id);

        // 5. Run strategy engine
        let action = strategy_engine.process_market(&mut ctx, &oracle, &yes_book, &inventory);

        let condition_id = ctx.condition_id.clone();
        let yes_token_id = ctx.yes_token_id.clone();
        let no_token_id = ctx.no_token_id.clone();
        let market_db_id = ctx.db_market_id.unwrap_or(0);
        let tick_size = ctx.tick_size;

        // Drop the lock before doing I/O
        drop(ctx);

        // 6. Execute the strategy action
        match action {
            StrategyAction::PostQuotes(quotes) => {
                execute_quote_update(
                    &order_manager,
                    &condition_id,
                    &yes_token_id,
                    &no_token_id,
                    market_db_id,
                    &quotes,
                    tick_size,
                    config.trading.paper_mode,
                )
                .await;
            }
            StrategyAction::PostArticle(article_order) => {
                info!(
                    condition_id = %condition_id,
                    direction = %article_order.direction,
                    price = article_order.price,
                    size = article_order.size,
                    conviction = article_order.conviction,
                    "firing article strategy"
                );

                // Cancel existing quotes first
                let _ = order_manager
                    .cancel_market_orders(&condition_id, config.trading.paper_mode)
                    .await;

                // Post the article order
                let token_id = if article_order.direction == "UP" {
                    &yes_token_id
                } else {
                    &no_token_id
                };

                let _ = order_manager
                    .post_order(
                        token_id,
                        &condition_id,
                        "BUY",
                        article_order.price,
                        article_order.size,
                        "article",
                        market_db_id,
                        config.trading.paper_mode,
                    )
                    .await;
            }
            StrategyAction::CancelAll => {
                let _ = order_manager
                    .cancel_market_orders(&condition_id, config.trading.paper_mode)
                    .await;
            }
            StrategyAction::Hold => {
                // Nothing to do
            }
            StrategyAction::Withdraw => {
                let _ = order_manager
                    .cancel_market_orders(&condition_id, config.trading.paper_mode)
                    .await;
            }
        }

        // 7. Track cycle time
        let elapsed = cycle_start.elapsed();
        if elapsed > Duration::from_millis(100) {
            warn!(
                condition_id = %condition_id,
                elapsed_ms = elapsed.as_millis(),
                "cancel/replace cycle exceeded 100ms target"
            );
        }
        debug!(
            condition_id = %condition_id,
            elapsed_ms = elapsed.as_millis(),
            "cycle complete"
        );

        // Sleep for remainder of cycle interval
        if elapsed < cycle_interval {
            time::sleep(cycle_interval - elapsed).await;
        } else {
            // Yield to avoid starving other tasks
            tokio::task::yield_now().await;
        }
    }
}

/// Compare new quotes against outstanding orders and cancel/replace as needed.
async fn execute_quote_update(
    order_manager: &OrderManager,
    condition_id: &str,
    yes_token_id: &str,
    no_token_id: &str,
    market_db_id: i64,
    quotes: &crate::strategy::pricing::QuoteSet,
    tick_size: f64,
    paper_mode: bool,
) {
    let existing = order_manager.get_market_orders(condition_id);

    // Build desired quote map: (token_id, side) -> (price, size)
    let desired = vec![
        (yes_token_id, "BUY", quotes.yes_bid, quotes.yes_bid_size),
        (yes_token_id, "SELL", quotes.yes_ask, quotes.yes_ask_size),
        (no_token_id, "BUY", quotes.no_bid, quotes.no_bid_size),
        (no_token_id, "SELL", quotes.no_ask, quotes.no_ask_size),
    ];

    // Check which existing orders need to be cancelled (price moved > 1 tick)
    let mut to_cancel = Vec::new();
    let mut covered: Vec<(String, String)> = Vec::new(); // (token_id, side) pairs already covered

    for order in &existing {
        let matching_desired = desired.iter().find(|(tid, side, _, _)| {
            *tid == order.token_id && *side == order.side
        });

        match matching_desired {
            Some((_, _, desired_price, _)) => {
                let price_diff = (order.price - desired_price).abs();
                if price_diff > tick_size * 0.99 {
                    // Price moved more than 1 tick — cancel and replace
                    to_cancel.push(order.order_id.clone());
                } else {
                    // Price is close enough — keep this order
                    covered.push((order.token_id.clone(), order.side.clone()));
                }
            }
            None => {
                // No matching desired quote — cancel orphaned order
                to_cancel.push(order.order_id.clone());
            }
        }
    }

    // Cancel stale orders
    for order_id in &to_cancel {
        if let Err(e) = order_manager.cancel_order(order_id, paper_mode).await {
            warn!(order_id, error = %e, "failed to cancel stale order");
        }
    }

    // Post new/replacement orders for uncovered slots
    for (token_id, side, price, size) in &desired {
        if size <= &0.0 || price <= &0.0 {
            continue;
        }

        let is_covered = covered.iter().any(|(tid, s)| tid == *token_id && s == *side);

        if !is_covered {
            if let Err(e) = order_manager
                .post_order(
                    token_id,
                    condition_id,
                    side,
                    *price,
                    *size,
                    "quoting",
                    market_db_id,
                    paper_mode,
                )
                .await
            {
                warn!(
                    token_id,
                    side,
                    price,
                    size,
                    error = %e,
                    "failed to post replacement order"
                );
            }
        }
    }
}
