use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::db::Database;
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;

struct ProfileMemoryGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    manifest: Mutex<Option<GraphGenerationManifest>>,
}

impl ProfileMemoryGraphRuntime {
    fn new(database: &Database) -> Self {
        Self {
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
            manifest: Mutex::new(None),
        }
    }

    fn remember(
        &self,
        manifest: &GraphGenerationManifest,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        *self
            .manifest
            .lock()
            .map_err(|_| GraphDbError::invalid("profile memory graph fixture lock poisoned"))? =
            Some(manifest.clone());
        Ok(snapshot)
    }
}

impl VerifiedGraphRuntimePortV1 for ProfileMemoryGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {}

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        self.remember(manifest)
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.remember(manifest)
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if read_control.interrupted() {
            return Err(GraphDbError::Cancelled);
        }
        let manifest = self
            .manifest
            .lock()
            .map_err(|_| GraphDbError::invalid("profile memory graph fixture lock poisoned"))?
            .clone();
        match manifest {
            Some(manifest) if &manifest.projection == projection => Ok(Some(
                VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))?,
            )),
            Some(_) | None => Ok(None),
        }
    }
}

/// Binds the profile memory graph fixture and returns the strong port.
///
/// `Database::bind_memory_graph_runtime` retains only a weak binding, so the
/// caller must hold the returned `Arc` for as long as graph operations should
/// stay mountable.
pub(super) fn bind_profile_memory_graph_runtime(
    database: &Database,
) -> Arc<dyn VerifiedGraphRuntimePortV1> {
    let runtime: Arc<dyn VerifiedGraphRuntimePortV1> =
        Arc::new(ProfileMemoryGraphRuntime::new(database));
    database
        .bind_memory_graph_runtime(Arc::clone(&runtime))
        .expect("bind profile memory graph fixture");
    runtime
}
