//! Redaction tests: confirmation tokens, bearer credentials, and body text.

use agently_cli_mcp::guard::redact;

#[test]
fn token_masks_ctk_to_eight_chars() {
    assert_eq!(redact::token("ctk_1234567890"), "ctk_12345678***");
}

#[test]
fn token_leaves_short_tokens_untouched() {
    assert_eq!(redact::token("ctk_1234"), "ctk_1234");
}

#[test]
fn token_masks_arbitrary_tokens() {
    assert_eq!(redact::token("abcdefghijk"), "abcdefgh***");
}

#[test]
fn text_masks_bearer_tokens() {
    let out = redact::text("Authorization: Bearer supersecrettoken123");
    assert!(!out.contains("supersecrettoken123"));
    assert!(out.contains("Bearer"));
}

#[test]
fn text_masks_ctk_tokens() {
    let out = redact::text("used token ctk_abcdef123456 for send");
    assert!(!out.contains("abcdef123456"));
    assert!(out.contains("ctk_abcdef12***"));
}

#[test]
fn text_does_not_log_full_bodies() {
    let body = "the quick brown fox ".repeat(500);
    let out = redact::text(&body);
    assert!(out.len() < body.len());
}
