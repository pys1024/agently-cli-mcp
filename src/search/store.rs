//! Cursor → search-filter cache.
//!
//! `+search` cursors do not carry their query; the CLI requires callers to
//! repeat `--q` and every filter on each page. This store remembers the query
//! that produced each cursor so a follow-up carrying only the cursor can
//! restore it. Entries are session-bound and expire after a TTL (default 600s);
//! the cache is LRU-evicted at a fixed capacity (default 200).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::AppError;

/// Default time-to-live for a stored cursor context.
pub const DEFAULT_TTL: Duration = Duration::from_secs(600);
/// Default maximum number of stored cursor contexts.
pub const DEFAULT_CAPACITY: usize = 200;

/// The filter set for a `mail_search_messages` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub search_in: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub dir: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub has_attachments: bool,
    pub is_unread: bool,
    pub limit: u32,
}

impl SearchQuery {
    /// Whether no filter was supplied (a call carrying only a cursor).
    pub fn is_empty(&self) -> bool {
        self.q.is_none()
            && self.search_in.is_none()
            && self.from.is_none()
            && self.to.is_none()
            && self.dir.is_none()
            && self.after.is_none()
            && self.before.is_none()
            && !self.has_attachments
            && !self.is_unread
            && self.limit == 0
    }
}

/// A stored search context: the query plus the session that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCtx {
    pub q: Option<String>,
    pub search_in: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub dir: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub has_attachments: bool,
    pub is_unread: bool,
    pub limit: u32,
    pub session_id: String,
}

impl SearchCtx {
    /// The query part of this context, without the session.
    pub fn query(&self) -> SearchQuery {
        SearchQuery {
            q: self.q.clone(),
            search_in: self.search_in.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            dir: self.dir.clone(),
            after: self.after.clone(),
            before: self.before.clone(),
            has_attachments: self.has_attachments,
            is_unread: self.is_unread,
            limit: self.limit,
        }
    }
}

#[derive(Debug)]
struct Entry {
    ctx: SearchCtx,
    expires_at: Instant,
    /// Monotonic last-access stamp used for LRU eviction.
    seq: u64,
}

#[derive(Default, Debug)]
struct State {
    entries: HashMap<String, Entry>,
    next_seq: u64,
}

impl State {
    fn touch(&mut self, cursor: &str) {
        self.next_seq += 1;
        if let Some(entry) = self.entries.get_mut(cursor) {
            entry.seq = self.next_seq;
        }
    }
}

/// An in-memory cursor-context store.
#[derive(Debug)]
pub struct SearchStore {
    state: Mutex<State>,
    ttl: Duration,
    capacity: usize,
}

impl Default for SearchStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchStore {
    /// Build a store with the default TTL (600s) and capacity (200).
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            ttl: DEFAULT_TTL,
            capacity: DEFAULT_CAPACITY,
        }
    }

    /// Build a store with a custom TTL (used by tests and future tuning).
    pub fn with_ttl(ttl: Duration) -> Self {
        Self { ttl, ..Self::new() }
    }

    /// Build a store with a custom capacity (used by tests and future tuning).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::new()
        }
    }

    /// Bind `cursor` to the search context for `session`, evicting expired and
    /// least-recently-used entries as needed.
    pub fn remember(&self, cursor: &str, ctx: SearchCtx, session: &str) {
        let mut state = self.lock();
        let now = Instant::now();

        let mut ctx = ctx;
        ctx.session_id = session.to_string();

        state.entries.retain(|_, e| e.expires_at > now);
        if state.entries.len() >= self.capacity
            && let Some(lru) = state
                .entries
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone())
        {
            state.entries.remove(&lru);
        }

        state.next_seq += 1;
        let seq = state.next_seq;
        state.entries.insert(
            cursor.to_string(),
            Entry {
                ctx,
                expires_at: now + self.ttl,
                seq,
            },
        );
    }

    /// Resolve a cursor back into a query.
    ///
    /// * If the cursor maps to a live, session-matching context and `provided`
    ///   is empty, the stored query is restored.
    /// * If `provided` supplies filters that conflict with the stored query,
    ///   [`AppError::SearchContextMismatch`] is returned.
    /// * If there is no live context and `provided` is empty,
    ///   [`AppError::SearchContextRequired`] is returned; otherwise `provided`
    ///   is returned unchanged (a fresh search).
    pub fn resolve(
        &self,
        cursor: &str,
        provided: &SearchQuery,
        session: &str,
    ) -> Result<SearchQuery, AppError> {
        let mut state = self.lock();
        let now = Instant::now();

        let stored = match state.entries.get(cursor) {
            Some(entry) if entry.expires_at > now && entry.ctx.session_id == session => {
                Some(entry.ctx.clone())
            }
            _ => None,
        };

        match stored {
            Some(ctx) => {
                if provided.is_empty() {
                    state.touch(cursor);
                    Ok(ctx.query())
                } else if has_conflict(provided, &ctx) {
                    Err(AppError::SearchContextMismatch(format!(
                        "search cursor `{cursor}` filters conflict with the stored query"
                    )))
                } else {
                    state.touch(cursor);
                    Ok(ctx.query())
                }
            }
            None => {
                if provided.is_empty() {
                    Err(AppError::SearchContextRequired(format!(
                        "search cursor `{cursor}` context is missing or expired; provide the full query"
                    )))
                } else {
                    Ok(provided.clone())
                }
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Whether `provided` carries a filter that differs from `stored`. Only fields
/// that are actually supplied are compared; a default (`None`/`false`/`0`)
/// value is treated as "not supplied".
fn has_conflict(provided: &SearchQuery, stored: &SearchCtx) -> bool {
    (provided.q.is_some() && provided.q != stored.q)
        || (provided.search_in.is_some() && provided.search_in != stored.search_in)
        || (provided.from.is_some() && provided.from != stored.from)
        || (provided.to.is_some() && provided.to != stored.to)
        || (provided.dir.is_some() && provided.dir != stored.dir)
        || (provided.after.is_some() && provided.after != stored.after)
        || (provided.before.is_some() && provided.before != stored.before)
        || (provided.has_attachments && !stored.has_attachments)
        || (provided.is_unread && !stored.is_unread)
        || (provided.limit != 0 && provided.limit != stored.limit)
}
