//! Logging initialization: tracing always writes to stderr.

use crate::config::LoggingConfig;
use crate::error::{AppError, Result};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initialize the global tracing subscriber to write to stderr.
pub fn init(cfg: &LoggingConfig) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_new(&cfg.level)
        .map_err(|e| AppError::Logging(format!("invalid log level `{}`: {e}", cfg.level)))?;

    let registry = tracing_subscriber::registry().with(filter);

    let result = if cfg.format == "json" {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::stderr),
            )
            .try_init()
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .try_init()
    };

    result.map_err(|e| AppError::Logging(e.to_string()))
}
