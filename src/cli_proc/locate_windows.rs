//! Windows `.cmd`/`.bat` shim handling.
//!
//! npm installs `agently-cli.cmd` on Windows; `CreateProcess` cannot run a
//! `.cmd` directly, so the runner routes those through `cmd.exe /d /s /c` with
//! command-line escaping that matches `CommandLineToArgvW`.

use std::path::Path;

/// Returns true when `path` names a Windows batch script (`.cmd` / `.bat`).
pub fn is_cmd_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Escape a single argument for the Windows command line.
///
/// Implements the standard MSVC CRT quoting rules (`CommandLineToArgvW`), so
/// spaces, quotes, and `%` are neither split nor expanded.
pub fn escape_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quotes = arg.chars().any(|c| c.is_whitespace() || c == '"');
    let mut out = String::with_capacity(arg.len() + 2);
    if needs_quotes {
        out.push('"');
    }
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    if needs_quotes {
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
    } else {
        out.push_str(&"\\".repeat(backslashes));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cmd_and_bat() {
        assert!(is_cmd_script(Path::new("agently-cli.cmd")));
        assert!(is_cmd_script(Path::new("agently-cli.bat")));
        assert!(is_cmd_script(Path::new("A.CMD")));
        assert!(!is_cmd_script(Path::new("agently-cli.exe")));
        assert!(!is_cmd_script(Path::new("agently-cli")));
    }

    #[test]
    fn empty_arg_is_quoted() {
        assert_eq!(escape_arg(""), "\"\"");
    }

    #[test]
    fn plain_arg_is_untouched() {
        assert_eq!(escape_arg("--limit"), "--limit");
    }

    #[test]
    fn spaced_arg_is_quoted() {
        assert_eq!(
            escape_arg("C:\\Program Files\\nodejs"),
            "\"C:\\Program Files\\nodejs\""
        );
    }

    #[test]
    fn embedded_quote_is_escaped() {
        assert_eq!(escape_arg("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn trailing_backslash_is_preserved() {
        assert_eq!(escape_arg("C:\\"), "C:\\");
    }
}
