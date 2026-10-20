//! Fake `agently-cli` binary used by integration tests.
//!
//! The scenario is selected by the `FAKE_SCENARIO` environment variable. The
//! arguments (everything after the program name) are appended as one JSON line
//! to the file named by `FAKE_ARGV_LOG`, so tests can assert the exact argv the
//! runner passed without shell interpolation.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::exit;
use std::thread;
use std::time::Duration;

const CONFIRM_ENVELOPE: &str = r#"{"ok":true,"data":{"confirmation_required":true,"confirmation_token":"ctk_test","summary":{"action":"send"}}}"#;
const SUCCESS_LIST: &str =
    r#"{"ok":true,"data":{"messages":[{"message_id":"msg_1"},{"message_id":"msg_2"}]}}"#;
const SEARCH_PAGE: &str =
    r#"{"ok":true,"data":{"messages":[{"message_id":"msg_1"}],"next_cursor":"cur_1"}}"#;
const QUEUED: &str = r#"{"ok":true,"data":{"queued":true}}"#;
const ERROR_404: &str =
    r#"{"ok":false,"error":{"type":"api_error","code":404,"message":"Resource not found"}}"#;
const EVENT_1: &str = r#"{"event":"arrived","id":"msg_1"}"#;
const EVENT_2: &str = r#"{"event":"arrived","id":"msg_2"}"#;

/// Poll until `path` exists, giving up after a few seconds.
fn wait_for_file(path: &str) {
    let path = std::path::Path::new(path);
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn main() {
    let scenario = env::var("FAKE_SCENARIO").unwrap_or_else(|_| "success_list".to_string());

    // Log argv as one JSON line.
    let argv: Vec<String> = env::args().skip(1).collect();
    if let Ok(log_path) = env::var("FAKE_ARGV_LOG")
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path)
    {
        let _ = writeln!(file, "{}", serde_json::to_string(&argv).unwrap());
        let _ = file.flush();
    }

    match scenario.as_str() {
        "success_list" => {
            println!("{SUCCESS_LIST}");
            exit(0);
        }
        "success_read" => {
            // A long body so tests can exercise `max_body_chars` truncation.
            let body = "x".repeat(4096);
            println!(r#"{{"ok":true,"data":{{"id":"msg_1","subject":"Hello","body":"{body}"}}}}"#);
            exit(0);
        }
        "search_page" => {
            println!("{SEARCH_PAGE}");
            exit(0);
        }
        "confirm_send" => {
            println!("{CONFIRM_ENVELOPE}");
            exit(0);
        }
        "confirm_then_success" => {
            if argv.iter().any(|a| a == "--confirmation-token") {
                println!("{QUEUED}");
            } else {
                println!("{CONFIRM_ENVELOPE}");
            }
            exit(0);
        }
        "error_404" => {
            println!("{ERROR_404}");
            exit(6);
        }
        "plain_error" => {
            eprintln!("--limit must be between 1 and 50");
            exit(1);
        }
        "non_json" => {
            println!("not json");
            exit(0);
        }
        "sleep" => {
            thread::sleep(Duration::from_secs(30));
            exit(0);
        }
        "sleep_brief" => {
            // Deterministic overlap probe: append START, sleep ~300ms, append
            // END. Serialized calls produce START,END,START,END; overlapping
            // calls produce START,START,END,END.
            if let Ok(log) = env::var("FAKE_PROBE_LOG")
                && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log)
            {
                let _ = writeln!(f, "START");
                let _ = f.flush();
            }
            thread::sleep(Duration::from_millis(300));
            if let Ok(log) = env::var("FAKE_PROBE_LOG")
                && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log)
            {
                let _ = writeln!(f, "END");
                let _ = f.flush();
            }
            println!("{SUCCESS_LIST}");
            exit(0);
        }
        "spawn_child" => {
            // Optional coordination for platforms that must attach the child
            // to a Job Object *after* spawn (Windows): wait for a "go" file so
            // the grandchild is guaranteed to be created inside the job.
            if let Ok(go) = env::var("FAKE_SPAWN_GO") {
                wait_for_file(&go);
            }
            let exe = env::current_exe().expect("current exe");
            let child = std::process::Command::new(exe)
                .env("FAKE_SCENARIO", "sleep")
                .env_remove("FAKE_ARGV_LOG")
                .spawn()
                .expect("spawn grandchild");
            let pid = child.id();
            if let Ok(path) = env::var("FAKE_GRANDCHILD_PID") {
                let _ = std::fs::write(&path, pid.to_string());
            }
            println!("{pid}");
            let _ = std::io::stdout().flush();
            thread::sleep(Duration::from_secs(30));
            exit(0);
        }
        "ndjson_events" => {
            println!("{EVENT_1}");
            println!("{EVENT_2}");
            let _ = std::io::stdout().flush();
            thread::sleep(Duration::from_secs(30));
            exit(0);
        }
        other => {
            eprintln!("unknown FAKE_SCENARIO: {other}");
            exit(2);
        }
    }
}
