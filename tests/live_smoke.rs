//! Live read-only smoke tests against the real, authorized `agently-cli`.
//!
//! Every test here is `#[ignore]`d so it never runs as part of the normal
//! suite. It only executes when invoked with `--ignored` **and**
//! `AGENTLY_MCP_LIVE=1` is set. When it runs it talks to the real
//! `agently-cli` found on `PATH` (or pointed at by `AGENTLY_CLI_PATH`) and
//! exercises only the read-only tools: `mail_get_account` and
//! `mail_list_messages`.
//!
//! These tests never send, reply, forward, trash, delete, or refresh
//! credentials. Sending mail additionally requires `AGENTLY_MCP_LIVE_SEND=1`
//! plus a self-addressed recipient, and is intentionally not part of this
//! suite.

use std::collections::HashMap;
use std::sync::Arc;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config;
use agently_cli_mcp::mcp::{AppState, build_server};
use clap::Parser;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RoleServer, RunningService, serve_directly};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// True when the live smoke suite is enabled via `AGENTLY_MCP_LIVE=1`.
fn live_enabled() -> bool {
    std::env::var("AGENTLY_MCP_LIVE").as_deref() == Ok("1")
}

/// Build the server state against the real CLI, honoring any `AGENTLY_MCP_*`
/// / `AGENTLY_CLI_PATH` overrides present in the process environment.
fn build_state(dir: &TempDir) -> Arc<AppState> {
    let args = CliArgs::try_parse_from(["agently-cli-mcp"]).unwrap();
    let env: HashMap<String, String> = std::env::vars().collect();
    let cfg = config::load_with_sources(args, &env, dir.path()).unwrap();
    agently_cli_mcp::mcp::build_state(cfg).unwrap()
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

#[tokio::test]
#[ignore = "requires AGENTLY_MCP_LIVE=1 and an authorized agently-cli"]
async fn get_account_reports_account_and_auth() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(CallToolRequestParams::new("mail_get_account"))
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], true, "mail_get_account failed: {sc}");
        assert!(
            sc["data"]["account"].is_object(),
            "mail_get_account must return the `+me` payload: {sc}"
        );
        assert!(
            sc["data"]["auth"].is_object(),
            "mail_get_account must return the `auth status` payload: {sc}"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "requires AGENTLY_MCP_LIVE=1 and an authorized agently-cli"]
async fn list_messages_returns_a_single_inbox_preview() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_list_messages")
                    .with_arguments(args(json!({ "limit": 1 }))),
            )
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], true, "mail_list_messages failed: {sc}");
        assert_eq!(
            sc["meta"]["untrusted_content"], true,
            "list results are untrusted email data: {sc}"
        );

        // The CLI nests the message list under `data.data` and the cursor under
        // `data.pagination`; both are forwarded verbatim by the tool.
        let messages = sc["data"]["data"]
            .as_array()
            .expect("data.data is an array");
        assert!(
            messages.len() <= 1,
            "--limit 1 must return at most one message: {sc}"
        );
    })
    .await;
}
