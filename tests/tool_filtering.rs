#![cfg(feature = "test-fixtures")]

//! Tool filtering: `--read-only` and `--disable-tool` omit tools from
//! `tools/list` and reject direct calls with `TOOL_DISABLED`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config;
use agently_cli_mcp::guard::Sandbox;
use agently_cli_mcp::mcp::{AppState, build_server};
use clap::Parser;
use rmcp::model::{CallToolRequestParams, Tool};
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

/// Build an `AppState` whose runner points at the fake CLI, with extra
/// env-driven config (read-only / disable-tool) layered on top.
fn build_state(dir: &TempDir, extra_env: &[(&str, &str)]) -> Arc<AppState> {
    let mut env = HashMap::new();
    env.insert("AGENTLY_CLI_PATH".to_string(), fake_bin().to_string());
    env.insert(
        "AGENTLY_MCP_WORKDIR".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    for (k, v) in extra_env {
        env.insert(k.to_string(), v.to_string());
    }
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

fn tool_names(tools: &[Tool]) -> Vec<String> {
    tools.iter().map(|t| t.name.to_string()).collect()
}

#[tokio::test]
async fn read_only_omits_write_tools_from_list() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let state = build_state(&dir, &[("AGENTLY_MCP_READ_ONLY", "true")]);

    with_server(state, |client| async move {
        let result = client.list_tools(None).await.unwrap();
        let names = tool_names(&result.tools);
        assert!(names.contains(&"mail_list_messages".to_string()));
        assert!(!names.contains(&"mail_send_message".to_string()));
        assert!(!names.contains(&"mail_reply_message".to_string()));
        assert!(!names.contains(&"mail_forward_message".to_string()));
        assert!(!names.contains(&"mail_move_to_trash".to_string()));
        assert!(!names.contains(&"mail_delete_messages".to_string()));
        assert!(!names.contains(&"mail_refresh_auth".to_string()));
    })
    .await;
}

#[tokio::test]
async fn disable_tool_omits_it_from_list() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let state = build_state(
        &dir,
        &[("AGENTLY_MCP_DISABLE_TOOLS", "mail_delete_messages")],
    );

    with_server(state, |client| async move {
        let result = client.list_tools(None).await.unwrap();
        let names = tool_names(&result.tools);
        assert!(!names.contains(&"mail_delete_messages".to_string()));
        assert!(
            names.contains(&"mail_send_message".to_string()),
            "unrelated write tool must remain enabled"
        );
    })
    .await;
}

#[tokio::test]
async fn disabled_tool_call_is_rejected_with_tool_disabled() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let state = build_state(&dir, &[("AGENTLY_MCP_READ_ONLY", "true")]);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_send_message").with_arguments(args(json!({
                    "to": ["a@example.com"],
                    "subject": "hi",
                    "body": "x",
                }))),
            )
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], false);
        assert_eq!(sc["error"]["code"], "TOOL_DISABLED");
        assert_eq!(result.is_error, Some(true));
    })
    .await;
}
