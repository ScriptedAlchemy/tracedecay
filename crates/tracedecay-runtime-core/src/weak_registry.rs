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

type SingleFlightSlot<V> = Arc<asupersync::sync::Mutex<Weak<V>>>;

/// Async get-or-open over weak entries with one single-flight slot per key.
///
/// The map lock is held only to find or insert a key's slot; the open runs
/// under that slot's own async lock. Concurrent joins of one key therefore
/// share a single open while other keys open independently, and a failed or
/// cancelled open leaves the slot empty for the next join to retry.
pub struct SingleFlightWeakRegistry<K, V> {
    slots: Mutex<HashMap<K, SingleFlightSlot<V>>>,
}

impl<K, V> Default for SingleFlightWeakRegistry<K, V> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V> SingleFlightWeakRegistry<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the live value for `key`, or runs `open` to create it. Joins
    /// of the same key that arrive while `open` runs wait for and share its
    /// result; joins of other keys do not wait.
    pub async fn get_or_open<E, Fut>(
        &self,
        key: K,
        open: impl FnOnce() -> Fut,
    ) -> std::result::Result<Arc<V>, E>
    where
        Fut: std::future::Future<Output = std::result::Result<V, E>>,
        E: From<tracedecay_domain::errors::TraceDecayError>,
    {
        let cx = crate::native_runtime::cx().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "single-flight open requires a native task context".into(),
            }
        })?;
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            // Holding the map lock stops anyone cloning a slot, so a slot only
            // the map holds is idle and is dead once its value has dropped.
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot.try_lock().is_ok_and(|value| value.strong_count() > 0)
            });
            Arc::clone(slots.entry(key).or_default())
        };
        let mut live = slot.lock(&cx).await.map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("single-flight open lock unavailable: {error}"),
            }
        })?;
        if let Some(value) = live.upgrade() {
            return Ok(value);
        }
        let value = Arc::new(open().await?);
        *live = Arc::downgrade(&value);
        Ok(value)
    }
}

#[cfg(test)]
mod single_flight_tests {
    use super::*;
    use asupersync::channel::oneshot;
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use tracedecay_domain::errors::TraceDecayError;

    fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn same_key_joins_share_an_open_without_blocking_other_keys() {
        let runtime = asupersync::runtime::RuntimeBuilder::new()
            .worker_threads(2)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = asupersync::Cx::current().unwrap();
            let registry = SingleFlightWeakRegistry::<&str, usize>::new();
            let opens = AtomicUsize::new(0);
            let (release_tx, mut release_rx) = oneshot::channel::<()>();
            let mut first = pin!(registry.get_or_open("a", || async {
                release_rx.recv(&cx).await.unwrap();
                Ok::<_, TraceDecayError>(opens.fetch_add(1, Ordering::SeqCst))
            }));
            assert!(poll_once(first.as_mut()).is_pending());
            let mut joiner = pin!(registry.get_or_open("a", || async {
                Ok::<_, TraceDecayError>(opens.fetch_add(1, Ordering::SeqCst))
            }));
            assert!(poll_once(joiner.as_mut()).is_pending());
            let other = registry
                .get_or_open("b", || async { Ok::<_, TraceDecayError>(3) })
                .await
                .unwrap();
            assert_eq!(*other, 3);
            release_tx.send(&cx, ()).unwrap();
            let first = first.await.unwrap();
            let joined = joiner.await.unwrap();
            assert!(Arc::ptr_eq(&first, &joined));
            assert_eq!(opens.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn failed_and_cancelled_opens_leave_the_key_available() {
        let runtime = asupersync::runtime::RuntimeBuilder::new()
            .worker_threads(2)
            .build()
            .unwrap();
        runtime.block_on(async {
            let registry = SingleFlightWeakRegistry::<&str, usize>::new();
            let failed = registry
                .get_or_open("a", || async {
                    Err::<usize, _>(TraceDecayError::Config {
                        message: "store busy".into(),
                    })
                })
                .await;
            assert!(matches!(failed, Err(TraceDecayError::Config { .. })));
            {
                let mut cancelled = pin!(registry.get_or_open("a", || std::future::pending::<
                    Result<usize, TraceDecayError>,
                >()));
                assert!(poll_once(cancelled.as_mut()).is_pending());
            }
            let opened = registry
                .get_or_open("a", || async { Ok::<_, TraceDecayError>(4) })
                .await
                .unwrap();
            assert_eq!(*opened, 4);
        });
    }

    #[test]
    fn dropped_values_reopen_and_idle_slots_are_swept() {
        let runtime = asupersync::runtime::RuntimeBuilder::new()
            .worker_threads(2)
            .build()
            .unwrap();
        runtime.block_on(async {
            let registry = SingleFlightWeakRegistry::<&str, usize>::new();
            drop(
                registry
                    .get_or_open("a", || async { Ok::<_, TraceDecayError>(1) })
                    .await
                    .unwrap(),
            );
            let second = registry
                .get_or_open("b", || async { Ok::<_, TraceDecayError>(2) })
                .await
                .unwrap();
            assert_eq!(
                registry
                    .slots
                    .lock()
                    .unwrap()
                    .keys()
                    .copied()
                    .collect::<Vec<_>>(),
                vec!["b"]
            );
            let reopened = registry
                .get_or_open("a", || async { Ok::<_, TraceDecayError>(3) })
                .await
                .unwrap();
            assert_eq!(*reopened, 3);
            drop(second);
        });
    }

    #[test]
    fn open_without_native_context_is_a_configuration_failure() {
        let registry = SingleFlightWeakRegistry::<&str, usize>::new();
        let mut opened = pin!(registry.get_or_open("a", || async { Ok::<_, TraceDecayError>(1) }));
        assert!(matches!(
            poll_once(opened.as_mut()),
            Poll::Ready(Err(TraceDecayError::Config { .. }))
        ));
    }
}
