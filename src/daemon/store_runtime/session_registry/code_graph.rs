use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    CodeGenerationId, RefId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbRegistration, GraphGenerationDependency,
    GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity, GraphProjectorRevision,
    GraphReplayCollectionOutcome, GraphWriteBatch, SealedCodeGenerationReplay,
    VerifiedGenerationBatchCommit, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalCodeGraphStoreLeaseV1, CanonicalGraphStoreLeaseV1, StoreRuntimeKey,
};
use tracedecay_store::{
    CodeShardScopeV1, GraphGenerationIdV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreV1, GraphReplayAppendOutcomeV1, ProjectId,
    RetainedGraphStoreLeaseV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, SemanticVectorStageBatchReceipt, SemanticVectorStageCancelOutcome,
    SemanticVectorStageKey, SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, SemanticVectorStagingStore,
    StoreShardIdV1,
};

use super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

mod seals;
mod semantic_vector;
use seals::{
    finalize_project_graph_replay_unlink, install_project_graph_replay_seal_at,
    lock_project_graph_replay_pool, publish_staged_replay_seal, sealed_digest_from_generation_file,
    stage_project_graph_replay_seal, stage_project_graph_replay_unlink,
};

const GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
const GRAPH_OPEN_DEADLINE: Duration = Duration::from_secs(30);

struct AtomicGraphCancellationV1 {
    cancelled: Arc<AtomicBool>,
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
}

/// Project-scoped publication runtime for immutable non-code graph journeys.
///
/// Code and journey projections share the daemon's sole `GraphDbRegistry` and
/// physical Grafeo store. Journey manifests use canonical inline replay; code
/// generations keep their sealed replay source through
/// [`RetainedCodeGraphRuntimeV1`].
pub(crate) struct RetainedProjectGraphRuntimeV1 {
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    authority: Arc<CanonicalGraphStoreLeaseV1>,
    project_database: Arc<crate::db::Database>,
    lifecycle_cancelled: Arc<AtomicBool>,
}

impl RetainedProjectGraphRuntimeV1 {
    pub(crate) fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
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
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.authority.clone();
        let registration = || GraphDbRegistration {
            authority_lease: Arc::clone(&authority_lease),
            cancellation: Arc::clone(&request_cancellation),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.lifecycle_cancelled,
            ))),
            deadline: deadline_at,
        };
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.authority.binding().shard_id.clone(),
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
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
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
            deadline: publish_deadline_identity,
            cancellation: publish_cancellation_identity,
        };
        let publish_context =
            GraphPublicationOperationContextV1::new(&publish_control, &publish_probe)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        match storage
            .replay(&publication_key, &context)
            .map_err(map_publication_error)?
        {
            GraphPublicationReplayLookupV1::Active(journaled) => {
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
                let publication = self.graph_registry.publish_verified(
                    registration(),
                    &mut storage,
                    &publish_context,
                    &publication_key,
                )?;
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
        let input = canonical_sha256(&(
            "tracedecay.inline-graph-publication-input.v1",
            &publication_key,
            manifest,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let replay = manifest.relational_replay(
            self.authority.binding().shard_id.clone(),
            idempotency_key,
            GraphPublicationInputDigestV1::new(input.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            prior,
            &|| match probe.interruption() {
                Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                    Err(GraphDbError::DeadlineExceeded)
                }
                None => Ok(()),
            },
        )?;
        match storage
            .append_replay(&replay, &context)
            .map_err(map_publication_error)?
        {
            GraphReplayAppendOutcomeV1::Appended(_)
            | GraphReplayAppendOutcomeV1::ExactReplay(_)
            | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => {}
            GraphReplayAppendOutcomeV1::Conflict { .. }
            | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. }
            | GraphReplayAppendOutcomeV1::VerifiedHeadConflict { .. }
            | GraphReplayAppendOutcomeV1::PendingReplayConflict { .. } => {
                return Err(GraphDbError::Conflict);
            }
        }
        let publication = self.graph_registry.publish_verified(
            registration(),
            &mut storage,
            &publish_context,
            &replay.key,
        )?;
        Ok(publication.snapshot)
    }

    pub(crate) fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
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
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.authority.binding().shard_id.clone(),
            namespace: tracedecay_store::GraphNamespaceV1::new(projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.authority.clone();
        let registration = GraphDbRegistration {
            authority_lease,
            cancellation: Arc::new(AtomicGraphCancellationV1::new(request_cancelled)),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.lifecycle_cancelled,
            ))),
            deadline: deadline_at,
        };
        let mut storage = self
            .project_database
            .graph_publication_storage()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
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
            .recover_verified_snapshot(registration, &mut storage, &context, &relational_projection)
            .map(Some)
    }
}

impl crate::global_db::ProjectGraphRuntimePortV1 for RetainedProjectGraphRuntimeV1 {
    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        RetainedProjectGraphRuntimeV1::publish_verified_manifest(
            self,
            manifest,
            idempotency_key,
            cancelled,
        )
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        RetainedProjectGraphRuntimeV1::verified_snapshot(self, projection, cancelled)
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
        let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
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
                       key: &GraphPublicationKeyV1|
         -> std::result::Result<_, GraphDbError> {
            let deadline_at = Instant::now() + GRAPH_OPERATION_DEADLINE;
            let cancellation_identity = RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!(
                    "graph-publish-commit:{}",
                    graph_generation.as_str()
                ))
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                generation: 2,
            };
            let deadline_identity = RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!(
                    "graph-publish-commit-deadline:{}",
                    graph_generation.as_str()
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
            self.graph_registry.publish_verified(
                GraphDbRegistration {
                    authority_lease: Arc::clone(&authority_lease),
                    cancellation: request_cancellation,
                    lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                        &self.lifecycle_cancelled,
                    ))),
                    deadline: deadline_at,
                },
                storage,
                &context,
                key,
            )
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
                let publication = publish(&mut storage, &publication_key)?;
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
        let prior = storage
            .verified_head(&relational_projection, &context)
            .map_err(map_publication_error)?;
        let input = canonical_sha256(&(
            "tracedecay.code-graph-publication-input.v1",
            &source,
            &manifest.generation,
            &manifest.source_generation,
            &manifest.watermark,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let replay = manifest.relational_sealed_replay(
            self.authority.binding().shard_id.clone(),
            idempotency_key,
            GraphPublicationInputDigestV1::new(input.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            prior,
            source,
            &|| match probe.interruption() {
                Some(RuntimeInterruptionV1::Cancelled) => Err(GraphDbError::Cancelled),
                Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                    Err(GraphDbError::DeadlineExceeded)
                }
                None => Ok(()),
            },
        )?;
        match storage
            .append_replay(&replay, &context)
            .map_err(map_publication_error)?
        {
            GraphReplayAppendOutcomeV1::Appended(_)
            | GraphReplayAppendOutcomeV1::ExactReplay(_)
            | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => {}
            GraphReplayAppendOutcomeV1::Conflict { .. }
            | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. }
            | GraphReplayAppendOutcomeV1::VerifiedHeadConflict { .. }
            | GraphReplayAppendOutcomeV1::PendingReplayConflict { .. } => {
                return Err(GraphDbError::Conflict);
            }
        }
        drop(replay_pool_lock);
        let publication = publish(&mut storage, &replay.key)?;
        Ok(publication.snapshot)
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
    pub(crate) async fn retain_project_graph_runtime(
        &self,
        project_id: ProjectId,
        project_database: Arc<crate::db::Database>,
    ) -> Result<RetainedProjectGraphRuntimeV1> {
        let project_shard = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(project_shard, self.incarnation))
            .await
            .map_err(|failure| {
                session_registry_error("retain project graph authority", format!("{failure:?}"))
            })?;
        Ok(RetainedProjectGraphRuntimeV1 {
            graph_registry: self.graph_registry.clone(),
            authority,
            project_database,
            lifecycle_cancelled: Arc::clone(&self.graph_lifecycle_cancelled),
        })
    }

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
            cleanup_sequence = cleanup_sequence
                .checked_add(1)
                .ok_or(GraphDbError::BudgetExhausted)?;
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

    /// Retains the daemon-owned native relation graph for one exact session
    /// shard and opens it through the shared graph registry.
    pub(crate) async fn retain_session_relation_graph_runtime(
        &self,
        shard_id: StoreShardIdV1,
    ) -> Result<Arc<GraphDb>> {
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(shard_id, self.incarnation))
            .await
            .map_err(|failure| {
                session_registry_error(
                    "retain exact session relation graph authority",
                    format!("{failure:?}"),
                )
            })?;
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority;
        let registration = GraphDbRegistration {
            authority_lease,
            cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.graph_lifecycle_cancelled,
            ))),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.graph_lifecycle_cancelled,
            ))),
            deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
        };
        let graph_registry = self.graph_registry.clone();
        tokio::task::spawn_blocking(move || graph_registry.resolve(registration))
            .await
            .map_err(|error| {
                session_registry_error("join session relation graph open", error.to_string())
            })?
            .map_err(|error| {
                session_registry_error("open session relation graph runtime", error.to_string())
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
        CodeGraphProjectionError::BudgetExhausted => GraphDbError::BudgetExhausted,
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
    }
}
