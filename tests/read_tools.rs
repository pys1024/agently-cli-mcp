#![cfg(feature = "test-fixtures")]

//! In-memory tool tests: connect a real rmcp client to `build_server` over a
//! `tokio::io::duplex` stream and exercise the read-only tools end to end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config;
use agently_cli_mcp::guard::Sandbox;
use agently_cli_mcp::mcp::{AppState, build_server};
use clap::Parser;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RoleServer, RunningService, serve_directly};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// Serializes access to the process-global `FAKE_SCENARIO` / `FAKE_ARGV_LOG`
/// environment variables, which the fake CLI reads at spawn time.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-agently-cli")
}

fn set_scenario(scenario: &str, argv_log: &Path) {
    // SAFETY: this test process is single-threaded with respect to these env
    // vars; every test that spawns the fake CLI holds `ENV_LOCK`.
    unsafe {
        std::env::set_var("FAKE_SCENARIO", scenario);
        std::env::set_var("FAKE_ARGV_LOG", argv_log);
    }
}

/// Build an `AppState` whose runner points at the fake CLI and whose sandbox is
/// rooted at the given temp directory.
fn build_state(dir: &TempDir) -> Arc<AppState> {
    let mut env = HashMap::new();
    env.insert("AGENTLY_CLI_PATH".to_string(), fake_bin().to_string());
    env.insert(
        "AGENTLY_MCP_WORKDIR".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    let args = CliArgs::try_parse_from(["agently-cli-mcp"]).unwrap();
    let cfg = config::load_with_sources(args, &env, dir.path()).unwrap();
    let runner = Runner::new(&cfg.cli).unwrap();
    let sandbox = Sandbox::new(dir.path().to_path_buf()).unwrap();
    Arc::new(AppState::new(
        Arc::new(cfg),
        Arc::new(runner),
        Arc::new(sandbox),
    ))
}

/// Run a client connected to `build_server(state)` and invoke `body`.
async fn with_server<F, Fut>(state: Arc<AppState>, body: F)
where
    F: FnOnce(RunningService<RoleClient, ()>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let server = build_server(state);
    let (server_tx, client_tx) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        let running = serve_directly::<RoleServer, _, _, _, _>(server, server_tx, None);
        let _ = running.waiting().await;
    });
    let client = serve_directly::<RoleClient, _, _, _, _>((), client_tx, None);
    body(client).await;
    server_task.abort();
}

/// Convert a JSON value into the `JsonObject` argument map `call_tool` expects.
fn args(obj: Value) -> Map<String, Value> {
    serde_json::from_value(obj).unwrap()
}

/// Read the fake CLI's argv log as a list of per-invocation argument vectors.
///
/// Returns an empty list when the log has not been created, so callers can
/// assert "the CLI was never spawned" without first checking for the file.
fn read_argv_log(path: &PathBuf) -> Vec<Vec<String>> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn list_returns_messages_and_fences_snippets() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(CallToolRequestParams::new("mail_list_messages"))
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], true);
        assert_eq!(sc["data"]["messages"][0]["message_id"], "msg_1");
        assert_eq!(sc["meta"]["untrusted_content"], true);

        let text = result.content[0].as_text().unwrap().text.as_str();
        assert!(text.contains("-----BEGIN UNTRUSTED EMAIL DATA-----"));
        assert!(text.contains("[SECURITY NOTICE]"));
    })
    .await;
}

#[tokio::test]
async fn get_marks_read_side_effect() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_read", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_get_message")
                    .with_arguments(args(json!({ "id": "msg_1", "max_body_chars": 100 }))),
            )
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        let side_effects = sc["meta"]["side_effects"]
            .as_array()
            .expect("side_effects array");
        assert!(
            side_effects
                .iter()
                .any(|v| v.as_str() == Some("marked_read")),
            "side_effects must contain marked_read"
        );
        assert_eq!(sc["meta"]["untrusted_content"], true);
        assert_eq!(sc["meta"]["body_truncated"], true);
        assert!(sc["data"]["body"].as_str().unwrap().chars().count() <= 100);
    })
    .await;
}

#[tokio::test]
async fn search_uses_cursor_context() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("search_page", &log);
    let state = build_state(&dir);

    with_server(state, move |client| {
        let log = log.clone();
        async move {
            let first = client
                .call_tool(
                    CallToolRequestParams::new("mail_search_messages")
                        .with_arguments(args(json!({ "q": "needle" }))),
                )
                .await
                .unwrap();

            let cursor = first.structured_content.as_ref().unwrap()["data"]["next_cursor"]
                .as_str()
                .expect("next_cursor")
                .to_string();

            let second = client
                .call_tool(
                    CallToolRequestParams::new("mail_search_messages")
                        .with_arguments(args(json!({ "cursor": cursor }))),
                )
                .await
                .unwrap();

            assert_eq!(
                first.structured_content.as_ref().unwrap()["data"]["messages"],
                second.structured_content.as_ref().unwrap()["data"]["messages"],
                "a cursor-only follow-up must restore the stored filters"
            );

            // Prove the context was restored on the wire: the second search
            // re-applied `--q needle` even though the client only sent `cursor`.
            let calls = read_argv_log(&log);
            assert!(
                calls.iter().any(|argv| {
                    argv.contains(&"--cursor".to_string())
                        && argv.contains(&"--q".to_string())
                        && argv.contains(&"needle".to_string())
                }),
                "the cursor-only call must re-apply the stored --q"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn download_rejects_outside_path() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_download_attachment").with_arguments(args(
                    json!({ "msg": "msg_1", "att": "att_1", "output": "/tmp" }),
                )),
            )
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], false);
        assert_eq!(sc["error"]["code"], "PATH_OUTSIDE_WORKDIR");
        assert_eq!(result.is_error, Some(true));
    })
    .await;
}

#[tokio::test]
async fn download_validates_msg_and_att_prefixes() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("success_list", &log);
    let state = build_state(&dir);

    with_server(state, move |client| {
        let log = log.clone();
        async move {
            let bad_msg = client
                .call_tool(
                    CallToolRequestParams::new("mail_download_attachment")
                        .with_arguments(args(json!({ "msg": "not_msg", "att": "att_1" }))),
                )
                .await
                .unwrap();
            assert_eq!(
                bad_msg.structured_content.as_ref().unwrap()["error"]["code"],
                "INVALID_ARGUMENT"
            );

            let bad_att = client
                .call_tool(
                    CallToolRequestParams::new("mail_download_attachment")
                        .with_arguments(args(json!({ "msg": "msg_1", "att": "not_att" }))),
                )
                .await
                .unwrap();
            assert_eq!(
                bad_att.structured_content.as_ref().unwrap()["error"]["code"],
                "INVALID_ARGUMENT"
            );

            // Validation happens before the CLI is spawned.
            let calls = read_argv_log(&log);
            assert!(calls.is_empty(), "no CLI call must happen on bad IDs");
        }
    })
    .await;
}

#[tokio::test]
async fn tool_error_message_redacts_ctk_token() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("error_with_token", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(CallToolRequestParams::new("mail_list_messages"))
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], false);
        assert_eq!(sc["error"]["code"], "INVALID_ARGUMENT");
        let message = sc["error"]["message"]
            .as_str()
            .expect("message is a string");
        assert!(
            !message.contains("ctk_1234567890"),
            "full token leaked into tool error: {message}"
        );
        assert!(
            message.contains("ctk_12345678***"),
            "token must be masked: {message}"
        );
    })
    .await;
}

#[tokio::test]
async fn list_normalizes_uppercase_dir_before_invoking_cli() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("success_list", &log);
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_list_messages")
                    .with_arguments(args(json!({ "dir": "INBOX", "limit": 5 }))),
            )
            .await
            .unwrap();
        assert_eq!(result.structured_content.as_ref().unwrap()["ok"], true);

        let calls = read_argv_log(&log);
        let list_call = calls
            .iter()
            .find(|call| call.first().map(String::as_str) == Some("message"))
            .expect("the list invocation must be logged");
        assert_eq!(
            list_call,
            &vec![
                "message".to_string(),
                "+list".to_string(),
                "--dir".to_string(),
                "inbox".to_string(),
                "--limit".to_string(),
                "5".to_string(),
            ]
        );
    })
    .await;
}
