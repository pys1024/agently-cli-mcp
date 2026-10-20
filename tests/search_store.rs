//! Search cursor context store tests.

use agently_cli_mcp::search::{SearchCtx, SearchQuery, SearchStore};
use std::time::Duration;

fn ctx(q: &str, from: &str, session: &str) -> SearchCtx {
    SearchCtx {
        q: Some(q.to_string()),
        search_in: None,
        from: Some(from.to_string()),
        to: None,
        dir: None,
        after: None,
        before: None,
        has_attachments: false,
        is_unread: false,
        limit: 0,
        session_id: session.to_string(),
    }
}

fn query(q: &str, from: &str) -> SearchQuery {
    SearchQuery {
        q: Some(q.to_string()),
        search_in: None,
        from: Some(from.to_string()),
        to: None,
        dir: None,
        after: None,
        before: None,
        has_attachments: false,
        is_unread: false,
        limit: 0,
    }
}

#[test]
fn cursor_roundtrip_restores_query() {
    let store = SearchStore::new();
    store.remember("cur_1", ctx("needle", "a@example.com", "sess"), "sess");

    let resolved = store
        .resolve("cur_1", &SearchQuery::default(), "sess")
        .unwrap();
    assert_eq!(resolved.q.as_deref(), Some("needle"));
    assert_eq!(resolved.from.as_deref(), Some("a@example.com"));
}

#[test]
fn changed_filter_mismatches() {
    let store = SearchStore::new();
    store.remember("cur_1", ctx("needle", "a@example.com", "sess"), "sess");

    let err = store
        .resolve("cur_1", &query("different", "a@example.com"), "sess")
        .unwrap_err();
    assert_eq!(err.code(), "SEARCH_CONTEXT_MISMATCH");
}

#[test]
fn expired_context_requires_query() {
    let store = SearchStore::with_ttl(Duration::from_millis(1));
    store.remember("cur_1", ctx("needle", "a@example.com", "sess"), "sess");
    std::thread::sleep(Duration::from_millis(10));

    let err = store
        .resolve("cur_1", &SearchQuery::default(), "sess")
        .unwrap_err();
    assert_eq!(err.code(), "SEARCH_CONTEXT_REQUIRED");
}

#[test]
fn missing_context_with_full_query_returns_provided() {
    let store = SearchStore::new();
    let provided = query("fresh", "b@example.com");

    let resolved = store.resolve("unknown", &provided, "sess").unwrap();
    assert_eq!(resolved, provided);
}

#[test]
fn session_mismatch_requires_query() {
    let store = SearchStore::new();
    store.remember("cur_1", ctx("needle", "a@example.com", "sess"), "sess");

    let err = store
        .resolve("cur_1", &SearchQuery::default(), "other")
        .unwrap_err();
    assert_eq!(err.code(), "SEARCH_CONTEXT_REQUIRED");
}
