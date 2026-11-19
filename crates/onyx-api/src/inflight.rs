//! Requests in flight per client (THREATS NL5-3).
//!
//! The guard's one semaphore bounds the requests in flight for the whole
//! server. Behind a trusted proxy every client arrives from the proxy's
//! address, so nothing in the server loop keeps one client from taking all
//! of those permits. This table counts the requests in flight per client key
//! (the forwarded address, as the per-client request limit reads it) and per
//! wider IPv6 scope ([`crate::inflight_scopes`]), and refuses a request whose
//! client already holds its share.
//!
//! A place in the table exists only inside a [`Held`], next to the
//! server-wide permit the request took first, and is given back before that
//! permit. So the table never holds more entries than three scopes for each
//! permit (`3 * max_concurrency`), whatever addresses the requests name; an
//! entry is removed when its count returns to zero, and a refused request
//! adds none. The lock is a plain mutex held for a few map operations,
//! never across an await.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::OwnedSemaphorePermit;

/// Requests in flight per scope key. Only scopes with a count above zero
/// have an entry.
#[derive(Default)]
pub(crate) struct ClientInflight {
    counts: Mutex<HashMap<String, u32>>,
}

/// The client already holds its share of the requests in flight.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OverShare;

impl ClientInflight {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, u32>> {
        // A poisoned lock still counts: the map is consistent after every
        // single operation on it.
        self.counts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Scopes with a request in flight.
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    /// `permit` plus one place in each of `scopes` (key and cap), or
    /// [`OverShare`] when any scope is at its cap: then nothing is counted
    /// and `permit` is given back. No scopes: the request is not counted per
    /// client.
    pub(crate) fn hold(
        self: &Arc<Self>,
        permit: OwnedSemaphorePermit,
        scopes: Vec<(String, u32)>,
    ) -> Result<Held, OverShare> {
        if scopes.is_empty() {
            return Ok(Held {
                _client: None,
                _permit: permit,
            });
        }
        let mut counts = self.lock();
        if scopes
            .iter()
            .any(|(k, cap)| counts.get(k).copied().unwrap_or(0) >= *cap)
        {
            return Err(OverShare);
        }
        let mut keys = Vec::with_capacity(scopes.len());
        for (k, _) in scopes {
            match counts.get_mut(&k) {
                Some(n) => *n += 1,
                None => {
                    counts.insert(k.clone(), 1);
                }
            }
            keys.push(k);
        }
        drop(counts);
        Ok(Held {
            _client: Some(ClientSlot {
                table: self.clone(),
                keys,
            }),
            _permit: permit,
        })
    }
}

/// One request's places in the table; dropping it gives them back and
/// removes every entry that reaches zero.
struct ClientSlot {
    table: Arc<ClientInflight>,
    keys: Vec<String>,
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        let mut counts = self.table.lock();
        for k in &self.keys {
            if let Some(n) = counts.get_mut(k) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    counts.remove(k);
                }
            }
        }
    }
}

/// What one request in flight holds: its client's places (none for a caller
/// that is not counted) and the server-wide permit. Dropped as one, the
/// client's places first (fields drop in this order), so a place in the
/// table never outlives its permit.
pub(crate) struct Held {
    _client: Option<ClientSlot>,
    _permit: OwnedSemaphorePermit,
}
