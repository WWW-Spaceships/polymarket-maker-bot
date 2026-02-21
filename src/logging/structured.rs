//! Structured logging setup using tracing-subscriber.

use crate::config::LoggingConfig;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize the global tracing subscriber.
pub fn init_logging(config: &LoggingConfig) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    if config.json_output {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().json().with_target(true).with_thread_ids(false))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .compact(),
            )
            .init();
    }
}
