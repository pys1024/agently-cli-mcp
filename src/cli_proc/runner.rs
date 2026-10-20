//! CLI process runner: spawn `agently-cli`, collect output, apply timeouts.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::cli_proc::locate;
use crate::cli_proc::locate_windows::{self, CommandSpec};
use crate::cli_proc::process;
use crate::cli_proc::status;
use crate::cli_proc::{CliOutcome, CliResult};
use crate::config::CliConfig;
use crate::error::AppError;

/// Executes `agently-cli` subprocesses.
#[derive(Debug, Clone)]
pub struct Runner {
    bin: PathBuf,
    workdir: PathBuf,
    /// Serializes non-watch CLI calls (spec §8: default concurrency 1). Sized
    /// from `CliConfig.max_concurrency`; shared across clones.
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl Runner {
    /// Resolve the CLI binary and working directory from configuration.
    pub fn new(cfg: &CliConfig) -> Result<Runner, AppError> {
        let bin = locate::find(cfg)?;
        if !bin.exists() {
            return Err(AppError::CliNotFound(format!(
                "agently-cli not found at `{}`",
                bin.display()
            )));
        }
        let workdir = match cfg.workdir.as_deref() {
            Some(dir) => PathBuf::from(dir),
            None => std::env::current_dir().map_err(|e| {
                AppError::CliError(format!("cannot determine working directory: {e}"))
            })?,
        };
        let permits = cfg.max_concurrency.max(1) as usize;
        Ok(Runner {
            bin,
            workdir,
            semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
        })
    }

    /// Run the CLI with the given arguments and timeout, returning raw output.
    ///
    /// The call is serialized against other non-watch calls: the concurrency
    /// permit is held until the child has been reaped.
    pub async fn run(&self, args: &[String], timeout: Duration) -> Result<CliResult, AppError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| AppError::CliError("CLI concurrency semaphore closed".to_string()))?;
        self.run_impl(args, timeout).await
    }

    /// Run the CLI without the concurrency gate. Used by the watch tool so a
    /// long-poll never blocks ordinary mail calls.
    pub async fn run_unserialized(
        &self,
        args: &[String],
        timeout: Duration,
    ) -> Result<CliResult, AppError> {
        self.run_impl(args, timeout).await
    }

    /// Shared spawn/reap body used by both [`Runner::run`] and
    /// [`Runner::run_unserialized`].
    async fn run_impl(&self, args: &[String], timeout: Duration) -> Result<CliResult, AppError> {
        let mut cmd = match locate_windows::shape_command(&self.bin, args)? {
            CommandSpec::Direct { program, args } => {
                let mut cmd = tokio::process::Command::new(&program);
                cmd.args(args);
                cmd
            }
            CommandSpec::CmdExe { command } => {
                let mut cmd = tokio::process::Command::new(comspec());
                cmd.arg("/d").arg("/s").arg("/c").arg(command);
                cmd
            }
        };
        cmd.current_dir(&self.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::configure(&mut cmd);

        let child = cmd
            .spawn()
            .map_err(|e| AppError::CliError(format!("failed to spawn CLI: {e}")))?;

        // Holds the process-group/Job Object needed to kill the whole tree on
        // timeout or if this future is dropped/cancelled.
        let mut tree = process::ProcessTree::attach(&child)?;

        let output =
            tokio::time::timeout(timeout, async move { child.wait_with_output().await }).await;

        match output {
            Ok(Ok(output)) => {
                tree.disarm();
                Ok(CliResult {
                    stdout: output.stdout,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code(),
                })
            }
            Ok(Err(e)) => {
                // The child has already been reaped; disarm so the empty group
                // does not incur the graceful 2s termination.
                tree.disarm();
                Err(AppError::CliError(format!(
                    "failed to read CLI output: {e}"
                )))
            }
            Err(_elapsed) => {
                // Graceful SIGTERM -> 2s -> SIGKILL of the whole tree.
                tree.terminate().await;
                Err(AppError::Timeout(format!(
                    "CLI timed out after {} ms",
                    timeout.as_millis()
                )))
            }
        }
    }

    /// Run the CLI and classify the result into a `CliOutcome`.
    pub async fn run_json(
        &self,
        args: &[String],
        timeout: Duration,
    ) -> Result<CliOutcome, AppError> {
        let result = self.run(args, timeout).await?;
        status::classify(&result.stdout, &result.stderr, result.exit_code)
    }
}

/// Resolve `cmd.exe` for the batch-script fallback, honoring `%COMSPEC%`.
fn comspec() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}
