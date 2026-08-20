use std::sync::Arc;

use super::Database;
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::VerifiedGraphRuntimePortV1;

impl Database {
    /// Binds the exact registered Grafeo runtime paired with this memory
    /// shard. A second binding is rejected so path-derived or sibling-project
    /// handles cannot silently replace the mounted authority.
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
        let mounted = self
            .inner
            .memory_graph_runtime
            .get_or_init(|| Arc::clone(&runtime));
        if Arc::ptr_eq(mounted, &runtime) {
            Ok(())
        } else {
            Err(TraceDecayError::Database {
                operation: "bind verified memory graph runtime".to_owned(),
                message: "verified memory graph runtime is already bound".to_owned(),
            })
        }
    }

    /// Returns the verified graph capability already bound to this exact
    /// relational database. Composition callers must reuse this Arc; creating
    /// a second runtime would split lifecycle and publication authority.
    #[doc(hidden)]
    pub fn memory_graph_runtime(&self) -> Option<Arc<dyn VerifiedGraphRuntimePortV1>> {
        self.inner.memory_graph_runtime.get().cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use tracedecay_domain::LocatorDigest;
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
        VerifiedGraphSnapshot,
    };
    use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

    use super::Database;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
    use crate::store_runtime::VerifiedGraphRuntimePortV1;

    struct TestGraphRuntime {
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
    }

    impl VerifiedGraphRuntimePortV1 for TestGraphRuntime {
        fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.locator
        }

        fn cancel_reconciliation(&self) {}

        fn close_reconciliation(&self) -> Result<(), GraphDbError> {
            Ok(())
        }

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
        assert!(Arc::ptr_eq(
            &database
                .memory_graph_runtime()
                .expect("bound graph runtime"),
            &runtime
        ));
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
        });

        assert!(database.bind_memory_graph_runtime(runtime).is_err());
        assert!(database.memory_graph_runtime().is_none());
    }
}
