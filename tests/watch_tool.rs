#![cfg(feature = "test-fixtures")]

//! Watch-tool tests: the bounded blocking contract, NDJSON event parsing, the
//! process-wide watch lock, and process-tree termination on timeout.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config;
use agently_cli_mcp::guard::Sandbox;
use agently_cli_mcp::mcp::{AppState, build_server};
use agently_cli_mcp::tools::read::{WatchArgs, watch_messages};
use clap::Parser;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RoleServer, RunningService, serve_directly};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// Serializes access to the process-global `FAKE_SCENARIO` / `FAKE_ARGV_LOG` /
/// `FAKE_GRANDCHILD_PID` environment variables, which the fake CLI reads at
/// spawn time.
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

/// Poll until the fake CLI's argv log has at least `lines` entries, indicating
/// a watch call has spawned its CLI and therefore holds the watch lock.
async fn wait_for_argv(log: &Path, lines: usize) {
    for _ in 0..400 {
        if let Ok(content) = std::fs::read_to_string(log)
            && content.lines().count() >= lines
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fake CLI never wrote the expected argv lines");
}

#[tokio::test]
async fn watch_blocks_until_timeout_and_returns_empty_events() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("sleep", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let start = Instant::now();
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_watch_messages")
                    .with_arguments(args(json!({ "timeout_seconds": 1 }))),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], true);
        assert_eq!(sc["data"]["count"], 0);
        assert_eq!(sc["data"]["timed_out"], true);
        assert_eq!(sc["data"]["events"], json!([]));
        assert_eq!(sc["meta"]["untrusted_content"], false);

        let elapsed_ms = sc["data"]["elapsed_ms"]
            .as_u64()
            .expect("elapsed_ms is an integer");
        assert!(
            elapsed_ms >= 1000,
            "elapsed_ms must be at least 1000ms, got {elapsed_ms}"
        );
        assert!(
            elapsed >= Duration::from_secs(1),
            "watch must block for the full timeout, got {elapsed:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn watch_parses_ndjson_events() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("ndjson_events", &dir.path().join("argv.log"));
    let state = build_state(&dir);

    with_server(state, |client| async move {
        let result = client
            .call_tool(
                CallToolRequestParams::new("mail_watch_messages")
                    .with_arguments(args(json!({ "timeout_seconds": 1 }))),
            )
            .await
            .unwrap();

        let sc = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(sc["ok"], true);
        assert_eq!(sc["data"]["count"], 2);
        assert_eq!(sc["data"]["events"][0]["id"], "msg_1");
        assert_eq!(sc["data"]["events"][1]["id"], "msg_2");
        assert_eq!(sc["meta"]["untrusted_content"], true);

        // Non-empty event data must be fenced.
        let text = result.content[0].as_text().unwrap().text.as_str();
        assert!(text.contains("-----BEGIN UNTRUSTED EMAIL DATA-----"));
        assert!(text.contains("[SECURITY NOTICE]"));
    })
    .await;
}

#[tokio::test]
async fn second_watch_returns_watch_busy() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("sleep", &log);
    let state = build_state(&dir);

    // Start the first watch in the background; it acquires the process-wide
    // lock and blocks until timeout.
    let first_state = state.clone();
    let first = tokio::spawn(async move {
        watch_messages(
            &first_state,
            WatchArgs {
                timeout_seconds: Some(30),
                msg_format: None,
            },
        )
        .await
    });

    // Wait until the first watch has spawned its CLI (and thus holds the lock).
    wait_for_argv(&log, 1).await;

    // A second concurrent watch must fail immediately with WATCH_BUSY.
    let second = watch_messages(
        &state,
        WatchArgs {
            timeout_seconds: Some(1),
            msg_format: None,
        },
    )
    .await;
    assert_eq!(second.unwrap_err().code(), "WATCH_BUSY");

    // Cancel the first watch; dropping its future kills the CLI process tree.
    first.abort();
    let _ = first.await;
}

#[cfg(unix)]
#[tokio::test]
async fn watch_timeout_kills_cli_tree() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("grandchild.pid");
    unsafe {
        std::env::set_var("FAKE_SCENARIO", "spawn_child");
        std::env::set_var("FAKE_GRANDCHILD_PID", &pid_file);
        std::env::remove_var("FAKE_ARGV_LOG");
        std::env::remove_var("FAKE_SPAWN_GO");
    }
    let state = build_state(&dir);

    let (data, _meta) = watch_messages(
        &state,
        WatchArgs {
            timeout_seconds: Some(1),
            msg_format: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(data["timed_out"], true);

    // The fake CLI recorded the grandchild PID before it was killed.
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("grandchild PID must be written")
        .trim()
        .parse()
        .expect("grandchild PID must be an integer");

    // The whole process tree must be gone: kill(pid, 0) returns ESRCH.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "grandchild {pid} survived the watch timeout"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
