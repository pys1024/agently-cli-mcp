//! stdio transport: build the server state and serve over stdin/stdout.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::ServiceExt;

use crate::cli_proc::runner::Runner;
use crate::config::Config;
use crate::error::AppError;
use crate::guard::Sandbox;

use super::build_server;
use super::server::AppState;

/// Build the shared state and serve the MCP server over stdio until the
/// transport closes. stdout carries JSON-RPC only; logging stays on stderr.
pub async fn serve(cfg: Config) -> Result<(), AppError> {
    let runner = Runner::new(&cfg.cli)?;

    let workdir = match cfg.cli.workdir.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| AppError::Other(format!("cannot determine working directory: {e}")))?,
    };
    let sandbox = Sandbox::new(workdir)?;

    let state = Arc::new(AppState::new(
        Arc::new(cfg),
        Arc::new(runner),
        Arc::new(sandbox),
    ));
    let server = build_server(state);

    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| AppError::Other(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok(())
}
