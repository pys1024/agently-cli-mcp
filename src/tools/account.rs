//! Account and authentication tools: `+me` plus `auth status` / `auth refresh`.

use std::time::Instant;

use serde_json::{Value, json};

use crate::error::AppError;
use crate::mcp::server::AppState;

use super::run_cli;

/// Combine `+me` and `auth status` into one account snapshot.
pub async fn get_account(state: &AppState) -> Result<(Value, Value), AppError> {
    let start = Instant::now();

    // The first account/list call seeds attachment limits from `+me`.
    state.seed_limits().await?;

    let me = run_cli(state, &["+me".to_string()], state.cfg.cli.timeout_secs).await?;
    let auth = run_cli(
        state,
        &["auth".to_string(), "status".to_string()],
        state.cfg.cli.timeout_secs,
    )
    .await?;

    let data = json!({ "account": me, "auth": auth });
    let meta = json!({
        "untrusted_content": false,
        "duration_ms": start.elapsed().as_millis(),
    });
    Ok((data, meta))
}

/// Refresh the stored OAuth credentials.
pub async fn refresh_auth(state: &AppState) -> Result<(Value, Value), AppError> {
    let start = Instant::now();
    let data = run_cli(
        state,
        &["auth".to_string(), "refresh".to_string()],
        state.cfg.cli.timeout_secs,
    )
    .await?;
    let meta = json!({
        "untrusted_content": false,
        "duration_ms": start.elapsed().as_millis(),
    });
    Ok((data, meta))
}
