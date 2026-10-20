//! Write tools: `message +send/+reply/+forward/+trash/+delete` with the
//! MRTR / legacy-elicitation / token confirmation flow.
//!
//! Every write body is written to a temp file under the sandbox and passed via
//! `--body-file` (never `--body`). The first CLI call yields a one-time `ctk_`
//! token; after a human confirms, the cached canonical arguments are replayed
//! with that token. The temp file is retained until confirm/decline/expiry and
//! deleted immediately in the `never` / no-confirm path.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmcp::ErrorData;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, InputRequiredResult,
    InputResponses as ModelInputResponses,
};
use rmcp::service::{ElicitationError, RequestContext, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli_proc::CliOutcome;
use crate::cli_proc::bodyfile::{BodyFile, BodyFormat};
use crate::config::ConfirmMode;
use crate::confirm::mrtr;
use crate::confirm::summary::{self, AttachmentMeta, SourceMessage, SummaryAction, SummaryInput};
use crate::error::AppError;
use crate::mcp::result;
use crate::mcp::server::AppState;

// ---------------------------------------------------------------------------
// Argument structs.
// ---------------------------------------------------------------------------

/// Arguments for `mail_send_message`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SendArgs {
    pub to: Vec<String>,
    pub subject: String,
    pub body: Option<String>,
    pub body_file: Option<String>,
    pub body_format: Option<String>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub attachments: Option<Vec<String>>,
    pub confirmation_token: Option<String>,
}

/// Arguments for `mail_reply_message`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReplyArgs {
    pub id: String,
    pub body: Option<String>,
    pub body_file: Option<String>,
    pub body_format: Option<String>,
    pub reply_all: Option<bool>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub attachments: Option<Vec<String>>,
    pub confirmation_token: Option<String>,
}

/// Arguments for `mail_forward_message`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ForwardArgs {
    pub id: String,
    pub to: Vec<String>,
    pub body: String,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub include_attachments: Option<bool>,
    pub attachments: Option<Vec<String>>,
    pub confirmation_token: Option<String>,
}

/// Arguments for `mail_move_to_trash`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TrashArgs {
    pub id: String,
    pub confirmation_token: Option<String>,
}

/// Arguments for `mail_delete_messages`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteArgs {
    pub id: Option<String>,
    pub all: Option<bool>,
    pub confirmation_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Prepared request.
// ---------------------------------------------------------------------------

/// A validated write request, ready for the first CLI call and the summary.
pub(crate) struct WriteRequest {
    action: SummaryAction,
    argv: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: Option<String>,
    body_preview: String,
    attachments: Vec<AttachmentMeta>,
    source: Option<SourceMessage>,
    body_temp: Option<BodyFile>,
    source_marked_read: bool,
    no_confirm: bool,
}

/// A deterministic fingerprint of the human-visible arguments, used to detect
/// (and report) a mutated follow-up call while still replaying cached args.
pub(crate) fn args_fingerprint(args: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};

    let mut value = serde_json::to_value(args).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.remove("confirmation_token");
    }
    let canonical = serde_json::to_string(&value).unwrap_or_default();
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

// ---------------------------------------------------------------------------
// Preparation: validate, build argv, write the body file.
// ---------------------------------------------------------------------------

pub(crate) async fn prepare_send(
    state: &AppState,
    args: SendArgs,
) -> Result<WriteRequest, AppError> {
    if args.to.is_empty() {
        return Err(AppError::InvalidArgument(
            "at least one `to` recipient is required".to_string(),
        ));
    }
    let subject = args.subject.trim();
    if subject.is_empty() {
        return Err(AppError::InvalidArgument(
            "`subject` is required and must be non-empty".to_string(),
        ));
    }

    let format = parse_body_format(args.body_format.as_deref())?;
    let body = resolve_body(state, &args.body, &args.body_file)?;
    let body_file = BodyFile::create(state.sandbox.root(), &body, format)?;

    let mut argv = vec!["message".to_string(), "+send".to_string()];
    for to in &args.to {
        push_pair(&mut argv, "--to", to);
    }
    push_pair(&mut argv, "--subject", subject);
    for cc in args.cc.iter().flatten() {
        push_pair(&mut argv, "--cc", cc);
    }
    for bcc in args.bcc.iter().flatten() {
        push_pair(&mut argv, "--bcc", bcc);
    }

    let attachments =
        resolve_attachments(state, args.attachments.as_deref().unwrap_or(&[])).await?;
    for a in &attachments {
        push_pair(&mut argv, "--attachment", &a.to_string_lossy());
    }
    push_pair(&mut argv, "--body-file", body_file.relative_path());
    push_pair(&mut argv, "--body-format", format_flag(format));

    let no_confirm = state.cfg.confirm.mode == ConfirmMode::Never;
    if no_confirm {
        argv.push("--confirmed".to_string());
    }

    Ok(WriteRequest {
        action: SummaryAction::Send,
        argv,
        to: args.to.clone(),
        cc: args.cc.clone().unwrap_or_default(),
        bcc: args.bcc.clone().unwrap_or_default(),
        subject: Some(subject.to_string()),
        body_preview: String::from_utf8_lossy(&body).into_owned(),
        attachments: attachment_meta(state, &attachments),
        source: None,
        body_temp: Some(body_file),
        source_marked_read: false,
        no_confirm,
    })
}

pub(crate) async fn prepare_reply(
    state: &AppState,
    args: ReplyArgs,
) -> Result<WriteRequest, AppError> {
    validate_id(&args.id)?;

    let format = parse_body_format(args.body_format.as_deref())?;
    let body = resolve_body(state, &args.body, &args.body_file)?;
    let body_file = BodyFile::create(state.sandbox.root(), &body, format)?;

    let mut argv = vec!["message".to_string(), "+reply".to_string()];
    push_pair(&mut argv, "--id", &args.id);
    if args.reply_all == Some(true) {
        argv.push("--reply-all".to_string());
    }
    for cc in args.cc.iter().flatten() {
        push_pair(&mut argv, "--cc", cc);
    }
    for bcc in args.bcc.iter().flatten() {
        push_pair(&mut argv, "--bcc", bcc);
    }

    let attachments =
        resolve_attachments(state, args.attachments.as_deref().unwrap_or(&[])).await?;
    for a in &attachments {
        push_pair(&mut argv, "--attachment", &a.to_string_lossy());
    }
    push_pair(&mut argv, "--body-file", body_file.relative_path());

    // Pre-read the original to complete the summary (this marks it read).
    let source = read_source(state, &args.id).await;
    let source_marked_read = source.is_some();

    let no_confirm = state.cfg.confirm.mode == ConfirmMode::Never;
    if no_confirm {
        argv.push("--confirmed".to_string());
    }

    Ok(WriteRequest {
        action: SummaryAction::Reply,
        argv,
        to: Vec::new(),
        cc: args.cc.clone().unwrap_or_default(),
        bcc: args.bcc.clone().unwrap_or_default(),
        subject: None,
        body_preview: String::from_utf8_lossy(&body).into_owned(),
        attachments: attachment_meta(state, &attachments),
        source,
        body_temp: Some(body_file),
        source_marked_read,
        no_confirm,
    })
}

pub(crate) async fn prepare_forward(
    state: &AppState,
    args: ForwardArgs,
) -> Result<WriteRequest, AppError> {
    validate_id(&args.id)?;
    if args.to.is_empty() {
        return Err(AppError::InvalidArgument(
            "at least one `to` recipient is required".to_string(),
        ));
    }
    if args.body.trim().is_empty() {
        return Err(AppError::InvalidArgument(
            "`body` is required and must be non-empty".to_string(),
        ));
    }

    let body = args.body.as_bytes().to_vec();
    let body_file = BodyFile::create(state.sandbox.root(), &body, BodyFormat::Plain)?;

    let mut argv = vec!["message".to_string(), "+forward".to_string()];
    push_pair(&mut argv, "--id", &args.id);
    for to in &args.to {
        push_pair(&mut argv, "--to", to);
    }
    for cc in args.cc.iter().flatten() {
        push_pair(&mut argv, "--cc", cc);
    }
    for bcc in args.bcc.iter().flatten() {
        push_pair(&mut argv, "--bcc", bcc);
    }
    if args.include_attachments == Some(true) {
        argv.push("--include-attachments".to_string());
    }

    let attachments =
        resolve_attachments(state, args.attachments.as_deref().unwrap_or(&[])).await?;
    for a in &attachments {
        push_pair(&mut argv, "--attachment", &a.to_string_lossy());
    }
    push_pair(&mut argv, "--body-file", body_file.relative_path());

    let source = read_source(state, &args.id).await;
    let source_marked_read = source.is_some();

    let no_confirm = state.cfg.confirm.mode == ConfirmMode::Never;
    if no_confirm {
        argv.push("--confirmed".to_string());
    }

    Ok(WriteRequest {
        action: SummaryAction::Forward,
        argv,
        to: args.to.clone(),
        cc: args.cc.clone().unwrap_or_default(),
        bcc: args.bcc.clone().unwrap_or_default(),
        subject: None,
        body_preview: String::from_utf8_lossy(&body).into_owned(),
        attachments: attachment_meta(state, &attachments),
        source,
        body_temp: Some(body_file),
        source_marked_read,
        no_confirm,
    })
}

pub(crate) async fn prepare_trash(
    _state: &AppState,
    args: TrashArgs,
) -> Result<WriteRequest, AppError> {
    validate_id(&args.id)?;

    let mut argv = vec!["message".to_string(), "+trash".to_string()];
    push_pair(&mut argv, "--id", &args.id);

    // trash has no `--confirmed`; `never` mode falls back to token.
    Ok(WriteRequest {
        action: SummaryAction::Trash,
        argv,
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: None,
        body_preview: String::new(),
        attachments: Vec::new(),
        source: None,
        body_temp: None,
        source_marked_read: false,
        no_confirm: false,
    })
}

pub(crate) async fn prepare_delete(
    _state: &AppState,
    args: DeleteArgs,
) -> Result<WriteRequest, AppError> {
    let has_id = args.id.is_some();
    let has_all = args.all == Some(true);
    if has_id == has_all {
        return Err(AppError::InvalidArgument(
            "exactly one of `id` or `all` is required".to_string(),
        ));
    }

    let mut argv = vec!["message".to_string(), "+delete".to_string()];
    let action = if let Some(id) = &args.id {
        validate_id(id)?;
        push_pair(&mut argv, "--id", id);
        SummaryAction::Delete
    } else {
        argv.push("--all".to_string());
        SummaryAction::DeleteAll
    };

    Ok(WriteRequest {
        action,
        argv,
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: None,
        body_preview: String::new(),
        attachments: Vec::new(),
        source: None,
        body_temp: None,
        source_marked_read: false,
        no_confirm: false,
    })
}

// ---------------------------------------------------------------------------
// Orchestration: dispatch first call / token replay / MRTR replay.
// ---------------------------------------------------------------------------

/// The per-call inputs that select the confirmation branch.
pub(crate) struct WriteCall {
    pub(crate) request_state: Option<String>,
    pub(crate) input_responses: Option<ModelInputResponses>,
    pub(crate) confirmation_token: Option<String>,
    pub(crate) fingerprint: String,
    pub(crate) prepared: Option<WriteRequest>,
}

/// The shared write-tool entry point. Exactly one of `confirmation_token`,
/// `request_state`, or `prepared` drives the branch.
pub(crate) async fn execute(
    tool: &'static str,
    state: &AppState,
    context: RequestContext<RoleServer>,
    call: WriteCall,
) -> Result<CallToolResponse, ErrorData> {
    if let Some(ctk) = call.confirmation_token {
        return replay_token(tool, state, ctk, call.fingerprint).await;
    }
    if let Some(pending_id) = call.request_state {
        return replay_mrtr(
            tool,
            state,
            pending_id,
            call.input_responses,
            call.fingerprint,
        )
        .await;
    }
    let request = match call.prepared {
        Some(r) => r,
        None => {
            return Ok(result::err(
                tool,
                &AppError::ProtocolError(
                    "write tool called without arguments or confirmation state".to_string(),
                ),
            )
            .into());
        }
    };
    first_call(tool, state, &context, request, call.fingerprint).await
}

/// A follow-up call carrying the one-time token (token / never-fallback mode).
async fn replay_token(
    tool: &'static str,
    state: &AppState,
    ctk: String,
    fingerprint: String,
) -> Result<CallToolResponse, ErrorData> {
    let pending = match state.confirm.take_checked(&ctk, &state.session_id) {
        Ok(p) => p,
        Err(take_err) => {
            remove_body(take_err.body_temp);
            return Ok(result::err(tool, &take_err.error).into());
        }
    };
    let args_changed = pending.args_hash != fingerprint;
    let source_marked_read = pending.source_marked_read;
    let body_temp = pending.body_temp.clone();

    let start = Instant::now();
    let data = match mrtr::finish(state, pending, true).await {
        Ok(d) => d,
        Err(e) => {
            remove_body(body_temp);
            return Ok(result::err(tool, &e).into());
        }
    };
    remove_body(body_temp);
    Ok(result::ok(
        tool,
        data,
        success_meta(start, args_changed, source_marked_read),
    )
    .into())
}

/// A retry with echoed MRTR state and the client's elicitation response.
async fn replay_mrtr(
    tool: &'static str,
    state: &AppState,
    pending_id: String,
    input_responses: Option<ModelInputResponses>,
    fingerprint: String,
) -> Result<CallToolResponse, ErrorData> {
    let pending = match state.confirm.take_checked(&pending_id, &state.session_id) {
        Ok(p) => p,
        Err(take_err) => {
            remove_body(take_err.body_temp);
            return Ok(result::err(tool, &take_err.error).into());
        }
    };

    let action = pending.action;
    let source_marked_read = pending.source_marked_read;
    let start = Instant::now();

    if !read_confirm(&input_responses) {
        remove_body(pending.body_temp.clone());
        return Ok(declined(
            tool,
            action.as_str(),
            None,
            declined_meta(start, source_marked_read),
        )
        .into());
    }

    let args_changed = pending.args_hash != fingerprint;
    let body_temp = pending.body_temp.clone();

    let data = match mrtr::finish(state, pending, true).await {
        Ok(d) => d,
        Err(e) => {
            remove_body(body_temp);
            return Ok(result::err(tool, &e).into());
        }
    };
    remove_body(body_temp);
    Ok(result::ok(
        tool,
        data,
        success_meta(start, args_changed, source_marked_read),
    )
    .into())
}

/// The first call: run the CLI unconfirmed, then branch on mode and protocol.
async fn first_call(
    tool: &'static str,
    state: &AppState,
    context: &RequestContext<RoleServer>,
    request: WriteRequest,
    fingerprint: String,
) -> Result<CallToolResponse, ErrorData> {
    let start = Instant::now();

    // `never` + send/reply/forward already carries `--confirmed`: run once.
    if request.no_confirm {
        return run_once(tool, state, request, start).await;
    }

    let outcome = state
        .runner
        .run_json(
            &request.argv,
            Duration::from_secs(state.cfg.cli.send_timeout_secs),
        )
        .await;
    let (ctk, cli_summary) = match outcome {
        Ok(CliOutcome::ConfirmationRequired { token, summary }) => (token, summary),
        Ok(CliOutcome::Success(data)) => {
            remove_body(body_temp_path(state, &request));
            return Ok(result::ok(
                tool,
                data,
                success_meta(start, false, request.source_marked_read),
            )
            .into());
        }
        Err(e) => {
            remove_body(body_temp_path(state, &request));
            return Ok(result::err(tool, &e).into());
        }
    };

    let summary = build_summary(&request, &cli_summary);
    let mode = state.cfg.confirm.mode;
    let ttl = Duration::from_secs(state.cfg.confirm.ttl_secs);

    // token mode (and never-mode trash/delete fallback) returns ctk directly.
    if mode == ConfirmMode::Token || mode == ConfirmMode::Never {
        let pending = mrtr::begin(
            state,
            tool,
            pending_spec(
                &request,
                fingerprint,
                ctk.clone(),
                body_temp_path(state, &request),
            ),
        );
        state.confirm.insert(ctk.clone(), pending);
        return Ok(confirmation_required(tool, &summary, &ctk, state.cfg.confirm.ttl_secs).into());
    }

    // elicit mode: branch on the negotiated protocol version.
    let version = context.protocol_version();
    let is_mrtr = version.as_ref().is_some_and(|v| v.as_str() >= "2026-07-28");

    if is_mrtr {
        let pending = mrtr::begin(
            state,
            tool,
            pending_spec(&request, fingerprint, ctk, body_temp_path(state, &request)),
        );
        let pending_id = format!("pending_{}", uuid::Uuid::new_v4());
        state.confirm.insert(pending_id.clone(), pending);
        let result =
            InputRequiredResult::new(Some(mrtr::confirm_requests(&summary)), Some(pending_id));
        return Ok(CallToolResponse::InputRequired(result));
    }

    // Legacy elicitation: ask the human synchronously.
    match context
        .peer
        .elicit_with_timeout::<mrtr::ConfirmResponse>(summary.to_string(), Some(ttl))
        .await
    {
        Ok(Some(resp)) if resp.confirm => {
            let pending = mrtr::begin(
                state,
                tool,
                pending_spec(&request, fingerprint, ctk, body_temp_path(state, &request)),
            );
            let data = match mrtr::finish(state, pending, true).await {
                Ok(d) => d,
                Err(e) => {
                    remove_body(body_temp_path(state, &request));
                    return Ok(result::err(tool, &e).into());
                }
            };
            remove_body(body_temp_path(state, &request));
            Ok(result::ok(
                tool,
                data,
                success_meta(start, false, request.source_marked_read),
            )
            .into())
        }
        Ok(_) => {
            remove_body(body_temp_path(state, &request));
            Ok(declined(
                tool,
                request.action.as_str(),
                Some(&summary),
                declined_meta(start, request.source_marked_read),
            )
            .into())
        }
        Err(ElicitationError::UserDeclined) | Err(ElicitationError::UserCancelled) => {
            remove_body(body_temp_path(state, &request));
            Ok(declined(
                tool,
                request.action.as_str(),
                Some(&summary),
                declined_meta(start, request.source_marked_read),
            )
            .into())
        }
        Err(ElicitationError::CapabilityNotSupported) => {
            // The client cannot elicit: fall back to token mode.
            let pending = mrtr::begin(
                state,
                tool,
                pending_spec(
                    &request,
                    fingerprint,
                    ctk.clone(),
                    body_temp_path(state, &request),
                ),
            );
            state.confirm.insert(ctk.clone(), pending);
            Ok(confirmation_required(tool, &summary, &ctk, state.cfg.confirm.ttl_secs).into())
        }
        Err(_) => {
            remove_body(body_temp_path(state, &request));
            Ok(result::err(
                tool,
                &AppError::ConfirmationExpired(
                    "confirmation elicitation failed or timed out".to_string(),
                ),
            )
            .into())
        }
    }
}

/// Run a `never`-mode write once (argv already carries `--confirmed`).
async fn run_once(
    tool: &'static str,
    state: &AppState,
    request: WriteRequest,
    start: Instant,
) -> Result<CallToolResponse, ErrorData> {
    let body_path = body_temp_path(state, &request);
    let outcome = state
        .runner
        .run_json(
            &request.argv,
            Duration::from_secs(state.cfg.cli.send_timeout_secs),
        )
        .await;
    match outcome {
        Ok(CliOutcome::Success(data)) => {
            remove_body(body_path);
            Ok(result::ok(
                tool,
                data,
                success_meta(start, false, request.source_marked_read),
            )
            .into())
        }
        Ok(CliOutcome::ConfirmationRequired { .. }) => {
            remove_body(body_path);
            Ok(result::err(
                tool,
                &AppError::ProtocolError(
                    "`never` mode write unexpectedly requested confirmation".to_string(),
                ),
            )
            .into())
        }
        Err(e) => {
            remove_body(body_path);
            Ok(result::err(tool, &e).into())
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn read_confirm(input_responses: &Option<ModelInputResponses>) -> bool {
    input_responses
        .as_ref()
        .and_then(|r| r.get("confirm"))
        .and_then(|v| v.get("action"))
        .and_then(|a| a.as_str())
        .map(|a| a == "accept")
        .unwrap_or(false)
        && input_responses
            .as_ref()
            .and_then(|r| r.get("confirm"))
            .and_then(|v| v.get("content"))
            .and_then(|c| c.get("confirm"))
            .and_then(|c| c.as_bool())
            .unwrap_or(false)
}

fn pending_spec(
    request: &WriteRequest,
    fingerprint: String,
    ctk: String,
    body_temp: Option<PathBuf>,
) -> mrtr::PendingSpec {
    mrtr::PendingSpec {
        action: request.action,
        canonical_args: request.argv.clone(),
        args_hash: fingerprint,
        ctk,
        body_temp,
        source_marked_read: request.source_marked_read,
    }
}

fn build_summary(request: &WriteRequest, cli_summary: &Value) -> Value {
    let from = cli_summary
        .get("from")
        .and_then(Value::as_str)
        .map(str::to_owned);
    summary::build(SummaryInput {
        action: request.action,
        from,
        to: request.to.clone(),
        cc: request.cc.clone(),
        bcc: request.bcc.clone(),
        subject: request.subject.clone(),
        body_preview: request.body_preview.clone(),
        attachments: request.attachments.clone(),
        source: request.source.clone(),
        cli: cli_summary.clone(),
    })
}

fn success_meta(start: Instant, args_changed: bool, source_marked_read: bool) -> Value {
    let mut meta = json!({
        "duration_ms": start.elapsed().as_millis(),
        "args_changed": args_changed,
    });
    if source_marked_read {
        meta["side_effects"] = json!(["source_marked_read"]);
    }
    meta
}

fn declined_meta(start: Instant, source_marked_read: bool) -> Value {
    let mut meta = json!({ "duration_ms": start.elapsed().as_millis() });
    if source_marked_read {
        meta["side_effects"] = json!(["source_marked_read"]);
    }
    meta
}

fn confirmation_required(tool: &str, summary: &Value, ctk: &str, ttl_secs: u64) -> CallToolResult {
    let envelope = json!({
        "ok": true,
        "tool": tool,
        "status": "confirmation_required",
        "action": summary["action"],
        "summary": summary,
        "confirmation_token": ctk,
        "expires_in": ttl_secs,
        "next": "请向用户展示 summary 并等待明确许可，然后用相同参数加 confirmation_token 再次调用",
    });
    let mut result = CallToolResult::success(vec![ContentBlock::text(envelope.to_string())]);
    result.structured_content = Some(envelope);
    result
}

fn declined(tool: &str, action: &str, summary: Option<&Value>, meta: Value) -> CallToolResult {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_string(), json!(true));
    envelope.insert("tool".to_string(), json!(tool));
    envelope.insert("status".to_string(), json!("declined"));
    envelope.insert("action".to_string(), json!(action));
    if let Some(summary) = summary {
        envelope.insert("summary".to_string(), summary.clone());
    }
    envelope.insert("meta".to_string(), meta);
    let envelope = Value::Object(envelope);

    let mut result = CallToolResult::success(vec![ContentBlock::text(envelope.to_string())]);
    result.structured_content = Some(envelope);
    result
}

fn body_temp_path(state: &AppState, request: &WriteRequest) -> Option<PathBuf> {
    request
        .body_temp
        .as_ref()
        .map(|b| state.sandbox.root().join(b.relative_path()))
}

fn remove_body(path: Option<PathBuf>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

fn validate_id(id: &str) -> Result<(), AppError> {
    if !id.starts_with("msg_") {
        return Err(AppError::InvalidArgument(format!(
            "message id `{id}` must start with `msg_`"
        )));
    }
    Ok(())
}

fn parse_body_format(fmt: Option<&str>) -> Result<BodyFormat, AppError> {
    match fmt.unwrap_or("plain") {
        "plain" => Ok(BodyFormat::Plain),
        "html" => Ok(BodyFormat::Html),
        "markdown" => Ok(BodyFormat::Markdown),
        other => Err(AppError::InvalidArgument(format!(
            "unknown body_format `{other}` (expected plain, html, or markdown)"
        ))),
    }
}

fn format_flag(format: BodyFormat) -> &'static str {
    match format {
        BodyFormat::Plain => "plain",
        BodyFormat::Html => "html",
        BodyFormat::Markdown => "markdown",
    }
}

fn resolve_body(
    state: &AppState,
    body: &Option<String>,
    body_file: &Option<String>,
) -> Result<Vec<u8>, AppError> {
    match (body, body_file) {
        (Some(_), Some(_)) => Err(AppError::InvalidArgument(
            "`body` and `body_file` are mutually exclusive".to_string(),
        )),
        (Some(text), None) => Ok(text.clone().into_bytes()),
        (None, Some(path)) => {
            let rel = state.sandbox.resolve_input(Path::new(path))?;
            std::fs::read(state.sandbox.root().join(rel)).map_err(|e| {
                AppError::InvalidArgument(format!("cannot read body_file `{path}`: {e}"))
            })
        }
        (None, None) => Ok(Vec::new()),
    }
}

async fn resolve_attachments(state: &AppState, paths: &[String]) -> Result<Vec<PathBuf>, AppError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    state.seed_limits().await?;
    let limits = *state.limits.read().await;
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    state.sandbox.validate_attachments(&paths, &limits)
}

fn attachment_meta(state: &AppState, resolved: &[PathBuf]) -> Vec<AttachmentMeta> {
    resolved
        .iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(state.sandbox.root().join(p)).ok()?;
            let filename = p
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned());
            Some(AttachmentMeta {
                filename,
                size: meta.len(),
            })
        })
        .collect()
}

async fn read_source(state: &AppState, id: &str) -> Option<SourceMessage> {
    let argv = vec![
        "message".to_string(),
        "+read".to_string(),
        "--id".to_string(),
        id.to_string(),
    ];
    match state
        .runner
        .run_json(&argv, Duration::from_secs(state.cfg.cli.read_timeout_secs))
        .await
    {
        Ok(CliOutcome::Success(data)) => Some(SourceMessage {
            id: id.to_string(),
            subject: data
                .get("subject")
                .and_then(Value::as_str)
                .map(str::to_owned),
            from: data.get("from").and_then(Value::as_str).map(str::to_owned),
        }),
        _ => None,
    }
}

fn push_pair(argv: &mut Vec<String>, flag: &str, value: &str) {
    argv.push(flag.to_string());
    argv.push(value.to_string());
}
