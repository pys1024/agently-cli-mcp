#![cfg(feature = "test-fixtures")]

//! Streamable HTTP transport tests: health endpoint, bearer auth, Host
//! allowlisting, and end-to-end tool calls over a real TCP connection.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config::{self, HttpConfig};
use agently_cli_mcp::mcp::{self, AppState};
use clap::Parser;
use reqwest::StatusCode;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

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

/// Bind a listener to an ephemeral loopback port, return its address, and drop
/// the listener so `serve` can rebind it.
async fn free_port() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

fn default_http_cfg(listen: SocketAddr) -> HttpConfig {
    HttpConfig {
        listen,
        http_token: None,
        http_token_file: None,
        allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
        disable_host_validation: false,
        cors_origins: vec![],
        session_idle_secs: 300,
    }
}

/// Build shared state rooted at `dir` with the fake CLI and the given HTTP config.
fn build_state(dir: &TempDir, http_cfg: &HttpConfig) -> Arc<AppState> {
    let mut env = HashMap::new();
    env.insert("AGENTLY_CLI_PATH".to_string(), fake_bin().to_string());
    env.insert(
        "AGENTLY_MCP_WORKDIR".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    let args = CliArgs::try_parse_from(["agently-cli-mcp"]).unwrap();
    let mut cfg = config::load_with_sources(args, &env, dir.path()).unwrap();
    cfg.http = http_cfg.clone();
    mcp::build_state(cfg).unwrap()
}

/// Spawn the HTTP server in the background, returning its address and a token
/// that stops it.
async fn spawn_server(
    state: Arc<AppState>,
    http_cfg: HttpConfig,
) -> (SocketAddr, CancellationToken) {
    let addr = http_cfg.listen;
    let ct = CancellationToken::new();
    let shutdown = ct.clone();
    tokio::spawn(async move {
        let _ = mcp::http::serve(state, &http_cfg, shutdown).await;
    });
    (addr, ct)
}

/// Poll `/healthz` until the server responds successfully.
async fn wait_ready(addr: SocketAddr) {
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/healthz");
    for _ in 0..200 {
        if let Ok(resp) = client.get(&url).send().await
            && resp.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not become ready at {addr}");
}

/// Send a minimal raw HTTP/1.1 request over TCP and return the status line code.
async fn raw_status(addr: SocketAddr, method: &str, path: &str, host: &str) -> StatusCode {
    raw_status_with_headers(addr, method, path, host, &[]).await
}

/// Like [`raw_status`], with extra raw header lines (each a full `Name: value`).
async fn raw_status_with_headers(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[&str],
) -> StatusCode {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    for header in extra_headers {
        req.push_str(header);
        req.push_str("\r\n");
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    let code = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    StatusCode::from_u16(code).unwrap()
}

/// Spawn a server with `http_cfg` and assert `/mcp` accepts an unauthenticated
/// initialize request (i.e. the endpoint is open).
async fn assert_open_access(http_cfg: HttpConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let client = reqwest::Client::new();
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    });
    let resp = client
        .post(format!("http://{addr}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    ct.cancel();
}

#[tokio::test]
async fn healthz_is_public() {
    let dir = tempfile::tempdir().unwrap();
    let listen = free_port().await;
    let http_cfg = default_http_cfg(listen);
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let resp = reqwest::get(format!("http://{addr}/healthz"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    ct.cancel();
}

#[tokio::test]
async fn mcp_requires_bearer_token_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let listen = free_port().await;
    let mut http_cfg = default_http_cfg(listen);
    http_cfg.http_token = Some("sekrit".to_string());
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    });

    async fn post(
        client: &reqwest::Client,
        url: &str,
        init: &serde_json::Value,
        token: Option<&str>,
    ) -> reqwest::Response {
        let mut req = client
            .post(url)
            .header("Accept", "application/json, text/event-stream")
            .json(init);
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        req.send().await.unwrap()
    }

    let no_auth = post(&client, &url, &init, None).await;
    assert_eq!(no_auth.status(), StatusCode::UNAUTHORIZED);

    let wrong = post(&client, &url, &init, Some("wrong")).await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let good = post(&client, &url, &init, Some("sekrit")).await;
    assert_eq!(good.status(), StatusCode::OK);

    ct.cancel();
}

#[tokio::test]
async fn no_token_means_open_access() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let listen = free_port().await;
    let http_cfg = default_http_cfg(listen);
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    // No Authorization header anywhere: the client connects and lists tools.
    let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
    let client = ().serve(transport).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name == "mail_list_messages"),
        "server must advertise mail_list_messages without auth"
    );

    client.cancel().await.unwrap();
    ct.cancel();
}

#[test]
fn non_loopback_without_token_warns_but_starts() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agently-cli-mcp"))
        .args([
            "--transport",
            "http",
            "--listen",
            "0.0.0.0:0",
            "--cli",
            fake_bin(),
            "--no-config",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");

    // Give it time to start and emit the warning.
    std::thread::sleep(Duration::from_millis(700));

    // The server must still be running (startup succeeded).
    if let Some(status) = child.try_wait().expect("try_wait") {
        panic!("server exited before it was killed: {status}");
    }

    child.kill().expect("kill");
    let output = child.wait_with_output().expect("wait_with_output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("WARNING"),
        "expected a prominent non-loopback warning on stderr, got: {stderr}"
    );
}

#[tokio::test]
async fn cors_origin_is_enforced_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let listen = free_port().await;
    let mut http_cfg = default_http_cfg(listen);
    http_cfg.cors_origins = vec!["https://app.example.com".to_string()];
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    // A configured (allowed) Origin is accepted: the request gets past origin
    // validation (it is not 403).
    let allowed = raw_status_with_headers(
        addr,
        "GET",
        "/mcp",
        "127.0.0.1",
        &["Origin: https://app.example.com"],
    )
    .await;
    assert_ne!(allowed, StatusCode::FORBIDDEN, "allowed origin must pass");

    // A disallowed Origin is rejected with 403.
    let denied = raw_status_with_headers(
        addr,
        "GET",
        "/mcp",
        "127.0.0.1",
        &["Origin: https://evil.example.com"],
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);

    ct.cancel();
}

#[tokio::test]
async fn bearer_is_case_insensitive_and_whitespace_agnostic() {
    let dir = tempfile::tempdir().unwrap();
    let listen = free_port().await;
    let mut http_cfg = default_http_cfg(listen);
    http_cfg.http_token = Some("sekrit".to_string());
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    });

    // Case-insensitive scheme, arbitrary whitespace, and the challenge header.
    let resp = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", "bearer   sekrit")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // A missing credential must be rejected with a WWW-Authenticate challenge.
    let no_auth = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(no_auth.status(), StatusCode::UNAUTHORIZED);
    let challenge = no_auth
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    assert_eq!(
        challenge.as_deref(),
        Some("Bearer"),
        "401 must carry WWW-Authenticate: Bearer"
    );

    ct.cancel();
}

#[tokio::test]
async fn disallowed_host_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let listen = free_port().await;
    let http_cfg = default_http_cfg(listen);
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let status = raw_status(addr, "GET", "/mcp", "evil.example").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    ct.cancel();
}

#[tokio::test]
async fn tool_call_over_http_works() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));
    let listen = free_port().await;
    let http_cfg = default_http_cfg(listen);
    let state = build_state(&dir, &http_cfg);
    let (addr, ct) = spawn_server(state, http_cfg).await;
    wait_ready(addr).await;

    let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
    let client = ().serve(transport).await.unwrap();

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

    client.cancel().await.unwrap();
    ct.cancel();
}

#[tokio::test]
async fn empty_or_whitespace_token_means_open_access() {
    // Empty token.
    let mut cfg = default_http_cfg(free_port().await);
    cfg.http_token = Some(String::new());
    assert_open_access(cfg).await;

    // Whitespace-only token.
    let mut cfg = default_http_cfg(free_port().await);
    cfg.http_token = Some("   ".to_string());
    assert_open_access(cfg).await;

    // Whitespace-only token file.
    let token_dir = tempfile::tempdir().unwrap();
    let token_file = token_dir.path().join("token");
    std::fs::write(&token_file, "  \n\t\n").unwrap();
    let mut cfg = default_http_cfg(free_port().await);
    cfg.http_token_file = Some(token_file.to_string_lossy().to_string());
    assert_open_access(cfg).await;
}

/// SIGTERM must cancel the shutdown token, close active SSE sessions, and let
/// the server exit gracefully within a bound — even while a client holds an
/// open SSE stream.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_shuts_down_within_bound() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let listen = free_port().await;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_agently-cli-mcp"))
        .args([
            "--transport",
            "http",
            "--listen",
            &format!("{listen}"),
            "--cli",
            fake_bin(),
            "--no-config",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    wait_ready(listen).await;

    // Open a session plus its long-lived SSE stream so graceful shutdown has a
    // connection to drain.
    let transport = StreamableHttpClientTransport::from_uri(format!("http://{listen}/mcp"));
    let client = ().serve(transport).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pid = child.id().expect("child pid");
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM).expect("SIGTERM");

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let status = match status {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => panic!("wait failed: {e}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            panic!("server did not exit within 5s of SIGTERM");
        }
    };
    assert!(
        status.success(),
        "graceful shutdown should exit 0, got {status}"
    );

    let _ = client.cancel().await;
}
