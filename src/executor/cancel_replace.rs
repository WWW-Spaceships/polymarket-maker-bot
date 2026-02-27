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
use crate::events::bus::{BotEvent, EventBus};
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
    event_bus: Arc<EventBus>,
    config: Arc<Config>,
    _db: PgPool,
) {
    let cycle_interval = Duration::from_millis(500); // Phase 0: 500ms is fine for monitoring

    {
        let cid = market.lock().await.condition_id.clone();
        info!(condition_id = %cid, "cancel/replace loop started");
    }

    let mut last_log = Instant::now();

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

        // 3. Get YES + NO token orderbooks
        let yes_book = feed_hub
            .get_book(&ctx.yes_token_id)
            .unwrap_or_else(|| crate::feeds::types::OrderBook::empty(ctx.yes_token_id.clone()));
        let no_book = feed_hub
            .get_book(&ctx.no_token_id)
            .unwrap_or_else(|| crate::feeds::types::OrderBook::empty(ctx.no_token_id.clone()));

        // 3a. Neg-vig monitor — check spreads on both sides
        let yes_ask = yes_book.best_ask();
        let no_ask = no_book.best_ask();
        let yes_bid = yes_book.best_bid();
        let no_bid = no_book.best_bid();
        let secs_left = ctx.seconds_until_close();

        // Periodic book state log (every 5 seconds per market)
        if last_log.elapsed() >= Duration::from_secs(5) && (yes_ask > 0.0 || no_ask > 0.0) {
            let taker_combined = yes_ask + no_ask;
            let taker_edge = 1.0 - taker_combined;
            let maker_combined = yes_bid + no_bid;
            let maker_edge = 1.0 - maker_combined;
            let yes_spread = if yes_ask > 0.0 && yes_bid > 0.0 { yes_ask - yes_bid } else { 0.0 };
            let no_spread = if no_ask > 0.0 && no_bid > 0.0 { no_ask - no_bid } else { 0.0 };
            info!(
                cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                secs = format!("{:.0}", secs_left),
                up_bid = format!("{:.2}", yes_bid),
                up_ask = format!("{:.2}", yes_ask),
                dn_bid = format!("{:.2}", no_bid),
                dn_ask = format!("{:.2}", no_ask),
                taker = format!("{:.3}", taker_combined),
                maker = format!("{:.3}", maker_combined),
                t_edge = format!("{:.4}", taker_edge),
                m_edge = format!("{:.4}", maker_edge),
                up_sprd = format!("{:.2}", yes_spread),
                dn_sprd = format!("{:.2}", no_spread),
                "📊 BOOK"
            );
            last_log = Instant::now();
        }

        // Only check edge when prices are in the realistic zone (0.20-0.80).
        // Bids at $0.01 are bottom-of-book noise, not real trading opportunities.
        let prices_realistic = yes_bid >= 0.20 && no_bid >= 0.20
            && yes_ask <= 0.80 && no_ask <= 0.80
            && yes_ask > 0.0 && no_ask > 0.0;

        if prices_realistic {
            let taker_combined = yes_ask + no_ask;
            let taker_edge = 1.0 - taker_combined;
            let maker_combined = yes_bid + no_bid;
            let maker_edge = 1.0 - maker_combined;

            // Taker edge: both asks sum to < $1.00
            if taker_edge > config.arb.min_alert_edge_cents {
                let yes_depth = yes_book.top_ask_size();
                let no_depth = no_book.top_ask_size();
                info!(
                    condition_id = %ctx.condition_id,
                    yes_ask, no_ask,
                    combined = format!("{:.3}", taker_combined),
                    edge = format!("{:.4}", taker_edge),
                    yes_depth, no_depth,
                    secs = format!("{:.0}", secs_left),
                    "🔥 TAKER NEG-VIG"
                );

                event_bus.publish(BotEvent::NegVigDetected {
                    condition_id: ctx.condition_id.clone(),
                    asset: ctx.asset.clone(),
                    timeframe: ctx.timeframe.clone(),
                    yes_ask,
                    no_ask,
                    combined: taker_combined,
                    edge_cents: taker_edge,
                    yes_depth,
                    no_depth,
                    secs_left,
                });
            }

            // Maker edge: both bids sum to < $1.00 (what 0x8dxd does)
            if maker_edge > config.arb.min_alert_edge_cents {
                let yes_bid_depth = yes_book.top_bid_size();
                let no_bid_depth = no_book.top_bid_size();
                info!(
                    condition_id = %ctx.condition_id,
                    yes_bid, no_bid,
                    combined = format!("{:.3}", maker_combined),
                    edge = format!("{:.4}", maker_edge),
                    yes_depth = yes_bid_depth,
                    no_depth = no_bid_depth,
                    secs = format!("{:.0}", secs_left),
                    "💎 MAKER NEG-VIG"
                );
            }
        }

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

                event_bus.publish(BotEvent::ArticleFired {
                    condition_id: condition_id.clone(),
                    direction: article_order.direction.clone(),
                    price: article_order.price,
                    size: article_order.size,
                    conviction: article_order.conviction,
                });

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
