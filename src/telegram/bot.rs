//! Telegram alert bot — listens for BotEvents and sends formatted alerts.

use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::config::TelegramConfig;
use crate::events::bus::BotEvent;

/// Telegram notification bot.
pub struct TelegramBot {
    config: TelegramConfig,
    events: broadcast::Receiver<BotEvent>,
    client: reqwest::Client,
}

impl TelegramBot {
    pub fn new(config: TelegramConfig, events: broadcast::Receiver<BotEvent>) -> Self {
        Self {
            config,
            events,
            client: reqwest::Client::new(),
        }
    }

    /// Run the Telegram bot event loop.
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

        info!("telegram bot started");

        loop {
            match self.events.recv().await {
                Ok(event) => {
                    if let Some(msg) = format_event(&event) {
                        if let Err(e) = self.send_message(&token, &chat_id, &msg).await {
                            error!(error = %e, "failed to send telegram message");
                        }
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

        Ok(())
    }

    async fn send_message(
        &self,
        token: &str,
        chat_id: &str,
        text: &str,
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
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            warn!(body = %body, "telegram API returned error");
        }

        Ok(())
    }
}

/// Format a BotEvent into a Telegram message. Returns None for events we don't alert on.
fn format_event(event: &BotEvent) -> Option<String> {
    match event {
        BotEvent::OrderFilled {
            order_id,
            fill_price,
            fill_size,
            fully_filled,
        } => {
            let status = if *fully_filled { "FILLED" } else { "PARTIAL" };
            Some(format!(
                "📊 <b>{}</b>\nOrder: <code>{}</code>\nPrice: {:.4} | Size: {:.1}",
                status,
                &order_id[..order_id.len().min(12)],
                fill_price,
                fill_size
            ))
        }
        BotEvent::CircuitBreaker { daily_pnl } => Some(format!(
            "🚨 <b>CIRCUIT BREAKER</b>\nDaily PnL: ${:.2}\nAll quoting halted!",
            daily_pnl
        )),
        BotEvent::RiskAlert {
            message,
            daily_pnl,
            exposure_usd,
        } => Some(format!(
            "⚠️ <b>Risk Alert</b>\n{}\nPnL: ${:.2} | Exposure: ${:.2}",
            message, daily_pnl, exposure_usd
        )),
        BotEvent::ArticleFired {
            condition_id,
            direction,
            price,
            size,
            conviction,
        } => Some(format!(
            "🎯 <b>ARTICLE FIRED</b>\nMarket: <code>{}</code>\nDir: {} | Price: {:.4} | Size: {:.1}\nConviction: {:.2}",
            &condition_id[..condition_id.len().min(12)],
            direction,
            price,
            size,
            conviction
        )),
        BotEvent::FeedDisconnected { source } => {
            Some(format!("🔴 <b>Feed Disconnected:</b> {}", source))
        }
        BotEvent::FeedReconnected { source } => {
            Some(format!("🟢 <b>Feed Reconnected:</b> {}", source))
        }
        BotEvent::DailySummary {
            total_pnl,
            trades_count,
            win_rate,
            rebates_usdc,
        } => Some(format!(
            "📈 <b>Daily Summary</b>\nPnL: ${:.2}\nTrades: {} | Win Rate: {:.1}%\nRebates: ${:.4}",
            total_pnl,
            trades_count,
            win_rate * 100.0,
            rebates_usdc
        )),
        // Don't alert on every order post/cancel — too noisy
        _ => None,
    }
}
