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

use crate::confirm::summary::SummaryAction;
use crate::error::AppError;

/// Default time-to-live for a pending confirmation.
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
/// Default maximum number of pending confirmations.
pub const DEFAULT_CAPACITY: usize = 100;

/// A pending confirmation: the request to replay once the human approves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub tool: String,
    pub action: SummaryAction,
    pub args_hash: String,
    pub canonical_args: Vec<String>,
    pub ctk: String,
    pub session_id: String,
    pub expires_at: Instant,
    pub body_temp: Option<PathBuf>,
    /// SHA-256 of the retained body file, verified before the confirmed replay.
    pub body_sha256: Option<[u8; 32]>,
    pub source_marked_read: bool,
}

/// A failed [`ConfirmStore::take_checked`]: the error plus any body file the
/// caller must delete (only set when the entry was actually consumed).
#[derive(Debug)]
pub struct TakeError {
    pub error: Box<AppError>,
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
    ///
    /// Any `body_temp` file belonging to an evicted entry is deleted (a missing
    /// file is ignored) so eviction cannot leak retained body files.
    pub fn insert(&self, key: String, mut pending: Pending) {
        let evicted = {
            let mut state = self.lock();
            let now = Instant::now();
            pending.expires_at = now + self.ttl;
            let mut evicted = Vec::new();

            state.entries.retain(|_, e| {
                if e.pending.expires_at > now {
                    true
                } else {
                    if let Some(path) = &e.pending.body_temp {
                        evicted.push(path.clone());
                    }
                    false
                }
            });
            if state.entries.len() >= self.capacity
                && let Some(lru) = state
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.seq)
                    .map(|(k, _)| k.clone())
                && let Some(entry) = state.entries.remove(&lru)
                && let Some(path) = &entry.pending.body_temp
            {
                evicted.push(path.clone());
            }

            state.next_seq += 1;
            let seq = state.next_seq;
            state.entries.insert(key, Entry { pending, seq });
            evicted
        };

        // Delete evicted body files outside the lock (ignore NotFound).
        for path in evicted {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Atomically remove and return the pending confirmation for `key`.
    ///
    /// This is single-use: a valid entry is consumed. A missing, expired, or
    /// wrong-session entry returns [`AppError::ConfirmationExpired`]; a
    /// wrong-session entry is left in place. Callers that need to delete the
    /// retained body file on an error should use [`ConfirmStore::take_checked`].
    pub fn take(&self, key: &str, session: &str) -> Result<Pending, AppError> {
        self.take_checked(key, session).map_err(|e| *e.error)
    }

    /// Atomically remove and return the pending confirmation for `key`,
    /// reporting any body file the caller must delete on error.
    ///
    /// * A valid entry is consumed and returned.
    /// * A missing key returns [`TakeError`] with `body_temp = None`.
    /// * An expired entry is consumed; its `body_temp` is returned so the
    ///   caller can delete it (it can no longer be reclaimed by
    ///   [`ConfirmStore::cleanup_expired`]).
    /// * A wrong-session entry is left untouched; `body_temp = None` so the
    ///   legitimate session's pending confirmation and body file survive.
    pub fn take_checked(&self, key: &str, session: &str) -> Result<Pending, TakeError> {
        let mut state = self.lock();
        let now = Instant::now();

        let snapshot = match state.entries.get(key) {
            None => {
                return Err(TakeError {
                    error: expired_error(key),
                    body_temp: None,
                });
            }
            Some(entry) => (
                entry.pending.expires_at <= now,
                entry.pending.body_temp.clone(),
                entry.pending.session_id != session,
            ),
        };
        let (expired, body_temp, wrong_session) = snapshot;

        if expired {
            state.entries.remove(key);
            return Err(TakeError {
                error: expired_error(key),
                body_temp,
            });
        }
        if wrong_session {
            return Err(TakeError {
                error: expired_error(key),
                body_temp: None,
            });
        }

        let entry = state.entries.remove(key).expect("entry was just observed");
        Ok(entry.pending)
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

fn expired_error(key: &str) -> Box<AppError> {
    Box::new(AppError::ConfirmationExpired(format!(
        "confirmation `{key}` is missing, expired, or belongs to another session"
    )))
}
