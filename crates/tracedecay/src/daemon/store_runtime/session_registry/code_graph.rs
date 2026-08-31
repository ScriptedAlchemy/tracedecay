use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{CodeGenerationId, RefId, RepositoryId, WorktreeId, canonical_sha256};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbOwnerAttachmentV1,
    GraphDbRegistration, GraphGenerationDependency, GraphGenerationManifest, GraphIdempotencyKey,
    GraphProjectionIdentity, GraphProjectorRevision, GraphPublicationPreparationV1,
    GraphReplayCollectionOutcome, GraphWriteBatch, SealedCodeGenerationReplay,
    VerifiedGenerationBatchCommit, VerifiedGraphCommit, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalCodeGraphStoreLeaseV1, CanonicalGraphStoreOwnerRetirementTargetV1, StoreRuntimeKey,
};
use tracedecay_store::{
    CodeShardScopeV1, FactReadControl, GraphGenerationIdV1, GraphPendingReplayDiscardOutcomeV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayLookupV1, GraphPublicationReplayRecordV1, GraphPublicationStoreErrorV1,
    GraphPublicationStoreV1, GraphReplayAppendOutcomeV1, GraphVerifiedHeadV1, ProjectId,
    RetainedGraphStoreLeaseV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, SemanticVectorStageBatchReceipt, SemanticVectorStageCancelOutcome,
    SemanticVectorStageKey, SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, SemanticVectorStagingStore,
    StoreShardIdV1,
};

use super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};
use tracedecay_code_index_runtime::{
    CodeGraphReplayBindingV1, CodeGraphSeatLeaseV1, CodeGraphSeatRuntimePortV1,
};

mod memory_runtime;
pub(super) use memory_runtime::{
    inline_graph_publication_input_digest, schedule_bound_memory_graph_reconciliation,
};
pub(super) mod graph_attachment;
#[cfg(test)]
mod sealed_publication_tests;
mod seals;
mod semantic_vector;
mod semantic_vector_runtime;
use seals::{
    finalize_project_graph_replay_unlink, lock_project_graph_replay_pool,
    sealed_digest_from_generation_file, stage_project_graph_replay_unlink,
};
use semantic_vector_runtime::DaemonVerifiedSemanticVectorGraphRuntimeV1;

const GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// Effectively-unbounded budget for background graph work (sealed projection
/// and generation publication); cancellation is the governing mechanism.
const GRAPH_BACKGROUND_OPERATION_BUDGET: Duration = Duration::from_hours(168);
const GRAPH_OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// How many orphaned pending predecessors one publication attempt will
/// complete before reporting Conflict. Each completion advances the verified
/// head by one, so even a journal wedged across many interrupted boots drains
/// across a few reconcile passes rather than blocking forever.
const MAX_PENDING_REPLAY_COMPLETIONS_V1: usize = 8;

#[derive(Clone, Copy)]
enum CodeGraphPublicationConflictStageV1 {
    ActiveReplayPublish,
    RetiredReplay,
    PendingCompletionLimit,
    PendingPredecessorPublish,
    VerifiedHeadRefreshLimit,
    ReplayAppend,
    FinalPublish,
}

impl CodeGraphPublicationConflictStageV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ActiveReplayPublish => "active_replay_publish",
            Self::RetiredReplay => "retired_replay",
            Self::PendingCompletionLimit => "pending_completion_limit",
            Self::PendingPredecessorPublish => "pending_predecessor_publish",
            Self::VerifiedHeadRefreshLimit => "verified_head_refresh_limit",
            Self::ReplayAppend => "replay_append",
            Self::FinalPublish => "final_publish",
        }
    }
}

fn observe_code_graph_publication<T>(
    stage: CodeGraphPublicationConflictStageV1,
    result: std::result::Result<T, GraphDbError>,
) -> std::result::Result<T, GraphDbError> {
    result.map_err(|error| {
        if matches!(error, GraphDbError::Conflict { .. }) {
            let reason = stage.as_str();
            tracing::warn!(
                event = "code_graph_publication_conflict",
                reason,
                "code graph publication reached a conflicting durable authority"
            );
            #[cfg(feature = "hotpath")]
            hotpath::val!("code_graph.publication.conflict_reason").set(&reason);
        }
        error
    })
}

const fn sealed_projection_deadline() -> Duration {
    // The background projection has no wall-clock bail-out. It is bounded by
    // cancellation (request and lifecycle), which is the mechanism that
    // reclaims a genuinely wedged projection; a wall clock cannot tell
    // "wedged" from "slower than a modeled throughput", and killing a
    // progressing projection only re-runs the same work into the same wall,
    // turning slow into never. The registration API wants an `Instant`, so
    // "no deadline" is expressed as a far-future one. Projection latency
    // itself is the number to fix (see the code_graph hotpath spans), not a
    // policy to tune.
    GRAPH_BACKGROUND_OPERATION_BUDGET
}

struct AtomicGraphCancellationV1 {
    cancelled: Arc<AtomicBool>,
}

struct FactReadGraphCancellationV1(FactReadControl);

impl GraphCancellation for FactReadGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.0.interrupted()
    }
}

impl AtomicGraphCancellationV1 {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled }
    }
}

impl GraphCancellation for AtomicGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

struct MaintenanceGraphCancellationV1(tracedecay_session_memory::context::CancellationToken);

impl GraphCancellation for MaintenanceGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct GraphPublicationProbeV1 {
    request_cancellation: Arc<dyn GraphCancellation>,
    lifecycle_cancellation: Arc<dyn GraphCancellation>,
    deadline_at: Instant,
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    commit_started: AtomicBool,
    /// One warn per probe when the deadline first trips: `interruption()` is
    /// polled from hot loops, and `DeadlineExceeded` is a unit error that
    /// cannot otherwise be attributed to the deadline that armed it.
    deadline_warned: AtomicBool,
}

impl RuntimeRequestProbeV1 for GraphPublicationProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if self.request_cancellation.is_cancelled() || self.lifecycle_cancellation.is_cancelled() {
            Some(RuntimeInterruptionV1::Cancelled)
        } else if Instant::now() >= self.deadline_at {
            if !self.deadline_warned.swap(true, Ordering::AcqRel) {
                tracing::warn!(
                    event = "graph_db_deadline_exceeded",
                    deadline_id = self.deadline.deadline_id.as_str(),
                    "graph publication probe deadline exceeded"
                );
            }
            Some(RuntimeInterruptionV1::DeadlineExceeded)
        } else {
            None
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    fn requires_isolated_commit(&self) -> bool {
        true
    }
}

struct CombinedAtomicGraphCancellationV1 {
    local: Arc<AtomicBool>,
    registry: Option<Arc<AtomicBool>>,
}

impl GraphCancellation for CombinedAtomicGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.local.load(Ordering::Acquire)
            || self
                .registry
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
    }
}

fn graph_lifecycle_cancellation(
    local: &Arc<AtomicBool>,
    registry: Option<&Arc<AtomicBool>>,
) -> Arc<dyn GraphCancellation> {
    Arc::new(CombinedAtomicGraphCancellationV1 {
        local: Arc::clone(local),
        registry: registry.map(Arc::clone),
    })
}

/// Per-shard table of publication keys with a sealed publish in flight.
///
/// The seat pass and the background reconcile publish the same sealed
/// generation; without this, both would run the corpus-sized prepare —
/// native staging, the sealed-store build, the digest proof — and interleave
/// staging pages in one physical namespace. A publisher that finds its key in
/// flight waits for the winner and then resumes through the idempotent
/// historical arm inside prepare. Publishers of different keys proceed
/// independently, and no read or serving path ever touches this table.
pub(crate) struct CodeGraphPublicationFlightV1 {
    in_flight: Mutex<std::collections::BTreeSet<GraphPublicationKeyV1>>,
    settled: std::sync::Condvar,
}

impl Default for CodeGraphPublicationFlightV1 {
    fn default() -> Self {
        Self {
            in_flight: Mutex::new(std::collections::BTreeSet::new()),
            settled: std::sync::Condvar::new(),
        }
    }
}

/// RAII flight claim for one publication key; dropping it wakes every waiter.
pub(crate) struct CodeGraphPublicationFlightClaimV1<'a> {
    flight: &'a CodeGraphPublicationFlightV1,
    key: GraphPublicationKeyV1,
}

impl Drop for CodeGraphPublicationFlightClaimV1<'_> {
    fn drop(&mut self) {
        let mut in_flight = self
            .flight
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        in_flight.remove(&self.key);
        drop(in_flight);
        self.flight.settled.notify_all();
    }
}

/// How long one flight-table wait sleeps between interruption polls. The
/// condvar wakes waiters the moment the winner settles; the timeout exists
/// only so a cancelled or expired request answers its typed interruption
/// instead of sleeping out a corpus-sized peer publish.
const PUBLICATION_FLIGHT_INTERRUPTION_POLL: Duration = Duration::from_millis(250);

impl CodeGraphPublicationFlightV1 {
    /// Claims `key`, waiting out a same-key publish already in flight.
    ///
    /// The wait is where a twin publisher parks instead of duplicating the
    /// prepare; it observes `interruption` so cancellation and deadlines stay
    /// typed. Waiting happens with no other publication lock held.
    fn claim<'a>(
        &'a self,
        key: &GraphPublicationKeyV1,
        interruption: &dyn Fn() -> std::result::Result<(), GraphDbError>,
    ) -> std::result::Result<CodeGraphPublicationFlightClaimV1<'a>, GraphDbError> {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while in_flight.contains(key) {
            interruption()?;
            let (guard, _timed_out) = hotpath::measure_block!(
                "daemon.session_registry.publish_snapshot.peer_wait",
                self.settled
                    .wait_timeout(in_flight, PUBLICATION_FLIGHT_INTERRUPTION_POLL)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            );
            in_flight = guard;
        }
        interruption()?;
        in_flight.insert(key.clone());
        Ok(CodeGraphPublicationFlightClaimV1 {
            flight: self,
            key: key.clone(),
        })
    }
}

/// The registry-owned per-code-shard publication locks.
///
/// `gate` is the serving gate: a leaf lock held only across the short
/// storage-ordered slices of a publication — manifest-provider bind plus
/// journal classification, each replay append, and the verified-head
/// CAS-plus-install swap. The corpus-sized work (native staging, the
/// sealed-store build, digest proofs, boot-from-sealed recovery) runs with no
/// gate held. `flight` dedupes same-key publishers; see
/// [`CodeGraphPublicationFlightV1`].
///
/// `build` admits one corpus-sized sealed publish at a time across the whole
/// shard, keys included. Every scope of a project stages into the one shared
/// staging store, and each in-flight publish holds a decoded generation, a
/// projection manifest, staging pages, and the sealed-store copy at once —
/// several times the corpus in transient memory. A daemon recovering a
/// quarantined store re-publishes every open scope's generation
/// concurrently, and unbounded overlap of those transients is what grew the
/// replay working set past physical memory (observed live: 39.9–45.6 GB anon
/// RSS against a 12.4 GB store while several scopes staged at once). Waiters
/// observe their typed interruption while parked, and no read or serving
/// path ever takes this permit.
///
/// Lock order is `replay-pool file lock -> build -> flight -> gate`; the
/// gate acquires nothing while held.
#[derive(Default)]
pub(crate) struct CodeGraphShardPublicationLocksV1 {
    gate: Mutex<()>,
    build: Mutex<()>,
    flight: CodeGraphPublicationFlightV1,
}

/// How long one build-permit wait sleeps between interruption polls, matching
/// the flight-table cadence: the permit turns over at corpus-publish
/// granularity, and the poll only bounds how late a cancelled or expired
/// waiter answers its typed interruption.
const PUBLICATION_BUILD_INTERRUPTION_POLL: Duration = Duration::from_millis(250);

impl CodeGraphShardPublicationLocksV1 {
    /// Claims the shard-wide corpus build permit, observing `interruption`
    /// while parked behind a peer's corpus-sized publish.
    fn claim_build<'a>(
        &'a self,
        interruption: &dyn Fn() -> std::result::Result<(), GraphDbError>,
    ) -> std::result::Result<std::sync::MutexGuard<'a, ()>, GraphDbError> {
        loop {
            interruption()?;
            match self.build.try_lock() {
                Ok(permit) => return Ok(permit),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    return Ok(poisoned.into_inner());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    hotpath::measure_block!(
                        "daemon.session_registry.publish_snapshot.build_wait",
                        std::thread::sleep(PUBLICATION_BUILD_INTERRUPTION_POLL)
                    );
                }
            }
        }
    }
}

pub(crate) struct RetainedCodeGraphRuntimeV1 {
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    graph_manifest_provider: Arc<super::code_graph_manifest::DaemonCodeGraphManifestProviderV1>,
    authority: Arc<CanonicalCodeGraphStoreLeaseV1>,
    project_database: Arc<tracedecay_runtime_core::db::Database>,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    code_shard: StoreShardIdV1,
    generation_id: CodeGenerationId,
    generations_root: std::path::PathBuf,
    replay_root: std::path::PathBuf,
    sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest,
    lifecycle_cancelled: Arc<AtomicBool>,
    /// Registry-owned per-shard publication locks; see
    /// `DaemonSessionRuntimeRegistryV1::code_graph_publication_gates`.
    publication_locks: Arc<CodeGraphShardPublicationLocksV1>,
}

/// Retirement releases the decoded-generation offer this runtime commissioned.
///
/// The offer exists to spare the activation window a second decode of bytes
/// that stay durable on disk. Once this runtime retires, no consumer can reach
/// that window again, so continuing to retain a whole decoded generation is
/// pure resident cost — and before this nothing removed an offer at all, which
/// is one of the holders that let a 16GiB admission limit sit inside a 42GiB
/// process. Dropping it never loses truth: the canonical seal read remains the
/// authority and reconstructs the same payload.
impl Drop for RetainedCodeGraphRuntimeV1 {
    fn drop(&mut self) {
        let released_bytes = self
            .graph_manifest_provider
            .release_decoded_offer(&self.authority.binding().shard_id);
        if released_bytes > 0 {
            tracing::debug!(
                event = "code_graph_decoded_offer_released",
                released_bytes,
                generation = %self.generation_id.as_str(),
                "released the retiring runtime's decoded generation offer"
            );
        }
    }
}

/// Memory-shard publication runtime for immutable non-code graph journeys.
///
/// Code and journey projections share the daemon's sole `GraphDbRegistry` and
/// physical Grafeo store. Journey manifests use canonical inline replay; code
/// generations keep their sealed replay source through
/// [`RetainedCodeGraphRuntimeV1`].
pub(crate) struct RetainedVerifiedGraphRuntimeV1 {
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    database: tracedecay_runtime_core::db::DatabaseOwnerV1,
    graph: GraphDbOwnerAttachmentV1,
    store_target: Mutex<Option<CanonicalGraphStoreOwnerRetirementTargetV1>>,
    relational_binding: tracedecay_store::StoreRuntimeBindingV1,
    relational_verified_locator: tracedecay_store::VerifiedStoreLocatorV1,
    operation_admission: Mutex<MemoryGraphOperationAdmissionV1>,
    publication_gate: Mutex<()>,
    lifecycle_cancelled: Arc<AtomicBool>,
    registry_lifecycle_cancelled: Arc<AtomicBool>,
}

enum MemoryGraphOperationAdmissionV1 {
    Ready,
    Retiring,
}

/// Fences synchronous graph-port issuance while the daemon map retains the
/// exact owner through a coordinated retirement attempt.
pub(crate) struct MemoryGraphOperationRetirementReservationV1<'a> {
    runtime: &'a RetainedVerifiedGraphRuntimeV1,
    armed: bool,
}

impl RetainedVerifiedGraphRuntimeV1 {
    pub(crate) fn issue_database_lease(
        &self,
    ) -> std::result::Result<tracedecay_runtime_core::db::Database, GraphDbError> {
        self.require_operation_admission()?;
        self.database.issue_lease().map_err(|error| {
            GraphDbError::unavailable(format!(
                "memory database owner cannot issue a client: {error:?}"
            ))
        })
    }

    pub(crate) fn take_store_graph_retirement_target(
        &self,
    ) -> std::result::Result<CanonicalGraphStoreOwnerRetirementTargetV1, GraphDbError> {
        self.store_target
            .lock()
            .map_err(|_| {
                GraphDbError::unavailable("memory graph retirement target lock is poisoned")
            })?
            .take()
            .ok_or(GraphDbError::conflict(
                "code_graph.take_store_graph_retirement_target",
            ))
    }

    pub(crate) fn restore_store_graph_retirement_target(
        &self,
        target: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> std::result::Result<(), GraphDbError> {
        let mut retained = self.store_target.lock().map_err(|_| {
            GraphDbError::unavailable("memory graph retirement target lock is poisoned")
        })?;
        if retained.is_some() {
            return Err(GraphDbError::conflict(
                "code_graph.restore_store_graph_retirement_target",
            ));
        }
        *retained = Some(target);
        Ok(())
    }

    pub(crate) fn graph_retirement_target(&self) -> tracedecay_graph_db::GraphDbRetirementTarget {
        self.graph.retirement_target()
    }

    pub(crate) fn reserve_database_retirement(
        &self,
    ) -> std::result::Result<
        tracedecay_runtime_core::db::DatabaseOwnerRetirementReservationV1,
        GraphDbError,
    > {
        self.database.reserve_retirement().map_err(|error| {
            GraphDbError::unavailable(format!(
                "memory database owner cannot reserve retirement: {error:?}"
            ))
        })
    }

    fn require_operation_admission(&self) -> std::result::Result<(), GraphDbError> {
        match *self.operation_admission.lock().map_err(|_| {
            GraphDbError::unavailable("memory graph operation admission lock is poisoned")
        })? {
            MemoryGraphOperationAdmissionV1::Ready => Ok(()),
            MemoryGraphOperationAdmissionV1::Retiring => Err(GraphDbError::conflict(
                "code_graph.require_operation_admission",
            )),
        }
    }

    pub(crate) fn reserve_operation_retirement(
        &self,
    ) -> std::result::Result<MemoryGraphOperationRetirementReservationV1<'_>, GraphDbError> {
        let mut admission = self.operation_admission.lock().map_err(|_| {
            GraphDbError::unavailable("memory graph operation admission lock is poisoned")
        })?;
        if matches!(*admission, MemoryGraphOperationAdmissionV1::Retiring) {
            return Err(GraphDbError::conflict(
                "code_graph.reserve_operation_retirement.retiring",
            ));
        }
        *admission = MemoryGraphOperationAdmissionV1::Retiring;
        Ok(MemoryGraphOperationRetirementReservationV1 {
            runtime: self,
            armed: true,
        })
    }

    #[hotpath::measure(label = "daemon.session_registry.publish_manifest")]
    pub(crate) fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        let _publication = self.publication_gate.lock().map_err(|_| {
            GraphDbError::unavailable("verified graph publication gate is poisoned")
        })?;
        if request_cancelled.load(Ordering::Acquire)
            || self.lifecycle_cancelled.load(Ordering::Acquire)
            || self.registry_lifecycle_cancelled.load(Ordering::Acquire)
        {
            return Err(GraphDbError::Cancelled);
        }
        let database = self.issue_database_lease()?;
        let mut storage = database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let graph = self.graph.issue_lease()?;
        // Publication of a staged generation is background reconcile work
        // bounded by the request/lifecycle cancellation in this same probe,
        // not by wall clock: a 30s wall killed ~6-minute publishes on large
        // graphs and the reconcile retried the identical work forever (see
        // sealed_projection_deadline). Foreground graph reads keep
        // GRAPH_OPERATION_DEADLINE.
        let deadline_at = Instant::now() + GRAPH_BACKGROUND_OPERATION_BUDGET;
        let identity = manifest.generation.as_str();
        let cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!("graph-publish:{identity}"))
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 1,
        };
        let deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!("graph-publish-deadline:{identity}"))
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let request_cancellation: Arc<dyn GraphCancellation> = Arc::new(
            AtomicGraphCancellationV1::new(Arc::clone(&request_cancelled)),
        );
        let probe = GraphPublicationProbeV1 {
            request_cancellation: Arc::clone(&request_cancellation),
            lifecycle_cancellation: graph_lifecycle_cancellation(
                &self.lifecycle_cancelled,
                Some(&self.registry_lifecycle_cancelled),
            ),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
            deadline_warned: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: tracedecay_application::clock::now_micros(),
            deadline: deadline_identity,
            cancellation: cancellation_identity,
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.relational_binding.shard_id.clone(),
            namespace: tracedecay_store::GraphNamespaceV1::new(
                manifest.projection.namespace.as_str(),
            )
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(manifest.projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let publication_key = GraphPublicationKeyV1::new(
            relational_projection.clone(),
            GraphGenerationIdV1::new(manifest.generation.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        // Observe cancellation and the deadline before touching the
        // publication authority: a registry torn down by lifecycle shutdown
        // must answer typed cancellation, not storage unavailability.
        match probe.interruption() {
            Some(RuntimeInterruptionV1::Cancelled) => return Err(GraphDbError::Cancelled),
            Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                return Err(GraphDbError::DeadlineExceeded);
            }
            None => {}
        }
        // The verified-head CAS inside `publish_verified` is its own
        // irreversible durable commit; the journal append above already
        // consumes this flow's first at-most-once commit grant, so the
        // publish phase gets a second arbitration context (same shape as the
        // sealed code-generation publish closure below).
        let publish_cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "graph-publish-commit:{identity}"
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 2,
        };
        let publish_deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "graph-publish-commit-deadline:{identity}"
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        // Minted fresh per publication rather than once for the whole flow:
        // the probe's commit grant is one-shot, and completing a pending
        // predecessor consumes a grant of its own before this flow's replay
        // publishes through another.
        let publish_journaled = |storage: &mut dyn GraphPublicationStoreV1,
                                 key: &GraphPublicationKeyV1|
         -> std::result::Result<VerifiedGraphCommit, GraphDbError> {
            let publish_probe = GraphPublicationProbeV1 {
                request_cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancellation: graph_lifecycle_cancellation(
                    &self.lifecycle_cancelled,
                    Some(&self.registry_lifecycle_cancelled),
                ),
                deadline_at,
                cancellation: publish_cancellation_identity.clone(),
                deadline: publish_deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
                deadline_warned: AtomicBool::new(false),
            };
            let publish_control = RuntimeRequestControlV1 {
                requested_at: tracedecay_application::clock::now_micros(),
                deadline: publish_deadline_identity.clone(),
                cancellation: publish_cancellation_identity.clone(),
            };
            let publish_context =
                GraphPublicationOperationContextV1::new(&publish_control, &publish_probe)
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            self.graph_registry
                .publish_verified_with_lease(&graph, storage, &publish_context, key)
        };
        let input = inline_graph_publication_input_digest(&publication_key, manifest)?;
        let requested_replay = |prior| {
            manifest.relational_replay(
                self.relational_binding.shard_id.clone(),
                idempotency_key.clone(),
                input.clone(),
                prior,
                &|| match probe.interruption() {
                    Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                    Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                        Err(GraphDbError::DeadlineExceeded)
                    }
                    None => Ok(()),
                },
            )
        };
        match storage
            .replay(&publication_key, &context)
            .map_err(map_publication_error)?
        {
            GraphPublicationReplayLookupV1::Active(journaled) => {
                if requested_replay(journaled.publication.expected_prior_head.clone())?
                    != journaled.publication
                {
                    return Err(GraphDbError::conflict(
                        "code_graph.publish_verified_manifest",
                    ));
                }
                let head = storage
                    .verified_head(&relational_projection, &context)
                    .map_err(map_publication_error)?;
                if head
                    .as_ref()
                    .is_some_and(|head| head.key == publication_key)
                {
                    return self.graph_registry.recover_verified_snapshot_with_lease(
                        &graph,
                        &mut storage,
                        &context,
                        &relational_projection,
                    );
                }
                // A newer publication already won the verified head, so this
                // journaled replay is superseded history: republishing it is a
                // stale conflict, never a resumable interruption.
                if head
                    .as_ref()
                    .is_some_and(|head| head.sequence > journaled.sequence)
                {
                    return Err(GraphDbError::conflict(
                        "code_graph.publish_verified_manifest",
                    ));
                }
                // The replay is journaled but the verified head never advanced
                // to it: an earlier publish was interrupted between the journal
                // append and the head CAS. `publish_verified` is idempotent
                // over the journaled replay and computes the authoritative
                // verdict (completes the pending publication, dedupes an exact
                // replay, or reports a true conflict) — answering Conflict here
                // would wedge the projection permanently.
                let publication = publish_journaled(&mut storage, &publication_key)?;
                return Ok(publication.snapshot);
            }
            GraphPublicationReplayLookupV1::Retired(_) => {
                return Err(GraphDbError::conflict(
                    "code_graph.publish_verified_manifest",
                ));
            }
            GraphPublicationReplayLookupV1::Missing => {}
        }
        let prior = storage
            .verified_head(&relational_projection, &context)
            .map_err(map_publication_error)?;
        let mut replay = requested_replay(prior)?;
        // Same ordered-journal recovery as the sealed code-generation path: a
        // predecessor journaled by an interrupted publisher can only land
        // through a live one, so complete it and re-append against the
        // advanced head instead of wedging on Conflict.
        let mut completed_predecessors = 0usize;
        loop {
            match storage
                .append_replay(&replay, &context)
                .map_err(map_publication_error)?
            {
                GraphReplayAppendOutcomeV1::Appended(_)
                | GraphReplayAppendOutcomeV1::ExactReplay(_)
                | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => break,
                GraphReplayAppendOutcomeV1::PendingReplayConflict { pending } => {
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return Err(GraphDbError::conflict(
                            "code_graph.publish_verified_manifest",
                        ));
                    }
                    completed_predecessors += 1;
                    publish_journaled(&mut storage, &pending.publication.key)?;
                    let prior = storage
                        .verified_head(&relational_projection, &context)
                        .map_err(map_publication_error)?;
                    replay = requested_replay(prior)?;
                }
                GraphReplayAppendOutcomeV1::VerifiedHeadConflict { actual } => {
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return Err(GraphDbError::conflict(
                            "code_graph.publish_verified_manifest",
                        ));
                    }
                    completed_predecessors += 1;
                    replay = requested_replay(actual)?;
                }
                GraphReplayAppendOutcomeV1::Conflict { .. }
                | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. } => {
                    return Err(GraphDbError::conflict(
                        "code_graph.publish_verified_manifest",
                    ));
                }
            }
        }
        let publication = publish_journaled(&mut storage, &replay.key)?;
        Ok(publication.snapshot)
    }

    pub(crate) fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> std::result::Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if self.lifecycle_cancelled.load(Ordering::Acquire)
            || self.registry_lifecycle_cancelled.load(Ordering::Acquire)
        {
            return Err(GraphDbError::Cancelled);
        }
        let database = self.issue_database_lease()?;
        let mut storage = database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let graph = self.graph.issue_lease()?;
        let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
        let cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "graph-read:{}",
                projection.projection.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 1,
        };
        let deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "graph-read-deadline:{}",
                projection.projection.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let request_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(FactReadGraphCancellationV1(read_control));
        let probe = GraphPublicationProbeV1 {
            request_cancellation: Arc::clone(&request_cancellation),
            lifecycle_cancellation: graph_lifecycle_cancellation(
                &self.lifecycle_cancelled,
                Some(&self.registry_lifecycle_cancelled),
            ),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
            deadline_warned: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: tracedecay_application::clock::now_micros(),
            deadline: deadline_identity,
            cancellation: cancellation_identity,
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.relational_binding.shard_id.clone(),
            namespace: tracedecay_store::GraphNamespaceV1::new(projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        // A projection that has never published a verified head is a typed
        // empty start, not an unavailability error (same pre-check as
        // `recover_semantic_vector_projection`).
        if storage
            .verified_head(&relational_projection, &context)
            .map_err(map_publication_error)?
            .is_none()
        {
            return Ok(None);
        }
        self.graph_registry
            .recover_verified_snapshot_with_lease(
                &graph,
                &mut storage,
                &context,
                &relational_projection,
            )
            .map(Some)
    }
}

impl MemoryGraphOperationRetirementReservationV1<'_> {
    /// Keeps operation admission closed after reconciliation cancellation has
    /// crossed its irreversible boundary. Graph registry retirement remains
    /// the sole graph-close authority.
    pub(crate) fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for MemoryGraphOperationRetirementReservationV1<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut admission = self
            .runtime
            .operation_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*admission, MemoryGraphOperationAdmissionV1::Retiring) {
            *admission = MemoryGraphOperationAdmissionV1::Ready;
        }
        self.armed = false;
    }
}

/// The arm one sealed publication takes, decided under the classification
/// gate slice from the journaled replay and the verified head.
enum SealedPublicationClassificationV1 {
    /// This publication already owns the verified head; recover it for reads.
    RecoverPublished,
    /// An active journaled replay whose head has not advanced; resume it.
    ResumeJournaled,
    /// The journaled replay was retired; publication is a typed conflict.
    RetiredConflict,
    /// No journal entry yet; append the replay and publish it.
    AppendAndPublish,
}

/// Pre-gate outputs of one sealed code-generation publication: everything
/// that is a pure function of the immutable published generation and this
/// runtime's identity. Computed before any publication lock is taken so
/// racing publishers project concurrently and the gate serializes only the
/// storage-ordered slices.
struct PreparedSealedPublicationV1 {
    projection_deadline: Duration,
    deadline_at: Instant,
    manifest: Arc<GraphGenerationManifest>,
    relational_projection: GraphProjectionIdentityV1,
    source: SealedCodeGenerationReplay,
    idempotency_key: GraphIdempotencyKey,
    publication_key: GraphPublicationKeyV1,
    request_cancelled: Arc<AtomicBool>,
}

impl RetainedCodeGraphRuntimeV1 {
    pub(crate) fn authority(&self) -> Arc<CanonicalCodeGraphStoreLeaseV1> {
        Arc::clone(&self.authority)
    }

    /// Drops aborted catalog/manifest staging files for this sealed digest.
    /// A retry must not inherit another attempt's `.read-bundle-*.tmp` scratch.
    #[hotpath::measure(label = "daemon.session_registry.sweep_read_bundle_temporaries")]
    pub(crate) fn sweep_aborted_read_bundle_temporaries(
        &self,
    ) -> std::result::Result<(), GraphDbError> {
        tracedecay_graph_db::sweep_aborted_sealed_read_bundle_temporaries(
            &self.generations_root,
            &self.sealed_state_digest,
        )
    }

    pub(crate) fn semantic_vector_identity(
        &self,
    ) -> std::result::Result<
        (
            ProjectId,
            RepositoryId,
            WorktreeId,
            CodeGenerationId,
            GraphGenerationDependency,
        ),
        GraphDbError,
    > {
        let revision = GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )?;
        let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
            self.authority.namespace().clone(),
        )
        .map_err(map_code_graph_error)?;
        let generation = tracedecay_code_index::graph_projection::code_graph_generation_id(
            &self.generation_id,
            &revision,
        )
        .map_err(map_code_graph_error)?;
        let idempotency = tracedecay_code_index::graph_projection::code_graph_idempotency_key(
            &self.generation_id,
            &revision,
        )
        .map_err(map_code_graph_error)?;
        Ok((
            self.project_id.clone(),
            self.repository_id.clone(),
            self.worktree_id.clone(),
            self.generation_id.clone(),
            GraphGenerationDependency::new(projection, generation, idempotency),
        ))
    }

    pub(crate) fn semantic_vector_staging_binding(
        &self,
    ) -> (&StoreShardIdV1, &tracedecay_store::StoreRuntimeBindingV1) {
        (&self.code_shard, self.authority.binding())
    }

    #[hotpath::measure(label = "daemon.session_registry.publish_snapshot")]
    pub(crate) fn publish_verified_snapshot(
        &self,
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        if generation.manifest().generation_id != self.generation_id {
            return Err(GraphDbError::conflict(
                "code_graph.publish_verified_snapshot_with_stage_boundary",
            ));
        }
        self.sweep_aborted_read_bundle_temporaries()?;
        // Everything up to `prepared` below is a pure function of the
        // immutable published generation and this runtime's identity — no
        // publication storage is read or written — so racing publishers
        // compute it concurrently instead of serializing manifest projection
        // behind the per-shard publication gate. The manifest build memoizes
        // on the generation handle (first complete build wins), so the seat
        // pass and the background reconcile still share one projection. The
        // deadline window consequently also spans the gate wait; under the
        // background budget, cancellation stays the governing mechanism.
        let projection_deadline = sealed_projection_deadline();
        let deadline_at = Instant::now() + projection_deadline;
        let graph_generation = tracedecay_code_index::graph_projection::code_graph_generation_id(
            &self.generation_id,
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )?,
        )
        .map_err(map_code_graph_error)?;
        let cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "graph-publish:{}",
                graph_generation.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 1,
        };
        let deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "graph-publish-deadline:{}",
                graph_generation.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let probe = GraphPublicationProbeV1 {
            request_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &request_cancelled,
            ))),
            lifecycle_cancellation: graph_lifecycle_cancellation(&self.lifecycle_cancelled, None),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
            deadline_warned: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: tracedecay_application::clock::now_micros(),
            deadline: deadline_identity,
            cancellation: cancellation_identity,
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
            self.authority.namespace().clone(),
        )
        .map_err(map_code_graph_error)?;
        let manifest =
            tracedecay_code_index::graph_projection::build_published_code_graph_manifest_checked(
                projection.clone(),
                generation,
                &GraphProjectorRevision::try_from(
                    tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION
                        .to_owned(),
                )?,
                &|| match probe.interruption() {
                    Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                    Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                        Err(GraphDbError::DeadlineExceeded)
                    }
                    None => Ok(()),
                },
            )
            .map_err(map_code_graph_error)?;
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.authority.binding().shard_id.clone(),
            namespace: tracedecay_store::GraphNamespaceV1::new(self.authority.namespace().as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let source = SealedCodeGenerationReplay {
            repository: self.repository_id.clone(),
            generation: self.generation_id.clone(),
            sealed_state_digest: self.sealed_state_digest.clone(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )?,
        };
        let idempotency_key = tracedecay_code_index::graph_projection::code_graph_idempotency_key(
            &self.generation_id,
            &source.projector_revision,
        )
        .map_err(map_code_graph_error)?;
        let publication_key = GraphPublicationKeyV1::new(
            relational_projection.clone(),
            GraphGenerationIdV1::new(manifest.generation.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        let prepared = PreparedSealedPublicationV1 {
            projection_deadline,
            deadline_at,
            manifest,
            relational_projection,
            source,
            idempotency_key,
            publication_key,
            request_cancelled,
        };
        self.publish_prepared_sealed_generation(&prepared, &probe, &context)
    }

    /// Discard one interrupted publication whose completion just refused with
    /// a deterministic conflict verdict: the journaled pending replay row and
    /// the partial store contents its dead publisher left behind. Every
    /// refusal from the compare-and-swap-shaped discard means the journal
    /// moved since the diagnosis — the caller re-reads and proceeds, so a
    /// completed or re-journaled publication is never swept (issue #765).
    #[hotpath::measure(label = "daemon.session_registry.publish_snapshot.discard_interrupted")]
    fn discard_interrupted_publication_row(
        &self,
        storage: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        registration: GraphDbRegistration,
        pending: &GraphPublicationReplayRecordV1,
        conflict: &GraphDbError,
    ) -> std::result::Result<(), GraphDbError> {
        let outcome = self.graph_registry.discard_interrupted_publication(
            registration,
            storage,
            context,
            pending,
        )?;
        match outcome {
            GraphPendingReplayDiscardOutcomeV1::Discarded(discarded) => {
                tracing::warn!(
                    event = "code_graph_interrupted_publication_discarded",
                    generation = %discarded.publication.key.generation,
                    sequence = discarded.sequence.get(),
                    error = %conflict,
                    "discarded an interrupted graph publication whose completion \
                     conflicts deterministically; the journal position is open for \
                     a fresh publication"
                );
            }
            GraphPendingReplayDiscardOutcomeV1::Missing
            | GraphPendingReplayDiscardOutcomeV1::CurrentVerifiedHead { .. }
            | GraphPendingReplayDiscardOutcomeV1::Superseded { .. }
            | GraphPendingReplayDiscardOutcomeV1::SequenceMismatch { .. } => {
                tracing::warn!(
                    event = "code_graph_interrupted_publication_discard_refused",
                    generation = %pending.publication.key.generation,
                    sequence = pending.sequence.get(),
                    error = %conflict,
                    "the interrupted graph publication moved before its discard; \
                     continuing against the refreshed journal"
                );
            }
        }
        Ok(())
    }

    /// Acquires the per-shard serving gate for one short storage-ordered
    /// slice. The wait and hold spans at every slice are what profiles key on
    /// to attribute gate contention; with the corpus-sized prepare running
    /// gateless, both must stay milliseconds-scale.
    fn hold_publication_gate(&self) -> std::sync::MutexGuard<'_, ()> {
        hotpath::measure_block!(
            "daemon.session_registry.publish_snapshot.gate_wait",
            self.publication_locks
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        )
    }

    /// The classification gate slice: observe the typed interruption first
    /// (a publisher may have blocked on the gate), bind the shared manifest
    /// provider, then read the journaled replay and verified head to decide
    /// the publication arm. No journal write and no corpus-sized work happens
    /// here.
    #[hotpath::measure(label = "daemon.session_registry.publish_snapshot.classify")]
    fn classify_sealed_publication(
        &self,
        prepared: &PreparedSealedPublicationV1,
        probe: &GraphPublicationProbeV1,
        storage: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> std::result::Result<SealedPublicationClassificationV1, GraphDbError> {
        match probe.interruption() {
            Some(RuntimeInterruptionV1::Cancelled) => return Err(GraphDbError::Cancelled),
            Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                return Err(GraphDbError::DeadlineExceeded);
            }
            None => {}
        }
        self.graph_manifest_provider.bind(
            self.authority.binding().shard_id.clone(),
            self.project_id.clone(),
            self.repository_id.clone(),
            self.generations_root.clone(),
            self.replay_root.clone(),
        )?;
        match storage
            .replay(&prepared.publication_key, context)
            .map_err(map_publication_error)?
        {
            GraphPublicationReplayLookupV1::Active(_) => {
                let head = storage
                    .verified_head(&prepared.relational_projection, context)
                    .map_err(map_publication_error)?;
                if head
                    .as_ref()
                    .is_some_and(|head| head.key == prepared.publication_key)
                {
                    Ok(SealedPublicationClassificationV1::RecoverPublished)
                } else {
                    Ok(SealedPublicationClassificationV1::ResumeJournaled)
                }
            }
            GraphPublicationReplayLookupV1::Retired(_) => {
                Ok(SealedPublicationClassificationV1::RetiredConflict)
            }
            GraphPublicationReplayLookupV1::Missing => {
                Ok(SealedPublicationClassificationV1::AppendAndPublish)
            }
        }
    }

    /// One sealed publication: journal classification, the resume/recovery
    /// arms, pending predecessor completion, and the final publish.
    ///
    /// The per-shard serving gate is held only across the storage-ordered
    /// slices — manifest-provider bind plus journal classification, each
    /// replay append, and the CAS-plus-install swap inside the publish
    /// closure. Native staging, the sealed-store build, the digest proofs,
    /// and the boot-from-sealed recovery arms all run with no gate held, so
    /// a corpus-sized build can no longer sit inside a gate hold. Same-key
    /// publishers dedupe on the flight table instead of serializing behind a
    /// build-length gate wait.
    #[hotpath::measure(label = "daemon.session_registry.publish_snapshot.execute")]
    fn publish_prepared_sealed_generation(
        &self,
        prepared: &PreparedSealedPublicationV1,
        probe: &GraphPublicationProbeV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        // A request cancelled or expired before publication starts must
        // answer its typed interruption before touching the publication
        // authority.
        match probe.interruption() {
            Some(RuntimeInterruptionV1::Cancelled) => return Err(GraphDbError::Cancelled),
            Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                return Err(GraphDbError::DeadlineExceeded);
            }
            None => {}
        }
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.authority.clone();
        let registration = || GraphDbRegistration {
            authority_lease: Arc::clone(&authority_lease),
            cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &prepared.request_cancelled,
            ))),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.lifecycle_cancelled,
            ))),
            deadline: prepared.deadline_at,
        };
        let verify_durable_source = || {
            let replay_pool_lock = lock_project_graph_replay_pool(
                &self.replay_root,
                &|| match probe.interruption() {
                    Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                    Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                        Err(GraphDbError::DeadlineExceeded)
                    }
                    None => Ok(()),
                },
            )?;
            hotpath::measure_block!(
                "daemon.session_registry.publish_snapshot.verify_source",
                super::code_graph_manifest::verify_sealed_generation_source_from_roots(
                    &self.generations_root,
                    &self.replay_root,
                    &self.sealed_state_digest,
                    &|| match probe.interruption() {
                        Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                        Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                            Err(GraphDbError::DeadlineExceeded)
                        }
                        None => Ok(()),
                    },
                )
            )?;
            Ok::<_, GraphDbError>(replay_pool_lock)
        };
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let publish = |storage: &mut dyn GraphPublicationStoreV1,
                       key: &GraphPublicationKeyV1,
                       manifest: Option<Arc<GraphGenerationManifest>>|
         -> std::result::Result<_, GraphDbError> {
            let deadline_at = Instant::now() + prepared.projection_deadline;
            let cancellation_identity = RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!(
                    "graph-publish-commit:{}",
                    key.generation.as_str()
                ))
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                generation: 2,
            };
            let deadline_identity = RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!(
                    "graph-publish-commit-deadline:{}",
                    key.generation.as_str()
                ))
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            };
            let request_cancellation: Arc<dyn GraphCancellation> = Arc::new(
                AtomicGraphCancellationV1::new(Arc::clone(&prepared.request_cancelled)),
            );
            let probe = GraphPublicationProbeV1 {
                request_cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancellation: graph_lifecycle_cancellation(
                    &self.lifecycle_cancelled,
                    None,
                ),
                deadline_at,
                cancellation: cancellation_identity.clone(),
                deadline: deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
                deadline_warned: AtomicBool::new(false),
            };
            let control = RuntimeRequestControlV1 {
                requested_at: tracedecay_application::clock::now_micros(),
                deadline: deadline_identity,
                cancellation: cancellation_identity,
            };
            let context = GraphPublicationOperationContextV1::new(&control, &probe)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let publish_registration = || GraphDbRegistration {
                authority_lease: Arc::clone(&authority_lease),
                cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                    &self.lifecycle_cancelled,
                ))),
                deadline: deadline_at,
            };
            // Same-key dedupe: a twin publisher (the seat pass against the
            // background reconcile) parks here while the winner runs the
            // corpus-sized prepare, then resumes through prepare's idempotent
            // historical arm on the winner's instance-cached proof instead of
            // duplicating staging and the sealed-store build.
            let interruption = || match probe.interruption() {
                Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                    Err(GraphDbError::DeadlineExceeded)
                }
                None => Ok(()),
            };
            // One corpus-sized build at a time across the shard: the decoded
            // generation, the projection manifest, the staging pages, and
            // the sealed-store copy of concurrent scope publications
            // otherwise overlap into an unbounded replay working set.
            let _build = self.publication_locks.claim_build(&interruption)?;
            let _flight = self.publication_locks.flight.claim(key, &interruption)?;
            // The already-built projection manifest rides along so first
            // publication does not re-read and re-project the sealed artifact
            // through the replay manifest provider; a pending predecessor
            // journaled by an interrupted publisher carries no in-hand
            // manifest, so publication reconstructs it from the journaled
            // canonical replay source.
            //
            // Prepare — native staging, the sealed-store build, and the
            // durable digest proof — runs with no gate held; only the
            // CAS-plus-install swap below takes the serving gate.
            let preparation = self.graph_registry.prepare_verified_publication(
                publish_registration(),
                storage,
                &context,
                key,
                manifest,
            )?;
            let proven = match preparation {
                GraphPublicationPreparationV1::Settled(commit) => return Ok(*commit),
                GraphPublicationPreparationV1::Proven(proven) => proven,
            };
            let completion = publish_registration();
            let _gate = self.hold_publication_gate();
            hotpath::measure_block!(
                "daemon.session_registry.publish_snapshot.gate_hold",
                self.graph_registry
                    .complete_verified_publication(completion, storage, &context, *proven,)
            )
        };
        // Classification slice: the manifest-provider bind (a shared-map
        // write the gate orders before the publish/recover reads that resolve
        // sealed sources through it) plus the journal lookup that decides the
        // arm. Everything the decision leads to — recovery, staging, the
        // sealed-store build — runs after the gate is released.
        let classification = {
            let _gate = self.hold_publication_gate();
            hotpath::measure_block!(
                "daemon.session_registry.publish_snapshot.gate_hold",
                self.classify_sealed_publication(prepared, probe, &mut storage, context)
            )
        }?;
        match classification {
            SealedPublicationClassificationV1::RecoverPublished => {
                // The idempotent recovery arm: this publication already owns
                // the verified head (a flight loser after the winner
                // published, or a re-activation before replay retirement).
                // Runs gateless — it reads durable state and installs an
                // idempotent lease, so a boot-from-sealed recovery no longer
                // sits inside a gate hold. Prefer the immutable sealed
                // artifact so a cold daemon does not mount and reopen the
                // corpus-sized shared staging database merely to recover an
                // already-active generation. Dependency-bearing or absent
                // sealed artifacts explicitly fall back to the ordinary
                // staging recovery path; every integrity or control failure
                // remains terminal.
                match self.graph_registry.recover_verified_sealed_snapshot(
                    registration(),
                    &mut storage,
                    context,
                    &prepared.relational_projection,
                ) {
                    Ok(snapshot) => return Ok(snapshot),
                    Err(GraphDbError::Unavailable { .. }) => {}
                    Err(error) => return Err(error),
                }
                return self.graph_registry.recover_verified_snapshot(
                    registration(),
                    &mut storage,
                    context,
                    &prepared.relational_projection,
                );
            }
            SealedPublicationClassificationV1::RetiredConflict => {
                return observe_code_graph_publication(
                    CodeGraphPublicationConflictStageV1::RetiredReplay,
                    Err(GraphDbError::conflict(
                        "code_graph.publish_prepared_sealed_generation",
                    )),
                );
            }
            SealedPublicationClassificationV1::ResumeJournaled => {
                let _replay_pool_lock = verify_durable_source()?;
                // Seal-time bundle: stage from the in-hand rows before the
                // publish consumes them, commit only after it succeeds.
                let bundle_identity = prepared.manifest.identity();
                let staged_bundle =
                    self.stage_sealed_read_bundle(&prepared.manifest, &prepared.request_cancelled);
                match observe_code_graph_publication(
                    CodeGraphPublicationConflictStageV1::ActiveReplayPublish,
                    publish(
                        &mut storage,
                        &prepared.publication_key,
                        Some(Arc::clone(&prepared.manifest)),
                    ),
                ) {
                    Ok(publication) => {
                        self.commit_sealed_read_bundle(staged_bundle, &bundle_identity);
                        return Ok(publication.snapshot);
                    }
                    // Resuming this publication's own journaled replay
                    // refused deterministically: the interrupted publisher
                    // left journal or store state that this exact resume can
                    // never complete (issue #765). Discard the poisoned row
                    // and its partial contents, then republish fresh through
                    // the append path below — that is what restores service.
                    Err(conflict @ GraphDbError::Conflict { .. }) => {
                        drop(staged_bundle);
                        let pending = match storage
                            .replay(&prepared.publication_key, context)
                            .map_err(map_publication_error)?
                        {
                            GraphPublicationReplayLookupV1::Active(pending) => pending,
                            // The row moved while the resume ran; the append
                            // path below re-reads and answers truthfully.
                            GraphPublicationReplayLookupV1::Retired(_)
                            | GraphPublicationReplayLookupV1::Missing => {
                                return Err(conflict);
                            }
                        };
                        self.discard_interrupted_publication_row(
                            &mut storage,
                            context,
                            registration(),
                            &pending,
                            &conflict,
                        )?;
                    }
                    Err(error) => return Err(error),
                }
            }
            SealedPublicationClassificationV1::AppendAndPublish => {}
        }
        let replay_pool_lock = verify_durable_source()?;
        let input = canonical_sha256(&(
            "tracedecay.code-graph-publication-input.v1",
            &prepared.source,
            &prepared.manifest.generation,
            &prepared.manifest.source_generation,
            &prepared.manifest.watermark,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let build_replay = |prior: Option<GraphVerifiedHeadV1>| {
            prepared.manifest.relational_sealed_replay(
                self.authority.binding().shard_id.clone(),
                prepared.idempotency_key.clone(),
                GraphPublicationInputDigestV1::new(input.as_str())
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                prior,
                prepared.source.clone(),
                &|| match probe.interruption() {
                    Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                    Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                        Err(GraphDbError::DeadlineExceeded)
                    }
                    None => Ok(()),
                },
            )
        };
        let prior = storage
            .verified_head(&prepared.relational_projection, context)
            .map_err(map_publication_error)?;
        let mut replay = build_replay(prior)?;
        // The relational journal is an ordered log: a replay journaled by an
        // interrupted publisher blocks every later sequence until it lands,
        // and a dead publisher can never land its own. Answering Conflict
        // here wedged the projection permanently — every reconcile sealed a
        // newer generation, appended a newer sequence, and conflicted on the
        // orphan forever while sealed artifacts piled up on disk. So this
        // publisher completes pending predecessors first (their sealed
        // sources are retained by collection precisely because they are
        // pending), then appends its own replay against the advanced head.
        // Bounded: every pass either appends or advances the verified head by
        // exactly one completed predecessor, and a repeated blocker surfaces
        // as that predecessor's own typed error.
        let mut completed_predecessors = 0usize;
        loop {
            // Append slice: one journal write per gate hold, with the typed
            // interruption observed after the wait so a request cancelled
            // while blocked never touches the journal.
            let outcome = {
                let _gate = self.hold_publication_gate();
                hotpath::measure_block!(
                    "daemon.session_registry.publish_snapshot.gate_hold",
                    match probe.interruption() {
                        Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                        Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                            Err(GraphDbError::DeadlineExceeded)
                        }
                        None => storage
                            .append_replay(&replay, context)
                            .map_err(map_publication_error),
                    }
                )
            }?;
            match outcome {
                GraphReplayAppendOutcomeV1::Appended(_)
                | GraphReplayAppendOutcomeV1::ExactReplay(_)
                | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => break,
                GraphReplayAppendOutcomeV1::PendingReplayConflict { pending } => {
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return observe_code_graph_publication(
                            CodeGraphPublicationConflictStageV1::PendingCompletionLimit,
                            Err(GraphDbError::conflict(
                                "code_graph.publish_prepared_sealed_generation",
                            )),
                        );
                    }
                    completed_predecessors += 1;
                    match observe_code_graph_publication(
                        CodeGraphPublicationConflictStageV1::PendingPredecessorPublish,
                        publish(&mut storage, &pending.publication.key, None),
                    ) {
                        Ok(_) => {}
                        // The orphan predecessor refused deterministically:
                        // its interrupted publisher left journal or store
                        // state that completion can never satisfy (issue
                        // #765). Discarding it reopens the journal position
                        // this append is blocked on; answering Conflict here
                        // wedged the projection forever.
                        Err(conflict @ GraphDbError::Conflict { .. }) => {
                            self.discard_interrupted_publication_row(
                                &mut storage,
                                context,
                                registration(),
                                &pending,
                                &conflict,
                            )?;
                        }
                        Err(error) => return Err(error),
                    }
                    let prior = storage
                        .verified_head(&prepared.relational_projection, context)
                        .map_err(map_publication_error)?;
                    replay = build_replay(prior)?;
                }
                GraphReplayAppendOutcomeV1::VerifiedHeadConflict { actual } => {
                    // A concurrent publisher advanced the head between our
                    // read and this append; the refreshed head is the only
                    // thing that was wrong with the replay.
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return observe_code_graph_publication(
                            CodeGraphPublicationConflictStageV1::VerifiedHeadRefreshLimit,
                            Err(GraphDbError::conflict(
                                "code_graph.publish_prepared_sealed_generation",
                            )),
                        );
                    }
                    completed_predecessors += 1;
                    replay = build_replay(actual)?;
                }
                GraphReplayAppendOutcomeV1::Conflict { .. }
                | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. } => {
                    return observe_code_graph_publication(
                        CodeGraphPublicationConflictStageV1::ReplayAppend,
                        Err(GraphDbError::conflict(
                            "code_graph.publish_prepared_sealed_generation",
                        )),
                    );
                }
            }
        }
        drop(replay_pool_lock);
        // Seal-time bundle: stage from the in-hand rows before the publish
        // consumes them, commit only after it succeeds.
        let bundle_identity = prepared.manifest.identity();
        let staged_bundle =
            self.stage_sealed_read_bundle(&prepared.manifest, &prepared.request_cancelled);
        let publication = observe_code_graph_publication(
            CodeGraphPublicationConflictStageV1::FinalPublish,
            publish(
                &mut storage,
                &replay.key,
                Some(Arc::clone(&prepared.manifest)),
            ),
        )?;
        self.commit_sealed_read_bundle(staged_bundle, &bundle_identity);
        Ok(publication.snapshot)
    }

    /// Loads this generation's interactive-catalog bundle artifact, verified
    /// against the generation identity through the sealed read bundle
    /// envelope. `Absent` and `Stale` are typed states the caller must log
    /// before falling back to open-time re-derivation.
    pub(crate) fn load_sealed_read_bundle_catalog(
        &self,
        request_cancelled: &Arc<AtomicBool>,
    ) -> std::result::Result<tracedecay_graph_db::SealedReadBundleArtifactStateV1, GraphDbError>
    {
        let identity = tracedecay_code_index::graph_projection::code_graph_manifest_identity(
            self.authority.namespace().clone(),
            &self.generation_id,
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )?,
        )
        .map_err(map_code_graph_error)?;
        let cancellation = CombinedAtomicGraphCancellationV1 {
            local: Arc::clone(request_cancelled),
            registry: Some(Arc::clone(&self.lifecycle_cancelled)),
        };
        tracedecay_graph_db::load_sealed_read_bundle_artifact(
            &self.generations_root,
            &self.sealed_state_digest,
            &identity,
            tracedecay_code_index::graph_projection::INTERACTIVE_CATALOG_ARTIFACT_NAME,
            &|| {
                if cancellation.is_cancelled() {
                    Err(GraphDbError::Cancelled)
                } else {
                    Ok(())
                }
            },
        )
    }

    /// Stages the sealed read bundle's artifacts from the manifest rows the
    /// seal already holds, before publication consumes them. Streaming to a
    /// staged temporary file keeps the derivation out of the publish RAM
    /// peak; nothing becomes visible until [`Self::commit_sealed_read_bundle`]
    /// runs after the publication succeeds. A staging failure is logged and
    /// degrades to the open-time re-derivation fallback — it never fails the
    /// seal itself.
    fn stage_sealed_read_bundle(
        &self,
        manifest: &GraphGenerationManifest,
        request_cancelled: &Arc<AtomicBool>,
    ) -> Option<tracedecay_graph_db::SealedReadBundleWriterV1> {
        let cancellation = CombinedAtomicGraphCancellationV1 {
            local: Arc::clone(request_cancelled),
            registry: Some(Arc::clone(&self.lifecycle_cancelled)),
        };
        let stage = || {
            self.sweep_aborted_read_bundle_temporaries()?;
            let mut writer = tracedecay_graph_db::SealedReadBundleWriterV1::create(
                &self.generations_root,
                &self.sealed_state_digest,
            )?;
            writer.stage_artifact(
                tracedecay_code_index::graph_projection::INTERACTIVE_CATALOG_ARTIFACT_NAME,
                &mut |out| {
                    tracedecay_code_index::graph_projection::write_interactive_catalog_artifact(
                        manifest,
                        out,
                        &cancellation,
                    )
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))
                },
            )?;
            Ok::<_, GraphDbError>(writer)
        };
        match stage() {
            Ok(writer) => Some(writer),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    generation = %self.generation_id,
                    "sealed read bundle staging failed; open will re-derive the interactive catalog"
                );
                None
            }
        }
    }

    /// Commits a staged sealed read bundle after its generation's publication
    /// succeeded. Commit failure degrades to the logged open-time fallback.
    fn commit_sealed_read_bundle(
        &self,
        writer: Option<tracedecay_graph_db::SealedReadBundleWriterV1>,
        identity: &tracedecay_graph_db::GraphGenerationManifestIdentity,
    ) {
        let Some(writer) = writer else {
            return;
        };
        match writer.commit(identity, &|| {
            if self.lifecycle_cancelled.load(Ordering::Acquire) {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        }) {
            Ok(manifest) => {
                tracing::info!(
                    generation = %self.generation_id,
                    artifacts = manifest.artifacts.len(),
                    "sealed read bundle written at seal"
                );
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    generation = %self.generation_id,
                    "sealed read bundle commit failed; open will re-derive the interactive catalog"
                );
            }
        }
    }

    pub(crate) fn recover_semantic_vector_projection(
        &self,
        projection: &GraphProjectionIdentity,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        let relational_projection = self.relational_projection(projection)?;
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_graph_operation(
            &mut storage,
            cancellation,
            deadline,
            "recover",
            |registration, storage, context| {
                if storage
                    .verified_head(&relational_projection, context)
                    .map_err(map_publication_error)?
                    .is_none()
                {
                    return Ok(None);
                }
                self.graph_registry
                    .recover_verified_snapshot(
                        registration,
                        storage,
                        context,
                        &relational_projection,
                    )
                    .map(Some)
            },
        )
    }

    pub(crate) fn recover_semantic_vector_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_graph_operation(
            &mut storage,
            cancellation,
            deadline,
            "recover-generation",
            |registration, storage, context| {
                self.graph_registry.verified_generation_snapshot(
                    registration,
                    storage,
                    context,
                    publication,
                )
            },
        )
    }

    fn relational_projection(
        &self,
        projection: &GraphProjectionIdentity,
    ) -> std::result::Result<GraphProjectionIdentityV1, GraphDbError> {
        Ok(GraphProjectionIdentityV1 {
            shard_id: self.authority.binding().shard_id.clone(),
            namespace: tracedecay_store::GraphNamespaceV1::new(projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        })
    }

    fn semantic_graph_operation<T>(
        &self,
        storage: &mut dyn GraphPublicationStoreV1,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
        operation: &str,
        execute: impl FnOnce(
            GraphDbRegistration,
            &mut dyn GraphPublicationStoreV1,
            &GraphPublicationOperationContextV1<'_>,
        ) -> std::result::Result<T, GraphDbError>,
    ) -> std::result::Result<T, GraphDbError> {
        self.semantic_operation(
            cancellation,
            deadline,
            operation,
            |registration, context| execute(registration, storage, context),
        )
    }

    fn semantic_operation<T>(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
        operation: &str,
        execute: impl FnOnce(
            GraphDbRegistration,
            &GraphPublicationOperationContextV1<'_>,
        ) -> std::result::Result<T, GraphDbError>,
    ) -> std::result::Result<T, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(GraphDbError::DeadlineExceeded);
        }
        let identity = canonical_sha256(&(
            "tracedecay.semantic-vector.graph-operation.v1",
            &self.project_id,
            &self.repository_id,
            &self.worktree_id,
            &self.generation_id,
            operation,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "semantic-vector:{}",
                identity.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 1,
        };
        let deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "semantic-vector-deadline:{}",
                identity.as_str()
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let probe = GraphPublicationProbeV1 {
            request_cancellation: Arc::clone(&cancellation),
            lifecycle_cancellation: graph_lifecycle_cancellation(&self.lifecycle_cancelled, None),
            deadline_at: deadline,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
            deadline_warned: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: tracedecay_application::clock::now_micros(),
            deadline: deadline_identity,
            cancellation: cancellation_identity,
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.authority.clone();
        execute(
            GraphDbRegistration {
                authority_lease,
                cancellation,
                lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                    &self.lifecycle_cancelled,
                ))),
                deadline,
            },
            &context,
        )
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    /// Self-heals the shared project shard's graph map-owner attachment when
    /// a prior owner was retired out from under this lease-only consumer.
    ///
    /// The code graph never attaches its own map owner: per the contract on
    /// [`tracedecay_graph_db::GraphDbRegistry::resolve_owner_attachment`],
    /// that is deliberately the only entry-creation path, and an ordinary
    /// lease (what [`Self::retain_code_graph_runtime`] takes below) can only
    /// resolve an entry that already exists. In production the project's
    /// memory/journey graph mount is the one that attaches this same
    /// physical shard (see `graph_attachment::open_session_relation_owner_for_task`
    /// in `mounts.rs`) and normally keeps it attached for as long as the
    /// project runtime stays mounted. But that owner can be retired
    /// independently of code-index activity — for example by the
    /// capacity-driven project-server reclaim in `project_composition.rs`,
    /// which calls `retire_project_memory_graph` to admit another project.
    /// Once that happens, every later code-graph reconcile fails permanently
    /// with "graph runtime is not registered", and retrying the same
    /// lease-only path can never recover it: nothing else ever re-attaches.
    ///
    /// This reuses the exact attach the memory/journey graph mount uses, but
    /// only when the shard is actually missing, and it does not retain the
    /// resulting attachment — the sole purpose is to leave the registry
    /// entry `Ready` so the lease immediately below succeeds. Losing a race
    /// to a concurrent attacher (or any other failure here) is swallowed:
    /// the ordinary lease path still runs and surfaces its own, more precise
    /// error if the shard is genuinely unavailable.
    #[hotpath::measure(
        label = "daemon.session_registry.ensure_graph_shard_attached",
        future = true
    )]
    async fn ensure_code_graph_shard_attached(&self, project_shard: &StoreShardIdV1) {
        match self.graph_registry.shard_is_registered(project_shard) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    event = "code_graph_shard_registration_probe_failed",
                    shard = ?project_shard,
                    error = %error,
                    "could not check whether the shared code graph shard is registered"
                );
                return;
            }
        }
        match graph_attachment::open_session_relation_owner(
            &self.registry,
            &self.graph_registry,
            &self.graph_lifecycle_cancelled,
            self.incarnation,
            project_shard.clone(),
        )
        .await
        {
            Ok((attachment, target)) => {
                tracing::info!(
                    event = "code_graph_shard_reattached",
                    shard = ?project_shard,
                    "re-attached the shared code graph shard after its owner was retired"
                );
                drop(attachment);
                drop(target);
            }
            Err(error) => {
                tracing::warn!(
                    event = "code_graph_shard_reattach_failed",
                    shard = ?project_shard,
                    error = %error,
                    "could not re-attach the shared code graph shard; the exact-lease path will report its own error"
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(
        label = "daemon.session_registry.retain_code_graph_runtime",
        future = true
    )]
    pub(crate) async fn retain_code_graph_runtime(
        &self,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
        generation_id: CodeGenerationId,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        replay_binding: CodeGraphReplayBindingV1,
        // The generation the code index just decoded to serve queries, when the
        // caller has one. Offering it to the manifest provider is what makes
        // cold activation parse the sealed payload once instead of twice
        // (plan 40, stage 1). `None` simply leaves the provider reading the
        // canonical seal exactly as before.
        decoded_generation: Option<
            Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
        >,
    ) -> Result<RetainedCodeGraphRuntimeV1> {
        let project_shard = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        self.ensure_code_graph_shard_attached(&project_shard).await;
        let code_scope = match reference {
            Some(ref_id) => CodeShardScopeV1::Branch {
                worktree_id: worktree_id.clone(),
                ref_id,
            },
            None => CodeShardScopeV1::Worktree {
                worktree_id: worktree_id.clone(),
            },
        };
        let code_shard = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
            repository_id.clone(),
            code_scope,
        );
        let authority = self
            .registry
            .retain_code_graph_store(
                StoreRuntimeKey::new(project_shard.clone(), self.incarnation),
                code_shard.clone(),
                generation_id.clone(),
            )
            .await
            .map_err(|failure| {
                session_registry_error("retain exact code graph authority", format!("{failure:?}"))
            })?;
        // The pre-retain heal above keys the freshly reconstructed project
        // shard, but every later publish/recover lease keys the registry by
        // this lease's binding (`registered_database` uses
        // `registration.binding().shard_id`). A preserved profile or a
        // publication row from a prior run can carry a different shard
        // identity than the one reconstructed here, so heal the exact key
        // the lookup will use and name both identities when they diverge.
        let bound_shard = authority.binding().shard_id.clone();
        if bound_shard != project_shard {
            tracing::warn!(
                event = "code_graph_lease_binding_shard_diverged",
                probed = ?project_shard,
                bound = ?bound_shard,
                "code graph lease binding names a different shard than the reconstructed project shard"
            );
            self.ensure_code_graph_shard_attached(&bound_shard).await;
        }
        // Offer the already-decoded seal before any publication or recovery can
        // reach the manifest provider. The offer is keyed by the exact shard the
        // provider resolves bindings under, and is only ever served on an exact
        // generation-and-digest match, so a stale offer cannot displace the
        // canonical seal.
        if let Some(decoded_generation) = decoded_generation {
            self.graph_manifest_provider
                .offer_decoded_code_generation(
                    authority.binding().shard_id.clone(),
                    generation_id.clone(),
                    replay_binding.sealed_state_digest.clone(),
                    decoded_generation,
                )
                .map_err(|error| {
                    session_registry_error("offer decoded code generation", error.to_string())
                })?;
        }
        let replay_root = project_database
            .database_path()
            .with_extension("graph-replay");
        let publication_locks = {
            let mut gates = self
                .code_graph_publication_gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(gates.entry(code_shard.clone()).or_default())
        };
        Ok(RetainedCodeGraphRuntimeV1 {
            graph_registry: self.graph_registry.clone(),
            graph_manifest_provider: Arc::clone(&self.graph_manifest_provider),
            authority,
            project_database,
            project_id,
            repository_id,
            worktree_id,
            code_shard,
            generation_id,
            generations_root: replay_binding.generations_root,
            replay_root,
            sealed_state_digest: replay_binding.sealed_state_digest,
            lifecycle_cancelled: Arc::clone(&self.graph_lifecycle_cancelled),
            publication_locks,
        })
    }

    #[hotpath::measure(
        label = "daemon.session_registry.reconcile_graph_replays",
        future = true
    )]
    pub(crate) async fn reconcile_deleted_code_generation_graph_replays(
        &self,
        project_id: ProjectId,
        project_database: &tracedecay_runtime_core::db::Database,
        generation: &CodeGenerationId,
        generation_file: &str,
        cancellation: &tracedecay_session_memory::context::CancellationToken,
    ) -> std::result::Result<bool, GraphDbError> {
        let sealed_digest = sealed_digest_from_generation_file(generation_file)?;
        let replay_root = project_database
            .database_path()
            .with_extension("graph-replay");
        let project_shard = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        // Same self-heal as `retain_code_graph_runtime`: this reconcile leases
        // the shared project shard through the lease-only path, so once the
        // shard's graph map-owner attachment has been retired (capacity-driven
        // project reclaim, or a crash-era restart that never re-attached it),
        // every `graph_registry` resolve below fails permanently with
        // "graph runtime is not registered" and nothing else ever re-attaches.
        // That exact failure surfaced on every retention tick as
        // `graph_replay_release_failed`, which blocked the code-generation
        // pass and let superseded sealed generations accumulate without bound.
        self.ensure_code_graph_shard_attached(&project_shard).await;
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(
                project_shard.clone(),
                self.incarnation,
            ))
            .await
            .map_err(|error| GraphDbError::unavailable(format!("{error:?}")))?;
        // The graph leases below key the registry by this lease's binding, not
        // the reconstructed project shard; heal the exact key the lookups use
        // when the two diverge (preserved profiles, prior-run publication rows).
        let bound_shard = authority.binding().shard_id.clone();
        if bound_shard != project_shard {
            tracing::warn!(
                event = "code_graph_lease_binding_shard_diverged",
                probed = ?project_shard,
                bound = ?bound_shard,
                "graph replay reconcile lease binding names a different shard than the reconstructed project shard"
            );
            self.ensure_code_graph_shard_attached(&bound_shard).await;
        }
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority;
        let mut storage = project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        // Everything below is synchronous journal access: a blocking pool
        // file lock, then retirement and cleanup sweeps whose store calls
        // block on writer-actor and reader-worker round trips (with
        // millisecond busy-retry sleeps inside `begin`/`begin_read`). Run
        // the whole sweep on the blocking pool so a retention tick cannot
        // stall the daemon's async runtime workers for its duration.
        let graph_registry = self.graph_registry.clone();
        let graph_lifecycle_cancelled = Arc::clone(&self.graph_lifecycle_cancelled);
        let cancellation = cancellation.clone();
        let generation = generation.clone();
        tokio::task::spawn_blocking(move || {
            let pool_deadline = Instant::now() + GRAPH_OPERATION_DEADLINE;
            let pool_check = || {
                if cancellation.is_cancelled() {
                    Err(GraphDbError::Cancelled)
                } else if Instant::now() >= pool_deadline {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            };
            let replay_pool_lock = lock_project_graph_replay_pool(&replay_root, &pool_check)?;

            let staged_unlink;
            loop {
                if cancellation.is_cancelled() {
                    return Err(GraphDbError::Cancelled);
                }
                let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
                let cancellation_identity = RuntimeCancellationIdentityV1 {
                    cancellation_id: RuntimeCancellationIdV1::new(format!(
                        "graph-retire:{}",
                        generation.as_str()
                    ))
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                    generation: 1,
                };
                let deadline_identity = RuntimeDeadlineV1 {
                    deadline_id: RuntimeDeadlineIdV1::new(format!(
                        "graph-retire-deadline:{}",
                        generation.as_str()
                    ))
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                };
                let request_cancellation: Arc<dyn GraphCancellation> =
                    Arc::new(MaintenanceGraphCancellationV1(cancellation.clone()));
                let probe = GraphPublicationProbeV1 {
                    request_cancellation: Arc::clone(&request_cancellation),
                    lifecycle_cancellation: graph_lifecycle_cancellation(
                        &graph_lifecycle_cancelled,
                        None,
                    ),
                    deadline_at,
                    cancellation: cancellation_identity.clone(),
                    deadline: deadline_identity.clone(),
                    commit_started: AtomicBool::new(false),
                    deadline_warned: AtomicBool::new(false),
                };
                let control = RuntimeRequestControlV1 {
                    requested_at: tracedecay_application::clock::now_micros(),
                    deadline: deadline_identity,
                    cancellation: cancellation_identity,
                };
                let context = GraphPublicationOperationContextV1::new(&control, &probe)
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                let registration = GraphDbRegistration {
                    authority_lease: Arc::clone(&authority_lease),
                    cancellation: Arc::clone(&request_cancellation),
                    lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                        &graph_lifecycle_cancelled,
                    ))),
                    deadline: deadline_at,
                };
                match graph_registry.retire_one_code_generation_replay(
                    registration,
                    &mut storage,
                    &context,
                    &generation,
                    &sealed_digest,
                )? {
                    GraphReplayCollectionOutcome::Retired(source) => {
                        let tracedecay_graph_db::GraphGenerationReplaySource::SealedCodeGeneration(
                            source,
                        ) = *source
                        else {
                            return Err(GraphDbError::Corrupt {
                                message:
                                    "code generation retirement selected an inline graph replay"
                                        .to_owned(),
                            });
                        };
                        if source.generation != generation
                            || source.sealed_state_digest != sealed_digest
                        {
                            return Err(GraphDbError::conflict(
                                "code_graph.reconcile_deleted_code_generation_graph_replays",
                            ));
                        }
                    }
                    GraphReplayCollectionOutcome::Retained => return Ok(false),
                    GraphReplayCollectionOutcome::Absent => {
                        staged_unlink =
                            stage_project_graph_replay_unlink(&replay_root, &sealed_digest)?;
                        break;
                    }
                }
            }
            drop(replay_pool_lock);
            if let Some(staged_unlink) = staged_unlink {
                finalize_project_graph_replay_unlink(
                    staged_unlink,
                    &replay_root,
                    &sealed_digest,
                    &pool_check,
                )?;
            }
            let mut cleanup_sequence = 0_u64;
            loop {
                cleanup_sequence = cleanup_sequence.checked_add(1).ok_or_else(|| {
                    GraphDbError::budget_exhausted(GraphBudgetKind::Capacity, u64::MAX)
                })?;
                let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
                let cancellation_identity = RuntimeCancellationIdentityV1 {
                    cancellation_id: RuntimeCancellationIdV1::new(format!(
                        "graph-cleanup:{}:{cleanup_sequence}",
                        generation.as_str()
                    ))
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                    generation: cleanup_sequence,
                };
                let deadline_identity = RuntimeDeadlineV1 {
                    deadline_id: RuntimeDeadlineIdV1::new(format!(
                        "graph-cleanup-deadline:{}:{cleanup_sequence}",
                        generation.as_str()
                    ))
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                };
                let request_cancellation: Arc<dyn GraphCancellation> =
                    Arc::new(MaintenanceGraphCancellationV1(cancellation.clone()));
                let probe = GraphPublicationProbeV1 {
                    request_cancellation: Arc::clone(&request_cancellation),
                    lifecycle_cancellation: graph_lifecycle_cancellation(
                        &graph_lifecycle_cancelled,
                        None,
                    ),
                    deadline_at,
                    cancellation: cancellation_identity.clone(),
                    deadline: deadline_identity.clone(),
                    commit_started: AtomicBool::new(false),
                    deadline_warned: AtomicBool::new(false),
                };
                let control = RuntimeRequestControlV1 {
                    requested_at: tracedecay_application::clock::now_micros(),
                    deadline: deadline_identity,
                    cancellation: cancellation_identity,
                };
                let context = GraphPublicationOperationContextV1::new(&control, &probe)
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                let registration = GraphDbRegistration {
                    authority_lease: Arc::clone(&authority_lease),
                    cancellation: Arc::clone(&request_cancellation),
                    lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                        &graph_lifecycle_cancelled,
                    ))),
                    deadline: deadline_at,
                };
                if !graph_registry.finalize_one_code_generation_replay_cleanup(
                    registration,
                    &mut storage,
                    &context,
                    &generation,
                    &sealed_digest,
                )? {
                    return Ok(true);
                }
            }
        })
        .await
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "graph replay reconcile blocking task failed: {error}"
            ))
        })?
    }
}

impl CodeGraphSeatLeaseV1 for RetainedCodeGraphRuntimeV1 {
    fn sweep_aborted_read_bundle_temporaries(
        &self,
    ) -> std::result::Result<(), tracedecay_graph_db::GraphDbError> {
        Self::sweep_aborted_read_bundle_temporaries(self)
    }

    fn authority(
        &self,
    ) -> Arc<tracedecay_runtime_core::store_runtime::registry::CanonicalCodeGraphStoreLeaseV1> {
        Self::authority(self)
    }

    fn publish_verified_snapshot(
        &self,
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<
        tracedecay_graph_db::VerifiedGraphSnapshot,
        tracedecay_graph_db::GraphDbError,
    > {
        Self::publish_verified_snapshot(self, generation, request_cancelled)
    }

    fn load_sealed_read_bundle_catalog(
        &self,
        request_cancelled: &Arc<AtomicBool>,
    ) -> std::result::Result<
        tracedecay_graph_db::SealedReadBundleArtifactStateV1,
        tracedecay_graph_db::GraphDbError,
    > {
        Self::load_sealed_read_bundle_catalog(self, request_cancelled)
    }

    fn semantic_vector_identity(
        &self,
    ) -> std::result::Result<
        (
            tracedecay_domain::ProjectId,
            RepositoryId,
            WorktreeId,
            CodeGenerationId,
            GraphGenerationDependency,
        ),
        tracedecay_graph_db::GraphDbError,
    > {
        Self::semantic_vector_identity(self)
    }

    fn semantic_vector_staging_binding(
        &self,
    ) -> (
        tracedecay_store::StoreShardIdV1,
        tracedecay_store::StoreRuntimeBindingV1,
    ) {
        let (scope, binding) = Self::semantic_vector_staging_binding(self);
        (scope.clone(), binding.clone())
    }

    fn into_semantic_vector_runtime(
        self: Box<Self>,
        scope: tracedecay_usecases::semantic_runtime::SemanticVectorGraphScopeV1,
    ) -> Arc<dyn tracedecay_usecases::semantic_runtime::VerifiedSemanticVectorGraphRuntimeV1> {
        let (source_scope, binding) = {
            let (scope, binding) =
                RetainedCodeGraphRuntimeV1::semantic_vector_staging_binding(self.as_ref());
            (scope.clone(), binding.clone())
        };
        Arc::new(DaemonVerifiedSemanticVectorGraphRuntimeV1::new(
            Arc::from(self),
            scope,
            source_scope,
            binding,
        ))
    }
}

impl CodeGraphSeatRuntimePortV1 for DaemonSessionRuntimeRegistryV1 {
    fn retain_code_graph_runtime(
        &self,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
        generation_id: CodeGenerationId,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        replay_binding: CodeGraphReplayBindingV1,
        decoded_generation: Option<
            Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
        >,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Box<dyn CodeGraphSeatLeaseV1 + Send>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let retained = DaemonSessionRuntimeRegistryV1::retain_code_graph_runtime(
                self,
                project_id,
                repository_id,
                worktree_id,
                reference,
                generation_id,
                project_database,
                replay_binding,
                decoded_generation,
            )
            .await?;
            Ok(Box::new(retained) as Box<dyn CodeGraphSeatLeaseV1 + Send>)
        })
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    /// Coerce this registry to the scheduler-facing seat port.
    ///
    /// `Arc::clone` keeps the concrete type; this is the unsized coercion
    /// `mount_worktree_with_graph_runtime` needs.
    pub(crate) fn code_graph_seat_port(self: &Arc<Self>) -> Arc<dyn CodeGraphSeatRuntimePortV1> {
        Arc::clone(self) as Arc<dyn CodeGraphSeatRuntimePortV1>
    }
}

fn map_publication_error(error: GraphPublicationStoreErrorV1) -> GraphDbError {
    match error {
        GraphPublicationStoreErrorV1::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::Cancelled) => {
            GraphDbError::Cancelled
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::DeadlineExceeded) => {
            GraphDbError::DeadlineExceeded
        }
        GraphPublicationStoreErrorV1::Infrastructure => {
            GraphDbError::unavailable("relational graph publication authority is unavailable")
        }
        GraphPublicationStoreErrorV1::Corrupt(message) => GraphDbError::Corrupt { message },
    }
}

fn map_code_graph_error(
    error: tracedecay_code_index::graph_projection::CodeGraphProjectionError,
) -> GraphDbError {
    use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
    match error {
        CodeGraphProjectionError::Cancelled => GraphDbError::Cancelled,
        CodeGraphProjectionError::BudgetExhausted { budget, limit } => {
            // Preserve budget identity; unrecognized names are
            // projection-local budgets reported under the read class.
            let kind = GraphBudgetKind::from_name(&budget).unwrap_or(GraphBudgetKind::Read);
            GraphDbError::budget_exhausted(kind, limit)
        }
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::Conflict { context } => GraphDbError::Conflict { context },
        CodeGraphProjectionError::ProjectionMismatch {
            namespace,
            projection,
            message,
        } => GraphDbError::ProjectionMismatch {
            namespace,
            projection,
            message,
        },
        CodeGraphProjectionError::RecoveredGenerationMismatch {
            namespace,
            projection,
            generation,
            message,
        } => GraphDbError::GenerationMismatch {
            namespace,
            projection,
            generation,
            message,
        },
        CodeGraphProjectionError::ResetRequired(message) => GraphDbError::ResetRequired { message },
        CodeGraphProjectionError::Corrupt(message) => GraphDbError::Corrupt { message },
        CodeGraphProjectionError::Unavailable(message) => GraphDbError::Unavailable { message },
        CodeGraphProjectionError::DurabilityUncertain(message) => {
            GraphDbError::DurabilityUncertain { message }
        }
        CodeGraphProjectionError::Closed => GraphDbError::Closed,
        CodeGraphProjectionError::Contract(message) => GraphDbError::invalid(message),
        CodeGraphProjectionError::GenerationMismatch => {
            GraphDbError::invalid("code graph generation does not match")
        }
    }
}

impl Drop for DaemonSessionRuntimeRegistryV1 {
    fn drop(&mut self) {
        self.graph_lifecycle_cancelled
            .store(true, Ordering::Release);
        self.cancel_memory_graph_reconciliation_tasks();
    }
}

#[cfg(test)]
mod sealed_projection_deadline_tests {
    use super::{GRAPH_BACKGROUND_OPERATION_BUDGET, sealed_projection_deadline};

    #[test]
    fn sealed_projection_has_no_wall_clock_bail_out() {
        // Background projection is bounded by cancellation, not wall clock,
        // regardless of artifact size. The live incident shape (a ~1.6 GB
        // sealed generation died at a 30-second wall, then at a size-scaled
        // wall) must never be budgeted below hours again.
        assert_eq!(
            sealed_projection_deadline(),
            GRAPH_BACKGROUND_OPERATION_BUDGET
        );
        assert!(GRAPH_BACKGROUND_OPERATION_BUDGET >= std::time::Duration::from_hours(24));
    }
}
