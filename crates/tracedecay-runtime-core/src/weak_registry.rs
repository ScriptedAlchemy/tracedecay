//! Generic weak-reference registry: `Mutex<HashMap<K, Weak<V>>>` with
//! poison-safe locking, retain-on-touch eviction, and get-or-insert
//! semantics.
//!
//! Consolidates the weak-registry pattern duplicated across the daemon, MCP
//! server, and core lifecycle modules: each site kept its own
//! `Mutex<Map<K, Weak<V>>>` with the same four moves --
//! `retain(|_, weak| weak.strong_count() > 0)`, `get(key).and_then(Weak::upgrade)`,
//! `insert(key, Arc::downgrade(value))`, and `lock().unwrap_or_else(PoisonError::into_inner)`
//! -- reimplemented by hand each time. This type owns only the weak-handle
//! mechanics; any extra per-entry bookkeeping a call site needs (hit/miss
//! counters, associated identity metadata, etc.) stays at the call site so
//! behavior at each converted site is unchanged.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

/// A `Mutex`-guarded map from `K` to `Weak<V>`, upgraded and swept lazily on
/// access.
pub struct WeakRegistry<K, V> {
    entries: Mutex<HashMap<K, Weak<V>>>,
}

impl<K, V> Default for WeakRegistry<K, V> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V> WeakRegistry<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, Weak<V>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drops entries whose last strong `Arc` has already gone away.
    pub fn retain_live(&self) {
        self.lock().retain(|_, weak| weak.strong_count() > 0);
    }

    /// Looks up `key` and upgrades it if still live. Does not sweep other
    /// entries and does not register anything on a miss.
    pub fn get_live(&self, key: &K) -> Option<Arc<V>> {
        self.lock().get(key).and_then(Weak::upgrade)
    }

    /// Registers `value` under `key`, replacing whatever was there.
    pub fn insert(&self, key: K, value: &Arc<V>) {
        self.lock().insert(key, Arc::downgrade(value));
    }

    /// Removes `key` only if it still points at `value` (by pointer
    /// identity), so a leader clearing its own registration never clobbers a
    /// newer registration that has since replaced it.
    pub fn remove_if_same(&self, key: &K, value: &Arc<V>) -> bool {
        let mut entries = self.lock();
        let still_registered = entries
            .get(key)
            .and_then(Weak::upgrade)
            .is_some_and(|live| Arc::ptr_eq(&live, value));
        if still_registered {
            entries.remove(key);
        }
        still_registered
    }

    /// Retains live entries, then returns the live `Arc` for `key` if one
    /// exists; otherwise inserts a fresh value built by `make` and returns
    /// it. The `bool` reports whether the entry was already live (`true`) or
    /// freshly inserted (`false`), so callers can distinguish a hit from a
    /// miss (e.g. leader/follower counters).
    pub fn get_or_insert_with(&self, key: K, make: impl FnOnce() -> Arc<V>) -> (Arc<V>, bool) {
        let mut entries = self.lock();
        entries.retain(|_, weak| weak.strong_count() > 0);
        if let Some(value) = entries.get(&key).and_then(Weak::upgrade) {
            return (value, true);
        }
        let value = make();
        entries.insert(key, Arc::downgrade(&value));
        (value, false)
    }

    /// Number of entries currently tracked, without sweeping dead ones.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Returns whether the registry currently tracks no entries, without
    /// sweeping dead ones.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}
