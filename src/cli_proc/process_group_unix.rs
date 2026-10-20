//! Unix process-tree termination via process groups.

use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::{Child, Command};

use crate::error::AppError;

/// Put the child into its own process group before `exec`, so a single
/// `kill(-pid, ...)` reaches the child and every descendant it later spawns
/// (e.g. the npm `agently-cli` shim re-execing the real platform binary).
pub fn configure(cmd: &mut Command) {
    // A pgid of 0 makes the child its own group leader (pgid == its pid).
    cmd.process_group(0);
}

/// Terminate a spawned child immediately (non-blocking SIGKILL of the group).
/// The runner normally holds a [`ProcessTree`] for a graceful SIGTERM→SIGKILL
/// shutdown, so this is a convenience for one-shot immediate kills.
pub fn terminate(child: &mut Child) {
    if let Ok(mut tree) = ProcessTree::attach(child) {
        tree.kill_now();
    }
}

/// Holds the process-group ID needed to terminate a child's whole tree.
///
/// Dropping this handle (e.g. when the runner future is cancelled) performs an
/// immediate, non-blocking SIGKILL of the group. The graceful SIGTERM→SIGKILL
/// sequence lives in the async [`ProcessTree::terminate`]; call
/// [`ProcessTree::disarm`] once the child has exited normally.
#[derive(Debug)]
pub struct ProcessTree {
    pgid: Option<i32>,
}

impl ProcessTree {
    /// Capture the process-group ID of a freshly spawned child.
    pub fn attach(child: &Child) -> Result<ProcessTree, AppError> {
        Ok(ProcessTree {
            pgid: child.id().map(|id| id as i32),
        })
    }

    /// SIGTERM the whole process group, wait up to 2 seconds for it to exit,
    /// then SIGKILL if any process is still alive. This awaits, so it must run
    /// in an async context (the runner's timeout path); `Drop` instead uses the
    /// non-blocking [`ProcessTree::kill_now`].
    pub async fn terminate(&mut self) {
        let Some(pgid) = self.pgid else {
            return;
        };
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Consume the pgid only now, so a cancellation during the grace period
        // still lets `Drop` perform an immediate SIGKILL.
        let Some(pgid) = self.pgid.take() else {
            return;
        };
        if group_alive(pgid) {
            let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
        }
    }

    /// Immediate, non-blocking SIGKILL of the whole group. Used by `Drop`.
    fn kill_now(&mut self) {
        let Some(pgid) = self.pgid.take() else {
            return;
        };
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
    }

    /// Mark the tree as already-exited so dropping the handle is a no-op.
    pub(crate) fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// Whether any process still exists in the group identified by `pgid`.
fn group_alive(pgid: i32) -> bool {
    !matches!(kill(Pid::from_raw(-pgid), None), Err(Errno::ESRCH))
}
