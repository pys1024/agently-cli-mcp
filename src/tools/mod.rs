//! Tool business logic: map MCP tool inputs to `agently-cli` argv and wrap the
//! raw CLI payloads in result envelopes.

pub mod account;
pub mod attachments;
pub mod read;

use std::time::Duration;

use serde_json::Value;

use crate::cli_proc::CliOutcome;
use crate::error::AppError;
use crate::mcp::server::AppState;

/// Run a single CLI invocation and unwrap its successful `data` payload.
///
/// Read tools never produce a confirmation; treat one as a protocol error so it
/// is surfaced clearly rather than silently dropped.
pub(crate) async fn run_cli(
    state: &AppState,
    args: &[String],
    secs: u64,
) -> Result<Value, AppError> {
    match state
        .runner
        .run_json(args, Duration::from_secs(secs))
        .await?
    {
        CliOutcome::Success(data) => Ok(data),
        CliOutcome::ConfirmationRequired { .. } => Err(AppError::ProtocolError(
            "read-only tool received an unexpected confirmation request".to_string(),
        )),
    }
}
