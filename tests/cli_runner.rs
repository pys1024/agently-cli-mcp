#![cfg(feature = "test-fixtures")]

use agently_cli_mcp::cli_proc::CliOutcome;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config::CliConfig;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

/// Serializes access to the process-global `FAKE_SCENARIO` / `FAKE_ARGV_LOG`
/// environment variables, which the fake CLI reads at spawn time.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cli_config(bin: &str) -> CliConfig {
    CliConfig {
        path: Some(bin.to_string()),
        workdir: None,
        max_concurrency: 1,
        timeout_secs: 30,
        read_timeout_secs: 60,
        send_timeout_secs: 60,
        watch_max_secs: 120,
    }
}

fn set_scenario(scenario: &str, argv_log: &Path) {
    unsafe {
        std::env::set_var("FAKE_SCENARIO", scenario);
        std::env::set_var("FAKE_ARGV_LOG", argv_log);
    }
}

fn fake_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-agently-cli")
}

#[tokio::test]
async fn captures_argv() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    let log = dir.path().join("argv.log");
    set_scenario("success_list", &log);

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();
    let args = vec![
        "message".to_string(),
        "+list".to_string(),
        "--limit".to_string(),
        "5".to_string(),
    ];
    runner.run(&args, Duration::from_secs(5)).await.unwrap();

    let logged: Vec<Vec<String>> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(logged, vec![args]);
}

#[tokio::test]
async fn success_envelope_is_parsed() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    set_scenario("success_list", &dir.path().join("argv.log"));

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();
    match runner.run_json(&[], Duration::from_secs(5)).await.unwrap() {
        CliOutcome::Success(data) => {
            assert_eq!(data["messages"][0]["message_id"], "msg_1");
        }
        _ => panic!("expected success"),
    }
}

#[tokio::test]
async fn exit_one_maps_to_invalid_argument() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    set_scenario("plain_error", &dir.path().join("argv.log"));

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();
    let err = runner
        .run_json(&[], Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.to_string().contains("--limit"));
}

#[tokio::test]
async fn timeout_is_reported_and_kills_process() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    set_scenario("sleep", &dir.path().join("argv.log"));

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();
    let err = runner
        .run_json(&[], Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "TIMEOUT");
}

#[tokio::test]
async fn concurrent_calls_are_serialized() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let probe = dir.path().join("probe.log");
    set_scenario("sleep_brief", &log);
    unsafe {
        std::env::set_var("FAKE_PROBE_LOG", &probe);
    }

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();

    // `max_concurrency` is 1, so the two calls must not overlap. The fake CLI
    // writes a deterministic START/END marker pair around a ~300ms sleep.
    let args_a = vec!["call-a".to_string()];
    let args_b = vec!["call-b".to_string()];
    let (a, b) = tokio::join!(
        runner.run_json(&args_a, Duration::from_secs(5)),
        runner.run_json(&args_b, Duration::from_secs(5)),
    );
    a.unwrap();
    b.unwrap();

    let markers: Vec<String> = std::fs::read_to_string(&probe)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        markers,
        vec!["START", "END", "START", "END"],
        "serialized calls must not overlap (got {markers:?})"
    );
}
