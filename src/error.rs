//! Unified error types for the maker bot.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum BotError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("feed error: {0}")]
    Feed(String),

    #[error("executor error: {0}")]
    Executor(String),

    #[error("strategy error: {0}")]
    Strategy(String),

    #[error("web server error: {0}")]
    Web(String),

    #[error("telegram error: {0}")]
    Telegram(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("websocket error: {0}")]
    WebSocket(String),

    #[error("signing error: {0}")]
    Signing(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BotError>;
