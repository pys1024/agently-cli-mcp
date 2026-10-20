//! Cross-platform discovery of the `agently-cli` executable.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::config::CliConfig;
use crate::error::AppError;

const INSTALL_HINT: &str = "install it with `npm install -g @tencent-qqmail/agently-cli`";

/// Resolve the `agently-cli` binary path.
///
/// Search order (spec §8.1): the configured path (`--cli` / env / config),
/// then `agently-cli` on `PATH`, then fail with `CLI_NOT_FOUND`.
pub fn find(cfg: &CliConfig) -> Result<PathBuf, AppError> {
    find_with_path(cfg, std::env::var_os("PATH"))
}

fn find_with_path(cfg: &CliConfig, path_var: Option<OsString>) -> Result<PathBuf, AppError> {
    if let Some(path) = cfg.path.as_deref() {
        return Ok(PathBuf::from(path));
    }

    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(&path_var) {
            for name in candidate_names() {
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    Err(AppError::CliNotFound(format!(
        "agently-cli not found in PATH; {INSTALL_HINT}"
    )))
}

/// Candidate executable names for the current platform.
fn candidate_names() -> Vec<String> {
    let mut names = vec![format!("agently-cli{}", std::env::consts::EXE_SUFFIX)];
    if cfg!(windows) {
        names.push("agently-cli.cmd".to_string());
        names.push("agently-cli.bat".to_string());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CliConfig;

    fn cfg_with(path: Option<&str>) -> CliConfig {
        CliConfig {
            path: path.map(str::to_string),
            workdir: None,
            max_concurrency: 1,
            timeout_secs: 30,
            read_timeout_secs: 60,
            send_timeout_secs: 60,
            watch_max_secs: 120,
        }
    }

    #[test]
    fn configured_path_is_returned_verbatim() {
        let cfg = cfg_with(Some("/opt/agently-cli"));
        assert_eq!(find(&cfg).unwrap(), PathBuf::from("/opt/agently-cli"));
    }

    #[test]
    fn missing_cli_is_cli_not_found() {
        let err = find_with_path(&cfg_with(None), Some(OsString::from(""))).unwrap_err();
        assert_eq!(err.code(), "CLI_NOT_FOUND");
    }
}
