//! Unified application error type.

use crate::config::ConfigError;

/// Convenience alias for the crate-wide result type.
pub type Result<T> = std::result::Result<T, AppError>;

/// Top-level error for `agently-cli-mcp`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A configuration error.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Logging failed to initialize.
    #[error("logging initialization failed: {0}")]
    Logging(String),

    /// A catch-all for otherwise-unclassified failures.
    #[error("{0}")]
    Other(String),
}
