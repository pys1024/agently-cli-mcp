//! Full config surface: HTTP token/token-file, non-loopback without token
//! (allowed, warning-only), `--no-config`, explicit missing config, the
//! allow/disable tool conflict, and numeric range validation.

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config::{Transport, load_with_sources};
use agently_cli_mcp::tools;
use clap::Parser;
use std::collections::HashMap;
use std::fs;
use tempfile::tempdir;

fn args(argv: &[&str]) -> CliArgs {
    CliArgs::try_parse_from(std::iter::once("agently-cli-mcp").chain(argv.iter().copied())).unwrap()
}

#[test]
fn http_token_and_token_file_are_resolved_and_redacted() {
    let dir = tempdir().unwrap();
    let token_path = dir.path().join("token.txt");
    fs::write(&token_path, "sekrit\n").unwrap();

    // --http-token-file stores only the path; the value is read at serve time.
    let cfg = load_with_sources(
        args(&["--http-token-file", token_path.to_str().unwrap()]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        cfg.http.http_token_file.as_deref(),
        Some(token_path.to_str().unwrap())
    );
    assert!(cfg.http.http_token.is_none());

    // The env var supplies the token value directly.
    let mut env = HashMap::new();
    env.insert(
        "AGENTLY_MCP_HTTP_TOKEN".to_string(),
        "topsecret".to_string(),
    );
    let cfg = load_with_sources(args(&[]), &env, dir.path()).unwrap();
    assert_eq!(cfg.http.http_token.as_deref(), Some("topsecret"));

    let redacted = cfg.redacted();
    assert_eq!(redacted["http"]["http_token"], "<redacted>");
    assert!(
        !redacted.to_string().contains("topsecret"),
        "token value leaked into redacted config"
    );
}

#[test]
fn non_loopback_without_token_is_allowed() {
    let dir = tempdir().unwrap();
    let cfg = load_with_sources(
        args(&["--transport", "http", "--listen", "0.0.0.0:8787"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();
    assert!(matches!(cfg.transport, Transport::Http));
    assert!(!cfg.http.listen.ip().is_loopback());
    assert!(cfg.http.http_token.is_none());
    assert!(cfg.http.http_token_file.is_none());
    // No error: validation allows it; a warning is logged only at serve time.
}

#[test]
fn no_config_ignores_default_file() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("config.toml"), "transport = \"http\"\n").unwrap();
    let cfg = load_with_sources(args(&["--no-config"]), &HashMap::new(), dir.path()).unwrap();
    assert!(matches!(cfg.transport, Transport::Stdio));
}

#[test]
fn explicit_missing_config_is_an_error() {
    let dir = tempdir().unwrap();
    let err = load_with_sources(
        args(&["--config", "nope.toml"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("nope.toml"));
}

#[test]
fn allow_and_disabled_conflict_is_an_error() {
    let dir = tempdir().unwrap();
    let err = load_with_sources(
        args(&[
            "--allow-tool",
            "mail_list_messages",
            "--disable-tool",
            "mail_delete_messages",
        ]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("allow"), "unexpected message: {msg}");
    assert!(msg.contains("disabled"), "unexpected message: {msg}");
}

#[test]
fn read_only_stacks_with_allow() {
    let dir = tempdir().unwrap();
    let cfg = load_with_sources(
        args(&["--read-only", "--allow-tool", "mail_send_message"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();
    let enabled = tools::enabled(&cfg.tools);
    assert!(
        !enabled.contains(&"mail_send_message"),
        "read-only must still filter an allowed write tool"
    );
}

#[test]
fn unknown_disable_tool_name_is_rejected() {
    let dir = tempdir().unwrap();
    let err = load_with_sources(
        args(&["--disable-tool", "mail_not_a_real_tool"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown tool name"),
        "unexpected message: {msg}"
    );
    assert!(
        msg.contains("mail_not_a_real_tool"),
        "unexpected message: {msg}"
    );
}

#[test]
fn unknown_allow_tool_name_is_rejected() {
    let dir = tempdir().unwrap();
    let err = load_with_sources(
        args(&["--allow-tool", "mail_does_not_exist"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown tool name"),
        "unexpected message: {msg}"
    );
    assert!(
        msg.contains("mail_does_not_exist"),
        "unexpected message: {msg}"
    );
}

#[test]
fn watch_max_secs_range_is_enforced() {
    let dir = tempdir().unwrap();
    let ok = load_with_sources(
        args(&["--watch-max-secs", "120"]),
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();
    assert_eq!(ok.cli.watch_max_secs, 120);

    assert!(
        load_with_sources(
            args(&["--watch-max-secs", "4"]),
            &HashMap::new(),
            dir.path()
        )
        .is_err()
    );
    assert!(
        load_with_sources(
            args(&["--watch-max-secs", "121"]),
            &HashMap::new(),
            dir.path()
        )
        .is_err()
    );
}

#[test]
fn max_body_chars_and_max_concurrency_are_validated() {
    let dir = tempdir().unwrap();
    assert!(
        load_with_sources(
            args(&["--max-body-chars", "0"]),
            &HashMap::new(),
            dir.path()
        )
        .is_err()
    );
    assert!(
        load_with_sources(
            args(&["--max-concurrency", "0"]),
            &HashMap::new(),
            dir.path()
        )
        .is_err()
    );
}
