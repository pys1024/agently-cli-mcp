//! Pending-confirmation cache.
//!
//! Every confirmed write operation is two CLI invocations bound to a one-time
//! `ctk_` token. The first call returns the token, and the request is recorded
//! here keyed by `pending_id` (MRTR) or `ctk` (token mode); the second call
//! looks the record up, verifies the session, and replays the canonical
//! arguments. Entries are single-use, session-bound, and expire after a TTL
//! (default 300s); the cache is LRU-evicted at a fixed capacity (default 100)
//! and lives entirely in memory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::AppError;

/// Default time-to-live for a pending confirmation.
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
/// Default maximum number of pending confirmations.
pub const DEFAULT_CAPACITY: usize = 100;

/// A pending confirmation: the request to replay once the human approves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub tool: String,
    pub args_hash: String,
    pub canonical_args: Vec<String>,
    pub ctk: String,
    pub session_id: String,
    pub expires_at: Instant,
    pub body_temp: Option<PathBuf>,
}

#[derive(Debug)]
struct Entry {
    pending: Pending,
    /// Monotonic last-access stamp used for LRU eviction.
    seq: u64,
}

#[derive(Default, Debug)]
struct State {
    entries: HashMap<String, Entry>,
    next_seq: u64,
}

/// An in-memory pending-confirmation store.
#[derive(Debug)]
pub struct ConfirmStore {
    state: Mutex<State>,
    ttl: Duration,
    capacity: usize,
}

impl Default for ConfirmStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfirmStore {
    /// Build a store with the default TTL (300s) and capacity (100).
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

    /// Bind `key` to a pending confirmation, evicting expired and
    /// least-recently-used entries as needed. The store recomputes
    /// `pending.expires_at` from its own TTL so a stale caller-supplied value
    /// can never outlive the store.
    pub fn insert(&self, key: String, mut pending: Pending) {
        let mut state = self.lock();
        let now = Instant::now();
        pending.expires_at = now + self.ttl;

        state.entries.retain(|_, e| e.pending.expires_at > now);
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
        state.entries.insert(key, Entry { pending, seq });
    }

    /// Atomically remove and return the pending confirmation for `key`.
    ///
    /// This is single-use: the entry is consumed whether or not it is valid.
    /// A missing, expired, or wrong-session entry returns
    /// [`AppError::ConfirmationExpired`].
    pub fn take(&self, key: &str, session: &str) -> Result<Pending, AppError> {
        let mut state = self.lock();
        let now = Instant::now();

        match state.entries.remove(key) {
            Some(entry)
                if entry.pending.expires_at > now && entry.pending.session_id == session =>
            {
                Ok(entry.pending)
            }
            _ => Err(AppError::ConfirmationExpired(format!(
                "confirmation `{key}` is missing, expired, or belongs to another session"
            ))),
        }
    }

    /// Remove and return the pending confirmation for `key`, if any. The
    /// caller is responsible for deleting the returned `body_temp` file.
    pub fn remove(&self, key: &str) -> Option<Pending> {
        let mut state = self.lock();
        state.entries.remove(key).map(|entry| entry.pending)
    }

    /// Remove every expired entry and return the `body_temp` paths that the
    /// caller must delete.
    pub fn cleanup_expired(&self) -> Vec<PathBuf> {
        let mut state = self.lock();
        let now = Instant::now();
        let mut paths = Vec::new();

        state.entries.retain(|_, entry| {
            if entry.pending.expires_at > now {
                true
            } else {
                if let Some(path) = &entry.pending.body_temp {
                    paths.push(path.clone());
                }
                false
            }
        });

        paths
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
