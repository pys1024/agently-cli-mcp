//! CLI process management: locate the `agently-cli` binary, run it, and
//! classify its output.

pub mod locate;
pub mod locate_windows;
pub mod runner;
pub mod status;

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
