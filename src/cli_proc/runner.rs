//! CLI process runner: spawn `agently-cli`, collect output, apply timeouts.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

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

/// Streaming output of a bounded [`Runner::run_watch`] invocation.
#[derive(Debug, Clone)]
pub struct WatchOutput {
    /// NDJSON events parsed from stdout, in arrival order.
    pub events: Vec<Value>,
    /// Whether the process was still running and had to be killed at timeout.
    pub timed_out: bool,
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

    /// Stream NDJSON from a long-running CLI until it exits or `timeout`
    /// elapses, killing the whole process tree on timeout.
    ///
    /// Unlike [`Runner::run`], this does not take the concurrency permit (spec
    /// §8: a watch must not block ordinary mail calls) and does not classify
    /// output; the caller receives the raw parsed event stream plus a
    /// timed-out flag.
    pub async fn run_watch(
        &self,
        args: &[String],
        timeout: Duration,
    ) -> Result<WatchOutput, AppError> {
        let mut cmd = match locate_windows::shape_command(&self.bin, args)? {
            CommandSpec::Direct { program, args } => {
                let mut cmd = tokio::process::Command::new(&program);
                cmd.args(args);
                cmd
            }
            CommandSpec::CmdExe { command } => cmd_exe(comspec(), command),
        };
        cmd.current_dir(&self.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::configure(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::CliError(format!("failed to spawn CLI: {e}")))?;
        // Same attach-failure guard as [`Runner::run_impl`]: if the (Windows)
        // Job Object cannot be attached after spawn, kill the child rather than
        // leak an orphan process.
        let mut tree = match process::ProcessTree::attach(&child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // Drain stderr in the background so a chatty CLI never blocks on a full
        // pipe and stalls the event stream.
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut reader = BufReader::new(stderr);
            let _ = AsyncReadExt::read_to_end(&mut reader, &mut buf).await;
        });

        let started = Instant::now();
        let mut lines = BufReader::new(stdout).lines();
        let mut events = Vec::new();
        let mut timed_out = false;

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = &mut deadline => {
                    timed_out = true;
                    break;
                }
                line = lines.next_line() => match line {
                    Ok(Some(text)) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            events.push(value);
                        }
                    }
                    // EOF or a read error ends the stream: the child has
                    // stopped producing events.
                    Ok(None) | Err(_) => break,
                },
            }
        }

        if timed_out {
            // Graceful SIGTERM -> 2s -> SIGKILL of the whole tree.
            tree.terminate().await;
            let _ = child.wait().await;
        } else {
            // EOF: the child closed stdout. It may still be running (for
            // example a CLI that prints a couple of events and then sleeps),
            // so bound the final reap by the remaining budget instead of
            // hanging the whole call on `child.wait()`.
            let remaining = timeout.saturating_sub(started.elapsed());
            match tokio::time::timeout(remaining, child.wait()).await {
                Ok(_) => tree.disarm(),
                Err(_elapsed) => {
                    tree.terminate().await;
                    let _ = child.wait().await;
                }
            }
        }
        let _ = stderr_task.await;

        Ok(WatchOutput { events, timed_out })
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
            CommandSpec::CmdExe { command } => cmd_exe(comspec(), command),
        };
        cmd.current_dir(&self.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::configure(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::CliError(format!("failed to spawn CLI: {e}")))?;

        // Holds the process-group/Job Object needed to kill the whole tree on
        // timeout or if this future is dropped/cancelled.
        //
        // On Windows the Job Object is attached *after* spawn (a limitation of
        // `tokio::process`, which exposes no `CREATE_SUSPENDED` hook), so there
        // is a sub-millisecond window between `spawn()` returning and the
        // `AssignProcessToJobObject` call in which the child could, in theory,
        // spawn a descendant outside the job. That window is inherent to the
        // current design and not hardened further in this fix wave. If `attach`
        // itself fails (e.g. the Job Object could not be created/assigned), the
        // child is already running outside any kill-on-close group, so we kill
        // it explicitly before propagating the error to avoid orphaning it.
        let mut tree = match process::ProcessTree::attach(&child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };

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

/// Build a `cmd.exe /d /s /c` invocation.
///
/// The command string is already fully quoted for `cmd.exe`. On Windows it must
/// be appended with `raw_arg`: Rust's normal argument quoting would escape the
/// embedded quotes with backslashes, producing a command such as
/// `\\C:\...\agently-cli.cmd\" \"message\"`, which `cmd.exe` rejects.
fn cmd_exe(comspec: PathBuf, command: String) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(comspec);
    cmd.arg("/d").arg("/s").arg("/c");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().raw_arg(&command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg(&command);
    }
    cmd
}

/// Resolve `cmd.exe` for the batch-script fallback, honoring `%COMSPEC%`.
fn comspec() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Stdio;

    #[tokio::test]
    async fn cmd_exe_raw_arg_preserves_script_and_args() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("echo args.cmd");
        std::fs::write(
            &script,
            "@echo off\r\n:loop\r\nif \"%~1\"==\"\" goto end\r\necho [%~1]\r\nshift\r\ngoto loop\r\n:end\r\n",
        )
        .unwrap();

        let spec = crate::cli_proc::locate_windows::shape_command(
            &script,
            &["+list".to_string(), "INBOX".to_string()],
        )
        .unwrap();
        let CommandSpec::CmdExe { command } = spec else {
            panic!("expected CmdExe fallback, got {spec:?}");
        };

        let mut cmd = cmd_exe(comspec(), command);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "cmd.exe failed: status={:?} stdout={stdout} stderr={stderr}",
            output.status
        );
        assert!(stdout.contains("[+list]"), "stdout: {stdout}");
        assert!(stdout.contains("[INBOX]"), "stdout: {stdout}");
    }
}
