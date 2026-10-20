//! Pending-confirmation store tests.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agently_cli_mcp::confirm::{ConfirmStore, Pending};

fn pending(session: &str) -> Pending {
    Pending {
        tool: "mail_send_message".to_string(),
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
