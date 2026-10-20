#![cfg(feature = "test-fixtures")]

//! Confirmation-flow tests: MRTR (2026-07-28), legacy elicitation
//! (2025-11-25), token fallback, and expiry, driven over a real rmcp
//! client/server pair.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config;
use agently_cli_mcp::confirm::ConfirmStore;
use agently_cli_mcp::guard::Sandbox;
use agently_cli_mcp::mcp::{AppState, build_server};
use clap::Parser;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ElicitRequestParams, ElicitResult,
    ElicitationAction, Implementation, InitializeRequestParams, InitializeResult, InputResponses,
    ProtocolVersion, ServerCapabilities,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, RunningService, serve_directly};
use rmcp::{ClientHandler, ErrorData};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// Serializes access to the process-global `FAKE_SCENARIO` / `FAKE_ARGV_LOG`
/// environment variables, which the fake CLI reads at spawn time.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-agently-cli")
}

fn set_scenario(scenario: &str, argv_log: &Path) {
    // SAFETY: every test that spawns the fake CLI holds `ENV_LOCK`.
    unsafe {
        std::env::set_var("FAKE_SCENARIO", scenario);
        std::env::set_var("FAKE_ARGV_LOG", argv_log);
    }
}

fn build_state_raw(dir: &TempDir, confirm_mode: &str) -> AppState {
    let mut env = HashMap::new();
    env.insert("AGENTLY_CLI_PATH".to_string(), fake_bin().to_string());
    env.insert(
        "AGENTLY_MCP_WORKDIR".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    env.insert("AGENTLY_MCP_CONFIRM".to_string(), confirm_mode.to_string());
    let args = CliArgs::try_parse_from(["agently-cli-mcp"]).unwrap();
    let cfg = config::load_with_sources(args, &env, dir.path()).unwrap();
    let runner = Runner::new(&cfg.cli).unwrap();
    let sandbox = Sandbox::new(dir.path().to_path_buf()).unwrap();
    AppState::new(Arc::new(cfg), Arc::new(runner), Arc::new(sandbox))
}

fn build_state(dir: &TempDir, confirm_mode: &str) -> Arc<AppState> {
    Arc::new(build_state_raw(dir, confirm_mode))
}

fn build_state_with_ttl(dir: &TempDir, confirm_mode: &str, ttl: Duration) -> Arc<AppState> {
    let mut state = build_state_raw(dir, confirm_mode);
    state.confirm = Arc::new(ConfirmStore::with_ttl(ttl));
    Arc::new(state)
}

/// What the client's elicitation handler answers.
#[derive(Clone, Copy)]
enum Elicit {
    Accept,
    Decline,
}

#[derive(Clone)]
struct ElicitHandler(Elicit);

impl ClientHandler for ElicitHandler {
    async fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        match self.0 {
            Elicit::Accept => {
                Ok(ElicitResult::new(ElicitationAction::Accept)
                    .with_content(json!({"confirm": true})))
            }
            Elicit::Decline => Ok(ElicitResult::new(ElicitationAction::Decline)),
        }
    }
}

fn client_info(
    protocol_version: ProtocolVersion,
    declares_elicitation: bool,
) -> InitializeRequestParams {
    let capabilities = if declares_elicitation {
        ClientCapabilities::builder().enable_elicitation().build()
    } else {
        ClientCapabilities::default()
    };
    InitializeRequestParams::new(
        capabilities,
        Implementation::new("confirm-test-client", "0.0.0"),
    )
    .with_protocol_version(protocol_version)
}

fn server_info(protocol_version: ProtocolVersion) -> InitializeResult {
    let mut info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
    info.protocol_version = protocol_version;
    info
}

/// Connect a real client/server pair, negotiating `protocol_version`.
async fn with_server<F, Fut>(
    state: Arc<AppState>,
    protocol_version: ProtocolVersion,
    declares_elicitation: bool,
    elicit: Elicit,
    body: F,
) where
    F: FnOnce(RunningService<RoleClient, ElicitHandler>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let server = build_server(state);
    let (server_tx, client_tx) = tokio::io::duplex(8192);
    let server_version = protocol_version.clone();
    let server_task = tokio::spawn(async move {
        let running = serve_directly::<RoleServer, _, _, _, _>(
            server,
            server_tx,
            Some(client_info(server_version, declares_elicitation)),
        );
        let _ = running.waiting().await;
    });
    let client = serve_directly::<RoleClient, _, _, _, _>(
        ElicitHandler(elicit),
        client_tx,
        Some(server_info(protocol_version).into()),
    );
    body(client).await;
    server_task.abort();
}

fn args(obj: Value) -> Map<String, Value> {
    serde_json::from_value(obj).unwrap()
}

fn read_argv_log(path: &PathBuf) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn send_args() -> CallToolRequestParams {
    CallToolRequestParams::new("mail_send_message").with_arguments(args(
        json!({"to": ["a@example.com"], "subject": "Hello", "body": "hi"}),
    ))
}

#[tokio::test]
async fn new_protocol_returns_input_required_then_accept_sends() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state(&dir, "elicit");

    with_server(
        state,
        ProtocolVersion::V_2026_07_28,
        true,
        Elicit::Accept,
        |client| async move {
            // First round: the server must return InputRequired, not a result.
            let first = client.call_tool_once(send_args()).await.unwrap();
            let ir = match first {
                CallToolResponse::InputRequired(ir) => ir,
                other => panic!("expected InputRequired, got {other:?}"),
            };
            let pending_id = ir.request_state.clone().expect("request_state");

            // Fulfill the elicitation by hand and retry with the echoed state.
            let mut responses = InputResponses::new();
            responses.insert(
                "confirm".to_string(),
                json!({"action": "accept", "content": {"confirm": true}}),
            );
            let retry = send_args()
                .with_request_state(pending_id)
                .with_input_responses(responses);
            let second = client.call_tool_once(retry).await.unwrap();
            match second {
                CallToolResponse::Complete(result) => {
                    let sc = result
                        .structured_content
                        .as_ref()
                        .expect("structured content");
                    assert_eq!(sc["ok"], true);
                    assert_eq!(sc["data"]["queued"], true);
                }
                other => panic!("expected Complete, got {other:?}"),
            }

            // The fake CLI must have seen the one-time token on the replay call.
            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 2, "first call + replay call");
            assert!(
                calls.iter().any(|argv| {
                    argv.contains(&"--confirmation-token".to_string())
                        && argv.contains(&"ctk_test".to_string())
                }),
                "replay must carry the one-time token"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn legacy_protocol_uses_peer_elicit_and_decline_sends_nothing() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state(&dir, "elicit");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        true,
        Elicit::Decline,
        |client| async move {
            let result = client.call_tool(send_args()).await.unwrap();
            let sc = result
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["ok"], true);
            assert_eq!(sc["status"], "declined");

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 1, "only the first CLI call happened");
            assert!(
                !calls
                    .iter()
                    .any(|argv| argv.contains(&"--confirmation-token".to_string())),
                "a decline must never replay the write"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn token_fallback_returns_ctk_and_second_call_uses_cached_args() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state(&dir, "elicit");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        false,
        Elicit::Accept,
        |client| async move {
            let first = client.call_tool(send_args()).await.unwrap();
            let sc = first
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["status"], "confirmation_required");
            let ctk = sc["confirmation_token"]
                .as_str()
                .expect("confirmation_token")
                .to_string();

            // Second call with a mutated subject and the token.
            let second = client
                .call_tool(
                    CallToolRequestParams::new("mail_send_message").with_arguments(args(json!({
                        "to": ["a@example.com"],
                        "subject": "MUTATED",
                        "body": "hi",
                        "confirmation_token": ctk,
                    }))),
                )
                .await
                .unwrap();

            let sc2 = second
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc2["ok"], true);
            assert_eq!(sc2["meta"]["args_changed"], true);

            // The replay must use the cached args (subject "Hello"), never the
            // mutated ones, and it must carry the one-time token.
            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 2);
            let replay = &calls[1];
            assert!(replay.contains(&"--confirmation-token".to_string()));
            assert!(
                replay.contains(&"Hello".to_string()),
                "replay must use the cached subject"
            );
            assert!(
                !replay.contains(&"MUTATED".to_string()),
                "replay must ignore the mutated subject"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn expired_pending_is_rejected() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state_with_ttl(&dir, "token", Duration::ZERO);

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        false,
        Elicit::Accept,
        |client| async move {
            let first = client.call_tool(send_args()).await.unwrap();
            let ctk = first.structured_content.as_ref().unwrap()["confirmation_token"]
                .as_str()
                .unwrap()
                .to_string();

            let second = client
                .call_tool(
                    CallToolRequestParams::new("mail_send_message").with_arguments(args(json!({
                        "to": ["a@example.com"],
                        "subject": "Hello",
                        "body": "hi",
                        "confirmation_token": ctk,
                    }))),
                )
                .await
                .unwrap();

            let sc2 = second
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc2["ok"], false);
            assert_eq!(sc2["error"]["code"], "CONFIRMATION_EXPIRED");

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 1, "no second CLI call after expiry");
        },
    )
    .await;
}
