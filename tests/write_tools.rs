#![cfg(feature = "test-fixtures")]

//! Write-tool tests: arg mapping (`--body-file`, no `--body`/`--confirmed`),
//! `never`-mode behavior, reply/forward side effects, and success envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config;
use agently_cli_mcp::guard::Sandbox;
use agently_cli_mcp::mcp::{AppState, build_server};
use clap::Parser;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ElicitRequestParams, ElicitResult,
    ElicitationAction, Implementation, InitializeRequestParams, InitializeResult, ProtocolVersion,
    ServerCapabilities,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, RunningService, serve_directly};
use rmcp::{ClientHandler, ErrorData};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-agently-cli")
}

fn set_scenario(scenario: &str, argv_log: &Path) {
    unsafe {
        std::env::set_var("FAKE_SCENARIO", scenario);
        std::env::set_var("FAKE_ARGV_LOG", argv_log);
    }
}

fn build_state(dir: &TempDir, confirm_mode: &str) -> Arc<AppState> {
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
    Arc::new(AppState::new(
        Arc::new(cfg),
        Arc::new(runner),
        Arc::new(sandbox),
    ))
}

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
        Implementation::new("write-test-client", "0.0.0"),
    )
    .with_protocol_version(protocol_version)
}

fn server_info(protocol_version: ProtocolVersion) -> InitializeResult {
    let mut info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
    info.protocol_version = protocol_version;
    info
}

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
    match std::fs::read_to_string(path) {
        Ok(content) => content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[tokio::test]
async fn send_uses_body_file_and_no_confirmed_in_token_mode() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state(&dir, "token");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        false,
        Elicit::Accept,
        |client| async move {
            let result = client
                .call_tool(
                    CallToolRequestParams::new("mail_send_message").with_arguments(args(json!({
                        "to": ["a@example.com"],
                        "subject": "Hello",
                        "body": "hi",
                    }))),
                )
                .await
                .unwrap();

            let sc = result
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["status"], "confirmation_required");

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 1);
            let argv = &calls[0];
            assert!(
                argv.iter().any(|a| a == "--body-file"),
                "send body must go through --body-file"
            );
            assert!(
                !argv.iter().any(|a| a == "--body"),
                "send must never use --body"
            );
            assert!(
                !argv.iter().any(|a| a == "--confirmed"),
                "elicit/token modes must never pass --confirmed"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn trash_and_delete_never_receive_confirmed_in_never_mode() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_then_success", &log);
    let state = build_state(&dir, "never");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        false,
        Elicit::Accept,
        |client| async move {
            let trash = client
                .call_tool(
                    CallToolRequestParams::new("mail_move_to_trash")
                        .with_arguments(args(json!({"id": "msg_1"}))),
                )
                .await
                .unwrap();
            let sc = trash
                .structured_content
                .as_ref()
                .expect("structured content");
            // trash has no --confirmed, so even `never` falls back to token.
            assert_eq!(sc["status"], "confirmation_required");

            let delete = client
                .call_tool(
                    CallToolRequestParams::new("mail_delete_messages")
                        .with_arguments(args(json!({"all": true}))),
                )
                .await
                .unwrap();
            let sc2 = delete
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc2["status"], "confirmation_required");

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 2);
            for argv in &calls {
                assert!(
                    !argv.iter().any(|a| a == "--confirmed"),
                    "trash/delete must never receive --confirmed"
                );
            }
        },
    )
    .await;
}

#[tokio::test]
async fn declined_reply_reports_source_marked_read() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("write_read_flow", &log);
    let state = build_state(&dir, "elicit");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        true,
        Elicit::Decline,
        |client| async move {
            let result = client
                .call_tool(
                    CallToolRequestParams::new("mail_reply_message").with_arguments(args(json!({
                        "id": "msg_1",
                        "body": "hi",
                    }))),
                )
                .await
                .unwrap();

            let sc = result
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["status"], "declined");
            let side_effects = sc["meta"]["side_effects"]
                .as_array()
                .expect("side_effects array");
            assert!(
                side_effects
                    .iter()
                    .any(|v| v.as_str() == Some("source_marked_read")),
                "a declined reply that pre-read the original must report source_marked_read"
            );

            let calls = read_argv_log(&log);
            assert!(
                calls
                    .iter()
                    .any(|argv| argv.get(1).map(|s| s.as_str()) == Some("+read"))
            );
            assert!(
                !calls
                    .iter()
                    .any(|argv| argv.contains(&"--confirmation-token".to_string()))
            );
        },
    )
    .await;
}

#[tokio::test]
async fn successful_send_returns_queued_in_never_mode() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("confirm_confirmed", &log);
    let state = build_state(&dir, "never");

    with_server(
        state,
        ProtocolVersion::V_2025_11_25,
        false,
        Elicit::Accept,
        |client| async move {
            let result = client
                .call_tool(
                    CallToolRequestParams::new("mail_send_message").with_arguments(args(json!({
                        "to": ["a@example.com"],
                        "subject": "Hello",
                        "body": "hi",
                    }))),
                )
                .await
                .unwrap();

            let sc = result
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["ok"], true);
            assert_eq!(sc["data"]["queued"], true);

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 1);
            assert!(
                calls[0].iter().any(|a| a == "--confirmed"),
                "never mode must pass --confirmed for send"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn forward_rejects_empty_body() {
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
            let result = client
                .call_tool(
                    CallToolRequestParams::new("mail_forward_message").with_arguments(args(
                        json!({
                            "id": "msg_1",
                            "to": ["a@example.com"],
                            "body": "",
                        }),
                    )),
                )
                .await
                .unwrap();

            let sc = result
                .structured_content
                .as_ref()
                .expect("structured content");
            assert_eq!(sc["ok"], false);
            assert_eq!(sc["error"]["code"], "INVALID_ARGUMENT");

            let calls = read_argv_log(&log);
            assert_eq!(calls.len(), 0, "validation must fail before the CLI runs");
        },
    )
    .await;
}
