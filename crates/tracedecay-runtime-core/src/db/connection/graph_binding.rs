use std::sync::Arc;

use super::Database;
use crate::store_runtime::{VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_db::GraphWatermark;

/// Watermark of the projected memory-graph source at an exact lineage stamp.
///
/// `memory_v2_lineage_events.event_sequence` is append-only (schema triggers
/// reject updates and deletes) and its `AUTOINCREMENT` key is never reused,
/// and every committed mutation that can change the projected source records
/// at least one lineage event in the same transaction (the invariant
/// `store::memory::graph::source_unchanged_since` already relies on). Two
/// snapshots observing the same maximum sequence therefore saw an identical
/// projected source, so a watermark computed under one snapshot remains valid
/// for any later snapshot that still observes the same stamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MemoryGraphSourceStampedWatermarkV1 {
    lineage_stamp: i64,
    watermark: GraphWatermark,
}

/// One short-lived use of the graph authority bound to a database owner.
///
/// The database stores only a weak graph binding. Holding this operation
/// retains the caller's already-issued database client for the duration of
/// graph work, but it does not create a persistent Store or Graph lease in
/// the database owner itself.
pub struct MemoryGraphRuntimeOperationV1 {
    _database: super::DatabaseClientGuardV1,
    runtime: Arc<dyn VerifiedGraphRuntimePortV1>,
}

impl MemoryGraphRuntimeOperationV1 {
    #[must_use]
    pub fn runtime(&self) -> &dyn VerifiedGraphRuntimePortV1 {
        self.runtime.as_ref()
    }
}

/// Typed outcome when a database cannot issue graph work beneath its exact
/// map-owned graph attachment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryGraphRuntimeOperationErrorV1 {
    Unbound,
    Unavailable,
    IdentityMismatch,
}

impl Database {
    /// Binds the exact registered Grafeo runtime paired with this memory
    /// shard. The binding is weak: its map owner retains the graph authority,
    /// while every caller must obtain a short-lived operation through
    /// [`Self::issue_memory_graph_runtime_operation`]. A second binding is
    /// rejected so path-derived or sibling-project handles cannot silently
    /// replace the mounted authority.
    pub fn bind_memory_graph_runtime(
        &self,
        runtime: Arc<dyn VerifiedGraphRuntimePortV1>,
    ) -> Result<()> {
        if !self.is_writable() {
            return Err(TraceDecayError::Database {
                operation: "bind verified memory graph runtime".to_owned(),
                message: "read-only memory databases cannot bind a graph publisher".to_owned(),
            });
        }
        if runtime.relational_binding() != self.registered_binding()
            || runtime.relational_verified_locator() != self.registered_verified_locator()
        {
            return Err(TraceDecayError::Database {
                operation: "bind verified memory graph runtime".to_owned(),
                message: "verified memory graph runtime does not match the retained database"
                    .to_owned(),
            });
        }
        let candidate = Arc::downgrade(&runtime);
        let mounted = self
            .inner
            .memory_graph_runtime
            .get_or_init(|| candidate.clone());
        if mounted.ptr_eq(&candidate) {
            Ok(())
        } else {
            Err(TraceDecayError::Database {
                operation: "bind verified memory graph runtime".to_owned(),
                message: "verified memory graph runtime is already bound".to_owned(),
            })
        }
    }

    /// Returns a cloneable non-retaining route to the graph runtime bound to
    /// this exact database.
    ///
    /// The proxy retains no database client, Store lease, Graph lease, or
    /// graph map owner. It carries the identity validated at binding time and
    /// upgrades the weak graph port privately for each operation. `None`
    /// proves graph activation has not completed yet; callers that must not
    /// wait for activation bind [`Self::deferred_memory_graph_runtime`]
    /// instead.
    #[must_use]
    pub fn memory_graph_runtime(&self) -> Option<VerifiedGraphRuntimeWeakProxyV1> {
        Some(VerifiedGraphRuntimeWeakProxyV1::new(
            self.registered_binding().clone(),
            self.registered_verified_locator().clone(),
            self.inner.memory_graph_runtime.get()?.clone(),
        ))
    }

    /// Returns the deferred-activation route to this exact database's graph
    /// runtime, available before [`Self::bind_memory_graph_runtime`] runs.
    ///
    /// The proxy shares the activation cell instead of a resolved weak
    /// pointer: every operation resolves the cell at use time, so a proxy
    /// bound while deferred graph activation is still warming starts
    /// answering the moment activation publishes the runtime and stays a
    /// typed unavailable state until then. Like the resolved route, it
    /// retains no database client, Store lease, Graph lease, or graph map
    /// owner. This is the production composition route: binding it cannot
    /// silently miss an activation that has not happened yet.
    #[must_use]
    pub fn deferred_memory_graph_runtime(&self) -> VerifiedGraphRuntimeWeakProxyV1 {
        VerifiedGraphRuntimeWeakProxyV1::new_deferred(
            self.registered_binding().clone(),
            self.registered_verified_locator().clone(),
            Arc::clone(&self.inner.memory_graph_runtime),
        )
    }

    /// Returns the memoized projected-source watermark when it was computed
    /// under the exact `lineage_stamp` the caller currently observes.
    #[must_use]
    pub(crate) fn memory_graph_source_watermark_at(
        &self,
        lineage_stamp: i64,
    ) -> Option<GraphWatermark> {
        self.inner
            .memory_graph_source_watermark
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|memo| memo.lineage_stamp == lineage_stamp)
            .map(|memo| memo.watermark.clone())
    }

    /// Records the projected-source watermark computed under `lineage_stamp`.
    /// The stamp and the hashed source must come from the same read snapshot.
    pub(crate) fn record_memory_graph_source_watermark(
        &self,
        lineage_stamp: i64,
        watermark: GraphWatermark,
    ) {
        *self
            .inner
            .memory_graph_source_watermark
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(MemoryGraphSourceStampedWatermarkV1 {
                lineage_stamp,
                watermark,
            });
    }

    /// Issues graph work through the exact weak map binding.
    ///
    /// The returned value is non-cloneable and keeps the database client
    /// alive only while graph work is actually in progress. Its graph port is
    /// supplied by the daemon's map owner, which must issue any native graph
    /// lease per operation and therefore remains retirement-fence visible.
    pub fn issue_memory_graph_runtime_operation(
        &self,
    ) -> std::result::Result<MemoryGraphRuntimeOperationV1, MemoryGraphRuntimeOperationErrorV1>
    {
        if !self.is_writable() {
            return Err(MemoryGraphRuntimeOperationErrorV1::Unbound);
        }
        let bound = self
            .inner
            .memory_graph_runtime
            .get()
            .ok_or(MemoryGraphRuntimeOperationErrorV1::Unbound)?;
        let runtime = bound
            .upgrade()
            .ok_or(MemoryGraphRuntimeOperationErrorV1::Unavailable)?;
        if runtime.relational_binding() != self.registered_binding()
            || runtime.relational_verified_locator() != self.registered_verified_locator()
        {
            return Err(MemoryGraphRuntimeOperationErrorV1::IdentityMismatch);
        }
        Ok(MemoryGraphRuntimeOperationV1 {
            _database: self.client_guard(),
            runtime,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use tracedecay_domain::LocatorDigest;
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
        GraphWatermark, VerifiedGraphSnapshot,
    };
    use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

    use super::Database;
    use crate::db::{
        DatabaseAuthority, MemoryGraphRuntimeOperationErrorV1, TestDatabaseRuntimeMode,
    };
    use crate::store_runtime::VerifiedGraphRuntimePortV1;

    struct TestGraphRuntime {
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        snapshot_calls: Arc<AtomicUsize>,
        /// Models a production graph port retaining a capability derived from
        /// the same database client. The database binding must stay weak or
        /// this creates a self-retaining owner cycle.
        _retained_database: Option<Database>,
    }

    impl VerifiedGraphRuntimePortV1 for TestGraphRuntime {
        fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.locator
        }

        fn cancel_reconciliation(&self) {}

        fn publish_verified_manifest(
            &self,
            _manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
            _cancelled: Arc<AtomicBool>,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            Err(GraphDbError::unavailable("test graph has no publication"))
        }

        fn reconcile_verified_manifest(
            &self,
            _manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            Err(GraphDbError::unavailable(
                "test graph has no reconciliation",
            ))
        }

        fn verified_snapshot(
            &self,
            _projection: &GraphProjectionIdentity,
            _read_control: FactReadControl,
        ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
            self.snapshot_calls.fetch_add(1, Ordering::AcqRel);
            Ok(None)
        }
    }

    #[tokio::test]
    async fn concurrent_identical_graph_runtime_binds_are_idempotent() {
        let directory = tempfile::tempdir().expect("graph binding directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "graph binding test")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let database = Arc::new(database);
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            snapshot_calls: Arc::new(AtomicUsize::new(0)),
            _retained_database: None,
        });
        let barrier = Arc::new(std::sync::Barrier::new(17));
        std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for _ in 0..16 {
                let database = Arc::clone(&database);
                let runtime = Arc::clone(&runtime);
                let barrier = Arc::clone(&barrier);
                threads.push(scope.spawn(move || {
                    barrier.wait();
                    database.bind_memory_graph_runtime(runtime)
                }));
            }
            barrier.wait();
            for thread in threads {
                thread
                    .join()
                    .expect("binding thread")
                    .expect("identical bind");
            }
        });
        let operation = database
            .issue_memory_graph_runtime_operation()
            .expect("bound graph operation");
        assert!(Arc::ptr_eq(&operation.runtime, &runtime));
    }

    #[tokio::test]
    async fn stamped_source_watermark_memo_serves_only_the_exact_lineage_stamp() {
        let directory = tempfile::tempdir().expect("watermark memo directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "watermark memo test")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");

        // Before any record, every stamp is a miss — never a fabricated hit.
        assert!(database.memory_graph_source_watermark_at(7).is_none());

        let first = GraphWatermark::new("sha256:memo-stamp-seven").expect("first watermark");
        database.record_memory_graph_source_watermark(7, first.clone());
        assert_eq!(
            database.memory_graph_source_watermark_at(7),
            Some(first.clone())
        );
        // A snapshot observing a different stamp must not see the memo.
        assert!(database.memory_graph_source_watermark_at(8).is_none());
        // A stale-stamp probe does not evict the memo for its exact stamp.
        assert_eq!(database.memory_graph_source_watermark_at(7), Some(first));

        // A newer record supersedes the memo; the old stamp becomes a miss.
        let second = GraphWatermark::new("sha256:memo-stamp-eight").expect("second watermark");
        database.record_memory_graph_source_watermark(8, second.clone());
        assert_eq!(database.memory_graph_source_watermark_at(8), Some(second));
        assert!(database.memory_graph_source_watermark_at(7).is_none());
    }

    #[tokio::test]
    async fn graph_runtime_binding_rejects_the_right_shard_with_the_wrong_locator() {
        let directory = tempfile::tempdir().expect("graph locator binding directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "graph locator test")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let mut locator = database.registered_verified_locator().clone();
        locator.locator_digest =
            LocatorDigest::new(format!("sha256:{}", "f".repeat(64))).expect("foreign locator");
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator,
            snapshot_calls: Arc::new(AtomicUsize::new(0)),
            _retained_database: None,
        });

        assert!(database.bind_memory_graph_runtime(runtime).is_err());
        assert!(matches!(
            database.issue_memory_graph_runtime_operation(),
            Err(MemoryGraphRuntimeOperationErrorV1::Unbound)
        ));
    }

    #[tokio::test]
    async fn bound_graph_runtime_does_not_retain_a_derived_store_or_graph_owner() {
        let directory = tempfile::tempdir().expect("weak graph binding directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "weak graph binding")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            snapshot_calls: Arc::new(AtomicUsize::new(0)),
            _retained_database: Some(database.clone()),
        });
        let weak_runtime = Arc::downgrade(&runtime);

        database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("bind exact graph runtime");
        drop(runtime);
        drop(database);

        assert!(weak_runtime.upgrade().is_none());
    }

    #[tokio::test]
    async fn deferred_graph_proxy_resolves_once_activation_completes() {
        let directory = tempfile::tempdir().expect("deferred graph proxy directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "deferred graph proxy")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let deferred = database.deferred_memory_graph_runtime();
        assert_eq!(deferred.relational_binding(), database.registered_binding());
        assert_eq!(
            deferred.relational_verified_locator(),
            database.registered_verified_locator()
        );
        let projection = GraphProjectionIdentity::new(
            tracedecay_graph_db::GraphNamespace::new("deferred-proxy")
                .expect("valid deferred proxy namespace"),
            tracedecay_graph_db::GraphProjectionId::new("activation")
                .expect("valid deferred proxy projection"),
        );
        // Before activation the deferred route is a typed unavailable state,
        // and two deferred routes over one database share the eventual
        // runtime.
        assert!(matches!(
            deferred.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false))),
            Err(GraphDbError::Unavailable { .. })
        ));
        assert!(deferred.shares_runtime_with(&database.deferred_memory_graph_runtime()));

        let snapshot_calls = Arc::new(AtomicUsize::new(0));
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            snapshot_calls: Arc::clone(&snapshot_calls),
            _retained_database: None,
        });
        database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("bind exact graph runtime");

        // The proxy minted before activation resolves without a rebind.
        assert!(matches!(
            deferred.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false))),
            Ok(None)
        ));
        assert_eq!(snapshot_calls.load(Ordering::Acquire), 1);
        let resolved = database
            .memory_graph_runtime()
            .expect("activation publishes the resolved proxy");
        assert!(deferred.shares_runtime_with(&resolved));
        assert!(resolved.shares_runtime_with(&deferred));
    }

    #[tokio::test]
    async fn deferred_graph_proxy_stays_typed_without_activation_and_across_owner_drop() {
        let directory = tempfile::tempdir().expect("never-activated graph proxy directory");
        let database_path = directory.path().join("memory.db");
        let sibling_path = directory.path().join("sibling.db");
        let authority =
            DatabaseAuthority::acquire_test(&database_path, "never-activated graph proxy")
                .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let sibling_authority =
            DatabaseAuthority::acquire_test(&sibling_path, "never-activated graph sibling")
                .expect("sibling authority");
        let (sibling, _) = Database::publish_test_runtime(
            &sibling_path,
            &sibling_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("sibling runtime");
        let deferred = database.deferred_memory_graph_runtime();
        let projection = GraphProjectionIdentity::new(
            tracedecay_graph_db::GraphNamespace::new("never-activated")
                .expect("valid never-activated namespace"),
            tracedecay_graph_db::GraphProjectionId::new("refusal")
                .expect("valid never-activated projection"),
        );
        // A runtime that never becomes available stays a typed refusal at
        // every read — never a panic, silent success, or empty result.
        assert!(matches!(
            deferred.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false))),
            Err(GraphDbError::Unavailable { .. })
        ));
        deferred.cancel_reconciliation();
        // Unresolved routes never claim another database's eventual runtime.
        assert!(!deferred.shares_runtime_with(&sibling.deferred_memory_graph_runtime()));

        let snapshot_calls = Arc::new(AtomicUsize::new(0));
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            snapshot_calls: Arc::clone(&snapshot_calls),
            _retained_database: None,
        });
        database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("bind exact graph runtime");
        drop(runtime);
        // Activation resolved but the owner dropped: the deferred route
        // reports the same typed unavailability as the resolved proxy.
        assert!(matches!(
            deferred.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false))),
            Err(GraphDbError::Unavailable { .. })
        ));
        assert_eq!(snapshot_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn weak_graph_proxy_delegates_live_and_reports_absent_owner() {
        let directory = tempfile::tempdir().expect("weak graph proxy directory");
        let database_path = directory.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "weak graph proxy")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("database runtime");
        let snapshot_calls = Arc::new(AtomicUsize::new(0));
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestGraphRuntime {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            snapshot_calls: Arc::clone(&snapshot_calls),
            _retained_database: None,
        });
        let weak_runtime = Arc::downgrade(&runtime);
        database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("bind exact graph runtime");

        let proxy = database
            .memory_graph_runtime()
            .expect("database issues its exact weak graph proxy");
        let projection = GraphProjectionIdentity::new(
            tracedecay_graph_db::GraphNamespace::new("weak-proxy")
                .expect("valid weak proxy namespace"),
            tracedecay_graph_db::GraphProjectionId::new("delegation")
                .expect("valid weak proxy projection"),
        );
        assert!(matches!(
            proxy.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false)),),
            Ok(None)
        ));
        assert_eq!(snapshot_calls.load(Ordering::Acquire), 1);

        drop(runtime);
        assert!(weak_runtime.upgrade().is_none());
        assert!(matches!(
            proxy.verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false)),),
            Err(GraphDbError::Unavailable { .. })
        ));
        proxy.cancel_reconciliation();
        assert_eq!(snapshot_calls.load(Ordering::Acquire), 1);
    }
}
