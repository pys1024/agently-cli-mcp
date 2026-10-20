//! Configuration types and precedence merge.
//!
//! Precedence (highest first): CLI flags, then environment variables, then
//! `./config.toml`, then built-in defaults. The default config file is
//! `./config.toml`; `--config` selects an explicit path and `--no-config`
//! disables file loading entirely.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use crate::cli::CliArgs;

/// MCP transport selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

impl Transport {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "stdio" => Ok(Transport::Stdio),
            "http" => Ok(Transport::Http),
            other => Err(format!(
                "unknown transport `{other}` (expected `stdio` or `http`)"
            )),
        }
    }
}

/// Confirmation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmMode {
    Elicit,
    Token,
    Never,
}

impl ConfirmMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "elicit" => Ok(ConfirmMode::Elicit),
            "token" => Ok(ConfirmMode::Token),
            "never" => Ok(ConfirmMode::Never),
            other => Err(format!(
                "unknown confirm mode `{other}` (expected `elicit`, `token`, or `never`)"
            )),
        }
    }
}

/// Fully resolved configuration.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub transport: Transport,
    pub cli: CliConfig,
    pub confirm: ConfirmConfig,
    pub search: SearchConfig,
    pub limits: LimitsConfig,
    pub logging: LoggingConfig,
    pub tools: ToolsConfig,
    pub http: HttpConfig,
}

impl Config {
    /// Return the config as JSON with secrets redacted.
    pub fn redacted(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("config is always serializable");
        if let Some(http) = value.get_mut("http").and_then(|v| v.as_object_mut()) {
            for key in ["http_token", "http_token_file"] {
                if let Some(secret) = http.get_mut(key)
                    && secret.is_string()
                    && secret.as_str() != Some("")
                {
                    *secret = serde_json::Value::String("<redacted>".to_string());
                }
            }
        }
        value
    }

    /// Validate cross-field and numeric invariants, returning the first
    /// violation as a [`ConfigError::Invalid`].
    ///
    /// Rules (spec §13.3 / §13.2):
    /// - `allow` and `disabled` tool lists are mutually exclusive;
    /// - `watch_max_secs` must be within `5..=120`;
    /// - `max_body_chars` must be greater than `0`;
    /// - `max_concurrency` must be at least `1`.
    ///
    /// Non-loopback listen without a token is *not* an error here: it is
    /// allowed and only warned about at HTTP serve time.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.tools.allow.is_empty() && !self.tools.disabled.is_empty() {
            return Err(ConfigError::Invalid(
                "`allow` and `disabled` tool lists are mutually exclusive".to_string(),
            ));
        }
        if !(5..=120).contains(&self.cli.watch_max_secs) {
            return Err(ConfigError::Invalid(format!(
                "watch_max_secs must be between 5 and 120, got {}",
                self.cli.watch_max_secs
            )));
        }
        if self.limits.max_body_chars == 0 {
            return Err(ConfigError::Invalid(
                "max_body_chars must be greater than 0".to_string(),
            ));
        }
        if self.cli.max_concurrency < 1 {
            return Err(ConfigError::Invalid(
                "max_concurrency must be at least 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// CLI execution configuration.
#[derive(Debug, Clone, Serialize)]
pub struct CliConfig {
    pub path: Option<String>,
    pub workdir: Option<String>,
    pub max_concurrency: u32,
    pub timeout_secs: u64,
    pub read_timeout_secs: u64,
    pub send_timeout_secs: u64,
    pub watch_max_secs: u64,
}

/// Confirmation configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmConfig {
    pub mode: ConfirmMode,
    pub ttl_secs: u64,
}

/// Search-pagination configuration.
#[derive(Debug, Clone, Serialize)]
pub struct SearchConfig {
    pub ttl_secs: u64,
}

/// Output limits.
#[derive(Debug, Clone, Serialize)]
pub struct LimitsConfig {
    pub max_body_chars: usize,
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

/// Tool gating configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ToolsConfig {
    pub read_only: bool,
    pub disabled: Vec<String>,
    pub allow: Vec<String>,
}

/// HTTP transport configuration.
#[derive(Debug, Clone, Serialize)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    pub http_token: Option<String>,
    pub http_token_file: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub disable_host_validation: bool,
    pub cors_origins: Vec<String>,
    pub session_idle_secs: u64,
}

/// Error returned when configuration cannot be loaded or merged.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------
// File-side (TOML) structs. Every one denies unknown fields so typos fail
// loudly rather than being silently ignored.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    transport: Option<String>,
    cli: Option<FileCliConfig>,
    confirm: Option<FileConfirmConfig>,
    search: Option<FileSearchConfig>,
    limits: Option<FileLimitsConfig>,
    logging: Option<FileLoggingConfig>,
    tools: Option<FileToolsConfig>,
    http: Option<FileHttpConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCliConfig {
    path: Option<String>,
    workdir: Option<String>,
    max_concurrency: Option<u32>,
    timeout_secs: Option<u64>,
    read_timeout_secs: Option<u64>,
    send_timeout_secs: Option<u64>,
    watch_max_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfirmConfig {
    mode: Option<String>,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSearchConfig {
    ttl_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLimitsConfig {
    max_body_chars: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLoggingConfig {
    level: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToolsConfig {
    read_only: Option<bool>,
    disabled: Option<Vec<String>>,
    allow: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHttpConfig {
    listen: Option<String>,
    allowed_hosts: Option<Vec<String>>,
    disable_host_validation: Option<bool>,
    cors_origins: Option<Vec<String>>,
    session_idle_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Loading.
// ---------------------------------------------------------------------------

/// Load configuration using the process environment and current directory.
pub fn load(cli: CliArgs) -> Result<Config, ConfigError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().map_err(|e| ConfigError::Read {
        path: ".".to_string(),
        source: e,
    })?;
    load_with_sources(cli, &env, &cwd)
}

/// Load configuration from explicit sources (used by tests).
pub fn load_with_sources(
    cli: CliArgs,
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<Config, ConfigError> {
    let file = read_config_file(&cli, env, cwd)?;

    let transport = if let Some(s) = cli.transport.as_deref() {
        Transport::parse(s).map_err(|e| ConfigError::Invalid(format!("transport: {e}")))?
    } else if let Some(s) = env.get("AGENTLY_MCP_TRANSPORT") {
        Transport::parse(s).map_err(|e| ConfigError::Invalid(format!("transport: {e}")))?
    } else if let Some(s) = file.as_ref().and_then(|f| f.transport.as_deref()) {
        Transport::parse(s).map_err(|e| ConfigError::Invalid(format!("transport: {e}")))?
    } else {
        Transport::Stdio
    };

    let cli_cfg = CliConfig {
        path: cli
            .cli
            .clone()
            .or_else(|| env.get("AGENTLY_CLI_PATH").cloned())
            .or_else(|| {
                file.as_ref()
                    .and_then(|f| f.cli.as_ref())
                    .and_then(|c| c.path.clone())
            }),
        workdir: cli
            .workdir
            .clone()
            .or_else(|| env.get("AGENTLY_MCP_WORKDIR").cloned())
            .or_else(|| {
                file.as_ref()
                    .and_then(|f| f.cli.as_ref())
                    .and_then(|c| c.workdir.clone())
            }),
        max_concurrency: resolve_num(
            cli.max_concurrency,
            env.get("AGENTLY_MCP_MAX_CONCURRENCY").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.cli.as_ref())
                .and_then(|c| c.max_concurrency),
            1,
            "max_concurrency",
        )?,
        timeout_secs: resolve_num(
            cli.timeout_secs,
            env.get("AGENTLY_MCP_TIMEOUT_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.cli.as_ref())
                .and_then(|c| c.timeout_secs),
            30,
            "timeout_secs",
        )?,
        read_timeout_secs: resolve_num(
            cli.read_timeout_secs,
            env.get("AGENTLY_MCP_READ_TIMEOUT_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.cli.as_ref())
                .and_then(|c| c.read_timeout_secs),
            60,
            "read_timeout_secs",
        )?,
        send_timeout_secs: resolve_num(
            cli.send_timeout_secs,
            env.get("AGENTLY_MCP_SEND_TIMEOUT_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.cli.as_ref())
                .and_then(|c| c.send_timeout_secs),
            60,
            "send_timeout_secs",
        )?,
        watch_max_secs: resolve_num(
            cli.watch_max_secs,
            env.get("AGENTLY_MCP_WATCH_MAX_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.cli.as_ref())
                .and_then(|c| c.watch_max_secs),
            120,
            "watch_max_secs",
        )?,
    };

    let confirm_mode = if let Some(s) = cli.confirm.as_deref() {
        ConfirmMode::parse(s).map_err(|e| ConfigError::Invalid(format!("confirm: {e}")))?
    } else if let Some(s) = env.get("AGENTLY_MCP_CONFIRM") {
        ConfirmMode::parse(s).map_err(|e| ConfigError::Invalid(format!("confirm: {e}")))?
    } else if let Some(s) = file
        .as_ref()
        .and_then(|f| f.confirm.as_ref())
        .and_then(|c| c.mode.as_deref())
    {
        ConfirmMode::parse(s).map_err(|e| ConfigError::Invalid(format!("confirm: {e}")))?
    } else {
        ConfirmMode::Elicit
    };

    let confirm_cfg = ConfirmConfig {
        mode: confirm_mode,
        ttl_secs: resolve_num(
            cli.confirm_ttl_secs,
            env.get("AGENTLY_MCP_CONFIRM_TTL_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.confirm.as_ref())
                .and_then(|c| c.ttl_secs),
            300,
            "confirm_ttl_secs",
        )?,
    };

    let search_cfg = SearchConfig {
        ttl_secs: resolve_num(
            cli.search_ttl_secs,
            env.get("AGENTLY_MCP_SEARCH_TTL_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.search.as_ref())
                .and_then(|c| c.ttl_secs),
            600,
            "search_ttl_secs",
        )?,
    };

    let limits_cfg = LimitsConfig {
        max_body_chars: resolve_num(
            cli.max_body_chars,
            env.get("AGENTLY_MCP_MAX_BODY_CHARS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.limits.as_ref())
                .and_then(|c| c.max_body_chars),
            50_000,
            "max_body_chars",
        )?,
    };

    let log_level = cli
        .log_level
        .clone()
        .or_else(|| env.get("AGENTLY_MCP_LOG_LEVEL").cloned())
        .or_else(|| {
            file.as_ref()
                .and_then(|f| f.logging.as_ref())
                .and_then(|l| l.level.clone())
        })
        .unwrap_or_else(|| "info".to_string());
    validate_log_level(&log_level)?;

    let log_format = cli
        .log_format
        .clone()
        .or_else(|| env.get("AGENTLY_MCP_LOG_FORMAT").cloned())
        .or_else(|| {
            file.as_ref()
                .and_then(|f| f.logging.as_ref())
                .and_then(|l| l.format.clone())
        })
        .unwrap_or_else(|| "text".to_string());
    if !matches!(log_format.as_str(), "text" | "json") {
        return Err(ConfigError::Invalid(format!(
            "log_format: unknown value `{log_format}` (expected `text` or `json`)"
        )));
    }

    let logging_cfg = LoggingConfig {
        level: log_level,
        format: log_format,
    };

    let read_only = resolve_bool(
        cli.read_only,
        env.get("AGENTLY_MCP_READ_ONLY").map(|s| s.as_str()),
        file.as_ref()
            .and_then(|f| f.tools.as_ref())
            .and_then(|t| t.read_only),
        false,
        "read_only",
    )?;

    let tools_cfg = ToolsConfig {
        read_only,
        disabled: resolve_list(
            &cli.disable_tool,
            env.get("AGENTLY_MCP_DISABLE_TOOLS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.tools.as_ref())
                .and_then(|t| t.disabled.clone()),
        ),
        allow: resolve_list(
            &cli.allow_tool,
            env.get("AGENTLY_MCP_ALLOW_TOOLS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.tools.as_ref())
                .and_then(|t| t.allow.clone()),
        ),
    };

    let listen = {
        let raw = cli
            .listen
            .clone()
            .or_else(|| env.get("AGENTLY_MCP_LISTEN").cloned())
            .or_else(|| {
                file.as_ref()
                    .and_then(|f| f.http.as_ref())
                    .and_then(|h| h.listen.clone())
            })
            .unwrap_or_else(|| "127.0.0.1:8787".to_string());
        raw.parse::<SocketAddr>()
            .map_err(|e| ConfigError::Invalid(format!("listen: invalid address `{raw}`: {e}")))?
    };

    let disable_host_validation = resolve_bool(
        cli.http_disable_host_validation,
        env.get("AGENTLY_MCP_HTTP_DISABLE_HOST_VALIDATION")
            .map(|s| s.as_str()),
        file.as_ref()
            .and_then(|f| f.http.as_ref())
            .and_then(|h| h.disable_host_validation),
        false,
        "http_disable_host_validation",
    )?;

    let http_cfg = HttpConfig {
        listen,
        http_token: cli
            .http_token
            .clone()
            .or_else(|| env.get("AGENTLY_MCP_HTTP_TOKEN").cloned()),
        http_token_file: cli
            .http_token_file
            .clone()
            .or_else(|| env.get("AGENTLY_MCP_HTTP_TOKEN_FILE").cloned()),
        allowed_hosts: resolve_list(
            &cli.http_allowed_host,
            env.get("AGENTLY_MCP_HTTP_ALLOWED_HOSTS")
                .map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.http.as_ref())
                .and_then(|h| h.allowed_hosts.clone())
                .or_else(|| Some(vec!["localhost".into(), "127.0.0.1".into(), "::1".into()])),
        ),
        disable_host_validation,
        cors_origins: resolve_list(
            &cli.cors_origin,
            env.get("AGENTLY_MCP_CORS_ORIGINS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.http.as_ref())
                .and_then(|h| h.cors_origins.clone()),
        ),
        session_idle_secs: resolve_num(
            cli.session_idle_secs,
            env.get("AGENTLY_MCP_SESSION_IDLE_SECS").map(|s| s.as_str()),
            file.as_ref()
                .and_then(|f| f.http.as_ref())
                .and_then(|h| h.session_idle_secs),
            300,
            "session_idle_secs",
        )?,
    };

    let cfg = Config {
        transport,
        cli: cli_cfg,
        confirm: confirm_cfg,
        search: search_cfg,
        limits: limits_cfg,
        logging: logging_cfg,
        tools: tools_cfg,
        http: http_cfg,
    };
    cfg.validate()?;
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Read and parse the config file, if any. Returns `Ok(None)` when no file is
/// present (or loading is disabled), and `Err` when an explicitly requested
/// file is missing or any file fails to parse.
fn read_config_file(
    cli: &CliArgs,
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<Option<FileConfig>, ConfigError> {
    if cli.no_config {
        return Ok(None);
    }

    let explicit = cli
        .config
        .clone()
        .or_else(|| env.get("AGENTLY_MCP_CONFIG").cloned());

    let path: PathBuf = match &explicit {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { cwd.join(p) }
        }
        None => cwd.join("config.toml"),
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            return Ok(None);
        }
        Err(e) => {
            return Err(ConfigError::Read {
                path: path.display().to_string(),
                source: e,
            });
        }
    };

    let file: FileConfig = toml::from_str(&content).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    Ok(Some(file))
}

fn validate_log_level(level: &str) -> Result<(), ConfigError> {
    if matches!(level, "error" | "warn" | "info" | "debug" | "trace") {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "log_level: unknown value `{level}` (expected error, warn, info, debug, or trace)"
        )))
    }
}

fn resolve_num<T>(
    cli: Option<T>,
    env_val: Option<&str>,
    file: Option<T>,
    default: T,
    field: &str,
) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    if let Some(v) = cli {
        return Ok(v);
    }
    if let Some(s) = env_val {
        return s
            .parse::<T>()
            .map_err(|e| ConfigError::Invalid(format!("{field}: invalid value `{s}`: {e}")));
    }
    if let Some(v) = file {
        return Ok(v);
    }
    Ok(default)
}

fn resolve_bool(
    cli: bool,
    env_val: Option<&str>,
    file: Option<bool>,
    default: bool,
    field: &str,
) -> Result<bool, ConfigError> {
    if cli {
        return Ok(true);
    }
    if let Some(s) = env_val {
        return parse_bool(s, field);
    }
    if let Some(v) = file {
        return Ok(v);
    }
    Ok(default)
}

fn parse_bool(s: &str, field: &str) -> Result<bool, ConfigError> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::Invalid(format!(
            "{field}: invalid boolean `{s}`"
        ))),
    }
}

fn resolve_list(cli: &[String], env_val: Option<&str>, file: Option<Vec<String>>) -> Vec<String> {
    if !cli.is_empty() {
        return cli.to_vec();
    }
    if let Some(s) = env_val {
        return split_list(s);
    }
    if let Some(v) = file {
        return v;
    }
    Vec::new()
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}
