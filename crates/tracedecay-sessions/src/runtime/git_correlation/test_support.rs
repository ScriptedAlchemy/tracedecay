//! Hermetic Git-evidence graph runtime shared by this crate's tests.
//!
//! Publishes each manifest into an in-memory verified snapshot
//! (`VerifiedGraphSnapshot::memory`) and serves it back, standing in for the
//! registered project graph runtime. Absent publication answers the same
//! typed `Ok(None)` empty start as the production registry so recovery paths
//! exercise their real fallback.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    NeverCancelled, VerifiedGraphSnapshot,
};

use super::store::GitEvidenceGraphRuntimePort;

#[derive(Default)]
pub(crate) struct MemoryEvidenceGraphRuntime {
    snapshot: Mutex<Option<VerifiedGraphSnapshot>>,
}

impl GitEvidenceGraphRuntimePort for MemoryEvidenceGraphRuntime {
    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        *self.snapshot.lock().unwrap() = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn verified_snapshot(
        &self,
        _projection: &GraphProjectionIdentity,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
}
