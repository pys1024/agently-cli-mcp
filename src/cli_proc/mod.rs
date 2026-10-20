//! CLI process management: locate the `agently-cli` binary, run it, and
//! classify its output.

pub mod bodyfile;
pub mod locate;
pub mod locate_windows;
pub mod runner;
pub mod status;

#[cfg(unix)]
mod process_group_unix;
#[cfg(windows)]
mod process_group_windows;

/// Cross-platform process-tree termination. On Unix this is a process-group
/// implementation; on Windows it is a Job-Object implementation. Both expose
/// [`process::configure`], [`process::terminate`], and [`process::ProcessTree`].
#[cfg(unix)]
pub mod process {
    pub use super::process_group_unix::{ProcessTree, configure, terminate};
}

#[cfg(windows)]
pub mod process {
    pub use super::process_group_windows::{ProcessTree, configure, terminate};
}

use serde_json::Value;

/// Raw output of a completed CLI invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CliResult {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Classification of a CLI invocation into success, pending confirmation, or
/// (via `Err`) a typed failure.
#[derive(Debug, Clone, PartialEq)]
pub enum CliOutcome {
    Success(Value),
    ConfirmationRequired { token: String, summary: Value },
}
