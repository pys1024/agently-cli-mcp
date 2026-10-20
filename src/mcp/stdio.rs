//! stdio transport: build the server state and serve over stdin/stdout.

use rmcp::ServiceExt;

use crate::config::Config;
use crate::error::AppError;

use super::build_server;
use super::build_state;

/// Build the shared state and serve the MCP server over stdio until the
/// transport closes. stdout carries JSON-RPC only; logging stays on stderr.
pub async fn serve(cfg: Config) -> Result<(), AppError> {
    let state = build_state(cfg)?;
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
