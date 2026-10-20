//! Command-line argument definitions for `agently-cli-mcp`.
//!
//! Every flag from the design spec (§13) is exposed here. Flags are
//! `Option<T>` so that "unset" can be distinguished from an explicit value;
//! repeatable list flags are `Vec<String>` where an empty vector means unset.

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "agently-cli-mcp",
    version,
    about = "MCP server wrapping the npm-installed agently-cli"
)]
pub struct CliArgs {
    /// Transport: `stdio` or `http`.
    #[arg(long)]
    pub transport: Option<String>,

    /// Path to a TOML config file (default: ./config.toml).
    #[arg(long)]
    pub config: Option<String>,

    /// Ignore the default config file.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub no_config: Option<bool>,

    /// Print the merged config (secrets redacted) and exit.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub print_config: Option<bool>,

    /// Check the CLI, version, auth, and workdir, then exit.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub check: Option<bool>,

    /// Log level: error, warn, info, debug, or trace.
    #[arg(long)]
    pub log_level: Option<String>,

    /// Log format: text or json.
    #[arg(long)]
    pub log_format: Option<String>,

    /// Path to the agently-cli binary (default: auto-detect).
    #[arg(long)]
    pub cli: Option<String>,

    /// Sandbox root directory for file arguments (default: process CWD).
    #[arg(long)]
    pub workdir: Option<String>,

    /// Concurrency for non-watch CLI calls.
    #[arg(long)]
    pub max_concurrency: Option<u32>,

    /// Timeout for ordinary CLI calls, in seconds.
    #[arg(long)]
    pub timeout_secs: Option<u64>,

    /// Timeout for list/read/search calls, in seconds.
    #[arg(long)]
    pub read_timeout_secs: Option<u64>,

    /// Timeout for send/reply/forward calls, in seconds.
    #[arg(long)]
    pub send_timeout_secs: Option<u64>,

    /// Maximum allowed watch duration, in seconds.
    #[arg(long)]
    pub watch_max_secs: Option<u64>,

    /// Confirmation mode: elicit, token, or never.
    #[arg(long)]
    pub confirm: Option<String>,

    /// Maximum number of body characters returned.
    #[arg(long)]
    pub max_body_chars: Option<usize>,

    /// Disable write and credential tools.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub read_only: Option<bool>,

    /// Disable a named tool (repeatable).
    #[arg(long)]
    pub disable_tool: Vec<String>,

    /// Allow only the named tools (repeatable; mutually exclusive with --disable-tool).
    #[arg(long)]
    pub allow_tool: Vec<String>,

    /// TTL for pending confirmations, in seconds.
    #[arg(long)]
    pub confirm_ttl_secs: Option<u64>,

    /// TTL for search-pagination context, in seconds.
    #[arg(long)]
    pub search_ttl_secs: Option<u64>,

    /// HTTP listen address.
    #[arg(long)]
    pub listen: Option<String>,

    /// Optional bearer token for HTTP authentication.
    #[arg(long)]
    pub http_token: Option<String>,

    /// Read the HTTP bearer token from a file.
    #[arg(long)]
    pub http_token_file: Option<String>,

    /// Allowed Host header value (repeatable).
    #[arg(long)]
    pub http_allowed_host: Vec<String>,

    /// Disable Host header validation (trusted networks only).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub http_disable_host_validation: Option<bool>,

    /// Allowed CORS origin (repeatable).
    #[arg(long)]
    pub cors_origin: Vec<String>,

    /// HTTP session idle timeout, in seconds.
    #[arg(long)]
    pub session_idle_secs: Option<u64>,
}
