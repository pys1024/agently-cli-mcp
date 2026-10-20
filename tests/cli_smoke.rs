use std::process::Command;

#[test]
fn print_config_shows_transport_and_redacts_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_agently-cli-mcp"))
        .args(["--print-config", "--http-token", "supersecret"])
        .output()
        .expect("failed to run agently-cli-mcp");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be JSON");

    assert_eq!(value["transport"], "stdio");
    assert!(
        !stdout.contains("supersecret"),
        "token leaked into --print-config output"
    );
}
