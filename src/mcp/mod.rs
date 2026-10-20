//! MCP transport and server glue.

pub mod http;
pub mod result;
pub mod server;
pub mod stdio;

pub use server::{AppState, MailServer, build_server};

use std::path::PathBuf;
use std::sync::Arc;

use crate::cli_proc::runner::Runner;
use crate::config::Config;
use crate::error::AppError;
use crate::guard::Sandbox;

/// Build the shared [`AppState`] from a resolved [`Config`], locating the CLI
/// binary and sandbox root. Shared by the stdio and HTTP transports so the two
/// cannot drift.
pub fn build_state(cfg: Config) -> Result<Arc<AppState>, AppError> {
    let runner = Runner::new(&cfg.cli)?;

    let workdir = match cfg.cli.workdir.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| AppError::Other(format!("cannot determine working directory: {e}")))?,
    };
    let sandbox = Sandbox::new(workdir)?;

    Ok(Arc::new(AppState::new(
        Arc::new(cfg),
        Arc::new(runner),
        Arc::new(sandbox),
    )))
}
