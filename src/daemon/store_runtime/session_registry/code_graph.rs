use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    CodeGenerationId, RefId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbOwnerAttachmentV1,
    GraphDbRegistration, GraphGenerationDependency, GraphGenerationManifest, GraphIdempotencyKey,
    GraphProjectionIdentity, GraphProjectorRevision, GraphReplayCollectionOutcome, GraphWriteBatch,
    SealedCodeGenerationReplay, VerifiedGenerationBatchCommit, VerifiedGraphCommit,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalCodeGraphStoreLeaseV1, CanonicalGraphStoreOwnerRetirementTargetV1, StoreRuntimeKey,
};
use tracedecay_store::{
    CodeShardScopeV1, FactReadControl, GraphGenerationIdV1, GraphProjectionIdV1,
    GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1,
    GraphPublicationKeyV1, GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreV1, GraphReplayAppendOutcomeV1,
    GraphVerifiedHeadV1, ProjectId, RetainedGraphStoreLeaseV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageKey, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageRecord,
    SemanticVectorStageResumeOutcome, SemanticVectorStagingStore, StoreShardIdV1,
};

use super::{DaemonSessionRuntimeRegistryV1, Result, SessionGraphOwnerV1, session_registry_error};

mod memory_runtime;
pub(super) use memory_runtime::{
    inline_graph_publication_input_digest, schedule_bound_memory_graph_reconciliation,
};
pub(super) mod graph_attachment;
#[cfg(test)]
mod sealed_publication_tests;
mod seals;
mod semantic_vector;
use seals::{
    finalize_project_graph_replay_unlink, install_project_graph_replay_seal_at,
    lock_project_graph_replay_pool, publish_staged_replay_seal, sealed_digest_from_generation_file,
    stage_project_graph_replay_seal, stage_project_graph_replay_unlink,
};

const GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
const GRAPH_OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// Sealed-generation projection replays the whole sealed artifact into the
/// native graph, so its ceiling scales with the sealed byte size instead of
/// reusing the ordinary 30-second graph-operation budget. The floor keeps the
/// small-generation behavior; the throughput divisor is a conservative
/// decode+apply rate; the ceiling matches the evaluation-runtime projection
/// bound so a pathological artifact still terminates.
const SEALED_PROJECTION_DEADLINE_FLOOR: Duration = GRAPH_OPERATION_DEADLINE;
const SEALED_PROJECTION_BYTES_PER_SECOND: u64 = 4 * 1024 * 1024;
const SEALED_PROJECTION_DEADLINE_CEILING: Duration = Duration::from_mins(15);
/// How many orphaned pending predecessors one publication attempt will
/// complete before reporting Conflict. Each completion advances the verified
/// head by one, so even a journal wedged across many interrupted boots drains
/// across a few reconcile passes rather than blocking forever.
const MAX_PENDING_REPLAY_COMPLETIONS_V1: usize = 8;

fn sealed_projection_deadline(sealed_bytes: u64) -> Duration {
    let scaled = Duration::from_secs(sealed_bytes.div_ceil(SEALED_PROJECTION_BYTES_PER_SECOND));
    SEALED_PROJECTION_DEADLINE_FLOOR
        .saturating_add(scaled)
        .min(SEALED_PROJECTION_DEADLINE_CEILING)
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

struct MaintenanceGraphCancellationV1(tracedecay_usecases::context::CancellationToken);

impl GraphCancellation for MaintenanceGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct GraphPublicationProbeV1 {
    request_cancellation: Arc<dyn GraphCancellation>,
    lifecycle_cancelled: Arc<AtomicBool>,
    deadline_at: Instant,
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for GraphPublicationProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if self.request_cancellation.is_cancelled()
            || self.lifecycle_cancelled.load(Ordering::Acquire)
        {
            Some(RuntimeInterruptionV1::Cancelled)
        } else if Instant::now() >= self.deadline_at {
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

pub(crate) struct RetainedCodeGraphRuntimeV1 {
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    graph_manifest_provider: Arc<super::code_graph_manifest::DaemonCodeGraphManifestProviderV1>,
    authority: Arc<CanonicalCodeGraphStoreLeaseV1>,
    project_database: Arc<crate::db::Database>,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    code_shard: StoreShardIdV1,
    generation_id: CodeGenerationId,
    generations_root: std::path::PathBuf,
    replay_root: std::path::PathBuf,
    sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest,
    lifecycle_cancelled: Arc<AtomicBool>,
    /// Registry-owned per-shard gate; see
    /// `DaemonSessionRuntimeRegistryV1::code_graph_publication_gates`.
    publication_gate: Arc<std::sync::Mutex<()>>,
}

/// Memory-shard publication runtime for immutable non-code graph journeys.
///
/// Code and journey projections share the daemon's sole `GraphDbRegistry` and
/// physical Grafeo store. Journey manifests use canonical inline replay; code
/// generations keep their sealed replay source through
/// [`RetainedCodeGraphRuntimeV1`].
pub(crate) struct RetainedVerifiedGraphRuntimeV1 {
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    database: crate::db::DatabaseOwnerV1,
    graph: GraphDbOwnerAttachmentV1,
    store_target: Mutex<Option<CanonicalGraphStoreOwnerRetirementTargetV1>>,
    relational_binding: tracedecay_store::StoreRuntimeBindingV1,
    relational_verified_locator: tracedecay_store::VerifiedStoreLocatorV1,
    operation_admission: Mutex<MemoryGraphOperationAdmissionV1>,
    publication_gate: Mutex<()>,
    lifecycle_cancelled: Arc<AtomicBool>,
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
    ) -> std::result::Result<crate::db::Database, GraphDbError> {
        self.require_operation_admission()?;
        self.database.issue_lease().map_err(|error| {
            GraphDbError::unavailable(format!(
                "memory database owner cannot issue a client: {error:?}"
            ))
        })
    }

    pub(crate) fn issue_database_read_only_lease(
        &self,
    ) -> std::result::Result<crate::db::Database, GraphDbError> {
        self.require_operation_admission()?;
        self.database.issue_read_only_lease().map_err(|error| {
            GraphDbError::unavailable(format!(
                "memory database owner cannot issue a read-only client: {error:?}"
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
            .ok_or(GraphDbError::Conflict)
    }

    pub(crate) fn restore_store_graph_retirement_target(
        &self,
        target: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> std::result::Result<(), GraphDbError> {
        let mut retained = self.store_target.lock().map_err(|_| {
            GraphDbError::unavailable("memory graph retirement target lock is poisoned")
        })?;
        if retained.is_some() {
            return Err(GraphDbError::Conflict);
        }
        *retained = Some(target);
        Ok(())
    }

    pub(crate) fn graph_retirement_target(&self) -> tracedecay_graph_db::GraphDbRetirementTarget {
        self.graph.retirement_target()
    }

    pub(crate) fn reserve_database_retirement(
        &self,
    ) -> std::result::Result<crate::db::DatabaseOwnerRetirementReservationV1, GraphDbError> {
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
            MemoryGraphOperationAdmissionV1::Retiring => Err(GraphDbError::Conflict),
        }
    }

    pub(crate) fn reserve_operation_retirement(
        &self,
    ) -> std::result::Result<MemoryGraphOperationRetirementReservationV1<'_>, GraphDbError> {
        let mut admission = self.operation_admission.lock().map_err(|_| {
            GraphDbError::unavailable("memory graph operation admission lock is poisoned")
        })?;
        if matches!(*admission, MemoryGraphOperationAdmissionV1::Retiring) {
            return Err(GraphDbError::Conflict);
        }
        *admission = MemoryGraphOperationAdmissionV1::Retiring;
        Ok(MemoryGraphOperationRetirementReservationV1 {
            runtime: self,
            armed: true,
        })
    }

    pub(crate) fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        let _publication = self.publication_gate.lock().map_err(|_| {
            GraphDbError::unavailable("verified graph publication gate is poisoned")
        })?;
        let database = self.issue_database_lease()?;
        let mut storage = database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let graph = self.graph.issue_lease()?;
        let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
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
            lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
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
                lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
                deadline_at,
                cancellation: publish_cancellation_identity.clone(),
                deadline: publish_deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
            };
            let publish_control = RuntimeRequestControlV1 {
                requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
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
                    return Err(GraphDbError::Conflict);
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
                    return Err(GraphDbError::Conflict);
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
                return Err(GraphDbError::Conflict);
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
                        return Err(GraphDbError::Conflict);
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
                        return Err(GraphDbError::Conflict);
                    }
                    completed_predecessors += 1;
                    replay = requested_replay(actual)?;
                }
                GraphReplayAppendOutcomeV1::Conflict { .. }
                | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. } => {
                    return Err(GraphDbError::Conflict);
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
            lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
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

impl RetainedCodeGraphRuntimeV1 {
    pub(crate) fn authority(&self) -> Arc<CanonicalCodeGraphStoreLeaseV1> {
        Arc::clone(&self.authority)
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

    pub(crate) fn publish_verified_snapshot(
        &self,
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        if generation.manifest().generation_id != self.generation_id {
            return Err(GraphDbError::Conflict);
        }
        // One publisher per code shard at a time, across every retained
        // runtime instance. The seat pass and the background reconcile both
        // reach this path for the same sealed generation; the loser waits
        // here (this runs inside spawn_blocking), then finds the verified
        // head already advanced and takes the idempotent recovery arm below
        // instead of racing the graph database into a Conflict.
        let _publication = self
            .publication_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let projection_deadline = sealed_projection_deadline(self.sealed_generation_bytes()?);
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
            lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
            deadline_at,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
            deadline: deadline_identity,
            cancellation: cancellation_identity,
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.authority.clone();
        let registration = || GraphDbRegistration {
            authority_lease: Arc::clone(&authority_lease),
            cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &request_cancelled,
            ))),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.lifecycle_cancelled,
            ))),
            deadline: deadline_at,
        };
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
        self.graph_manifest_provider.bind(
            self.authority.binding().shard_id.clone(),
            self.project_id.clone(),
            self.repository_id.clone(),
            self.replay_root.clone(),
        )?;
        let source = SealedCodeGenerationReplay {
            repository: self.repository_id.clone(),
            generation: self.generation_id.clone(),
            sealed_state_digest: self.sealed_state_digest.clone(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )?,
        };
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
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
        let publish = |storage: &mut dyn GraphPublicationStoreV1,
                       key: &GraphPublicationKeyV1,
                       manifest: Option<GraphGenerationManifest>|
         -> std::result::Result<_, GraphDbError> {
            let deadline_at = Instant::now() + projection_deadline;
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
                AtomicGraphCancellationV1::new(Arc::clone(&request_cancelled)),
            );
            let probe = GraphPublicationProbeV1 {
                request_cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
                deadline_at,
                cancellation: cancellation_identity.clone(),
                deadline: deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
            };
            let control = RuntimeRequestControlV1 {
                requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
                deadline: deadline_identity,
                cancellation: cancellation_identity,
            };
            let context = GraphPublicationOperationContextV1::new(&control, &probe)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let registration = GraphDbRegistration {
                authority_lease: Arc::clone(&authority_lease),
                cancellation: request_cancellation,
                lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                    &self.lifecycle_cancelled,
                ))),
                deadline: deadline_at,
            };
            // The already-built projection manifest rides along so first
            // publication does not re-read and re-project the sealed artifact
            // through the replay manifest provider; a pending predecessor
            // journaled by an interrupted publisher carries no in-hand
            // manifest, so publication reconstructs it from the journaled
            // canonical replay source.
            self.graph_registry
                .publish_verified(registration, storage, &context, key, manifest)
        };
        match storage
            .replay(&publication_key, &context)
            .map_err(map_publication_error)?
        {
            GraphPublicationReplayLookupV1::Active(_) => {
                install_project_graph_replay_seal_at(
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
                )?;
                let head = storage
                    .verified_head(&relational_projection, &context)
                    .map_err(map_publication_error)?;
                if head
                    .as_ref()
                    .is_some_and(|head| head.key == publication_key)
                {
                    return self.graph_registry.recover_verified_snapshot(
                        registration(),
                        &mut storage,
                        &context,
                        &relational_projection,
                    );
                }
                let publication = publish(&mut storage, &publication_key, Some(manifest))?;
                return Ok(publication.snapshot);
            }
            GraphPublicationReplayLookupV1::Retired(_) => return Err(GraphDbError::Conflict),
            GraphPublicationReplayLookupV1::Missing => {}
        }
        let staged_seal = stage_project_graph_replay_seal(
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
        )?;
        let replay_pool_lock = lock_project_graph_replay_pool(&self.replay_root, &|| match probe
            .interruption()
        {
            Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
            Some(RuntimeInterruptionV1::DeadlineExceeded) => Err(GraphDbError::DeadlineExceeded),
            None => Ok(()),
        })?;
        publish_staged_replay_seal(
            staged_seal,
            &self.replay_root,
            &self.sealed_state_digest,
            &|| match probe.interruption() {
                Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                    Err(GraphDbError::DeadlineExceeded)
                }
                None => Ok(()),
            },
        )?;
        let input = canonical_sha256(&(
            "tracedecay.code-graph-publication-input.v1",
            &source,
            &manifest.generation,
            &manifest.source_generation,
            &manifest.watermark,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let build_replay = |prior: Option<GraphVerifiedHeadV1>| {
            manifest.relational_sealed_replay(
                self.authority.binding().shard_id.clone(),
                idempotency_key.clone(),
                GraphPublicationInputDigestV1::new(input.as_str())
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                prior,
                source.clone(),
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
            .verified_head(&relational_projection, &context)
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
            match storage
                .append_replay(&replay, &context)
                .map_err(map_publication_error)?
            {
                GraphReplayAppendOutcomeV1::Appended(_)
                | GraphReplayAppendOutcomeV1::ExactReplay(_)
                | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => break,
                GraphReplayAppendOutcomeV1::PendingReplayConflict { pending } => {
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return Err(GraphDbError::Conflict);
                    }
                    completed_predecessors += 1;
                    publish(&mut storage, &pending.publication.key, None)?;
                    let prior = storage
                        .verified_head(&relational_projection, &context)
                        .map_err(map_publication_error)?;
                    replay = build_replay(prior)?;
                }
                GraphReplayAppendOutcomeV1::VerifiedHeadConflict { actual } => {
                    // A concurrent publisher advanced the head between our
                    // read and this append; the refreshed head is the only
                    // thing that was wrong with the replay.
                    if completed_predecessors >= MAX_PENDING_REPLAY_COMPLETIONS_V1 {
                        return Err(GraphDbError::Conflict);
                    }
                    completed_predecessors += 1;
                    replay = build_replay(actual)?;
                }
                GraphReplayAppendOutcomeV1::Conflict { .. }
                | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. } => {
                    return Err(GraphDbError::Conflict);
                }
            }
        }
        drop(replay_pool_lock);
        let publication = publish(&mut storage, &replay.key, Some(manifest))?;
        Ok(publication.snapshot)
    }

    fn sealed_generation_bytes(&self) -> std::result::Result<u64, GraphDbError> {
        let digest = self
            .sealed_state_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
        let path = self
            .generations_root
            .join(format!("generation-{digest}.json"));
        std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .map_err(|error| {
                GraphDbError::unavailable(format!(
                    "sealed code generation file is unreadable at '{}': {error}",
                    path.display()
                ))
            })
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
            lifecycle_cancelled: Arc::clone(&self.lifecycle_cancelled),
            deadline_at: deadline,
            cancellation: cancellation_identity.clone(),
            deadline: deadline_identity.clone(),
            commit_started: AtomicBool::new(false),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn retain_code_graph_runtime(
        &self,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
        generation_id: CodeGenerationId,
        project_database: Arc<crate::db::Database>,
        replay_binding: crate::daemon::code_index_scheduler::CodeGraphReplayBindingV1,
    ) -> Result<RetainedCodeGraphRuntimeV1> {
        let project_shard = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
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
                StoreRuntimeKey::new(project_shard, self.incarnation),
                code_shard.clone(),
                generation_id.clone(),
            )
            .await
            .map_err(|failure| {
                session_registry_error("retain exact code graph authority", format!("{failure:?}"))
            })?;
        let replay_root = project_database
            .database_path()
            .with_extension("graph-replay");
        let publication_gate = {
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
            publication_gate,
        })
    }

    pub(crate) async fn reconcile_deleted_code_generation_graph_replays(
        &self,
        project_id: ProjectId,
        project_database: &crate::db::Database,
        generation: &CodeGenerationId,
        generation_file: &str,
        cancellation: &tracedecay_usecases::context::CancellationToken,
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
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(project_shard, self.incarnation))
            .await
            .map_err(|error| GraphDbError::unavailable(format!("{error:?}")))?;
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
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority;
        let mut storage = project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;

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
                lifecycle_cancelled: Arc::clone(&self.graph_lifecycle_cancelled),
                deadline_at,
                cancellation: cancellation_identity.clone(),
                deadline: deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
            };
            let control = RuntimeRequestControlV1 {
                requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
                deadline: deadline_identity,
                cancellation: cancellation_identity,
            };
            let context = GraphPublicationOperationContextV1::new(&control, &probe)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let registration = GraphDbRegistration {
                authority_lease: Arc::clone(&authority_lease),
                cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                    &self.graph_lifecycle_cancelled,
                ))),
                deadline: deadline_at,
            };
            match self.graph_registry.retire_one_code_generation_replay(
                registration,
                &mut storage,
                &context,
                generation,
                &sealed_digest,
            )? {
                GraphReplayCollectionOutcome::Retired(source) => {
                    let tracedecay_graph_db::GraphGenerationReplaySource::SealedCodeGeneration(
                        source,
                    ) = source
                    else {
                        return Err(GraphDbError::Corrupt {
                            message: "code generation retirement selected an inline graph replay"
                                .to_owned(),
                        });
                    };
                    if source.generation != *generation
                        || source.sealed_state_digest != sealed_digest
                    {
                        return Err(GraphDbError::Conflict);
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
                lifecycle_cancelled: Arc::clone(&self.graph_lifecycle_cancelled),
                deadline_at,
                cancellation: cancellation_identity.clone(),
                deadline: deadline_identity.clone(),
                commit_started: AtomicBool::new(false),
            };
            let control = RuntimeRequestControlV1 {
                requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
                deadline: deadline_identity,
                cancellation: cancellation_identity,
            };
            let context = GraphPublicationOperationContextV1::new(&control, &probe)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let registration = GraphDbRegistration {
                authority_lease: Arc::clone(&authority_lease),
                cancellation: Arc::clone(&request_cancellation),
                lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                    &self.graph_lifecycle_cancelled,
                ))),
                deadline: deadline_at,
            };
            if !self
                .graph_registry
                .finalize_one_code_generation_replay_cleanup(
                    registration,
                    &mut storage,
                    &context,
                    generation,
                    &sealed_digest,
                )?
            {
                return Ok(true);
            }
        }
    }

    pub(super) async fn retain_session_relation_graph_owner(
        &self,
        shard_id: StoreShardIdV1,
    ) -> Result<SessionGraphOwnerV1> {
        let (graph, store_target) = graph_attachment::open_session_relation_owner(
            &self.registry,
            &self.graph_registry,
            &self.graph_lifecycle_cancelled,
            self.incarnation,
            shard_id,
        )
        .await?;
        Ok(SessionGraphOwnerV1 {
            graph,
            store_target,
        })
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
        CodeGraphProjectionError::BudgetExhausted => {
            GraphDbError::budget_exhausted(GraphBudgetKind::Read, u64::MAX)
        }
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::Conflict => GraphDbError::Conflict,
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
    use super::{
        SEALED_PROJECTION_DEADLINE_CEILING, SEALED_PROJECTION_DEADLINE_FLOOR,
        sealed_projection_deadline,
    };

    #[test]
    fn sealed_projection_deadline_scales_with_artifact_size_between_floor_and_ceiling() {
        assert_eq!(
            sealed_projection_deadline(0),
            SEALED_PROJECTION_DEADLINE_FLOOR
        );
        let small = sealed_projection_deadline(8 * 1024 * 1024);
        assert!(small > SEALED_PROJECTION_DEADLINE_FLOOR);
        assert!(small < SEALED_PROJECTION_DEADLINE_CEILING);
        // The live incident shape: a ~1.6 GB sealed generation must get far
        // more than the ordinary 30-second graph-operation budget.
        let incident = sealed_projection_deadline(1_603_803_371);
        assert!(incident >= std::time::Duration::from_mins(5));
        assert!(incident <= SEALED_PROJECTION_DEADLINE_CEILING);
        assert_eq!(
            sealed_projection_deadline(u64::MAX),
            SEALED_PROJECTION_DEADLINE_CEILING
        );
    }
}
