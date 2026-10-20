//! Deterministic confirmation summary.
//!
//! The summary shown to a human before a write operation is built here from
//! the validated tool inputs, never from the CLI's lossy merged `to` list.
//! Recipients stay separate, attachments are described by filename and size
//! only (their contents are never read here), the body preview is capped, and
//! the CLI's own summary is preserved under `summary.cli` for diagnostics.

use serde_json::{Map, Value, json};

/// Maximum characters retained in `body_preview`.
pub const BODY_PREVIEW_CHARS: usize = 200;

/// The write operation a summary describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryAction {
    Send,
    Reply,
    Forward,
    Trash,
    Delete,
    DeleteAll,
}

impl SummaryAction {
    /// Stable string form used in the summary output.
    pub fn as_str(self) -> &'static str {
        match self {
            SummaryAction::Send => "send",
            SummaryAction::Reply => "reply",
            SummaryAction::Forward => "forward",
            SummaryAction::Trash => "trash",
            SummaryAction::Delete => "delete",
            SummaryAction::DeleteAll => "delete_all",
        }
    }
}

/// A local attachment, described without reading its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub filename: String,
    pub size: u64,
}

/// The message being replied to or forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMessage {
    pub id: String,
    pub subject: Option<String>,
    pub from: Option<String>,
}

/// Validated inputs for [`build`].
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryInput {
    pub action: SummaryAction,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub body_preview: String,
    pub attachments: Vec<AttachmentMeta>,
    pub source: Option<SourceMessage>,
    pub cli: Value,
}

/// Build a deterministic summary from validated tool inputs.
///
/// Recipients are emitted in input order and never merged; attachments carry
/// only `filename` and `size`; the body preview is truncated to
/// [`BODY_PREVIEW_CHARS`]; `source_message` appears only for reply/forward
/// calls that provided one; and the CLI's original summary is preserved under
/// `cli`.
pub fn build(input: SummaryInput) -> Value {
    let SummaryInput {
        action,
        from,
        to,
        cc,
        bcc,
        subject,
        body_preview,
        attachments,
        source,
        cli,
    } = input;

    let mut map = Map::new();

    map.insert("action".to_string(), json!(action.as_str()));

    if let Some(from) = from {
        map.insert("from".to_string(), json!(from));
    }
    if !to.is_empty() {
        map.insert("to".to_string(), json!(to));
    }
    if !cc.is_empty() {
        map.insert("cc".to_string(), json!(cc));
    }
    if !bcc.is_empty() {
        map.insert("bcc".to_string(), json!(bcc));
    }
    if let Some(subject) = subject {
        map.insert("subject".to_string(), json!(subject));
    }

    if !attachments.is_empty() {
        let attachments: Vec<Value> = attachments
            .into_iter()
            .map(|a| json!({ "filename": a.filename, "size": a.size }))
            .collect();
        map.insert("attachments".to_string(), json!(attachments));
    }

    let preview: String = body_preview.chars().take(BODY_PREVIEW_CHARS).collect();
    if !preview.is_empty() {
        map.insert("body_preview".to_string(), json!(preview));
    }

    if let Some(source) = source {
        let mut source_map = Map::new();
        source_map.insert("id".to_string(), json!(source.id));
        if let Some(subject) = source.subject {
            source_map.insert("subject".to_string(), json!(subject));
        }
        if let Some(from) = source.from {
            source_map.insert("from".to_string(), json!(from));
        }
        map.insert("source_message".to_string(), Value::Object(source_map));
    }

    if action == SummaryAction::DeleteAll {
        map.insert(
            "warning".to_string(),
            json!("永久删除回收站全部邮件，数量未知"),
        );
    }

    map.insert("cli".to_string(), cli);

    Value::Object(map)
}
