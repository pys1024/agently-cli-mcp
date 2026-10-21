//! Read tools: `message +list`, `message +read`, and `message +search`.

use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::AppError;
use crate::mcp::result::SECURITY_NOTICE;
use crate::mcp::server::AppState;
use crate::search::{SearchCtx, SearchQuery};

use super::run_cli;

/// Arguments for `mail_list_messages`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListArgs {
    pub dir: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub has_attachments: Option<bool>,
    pub is_unread: Option<bool>,
}

/// Arguments for `mail_get_message`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetArgs {
    pub id: String,
    pub include_body: Option<bool>,
    pub max_body_chars: Option<u32>,
}

/// Arguments for `mail_search_messages`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchArgs {
    pub q: Option<String>,
    pub search_in: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub dir: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub has_attachments: Option<bool>,
    pub is_unread: Option<bool>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// Output format for `mail_watch_messages` events.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WatchFormat {
    Full,
    Event,
}

impl WatchFormat {
    fn as_str(self) -> &'static str {
        match self {
            WatchFormat::Full => "full",
            WatchFormat::Event => "event",
        }
    }
}

/// Arguments for `mail_watch_messages`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WatchArgs {
    pub timeout_seconds: Option<u64>,
    pub msg_format: Option<WatchFormat>,
}

pub async fn list_messages(state: &AppState, args: ListArgs) -> Result<(Value, Value), AppError> {
    let start = Instant::now();
    state.seed_limits().await?;

    let dir = normalize_dir(args.dir.as_deref())?;
    let mut argv = vec!["message".to_string(), "+list".to_string()];
    push_opt(&mut argv, "--dir", dir.as_ref());
    if let Some(limit) = args.limit {
        push_pair(&mut argv, "--limit", &limit.to_string());
    }
    push_opt(&mut argv, "--cursor", args.cursor.as_ref());
    push_opt(&mut argv, "--after", args.after.as_ref());
    push_opt(&mut argv, "--before", args.before.as_ref());
    push_bool(&mut argv, "--has-attachments", args.has_attachments);
    push_bool(&mut argv, "--is-unread", args.is_unread);

    let data = run_cli(state, &argv, state.cfg.cli.read_timeout_secs).await?;
    Ok((data, read_meta(start)))
}

pub async fn get_message(state: &AppState, args: GetArgs) -> Result<(Value, Value), AppError> {
    let start = Instant::now();

    if !args.id.starts_with("msg_") {
        return Err(AppError::InvalidArgument(format!(
            "message id `{}` must start with `msg_`",
            args.id
        )));
    }

    let mut argv = vec!["message".to_string(), "+read".to_string()];
    push_pair(&mut argv, "--id", &args.id);

    let mut data = run_cli(state, &argv, state.cfg.cli.read_timeout_secs).await?;

    // Truncate the body to `max_body_chars` (0 = unlimited) and record whether
    // it was truncated.
    let mut truncated = false;
    if let Some(body) = data.get("body").and_then(Value::as_str).map(str::to_owned) {
        let max_chars = args
            .max_body_chars
            .map(|v| v as usize)
            .unwrap_or(state.cfg.limits.max_body_chars);
        if max_chars > 0 && body.chars().count() > max_chars {
            data["body"] = json!(body.chars().take(max_chars).collect::<String>());
            truncated = true;
        }
    }

    if args.include_body == Some(false)
        && let Some(obj) = data.as_object_mut()
    {
        obj.remove("body");
    }

    let meta = json!({
        "untrusted_content": true,
        "security_notice": SECURITY_NOTICE,
        "side_effects": ["marked_read"],
        "body_truncated": truncated,
        "duration_ms": start.elapsed().as_millis(),
    });
    Ok((data, meta))
}

pub async fn search_messages(
    state: &AppState,
    args: SearchArgs,
) -> Result<(Value, Value), AppError> {
    let start = Instant::now();
    state.seed_limits().await?;

    let provided = SearchQuery {
        q: args.q.clone(),
        search_in: normalize_search_in(args.search_in.as_deref())?,
        from: args.from.clone(),
        to: args.to.clone(),
        dir: args.dir.clone(),
        after: args.after.clone(),
        before: args.before.clone(),
        has_attachments: args.has_attachments,
        is_unread: args.is_unread,
        limit: args.limit.unwrap_or(0),
    };

    // A cursor-only follow-up restores the stored filters; a fresh query uses
    // the supplied filters verbatim.
    let query = if let Some(cursor) = args.cursor.as_deref() {
        state.search.resolve(cursor, &provided, &state.session_id)?
    } else {
        provided.clone()
    };

    let mut argv = vec!["message".to_string(), "+search".to_string()];
    push_opt(&mut argv, "--q", query.q.as_ref());
    push_opt(&mut argv, "--search-in", query.search_in.as_ref());
    push_opt(&mut argv, "--from", query.from.as_ref());
    push_opt(&mut argv, "--to", query.to.as_ref());
    push_opt(&mut argv, "--dir", query.dir.as_ref());
    push_opt(&mut argv, "--after", query.after.as_ref());
    push_opt(&mut argv, "--before", query.before.as_ref());
    push_bool(&mut argv, "--has-attachments", query.has_attachments);
    push_bool(&mut argv, "--is-unread", query.is_unread);
    if query.limit > 0 {
        push_pair(&mut argv, "--limit", &query.limit.to_string());
    }
    if let Some(cursor) = args.cursor.as_deref() {
        push_pair(&mut argv, "--cursor", cursor);
    }

    let data = run_cli(state, &argv, state.cfg.cli.read_timeout_secs).await?;

    // Remember the query that produced the next cursor so a cursor-only
    // follow-up can restore it.
    if let Some(next) = data.get("next_cursor").and_then(Value::as_str) {
        let ctx = SearchCtx {
            q: query.q.clone(),
            search_in: query.search_in.clone(),
            from: query.from.clone(),
            to: query.to.clone(),
            dir: query.dir.clone(),
            after: query.after.clone(),
            before: query.before.clone(),
            has_attachments: query.has_attachments,
            is_unread: query.is_unread,
            limit: query.limit,
            session_id: state.session_id.clone(),
        };
        state.search.remember(next, ctx, &state.session_id);
    }

    Ok((data, read_meta(start)))
}

pub async fn watch_messages(state: &AppState, args: WatchArgs) -> Result<(Value, Value), AppError> {
    let start = Instant::now();

    // One watch at a time process-wide: a second concurrent call fails fast
    // with WATCH_BUSY instead of queueing behind a long poll.
    let _guard = state
        .watch_lock
        .try_lock()
        .map_err(|_| AppError::WatchBusy("another mail watch is already running".to_string()))?;

    // Default 30s, clamped to [5, watch_max_secs] (spec §5.1: timeout_seconds
    // is 5..=120; watch_max_secs is itself validated to 5..=120 by config).
    let secs = args
        .timeout_seconds
        .unwrap_or(30)
        .clamp(5, state.cfg.cli.watch_max_secs);

    let mut argv = vec!["message".to_string(), "+watch".to_string()];
    if let Some(fmt) = args.msg_format {
        argv.push("--msg-format".to_string());
        argv.push(fmt.as_str().to_string());
    }

    let output = state
        .runner
        .run_watch(&argv, Duration::from_secs(secs))
        .await?;

    let count = output.events.len();
    let elapsed_ms = start.elapsed().as_millis();

    let data = json!({
        "events": output.events,
        "count": count,
        "timed_out": output.timed_out,
        "elapsed_ms": elapsed_ms,
    });
    let meta = json!({
        "untrusted_content": count > 0,
        "security_notice": SECURITY_NOTICE,
        "duration_ms": elapsed_ms,
    });
    Ok((data, meta))
}

/// Normalize a folder name to the CLI's lowercase enum. Clients often send
/// `INBOX` / `Inbox`; the CLI expects `inbox`.
fn normalize_dir(dir: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(dir) = dir else { return Ok(None) };
    let lower = dir.to_ascii_lowercase();
    match lower.as_str() {
        "inbox" | "sent" | "trash" | "spam" => Ok(Some(lower)),
        other => Err(AppError::InvalidArgument(format!(
            "unknown dir `{other}` (expected inbox, sent, trash, or spam)"
        ))),
    }
}

/// Normalize `search_in` to the CLI's canonical enum. Accepts both
/// `SEARCH_IN_SUBJECT` and `subject` (case-insensitive).
fn normalize_search_in(value: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(value) = value else { return Ok(None) };
    let upper = value.to_ascii_uppercase();
    let canonical = match upper.as_str() {
        "SEARCH_IN_ALL" | "ALL" => "SEARCH_IN_ALL",
        "SEARCH_IN_SUBJECT" | "SUBJECT" => "SEARCH_IN_SUBJECT",
        "SEARCH_IN_CONTENT" | "CONTENT" => "SEARCH_IN_CONTENT",
        other => {
            return Err(AppError::InvalidArgument(format!(
                "unknown search_in `{other}` (expected SEARCH_IN_ALL, SEARCH_IN_SUBJECT, or SEARCH_IN_CONTENT)"
            )));
        }
    };
    Ok(Some(canonical.to_string()))
}

fn read_meta(start: Instant) -> Value {
    json!({
        "untrusted_content": true,
        "security_notice": SECURITY_NOTICE,
        "duration_ms": start.elapsed().as_millis(),
    })
}

fn push_pair(argv: &mut Vec<String>, flag: &str, value: &str) {
    argv.push(flag.to_string());
    argv.push(value.to_string());
}

fn push_opt(argv: &mut Vec<String>, flag: &str, value: Option<&String>) {
    if let Some(v) = value {
        argv.push(flag.to_string());
        argv.push(v.clone());
    }
}

fn push_bool(argv: &mut Vec<String>, flag: &str, value: Option<bool>) {
    if value == Some(true) {
        argv.push(flag.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_is_case_insensitive() {
        assert_eq!(
            normalize_dir(Some("INBOX")).unwrap(),
            Some("inbox".to_string())
        );
        assert_eq!(
            normalize_dir(Some("Trash")).unwrap(),
            Some("trash".to_string())
        );
        assert_eq!(normalize_dir(None).unwrap(), None);
    }

    #[test]
    fn unknown_dir_is_rejected() {
        let err = normalize_dir(Some("archive")).unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn search_in_accepts_canonical_and_short_forms() {
        assert_eq!(
            normalize_search_in(Some("subject")).unwrap(),
            Some("SEARCH_IN_SUBJECT".to_string())
        );
        assert_eq!(
            normalize_search_in(Some("SEARCH_IN_CONTENT")).unwrap(),
            Some("SEARCH_IN_CONTENT".to_string())
        );
        assert_eq!(
            normalize_search_in(Some("all")).unwrap(),
            Some("SEARCH_IN_ALL".to_string())
        );
        assert_eq!(normalize_search_in(None).unwrap(), None);
    }

    #[test]
    fn unknown_search_in_is_rejected() {
        let err = normalize_search_in(Some("headers")).unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }
}
