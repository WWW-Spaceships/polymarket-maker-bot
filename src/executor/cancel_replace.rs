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
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::events::bus::{BotEvent, EventBus};
use crate::feeds::FeedHub;
use crate::position::tracker::PositionTracker;
use crate::strategy::engine::{MarketContext, StrategyAction, StrategyEngine};
use crate::strategy::risk::RiskManager;

use super::order_manager::OrderManager;

/// Track edge state per market to avoid spamming the same alert.
#[derive(Default)]
struct EdgeTracker {
    /// Last maker edge we alerted on (None = no active edge window)
    maker_edge_active: bool,
    /// Last taker edge we alerted on
    taker_edge_active: bool,
    /// When the current edge window opened
    edge_window_start: Option<Instant>,
    /// Last prices we alerted on (for dedup)
    last_yes_bid: f64,
    last_no_bid: f64,
    last_yes_ask: f64,
    last_no_ask: f64,
    /// Whether we already placed arb orders for this edge window
    arb_orders_placed: bool,
    /// How many UP shares we hold (from fills)
    up_shares: f64,
    /// How many DOWN shares we hold (from fills)
    dn_shares: f64,
}

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
    let mut edge_tracker = EdgeTracker::default();

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

        // --- Price realism filter ---
        // Only consider prices in 0.20-0.80 as real tradeable liquidity.
        // Bids at $0.01 / asks at $0.99 are empty-book placeholders.
        let has_real_book = yes_bid >= 0.10 && no_bid >= 0.10
            && yes_ask <= 0.90 && no_ask <= 0.90
            && yes_ask > 0.0 && no_ask > 0.0;

        let maker_combined = yes_bid + no_bid;
        let maker_edge = 1.0 - maker_combined;
        let taker_combined = yes_ask + no_ask;
        let taker_edge = 1.0 - taker_combined;

        // --- Periodic book summary (every 10s, only markets with real books) ---
        if last_log.elapsed() >= Duration::from_secs(10) {
            if has_real_book {
                info!(
                    cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                    secs = format!("{:.0}", secs_left),
                    up = format!("{:.2}/{:.2}", yes_bid, yes_ask),
                    dn = format!("{:.2}/{:.2}", no_bid, no_ask),
                    m_edge = format!("{:.1}¢", maker_edge * 100.0),
                    t_edge = format!("{:.1}¢", taker_edge * 100.0),
                    "📊 BOOK"
                );
            } else {
                debug!(
                    cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                    secs = format!("{:.0}", secs_left),
                    "📊 no real book yet"
                );
            }
            last_log = Instant::now();
        }

        // --- Edge detection with deduplication ---
        if has_real_book {
            let min_edge = config.arb.min_alert_edge_cents;

            // Did prices actually change from last alert? (dedup)
            let prices_changed = (yes_bid - edge_tracker.last_yes_bid).abs() > 0.001
                || (no_bid - edge_tracker.last_no_bid).abs() > 0.001
                || (yes_ask - edge_tracker.last_yes_ask).abs() > 0.001
                || (no_ask - edge_tracker.last_no_ask).abs() > 0.001;

            // --- MAKER edge: posting GTC buys at bid on both sides ---
            if maker_edge > min_edge {
                if !edge_tracker.maker_edge_active || prices_changed {
                    let yes_depth = yes_book.top_bid_size();
                    let no_depth = no_book.top_bid_size();

                    if !edge_tracker.maker_edge_active {
                        edge_tracker.edge_window_start = Some(Instant::now());
                        edge_tracker.arb_orders_placed = false;
                    }

                    info!(
                        cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                        secs = format!("{:.0}", secs_left),
                        up_bid = format!("{:.2}", yes_bid),
                        dn_bid = format!("{:.2}", no_bid),
                        cost = format!("{:.2}¢", maker_combined * 100.0),
                        edge = format!("{:.1}¢", maker_edge * 100.0),
                        up_depth = format!("{:.0}", yes_depth),
                        dn_depth = format!("{:.0}", no_depth),
                        "💎 MAKER EDGE"
                    );
                    edge_tracker.maker_edge_active = true;
                }

                // --- PLACE ARB ORDERS: Buy both UP and DOWN at bid ---
                // Only place once per edge window, only if not already holding shares
                if !edge_tracker.arb_orders_placed
                    && !config.arb.monitor_only
                    && secs_left > 60.0  // Don't enter with < 60s left
                    && edge_tracker.up_shares < 1.0  // Not already holding
                    && edge_tracker.dn_shares < 1.0
                {
                    // $1 per market: split between UP and DOWN
                    // Buy at the bid price on each side
                    let up_size = (config.arb.order_size_usd / yes_bid).floor().max(1.0);
                    let dn_size = (config.arb.order_size_usd / no_bid).floor().max(1.0);

                    // Store values before dropping ctx
                    let cid = ctx.condition_id.clone();
                    let yes_tid = ctx.yes_token_id.clone();
                    let no_tid = ctx.no_token_id.clone();
                    let db_id = ctx.db_market_id.unwrap_or(0);

                    // Drop ctx lock before async I/O
                    drop(ctx);

                    info!(
                        cid = &cid[..cid.len().min(10)],
                        up_price = format!("{:.2}", yes_bid),
                        dn_price = format!("{:.2}", no_bid),
                        up_size = format!("{:.0}", up_size),
                        dn_size = format!("{:.0}", dn_size),
                        total_cost = format!("{:.2}", yes_bid * up_size + no_bid * dn_size),
                        edge = format!("{:.1}¢", maker_edge * 100.0),
                        "🎯 PLACING ARB ORDERS"
                    );

                    // Place UP BUY order
                    match order_manager
                        .post_order(
                            &yes_tid,
                            &cid,
                            "BUY",
                            yes_bid,
                            up_size,
                            "arb_up",
                            db_id,
                            config.trading.paper_mode,
                        )
                        .await
                    {
                        Ok(Some(id)) => info!(order_id = %id, side = "UP", "arb order posted"),
                        Ok(None) => warn!("UP arb order returned no ID"),
                        Err(e) => error!(error = %e, "failed to post UP arb order"),
                    }

                    // Place DOWN BUY order
                    match order_manager
                        .post_order(
                            &no_tid,
                            &cid,
                            "BUY",
                            no_bid,
                            dn_size,
                            "arb_dn",
                            db_id,
                            config.trading.paper_mode,
                        )
                        .await
                    {
                        Ok(Some(id)) => info!(order_id = %id, side = "DN", "arb order posted"),
                        Ok(None) => warn!("DN arb order returned no ID"),
                        Err(e) => error!(error = %e, "failed to post DN arb order"),
                    }

                    edge_tracker.arb_orders_placed = true;

                    // Re-acquire ctx for the rest of the loop
                    // We need to continue to the sleep at the end, skip the strategy engine
                    let elapsed = cycle_start.elapsed();
                    if elapsed < cycle_interval {
                        time::sleep(cycle_interval - elapsed).await;
                    } else {
                        tokio::task::yield_now().await;
                    }
                    continue;
                }
            } else if edge_tracker.maker_edge_active {
                // Edge window closed — log how long it lasted
                let duration = edge_tracker.edge_window_start
                    .map(|s| s.elapsed().as_secs())
                    .unwrap_or(0);
                info!(
                    cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                    lasted_secs = duration,
                    "💎 maker edge window CLOSED"
                );
                edge_tracker.maker_edge_active = false;
                edge_tracker.edge_window_start = None;
                edge_tracker.arb_orders_placed = false;
            }

            // --- TAKER edge: crossing both asks < $1.00 ---
            if taker_edge > min_edge {
                if !edge_tracker.taker_edge_active || prices_changed {
                    let yes_depth = yes_book.top_ask_size();
                    let no_depth = no_book.top_ask_size();

                    info!(
                        cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                        secs = format!("{:.0}", secs_left),
                        up_ask = format!("{:.2}", yes_ask),
                        dn_ask = format!("{:.2}", no_ask),
                        cost = format!("{:.2}¢", taker_combined * 100.0),
                        edge = format!("{:.1}¢", taker_edge * 100.0),
                        up_depth = format!("{:.0}", yes_depth),
                        dn_depth = format!("{:.0}", no_depth),
                        "🔥 TAKER EDGE"
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
                    edge_tracker.taker_edge_active = true;
                }
            } else {
                edge_tracker.taker_edge_active = false;
            }

            // Update last-alerted prices for dedup
            if prices_changed {
                edge_tracker.last_yes_bid = yes_bid;
                edge_tracker.last_no_bid = no_bid;
                edge_tracker.last_yes_ask = yes_ask;
                edge_tracker.last_no_ask = no_ask;
            }
        } else {
            // Book went back to empty/unrealistic — close any active edge windows
            if edge_tracker.maker_edge_active {
                let duration = edge_tracker.edge_window_start
                    .map(|s| s.elapsed().as_secs())
                    .unwrap_or(0);
                info!(
                    cid = &ctx.condition_id[..ctx.condition_id.len().min(10)],
                    lasted_secs = duration,
                    reason = "book emptied",
                    "💎 maker edge window CLOSED"
                );
                edge_tracker.maker_edge_active = false;
                edge_tracker.edge_window_start = None;
            }
            if edge_tracker.taker_edge_active {
                edge_tracker.taker_edge_active = false;
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
