//! Unix process-tree termination via process groups.

use std::time::Duration;

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

/// Terminate a spawned child immediately by killing its process group. The
/// runner normally holds a [`ProcessTree`] across the child's lifetime, so this
/// is a convenience for one-shot kills.
pub fn terminate(child: &mut Child) {
    if let Ok(mut tree) = ProcessTree::attach(child) {
        tree.terminate();
    }
}

/// Holds the process-group ID needed to terminate a child's whole tree.
///
/// Dropping this handle (e.g. when the runner future is cancelled) terminates
/// the tree; call [`ProcessTree::disarm`] once the child has exited normally.
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

    /// SIGTERM the whole process group, wait for graceful shutdown, then
    /// escalate to SIGKILL.
    pub fn terminate(&mut self) {
        let Some(pgid) = self.pgid.take() else {
            return;
        };
        let group = Pid::from_raw(-pgid);
        let _ = kill(group, Signal::SIGTERM);
        std::thread::sleep(Duration::from_secs(2));
        let _ = kill(group, Signal::SIGKILL);
    }

    /// Mark the tree as already-exited so dropping the handle is a no-op.
    pub(crate) fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.terminate();
    }
}
