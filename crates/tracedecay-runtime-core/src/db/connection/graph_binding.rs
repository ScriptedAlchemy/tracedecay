use std::sync::Arc;

use super::Database;
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::{VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1};

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
    /// upgrades the weak graph port privately for each operation.
    #[must_use]
    pub fn memory_graph_runtime(&self) -> Option<VerifiedGraphRuntimeWeakProxyV1> {
        Some(VerifiedGraphRuntimeWeakProxyV1::new(
            self.registered_binding().clone(),
            self.registered_verified_locator().clone(),
            self.inner.memory_graph_runtime.get()?.clone(),
        ))
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
        VerifiedGraphSnapshot,
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
