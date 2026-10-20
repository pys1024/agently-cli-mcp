//! Tool business logic: map MCP tool inputs to `agently-cli` argv and wrap the
//! raw CLI payloads in result envelopes.

pub mod account;
pub mod attachments;
pub mod read;
pub mod write;

use std::time::Duration;

use serde_json::Value;

use crate::cli_proc::CliOutcome;
use crate::config::ToolsConfig;
use crate::error::AppError;
use crate::mcp::server::AppState;

/// Every tool name the server can expose, in canonical (registration) order.
///
/// `mail_watch_messages` is listed even though it is wired into the router in a
/// later task: `enabled` is the single source of truth for *which* tools are
/// allowed, and filtering is intersected against the actually-registered router
/// routes at request time.
pub const ALL_TOOLS: &[&str] = &[
    "mail_get_account",
    "mail_refresh_auth",
    "mail_list_messages",
    "mail_get_message",
    "mail_search_messages",
    "mail_download_attachment",
    "mail_send_message",
    "mail_reply_message",
    "mail_forward_message",
    "mail_move_to_trash",
    "mail_delete_messages",
    "mail_watch_messages",
];

/// Tools that `--read-only` disables (send/reply/forward/trash/delete and the
/// credential-writing refresh), per spec §13.3.
pub const READ_ONLY_DISABLED: &[&str] = &[
    "mail_refresh_auth",
    "mail_send_message",
    "mail_reply_message",
    "mail_forward_message",
    "mail_move_to_trash",
    "mail_delete_messages",
];

/// Return the enabled tool names after applying `read_only`, `allow`, and
/// `disabled` restrictions.
///
/// - An empty `allow` means "all tools" (minus `disabled`).
/// - A non-empty `allow` whitelists exactly those names (minus `disabled`,
///   which validation guarantees is empty in that case).
/// - `read_only` additionally removes the write/credential tools.
pub fn enabled(cfg: &ToolsConfig) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ALL_TOOLS
        .iter()
        .copied()
        .filter(|tool| cfg.allow.is_empty() || cfg.allow.iter().any(|a| a.as_str() == *tool))
        .filter(|tool| !cfg.disabled.iter().any(|d| d.as_str() == *tool))
        .collect();
    if cfg.read_only {
        names.retain(|tool| !READ_ONLY_DISABLED.contains(tool));
    }
    names
}

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
