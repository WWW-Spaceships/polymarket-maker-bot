//! Gamma API market discovery — polls for active crypto markets.

use dashmap::DashMap;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, info, warn};

use crate::config::{Config, DiscoveryConfig};
use crate::db::queries;
use crate::events::bus::{BotEvent, EventBus};
use crate::executor::cancel_replace;
use crate::executor::order_manager::OrderManager;
use crate::feeds::FeedHub;
use crate::position::tracker::PositionTracker;
use crate::strategy::engine::{MarketContext, MarketState, StrategyEngine};
use crate::strategy::risk::RiskManager;

/// A market returned from the Gamma API.
#[derive(Debug, Deserialize)]
pub struct GammaMarket {
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub tokens: Option<Vec<GammaToken>>,
    #[serde(rename = "startDate")]
    pub start_date: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(rename = "negRisk")]
    pub neg_risk: Option<bool>,
    #[serde(rename = "minimumOrderSize")]
    pub minimum_order_size: Option<f64>,
    #[serde(rename = "minimumTickSize")]
    pub minimum_tick_size: Option<f64>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
}

/// A token within a Gamma market.
#[derive(Debug, Deserialize)]
pub struct GammaToken {
    pub token_id: Option<String>,
    pub outcome: Option<String>,
}

/// Discovers and manages active markets from the Gamma API.
pub struct GammaDiscovery {
    config: DiscoveryConfig,
    gamma_url: String,
    client: Client,
    db: PgPool,
    /// Currently tracked markets by condition_id.
    pub tracked: DashMap<String, Arc<tokio::sync::Mutex<MarketContext>>>,
}

impl GammaDiscovery {
    pub fn new(config: DiscoveryConfig, gamma_url: String, db: PgPool) -> Self {
        Self {
            config,
            gamma_url,
            client: Client::new(),
            db,
            tracked: DashMap::new(),
        }
    }

    /// Fetch active markets from Gamma API.
    pub async fn fetch_markets(&self) -> anyhow::Result<Vec<GammaMarket>> {
        let url = format!("{}/markets", self.gamma_url);
        let response = self
            .client
            .get(&url)
            .query(&[
                ("active", "true"),
                ("closed", "false"),
                ("limit", "100"),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Gamma API returned {}",
                response.status()
            ));
        }

        let markets: Vec<GammaMarket> = response.json().await?;
        Ok(markets)
    }

    /// Parse asset and timeframe from a market question.
    fn parse_question(question: &str) -> Option<(String, String)> {
        let q = question.to_lowercase();

        // Detect asset
        let asset = if q.contains("bitcoin") || q.contains("btc") {
            "btc"
        } else if q.contains("ethereum") || q.contains("eth") {
            "eth"
        } else {
            return None;
        };

        // Detect timeframe
        let timeframe = if q.contains("5 minute") || q.contains("5-min") || q.contains("5min") {
            "5min"
        } else if q.contains("15 minute") || q.contains("15-min") || q.contains("15min") {
            "15min"
        } else if q.contains("1 hour") || q.contains("1-hour") || q.contains("1hr") {
            "1hr"
        } else {
            return None;
        };

        Some((asset.to_string(), timeframe.to_string()))
    }

    /// Filter and create MarketContext from Gamma data.
    pub fn process_market(&self, gm: &GammaMarket) -> Option<MarketContext> {
        let condition_id = gm.condition_id.as_ref()?;
        let question = gm.question.as_ref()?;

        // Already tracking?
        if self.tracked.contains_key(condition_id) {
            return None;
        }

        // Parse asset + timeframe
        let (asset, timeframe) = Self::parse_question(question)?;

        // Check allowed assets/timeframes
        if !self.config.allowed_assets.contains(&asset) {
            return None;
        }
        if !self.config.allowed_timeframes.contains(&timeframe) {
            return None;
        }

        // Check max tracked
        if self.tracked.len() >= self.config.max_tracked_markets {
            return None;
        }

        // Must have tokens
        let tokens = gm.tokens.as_ref()?;
        if tokens.len() < 2 {
            return None;
        }

        // Find YES and NO tokens
        let yes_token = tokens
            .iter()
            .find(|t| t.outcome.as_deref() == Some("Yes"))?;
        let no_token = tokens
            .iter()
            .find(|t| t.outcome.as_deref() == Some("No"))?;

        let yes_token_id = yes_token.token_id.as_ref()?.clone();
        let no_token_id = no_token.token_id.as_ref()?.clone();

        // Parse times
        let start_time = parse_iso_to_epoch(gm.start_date.as_deref()?)?;
        let end_time = parse_iso_to_epoch(gm.end_date.as_deref()?)?;

        // Market must not have already ended
        let now = now_ts();
        if end_time <= now {
            return None;
        }

        Some(MarketContext {
            condition_id: condition_id.clone(),
            asset,
            timeframe,
            yes_token_id,
            no_token_id,
            start_time,
            end_time,
            state: MarketState::Discovered,
            neg_risk: gm.neg_risk.unwrap_or(false),
            tick_size: gm.minimum_tick_size.unwrap_or(0.01),
            min_order_size: gm.minimum_order_size.unwrap_or(5.0),
            db_market_id: None,
        })
    }
}

/// Main discovery loop — polls Gamma, spawns cancel/replace loops for new markets.
pub async fn run_discovery_loop(
    discovery: Arc<GammaDiscovery>,
    feed_hub: Arc<FeedHub>,
    strategy_engine: Arc<StrategyEngine>,
    order_manager: Arc<OrderManager>,
    risk_manager: Arc<RiskManager>,
    position_tracker: Arc<PositionTracker>,
    event_bus: Arc<EventBus>,
    db: PgPool,
    config: Config,
) {
    let interval = Duration::from_millis(discovery.config.poll_interval_ms);
    let config = Arc::new(config);

    info!(
        poll_interval_ms = discovery.config.poll_interval_ms,
        allowed_assets = ?discovery.config.allowed_assets,
        allowed_timeframes = ?discovery.config.allowed_timeframes,
        "discovery loop started"
    );

    loop {
        match discovery.fetch_markets().await {
            Ok(markets) => {
                debug!(count = markets.len(), "fetched markets from Gamma");

                for gm in &markets {
                    if let Some(mut ctx) = discovery.process_market(gm) {
                        let condition_id = ctx.condition_id.clone();

                        // Insert into DB
                        match queries::insert_market(
                            &db,
                            &ctx.condition_id,
                            &ctx.asset,
                            &ctx.timeframe,
                            ctx.start_time,
                            ctx.end_time,
                        )
                        .await
                        {
                            Ok(db_id) => {
                                ctx.db_market_id = Some(db_id);
                            }
                            Err(e) => {
                                warn!(
                                    condition_id = %condition_id,
                                    error = %e,
                                    "failed to insert market in DB"
                                );
                            }
                        }

                        info!(
                            condition_id = %condition_id,
                            asset = %ctx.asset,
                            timeframe = %ctx.timeframe,
                            secs_left = ctx.seconds_until_close(),
                            "discovered new market"
                        );

                        event_bus.publish(BotEvent::MarketDiscovered {
                            condition_id: condition_id.clone(),
                            asset: ctx.asset.clone(),
                            timeframe: ctx.timeframe.clone(),
                        });

                        // Subscribe to orderbook feeds
                        feed_hub
                            .subscribe_market(&ctx.yes_token_id, &ctx.no_token_id)
                            .await;

                        // Create shared market context
                        let market = Arc::new(tokio::sync::Mutex::new(ctx));

                        // Track it
                        discovery
                            .tracked
                            .insert(condition_id.clone(), market.clone());

                        // Spawn cancel/replace loop for this market
                        let fh = feed_hub.clone();
                        let se = strategy_engine.clone();
                        let om = order_manager.clone();
                        let rm = risk_manager.clone();
                        let pt = position_tracker.clone();
                        let eb = event_bus.clone();
                        let cfg = config.clone();
                        let db2 = db.clone();
                        let disc = discovery.clone();

                        tokio::spawn(async move {
                            cancel_replace::run_cancel_replace_loop(
                                market, fh, se, om, rm, pt, eb, cfg, db2,
                            )
                            .await;

                            // Clean up when market loop ends
                            disc.tracked.remove(&condition_id);
                            info!(condition_id = %condition_id, "market removed from tracking");
                        });
                    }
                }

                // Update risk manager with active market count
                risk_manager.set_active_market_count(discovery.tracked.len());
            }
            Err(e) => {
                warn!(error = %e, "failed to fetch markets from Gamma API");
            }
        }

        // Clean up expired markets that the loop hasn't caught yet
        let to_remove: Vec<String> = discovery
            .tracked
            .iter()
            .filter(|entry| {
                if let Ok(ctx) = entry.value().try_lock() {
                    ctx.seconds_until_close() <= 0.0
                } else {
                    false
                }
            })
            .map(|entry| entry.key().clone())
            .collect();

        for cid in &to_remove {
            discovery.tracked.remove(cid);
        }

        time::sleep(interval).await;
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn parse_iso_to_epoch(s: &str) -> Option<f64> {
    use chrono::{DateTime, Utc};
    s.parse::<DateTime<Utc>>().ok().map(|dt| dt.timestamp() as f64)
}
