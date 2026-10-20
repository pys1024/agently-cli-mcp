use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config::{ConfirmMode, Transport, load_with_sources};
use clap::Parser;
use std::collections::HashMap;
use std::fs;
use tempfile::tempdir;

fn args(argv: &[&str]) -> CliArgs {
    CliArgs::try_parse_from(std::iter::once("agently-cli-mcp").chain(argv.iter().copied())).unwrap()
}

#[test]
fn defaults_are_stable() {
    let dir = tempdir().unwrap();
    let cfg = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap();
    assert!(matches!(cfg.transport, Transport::Stdio));
    assert!(matches!(cfg.confirm.mode, ConfirmMode::Elicit));
    assert_eq!(cfg.cli.watch_max_secs, 120);
    assert_eq!(cfg.limits.max_body_chars, 50_000);
    assert_eq!(cfg.cli.max_concurrency, 1);
}

#[test]
fn flag_beats_env_beats_file() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "transport = \"http\"\n[logging]\nlevel = \"debug\"\n",
    )
    .unwrap();
    let mut env = HashMap::new();
    env.insert("AGENTLY_MCP_LOG_LEVEL".into(), "warn".into());
    let cfg = load_with_sources(args(&["--log-level", "trace"]), &env, dir.path()).unwrap();
    assert_eq!(cfg.logging.level, "trace");
}

#[test]
fn config_file_is_read_from_cwd() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "transport = \"http\"\n[http]\nlisten = \"0.0.0.0:9000\"\n",
    )
    .unwrap();
    let cfg = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap();
    assert!(matches!(cfg.transport, Transport::Http));
    assert_eq!(cfg.http.listen.port(), 9000);
}

#[test]
fn unknown_field_is_rejected() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("config.toml"), "nope = 1\n").unwrap();
    let err = load_with_sources(args(&[]), &HashMap::new(), dir.path()).unwrap_err();
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn missing_default_file_is_ok_but_explicit_missing_errors() {
    let dir = tempdir().unwrap();
    assert!(load_with_sources(args(&[]), &HashMap::new(), dir.path()).is_ok());
    assert!(
        load_with_sources(
            args(&["--config", "missing.toml"]),
            &HashMap::new(),
            dir.path()
        )
        .is_err()
    );
}
