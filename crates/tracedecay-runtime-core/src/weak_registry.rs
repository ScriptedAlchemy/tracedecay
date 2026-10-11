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
use std::future::Future;
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

/// One key's place in a [`SingleFlightRegistry`]: its lock serializes the
/// opens of that key only.
type Slot<V> = Arc<tokio::sync::Mutex<Weak<V>>>;

/// A weak registry whose values take an async open: concurrent joins of one
/// key share a single open, and an open in flight never delays other keys.
/// The map lock only finds a key's slot; the open runs under the slot.
pub struct SingleFlightRegistry<K, V> {
    slots: Mutex<HashMap<K, Slot<V>>>,
}

impl<K, V> Default for SingleFlightRegistry<K, V> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V> SingleFlightRegistry<K, V> {
    /// Returns the live value for `key`, or the one `open` builds. A join that
    /// arrives while `key` is opening waits for that open; a failed or
    /// cancelled open leaves the next join to try again.
    pub async fn get_or_open<E, F, O>(&self, key: K, open: O) -> Result<Arc<V>, E>
    where
        O: FnOnce() -> F,
        F: Future<Output = Result<Arc<V>, E>>,
    {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            // A slot nobody else holds is unlocked, so `try_lock` reads it.
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot.try_lock().is_ok_and(|live| live.strong_count() > 0)
            });
            Arc::clone(slots.entry(key).or_default())
        };
        let mut live = slot.lock().await;
        if let Some(value) = live.upgrade() {
            return Ok(value);
        }
        let value = open().await?;
        *live = Arc::downgrade(&value);
        Ok(value)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::*;

    type Registry = SingleFlightRegistry<&'static str, &'static str>;

    /// Store opens take seconds on the native engine. One lock shared by
    /// every key turned concurrent per-profile opens into a serial queue that
    /// outran the test suites' timeouts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_in_flight_delays_only_joins_of_its_own_key() {
        let registry = Arc::new(Registry::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let (release_a, held_a) = oneshot::channel::<()>();
        let (a_entered, a_opening) = oneshot::channel::<()>();

        let first_a = tokio::spawn({
            let (registry, opens) = (Arc::clone(&registry), Arc::clone(&opens));
            async move {
                registry
                    .get_or_open("a", || async move {
                        opens.fetch_add(1, Ordering::SeqCst);
                        a_entered.send(()).unwrap();
                        held_a.await.unwrap();
                        Ok::<_, ()>(Arc::new("a"))
                    })
                    .await
            }
        });
        a_opening.await.unwrap();
        let second_a = tokio::spawn({
            let (registry, opens) = (Arc::clone(&registry), Arc::clone(&opens));
            async move {
                registry
                    .get_or_open("a", || async move {
                        opens.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, ()>(Arc::new("a again"))
                    })
                    .await
            }
        });

        let b = tokio::time::timeout(
            Duration::from_secs(10),
            registry.get_or_open("b", || async { Ok::<_, ()>(Arc::new("b")) }),
        )
        .await
        .expect("key b opens while key a's open is still in flight");
        assert_eq!(*b.unwrap(), "b");

        release_a.send(()).unwrap();
        let first_a = first_a.await.unwrap().unwrap();
        let second_a = second_a.await.unwrap().unwrap();
        assert_eq!(*second_a, "a", "the second join got the first open's value");
        assert!(Arc::ptr_eq(&first_a, &second_a));
        assert_eq!(opens.load(Ordering::SeqCst), 1, "key a opened once");
    }

    #[tokio::test]
    async fn a_key_reopens_after_its_last_holder_drops_and_its_slot_is_swept() {
        let registry = Registry::default();
        let first = registry
            .get_or_open("p", || async { Ok::<_, ()>(Arc::new("first")) })
            .await
            .unwrap();
        drop(first);
        let second = registry
            .get_or_open("p", || async { Ok::<_, ()>(Arc::new("second")) })
            .await
            .unwrap();
        assert_eq!(*second, "second");
        drop(second);
        let _other = registry
            .get_or_open("q", || async { Ok::<_, ()>(Arc::new("other")) })
            .await
            .unwrap();
        assert_eq!(
            registry.len(),
            1,
            "the slot of a key nobody holds was swept"
        );
    }
}
