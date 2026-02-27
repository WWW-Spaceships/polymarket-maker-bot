//! Configuration — TOML file defaults + environment variable overrides.
//!
//! Strategy parameters live in `config/default.toml`.
//! Secrets (private key, API keys, tokens) come from environment variables.

use serde::Deserialize;
use std::env;

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub auth: AuthConfig,
    pub clob: ClobConfig,
    pub trading: TradingConfig,
    pub strategy: StrategyConfig,
    pub risk: RiskConfig,
    pub article: ArticleConfig,
    pub arb: ArbConfig,
    pub feeds: FeedConfig,
    pub discovery: DiscoveryConfig,
    pub database: DatabaseConfig,
    pub telegram: TelegramConfig,
    pub web: WebConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub funder_address: String,
    #[serde(default = "default_sig_type")]
    pub signature_type: u8,
    #[serde(default)]
    pub clob_api_key: String,
    #[serde(default)]
    pub clob_api_secret: String,
    #[serde(default)]
    pub clob_api_passphrase: String,
}

fn default_sig_type() -> u8 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClobConfig {
    #[serde(default = "default_clob_url")]
    pub base_url: String,
    #[serde(default = "default_gamma_url")]
    pub gamma_base_url: String,
    #[serde(default = "default_ws_market_url")]
    pub ws_market_url: String,
    #[serde(default = "default_ws_user_url")]
    pub ws_user_url: String,
    #[serde(default = "default_chain_id")]
    pub chain_id: u64,
}

fn default_clob_url() -> String {
    "https://clob.polymarket.com".into()
}
fn default_gamma_url() -> String {
    "https://gamma-api.polymarket.com".into()
}
fn default_ws_market_url() -> String {
    "wss://ws-subscriptions-clob.polymarket.com/ws/market".into()
}
fn default_ws_user_url() -> String {
    "wss://ws-subscriptions-clob.polymarket.com/ws/user".into()
}
fn default_chain_id() -> u64 {
    137
}

#[derive(Debug, Clone, Deserialize)]
pub struct TradingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub paper_mode: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    #[serde(default = "default_min_half_spread")]
    pub min_half_spread: f64,
    #[serde(default = "default_vol_coeff")]
    pub volatility_coefficient: f64,
    #[serde(default = "default_skew_factor")]
    pub inventory_skew_factor: f64,
    #[serde(default = "default_cancel_threshold")]
    pub cancel_threshold_bps: f64,
    #[serde(default = "default_default_order_size")]
    pub default_order_size: f64,
    #[serde(default = "default_quote_size")]
    pub quote_size: f64,
}

fn default_min_half_spread() -> f64 {
    0.01
}
fn default_vol_coeff() -> f64 {
    0.5
}
fn default_skew_factor() -> f64 {
    0.02
}
fn default_cancel_threshold() -> f64 {
    2.0
}
fn default_default_order_size() -> f64 {
    10.0
}
fn default_quote_size() -> f64 {
    20.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    #[serde(default = "default_max_position")]
    pub max_yes_position: f64,
    #[serde(default = "default_max_position")]
    pub max_no_position: f64,
    #[serde(default = "default_max_exposure")]
    pub max_total_exposure_usd: f64,
    #[serde(default = "default_max_markets")]
    pub max_concurrent_markets: usize,
    #[serde(default = "default_max_daily_loss")]
    pub max_daily_loss_usd: f64,
    #[serde(default = "default_max_order_size")]
    pub max_order_size: f64,
    #[serde(default = "default_min_order_size")]
    pub min_order_size: f64,
    #[serde(default = "default_max_imbalance")]
    pub max_inventory_imbalance: f64,
}

fn default_max_position() -> f64 {
    100.0
}
fn default_max_exposure() -> f64 {
    500.0
}
fn default_max_markets() -> usize {
    8
}
fn default_max_daily_loss() -> f64 {
    50.0
}
fn default_max_order_size() -> f64 {
    50.0
}
fn default_min_order_size() -> f64 {
    5.0
}
fn default_max_imbalance() -> f64 {
    80.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArticleConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_article_bn_delta")]
    pub min_bn_delta_60s_bps: f64,
    #[serde(default = "default_article_cb_delta")]
    pub min_cb_delta_60s_bps: f64,
    #[serde(default = "default_article_trending_mid")]
    pub min_trending_mid: f64,
    #[serde(default = "default_article_size")]
    pub article_size: f64,
    #[serde(default = "default_article_price_cap")]
    pub entry_price_cap: f64,
    #[serde(default = "default_article_trigger_secs")]
    pub trigger_seconds_before_close: f64,
    #[serde(default = "default_article_min_secs")]
    pub min_seconds_before_close: f64,
}

fn default_true() -> bool {
    true
}
fn default_article_bn_delta() -> f64 {
    15.0
}
fn default_article_cb_delta() -> f64 {
    10.0
}
fn default_article_trending_mid() -> f64 {
    0.75
}
fn default_article_size() -> f64 {
    20.0
}
fn default_article_price_cap() -> f64 {
    0.95
}
fn default_article_trigger_secs() -> f64 {
    10.0
}
fn default_article_min_secs() -> f64 {
    2.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArbConfig {
    /// Monitor-only mode — log neg-vig opportunities without acting on them.
    #[serde(default = "default_true")]
    pub monitor_only: bool,
    /// Minimum edge in cents to trigger a Telegram alert.
    #[serde(default = "default_min_alert_edge")]
    pub min_alert_edge_cents: f64,
    /// USD to allocate per side of each arb (e.g. 0.50 = $0.50 UP + $0.50 DN = $1 total).
    #[serde(default = "default_order_size_usd")]
    pub order_size_usd: f64,
}

fn default_order_size_usd() -> f64 {
    0.50
}

fn default_min_alert_edge() -> f64 {
    0.005
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeedConfig {
    #[serde(default = "default_binance_ws")]
    pub binance_ws_url: String,
    #[serde(default = "default_coinbase_ws")]
    pub coinbase_ws_url: String,
    #[serde(default = "default_oracle_stale")]
    pub oracle_stale_max_ms: f64,
    #[serde(default = "default_book_stale")]
    pub book_stale_max_ms: f64,
    #[serde(default = "default_ws_market_url")]
    pub polymarket_ws_url: String,
}

fn default_binance_ws() -> String {
    "wss://stream.binance.com:9443/ws".into()
}
fn default_coinbase_ws() -> String {
    "wss://ws-feed.exchange.coinbase.com".into()
}
fn default_oracle_stale() -> f64 {
    2000.0
}
fn default_book_stale() -> f64 {
    3000.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_allowed_assets")]
    pub allowed_assets: Vec<String>,
    #[serde(default = "default_allowed_timeframes")]
    pub allowed_timeframes: Vec<String>,
    #[serde(default = "default_max_tracked")]
    pub max_tracked_markets: usize,
}

fn default_poll_interval() -> u64 {
    1500
}
fn default_allowed_assets() -> Vec<String> {
    vec!["btc".into(), "eth".into()]
}
fn default_allowed_timeframes() -> Vec<String> {
    vec!["5min".into(), "15min".into()]
}
fn default_max_tracked() -> usize {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: Option<String>,
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    8080
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub json_output: bool,
}

fn default_log_level() -> String {
    "info".into()
}

impl Config {
    /// Load configuration from `config/default.toml` merged with env vars.
    /// Secrets come from env vars prefixed with `PM_`.
    pub fn load() -> anyhow::Result<Self> {
        let builder = config::Config::builder()
            .add_source(config::File::with_name("config/default").required(false))
            .add_source(
                config::Environment::with_prefix("PM")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        let mut cfg: Config = builder.try_deserialize()?;

        // Override secrets from env (these should never be in TOML)
        if let Ok(v) = env::var("PRIVATE_KEY") {
            cfg.auth.private_key = v;
        }
        if let Ok(v) = env::var("FUNDER_ADDRESS") {
            cfg.auth.funder_address = v;
        }
        if let Ok(v) = env::var("SIGNATURE_TYPE") {
            cfg.auth.signature_type = v.parse().unwrap_or(1);
        }
        if let Ok(v) = env::var("CLOB_API_KEY") {
            cfg.auth.clob_api_key = v;
        }
        if let Ok(v) = env::var("CLOB_SECRET") {
            cfg.auth.clob_api_secret = v;
        }
        if let Ok(v) = env::var("CLOB_PASSPHRASE") {
            cfg.auth.clob_api_passphrase = v;
        }
        if let Ok(v) = env::var("DATABASE_URL") {
            cfg.database.url = v;
        }
        if let Ok(v) = env::var("TELEGRAM_BOT_TOKEN") {
            cfg.telegram.bot_token = Some(v);
        }
        if let Ok(v) = env::var("TELEGRAM_CHAT_ID") {
            cfg.telegram.chat_id = Some(v);
        }

        Ok(cfg)
    }
}
