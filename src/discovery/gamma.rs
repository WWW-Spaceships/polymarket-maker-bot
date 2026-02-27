//! Gamma API market discovery — deterministic slug-based discovery for crypto Up/Down markets.
//!
//! Instead of paginating `/markets`, we compute what slugs MUST exist based on
//! the current time, then query `/events?slug={slug}` for each candidate.
//! This avoids rate limiting and guarantees we find the right markets.
//!
//! Slug patterns:
//!   5min:  btc-updown-5m-{epoch_start}    (aligned to 300s boundaries)
//!   15min: btc-updown-15m-{epoch_start}   (aligned to 900s boundaries)
//!   1hr:   bitcoin-up-or-down-{month}-{day}-{hour12}{ampm}-et

use dashmap::DashMap;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
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

// ── Gamma API response types ───────────────────────────────────────

/// An event returned from `/events?slug=...`
#[derive(Debug, Deserialize)]
pub struct GammaEvent {
    pub id: Option<String>,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub markets: Option<Vec<GammaMarket>>,
}

/// A market within a Gamma event.
#[derive(Debug, Deserialize)]
pub struct GammaMarket {
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub outcomes: Option<serde_json::Value>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<serde_json::Value>,
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
    #[serde(rename = "orderPriceMinTickSize")]
    pub order_price_min_tick_size: Option<f64>,
    #[serde(rename = "orderMinSize")]
    pub order_min_size: Option<i64>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
}

/// A token within a Gamma market (may be null for Up/Down markets).
#[derive(Debug, Deserialize)]
pub struct GammaToken {
    pub token_id: Option<String>,
    pub outcome: Option<String>,
}

// ── Discovery service ──────────────────────────────────────────────

/// Discovers and manages active markets using deterministic slug generation.
pub struct GammaDiscovery {
    config: DiscoveryConfig,
    gamma_url: String,
    client: Client,
    db: PgPool,
    /// Currently tracked markets by condition_id.
    pub tracked: DashMap<String, Arc<tokio::sync::Mutex<MarketContext>>>,
    /// Notifier to trigger WS reconnect when new tokens are added.
    pub reconnect_notify: Arc<Notify>,
}

impl GammaDiscovery {
    pub fn new(config: DiscoveryConfig, gamma_url: String, db: PgPool) -> Self {
        Self {
            config,
            gamma_url,
            client: Client::new(),
            db,
            tracked: DashMap::new(),
            reconnect_notify: Arc::new(Notify::new()),
        }
    }

    /// Generate candidate slugs for the current time window.
    /// Returns Vec<(slug, asset, timeframe, start_epoch, end_epoch)>.
    fn candidate_slugs(&self) -> Vec<(String, String, String, f64, f64)> {
        let now_secs = now_ts() as i64;
        let mut slugs = Vec::new();

        for asset in &self.config.allowed_assets {
            let asset_prefix = match asset.as_str() {
                "btc" => "btc",
                "eth" => "eth",
                _ => continue,
            };

            for tf in &self.config.allowed_timeframes {
                match tf.as_str() {
                    "5min" => {
                        // 5-minute markets aligned to 300s boundaries
                        let interval = 300i64;
                        // Look back 1 window and ahead 2 windows
                        let from = (now_secs / interval) * interval - interval;
                        let to = (now_secs / interval) * interval + interval * 2;

                        let mut start = from;
                        while start <= to {
                            let slug = format!("{}-updown-5m-{}", asset_prefix, start);
                            slugs.push((
                                slug,
                                asset.clone(),
                                tf.clone(),
                                start as f64,
                                (start + interval) as f64,
                            ));
                            start += interval;
                        }
                    }
                    "15min" => {
                        // 15-minute markets aligned to 900s boundaries
                        let interval = 900i64;
                        let from = (now_secs / interval) * interval - interval;
                        let to = (now_secs / interval) * interval + interval;

                        let mut start = from;
                        while start <= to {
                            let slug = format!("{}-updown-15m-{}", asset_prefix, start);
                            slugs.push((
                                slug,
                                asset.clone(),
                                tf.clone(),
                                start as f64,
                                (start + interval) as f64,
                            ));
                            start += interval;
                        }
                    }
                    _ => {}
                }
            }
        }

        slugs
    }

    /// Fetch a single event by slug from Gamma API.
    async fn fetch_event_by_slug(&self, slug: &str) -> anyhow::Result<Option<GammaEvent>> {
        let url = format!("{}/events", self.gamma_url);
        let response = self
            .client
            .get(&url)
            .query(&[("slug", slug)])
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow::anyhow!("429 rate limited"));
        }

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Gamma API returned {}",
                response.status()
            ));
        }

        // The /events endpoint returns an array
        let events: Vec<GammaEvent> = response.json().await?;
        Ok(events.into_iter().next())
    }

    /// Extract token IDs from a Gamma market.
    /// Handles both `clobTokenIds` (JSON string array) and `tokens` array.
    /// Returns (up_token_id, down_token_id) — mapped to (yes_token_id, no_token_id) in our system.
    fn extract_tokens(gm: &GammaMarket) -> Option<(String, String)> {
        // Strategy 1: Parse outcomes + clobTokenIds (the common path for Up/Down markets)
        if let (Some(outcomes_val), Some(clob_val)) = (&gm.outcomes, &gm.clob_token_ids) {
            let outcomes = parse_json_string_or_array(outcomes_val);
            let clob_ids = parse_json_string_or_array(clob_val);

            if outcomes.len() >= 2 && clob_ids.len() >= 2 {
                // Find Up and Down positions
                let mut up_idx = None;
                let mut down_idx = None;
                let mut yes_idx = None;
                let mut no_idx = None;

                for (i, o) in outcomes.iter().enumerate() {
                    let lower = o.to_lowercase();
                    if lower == "up" || lower == "yes" {
                        up_idx = Some(i);
                    }
                    if lower == "down" || lower == "no" {
                        down_idx = Some(i);
                    }
                    if lower == "yes" {
                        yes_idx = Some(i);
                    }
                    if lower == "no" {
                        no_idx = Some(i);
                    }
                }

                // Prefer Up/Down naming, fall back to Yes/No
                let (first, second) = if let (Some(u), Some(d)) = (up_idx, down_idx) {
                    (u, d)
                } else if let (Some(y), Some(n)) = (yes_idx, no_idx) {
                    (y, n)
                } else {
                    // Default: first = yes/up, second = no/down
                    (0, 1)
                };

                return Some((clob_ids[first].clone(), clob_ids[second].clone()));
            }
        }

        // Strategy 2: From tokens array
        if let Some(tokens) = &gm.tokens {
            if tokens.len() >= 2 {
                let up_token = tokens.iter().find(|t| {
                    let o = t.outcome.as_deref().unwrap_or("").to_lowercase();
                    o == "up" || o == "yes"
                });
                let down_token = tokens.iter().find(|t| {
                    let o = t.outcome.as_deref().unwrap_or("").to_lowercase();
                    o == "down" || o == "no"
                });

                if let (Some(u), Some(d)) = (up_token, down_token) {
                    if let (Some(uid), Some(did)) = (&u.token_id, &d.token_id) {
                        return Some((uid.clone(), did.clone()));
                    }
                }
            }
        }

        None
    }

    /// Process a Gamma market into a MarketContext.
    pub fn process_market(
        &self,
        gm: &GammaMarket,
        asset: &str,
        timeframe: &str,
        slug_start: f64,
        slug_end: f64,
    ) -> Option<MarketContext> {
        let condition_id = gm.condition_id.as_ref()?;

        // Already tracking?
        if self.tracked.contains_key(condition_id) {
            return None;
        }

        // Must be active and not closed
        if gm.active == Some(false) || gm.closed == Some(true) {
            return None;
        }

        // Check max tracked
        if self.tracked.len() >= self.config.max_tracked_markets {
            return None;
        }

        // Extract token IDs
        let (yes_token_id, no_token_id) = Self::extract_tokens(gm)?;

        // Parse times — prefer API dates, fall back to slug-computed times
        let start_time = gm
            .start_date
            .as_deref()
            .and_then(parse_iso_to_epoch)
            .unwrap_or(slug_start);
        let end_time = gm
            .end_date
            .as_deref()
            .and_then(parse_iso_to_epoch)
            .unwrap_or(slug_end);

        // Market must not have already ended
        let now = now_ts();
        if end_time <= now {
            return None;
        }

        // Get tick size: prefer orderPriceMinTickSize, then minimumTickSize, default 0.01
        let tick_size = gm
            .order_price_min_tick_size
            .or(gm.minimum_tick_size)
            .unwrap_or(0.01);

        // Get min order size
        let min_order_size = gm
            .order_min_size
            .map(|s| s as f64)
            .or(gm.minimum_order_size)
            .unwrap_or(5.0);

        Some(MarketContext {
            condition_id: condition_id.clone(),
            asset: asset.to_string(),
            timeframe: timeframe.to_string(),
            yes_token_id,
            no_token_id,
            start_time,
            end_time,
            state: MarketState::Discovered,
            neg_risk: gm.neg_risk.unwrap_or(false),
            tick_size,
            min_order_size,
            db_market_id: None,
        })
    }
}

/// Parse a JSON value that might be a string containing a JSON array, or an actual array.
/// Returns Vec<String> of the elements.
fn parse_json_string_or_array(val: &serde_json::Value) -> Vec<String> {
    // If it's already an array, extract strings
    if let Some(arr) = val.as_array() {
        return arr
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| Some(v.to_string().trim_matches('"').to_string()))
            })
            .collect();
    }

    // If it's a string, try to parse it as JSON array
    if let Some(s) = val.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(s) {
            return parsed
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| Some(v.to_string().trim_matches('"').to_string()))
                })
                .collect();
        }
    }

    Vec::new()
}

/// Main discovery loop — generates candidate slugs, queries Gamma, spawns loops.
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
    let poll_interval = Duration::from_millis(discovery.config.poll_interval_ms);
    let config = Arc::new(config);
    let mut backoff_ms: u64 = 0;

    info!(
        poll_interval_ms = discovery.config.poll_interval_ms,
        allowed_assets = ?discovery.config.allowed_assets,
        allowed_timeframes = ?discovery.config.allowed_timeframes,
        max_tracked = discovery.config.max_tracked_markets,
        "discovery loop started (slug-based)"
    );

    loop {
        // Generate candidate slugs for current time
        let candidates = discovery.candidate_slugs();

        debug!(
            candidate_count = candidates.len(),
            tracked = discovery.tracked.len(),
            "checking slug candidates"
        );

        let mut any_new = false;
        let mut rate_limited = false;

        for (slug, asset, timeframe, start_epoch, end_epoch) in &candidates {
            // Skip if we already have enough markets tracked
            if discovery.tracked.len() >= discovery.config.max_tracked_markets {
                break;
            }

            // Skip slug if the market would already be ended
            if *end_epoch <= now_ts() {
                continue;
            }

            // Check if we already track a market for this slug's time window
            let already_has = discovery.tracked.iter().any(|entry| {
                if let Ok(ctx) = entry.value().try_lock() {
                    ctx.asset == *asset
                        && ctx.timeframe == *timeframe
                        && (ctx.start_time - start_epoch).abs() < 1.0
                } else {
                    false
                }
            });
            if already_has {
                continue;
            }

            // Fetch event by slug
            match discovery.fetch_event_by_slug(slug).await {
                Ok(Some(event)) => {
                    backoff_ms = 0; // Reset backoff on success

                    if let Some(markets) = &event.markets {
                        for gm in markets {
                            if let Some(mut ctx) =
                                discovery.process_market(gm, asset, timeframe, *start_epoch, *end_epoch)
                            {
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
                                    slug = %slug,
                                    secs_left = format!("{:.0}", ctx.seconds_until_close()),
                                    yes_token = &ctx.yes_token_id[..ctx.yes_token_id.len().min(16)],
                                    no_token = &ctx.no_token_id[..ctx.no_token_id.len().min(16)],
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

                                // Notify WS to reconnect with new tokens
                                discovery.reconnect_notify.notify_one();

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

                                any_new = true;
                            }
                        }
                    }
                }
                Ok(None) => {
                    // Event doesn't exist yet — normal for future slugs
                    debug!(slug, "no event found for slug");
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("429") {
                        rate_limited = true;
                        warn!(slug, "rate limited on slug lookup, backing off");
                        break; // Stop querying more slugs this cycle
                    }
                    warn!(slug, error = %e, "failed to fetch event by slug");
                }
            }

            // Small delay between slug queries to be polite
            time::sleep(Duration::from_millis(200)).await;
        }

        // Update risk manager with active market count
        risk_manager.set_active_market_count(discovery.tracked.len());

        if any_new {
            info!(tracked = discovery.tracked.len(), "discovery cycle complete, new markets added");
        }

        // Clean up expired markets
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
            debug!(condition_id = %cid, "removed expired market");
        }

        // Adaptive sleep: longer if rate limited, shorter if we need new markets
        if rate_limited {
            backoff_ms = (backoff_ms + 5000).min(30000);
            warn!(backoff_ms, "rate limited, increasing backoff");
            time::sleep(Duration::from_millis(backoff_ms)).await;
        } else {
            time::sleep(poll_interval).await;
        }
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
    s.parse::<DateTime<Utc>>()
        .ok()
        .map(|dt| dt.timestamp() as f64)
}
