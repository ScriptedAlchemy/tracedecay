//! Hermetic Git-evidence graph runtime shared by this crate's tests.
//!
//! Publishes each manifest into an in-memory verified snapshot
//! (`VerifiedGraphSnapshot::memory`) and serves it back, standing in for the
//! registered project graph runtime. Absent publication answers the same
//! typed `Ok(None)` empty start as the production registry so recovery paths
//! exercise their real fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_store::{
    FactReadControl, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1,
};

pub(crate) struct MemoryEvidenceGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    snapshot: Mutex<Option<VerifiedGraphSnapshot>>,
    cancelled: AtomicBool,
}

impl Default for MemoryEvidenceGraphRuntime {
    fn default() -> Self {
        let shard_id = StoreShardIdV1::project(
            BrainId::new("brain.git-evidence-test").unwrap(),
            UserProfileId::new("profile.git-evidence-test").unwrap(),
            ProjectId::new("project.git-evidence-test").unwrap(),
        );
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        Self {
            binding: StoreRuntimeBindingV1::new(
                shard_id.clone(),
                incarnation,
                StoreAuthorityEpochV1::new(1).unwrap(),
            ),
            locator: VerifiedStoreLocatorV1::new(
                shard_id,
                incarnation,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            ),
            snapshot: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl VerifiedGraphRuntimePortV1 for MemoryEvidenceGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn close_reconciliation(&self) -> Result<(), GraphDbError> {
        Ok(())
    }

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if cancelled.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        *self.snapshot.lock().unwrap() = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        *self.snapshot.lock().unwrap() = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if read_control.interrupted() || self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        Ok(self
            .snapshot
            .lock()
            .unwrap()
            .as_ref()
            .filter(|snapshot| snapshot.projection() == projection)
            .cloned())
    }
}
