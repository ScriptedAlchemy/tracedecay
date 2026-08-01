//! Generic weak-reference registry: `Mutex<HashMap<K, Weak<V>>>` with
//! poison-safe locking, retain-on-touch eviction, and get-or-insert
//! semantics.
//!
//! Consolidates the weak-registry pattern duplicated across the daemon, MCP
//! server, and core lifecycle modules (audit finding F6): each site kept its
//! own `Mutex<Map<K, Weak<V>>>` with the same four moves --
//! `retain(|_, weak| weak.strong_count() > 0)`, `get(key).and_then(Weak::upgrade)`,
//! `insert(key, Arc::downgrade(value))`, and `lock().unwrap_or_else(PoisonError::into_inner)`
//! -- reimplemented by hand each time. This type owns only the weak-handle
//! mechanics; any extra per-entry bookkeeping a call site needs (hit/miss
//! counters, associated identity metadata, etc.) stays at the call site so
//! behavior at each converted site is unchanged.
//!
//! `M` is optional per-entry metadata stored alongside the `Weak<V>` (default
//! `()` for sites that don't need any). Methods that ignore metadata are
//! available regardless of `M`; methods that produce or consume metadata are
//! suffixed `_with_meta`.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

struct WeakEntry<M, V> {
    meta: M,
    handle: Weak<V>,
}

/// A `Mutex`-guarded map from `K` to `Weak<V>` (plus optional metadata `M`),
/// upgraded and swept lazily on access.
pub(crate) struct WeakRegistry<K, V, M = ()> {
    entries: Mutex<HashMap<K, WeakEntry<M, V>>>,
}

impl<K, V, M> Default for WeakRegistry<K, V, M> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

// Not every method here is reachable from every feature/cfg combination a
// caller may build with (e.g. `standalone_maintenance_scope` is only
// compiled without `test-transport`, and the raw-membership check is
// `#[cfg(test)]`-only) -- this is a small shared primitive, not a
// single-purpose helper, so allow methods that are genuinely used by *some*
// call site to go unused in any one configuration.
#[allow(dead_code)]
impl<K: Eq + Hash, V, M> WeakRegistry<K, V, M> {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, WeakEntry<M, V>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drops entries whose last strong `Arc` has already gone away.
    pub(crate) fn retain_live(&self) {
        self.lock()
            .retain(|_, entry| entry.handle.strong_count() > 0);
    }

    /// Looks up `key` and upgrades it if still live. Does not sweep other
    /// entries and does not register anything on a miss.
    pub(crate) fn get_live(&self, key: &K) -> Option<Arc<V>> {
        self.lock()
            .get(key)
            .and_then(|entry| entry.handle.upgrade())
    }

    /// Like [`Self::get_live`] but also returns a clone of the entry's
    /// metadata.
    pub(crate) fn get_live_with_meta(&self, key: &K) -> Option<(M, Arc<V>)>
    where
        M: Clone,
    {
        let entries = self.lock();
        let entry = entries.get(key)?;
        let value = entry.handle.upgrade()?;
        Some((entry.meta.clone(), value))
    }

    /// Reports whether `key` is present in the registry, without upgrading
    /// its handle. Distinct from [`Self::get_live`]: a dead entry that has
    /// not yet been swept by [`Self::retain_live`] is still "present" here,
    /// so retain-driven eviction stays observable separately from a value
    /// that merely died.
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.lock().contains_key(key)
    }

    /// Registers `value` under `key`, replacing whatever was there.
    pub(crate) fn insert(&self, key: K, value: &Arc<V>)
    where
        M: Default,
    {
        self.insert_with_meta(key, M::default(), value);
    }

    /// Registers `value` and its metadata under `key`, replacing whatever
    /// was there.
    pub(crate) fn insert_with_meta(&self, key: K, meta: M, value: &Arc<V>) {
        self.lock().insert(
            key,
            WeakEntry {
                meta,
                handle: Arc::downgrade(value),
            },
        );
    }

    /// Removes `key` only if it still points at `value` (by pointer
    /// identity), so a leader clearing its own registration never clobbers a
    /// newer registration that has since replaced it.
    pub(crate) fn remove_if_same(&self, key: &K, value: &Arc<V>) -> bool {
        let mut entries = self.lock();
        let still_registered = entries
            .get(key)
            .and_then(|entry| entry.handle.upgrade())
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
    pub(crate) fn get_or_insert_with(&self, key: K, make: impl FnOnce() -> Arc<V>) -> (Arc<V>, bool)
    where
        M: Default,
    {
        self.get_or_insert_with_meta(key, || (M::default(), make()))
    }

    /// Metadata-carrying counterpart of [`Self::get_or_insert_with`]. `make`
    /// is only invoked on a miss.
    pub(crate) fn get_or_insert_with_meta(
        &self,
        key: K,
        make: impl FnOnce() -> (M, Arc<V>),
    ) -> (Arc<V>, bool) {
        let mut entries = self.lock();
        entries.retain(|_, entry| entry.handle.strong_count() > 0);
        if let Some(value) = entries.get(&key).and_then(|entry| entry.handle.upgrade()) {
            return (value, true);
        }
        let (meta, value) = make();
        entries.insert(
            key,
            WeakEntry {
                meta,
                handle: Arc::downgrade(&value),
            },
        );
        (value, false)
    }

    /// Number of entries currently tracked, without sweeping dead ones.
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}
