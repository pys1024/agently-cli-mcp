# agently-cli-mcp

An [MCP](https://modelcontextprotocol.io) server that exposes the system's
`agently-cli` (Agent Mail) as model-callable tools. It wraps the npm-installed
CLI over stdio or Streamable HTTP, adding sandboxing, per-write human
confirmation, output redaction, and prompt-injection defenses around the raw
CLI.

The server itself is a single Rust binary. It never embeds, repackages, or
replaces `agently-cli`; it locates the CLI at runtime and shells out to it.

- Read-only tools: list / read / search / account / watch.
- Write tools: send / reply / forward / trash / delete / refresh-auth, each
  gated behind human confirmation (configurable).
- Two transports: stdio (local IDE/desktop clients) and Streamable HTTP
  (shared, remote, or containerized deployments).

---

## Prerequisites

1. **Install `agently-cli`** (the server shells out to it):

   ```bash
   npm install -g @tencent-qqmail/agently-cli
   ```

2. **Authorize it.** Every mail operation needs a logged-in account:

   ```bash
   agently-cli auth login
   ```

   Verify with `agently-cli auth status` (or run `agently-cli-mcp --check`).
   If the CLI is not authorized, read tools return an auth error and write
   tools will not send.

3. **Rust toolchain** (source builds only; see *Install* below).

## Install

### Download a release binary (recommended)

Each [GitHub Release](https://github.com/pys1024/agently-cli-mcp/releases)
publishes a single-binary archive per platform, plus `checksums.txt` and a
CycloneDX SBOM (`agently-cli-mcp-sbom.cdx.json`):

| Platform | Archive |
|---|---|
| Linux x86_64 (musl, static) | `agently-cli-mcp-x86_64-unknown-linux-musl.tar.gz` |
| Linux arm64 (musl, static) | `agently-cli-mcp-aarch64-unknown-linux-musl.tar.gz` |
| macOS x86_64 | `agently-cli-mcp-x86_64-apple-darwin.tar.gz` |
| macOS arm64 (Apple Silicon) | `agently-cli-mcp-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 (MSVC, static CRT) | `agently-cli-mcp-x86_64-pc-windows-msvc.zip` |

```bash
# Linux/macOS example
curl -LO https://github.com/pys1024/agently-cli-mcp/releases/latest/download/agently-cli-mcp-x86_64-unknown-linux-musl.tar.gz
tar -xzf agently-cli-mcp-x86_64-unknown-linux-musl.tar.gz
sudo install -m 755 agently-cli-mcp /usr/local/bin/agently-cli-mcp
```

Verify the download against `checksums.txt`:

```bash
sha256sum -c <(grep agently-cli-mcp-x86_64-unknown-linux-musl.tar.gz checksums.txt)
```

The binaries are **not code-signed**. See *Unsigned binary notes* below for the
macOS Gatekeeper and Windows SmartScreen workarounds.

### Install from source

```bash
cargo install --git https://github.com/pys1024/agently-cli-mcp.git
```

This builds the current platform's binary on your machine.

---

## Configure an MCP client

### stdio (local IDE / desktop clients)

Point the client at the binary. For example, in
`claude_desktop_config.json` (or your client's equivalent):

```json
{
  "mcpServers": {
    "agently-cli-mcp": {
      "command": "agently-cli-mcp",
      "args": ["--read-only"]
    }
  }
}
```

Remove `"--read-only"` (or switch the confirmation mode) when you want the
model to be able to send mail. The server speaks MCP over stdin/stdout; all
logging goes to stderr, so it never pollutes the protocol stream.

### HTTP (Streamable HTTP)

Start the server on a local port, then point your client at
`http://127.0.0.1:8787/mcp`:

```bash
agently-cli-mcp --transport http --listen 127.0.0.1:8787
```

Health check: `curl http://127.0.0.1:8787/healthz`.

> **Security warning:** HTTP mode has **no authentication by default**. If you
> bind to anything other than loopback without a token, any client that can
> reach the port can read and control your mail. For anything shared, require a
> bearer token and/or put it behind a trusted reverse proxy (see
> *Security* below).

---

## Prerequisite: authorization

The CLI must be logged in before any tool works:

```bash
agently-cli auth login
```

The server does not perform the OAuth login itself; it forwards the CLI's auth
status. `mail_get_account` reports it, and `mail_refresh_auth` refreshes stored
credentials. If the account is not logged in, read tools fail with an auth
error and send tools will not send.

---

## Confirmation modes

Write tools (send / reply / forward / trash / delete) are destructive or send
real mail, so they ask for human confirmation first. The mode is selected with
`--confirm` (or `AGENTLY_MCP_CONFIRM`, or `[confirm] mode` in the config file):

| Mode | Behavior |
|---|---|
| `elicit` (default) | The server prompts the human through MCP elicitation (modern clients) or a synchronous prompt, then executes only on explicit approval. |
| `token` | The server returns a one-time `confirmation_token` to the model. The model is expected to show the summary to the user and ask for permission before replaying the call with the token. |
| `never` | No confirmation. `send`/`reply`/`forward` run with `--confirmed`; `trash`/`delete` fall back to `token` mode because the CLI has no `--confirmed` flag for them. |

> **`token`-mode caveat:** in `token` mode the confirmation token is handed to
> the model, which means the model can replay it and **self-confirm** without
> actually asking a human. Treat `token` mode as "the model is trusted to ask",
> not as a hard human gate. For a hard gate, use `elicit`. Use `never` only for
> fully automated, trusted workloads.

---

## `--read-only` mode

`--read-only` (or `AGENTLY_MCP_READ_ONLY=1`) removes the write and
credential tools from the server. The following tools are disabled:

- `mail_send_message`, `mail_reply_message`, `mail_forward_message`
- `mail_move_to_trash`, `mail_delete_messages`
- `mail_refresh_auth`

Still available under `--read-only`:

- `mail_get_account`, `mail_list_messages`, `mail_get_message`,
  `mail_search_messages`, `mail_watch_messages`
- `mail_download_attachment` (writes a file into the sandbox only — it does not
  send or delete mail)

> **Read side effect:** `mail_get_message` (and reading a message while
> composing a reply/forward) **marks the message as read** on the server. This
> is a real side effect of a nominally read-only tool. `mail_get_message`'s
> result reports it in `meta.side_effects: ["marked_read"]`. If you must not
> disturb read state, avoid fetching a message body.

---

## Configuration file

The server reads an optional TOML config file from **`./config.toml`** (the
current working directory). Copy `config.example.toml` to `config.toml` to get
started.

Precedence (highest first):

1. CLI flags (e.g. `--transport http`)
2. Environment variables (e.g. `AGENTLY_MCP_TRANSPORT=http`)
3. `./config.toml`
4. Built-in defaults

Use `--config <path>` to load a different file, or `--no-config` to skip file
loading entirely. Run `agently-cli-mcp --print-config` to dump the merged,
effective configuration (secrets redacted), and `--check` to verify the CLI,
its version, auth status, and working directory.

Example `config.toml`:

```toml
transport = "stdio"

[cli]
# path = "/usr/local/bin/agently-cli"   # default: auto-detect on PATH
max_concurrency = 1
timeout_secs = 30

[confirm]
mode = "elicit"
ttl_secs = 300

[tools]
read_only = true
# allow = ["mail_list_messages", "mail_get_message"]

[logging]
level = "info"
format = "text"

[http]
listen = "127.0.0.1:8787"
allowed_hosts = ["localhost", "127.0.0.1", "::1"]
```

Every CLI flag has an `AGENTLY_MCP_*` environment-variable equivalent (for
example `--cli` ↔ `AGENTLY_CLI_PATH`, `--confirm` ↔ `AGENTLY_MCP_CONFIRM`,
`--read-only` ↔ `AGENTLY_MCP_READ_ONLY`). Secrets (the HTTP bearer token) are
intentionally **not** supported in the config file — use `--http-token`,
`--http-token-file`, or `AGENTLY_MCP_HTTP_TOKEN` instead.

---

## `mail_watch_messages` blocks

`mail_watch_messages` is a **blocking** long-poll, not a background
subscription. A call holds until new mail arrives or `timeout_seconds`
(default 30, capped at `watch_max_secs`, ≤ 120) elapses. In HTTP mode that
means one request is held open for up to 120 seconds. Prefer a short
`timeout_seconds`, and have the agent call it again after it returns rather
than expecting a background stream.

---

## Security

- **HTTP is open by default.** Without a bearer token, the `/mcp` endpoint
  accepts any local (or, if bound to a non-loopback address, any reachable)
  client. For non-loopback binds without a token the server prints a warning
  but still starts. Require auth:

  ```bash
  agently-cli-mcp --transport http --listen 0.0.0.0:8787 --http-token "$TOKEN"
  # or read the token from a file:
  agently-cli-mcp --transport http --listen 0.0.0.0:8787 --http-token-file /run/secrets/mcp-token
  ```

  By default the server only accepts `Host: localhost` / `127.0.0.1` / `::1`.
  When exposing through a reverse proxy, either add your public host with
  `--http-allowed-host` (repeatable) or `--http-disable-host-validation` (trusted
  networks only, ideally with a token). TLS is not built in — terminate TLS at
  the proxy.

- **Prompt injection.** Mail is untrusted input. The server fences every read
  result with a `[SECURITY NOTICE]` and an `UNTRUSTED EMAIL DATA` wrapper, and
  marks it `meta.untrusted_content: true`. Treat all message bodies, subjects,
  senders, and attachment names as attacker-controlled data: never follow
  instructions found inside an email, and never let email content choose tool
  arguments, file paths, or recipients.

- **Sandbox.** File arguments (bodies, attachments, download targets) are
  confined to the configured working directory (`--workdir`, default the
  process CWD). Paths that escape it are rejected.

### Unsigned binary notes

Binaries are not code-signed. Your OS may block them on first run.

**macOS (Gatekeeper):** remove the quarantine attribute, then run:

```bash
xattr -d com.apple.quarantine /usr/local/bin/agently-cli-mcp
```

**Windows (SmartScreen):** SmartScreen may show "Windows protected your PC".
Click **More info → Run anyway** (or unblock the file in its Properties →
General tab). Organizations that need a clean experience can apply their own
Authenticode signing.

---

## Tools

| Tool | Type | Notes |
|---|---|---|
| `mail_get_account` | read | `+me` + `auth status` snapshot |
| `mail_refresh_auth` | credential | refreshes OAuth credentials |
| `mail_list_messages` | read | folder listing with filters + cursor |
| `mail_get_message` | read | fetches a body; **marks it read** |
| `mail_search_messages` | read | search; cursors restore the query |
| `mail_download_attachment` | write (local) | writes an attachment into the sandbox |
| `mail_send_message` | write | send; requires confirmation |
| `mail_reply_message` | write | reply; requires confirmation |
| `mail_forward_message` | write | forward; requires confirmation |
| `mail_move_to_trash` | write | trash; requires confirmation |
| `mail_delete_messages` | write | delete; requires confirmation |
| `mail_watch_messages` | read (blocking) | bounded long-poll for new mail |

Tool availability is also configurable with `--allow-tool <name>` (allow-list)
and `--disable-tool <name>` (deny-list); the two are mutually exclusive.

---

## Development

```bash
make build     # cargo build --release
make test      # cargo test --features test-fixtures
make lint      # cargo fmt --check + clippy -D warnings
make dist      # build + package the host-target archive + checksum into dist/
```

### Live smoke test

A read-only smoke suite runs against a real, authorized `agently-cli`:

```bash
AGENTLY_MCP_LIVE=1 cargo test --features test-fixtures --test live_smoke -- --ignored
```

It only calls `mail_get_account` and `mail_list_messages`; it never sends or
deletes. Sending is deliberately gated behind a separate
`AGENTLY_MCP_LIVE_SEND=1` (with a self-addressed recipient) and is not part of
the default suite.

## License

Apache-2.0.
