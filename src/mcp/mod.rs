//! MCP transport and server glue.

pub mod http;
pub mod result;
pub mod server;
pub mod stdio;

pub use server::{AppState, MailServer, build_server};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::cli_proc::bodyfile;
use crate::cli_proc::runner::Runner;
use crate::config::Config;
use crate::confirm::ConfirmStore;
use crate::error::AppError;
use crate::guard::Sandbox;

/// How old a body temp file must be before the startup sweep removes it.
const STALE_BODY_FILE_AGE: Duration = Duration::from_secs(10 * 60);
/// How often the background janitor purges expired pending confirmations and
/// their retained body files.
const CONFIRM_JANITOR_INTERVAL: Duration = Duration::from_secs(60);

/// Build the shared [`AppState`] from a resolved [`Config`], locating the CLI
/// binary and sandbox root. Shared by the stdio and HTTP transports so the two
/// cannot drift.
///
/// Also sweeps stale body temp files left by a previous run and starts the
/// background confirmation janitor (a detached task that reaps expired pending
/// confirmations and deletes their retained body files every minute).
pub fn build_state(cfg: Config) -> Result<Arc<AppState>, AppError> {
    let runner = Runner::new(&cfg.cli)?;

    let workdir = match cfg.cli.workdir.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| AppError::Other(format!("cannot determine working directory: {e}")))?,
    };
    let sandbox = Sandbox::new(workdir)?;
    let root = sandbox.root().to_path_buf();

    let state = Arc::new(AppState::new(
        Arc::new(cfg),
        Arc::new(runner),
        Arc::new(sandbox),
    ));

    // Startup sweep: clear stale body files from a previous run.
    bodyfile::sweep_stale_body_files(&root, STALE_BODY_FILE_AGE);
    spawn_confirm_janitor(state.confirm.clone());

    Ok(state)
}

/// Spawn a detached task that periodically calls [`ConfirmStore::cleanup_expired`]
/// and deletes the returned body files. The task holds only a clone of the
/// shared [`ConfirmStore`] and never blocks shutdown: when the tokio runtime is
/// dropped the task is dropped with it, so it does not keep the process alive.
fn spawn_confirm_janitor(confirm: Arc<ConfirmStore>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        // No runtime (e.g. a synchronous caller); the startup sweep already ran.
        return;
    };
    handle.spawn(async move {
        let mut interval = tokio::time::interval(CONFIRM_JANITOR_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            for path in confirm.cleanup_expired() {
                let _ = std::fs::remove_file(path);
            }
        }
    });
}
