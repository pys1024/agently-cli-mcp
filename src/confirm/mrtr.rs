//! MRTR / legacy-elicitation confirmation flow.
//!
//! Write tools run the CLI twice. The first call yields a one-time `ctk_`
//! token bound to the exact request; the human confirms, then the cached
//! canonical arguments are replayed with that token. On protocol 2026-07-28
//! the confirmation is a multi round-trip [`InputRequiredResult`]; on older
//! protocols the handler calls `peer.elicit` directly and falls back to token
//! mode when the client lacks the elicitation capability.

use std::path::PathBuf;
use std::time::Duration;

use rmcp::model::{
    ElicitRequest, ElicitRequestParams, ElicitationSchema, InputRequest, InputRequests,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli_proc::CliOutcome;
use crate::confirm::store::Pending;
use crate::confirm::summary::SummaryAction;
use crate::error::AppError;
use crate::mcp::server::AppState;

/// The human's answer to a confirmation prompt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmResponse {
    /// `true` to proceed with the write, `false`/decline to abort it.
    pub confirm: bool,
}

rmcp::elicit_safe!(ConfirmResponse);

/// Build the MRTR elicitation request that asks a human to confirm `summary`.
pub fn confirm_request(summary: &Value) -> InputRequest {
    let schema = ElicitationSchema::from_type::<ConfirmResponse>()
        .expect("ConfirmResponse generates a valid elicitation schema");
    InputRequest::Elicitation(ElicitRequest::new(
        ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: summary.to_string(),
            requested_schema: schema,
        },
    ))
}

/// Build an [`InputRequests`] map carrying the single `confirm` elicitation.
pub fn confirm_requests(summary: &Value) -> InputRequests {
    let mut requests = InputRequests::new();
    requests.insert("confirm".to_string(), confirm_request(summary));
    requests
}

/// Everything needed to build a [`Pending`] record for a first CLI call that
/// yielded `ctk`.
pub struct PendingSpec {
    pub action: SummaryAction,
    pub canonical_args: Vec<String>,
    pub args_hash: String,
    pub ctk: String,
    pub body_temp: Option<PathBuf>,
    pub source_marked_read: bool,
}

/// Build a [`Pending`] record from [`PendingSpec`].
///
/// `canonical_args` is the exact argv to replay; `args_hash` is a fingerprint
/// of the semantic (human-visible) request, used to report `args_changed` when
/// a follow-up call mutates an input. The caller stores the record under the
/// MRTR `pending_id` or the token-mode `ctk` key.
pub fn begin(state: &AppState, tool: &str, spec: PendingSpec) -> Pending {
    Pending {
        tool: tool.to_string(),
        action: spec.action,
        args_hash: spec.args_hash,
        canonical_args: spec.canonical_args,
        ctk: spec.ctk,
        session_id: state.session_id.clone(),
        // Recomputed by the store from its own TTL on insert.
        expires_at: std::time::Instant::now(),
        body_temp: spec.body_temp,
        source_marked_read: spec.source_marked_read,
    }
}

/// Replay the cached canonical arguments with the one-time token.
///
/// `confirm` must be `true`; a `false` value is a programming error guarded
/// here (callers detect a decline before invoking this).
pub async fn finish(state: &AppState, pending: Pending, confirm: bool) -> Result<Value, AppError> {
    if !confirm {
        return Err(AppError::Rejected("confirmation was declined".to_string()));
    }

    let mut argv = pending.canonical_args;
    argv.push("--confirmation-token".to_string());
    argv.push(pending.ctk);

    match state
        .runner
        .run_json(&argv, Duration::from_secs(state.cfg.cli.send_timeout_secs))
        .await?
    {
        CliOutcome::Success(data) => Ok(data),
        CliOutcome::ConfirmationRequired { .. } => Err(AppError::ProtocolError(
            "confirmed write unexpectedly requested another confirmation".to_string(),
        )),
    }
}
