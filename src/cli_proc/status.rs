//! Status classification: map CLI stdout/stderr/exit-code into an outcome.

use serde_json::Value;

use crate::cli_proc::CliOutcome;
use crate::error::AppError;

/// Classify a completed CLI invocation into a success, a pending confirmation,
/// or an error.
///
/// Decision order (spec §8.2): confirmation first, then `ok:false`, then exit
/// code as a fallback. Non-JSON stdout is a protocol error and is never echoed.
pub fn classify(
    stdout: &[u8],
    stderr: &str,
    exit_code: Option<i32>,
) -> Result<CliOutcome, AppError> {
    match serde_json::from_slice::<Value>(stdout) {
        Ok(value) => classify_json(value, stderr, exit_code),
        Err(_) => {
            if stdout_is_blank(stdout) {
                Err(classify_by_exit_code(stderr, exit_code))
            } else {
                Err(AppError::ProtocolError(
                    "CLI returned non-JSON output on stdout".to_string(),
                ))
            }
        }
    }
}

fn stdout_is_blank(stdout: &[u8]) -> bool {
    stdout.iter().all(|b| b.is_ascii_whitespace())
}

fn classify_json(
    value: Value,
    stderr: &str,
    exit_code: Option<i32>,
) -> Result<CliOutcome, AppError> {
    let Some(obj) = value.as_object() else {
        // Valid JSON but not an object: treat as a missing envelope.
        return Err(classify_by_exit_code(stderr, exit_code));
    };

    // 1. Confirmation required (not success), regardless of exit code.
    if let Some(data) = obj.get("data").and_then(Value::as_object)
        && data.get("confirmation_required").and_then(Value::as_bool) == Some(true)
    {
        let token = data
            .get("confirmation_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let summary = data.get("summary").cloned().unwrap_or(Value::Null);
        return Ok(CliOutcome::ConfirmationRequired { token, summary });
    }

    match obj.get("ok") {
        // 2. Explicit failure envelope.
        Some(Value::Bool(false)) => Err(classify_error(obj, stderr)),
        // 4. Success.
        Some(Value::Bool(true)) => {
            let data = obj.get("data").cloned().unwrap_or(Value::Null);
            Ok(CliOutcome::Success(data))
        }
        // 3. Missing/non-bool `ok`: fall back to exit code.
        _ => Err(classify_by_exit_code(stderr, exit_code)),
    }
}

fn classify_error(obj: &serde_json::Map<String, Value>, stderr: &str) -> AppError {
    let error = obj.get("error").and_then(Value::as_object);
    let etype = error.and_then(|e| e.get("type")).and_then(Value::as_str);
    let ecode = error.and_then(|e| e.get("code")).and_then(Value::as_i64);
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| first_line(stderr).map(str::to_string))
        .unwrap_or_default();

    match etype {
        Some("auth_error" | "auth" | "unauthorized") => AppError::AuthRequired(message),
        Some("network_error" | "network") => AppError::NetworkError(message),
        Some("rate_limited" | "rate_limit") => AppError::RateLimited(message),
        Some("not_found") => AppError::NotFound(message),
        Some("api_error") | None => match ecode {
            Some(400) => AppError::Rejected(message),
            Some(401) | Some(403) => AppError::AuthRequired(message),
            Some(404) => AppError::NotFound(message),
            Some(429) => AppError::RateLimited(message),
            _ => AppError::Rejected(message),
        },
        Some(_) => AppError::Rejected(message),
    }
}

fn classify_by_exit_code(stderr: &str, exit_code: Option<i32>) -> AppError {
    let line = first_line(stderr).unwrap_or_default().to_string();
    match exit_code {
        Some(1) | Some(2) => AppError::InvalidArgument(line),
        Some(3) => AppError::AuthRequired(line),
        Some(4) => AppError::NetworkError(line),
        Some(6) => AppError::Rejected(line),
        Some(7) => AppError::RateLimited(line),
        Some(0) => {
            AppError::ProtocolError("CLI exited successfully without a JSON envelope".to_string())
        }
        Some(code) => AppError::CliError(format!("CLI exited with code {code}: {line}")),
        None => AppError::CliError(format!("CLI terminated abnormally: {line}")),
    }
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().next().map(str::trim).filter(|l| !l.is_empty())
}
