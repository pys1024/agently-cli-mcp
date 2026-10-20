//! Windows `.cmd`/`.bat` shim handling.
//!
//! npm installs `agently-cli.cmd` on Windows; `CreateProcess` cannot run a
//! `.cmd` directly. We therefore prefer to resolve the underlying platform
//! binary (or the Node `run.js` entry) and execute that instead, and only fall
//! back to `cmd.exe /d /s /c` when no real program can be located.

use std::path::{Path, PathBuf};

use crate::error::AppError;

/// A real program located behind an npm `.cmd`/`.bat` shim.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedProgram {
    /// A real platform executable (e.g. `.../bin/agently-cli.exe`).
    Exe(PathBuf),
    /// The Node entry script (e.g. `.../scripts/run.js`), executed via `node`.
    Node(PathBuf),
}

/// How to execute the resolved CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandSpec {
    /// Run a program directly (a real executable, or `node` with a script).
    Direct { program: PathBuf, args: Vec<String> },
    /// Run a batch script through `cmd.exe /d /s /c <command>`, where `command`
    /// is the fully escaped command line (script path plus arguments).
    CmdExe { command: String },
}

/// Returns true when `path` names a Windows batch script (`.cmd` / `.bat`).
pub fn is_cmd_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Escape a single argument for the `cmd.exe /c` command line.
///
/// The argument is wrapped in double quotes. Inside `cmd.exe` double quotes the
/// characters `&`, `|`, `<`, `>`, `^`, `(`, `)` and whitespace are literal, and
/// an embedded double quote is escaped by doubling it (`"` -> `""`).
///
/// `%` cannot be passed reliably through `cmd.exe` (it always triggers
/// environment-variable expansion, even inside quotes), so an argument
/// containing `%` is rejected with `INVALID_ARGUMENT` rather than corrupted.
pub fn escape_cmd_arg(arg: &str) -> Result<String, AppError> {
    if arg.contains('%') {
        return Err(AppError::InvalidArgument(format!(
            "argument contains '%', which cannot be passed safely through cmd.exe: {arg}"
        )));
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

/// Resolve the real program behind an npm `.cmd`/`.bat` shim, if possible.
///
/// Probes the standard npm platform-package layouts (local, global, and
/// hoisted) for the platform binary or the Node `run.js` entry point.
pub fn resolve_real_program(shim: &Path) -> Option<ResolvedProgram> {
    let shim_canonical = std::fs::canonicalize(shim).unwrap_or_else(|_| shim.to_path_buf());
    let shim_dir = shim_canonical.parent()?;
    let exe_name = platform_exe_name();

    // Candidate package roots, in order: local npm
    // (`node_modules/.bin/shim` -> `node_modules/<pkg>`), global npm
    // (`<prefix>/shim` -> `<prefix>/node_modules/<pkg>`), and hoisted layouts.
    let roots: Vec<PathBuf> = vec![
        shim_dir.join(".."),
        shim_dir.join("node_modules"),
        shim_dir.join("..").join("node_modules"),
        shim_dir.to_path_buf(),
    ];

    for root in roots {
        for pkg in platform_package_names() {
            let pkg_root = root.join(&pkg);
            let exe = pkg_root.join("bin").join(&exe_name);
            if exe.is_file() {
                return Some(ResolvedProgram::Exe(canonical(&exe)));
            }
            let run_js = pkg_root.join("scripts").join("run.js");
            if run_js.is_file() {
                return Some(ResolvedProgram::Node(canonical(&run_js)));
            }
        }
    }
    None
}

/// Normalize a resolved path, collapsing `..`/symlinks when possible.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Shape a CLI invocation into the concrete program/argv to execute.
///
/// Non-batch binaries are executed directly. Batch scripts (`.cmd`/`.bat`) are
/// resolved to their real program when possible and otherwise executed via
/// `cmd.exe /d /s /c`.
pub fn shape_command(bin: &Path, args: &[String]) -> Result<CommandSpec, AppError> {
    if !is_cmd_script(bin) {
        return Ok(CommandSpec::Direct {
            program: bin.to_path_buf(),
            args: args.to_vec(),
        });
    }

    if let Some(resolved) = resolve_real_program(bin) {
        return Ok(match resolved {
            ResolvedProgram::Exe(exe) => CommandSpec::Direct {
                program: exe,
                args: args.to_vec(),
            },
            ResolvedProgram::Node(run_js) => {
                let mut argv = vec![run_js.to_string_lossy().into_owned()];
                argv.extend_from_slice(args);
                CommandSpec::Direct {
                    program: PathBuf::from("node"),
                    args: argv,
                }
            }
        });
    }

    // Fall back to `cmd.exe /d /s /c`. The leading `"` opens an outer quote
    // pair that cmd's `/s` rules strip, leaving `"script" "arg1" ...`.
    let mut command = String::from("\"");
    command.push_str(&escape_cmd_arg(&bin.to_string_lossy())?);
    for arg in args {
        command.push(' ');
        command.push_str(&escape_cmd_arg(arg)?);
    }
    command.push('"');
    Ok(CommandSpec::CmdExe { command })
}

/// The platform npm package directory names to probe, in order.
fn platform_package_names() -> Vec<String> {
    let base = format!("agently-cli-{}-{}", platform_name(), arch_name());
    vec![format!("@tencent-qqmail/{base}"), base]
}

fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn platform_exe_name() -> String {
    format!("agently-cli{}", std::env::consts::EXE_SUFFIX)
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
    fn escape_plain_arg_is_quoted() {
        assert_eq!(escape_cmd_arg("--limit").unwrap(), "\"--limit\"");
    }

    #[test]
    fn escape_empty_arg_is_quoted() {
        assert_eq!(escape_cmd_arg("").unwrap(), "\"\"");
    }

    #[test]
    fn escape_spaced_arg_is_quoted() {
        assert_eq!(
            escape_cmd_arg("C:\\Program Files\\nodejs").unwrap(),
            "\"C:\\Program Files\\nodejs\""
        );
    }

    #[test]
    fn escape_doubles_embedded_quotes() {
        assert_eq!(escape_cmd_arg("a\"b").unwrap(), "\"a\"\"b\"");
    }

    #[test]
    fn escape_keeps_metacharacters_literal() {
        assert_eq!(
            escape_cmd_arg("a&b|c<d>e^f(g)h").unwrap(),
            "\"a&b|c<d>e^f(g)h\""
        );
    }

    #[test]
    fn escape_rejects_percent() {
        let err = escape_cmd_arg("100% sure").unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn non_cmd_path_is_direct() {
        let spec = shape_command(
            Path::new("/usr/bin/agently-cli"),
            &["--version".to_string()],
        )
        .unwrap();
        match spec {
            CommandSpec::Direct { program, args } => {
                assert_eq!(program, PathBuf::from("/usr/bin/agently-cli"));
                assert_eq!(args, vec!["--version".to_string()]);
            }
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn cmd_shim_resolves_platform_binary() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("node_modules/.bin/agently-cli.cmd");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\n").unwrap();

        let pkg = platform_package_names()[0].clone();
        let exe = dir
            .path()
            .join("node_modules")
            .join(&pkg)
            .join("bin")
            .join(platform_exe_name());
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();

        let args = vec!["--limit".to_string(), "5".to_string()];
        match shape_command(&shim, &args).unwrap() {
            CommandSpec::Direct { program, args: got } => {
                assert_eq!(
                    std::fs::canonicalize(&program).unwrap(),
                    std::fs::canonicalize(&exe).unwrap()
                );
                assert_eq!(got, args);
            }
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn cmd_shim_resolves_node_run_js() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("node_modules/.bin/agently-cli.cmd");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\n").unwrap();

        let pkg = platform_package_names()[0].clone();
        let run_js = dir
            .path()
            .join("node_modules")
            .join(&pkg)
            .join("scripts")
            .join("run.js");
        std::fs::create_dir_all(run_js.parent().unwrap()).unwrap();
        std::fs::write(&run_js, "// run\n").unwrap();

        match shape_command(&shim, &["+list".to_string()]).unwrap() {
            CommandSpec::Direct { program, args } => {
                assert_eq!(program, PathBuf::from("node"));
                assert_eq!(
                    args,
                    vec![run_js.to_string_lossy().into_owned(), "+list".to_string()]
                );
            }
            other => panic!("expected Direct node, got {other:?}"),
        }
    }

    #[test]
    fn cmd_shim_falls_back_to_cmd_exe_preserving_args() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("agently-cli.cmd");
        std::fs::write(&shim, "@echo off\n").unwrap();

        let args = vec!["hello world".to_string(), "a\"b".to_string()];
        match shape_command(&shim, &args).unwrap() {
            CommandSpec::CmdExe { command } => {
                assert!(command.contains("\"hello world\""));
                assert!(command.contains("\"a\"\"b\""));
            }
            other => panic!("expected CmdExe, got {other:?}"),
        }
    }

    #[test]
    fn cmd_shim_rejects_percent_argument() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("agently-cli.cmd");
        std::fs::write(&shim, "@echo off\n").unwrap();

        let err = shape_command(&shim, &["100%".to_string()]).unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }
}
