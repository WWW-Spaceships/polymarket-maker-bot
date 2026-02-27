//! Telegram bot — inline keyboard buttons + event alerts.
//!
//! Main menu shown as inline buttons. User taps a button, bot responds.
//! Also broadcasts event alerts (fills, circuit breaker, etc).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::config::TelegramConfig;
use crate::events::bus::BotEvent;
use crate::executor::order_manager::OrderManager;
use crate::feeds::FeedHub;
use crate::position::tracker::PositionTracker;
use crate::discovery::gamma::GammaDiscovery;
use crate::strategy::risk::RiskManager;

/// Shared state the Telegram bot needs to answer queries.
pub struct TelegramState {
    pub position_tracker: Arc<PositionTracker>,
    pub order_manager: Arc<OrderManager>,
    pub feed_hub: Arc<FeedHub>,
    pub discovery: Arc<GammaDiscovery>,
    pub risk_manager: Arc<RiskManager>,
    pub paused: Arc<AtomicBool>,
}

/// Telegram notification + command bot with inline keyboard.
pub struct TelegramBot {
    config: TelegramConfig,
    events: broadcast::Receiver<BotEvent>,
    state: Arc<TelegramState>,
    client: reqwest::Client,
    last_update_id: i64,
}

impl TelegramBot {
    pub fn new(
        config: TelegramConfig,
        events: broadcast::Receiver<BotEvent>,
        state: Arc<TelegramState>,
    ) -> Self {
        Self {
            config,
            events,
            state,
            client: reqwest::Client::new(),
            last_update_id: 0,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let token = match &self.config.bot_token {
            Some(t) => t.clone(),
            None => {
                warn!("telegram bot token not configured, skipping");
                return Ok(());
            }
        };

        let chat_id = match &self.config.chat_id {
            Some(c) => c.clone(),
            None => {
                warn!("telegram chat_id not configured, skipping");
                return Ok(());
            }
        };

        info!("telegram bot started with inline keyboards");

        // Send startup message with main menu
        let _ = self.send_with_keyboard(
            &token, &chat_id,
            "🟢 <b>Polymarket Maker Bot Online</b>\nPaper mode active",
            &main_menu_keyboard(),
        ).await;

        let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    match self.get_updates(&token).await {
                        Ok(updates) => {
                            for update in updates {
                                match update {
                                    Update::Message { update_id, text, chat_id: msg_cid } => {
                                        self.last_update_id = update_id + 1;
                                        if msg_cid.to_string() == chat_id {
                                            let (response, kb) = self.handle_input(&text);
                                            let _ = self.send_with_keyboard(&token, &chat_id, &response, &kb).await;
                                        }
                                    }
                                    Update::Callback { update_id, callback_id, data, chat_id: cb_cid } => {
                                        self.last_update_id = update_id + 1;
                                        // Acknowledge the callback
                                        let _ = self.answer_callback(&token, &callback_id).await;
                                        if cb_cid.to_string() == chat_id {
                                            let (response, kb) = self.handle_input(&data);
                                            let _ = self.send_with_keyboard(&token, &chat_id, &response, &kb).await;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "failed to poll telegram updates");
                        }
                    }
                }
                event = self.events.recv() => {
                    match event {
                        Ok(ev) => {
                            if let Some(msg) = format_event(&ev) {
                                let _ = self.send_message(&token, &chat_id, &msg).await;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "telegram event receiver lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("event bus closed, telegram bot shutting down");
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle any input (text command or callback data) and return (response, keyboard).
    fn handle_input(&self, input: &str) -> (String, serde_json::Value) {
        let cmd = input.trim().to_lowercase();
        match cmd.as_str() {
            "/start" | "/help" | "menu" | "back" => {
                (self.txt_status_brief(), main_menu_keyboard())
            }
            "status" | "/status" => {
                (self.txt_status(), back_keyboard())
            }
            "positions" | "/positions" | "/pos" => {
                (self.txt_positions(), back_keyboard())
            }
            "orders" | "/orders" => {
                (self.txt_orders(), back_keyboard())
            }
            "markets" | "/markets" => {
                (self.txt_markets(), back_keyboard())
            }
            "pnl" | "/pnl" => {
                (self.txt_pnl(), back_keyboard())
            }
            "pause" | "/pause" => {
                self.state.paused.store(true, Ordering::SeqCst);
                ("⏸ <b>Quoting paused</b>\nAll new orders halted.".to_string(), pause_resume_keyboard(true))
            }
            "resume" | "/resume" => {
                self.state.paused.store(false, Ordering::SeqCst);
                ("▶️ <b>Quoting resumed</b>".to_string(), pause_resume_keyboard(false))
            }
            "feeds" | "/feeds" => {
                (self.txt_feeds(), back_keyboard())
            }
            _ => {
                ("Tap a button below 👇".to_string(), main_menu_keyboard())
            }
        }
    }

    fn txt_status_brief(&self) -> String {
        let feed_status = self.state.feed_hub.feed_status();
        let tracked = self.state.discovery.tracked.len();
        let orders = self.state.order_manager.live_order_count();
        let paused = self.state.paused.load(Ordering::SeqCst);

        format!(
            "🤖 <b>Polymarket Maker Bot</b>\n\n\
            {} | Markets: {} | Orders: {}\n\
            Binance {} Coinbase {}",
            if paused { "⏸ Paused" } else { "▶️ Running" },
            tracked,
            orders,
            if feed_status.binance_connected { "🟢" } else { "🔴" },
            if feed_status.coinbase_connected { "🟢" } else { "🔴" },
        )
    }

    fn txt_status(&self) -> String {
        let feed_status = self.state.feed_hub.feed_status();
        let live_orders = self.state.order_manager.live_order_count();
        let positions = self.state.position_tracker.position_count();
        let exposure = self.state.position_tracker.get_total_exposure_usd();
        let tracked_markets = self.state.discovery.tracked.len();
        let paused = self.state.paused.load(Ordering::SeqCst);
        let risk = self.state.risk_manager.state();

        format!(
            "📊 <b>Full Status</b>\n\n\
            State: {}\n\
            Tracked markets: {}\n\
            Live orders: {}\n\
            Open positions: {}\n\
            Exposure: ${:.2}\n\
            Daily PnL: ${:.2}\n\
            Circuit breaker: {}\n\n\
            <b>Feeds</b>\n\
            Binance: {} (age: {:.0}ms)\n\
            Coinbase: {} (age: {:.0}ms)",
            if paused { "⏸ PAUSED" } else { "▶️ RUNNING" },
            tracked_markets,
            live_orders,
            positions,
            exposure,
            risk.daily_pnl,
            if risk.halted { "🔴 TRIPPED" } else { "✅ OK" },
            if feed_status.binance_connected { "🟢" } else { "🔴" },
            feed_status.binance_last_tick_ms,
            if feed_status.coinbase_connected { "🟢" } else { "🔴" },
            feed_status.coinbase_last_tick_ms,
        )
    }

    fn txt_positions(&self) -> String {
        let positions = self.state.position_tracker.all_positions();
        let active: Vec<_> = positions.iter().filter(|p| p.shares > 0.0).collect();

        if active.is_empty() {
            return "📦 <b>Positions</b>\n\nNo open positions".to_string();
        }

        let mut msg = format!("📦 <b>Positions ({})</b>\n", active.len());
        for pos in &active {
            msg.push_str(&format!(
                "\n<code>{}…</code>\n  {} {:.1} shares @ ${:.4}\n",
                &pos.token_id[..pos.token_id.len().min(12)],
                pos.side,
                pos.shares,
                pos.avg_entry_price,
            ));
        }
        msg
    }

    fn txt_orders(&self) -> String {
        let orders: Vec<_> = self.state.order_manager.live_orders
            .iter()
            .map(|e| e.value().clone())
            .collect();

        if orders.is_empty() {
            return "📋 <b>Orders</b>\n\nNo live orders".to_string();
        }

        let mut msg = format!("📋 <b>Live Orders ({})</b>\n", orders.len());
        for o in &orders {
            msg.push_str(&format!(
                "\n{} {:.1} @ ${:.4}\n  <code>{}…</code> [{}]\n",
                o.side,
                o.size,
                o.price,
                &o.token_id[..o.token_id.len().min(10)],
                o.strategy,
            ));
        }
        msg
    }

    fn txt_markets(&self) -> String {
        let count = self.state.discovery.tracked.len();
        if count == 0 {
            return "🔍 <b>Markets</b>\n\nNo active markets being tracked.\nDiscovery polls Gamma API every 1.5s for BTC/ETH 5min/15min markets.".to_string();
        }

        let mut msg = format!("🔍 <b>Tracked Markets ({})</b>\n", count);
        for entry in self.state.discovery.tracked.iter() {
            if let Ok(ctx) = entry.value().try_lock() {
                let secs_left = ctx.seconds_until_close();
                msg.push_str(&format!(
                    "\n{} {} — {:.0}s left\n  State: {:?}\n",
                    ctx.asset.to_uppercase(),
                    ctx.timeframe,
                    secs_left,
                    ctx.state,
                ));
            }
        }
        msg
    }

    fn txt_pnl(&self) -> String {
        let risk = self.state.risk_manager.state();
        let exposure = self.state.position_tracker.get_total_exposure_usd();

        format!(
            "💰 <b>PnL Summary</b>\n\n\
            Daily PnL: ${:.2}\n\
            Total exposure: ${:.2}\n\
            Active markets: {}\n\
            Circuit breaker: {}",
            risk.daily_pnl,
            exposure,
            risk.active_markets,
            if risk.halted { "🔴 TRIPPED" } else { "✅ OK" },
        )
    }

    fn txt_feeds(&self) -> String {
        let fs = self.state.feed_hub.feed_status();

        format!(
            "📡 <b>Feed Status</b>\n\n\
            Binance: {} (last tick {:.0}ms ago)\n\
            Coinbase: {} (last tick {:.0}ms ago)",
            if fs.binance_connected { "🟢 Connected" } else { "🔴 Disconnected" },
            fs.binance_last_tick_ms,
            if fs.coinbase_connected { "🟢 Connected" } else { "🔴 Disconnected" },
            fs.coinbase_last_tick_ms,
        )
    }

    // --- Telegram API helpers ---

    async fn get_updates(&self, token: &str) -> anyhow::Result<Vec<Update>> {
        let url = format!("https://api.telegram.org/bot{}/getUpdates", token);

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("offset", self.last_update_id.to_string()),
                ("timeout", "0".to_string()),
                ("allowed_updates", "[\"message\",\"callback_query\"]".to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let body: serde_json::Value = resp.json().await?;
        let mut results = vec![];

        if let Some(updates) = body.get("result").and_then(|r| r.as_array()) {
            for update in updates {
                let update_id = update.get("update_id").and_then(|u| u.as_i64()).unwrap_or(0);

                // Check for callback_query (button press)
                if let Some(cb) = update.get("callback_query") {
                    let callback_id = cb.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let data = cb.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string();
                    let chat_id = cb
                        .get("message")
                        .and_then(|m| m.get("chat"))
                        .and_then(|c| c.get("id"))
                        .and_then(|id| id.as_i64())
                        .unwrap_or(0);

                    if !data.is_empty() {
                        results.push(Update::Callback { update_id, callback_id, data, chat_id });
                    }
                    continue;
                }

                // Check for regular message
                if let Some(msg) = update.get("message") {
                    let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    let chat_id = msg
                        .get("chat")
                        .and_then(|c| c.get("id"))
                        .and_then(|id| id.as_i64())
                        .unwrap_or(0);

                    if !text.is_empty() {
                        results.push(Update::Message { update_id, text, chat_id });
                    }
                }
            }
        }

        Ok(results)
    }

    async fn answer_callback(&self, token: &str, callback_id: &str) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/answerCallbackQuery", token);
        let _ = self.client.post(&url)
            .json(&serde_json::json!({ "callback_query_id": callback_id }))
            .send()
            .await?;
        Ok(())
    }

    async fn send_with_keyboard(
        &self,
        token: &str,
        chat_id: &str,
        text: &str,
        keyboard: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);

        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
                "reply_markup": keyboard,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            warn!(body = %body, "telegram API error");
        }
        Ok(())
    }

    async fn send_message(&self, token: &str, chat_id: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            warn!(body = %body, "telegram API error");
        }
        Ok(())
    }
}

// --- Update types ---

enum Update {
    Message { update_id: i64, text: String, chat_id: i64 },
    Callback { update_id: i64, callback_id: String, data: String, chat_id: i64 },
}

// --- Inline keyboard builders ---

fn main_menu_keyboard() -> serde_json::Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {"text": "📊 Status", "callback_data": "status"},
                {"text": "💰 PnL", "callback_data": "pnl"},
            ],
            [
                {"text": "📦 Positions", "callback_data": "positions"},
                {"text": "📋 Orders", "callback_data": "orders"},
            ],
            [
                {"text": "🔍 Markets", "callback_data": "markets"},
                {"text": "📡 Feeds", "callback_data": "feeds"},
            ],
            [
                {"text": "⏸ Pause", "callback_data": "pause"},
                {"text": "▶️ Resume", "callback_data": "resume"},
            ],
        ]
    })
}

fn back_keyboard() -> serde_json::Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {"text": "🔄 Refresh", "callback_data": "status"},
                {"text": "⬅️ Menu", "callback_data": "menu"},
            ]
        ]
    })
}

fn pause_resume_keyboard(is_paused: bool) -> serde_json::Value {
    if is_paused {
        serde_json::json!({
            "inline_keyboard": [
                [
                    {"text": "▶️ Resume", "callback_data": "resume"},
                    {"text": "⬅️ Menu", "callback_data": "menu"},
                ]
            ]
        })
    } else {
        serde_json::json!({
            "inline_keyboard": [
                [
                    {"text": "⏸ Pause", "callback_data": "pause"},
                    {"text": "⬅️ Menu", "callback_data": "menu"},
                ]
            ]
        })
    }
}

/// Format a BotEvent into a Telegram alert message.
fn format_event(event: &BotEvent) -> Option<String> {
    match event {
        BotEvent::OrderFilled { order_id, fill_price, fill_size, fully_filled } => {
            let status = if *fully_filled { "FILLED" } else { "PARTIAL" };
            Some(format!(
                "📊 <b>{}</b>\nOrder: <code>{}</code>\nPrice: {:.4} | Size: {:.1}",
                status, &order_id[..order_id.len().min(12)], fill_price, fill_size
            ))
        }
        BotEvent::CircuitBreaker { daily_pnl } => Some(format!(
            "🚨 <b>CIRCUIT BREAKER</b>\nDaily PnL: ${:.2}\nAll quoting halted!", daily_pnl
        )),
        BotEvent::RiskAlert { message, daily_pnl, exposure_usd } => Some(format!(
            "⚠️ <b>Risk Alert</b>\n{}\nPnL: ${:.2} | Exposure: ${:.2}", message, daily_pnl, exposure_usd
        )),
        BotEvent::ArticleFired { condition_id, direction, price, size, conviction } => Some(format!(
            "🎯 <b>ARTICLE FIRED</b>\nMarket: <code>{}</code>\nDir: {} | Price: {:.4} | Size: {:.1}\nConviction: {:.2}",
            &condition_id[..condition_id.len().min(12)], direction, price, size, conviction
        )),
        BotEvent::MarketDiscovered { condition_id, asset, timeframe } => Some(format!(
            "🔍 <b>New Market</b>\n{} {} — <code>{}…</code>",
            asset.to_uppercase(), timeframe, &condition_id[..condition_id.len().min(16)],
        )),
        BotEvent::FeedDisconnected { source } => Some(format!("🔴 <b>Feed Down:</b> {}", source)),
        BotEvent::FeedReconnected { source } => Some(format!("🟢 <b>Feed Up:</b> {}", source)),
        BotEvent::DailySummary { total_pnl, trades_count, win_rate, rebates_usdc } => Some(format!(
            "📈 <b>Daily Summary</b>\nPnL: ${:.2} | Trades: {} | Win: {:.1}%\nRebates: ${:.4}",
            total_pnl, trades_count, win_rate * 100.0, rebates_usdc
        )),
        BotEvent::NegVigDetected { condition_id, asset, timeframe, yes_ask, no_ask, combined, edge_cents, yes_depth, no_depth, secs_left } => Some(format!(
            "💎 <b>NEG-VIG DETECTED</b>\n\
            {} {} — {:.0}s left\n\
            YES ask: {:.4} | NO ask: {:.4}\n\
            Combined: {:.4} | <b>Edge: {:.4}¢</b>\n\
            Depth: YES {:.0} / NO {:.0}\n\
            <code>{}…</code>",
            asset.to_uppercase(), timeframe, secs_left,
            yes_ask, no_ask,
            combined, edge_cents * 100.0,
            yes_depth, no_depth,
            &condition_id[..condition_id.len().min(16)],
        )),
        _ => None,
    }
}
