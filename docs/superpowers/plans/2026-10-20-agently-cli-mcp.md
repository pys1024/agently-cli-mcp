# agently-cli-mcp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust MCP server that exposes the npm-installed `agently-cli` as 12 safe, schema-typed mail tools over stdio and Streamable HTTP, distributed as one binary for 5 targets.

**Architecture:** A single `agently-cli-mcp` process hosts an rmcp `ServerHandler`. Each tool call maps to one short-lived `agently-cli` subprocess; a guard layer enforces a path sandbox, attachment limits, confirmation, redaction, and untrusted-content fencing. Write tools use MRTR confirmation on new protocol versions and `peer.elicit` on legacy clients, with a token fallback.

**Tech Stack:** Rust 2024 (toolchain 1.97.1, MSRV 1.88), `rmcp` 3.5.1, `tokio`, `axum`, `clap`, `serde`, `toml`, `schemars`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-10-20-agently-cli-mcp-design.md`

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`; use the local 1.97.1 toolchain.
- Pin `rmcp = "=3.5.1"` with features `server`, `macros`, `schemars`, `transport-io`, `transport-streamable-http-server`, `elicitation`, `request-state`.
- Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. No Windows arm64.
- Distribution: GitHub Releases binaries only. No npm platform packages. No code signing.
- Never bundle or reimplement `agently-cli`; invoke the npm-installed binary.
- stdio: stdout carries JSON-RPC only; all logs go to stderr.
- Config precedence: CLI flags > environment variables > `./agently-cli-mcp.toml` > built-in defaults.
- Defaults: transport `stdio`, confirm `elicit`, watch `30s` (max `120s`), max body `50000` chars, HTTP `127.0.0.1:8787`, max concurrency `1`.
- Write operations are never auto-retried. Confirmation tokens are never logged in full.
- `send/reply/forward` bodies always go through `--body-file`.
- `trash/delete` never receive `--confirmed`.
- All tool results must be JSON envelopes `{ok, tool, data|error, meta}` in `structured_content`.
- Every read result must carry `meta.untrusted_content` and a fenced security notice.
- `--read-only` disables `mail_send_message`, `mail_reply_message`, `mail_forward_message`, `mail_move_to_trash`, `mail_delete_messages`, `mail_refresh_auth`.

## Review Focus

1. **Windows npm shim**: a path like `C:\Program Files\nodejs\agently-cli.cmd` with spaces, quotes, or `%` in an argument must not be split or expanded. Test the `.cmd` quoting function and the shim fallback.
2. **Large/multiline body on Windows**: a 100KB body with newlines, quotes, and `%PATH%` must arrive intact; the 32K `cmd.exe` limit must not be hit because bodies use `--body-file`.
3. **Orphan processes**: timing out `watch` or a normal call must kill the real CLI, not just the npm shim. Test a grandchild process.
4. **Prompt injection**: subject/body/snippet containing “ignore previous instructions”, closing fences, and links must remain data and be fenced; the server must never fetch links.
5. **Concurrent CLI calls**: two simultaneous calls must not race credential refresh; default serialization must hold, while watch must not deadlock the rest of the server.

---

### Task 1: Cargo scaffold, config merge, logging

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`
- Create: `src/lib.rs`, `src/main.rs`, `src/cli.rs`, `src/config.rs`, `src/error.rs`, `src/logging.rs`
- Create: `tests/config_precedence.rs`
- Create: `agently-cli-mcp.example.toml`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `config::Config` with `transport`, `cli`, `confirm`, `search`, `limits`, `logging`, `tools`, `http`.
  - `config::CliArgs` (clap `Parser`).
  - `config::load(cli: CliArgs) -> Result<Config, ConfigError>`.
  - `config::load_with_sources(cli: CliArgs, env: &HashMap<String, String>, cwd: &Path) -> Result<Config, ConfigError>`.
  - `config::Config::redacted(&self) -> serde_json::Value`.
  - `error::AppError` and `error::Result<T>`.
  - `logging::init(&LoggingConfig) -> Result<(), AppError>`.

- [ ] **Step 1: Write the failing config tests**

`tests/config_precedence.rs`:

```rust
use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config::{load_with_sources, ConfirmMode, Transport};
use clap::Parser;
use std::collections::HashMap;
use std::fs;
use tempfile::tempdir;

fn args(argv: &[&str]) -> CliArgs {
    CliArgs::try_parse_from(std::iter::once("agently-cli-mcp").chain(argv.iter().copied())).unwrap()
}

#[test]
fn defaults_are_stable() {
    let dir = tempdir().unwrap();
    let cfg = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap();
    assert!(matches!(cfg.transport, Transport::Stdio));
    assert!(matches!(cfg.confirm.mode, ConfirmMode::Elicit));
    assert_eq!(cfg.cli.watch_max_secs, 120);
    assert_eq!(cfg.limits.max_body_chars, 50_000);
    assert_eq!(cfg.cli.max_concurrency, 1);
}

#[test]
fn flag_beats_env_beats_file() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("agently-cli-mcp.toml"),
        "transport = \"http\"\n[logging]\nlevel = \"debug\"\n",
    )
    .unwrap();
    let mut env = HashMap::new();
    env.insert("AGENTLY_MCP_LOG_LEVEL".into(), "warn".into());
    let cfg = load_with_sources(args(&["--log-level", "trace"]), &env, dir.path()).unwrap();
    assert_eq!(cfg.logging.level, "trace");
}

#[test]
fn config_file_is_read_from_cwd() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("agently-cli-mcp.toml"),
        "transport = \"http\"\n[http]\nlisten = \"0.0.0.0:9000\"\n",
    )
    .unwrap();
    let cfg = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap();
    assert!(matches!(cfg.transport, Transport::Http));
    assert_eq!(cfg.http.listen.port(), 9000);
}

#[test]
fn unknown_field_is_rejected() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("agently-cli-mcp.toml"), "nope = 1\n").unwrap();
    let err = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap_err();
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn missing_default_file_is_ok_but_explicit_missing_errors() {
    let dir = tempdir().unwrap();
    assert!(load_with_sources(args(&[]), &HashMap::new(), dir.path()).is_ok());
    assert!(load_with_sources(args(&["--config", "missing.toml"]), &HashMap::new(), dir.path()).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test config_precedence`
Expected: FAIL — crate/modules do not exist.

- [ ] **Step 3: Create the Cargo project and config types**

`Cargo.toml` must include:

```toml
[package]
name = "agently-cli-mcp"
version = "0.1.0"
edition = "2024"
rust-version = "1.88"
license = "Apache-2.0"

[[bin]]
name = "agently-cli-mcp"
path = "src/main.rs"

[lib]
name = "agently_cli_mcp"
path = "src/lib.rs"

[features]
default = []
test-fixtures = []

[[bin]]
name = "fake-agently-cli"
path = "tests/fixtures/fake_cli.rs"
required-features = ["test-fixtures"]

[dependencies]
rmcp = { version = "=3.5.1", features = ["server", "macros", "schemars", "transport-io", "transport-streamable-http-server", "elicitation", "request-state"] }
tokio = { version = "1.54", features = ["rt-multi-thread", "macros", "process", "io-std", "io-util", "signal", "time", "sync", "fs", "net"] }
axum = "0.8"
clap = { version = "4.5", features = ["derive", "env"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
schemars = "1.2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
tempfile = "3"
sha2 = "0.10"
tower = "0.5"
subtle = "2"

[target.'cfg(unix)'.dependencies]
nix = { version = "0.30", features = ["signal", "process"] }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.60", features = ["Win32_Foundation", "Win32_System_JobObjects", "Win32_System_Threading"] }

[dev-dependencies]
rmcp = { version = "=3.5.1", features = ["client", "transport-streamable-http-client-reqwest"] }
tokio = { version = "1.54", features = ["full"] }
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

`src/config.rs` defines the structs listed in Interfaces. Use `#[serde(deny_unknown_fields)]` on every file-side struct. `Config::load_with_sources` resolves the file at `cwd/agently-cli-mcp.toml` unless `--config` is set or `--no-config` is passed. Unknown env values return `ConfigError::Invalid`.

`src/cli.rs` uses `#[derive(Parser)]` and exposes every flag from spec §13, all as `Option<T>` (lists as `Vec<String>` empty = unset).

- [ ] **Step 4: Implement logging and the binary entry point**

`src/logging.rs` initializes `tracing_subscriber` with a stderr writer. `src/main.rs` parses `CliArgs`, loads `Config`, initializes logging, prints redacted config for `--print-config`, and exits 0. Wire `--version` through clap.

- [ ] **Step 5: Run the config tests**

Run: `cargo test --test config_precedence`
Expected: PASS (5 tests).

- [ ] **Step 6: Add a CLI smoke test**

`tests/cli_smoke.rs` asserts `--print-config` outputs JSON with `"transport": "stdio"` and does not print the HTTP token value. Run `cargo test --test cli_smoke`.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore src tests/config_precedence.rs tests/cli_smoke.rs agently-cli-mcp.example.toml
git commit -m "feat: scaffold Rust MCP server with config and logging"
```

---

### Task 2: CLI locate, runner, status classification, fake CLI

**Files:**
- Create: `src/cli_proc/mod.rs`, `src/cli_proc/locate.rs`, `src/cli_proc/locate_windows.rs`, `src/cli_proc/runner.rs`, `src/cli_proc/status.rs`
- Create: `tests/fixtures/fake_cli.rs`
- Create: `tests/cli_status.rs`, `tests/cli_runner.rs`
- Modify: `src/lib.rs` (export `cli_proc`)

**Interfaces:**
- Consumes: `config::CliConfig`, `error::AppError`.
- Produces:
  - `locate::find(cfg: &CliConfig) -> Result<PathBuf, AppError>`.
  - `Runner::new(cfg: &CliConfig) -> Result<Runner, AppError>`.
  - `Runner::run(&self, args: &[String], timeout: Duration) -> Result<CliResult, AppError>`.
  - `Runner::run_json(&self, args: &[String], timeout: Duration) -> Result<CliOutcome, AppError>`.
  - `status::classify(stdout: &[u8], stderr: &str, exit_code: Option<i32>) -> Result<CliOutcome, AppError>`.
  - `CliResult { stdout: Vec<u8>, stderr: String, exit_code: Option<i32> }`.
  - `CliOutcome::{Success(Value), ConfirmationRequired { token, summary } }`; all failures are returned as `Err(AppError)`.

- [ ] **Step 1: Write failing status tests**

`tests/cli_status.rs`:

```rust
use agently_cli_mcp::cli_proc::status::classify;
use agently_cli_mcp::cli_proc::CliOutcome;

#[test]
fn confirmation_is_detected_before_exit_code() {
    let out = br#"{"ok":true,"data":{"confirmation_required":true,"confirmation_token":"ctk_1","summary":{"action":"send"}}}"#;
    match classify(out, "", Some(0)).unwrap() {
        CliOutcome::ConfirmationRequired { token, .. } => assert_eq!(token, "ctk_1"),
        _ => panic!("expected confirmation"),
    }
}

#[test]
fn ok_false_with_exit_zero_is_an_error() {
    let out = br#"{"ok":false,"error":{"type":"api_error","code":404,"message":"Resource not found"}}"#;
    let err = classify(out, "", Some(0)).unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn exit_one_plain_stderr_is_invalid_argument() {
    let err = classify(b"", "--limit must be between 1 and 50\n", Some(1)).unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.to_string().contains("--limit"));
}

#[test]
fn non_json_is_protocol_error_without_echoing_stdout() {
    let err = classify(b"secret body", "boom", Some(0)).unwrap_err();
    assert_eq!(err.code(), "PROTOCOL_ERROR");
    assert!(!format!("{err}").contains("secret body"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test cli_status`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `status::classify` and `AppError` mapping**

Implement the decision order from spec §8.2. `AppError::code()` returns `CLI_NOT_FOUND`, `INVALID_ARGUMENT`, `AUTH_REQUIRED`, `NETWORK_ERROR`, `NOT_FOUND`, `RATE_LIMITED`, `REJECTED`, `PROTOCOL_ERROR`, `TIMEOUT`, `CONFIRMATION_EXPIRED`, `TOOL_DISABLED`, `PATH_OUTSIDE_WORKDIR`, `CLI_ERROR`. `retryable()` is true only for `RATE_LIMITED` and `NETWORK_ERROR`.

- [ ] **Step 4: Implement the fake CLI**

`tests/fixtures/fake_cli.rs` reads `FAKE_SCENARIO`, writes `std::env::args().skip(1)` as one JSON line to `FAKE_ARGV_LOG`, then:

- `success_list`: prints a list envelope, exit 0.
- `confirm_send`: prints the confirmation envelope, exit 0.
- `confirm_then_success`: if argv contains `--confirmation-token`, prints `{"ok":true,"data":{"queued":true}}`, exit 0; otherwise prints the confirmation envelope with `ctk_test`.
- `error_404`: prints `{ok:false,error:{type:"api_error",code:404,...}}`, exit 6.
- `plain_error`: writes `--limit must be between 1 and 50` to stderr, exit 1.
- `non_json`: prints `not json`, exit 0.
- `sleep`: sleeps 30 seconds.
- `spawn_child`: prints the child PID and sleeps 30 seconds (used in Task 3).
- `ndjson_events`: prints two JSON lines and sleeps 30 seconds (used in Task 10).

- [ ] **Step 5: Implement `Runner::run` with timeout and env isolation**

Use `tokio::process::Command`, `kill_on_drop(true)`, and `tokio::time::timeout`. Map timeout to `AppError::Timeout`. Do not add `--confirmed` anywhere. `run_json` calls `classify`.

- [ ] **Step 6: Write and run runner tests**

`tests/cli_runner.rs` builds the fake CLI path via `env!("CARGO_BIN_EXE_fake-agently-cli")`, sets `FAKE_SCENARIO`, and asserts argv capture, success JSON, exit 1 mapping, and timeout. Run:

`cargo test --features test-fixtures --test cli_runner --test cli_status`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cli_proc src/lib.rs tests/fixtures tests/cli_runner.rs tests/cli_status.rs Cargo.toml
git commit -m "feat: add CLI runner, status classification, and fake CLI"
```

---

### Task 3: Process-tree termination and body temp files

**Files:**
- Create: `src/cli_proc/process_group_unix.rs`, `src/cli_proc/process_group_windows.rs`
- Create: `src/cli_proc/bodyfile.rs`
- Create: `tests/process_tree.rs`, `tests/bodyfile.rs`
- Modify: `src/cli_proc/runner.rs`, `src/cli_proc/mod.rs`

**Interfaces:**
- Consumes: `Runner`, fake CLI scenarios.
- Produces:
  - `process::configure(cmd: &mut Command)` (Unix `setpgid`, Windows no-op + job attach after spawn).
  - `process::terminate(child: &mut Child)` (Unix `kill(-pid, SIGTERM)`, then `SIGKILL`; Windows `TerminateJobObject`).
  - `BodyFile::create(root: &Path, content: &[u8], format: BodyFormat) -> Result<BodyFile, AppError>`.
  - `BodyFile::relative_path(&self) -> &str`, `BodyFile::verify(&self) -> Result<(), AppError>`, `BodyFile::remove(self) -> io::Result<()>`.
  - `BodyFormat::{Plain, Html, Markdown}` with extensions `.txt`, `.html`, `.md`.

- [ ] **Step 1: Write the failing process-tree test**

`tests/process_tree.rs` (Unix):

```rust
#[tokio::test]
async fn timeout_kills_grandchild() {
    // FAKE_SCENARIO=spawn_child prints the grandchild PID; run with a 300ms timeout.
    // Assert the grandchild PID no longer exists with kill(pid, 0) == ESRCH.
}
```

Also add a Windows-gated test that asserts the Job Object handle kills the tree.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --features test-fixtures --test process_tree`
Expected: FAIL — grandchild survives.

- [ ] **Step 3: Implement process groups and Job Objects**

Unix: `pre_exec`/`CommandExt::process_group(0)`, terminate with `nix::sys::signal::kill(Pid::from_raw(-pid), SIGTERM)`, wait 2 seconds, then `SIGKILL`.

Windows: create a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, assign the child, and close the job on timeout/drop. Do not use `cmd /c taskkill`.

- [ ] **Step 4: Write the failing body-file tests**

`tests/bodyfile.rs` asserts extension by format, `0600` permissions on Unix, content hash verification, and removal.

- [ ] **Step 5: Implement `BodyFile`**

Create files under `<workdir>/.agently-cli-mcp/tmp/` with a UUID name. On Unix set mode `0600`. `verify` recomputes SHA-256 and compares.

- [ ] **Step 6: Run both tests**

Run: `cargo test --features test-fixtures --test process_tree --test bodyfile`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cli_proc tests/process_tree.rs tests/bodyfile.rs
git commit -m "feat: kill full process tree and manage body temp files"
```

---

### Task 4: Path sandbox, redaction, search context

**Files:**
- Create: `src/guard/mod.rs`, `src/guard/paths.rs`, `src/guard/redact.rs`
- Create: `src/search/mod.rs`, `src/search/store.rs`
- Create: `tests/guard_paths.rs`, `tests/redact.rs`, `tests/search_store.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `AppError`.
- Produces:
  - `Sandbox::new(root: PathBuf) -> Result<Sandbox, AppError>`.
  - `Sandbox::resolve_input(&self, p: &Path) -> Result<PathBuf, AppError>`.
  - `Sandbox::resolve_output_dir(&self, p: &Path) -> Result<PathBuf, AppError>`.
  - `Sandbox::relative(&self, p: &Path) -> Result<PathBuf, AppError>`.
  - `AttachmentLimits { max_files: usize, max_file_bytes: u64, max_total_bytes: u64 }` (defaults 50, 20 MiB, 20 MiB).
  - `Sandbox::validate_attachments(&self, paths: &[PathBuf], limits: &AttachmentLimits) -> Result<Vec<PathBuf>, AppError>`.
  - `redact::token(s: &str) -> String`, `redact::text(s: &str) -> String`.
  - `SearchStore::remember(&self, cursor: &str, ctx: SearchCtx, session: &str)`.
  - `SearchQuery { q, search_in, from, to, dir, after, before, has_attachments, is_unread, limit }`.
  - `SearchStore::resolve(&self, cursor: &str, provided: &SearchQuery, session: &str) -> Result<SearchQuery, AppError>`.

- [ ] **Step 1: Write failing sandbox tests**

```rust
#[test]
fn rejects_parent_escape() { /* ../secret -> PATH_OUTSIDE_WORKDIR */ }

#[test]
fn rejects_symlink_escape() { /* symlink to /tmp/outside -> PATH_OUTSIDE_WORKDIR */ }

#[test]
fn accepts_absolute_path_inside_root() { /* returns relative path */ }

#[test]
fn rejects_oversized_attachment() { /* 21MB -> INVALID_ARGUMENT before spawn */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test guard_paths`
Expected: FAIL.

- [ ] **Step 3: Implement the sandbox**

Use `canonicalize` for existing paths and a lexical prefix check for output paths that do not exist yet. Convert inside-root absolute paths to relative strings for CLI arguments. Reject non-regular files and over-limit attachments.

- [ ] **Step 4: Write and run redaction tests**

`tests/redact.rs` asserts `ctk_1234567890` becomes `ctk_12345678***`, bearer tokens are masked, and body text is not logged. Run `cargo test --test redact`.

- [ ] **Step 5: Write failing search-store tests**

```rust
#[test]
fn cursor_roundtrip_restores_query() { /* remember then resolve with only cursor -> same q/from */ }

#[test]
fn changed_filter_mismatches() { /* q changed -> SEARCH_CONTEXT_MISMATCH */ }

#[test]
fn expired_context_requires_query() { /* advance clock -> SEARCH_CONTEXT_REQUIRED */ }
```

- [ ] **Step 6: Implement `SearchStore` with TTL + LRU**

Use `tokio::sync::Mutex` or `std::sync::Mutex` with `Instant`; default TTL 600s, capacity 200, session-bound. Run `cargo test --test search_store`.

- [ ] **Step 7: Commit**

```bash
git add src/guard src/search tests/guard_paths.rs tests/redact.rs tests/search_store.rs src/lib.rs
git commit -m "feat: add path sandbox, redaction, and search context store"
```

---

### Task 5: Confirmation store and summary builder

**Files:**
- Create: `src/confirm/mod.rs`, `src/confirm/store.rs`, `src/confirm/summary.rs`
- Create: `tests/confirm_store.rs`, `tests/summary.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `AppError`, `redact`.
- Produces:
  - `Pending { tool, args_hash, canonical_args, ctk, session_id, expires_at, body_temp }`.
  - `ConfirmStore::insert(&self, key: String, pending: Pending)`.
  - `ConfirmStore::take(&self, key: &str, session: &str) -> Result<Pending, AppError>`.
  - `ConfirmStore::remove(&self, key: &str)`.
  - `ConfirmStore::cleanup_expired(&self) -> Vec<PathBuf>`.
  - `summary::build(input: SummaryInput) -> Value`.
  - `SummaryInput { action, from, to, cc, bcc, subject, body_preview, attachments: Vec<AttachmentMeta>, source: Option<SourceMessage>, cli: Value }`.
  - `AttachmentMeta { filename: String, size: u64 }`; `SourceMessage { id: String, subject: Option<String>, from: Option<String> }`.
  - `SummaryAction::{Send, Reply, Forward, Trash, Delete, DeleteAll}`.

- [ ] **Step 1: Write failing store tests**

```rust
#[test]
fn take_is_single_use() { /* second take -> CONFIRMATION_EXPIRED */ }

#[test]
fn session_mismatch_is_rejected() { /* other session -> CONFIRMATION_EXPIRED */ }

#[test]
fn expired_pending_is_rejected() { /* ttl 0 -> CONFIRMATION_EXPIRED */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test confirm_store`
Expected: FAIL.

- [ ] **Step 3: Implement the store**

Use `Mutex<HashMap<String, Pending>>` plus a monotonic counter for LRU (capacity 100). `take` removes the entry atomically and returns `CONFIRMATION_EXPIRED` for missing, expired, or wrong-session entries.

- [ ] **Step 4: Write failing summary tests**

`tests/summary.rs` asserts:

- send with `to/cc/bcc` keeps them separate and never merges them like the CLI summary;
- attachments include filename and size;
- body preview is truncated to 200 chars;
- `DeleteAll` includes an explicit “数量未知” warning;
- `reply` includes `source_message` when provided.

- [ ] **Step 5: Implement `build`**

`build` must be deterministic: iterate recipients in input order, stat attachments without reading contents, and place the CLI-provided summary under `summary.cli`.

- [ ] **Step 6: Run tests**

Run: `cargo test --test confirm_store --test summary`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/confirm tests/confirm_store.rs tests/summary.rs src/lib.rs
git commit -m "feat: add confirmation store and deterministic summary builder"
```

---

### Task 6: MCP server skeleton and read-only tools

**Files:**
- Create: `src/mcp/mod.rs`, `src/mcp/server.rs`, `src/mcp/stdio.rs`
- Create: `src/tools/mod.rs`, `src/tools/account.rs`, `src/tools/read.rs`, `src/tools/attachments.rs`
- Create: `tests/read_tools.rs`
- Modify: `src/main.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: `Runner`, `Sandbox`, `ConfirmStore`, `SearchStore`, `Config`.
- Produces:
  - `AppState { cfg, runner, sandbox, confirm, search, session_id, limits: Arc<RwLock<AttachmentLimits>> }`.
  - `build_server(state: Arc<AppState>) -> MailServer`.
  - `MailServer` implements `ServerHandler`; tool names exactly `mail_get_account`, `mail_refresh_auth`, `mail_list_messages`, `mail_get_message`, `mail_search_messages`, `mail_download_attachment`.
  - `result::ok(tool, data, meta) -> CallToolResult`, `result::err(tool, &AppError) -> CallToolResult`.

- [ ] **Step 1: Write the failing in-memory tool tests**

`tests/read_tools.rs` uses `tokio::io::duplex(8192)` + `rmcp::service::serve_directly` to connect a client to `build_server`. Tests:

```rust
#[tokio::test]
async fn list_returns_messages_and_fences_snippets() { /* call mail_list_messages; assert structured_content.data.data[0].message_id and meta.untrusted_content == true; assert text contains BEGIN UNTRUSTED */ }

#[tokio::test]
async fn get_marks_read_side_effect() { /* assert meta.side_effects contains "marked_read" */ }

#[tokio::test]
async fn search_uses_cursor_context() { /* first search returns next_cursor; second with only cursor returns same page */ }

#[tokio::test]
async fn download_rejects_outside_path() { /* output=/tmp -> PATH_OUTSIDE_WORKDIR */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features test-fixtures --test read_tools`
Expected: FAIL.

- [ ] **Step 3: Implement `MailServer` with `#[tool_router]`**

Use `#[tool(name = "mail_get_account", annotations(title = "...", read_only_hint = true))]`. Read handlers return `Result<CallToolResponse, ErrorData>` and convert errors via `result::err`.

- [ ] **Step 4: Implement account/list/get/search/download arg mapping**

Map snake_case fields to CLI flags exactly:
`--dir`, `--limit`, `--cursor`, `--after`, `--before`, `--has-attachments`, `--is-unread`, `--q`, `--search-in`, `--from`, `--to`, `--id`, `--msg`, `--att`, `--output`.

For `get`, truncate `data.body` to `max_body_chars` and set `meta.body_truncated`. For `search`, remember the cursor context. On the first account/list call, run `+me` and update `AppState.limits` from `constraints`; fall back to the 50-file / 20 MiB defaults if `+me` fails.

- [ ] **Step 5: Implement untrusted-content fencing**

Prefix text content with the fixed `SECURITY NOTICE` and wrap the JSON payload in `-----BEGIN UNTRUSTED EMAIL DATA-----` / `-----END UNTRUSTED EMAIL DATA-----`. Set `meta.untrusted_content = true` for list/get/search/watch results.

- [ ] **Step 6: Wire stdio in `main.rs`**

If `transport == stdio`, `build_server(...).serve(rmcp::transport::stdio()).await`. Ensure no logging writes to stdout.

- [ ] **Step 7: Run tests**

Run: `cargo test --features test-fixtures --test read_tools`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/mcp src/tools src/main.rs src/lib.rs tests/read_tools.rs
git commit -m "feat: add MCP server skeleton and read-only mail tools"
```

---

### Task 7: Confirmation flow and write tools

**Files:**
- Create: `src/confirm/mrtr.rs`
- Create: `src/tools/write.rs`
- Create: `tests/confirm_flow.rs`, `tests/write_tools.rs`
- Modify: `src/mcp/server.rs`, `src/tools/mod.rs`

**Interfaces:**
- Consumes: `ConfirmStore`, `summary::build`, `Runner`, `BodyFile`.
- Produces:
  - `ConfirmResponse { confirm: bool }` with `elicit_safe!`.
  - `confirm::mrtr::begin(state, tool, args, summary, body_temp, session) -> Pending`.
  - `confirm::mrtr::finish(state, pending, confirm) -> Result<Value, AppError>`.
  - Tool names `mail_send_message`, `mail_reply_message`, `mail_forward_message`, `mail_move_to_trash`, `mail_delete_messages`.
  - `WriteArgs` structs with `confirmation_token: Option<String>`.

- [ ] **Step 1: Write failing confirmation tests**

`tests/confirm_flow.rs` uses a `ClientConfig` explicitly negotiated to `ProtocolVersion::V_2026_07_28` for the MRTR test and `V_2025_11_25` for the legacy test, with an `ElicitationHandler` returning `accept` or `decline`:

```rust
#[tokio::test]
async fn new_protocol_returns_input_required_then_accept_sends() {
    // Client negotiates 2026-07-28 with an elicitation handler returning accept.
    // First tool result is InputRequired; after retry, assert the fake CLI saw a ctk.
}

#[tokio::test]
async fn legacy_protocol_uses_peer_elicit_and_decline_sends_nothing() {
    // Client negotiates 2025-11-25; decline; assert no --confirmation-token call.
}

#[tokio::test]
async fn token_fallback_returns_ctk_and_second_call_uses_cached_args() {
    // Client without elicitation; first call returns confirmation_required;
    // second call with a mutated subject still uses cached args and reports args_changed.
}

#[tokio::test]
async fn expired_pending_is_rejected() { /* ttl 0 -> CONFIRMATION_EXPIRED, no second CLI */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features test-fixtures --test confirm_flow`
Expected: FAIL.

- [ ] **Step 3: Implement the protocol branch**

In the write handler:

- If `confirmation_token` is present: `ConfirmStore::take(token, session)`, run with cached args.
- Else if `RequestState` is present: `ConfirmStore::take(state, session)`, read `InputResponses["confirm"]`, run or decline.
- Else: run the CLI unconfirmed, build the pending record, then:
  - protocol ≥ `2026-07-28`: return `InputRequiredResult::new(Some(requests), Some(pending_id))`.
  - protocol < `2026-07-28`: call `context.peer.elicit_with_timeout::<ConfirmResponse>(...)`; on accept run, on decline return `status:"declined"`, on `CapabilityNotSupported` return token mode.
  - `never`: pass `--confirmed` for send/reply/forward; trash/delete fall back to token.

- [ ] **Step 4: Implement write tool arg mapping**

- `send`: `--to`, `--subject`, `--cc`, `--bcc`, `--attachment`, `--body-file`, `--body-format`.
- `reply`: `--id`, `--reply-all`, `--cc`, `--bcc`, `--attachment`.
- `forward`: `--id`, `--to`, `--body-file`, `--cc`, `--bcc`, `--include-attachments`, `--attachment`.
- `trash`: `--id`.
- `delete`: `--id` or `--all` (exactly one).

Always validate recipient counts and attachment limits before the first CLI call.

- [ ] **Step 5: Implement body-file retention**

Write the body with `BodyFile::create`, pass `--body-file <relative>`, and store the path in `Pending`. Delete on decline/expiry/success. In `never`/no-confirm paths, delete immediately after the call.

- [ ] **Step 6: Write and run write-tool tests**

`tests/write_tools.rs` asserts:

- every send uses `--body-file`, never `--body` (except empty-body validation errors);
- no `--confirmed` appears in elicit/token modes;
- `trash/delete` never receive `--confirmed` even in `never` mode;
- declined reply/forward reports `source_marked_read` when the summary pre-read ran;
- successful send returns a queued/success envelope.

Run: `cargo test --features test-fixtures --test confirm_flow --test write_tools`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/confirm src/tools/write.rs src/mcp/server.rs tests/confirm_flow.rs tests/write_tools.rs
git commit -m "feat: add MRTR/legacy confirmation and write tools"
```

---

### Task 8: Streamable HTTP transport

**Files:**
- Create: `src/mcp/http.rs`
- Create: `tests/http_transport.rs`
- Modify: `src/main.rs`, `src/mcp/mod.rs`

**Interfaces:**
- Consumes: `build_server`, `Config.http`.
- Produces:
  - `http::serve(state: Arc<AppState>, cfg: &HttpConfig, shutdown: CancellationToken) -> Result<(), AppError>`.
  - `GET /healthz` → `{"ok":true,"version":...}`.
  - `POST/GET/DELETE /mcp` → `StreamableHttpService`.
  - Bearer auth middleware and Host allowlist.

- [ ] **Step 1: Write failing HTTP tests**

`tests/http_transport.rs`:

```rust
#[tokio::test]
async fn healthz_is_public() { /* 200, no auth */ }

#[tokio::test]
async fn mcp_requires_bearer_token_when_configured() { /* 401 without, 200 with */ }

#[tokio::test]
async fn non_loopback_requires_token_at_startup() { /* config validation error */ }

#[tokio::test]
async fn disallowed_host_is_rejected() { /* Host: evil.example -> 403 */ }

#[tokio::test]
async fn tool_call_over_http_works() { /* connect with rmcp client to /mcp, call mail_list_messages */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features test-fixtures --test http_transport`
Expected: FAIL.

- [ ] **Step 3: Implement the HTTP stack**

Use `axum::Router` with `nest_service("/mcp", StreamableHttpService::new(factory, LocalSessionManager::default(), config))`. The factory creates a new `MailServer` with a fresh UUID `session_id` and the shared `Arc<AppState>`.

- [ ] **Step 4: Implement auth and Host validation**

Bearer token from `--http-token` or file; compare with `subtle::ConstantTimeEq`. Configure `StreamableHttpServerConfig.allowed_hosts` from `--http-allowed-host`, defaulting to loopback. Refuse to start if `listen` is non-loopback and no token is configured.

- [ ] **Step 5: Implement graceful shutdown**

Listen for `tokio::signal::ctrl_c` and `SIGTERM`; cancel the token and close the server.

- [ ] **Step 6: Run tests**

Run: `cargo test --features test-fixtures --test http_transport`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/mcp/http.rs src/mcp/mod.rs src/main.rs tests/http_transport.rs
git commit -m "feat: add Streamable HTTP transport with auth and host validation"
```

---

### Task 9: Complete flags, env, config, and tool filtering

**Files:**
- Modify: `src/cli.rs`, `src/config.rs`, `src/mcp/server.rs`, `src/main.rs`
- Create: `tests/config_full.rs`, `tests/tool_filtering.rs`
- Modify: `agently-cli-mcp.example.toml`

**Interfaces:**
- Consumes: all previous modules.
- Produces:
  - Every flag/env in spec §13.
  - `Config::validate() -> Result<(), ConfigError>`.
  - `tools::enabled(cfg: &ToolsConfig) -> Vec<&'static str>`.
  - `--print-config` JSON (secrets redacted).
  - `--check` that runs `agently-cli --version` and `auth status`.

- [ ] **Step 1: Write failing precedence and validation tests**

`tests/config_full.rs` covers HTTP token file, non-loopback without token, `--no-config`, explicit missing config, `read_only` + `allow` conflict, and `--watch-max-secs` clamping.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test config_full`
Expected: FAIL.

- [ ] **Step 3: Implement the full flag surface and validation**

Add all flags from spec §13. Validate:

- non-loopback + no token → error at startup;
- `allow` and `disabled` both non-empty → error;
- `watch_max_secs` in 5..=120;
- `max_body_chars` > 0;
- `max_concurrency` >= 1.

- [ ] **Step 4: Implement tool filtering**

`tools::enabled` returns the 12 tool names minus disabled/read-only restrictions. `build_server` only registers enabled tools. Calling a disabled tool returns `TOOL_DISABLED`.

- [ ] **Step 5: Write and run filtering tests**

`tests/tool_filtering.rs` asserts `tools/list` omits `mail_send_message` under `--read-only`, omits `--disable-tool mail_delete_messages`, and rejects direct calls with `TOOL_DISABLED`. Run `cargo test --test tool_filtering`.

- [ ] **Step 6: Update the example config and verify `--print-config`**

Ensure `agently-cli-mcp.example.toml` matches `Config` exactly and `--print-config` output contains no token value.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/config.rs src/mcp/server.rs src/main.rs tests/config_full.rs tests/tool_filtering.rs agently-cli-mcp.example.toml
git commit -m "feat: complete CLI/config surface and tool filtering"
```

---

### Task 10: Watch tool with explicit blocking contract

**Files:**
- Modify: `src/tools/read.rs`, `src/mcp/server.rs`, `src/cli_proc/runner.rs`
- Create: `tests/watch_tool.rs`

**Interfaces:**
- Consumes: `Runner`, `Config.limits`, process-tree termination.
- Produces:
  - `mail_watch_messages` tool with `timeout_seconds: Option<u64>` and `msg_format: Option<WatchFormat>`.
  - A process-wide `watch_lock: Arc<tokio::sync::Mutex<()>>`; second concurrent call returns `WATCH_BUSY`.
  - Result data `{ events, count, timed_out, elapsed_ms }`.

- [ ] **Step 1: Write failing watch tests**

`tests/watch_tool.rs`:

```rust
#[tokio::test]
async fn watch_blocks_until_timeout_and_returns_empty_events() { /* timeout=1s; assert timed_out true, count 0, elapsed >= 1s */ }

#[tokio::test]
async fn watch_parses_ndjson_events() { /* fake CLI prints two lines; assert count 2 */ }

#[tokio::test]
async fn second_watch_returns_watch_busy() { /* start one, call another -> WATCH_BUSY */ }

#[tokio::test]
async fn watch_timeout_kills_cli_tree() { /* fake CLI spawns grandchild; assert gone */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features test-fixtures --test watch_tool`
Expected: FAIL.

- [ ] **Step 3: Implement bounded watch**

Spawn the CLI, read stdout line by line, and stop at `timeout_seconds` or when the process exits. Kill the process group/Job Object on timeout. Return events as an array of JSON values; set `meta.untrusted_content = !events.is_empty()` and apply the same fencing as other read tools.

- [ ] **Step 4: Add the exact tool description**

Use the text from spec §5.1, including “阻塞式”, the default 30s, max 120s, and “需再次调用”. Set `read_only_hint = true`.

- [ ] **Step 5: Run tests**

Run: `cargo test --features test-fixtures --test watch_tool`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tools/read.rs src/mcp/server.rs src/cli_proc/runner.rs tests/watch_tool.rs
git commit -m "feat: add bounded watch tool with blocking contract"
```

---

### Task 11: Release pipeline, README, and live smoke

**Files:**
- Create: `.github/workflows/release.yml`, `Makefile`, `README.md`
- Create: `tests/live_smoke.rs`
- Modify: `Cargo.toml` (repository metadata)

**Interfaces:**
- Consumes: the complete server.
- Produces:
  - 5 release artifacts with checksums.
  - README covering install, MCP config, HTTP mode, confirmation, config file, unsigned binary workarounds, and watch caveat.
  - `AGENTLY_MCP_LIVE=1` read-only smoke tests.

- [ ] **Step 1: Add the live smoke test**

`tests/live_smoke.rs` is `#[ignore]` unless `AGENTLY_MCP_LIVE=1`. It calls `mail_get_account` and `mail_list_messages --limit 1` against the real CLI. It never sends or deletes.

- [ ] **Step 2: Run the full local verification**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --features test-fixtures -- -D warnings
cargo test --features test-fixtures
AGENTLY_MCP_LIVE=1 cargo test --features test-fixtures --test live_smoke -- --ignored
cargo build --release
```

Expected: all pass; `target/release/agently-cli-mcp --version` prints `0.1.0`.

- [ ] **Step 3: Add the release workflow**

Matrix:

| runner | target |
|---|---|
| `ubuntu-24.04` | `x86_64-unknown-linux-musl` |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` |
| `macos-13` | `x86_64-apple-darwin` |
| `macos-14` | `aarch64-apple-darwin` |
| `windows-latest` | `x86_64-pc-windows-msvc` |

Each job runs `cargo build --release --target ...`, strips, archives, and uploads. A final job generates `checksums.txt` and creates the GitHub Release. Do not sign.

- [ ] **Step 4: Write the README**

Include:

- `cargo install` and release download instructions;
- stdio and HTTP MCP client config examples;
- `agently-cli auth login` prerequisite;
- confirmation modes and the fact that `token` mode lets the model self-confirm;
- `--read-only` semantics and `mail_get_message` read side effect;
- config file location `./agently-cli-mcp.toml` and precedence;
- watch blocking warning;
- unsigned binary instructions for macOS (`xattr -d com.apple.quarantine`) and Windows SmartScreen;
- prompt-injection warning.

- [ ] **Step 5: Verify the release workflow locally**

Run `make dist` (current target only) and confirm the artifact name, checksum, and `--version` output. CI performs the 5-target build.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml Makefile README.md tests/live_smoke.rs Cargo.toml
git commit -m "chore: add release pipeline, docs, and live smoke test"
```

---

## Self-Review Checklist

- **Spec coverage:** every section of the design maps to a task: config/flags (1, 9), CLI adapter (2, 3), sandbox/search (4), confirmation (5, 7), tools (6, 7, 10), HTTP (8), release (11).
- **Type consistency:** `AppState` fields used by all tools match Task 6; `Pending` fields match Tasks 5 and 7; `SearchCtx` matches Task 4; `BodyFile` matches Tasks 3 and 7.
- **Review Focus tests:** Windows shim (Task 2/3), large body (Task 7), orphan process (Task 3/10), prompt injection (Task 6), concurrency (Task 2/10).
- **No placeholder steps:** every task ends with a runnable command and a commit.
