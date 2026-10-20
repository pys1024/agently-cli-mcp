//! Deterministic confirmation summary tests.

use agently_cli_mcp::confirm::summary::{
    self, AttachmentMeta, SourceMessage, SummaryAction, SummaryInput,
};
use serde_json::json;

fn input(action: SummaryAction) -> SummaryInput {
    SummaryInput {
        action,
        from: Some("me@example.com".to_string()),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: None,
        body_preview: String::new(),
        attachments: vec![],
        source: None,
        cli: json!({}),
    }
}

#[test]
fn send_keeps_recipients_separate() {
    let value = summary::build(SummaryInput {
        to: vec!["a@example.com".to_string(), "b@example.com".to_string()],
        cc: vec!["c@example.com".to_string()],
        bcc: vec!["d@example.com".to_string()],
        subject: Some("Hello".to_string()),
        // The CLI merges all recipients into its own `to`; our summary must not.
        cli: json!({
            "action": "send",
            "to": ["a@example.com", "b@example.com", "c@example.com", "d@example.com"]
        }),
        ..input(SummaryAction::Send)
    });

    assert_eq!(value["to"], json!(["a@example.com", "b@example.com"]));
    assert_eq!(value["cc"], json!(["c@example.com"]));
    assert_eq!(value["bcc"], json!(["d@example.com"]));
    // The merged `to` stays confined to the CLI's own summary.
    assert_eq!(
        value["cli"]["to"],
        json!([
            "a@example.com",
            "b@example.com",
            "c@example.com",
            "d@example.com"
        ])
    );
    assert_eq!(value["to"].as_array().unwrap().len(), 2);
}

#[test]
fn attachments_include_filename_and_size() {
    let value = summary::build(SummaryInput {
        attachments: vec![
            AttachmentMeta {
                filename: "report.pdf".to_string(),
                size: 12345,
            },
            AttachmentMeta {
                filename: "pic.png".to_string(),
                size: 999,
            },
        ],
        ..input(SummaryAction::Send)
    });

    assert_eq!(
        value["attachments"],
        json!([
            { "filename": "report.pdf", "size": 12345 },
            { "filename": "pic.png", "size": 999 },
        ])
    );
}

#[test]
fn body_preview_is_truncated_to_200_chars() {
    let long: String = "字".repeat(250);
    let value = summary::build(SummaryInput {
        body_preview: long,
        ..input(SummaryAction::Send)
    });

    assert_eq!(
        value["body_preview"].as_str().unwrap(),
        "字".repeat(200).as_str()
    );
    assert_eq!(value["body_preview"].as_str().unwrap().chars().count(), 200);
}

#[test]
fn delete_all_includes_unknown_count_warning() {
    let value = summary::build(input(SummaryAction::DeleteAll));

    let warning = value["warning"].as_str().unwrap();
    assert!(warning.contains("数量未知"), "warning was: {warning}");
}

#[test]
fn reply_includes_source_message_when_provided() {
    let value = summary::build(SummaryInput {
        action: SummaryAction::Reply,
        source: Some(SourceMessage {
            id: "msg_123".to_string(),
            subject: Some("Original subject".to_string()),
            from: Some("sender@example.com".to_string()),
        }),
        ..input(SummaryAction::Reply)
    });

    assert_eq!(value["source_message"]["id"], json!("msg_123"));
    assert_eq!(
        value["source_message"]["subject"],
        json!("Original subject")
    );
    assert_eq!(value["source_message"]["from"], json!("sender@example.com"));
}

#[test]
fn source_message_is_omitted_when_absent() {
    let value = summary::build(input(SummaryAction::Send));

    assert!(value.get("source_message").is_none());
}
