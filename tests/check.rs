#![cfg(feature = "test-fixtures")]

//! End-to-end `--check` diagnostics: the real binary is run against the fake
//! CLI so the success and failure paths of `--check` (spec §13.1) are covered.

use std::process::Command;

fn check_command(extra_env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agently-cli-mcp"));
    cmd.args(["--check", "--cli", env!("CARGO_BIN_EXE_fake-agently-cli")]);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd
}

#[test]
fn check_runs_version_and_auth_status_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let argv_log = dir.path().join("argv.log");

    let output = check_command(&[
        ("FAKE_SCENARIO", "success_list"),
        ("FAKE_ARGV_LOG", argv_log.to_str().unwrap()),
    ])
    .output()
    .expect("failed to run agently-cli-mcp --check");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The fake CLI logs its argv (one JSON line per invocation). `--check`
    // must run exactly `--version` followed by `auth status`.
    let argv: Vec<Vec<String>> = std::fs::read_to_string(&argv_log)
        .expect("fake CLI should log argv")
        .lines()
        .map(|line| serde_json::from_str(line).expect("argv line must be JSON"))
        .collect();
    assert_eq!(
        argv,
        vec![
            vec!["--version".to_string()],
            vec!["auth".to_string(), "status".to_string()],
        ]
    );
}

#[test]
fn check_fails_when_version_command_fails() {
    let output = check_command(&[("FAKE_SCENARIO", "plain_error")])
        .output()
        .expect("failed to run agently-cli-mcp --check");

    assert!(
        !output.status.success(),
        "a failing --version must make --check exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--version"),
        "stderr should identify the failing command: {stderr}"
    );
}
