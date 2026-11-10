//! Per-store writer gates.
//!
//! # Why this is not one process-wide mutex
//!
//! Writer administration used to be a single process-wide `Mutex`. Its own
//! comment conceded that "a background refresh or a generation rebuild can hold
//! this gate for minutes", and it did: a git-watch sync of project A held the
//! only gate across a full `cg.sync()` while the *first* context call for
//! project B parked behind it with no deadline. That is a cross-project,
//! unbounded hold on a request path.
//!
//! Logical owner and content operations are scoped per store. Physical writer
//! ownership and destructive maintenance belong to `StoreRuntimeRegistry`.
//!
//! # The lock hierarchy
//!
//! ```text
//! daemon: RwLock       Daemon scope = write, store scopes = read
//!   └── store: RwLock  Maintenance = write, owner/content = read
//!       ├── owner: Mutex
//!       └── content: Mutex
//! ```
//!
//! Admitted operations hold this hierarchy. A queued store operation first
//! reserves its local resources without holding daemon admission. It only
//! *tries* the daemon read gate with that reservation held; if the daemon is
//! busy, it releases every local resource before awaiting the daemon. Thus
//! no local waiter pins daemon admission, and no task waits for the daemon
//! while holding a resource another daemon reader could need.
//!
//! # Exclusivity argument
//!
//! * **Never two logical operations in one class on one store.** They contend
//!   on the same `owner` or `content` mutex.
//! * **Daemon scope still excludes everything.** A `Daemon` acquisition takes
//!   `daemon.write()`, which no store-scoped acquisition can hold concurrently.
//! * **Owner and Content are deliberately concurrent.** They are disjoint
//!   concerns: `Owner` mutates in-memory owner/scheduler bookkeeping for a
//!   store, `Content` writes index rows into a store that is already open.
//!   They were only serialized before because there was one gate. Content
//!   writes were never exclusive against the rest of the process anyway,
//!   hook writes and memory writes go straight to the store without taking
//!   this gate at all, so admitting an owner-bookkeeping mutation beside a
//!   sync adds no writer that did not already exist.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// What one writer acquisition is allowed to do to a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreWriterClass {
    /// Mutates owner/scheduler bookkeeping for the store (project open, owner
    /// rekey, scheduler start/stop). Serialized against itself.
    Owner,
    /// Writes index content into an already-open store (git-watch sync,
    /// background refresh). Serialized against itself.
    Content,
}

/// Which lane a writer acquisition takes.
#[derive(Clone, Debug)]
pub enum WriterScope {
    /// Process-wide exclusion. Reserved for operations that sweep every mounted
    /// store, or whose store cannot be resolved.
    Daemon,
    /// Exclusion scoped to one store family, keyed by its canonical `data_root`.
    Store {
        data_root: PathBuf,
        class: StoreWriterClass,
    },
    /// Fair exclusive maintenance for one store, without blocking unrelated stores.
    StoreMaintenance { data_root: PathBuf },
}

impl WriterScope {
    /// Store-scoped acquisition for `data_root`. The caller is responsible for
    /// passing a canonical path; [`StoreWriterGates`] keys on it verbatim so
    /// that a mismatched key can never silently split a store's gate.
    #[must_use]
    pub fn store(data_root: impl Into<PathBuf>, class: StoreWriterClass) -> Self {
        Self::Store {
            data_root: data_root.into(),
            class,
        }
    }

    /// Same canonical store identity as [`Self::store`], with exclusive admission
    /// only against this store's owner and content operations.
    #[must_use]
    pub fn maintenance(data_root: impl Into<PathBuf>) -> Self {
        Self::StoreMaintenance {
            data_root: data_root.into(),
        }
    }
}

/// The logical lanes of one store family. Held by `Arc` from the registry and by
/// every live guard, so a gate outlives any holder even if the registry prunes
/// its weak entry.
#[derive(Default)]
struct StoreGate {
    maintenance: Arc<RwLock<()>>,
    owner: Arc<Mutex<()>>,
    content: Arc<Mutex<()>>,
}

impl StoreGate {
    fn class_mutex(&self, class: StoreWriterClass) -> Arc<Mutex<()>> {
        match class {
            StoreWriterClass::Owner => Arc::clone(&self.owner),
            StoreWriterClass::Content => Arc::clone(&self.content),
        }
    }

    fn try_reserve(
        &self,
        class: Option<StoreWriterClass>,
    ) -> Option<(StoreMaintenanceGuard, Option<OwnedMutexGuard<()>>)> {
        match class {
            Some(class) => {
                let maintenance = Arc::clone(&self.maintenance).try_read_owned().ok()?;
                let class_guard = self.class_mutex(class).try_lock_owned().ok()?;
                Some((
                    StoreMaintenanceGuard::Shared(maintenance),
                    Some(class_guard),
                ))
            }
            None => Some((
                StoreMaintenanceGuard::Exclusive(
                    Arc::clone(&self.maintenance).try_write_owned().ok()?,
                ),
                None,
            )),
        }
    }
}

/// Admission held for the duration of one writer operation.
///
/// Dropping this releases the whole hierarchy. The fields are never read; they
/// exist to pin the guards.
pub struct WriterAdmissionGuard {
    _class: Option<OwnedMutexGuard<()>>,
    _maintenance: Option<StoreMaintenanceGuard>,
    _daemon: DaemonGuard,
    /// Keeps the store's gate alive for as long as it is held, so a registry
    /// prune between acquisition and release cannot hand a second acquirer a
    /// fresh, uncontended gate for the same store.
    _gate: Option<Arc<StoreGate>>,
}

/// Held for RAII only: the payload is the lock guard, and it is released by
/// dropping this value, never by reading it.
#[allow(dead_code)]
enum DaemonGuard {
    Shared(OwnedRwLockReadGuard<()>),
    Exclusive(OwnedRwLockWriteGuard<()>),
}

/// Store-local readers and the queued exclusive retention guard.
#[allow(dead_code)]
enum StoreMaintenanceGuard {
    Shared(OwnedRwLockReadGuard<()>),
    Exclusive(OwnedRwLockWriteGuard<()>),
}

/// Writer-gate registry: one process-wide lane plus one lane set per store family.
pub struct StoreWriterGates {
    daemon: Arc<RwLock<()>>,
    stores: std::sync::Mutex<HashMap<PathBuf, Weak<StoreGate>>>,
}

impl Default for StoreWriterGates {
    fn default() -> Self {
        Self {
            daemon: Arc::new(RwLock::new(())),
            stores: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl StoreWriterGates {
    /// Resolves (creating if needed) the gate set for one store family.
    ///
    /// Entries are weak so a retired store's gate is reclaimed, and dead
    /// entries are pruned opportunistically on insert. The returned `Arc` is
    /// retained by the guard, so an upgrade always observes the same gate any
    /// live holder is using.
    fn store_gate(&self, data_root: &Path) -> Arc<StoreGate> {
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(gate) = stores.get(data_root).and_then(Weak::upgrade) {
            return gate;
        }
        stores.retain(|_, entry| entry.strong_count() > 0);
        let gate = Arc::new(StoreGate::default());
        stores.insert(data_root.to_path_buf(), Arc::downgrade(&gate));
        gate
    }

    /// Number of live store gates. Test-only observability for the isolation
    /// proofs.
    #[cfg(test)]
    pub fn live_store_gates(&self) -> usize {
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        stores.retain(|_, entry| entry.strong_count() > 0);
        stores.len()
    }

    /// Acquires admission for `scope`, waiting as long as necessary.
    pub async fn acquire(&self, scope: &WriterScope) -> WriterAdmissionGuard {
        match scope {
            WriterScope::Daemon => WriterAdmissionGuard {
                _class: None,
                _maintenance: None,
                _daemon: DaemonGuard::Exclusive(
                    tracing::Instrument::instrument(
                        Arc::clone(&self.daemon).write_owned(),
                        tracing::trace_span!("daemon.writer_gate.acquire.daemon"),
                    )
                    .await,
                ),
                _gate: None,
            },
            WriterScope::Store { data_root, class } => {
                self.acquire_store(data_root, Some(*class)).await
            }
            WriterScope::StoreMaintenance { data_root } => {
                self.acquire_store(data_root, None).await
            }
        }
    }

    /// Reserve the store without retaining daemon admission while queued.
    /// `None` is exclusive maintenance; owner/content reserve a shared store
    /// guard plus their class mutex.
    async fn acquire_store(
        &self,
        data_root: &Path,
        class: Option<StoreWriterClass>,
    ) -> WriterAdmissionGuard {
        let gate = self.store_gate(data_root);
        loop {
            let (maintenance, class_guard) = match class {
                Some(class) => {
                    let maintenance = tracing::Instrument::instrument(
                        Arc::clone(&gate.maintenance).read_owned(),
                        tracing::trace_span!("daemon.writer_gate.acquire.store_read"),
                    )
                    .await;
                    let class_guard = tracing::Instrument::instrument(
                        gate.class_mutex(class).lock_owned(),
                        tracing::trace_span!("daemon.writer_gate.acquire.class"),
                    )
                    .await;
                    (
                        StoreMaintenanceGuard::Shared(maintenance),
                        Some(class_guard),
                    )
                }
                None => {
                    let maintenance = tracing::Instrument::instrument(
                        Arc::clone(&gate.maintenance).write_owned(),
                        tracing::trace_span!("daemon.writer_gate.acquire.store_maintenance"),
                    )
                    .await;
                    (StoreMaintenanceGuard::Exclusive(maintenance), None)
                }
            };
            if let Ok(daemon) = Arc::clone(&self.daemon).try_read_owned() {
                return WriterAdmissionGuard {
                    _class: class_guard,
                    _maintenance: Some(maintenance),
                    _daemon: DaemonGuard::Shared(daemon),
                    _gate: Some(gate),
                };
            }
            // Take our fair daemon turn without awaiting any local resource
            // under it. An immediate local probe can use this turn even when
            // another daemon writer is queued; dropping the turn first could
            // starve a store behind continuous daemon-wide work.
            drop(class_guard);
            drop(maintenance);
            let daemon = tracing::Instrument::instrument(
                Arc::clone(&self.daemon).read_owned(),
                tracing::trace_span!("daemon.writer_gate.acquire.store"),
            )
            .await;
            if let Some((maintenance, class_guard)) = gate.try_reserve(class) {
                return WriterAdmissionGuard {
                    _class: class_guard,
                    _maintenance: Some(maintenance),
                    _daemon: DaemonGuard::Shared(daemon),
                    _gate: Some(gate),
                };
            }
            drop(daemon);
        }
    }

    /// Acquires admission only if every level is free right now.
    #[tracing::instrument(name = "daemon.writer_gate.try_acquire", level = "trace", skip_all)]
    pub fn try_acquire(&self, scope: &WriterScope) -> Option<WriterAdmissionGuard> {
        match scope {
            WriterScope::Daemon => Some(WriterAdmissionGuard {
                _class: None,
                _maintenance: None,
                _daemon: DaemonGuard::Exclusive(Arc::clone(&self.daemon).try_write_owned().ok()?),
                _gate: None,
            }),
            WriterScope::Store { data_root, class } => {
                self.try_acquire_store(data_root, Some(*class))
            }
            WriterScope::StoreMaintenance { data_root } => self.try_acquire_store(data_root, None),
        }
    }

    fn try_acquire_store(
        &self,
        data_root: &Path,
        class: Option<StoreWriterClass>,
    ) -> Option<WriterAdmissionGuard> {
        let daemon = Arc::clone(&self.daemon).try_read_owned().ok()?;
        let gate = self.store_gate(data_root);
        let (maintenance, class_guard) = gate.try_reserve(class)?;
        Some(WriterAdmissionGuard {
            _class: class_guard,
            _maintenance: Some(maintenance),
            _daemon: DaemonGuard::Shared(daemon),
            _gate: Some(gate),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use super::*;

    fn store(root: &str, class: StoreWriterClass) -> WriterScope {
        WriterScope::store(PathBuf::from(root), class)
    }

    #[tokio::test]
    async fn two_writers_on_one_store_are_refused() {
        let gates = StoreWriterGates::default();
        let held = gates.acquire(&store("/a", StoreWriterClass::Owner)).await;
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_none(),
            "a second owner writer on the same store must be refused"
        );
        drop(held);
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_some(),
            "the gate must be released when the guard drops"
        );
    }

    #[tokio::test]
    async fn owner_and_content_share_a_store() {
        let gates = StoreWriterGates::default();
        let _content = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_some(),
            "owner bookkeeping must not queue behind an index sync"
        );
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Content))
                .is_none(),
            "a second index sync on the same store still queues"
        );
    }

    #[tokio::test]
    async fn a_projects_sync_does_not_block_another_projects_open() {
        let gates = StoreWriterGates::default();
        let _sync = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        let admitted = tokio::time::timeout(
            Duration::from_secs(5),
            gates.acquire(&store("/b", StoreWriterClass::Owner)),
        )
        .await;
        assert!(
            admitted.is_ok(),
            "project B must open while project A is syncing"
        );
    }

    #[tokio::test]
    async fn daemon_scope_excludes_every_store() {
        let gates = StoreWriterGates::default();
        let daemon = gates.acquire(&WriterScope::Daemon).await;
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_none(),
            "daemon-wide administration must exclude store writers"
        );
        drop(daemon);
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_some(),
            "store writers are admitted once daemon administration releases"
        );
    }

    #[tokio::test]
    async fn a_store_writer_excludes_daemon_scope() {
        let gates = StoreWriterGates::default();
        let store_writer = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        assert!(
            gates.try_acquire(&WriterScope::Daemon).is_none(),
            "a store writer must exclude daemon-wide administration"
        );
        drop(store_writer);
        assert!(
            gates.try_acquire(&WriterScope::Daemon).is_some(),
            "daemon administration is admitted once the store writer releases"
        );
    }

    #[tokio::test]
    async fn queued_store_maintenance_preserves_other_project_admission() {
        let gates = StoreWriterGates::default();
        let held = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        let maintenance_scope = WriterScope::maintenance("/a");
        let queued = gates.acquire(&maintenance_scope);
        tokio::pin!(queued);
        poll_fn(|cx| {
            assert!(queued.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        let following_scope = store("/a", StoreWriterClass::Owner);
        let following = gates.acquire(&following_scope);
        tokio::pin!(following);
        poll_fn(|cx| {
            assert!(
                following.as_mut().poll(cx).is_pending(),
                "same-store operations wait behind queued maintenance"
            );
            Poll::Ready(())
        })
        .await;

        assert!(
            gates
                .try_acquire(&store("/b", StoreWriterClass::Owner))
                .is_some(),
            "an unrelated project can open while maintenance queues behind a long sync"
        );
        drop(held);
        let admitted = tokio::time::timeout(Duration::from_secs(5), &mut queued)
            .await
            .expect("queued maintenance acquires when its own store is free");
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Content))
                .is_none(),
            "admitted maintenance excludes writes to its own store"
        );
        drop(admitted);
        let follower = tokio::time::timeout(Duration::from_secs(5), &mut following)
            .await
            .expect("same-store owner resumes after maintenance");
        drop(follower);
        assert!(gates.try_acquire(&maintenance_scope).is_some());
    }

    #[tokio::test]
    async fn queued_maintenance_yields_to_a_daemon_writer_without_a_lock_cycle() {
        let gates = StoreWriterGates::default();
        let held = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        let scope = WriterScope::maintenance("/a");
        let maintenance = gates.acquire(&scope);
        tokio::pin!(maintenance);
        poll_fn(|cx| {
            assert!(maintenance.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let follower_scope = store("/a", StoreWriterClass::Owner);
        let follower = gates.acquire(&follower_scope);
        tokio::pin!(follower);
        poll_fn(|cx| {
            assert!(follower.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let daemon_scope = WriterScope::Daemon;
        let daemon = gates.acquire(&daemon_scope);
        tokio::pin!(daemon);
        poll_fn(|cx| {
            assert!(daemon.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(held);
        // Neither queued local future is repolled. Their reservations must
        // not retain daemon admission, even after their store becomes free.
        let exclusive = tokio::time::timeout(Duration::from_secs(5), &mut daemon)
            .await
            .expect("daemon writer progresses independently of queued store futures");
        assert!(gates.try_acquire(&scope).is_none());
        poll_fn(|cx| {
            assert!(maintenance.as_mut().poll(cx).is_pending());
            assert!(follower.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(exclusive);
        let retained = tokio::time::timeout(Duration::from_secs(5), &mut maintenance)
            .await
            .expect("maintenance resumes after daemon writer");
        drop(retained);
        let following = tokio::time::timeout(Duration::from_secs(5), &mut follower)
            .await
            .expect("store follower resumes after maintenance");
        drop(following);
    }

    #[tokio::test]
    async fn a_queued_store_uses_its_fair_daemon_turn() {
        for scope in [
            WriterScope::maintenance("/a"),
            store("/a", StoreWriterClass::Content),
        ] {
            let gates = StoreWriterGates::default();
            let held = gates.acquire(&WriterScope::Daemon).await;
            let store = gates.acquire(&scope);
            tokio::pin!(store);
            poll_fn(|cx| {
                assert!(store.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let daemon_scope = WriterScope::Daemon;
            let following = gates.acquire(&daemon_scope);
            tokio::pin!(following);
            poll_fn(|cx| {
                assert!(following.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(held);
            // The later daemon writer stays queued and unpolled. The store
            // must consume its earlier shared turn rather than discard it.
            let admitted = tokio::time::timeout(Duration::from_secs(5), &mut store)
                .await
                .expect("store uses its fair daemon turn before the next daemon writer");
            assert!(gates.try_acquire(&WriterScope::Daemon).is_none());
            drop(admitted);
            let next = tokio::time::timeout(Duration::from_secs(5), &mut following)
                .await
                .expect("following daemon writer progresses after the store");
            drop(next);
        }
    }

    #[tokio::test]
    async fn retired_store_gates_are_reclaimed() {
        let gates = StoreWriterGates::default();
        {
            let _a = gates.acquire(&store("/a", StoreWriterClass::Owner)).await;
            let _b = gates.acquire(&store("/b", StoreWriterClass::Owner)).await;
            assert_eq!(gates.live_store_gates(), 2);
        }
        assert_eq!(
            gates.live_store_gates(),
            0,
            "gates for stores with no live holder must be reclaimed"
        );
    }
}
