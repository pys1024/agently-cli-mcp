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

    /// The `agently-cli` executable could not be located.
    #[error("{0}")]
    CliNotFound(String),

    /// A locally invalid argument or request (CLI exit 1/2).
    #[error("{0}")]
    InvalidArgument(String),

    /// Authorization is missing or expired (CLI exit 3).
    #[error("{0}")]
    AuthRequired(String),

    /// A network/connectivity failure (CLI exit 4).
    #[error("{0}")]
    NetworkError(String),

    /// The requested resource does not exist (API 404).
    #[error("{0}")]
    NotFound(String),

    /// The request was rate-limited (CLI exit 7).
    #[error("{0}")]
    RateLimited(String),

    /// The CLI rejected the request (API 400, token mismatch, etc.).
    #[error("{0}")]
    Rejected(String),

    /// The CLI produced output that is not a valid JSON envelope.
    #[error("{0}")]
    ProtocolError(String),

    /// The CLI invocation timed out.
    #[error("{0}")]
    Timeout(String),

    /// A confirmation token is missing, expired, or already used.
    #[error("{0}")]
    ConfirmationExpired(String),

    /// The requested tool is disabled by configuration.
    #[error("{0}")]
    ToolDisabled(String),

    /// A path resolved outside the configured working directory.
    #[error("{0}")]
    PathOutsideWorkdir(String),

    /// A search cursor's stored filters conflict with the newly supplied ones.
    #[error("{0}")]
    SearchContextMismatch(String),

    /// A search cursor context is missing or expired and no full query was
    /// supplied to fall back on.
    #[error("{0}")]
    SearchContextRequired(String),

    /// Another watch is already running; the process-wide watch lock is held.
    #[error("{0}")]
    WatchBusy(String),

    /// A generic CLI failure with no more specific classification.
    #[error("{0}")]
    CliError(String),
}

impl AppError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Config(_) => "CONFIG_ERROR",
            AppError::Logging(_) => "LOGGING_ERROR",
            AppError::Other(_) => "INTERNAL_ERROR",
            AppError::CliNotFound(_) => "CLI_NOT_FOUND",
            AppError::InvalidArgument(_) => "INVALID_ARGUMENT",
            AppError::AuthRequired(_) => "AUTH_REQUIRED",
            AppError::NetworkError(_) => "NETWORK_ERROR",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::RateLimited(_) => "RATE_LIMITED",
            AppError::Rejected(_) => "REJECTED",
            AppError::ProtocolError(_) => "PROTOCOL_ERROR",
            AppError::Timeout(_) => "TIMEOUT",
            AppError::ConfirmationExpired(_) => "CONFIRMATION_EXPIRED",
            AppError::ToolDisabled(_) => "TOOL_DISABLED",
            AppError::PathOutsideWorkdir(_) => "PATH_OUTSIDE_WORKDIR",
            AppError::SearchContextMismatch(_) => "SEARCH_CONTEXT_MISMATCH",
            AppError::SearchContextRequired(_) => "SEARCH_CONTEXT_REQUIRED",
            AppError::WatchBusy(_) => "WATCH_BUSY",
            AppError::CliError(_) => "CLI_ERROR",
        }
    }

    /// Whether the caller may safely retry the operation.
    pub fn retryable(&self) -> bool {
        matches!(self, AppError::RateLimited(_) | AppError::NetworkError(_))
    }
}
