//! Read tools: `message +list`, `message +read`, and `message +search`.

use std::time::Instant;

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

pub async fn list_messages(state: &AppState, args: ListArgs) -> Result<(Value, Value), AppError> {
    let start = Instant::now();
    state.seed_limits().await?;

    let mut argv = vec!["message".to_string(), "+list".to_string()];
    push_opt(&mut argv, "--dir", args.dir.as_ref());
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
        search_in: args.search_in.clone(),
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
