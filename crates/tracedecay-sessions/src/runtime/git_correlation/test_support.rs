//! Hermetic Git-evidence graph runtime shared by this crate's tests.
//!
//! Publishes each manifest into an in-memory verified snapshot
//! (`VerifiedGraphSnapshot::memory`) and serves it back, standing in for the
//! registered project graph runtime. Absent publication answers the same
//! typed `Ok(None)` empty start as the production registry so recovery paths
//! exercise their real fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

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

/// Blocks gated `verified_snapshot` readers in place of fake slow IO, so
/// tests order overlap and cancellation explicitly instead of racing
/// wall-clock sleeps. Release is the only wake-up: a test that cancels a
/// gated reader must store its cancellation signal first and then release,
/// so the reader's post-gate check observes it deterministically.
#[derive(Default)]
struct SnapshotReadGate {
    state: Mutex<SnapshotReadGateState>,
    changed: Condvar,
}

#[derive(Default)]
struct SnapshotReadGateState {
    enabled: bool,
    entered: usize,
    released: bool,
}

impl SnapshotReadGate {
    fn enable(&self) {
        self.state.lock().unwrap().enabled = true;
    }

    /// Reader side: record entry and hold until the gate is released.
    /// A gate that was never enabled passes straight through.
    fn pass(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return;
        }
        state.entered += 1;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn await_reader(&self) {
        let mut state = self.state.lock().unwrap();
        while state.entered == 0 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }

    fn readers_entered(&self) -> usize {
        self.state.lock().unwrap().entered
    }
}

pub(crate) struct MemoryEvidenceGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    snapshot: Mutex<Option<VerifiedGraphSnapshot>>,
    publication_lock: Mutex<()>,
    cancelled: AtomicBool,
    cancel_after_publish: AtomicBool,
    read_gate: SnapshotReadGate,
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
            publication_lock: Mutex::new(()),
            cancelled: AtomicBool::new(false),
            cancel_after_publish: AtomicBool::new(false),
            read_gate: SnapshotReadGate::default(),
        }
    }
}

impl MemoryEvidenceGraphRuntime {
    pub(crate) fn git_evidence_publication_lock(&self) -> &Mutex<()> {
        &self.publication_lock
    }

    pub(crate) fn cancel_request_after_next_publish(&self) {
        self.cancel_after_publish.store(true, Ordering::Release);
    }

    /// Holds every subsequent `verified_snapshot` read at a gate until
    /// [`Self::release_gated_snapshot_reads`] runs.
    pub(crate) fn gate_snapshot_reads(&self) {
        self.read_gate.enable();
    }

    /// Blocks until at least one reader is held inside `verified_snapshot`.
    pub(crate) fn await_gated_snapshot_reader(&self) {
        self.read_gate.await_reader();
    }

    /// Opens the gate permanently; held and future readers pass through.
    pub(crate) fn release_gated_snapshot_reads(&self) {
        self.read_gate.release();
    }

    pub(crate) fn gated_snapshot_readers_entered(&self) -> usize {
        self.read_gate.readers_entered()
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
        if self.cancel_after_publish.swap(false, Ordering::AcqRel) {
            cancelled.store(true, Ordering::Release);
        }
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
        self.read_gate.pass();
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
