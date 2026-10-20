//! Pending-confirmation store tests.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agently_cli_mcp::confirm::summary::SummaryAction;
use agently_cli_mcp::confirm::{ConfirmStore, Pending};

fn pending(session: &str) -> Pending {
    Pending {
        tool: "mail_send_message".to_string(),
        action: SummaryAction::Send,
        args_hash: "deadbeef".to_string(),
        canonical_args: vec![
            "message".to_string(),
            "+send".to_string(),
            "--to".to_string(),
            "a@example.com".to_string(),
        ],
        ctk: "ctk_test".to_string(),
        session_id: session.to_string(),
        // The store recomputes `expires_at` from its TTL on insert.
        expires_at: Instant::now(),
        body_temp: None,
        body_sha256: None,
        source_marked_read: false,
    }
}

#[test]
fn take_is_single_use() {
    let store = ConfirmStore::new();
    store.insert("ctk_1".to_string(), pending("sess"));

    assert!(store.take("ctk_1", "sess").is_ok());

    let err = store.take("ctk_1", "sess").unwrap_err();
    assert_eq!(err.code(), "CONFIRMATION_EXPIRED");
}

#[test]
fn session_mismatch_is_rejected() {
    let store = ConfirmStore::new();
    store.insert("ctk_1".to_string(), pending("sess"));

    let err = store.take("ctk_1", "other").unwrap_err();
    assert_eq!(err.code(), "CONFIRMATION_EXPIRED");
}

#[test]
fn expired_pending_is_rejected() {
    let store = ConfirmStore::with_ttl(Duration::from_millis(1));
    store.insert("ctk_1".to_string(), pending("sess"));
    std::thread::sleep(Duration::from_millis(10));

    let err = store.take("ctk_1", "sess").unwrap_err();
    assert_eq!(err.code(), "CONFIRMATION_EXPIRED");
}

#[test]
fn missing_key_is_rejected() {
    let store = ConfirmStore::new();

    let err = store.take("unknown", "sess").unwrap_err();
    assert_eq!(err.code(), "CONFIRMATION_EXPIRED");
}

#[test]
fn remove_returns_and_drops_the_entry() {
    let store = ConfirmStore::new();
    store.insert("ctk_1".to_string(), pending("sess"));

    let removed = store.remove("ctk_1");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().session_id, "sess");

    assert!(store.remove("ctk_1").is_none());
}

#[test]
fn cleanup_expired_returns_body_temp_paths() {
    let store = ConfirmStore::with_ttl(Duration::from_millis(1));

    let mut with_body = pending("sess");
    with_body.body_temp = Some(PathBuf::from("tmp/body.txt"));
    store.insert("ctk_body".to_string(), with_body);

    let without_body = pending("sess");
    store.insert("ctk_none".to_string(), without_body);

    std::thread::sleep(Duration::from_millis(10));

    let paths = store.cleanup_expired();
    assert_eq!(paths, vec![PathBuf::from("tmp/body.txt")]);
    assert!(store.take("ctk_body", "sess").is_err());
    assert!(store.take("ctk_none", "sess").is_err());
}

#[test]
fn capacity_evicts_least_recently_used() {
    let store = ConfirmStore::with_capacity(2);
    store.insert("k1".to_string(), pending("sess"));
    store.insert("k2".to_string(), pending("sess"));
    store.insert("k3".to_string(), pending("sess"));

    // k1 was inserted first and never accessed, so it is evicted.
    assert!(store.take("k1", "sess").is_err());
    assert!(store.take("k2", "sess").is_ok());
    assert!(store.take("k3", "sess").is_ok());
}

#[test]
fn insert_at_capacity_deletes_evicted_body_file() {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("evicted.txt");
    std::fs::write(&body, b"x").unwrap();

    let store = ConfirmStore::with_capacity(2);

    let mut evicted = pending("sess");
    evicted.body_temp = Some(body.clone());
    store.insert("k1".to_string(), evicted);
    store.insert("k2".to_string(), pending("sess"));
    store.insert("k3".to_string(), pending("sess"));

    // k1 is the least recently used and is evicted; its body file must go too.
    assert!(!body.exists(), "evicted body file must be deleted");
    assert!(store.take("k1", "sess").is_err());
}

#[test]
fn cleanup_expired_body_file_is_removed_by_caller() {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("expired.txt");
    std::fs::write(&body, b"x").unwrap();

    let store = ConfirmStore::with_ttl(Duration::from_millis(1));
    let mut p = pending("sess");
    p.body_temp = Some(body.clone());
    store.insert("ctk_body".to_string(), p);

    std::thread::sleep(Duration::from_millis(10));

    // The janitor calls `cleanup_expired` and deletes each returned path.
    let paths = store.cleanup_expired();
    assert_eq!(paths, vec![body.clone()]);
    for path in paths {
        std::fs::remove_file(path).unwrap();
    }
    assert!(!body.exists(), "expired body file must be removed");
    assert!(store.take("ctk_body", "sess").is_err());
}

#[test]
fn take_checked_wrong_session_does_not_consume() {
    let store = ConfirmStore::new();
    let mut with_body = pending("sess");
    with_body.body_temp = Some(PathBuf::from("tmp/body.txt"));
    store.insert("ctk_1".to_string(), with_body);

    let err = store.take_checked("ctk_1", "other").unwrap_err();
    assert_eq!(err.error.code(), "CONFIRMATION_EXPIRED");
    // Wrong-session must not delete the legitimate session's body file.
    assert_eq!(err.body_temp, None);

    // The legitimate entry must survive for its own session.
    let taken = store.take_checked("ctk_1", "sess").unwrap();
    assert_eq!(taken.session_id, "sess");
}

#[test]
fn take_checked_expired_returns_body_temp() {
    let store = ConfirmStore::with_ttl(Duration::from_millis(1));
    let mut with_body = pending("sess");
    with_body.body_temp = Some(PathBuf::from("tmp/body.txt"));
    store.insert("ctk_1".to_string(), with_body);
    std::thread::sleep(Duration::from_millis(10));

    let err = store.take_checked("ctk_1", "sess").unwrap_err();
    assert_eq!(err.error.code(), "CONFIRMATION_EXPIRED");
    // The expired entry was consumed, so its body file path is reported.
    assert_eq!(err.body_temp, Some(PathBuf::from("tmp/body.txt")));

    // It is single-use: a second take finds nothing and reports no body file.
    let again = store.take_checked("ctk_1", "sess").unwrap_err();
    assert_eq!(again.body_temp, None);
}

#[test]
fn take_checked_missing_reports_no_body_temp() {
    let store = ConfirmStore::new();
    let err = store.take_checked("unknown", "sess").unwrap_err();
    assert_eq!(err.error.code(), "CONFIRMATION_EXPIRED");
    assert_eq!(err.body_temp, None);
}
