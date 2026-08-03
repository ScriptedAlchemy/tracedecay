//! Per-store daemon writer gates.
//!
//! # Why this is not one daemon-wide mutex
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
//! daemon: RwLock          Daemon scope = write, Store scope = read
//!   ├── owner:  Mutex
//!   └── content: Mutex
//! ```
//!
//! Every acquisition takes these in exactly that order, so no lock-order
//! inversion is possible.
//!
//! # Exclusivity argument
//!
//! * **Never two logical operations in one class on one store.** They contend
//!   on the same `owner` or `content` mutex.
//! * **Daemon scope still excludes everything.** A `Daemon` acquisition takes
//!   `daemon.write()`, which no store-scoped acquisition can hold concurrently.
//! * **Owner and Content are deliberately concurrent.** They are disjoint
//!   concerns: `Owner` mutates the daemon's in-memory owner/scheduler
//!   bookkeeping for a store, `Content` writes index rows into a store that is
//!   already open. They were only serialized before because there was one gate.
//!   Content writes were never exclusive against the rest of the daemon anyway
//!   — hook writes and memory writes go straight to the store without taking
//!   this gate at all — so admitting an owner-bookkeeping mutation beside a
//!   sync adds no writer that did not already exist.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// What one writer acquisition is allowed to do to a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StoreWriterClass {
    /// Mutates the daemon's owner/scheduler bookkeeping for the store (project
    /// open, owner rekey, scheduler start/stop). Serialized against itself.
    Owner,
    /// Writes index content into an already-open store (git-watch sync,
    /// background refresh). Serialized against itself.
    Content,
}

/// Which lane a writer acquisition takes.
#[derive(Clone, Debug)]
pub(super) enum WriterScope {
    /// Daemon-wide exclusion. Reserved for operations that sweep every mounted
    /// store, or whose store cannot be resolved.
    Daemon,
    /// Exclusion scoped to one store family, keyed by its canonical `data_root`.
    Store {
        data_root: PathBuf,
        class: StoreWriterClass,
    },
}

impl WriterScope {
    /// Store-scoped acquisition for `data_root`. The caller is responsible for
    /// passing a canonical path; [`StoreWriterGates`] keys on it verbatim so
    /// that a mismatched key can never silently split a store's gate.
    pub(super) fn store(data_root: impl Into<PathBuf>, class: StoreWriterClass) -> Self {
        Self::Store {
            data_root: data_root.into(),
            class,
        }
    }
}

/// The logical lanes of one store family. Held by `Arc` from the registry and by
/// every live guard, so a gate outlives any holder even if the registry prunes
/// its weak entry.
#[derive(Default)]
struct StoreGate {
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
}

/// Admission held for the duration of one writer operation.
///
/// Dropping this releases the whole hierarchy. The fields are never read; they
/// exist to pin the guards.
pub(super) struct WriterAdmissionGuard {
    _class: Option<OwnedMutexGuard<()>>,
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

/// The daemon's writer-gate registry: one daemon-wide lane plus one lane set
/// per store family.
pub(super) struct StoreWriterGates {
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
    pub(super) fn live_store_gates(&self) -> usize {
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        stores.retain(|_, entry| entry.strong_count() > 0);
        stores.len()
    }

    /// Acquires admission for `scope`, waiting as long as necessary.
    pub(super) async fn acquire(&self, scope: &WriterScope) -> WriterAdmissionGuard {
        match scope {
            WriterScope::Daemon => WriterAdmissionGuard {
                _class: None,
                _daemon: DaemonGuard::Exclusive(Arc::clone(&self.daemon).write_owned().await),
                _gate: None,
            },
            WriterScope::Store { data_root, class } => {
                let daemon = Arc::clone(&self.daemon).read_owned().await;
                let gate = self.store_gate(data_root);
                let class_guard = gate.class_mutex(*class).lock_owned().await;
                WriterAdmissionGuard {
                    _class: Some(class_guard),
                    _daemon: DaemonGuard::Shared(daemon),
                    _gate: Some(gate),
                }
            }
        }
    }

    /// Acquires admission only if every level is free right now.
    ///
    pub(super) fn try_acquire(&self, scope: &WriterScope) -> Option<WriterAdmissionGuard> {
        match scope {
            WriterScope::Daemon => Some(WriterAdmissionGuard {
                _class: None,
                _daemon: DaemonGuard::Exclusive(Arc::clone(&self.daemon).try_write_owned().ok()?),
                _gate: None,
            }),
            WriterScope::Store { data_root, class } => {
                let daemon = Arc::clone(&self.daemon).try_read_owned().ok()?;
                let gate = self.store_gate(data_root);
                let class_guard = gate.class_mutex(*class).try_lock_owned().ok()?;
                Some(WriterAdmissionGuard {
                    _class: Some(class_guard),
                    _daemon: DaemonGuard::Shared(daemon),
                    _gate: Some(gate),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        let _daemon = gates.acquire(&WriterScope::Daemon).await;
        assert!(
            gates
                .try_acquire(&store("/a", StoreWriterClass::Owner))
                .is_none(),
            "daemon-wide administration must exclude store writers"
        );
    }

    #[tokio::test]
    async fn a_store_writer_excludes_daemon_scope() {
        let gates = StoreWriterGates::default();
        let _store = gates.acquire(&store("/a", StoreWriterClass::Content)).await;
        assert!(
            gates.try_acquire(&WriterScope::Daemon).is_none(),
            "a store writer must exclude daemon-wide administration"
        );
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
