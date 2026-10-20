#![cfg(feature = "test-fixtures")]

use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[cfg(unix)]
use agently_cli_mcp::cli_proc::runner::Runner;
#[cfg(unix)]
use agently_cli_mcp::config::CliConfig;

/// Serializes access to the process-global `FAKE_SCENARIO` / `FAKE_ARGV_LOG` /
/// `FAKE_GRANDCHILD_PID` environment variables, which the fake CLI reads at
/// spawn time.
#[cfg(unix)]
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-agently-cli")
}

#[cfg(unix)]
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

/// Configure the fake CLI for the `spawn_child` scenario, recording the
/// grandchild PID to `grandchild_pid`.
#[cfg(unix)]
fn set_spawn_child(argv_log: &Path, grandchild_pid: &Path) {
    unsafe {
        std::env::set_var("FAKE_SCENARIO", "spawn_child");
        std::env::set_var("FAKE_ARGV_LOG", argv_log);
        std::env::set_var("FAKE_GRANDCHILD_PID", grandchild_pid);
        std::env::remove_var("FAKE_SPAWN_GO");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_grandchild() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let pid_file = dir.path().join("grandchild.pid");
    set_spawn_child(&log, &pid_file);

    let runner = Runner::new(&cli_config(fake_bin())).unwrap();
    let err = runner
        .run(&[], Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "TIMEOUT");

    // The fake CLI recorded the grandchild PID before it was killed.
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("grandchild PID must be written")
        .trim()
        .parse()
        .expect("grandchild PID must be an integer");

    // The whole process tree must be gone: kill(pid, 0) returns ESRCH.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "grandchild {pid} survived the timeout"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
#[test]
fn job_object_kills_process_tree() {
    use agently_cli_mcp::cli_proc::process::ProcessTree;

    let dir = tempdir().unwrap();
    let pid_file = dir.path().join("grandchild.pid");
    let go_file = dir.path().join("go");

    let child = tokio::process::Command::new(fake_bin())
        .env("FAKE_SCENARIO", "spawn_child")
        .env("FAKE_GRANDCHILD_PID", &pid_file)
        .env("FAKE_SPAWN_GO", &go_file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn fake CLI");

    let parent_pid = child.id().expect("parent pid");

    // Attach the child to a Job Object *before* it spawns the grandchild, so
    // the grandchild inherits job membership and is killed with its parent.
    let mut tree = ProcessTree::attach(&child).expect("attach child to job");

    // Let the child spawn the grandchild now that the job is attached.
    std::fs::write(&go_file, "").expect("write go file");

    let grandchild_pid = wait_for_pid_file(&pid_file, Duration::from_secs(5));

    tree.terminate();

    assert!(
        wait_until_dead(parent_pid, Duration::from_secs(5)),
        "parent {parent_pid} survived the Job Object termination"
    );
    assert!(
        wait_until_dead(grandchild_pid, Duration::from_secs(5)),
        "grandchild {grandchild_pid} survived the Job Object termination"
    );
}

#[cfg(windows)]
fn wait_for_pid_file(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(pid) = content.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "grandchild PID was never reported"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const STILL_ACTIVE: u32 = 259;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(windows)]
fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
