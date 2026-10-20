//! Attachment download tool: `attachment +download` with an output path that
//! must stay inside the sandbox.

use std::path::Path;
use std::time::Instant;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::AppError;
use crate::mcp::server::AppState;

use super::run_cli;

/// Arguments for `mail_download_attachment`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DownloadArgs {
    pub msg: String,
    pub att: String,
    pub output: Option<String>,
}

pub async fn download_attachment(
    state: &AppState,
    args: DownloadArgs,
) -> Result<(Value, Value), AppError> {
    let start = Instant::now();

    // Resolve the output directory inside the sandbox, defaulting to `.`.
    let output = match args.output.as_deref() {
        Some(dir) => {
            let rel = state.sandbox.resolve_output_dir(Path::new(dir))?;
            if rel.as_os_str().is_empty() {
                ".".to_string()
            } else {
                rel.to_string_lossy().into_owned()
            }
        }
        None => ".".to_string(),
    };

    let mut argv = vec!["attachment".to_string(), "+download".to_string()];
    argv.push("--msg".to_string());
    argv.push(args.msg);
    argv.push("--att".to_string());
    argv.push(args.att);
    argv.push("--output".to_string());
    argv.push(output);

    let data = run_cli(state, &argv, state.cfg.cli.timeout_secs).await?;
    let meta = json!({
        "untrusted_content": false,
        "duration_ms": start.elapsed().as_millis(),
    });
    Ok((data, meta))
}
