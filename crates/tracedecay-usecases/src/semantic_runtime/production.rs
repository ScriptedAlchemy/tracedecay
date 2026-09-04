//! Production bridge between daemon semantic scheduling and application search.
//!
//! Saved code generations call [`schedule_saved_code_generation`] without waiting
//! for `FastEmbed` download/indexing. Application search admits a semantic lane
//! only through [`query_factory`] once the committed configuration's complete
//! generation is present in the exact warmed cache. Status projection carries indexing progress, degraded
//! reason, and prior generation for Doctor/`tracedecay_runtime`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkV1,
    CompactCandidate, ComponentRevision, EmbeddingDocumentCompositionV1, EvidenceRole,
    FixedPointScore, LogicalEvidenceId, ManifestDigest, ProjectionBatchRequestV1,
    ProjectionOperationV1, ProjectionReplayReasonV1, QueryFallbackSubpayload, RetrievalAnchorId,
    RetrievalCursorKeyId, RetrieverBatch, RetrieverKind, RetrieverOutcome, ScoreDomainId,
    SemanticSearchIndexKeyV1, SemanticSearchIndexKindV1, SemanticSearchIndexProfileV1,
    SourceOccurrenceId, VectorGenerationIdV1, WorktreeId, canonical_sha256,
};
use tracedecay_policy::retrieval_selection::{
    RetrievalAvailabilityV1, RetrievalRequirementV1, RetrievalSelectionV1, select_retrieval,
};
#[cfg(test)]
use tracedecay_semantic_contracts::SemanticFallbackReasonV1;
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticLifecycleVerifiedReadyEventV1,
    SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1, SemanticResourceCeilings,
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
};

use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, IsolatedSemanticEvaluationGraphV1, PublishedVectorGenerationV1,
    SemanticAnnServingIndexV1, SemanticVectorStageDescriptorV1, VectorGenerationBeginOutcomeV1,
    VectorGenerationPlanV1, VectorGenerationStoreErrorV1, generation_identity_digest,
    isolated_semantic_evaluation_graph,
};

mod application_status;
mod publication_failure;
mod vector_projection_support;
pub use application_status::{
    application_status_from_projection, lifecycle_to_runtime_state,
    prefer_lifecycle_over_generic_unavailable, resolve_semantic_application_status,
};
use publication_failure::SemanticPublicationFailureRecorderV1;
use tracedecay_code_index::embedding_document::{
    EmbeddingDocumentComposerV1, EmbeddingSymbolContextIndexV1,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_code_index::projection::expected_request_digest;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_query::retrieval::graph::production_code_index_freshness;
use tracedecay_query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError,
};
use tracedecay_query::retrieval::rerank::RerankExecutionControlV1;
use tracedecay_query::retrieval::semantic::{
    CalibratedSemanticQueryService, CodeSemanticEvidenceV1, CompleteSemanticGenerationV1,
    SemanticAbstentionDispositionV1, SemanticAnnCandidateWindowV1, SemanticAnnCandidatesV1,
    SemanticAnnIndexStateV1, SemanticCalibrationProfileV1, SemanticCodeRetriever,
    SemanticExecutionControl, SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticLaneRetriever,
    SemanticQueryDecisionV1, SemanticQueryModeV1, SemanticQueryServiceError,
    SemanticQueryServiceOutcomeV1, SemanticRetrievalRequestV1, SemanticSearchKindV1,
    SemanticVectorReadPort, SemanticVectorReadRequestV1, SemanticVectorRecordV1,
    SemanticVectorScanSummaryV1,
};
use tracedecay_query::search_quality::candidate_output::ProductionCandidateSemanticProjectionSourcesV1;
use tracedecay_query::search_quality::semantic_native::{
    SemanticProjectionCaseOutcomeV1, SemanticProjectionCaseSampleV1, SemanticProjectionCaseV1,
};
use tracedecay_query::search_quality::{
    CandidateOutputError, ProductionCandidateNativeGenerationResourcesV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::rerank_adapter::{
    GenerationBoundCodeRerankViewsV1, ProductionCodeRerankAuthorityV1,
};
use tracedecay_semantic::{
    DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticEvaluationProjectionV1, PreparedSemanticRuntimeCommitV1,
    PreparedSemanticRuntimeObservationV1, PreparedSemanticRuntimeRestoreV1,
    SemanticEvaluationCancellationV1, SemanticEvaluationProjectionBatchCachePolicyV1,
    SemanticEvaluationProjectionBatchCacheV1, SemanticEvaluationProjectionResourcesV1,
    SemanticEvaluationQueryFactoryV1, SemanticModelLifecycleEvaluationPublicationLeaseV1,
    SemanticModelLifecycleOwnerV1, SemanticModelLifecyclePublicationIdentityV1,
    SemanticProjectionResumeOutcomeV1, measure_semantic_evaluation_projection_cancellation,
    prepare_semantic_evaluation_projection,
};
use vector_projection_support::{
    BatchCommitStateV1, commit_evaluation_prepared_generation, projection_input_bytes,
};

use super::acceptance_calibration::{
    UNCALIBRATED_MAXIMUM_DISTANCE_MICROS, measure_acceptance_calibration,
};
use super::graph_provider::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticVectorGraphProviderV1,
};
#[cfg(test)]
use super::ports::SemanticActivationRequestV1;
use super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticConfigurationPinV1,
    SemanticExecutableGenerationLeaseV1, SemanticExecutableGenerationV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use super::{
    DaemonGlobalSemanticProjectionSchedulerV1, SemanticProjectionBatchV1,
    SemanticProjectionLeaseV1, SemanticProjectionScheduleErrorV1,
};
#[cfg(test)]
use tracedecay_semantic::SemanticExecutionInterruptionV1;

struct SemanticEvaluationGraphCancellationV1 {
    evaluation: Arc<dyn SemanticEvaluationCancellationV1>,
}

impl GraphCancellation for SemanticEvaluationGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.evaluation.interruption().is_some()
    }
}

/// Chunks embedded before the run commits and releases them.
///
/// This bounds the live float set and the work a crash discards, and it is a
/// multiple of the projector's encoder group size so splitting a run never
/// changes a tensor shape and therefore never changes a vector. It is sizing,
/// not semantics: the generation a run publishes is identical at any value.
///
/// The durable stage receipt contract owns the batch ceiling. Keeping the
/// projector at that same limit prevents a prepared batch from requiring a
/// second, adapter-local receipt partition and preserves exact restart replay.
const SEMANTIC_EMBEDS_PER_COMMIT: usize =
    tracedecay_store::MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH;

/// Schedule `FastEmbed` projection for one published code generation.
///
/// Returns immediately after enqueueing; artifact load, model download, and
/// indexing run asynchronously and never join into ordinary search.
#[hotpath::measure(label = "usecases.semantic.schedule")]
pub fn schedule_saved_code_generation<LoadArtifact, StageProjection, StageFuture>(
    handle: &DaemonSemanticRuntimeHandleV1,
    generation: &CodeIndexPublishedGenerationV1,
    load_artifact: LoadArtifact,
    stage_projection: StageProjection,
) -> bool
where
    LoadArtifact: FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
    StageProjection: FnOnce() -> StageFuture + Send + 'static,
    StageFuture: Future<Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>>
        + Send
        + 'static,
{
    let Ok(request) = FastEmbedSemanticGenerationRequestV1::new(
        generation.manifest().generation_id.clone(),
        generation.projection().request().clone(),
        generation.chunks().chunks().to_vec(),
        embedding_documents(generation),
        SEMANTIC_EMBEDS_PER_COMMIT,
        load_artifact,
        // This helper owns no staged build, so it never resumes and its
        // batches commit nowhere; callers that need durability go through
        // `ProductionSemanticRuntimeV1`.
        || async { Ok(SemanticProjectionResumeOutcomeV1::ReplayFromStart) },
        |_prepared| async { Ok(()) },
        stage_projection,
    ) else {
        return false;
    };
    // Enqueue only — callers must not await download/index completion.
    handle.schedule_generation(request)
}

/// The symbol-context authority that composes one published generation's
/// embedding documents. It is the generation's own sealed symbol index, so
/// header content is parser evidence from the same sanitized bytes as the
/// chunks it accompanies.
fn embedding_documents(
    generation: &CodeIndexPublishedGenerationV1,
) -> Arc<EmbeddingDocumentComposerV1> {
    Arc::new(EmbeddingDocumentComposerV1::new(
        EmbeddingSymbolContextIndexV1::from_generation_symbols(generation.symbols()),
    ))
}

/// Daemon-owned production bridge from lifecycle-ready model bytes to the
/// persistent vector store and exact process-local query cache.
#[derive(Clone)]
pub struct ProductionSemanticRuntimeV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    graph: Arc<dyn SemanticVectorGraphProviderV1>,
    /// Runtime-owned writer lane shared across clones.
    vector_writer: Arc<tokio::sync::Mutex<()>>,
    code_index_store_root: PathBuf,
    lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    resources: SemanticResourceCeilings,
    /// Configured embedding-document composition; with `resources` it fixes
    /// the projection key every production and evaluator projection mints.
    document_composition: EmbeddingDocumentCompositionV1,
    vector_read_cache: Arc<Mutex<Option<CachedPublishedVectorsV1>>>,
}

struct CachedPublishedVectorsV1 {
    generation: VectorGenerationIdV1,
    search_index_key: SemanticSearchIndexKeyV1,
    source_generation: CodeGenerationId,
    port: Arc<PublishedSemanticVectorReadPortV1>,
}

impl CachedPublishedVectorsV1 {
    fn matches(
        &self,
        generation: &VectorGenerationIdV1,
        projection_key: &tracedecay_domain::ProjectionKeyV1,
        search_index_key: &SemanticSearchIndexKeyV1,
        source_generation: &CodeGenerationId,
        capability_manifest_digest: &ManifestDigest,
    ) -> bool {
        self.generation == *generation
            && self.search_index_key == *search_index_key
            && self.source_generation == *source_generation
            && self.port.projection_key == *projection_key
            && self.port.capability_manifest_digest == *capability_manifest_digest
    }
}

fn retained_vector_read_port(
    cache: &Mutex<Option<CachedPublishedVectorsV1>>,
    generation: &VectorGenerationIdV1,
    projection_key: &tracedecay_domain::ProjectionKeyV1,
    search_index_key: &SemanticSearchIndexKeyV1,
    source_generation: &CodeGenerationId,
    capability_manifest_digest: &ManifestDigest,
) -> Option<Arc<PublishedSemanticVectorReadPortV1>> {
    let cached = cache.lock().ok()?;
    cached
        .as_ref()
        .filter(|cached| {
            cached.matches(
                generation,
                projection_key,
                search_index_key,
                source_generation,
                capability_manifest_digest,
            )
        })
        .map(|cached| Arc::clone(&cached.port))
}

/// The handles every stage of one scheduled projection shares.
///
/// A scheduled generation runs as four callbacks — load, resume, per-batch
/// commit, and stage/publish — and each of them needs the same graph provider,
/// writer lane, scheduled generation, batch commit state, and lifecycle owner.
/// Holding them in one `Arc` keeps each callback to a single clone instead of
/// one clone per handle per stage, and keeps "what a stage may touch" stated in
/// one place.
struct ScheduledProjectionHandlesV1 {
    graph: Arc<dyn SemanticVectorGraphProviderV1>,
    /// Runtime-owned writer lane shared across clones.
    writer: Arc<tokio::sync::Mutex<()>>,
    /// The generation this schedule is projecting.
    generation: Arc<CodeIndexPublishedGenerationV1>,
    /// Build, store, and checkpoint carried across batch commits.
    commit_state: Arc<tokio::sync::Mutex<BatchCommitStateV1>>,
    lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    vector_read_cache: Arc<Mutex<Option<CachedPublishedVectorsV1>>>,
}

#[derive(Clone, Debug)]
pub struct SemanticCompatibleCurrentGenerationSnapshotV1 {
    pub executable: SemanticExecutableGenerationV1,
    pub vector_state_revision: i64,
    pub vector_generation_id: VectorGenerationIdV1,
}

#[derive(Clone, Debug)]
pub struct SemanticEvaluationCurrentGenerationSnapshotV1 {
    pub vector_state_revision: i64,
    pub vector_generation_id: VectorGenerationIdV1,
    pub source_manifest_digest: ManifestDigest,
}

/// Exact pre-acceptance target certified against the live semantic runtime.
///
/// This is intentionally independent of accepted-profile configuration: it
/// proves that a proposed semantic compatibility pin can be evaluated against
/// the current vector generation and the runtime's actual configured ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVerifiedEvaluationTargetSnapshotV1 {
    semantic_compatibility: crate::config::retrieval::SemanticCompatibilityPinsV1,
    vector_state_revision: i64,
    vector_generation_id: VectorGenerationIdV1,
    configured_resource_ceiling: crate::config::retrieval::SemanticResourceRequirementV1,
    lifecycle_verification: SemanticEvaluationLifecycleVerificationV1,
}

impl SemanticVerifiedEvaluationTargetSnapshotV1 {
    pub fn semantic_compatibility(&self) -> &crate::config::retrieval::SemanticCompatibilityPinsV1 {
        &self.semantic_compatibility
    }

    pub const fn vector_state_revision(&self) -> i64 {
        self.vector_state_revision
    }

    pub fn vector_generation_id(&self) -> &VectorGenerationIdV1 {
        &self.vector_generation_id
    }

    pub const fn configured_resource_ceiling(
        &self,
    ) -> crate::config::retrieval::SemanticResourceRequirementV1 {
        self.configured_resource_ceiling
    }

    pub fn lifecycle_verification(&self) -> &SemanticEvaluationLifecycleVerificationV1 {
        &self.lifecycle_verification
    }
}

/// Opaque lifecycle/runtime observation bound to one pre-acceptance target.
/// Only [`ProductionSemanticRuntimeV1`] can mint or revalidate it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationLifecycleVerificationV1 {
    compatibility: crate::config::retrieval::SemanticCompatibilityPinsV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    capability_manifest_digest: ManifestDigest,
    vector_state_revision: i64,
    lifecycle_identity: SemanticModelLifecyclePublicationIdentityV1,
}

/// Final lifecycle read lease held across daemon publication. Its drop releases
/// model selection, acquisition, and remediation writers through the canonical
/// lifecycle owner.
pub struct SemanticEvaluationPublicationLeaseV1 {
    _lifecycle: SemanticModelLifecycleEvaluationPublicationLeaseV1,
}

pub struct SemanticVectorPublicationLeaseV1 {
    _writer: tokio::sync::OwnedMutexGuard<()>,
}

pub struct PreparedProductionSemanticCacheCommitV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    prepared: PreparedProductionSemanticCacheActionV1,
}

enum PreparedProductionSemanticCacheActionV1 {
    Observation {
        prepared: PreparedSemanticRuntimeObservationV1,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    },
    Restore {
        prepared: Box<PreparedSemanticRuntimeRestoreV1>,
        cache: Arc<Mutex<Option<CachedPublishedVectorsV1>>>,
        vectors: CachedPublishedVectorsV1,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    },
}

impl PreparedProductionSemanticCacheCommitV1 {
    pub fn commit(self) -> bool {
        match self.prepared {
            PreparedProductionSemanticCacheActionV1::Observation {
                prepared,
                lifecycle,
            } => commit_current_observation_and_then(&self.handle, prepared, || {
                let _ = lifecycle.mark_ready();
            }),
            PreparedProductionSemanticCacheActionV1::Restore {
                prepared,
                cache,
                vectors,
                lifecycle,
            } => {
                let Ok(mut cached) = cache.lock() else {
                    return false;
                };
                let previous = cached.replace(vectors);
                let committed = self.handle.commit_restore(*prepared);
                if !committed {
                    *cached = previous;
                    return false;
                }
                drop(cached);
                let _ = lifecycle.mark_ready();
                true
            }
        }
    }
}

fn commit_current_observation_and_then(
    handle: &DaemonSemanticRuntimeHandleV1,
    prepared: PreparedSemanticRuntimeObservationV1,
    after_commit: impl FnOnce(),
) -> bool {
    let committed = handle.commit_current_observation(prepared);
    if committed {
        after_commit();
    }
    committed
}

impl ProductionSemanticRuntimeV1 {
    pub fn new(
        handle: DaemonSemanticRuntimeHandleV1,
        database: Arc<Database>,
        graph: Arc<dyn SemanticVectorGraphProviderV1>,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
        resources: SemanticResourceCeilings,
        document_composition: EmbeddingDocumentCompositionV1,
    ) -> Self {
        let code_index_store_root = database
            .database_path()
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("code-index-v1");
        Self::new_with_code_index_store_root(
            handle,
            graph,
            code_index_store_root,
            lifecycle,
            resources,
            document_composition,
        )
    }

    fn new_with_code_index_store_root(
        handle: DaemonSemanticRuntimeHandleV1,
        graph: Arc<dyn SemanticVectorGraphProviderV1>,
        code_index_store_root: PathBuf,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
        resources: SemanticResourceCeilings,
        document_composition: EmbeddingDocumentCompositionV1,
    ) -> Self {
        Self {
            handle,
            graph,
            vector_writer: Arc::new(tokio::sync::Mutex::new(())),
            code_index_store_root,
            lifecycle,
            resources,
            document_composition,
            vector_read_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn verified_ready_events(
        &self,
    ) -> tokio::sync::watch::Receiver<SemanticLifecycleVerifiedReadyEventV1> {
        self.lifecycle.verified_ready_events()
    }

    /// Process-local lifecycle observation bound to this mounted runtime.
    pub fn lifecycle_status(&self) -> SemanticModelLifecycleStatusV1 {
        self.lifecycle.status()
    }

    /// Restore a compatible immutable generation after daemon restart.
    #[hotpath::measure(label = "usecases.semantic.restore_current", future = true)]
    pub async fn restore_current(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        required_generation: &VectorGenerationIdV1,
    ) -> Result<bool, SemanticRuntimeScheduleFailureV1> {
        let Some(prepared) = self
            .prepare_restore_current(generation, required_generation)
            .await?
        else {
            return Ok(false);
        };
        Ok(prepared.commit())
    }

    #[hotpath::measure(label = "usecases.semantic.prepare_restore", future = true)]
    pub async fn prepare_restore_current(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        required_generation: &VectorGenerationIdV1,
    ) -> Result<Option<PreparedProductionSemanticCacheCommitV1>, SemanticRuntimeScheduleFailureV1>
    {
        let retained = self
            .graph
            .graph_for_generation(generation)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = match GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            required_generation,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?
        {
            Some(store) => store,
            None => return Ok(None),
        };
        let projection = LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let source_manifest_digest =
            semantic_source_manifest_digest(generation.projection().request());
        let active = store
            .generation(required_generation, Arc::clone(&cancellation))
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let Some(active) = active else {
            return Ok(None);
        };
        let replay_digest = semantic_projection_request(generation, &projection, None)?
            .changes
            .manifest_digest;
        if active.generation_id() != required_generation
            || active.embedding_key() != &projection
            || active.source_generation() != &generation.manifest().generation_id
            || (active.source_manifest_digest() != source_manifest_digest
                && active.source_manifest_digest() != &replay_digest)
        {
            return Ok(None);
        }
        let pointer = SemanticGenerationPointerV1 {
            generation: active.generation_id().clone(),
            source_generation: active.source_generation().clone(),
            projection_key: active.projection_key().clone(),
        };
        let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let ann = semantic_ann_serving_index(
            &store,
            &active,
            &search_index_key,
            Arc::clone(&cancellation),
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let port = Arc::new(
            PublishedSemanticVectorReadPortV1::new(
                active,
                search_index_key.clone(),
                generation,
                ann,
            )
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
        let vectors = CachedPublishedVectorsV1 {
            generation: port.generation.clone(),
            search_index_key,
            source_generation: port.source_generation.clone(),
            port,
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let document_composition = self.document_composition;
        let artifact = tokio::task::spawn_blocking(move || {
            LoadedSemanticArtifactV1::from_lifecycle(
                &lifecycle,
                &manifest,
                resources,
                document_composition,
            )
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        if artifact.projection() != &projection {
            return Ok(None);
        }
        let handle = self.handle.clone();
        let prepared_handle = handle.clone();
        let prepared =
            tokio::task::spawn_blocking(move || prepared_handle.prepare_restore(pointer, artifact))
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        Ok(Some(PreparedProductionSemanticCacheCommitV1 {
            handle,
            prepared: PreparedProductionSemanticCacheActionV1::Restore {
                prepared: Box::new(prepared),
                cache: Arc::clone(&self.vector_read_cache),
                vectors,
                lifecycle: Arc::clone(&self.lifecycle),
            },
        }))
    }

    pub fn prepare_current_cache_observation(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
    ) -> Option<PreparedProductionSemanticCacheCommitV1> {
        let pointer = SemanticGenerationPointerV1 {
            generation: pins.vector_generation_id.clone(),
            source_generation: source_generation.clone(),
            projection_key: pins.projection.projection_key().clone(),
        };
        let prepared = self.handle.prepare_current_observation(&pointer)?;
        Some(PreparedProductionSemanticCacheCommitV1 {
            handle: self.handle.clone(),
            prepared: PreparedProductionSemanticCacheActionV1::Observation {
                prepared,
                lifecycle: Arc::clone(&self.lifecycle),
            },
        })
    }

    /// Evict one exact process-local generation while retaining every durable
    /// graph snapshot and staging record.
    pub fn unbind_cache_if_current(&self, generation: &VectorGenerationIdV1) -> bool {
        let runtime_unbound = self.handle.unbind_query_runtime_if_current(generation);
        let vectors_unbound = self
            .vector_read_cache
            .lock()
            .map(|mut cached| {
                if cached
                    .as_ref()
                    .is_some_and(|cached| cached.generation == *generation)
                {
                    *cached = None;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        runtime_unbound || vectors_unbound
    }

    /// Enqueue one saved code generation. Model verification, ORT startup,
    /// changed-chunk embedding, and database publication remain background work.
    pub fn schedule_saved_generation(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> bool {
        self.schedule_saved_generation_inner(generation, None)
    }

    /// Build an evaluator-only exact-flat lane from the checked-in sanitized
    /// corpus. The verified production artifact/runtime are reused, while the
    /// resulting vectors remain process-local and cannot alter the project's
    /// committed semantic activation.
    pub fn prepare_evaluation_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<PreparedSemanticEvaluationGenerationV1, SemanticRuntimeScheduleFailureV1> {
        self.prepare_evaluation_generation_with_cache(
            generation,
            None,
            Arc::new(SemanticEvaluationProjectionBatchCacheV1::new()),
            cancellation,
        )
    }

    /// Mint pre-evaluation resource identity from the installed artifact and
    /// the configured execution ceiling. Artifact member lengths are observed
    /// facts; the remaining fields are admission bounds that the genuine
    /// evaluator later replaces with measured report evidence.
    pub fn evaluation_target_resource_requirement(
        &self,
    ) -> Result<
        crate::config::retrieval::SemanticResourceRequirementV1,
        SemanticRuntimeBackendErrorV1,
    > {
        let artifact = installed_artifact_member_bytes(&self.lifecycle)
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        Ok(evaluation_target_resource_requirement(
            self.resources,
            artifact,
        ))
    }

    /// Prepare one evaluator generation with authorities retained by the
    /// daemon's enclosing evaluation request. The optional query factory owns
    /// the one shared model runtime; the batch cache remains detached from the
    /// lifecycle and durable vector state.
    pub fn prepare_evaluation_generation_with_cache(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        query_factory: Option<&SemanticEvaluationQueryFactoryV1>,
        projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<PreparedSemanticEvaluationGenerationV1, SemanticRuntimeScheduleFailureV1> {
        let artifact_bytes = installed_artifact_member_bytes(&self.lifecycle)?;
        let execution = self.resources;
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let artifact_digest = artifact
            .projection()
            .embedding_key()
            .model_artifact_digest
            .clone();
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, None)?;
        let projection_input_bytes = projection_input_bytes(generation.chunks().chunks())?;
        let started = std::time::Instant::now();
        let prepared = hotpath::measure_block!("search_eval.projection.case.clean.prepare", {
            prepare_semantic_evaluation_projection(
                artifact,
                query_factory,
                request,
                generation.chunks().chunks(),
                embedding_documents(generation),
                evaluation_projection_resources(execution),
                projection_batch_cache.as_ref(),
                SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
                Arc::clone(&cancellation),
            )
        })?;
        PreparedSemanticEvaluationGenerationV1::new(
            generation.clone(),
            prepared,
            artifact_digest,
            artifact_bytes,
            execution,
            elapsed_micros(started),
            projection_input_bytes,
            projection_batch_cache,
            cancellation,
        )
    }

    /// Measure a genuine incremental evaluator projection from an already
    /// prepared immutable generation. This never publishes a durable pointer
    /// or relabels a clean rebuild.
    pub fn measure_incremental_evaluation_projection(
        &self,
        current: &PreparedSemanticEvaluationGenerationV1,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<ProductionCandidateNativeGenerationResourcesV1, SemanticRuntimeScheduleFailureV1>
    {
        let request = semantic_projection_request(
            generation,
            &current.projection,
            Some(&SemanticGenerationPointerV1 {
                generation: current.vector_generation.clone(),
                source_generation: current.source_generation.clone(),
                projection_key: current.projection.projection_key().clone(),
            }),
        )?;
        if request.changes.from_generation.as_ref() != Some(&current.source_generation)
            || request.changes.added_or_changed.is_empty()
            || request.changes.added_or_changed.len() >= generation.chunks().chunks().len()
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let started = std::time::Instant::now();
        let prepared = hotpath::measure_block!("search_eval.projection.incremental.prepare", {
            prepare_semantic_evaluation_projection(
                artifact,
                Some(&current.query_factory),
                request,
                &chunks,
                embedding_documents(generation),
                evaluation_projection_resources(self.resources),
                current.projection_batch_cache.as_ref(),
                SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
                Arc::clone(&current.cancellation),
            )
        })?;
        if prepared.prepared.request.changes.from_generation.as_ref()
            != Some(&current.source_generation)
            || prepared.prepared.request.changes.to_generation
                != generation.manifest().generation_id
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let mut resources = current.generation_resources();
        resources.incremental_source_generation = generation.manifest().generation_id.clone();
        resources.incremental_source_manifest_digest = generation
            .projection()
            .request()
            .changes
            .manifest_digest
            .clone();
        resources.incremental_rebuild_micros = elapsed_micros(started);
        Ok(resources)
    }

    pub fn measure_evaluation_projection_cases(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        block_on_semantic_evaluation(
            self.measure_evaluation_projection_cases_isolated(clean, sources),
        )
    }

    async fn measure_evaluation_projection_cases_isolated(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SemanticEvaluationGraphCancellationV1 {
                evaluation: Arc::clone(&clean.cancellation),
            });
        let graph = hotpath::measure_block!("search_eval.projection.case_graph.materialize", {
            isolated_semantic_evaluation_graph(
                &[
                    &clean.code,
                    sources.one_symbol,
                    sources.no_op,
                    sources.deletion,
                ],
                graph_cancellation,
            )
        })
        .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        self.measure_evaluation_projection_cases_in_store(&graph, clean, sources)
            .await
    }

    async fn measure_evaluation_projection_cases_in_store(
        &self,
        graph: &Arc<IsolatedSemanticEvaluationGraphV1>,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let clean_prepared = &clean.prepared_projection;
        if clean_prepared.request.changes.to_generation != clean.source_generation
            || clean_prepared.request.changes.from_generation.is_some()
        {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation projection is not a root generation",
            ));
        }
        let clean_plan = evaluation_projection_plan_from_canonical_chunks(
            clean.code.chunks().chunks(),
            &clean_prepared.request,
            None,
        );
        let clean_retained = graph
            .retained(&clean.source_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let cancellation = Arc::clone(clean_retained.cancellation());
        let store = evaluation_projection_case_store(&clean_retained, clean_prepared)?;
        let clean_build = hotpath::future!(
            store.rebuild_generation(clean_plan.clone(), Arc::clone(&cancellation)),
            label = "search_eval.projection.case.clean.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();

        // Replay workload: the first writer begins the durable
        // stage and dies before committing; a fresh store partition recovers
        // that stage and drives the identical prepared batch through commit
        // and publication. This measures the real replay path — durable
        // stage recovery, byte-exact batch convergence, prepare, publish,
        // settle — with zero model calls, instead of a zero-work
        // already-published lookup.
        let replay_store = evaluation_projection_case_store(&clean_retained, clean_prepared)?;
        let replay_build = match replay_store
            .begin_generation(clean_plan.clone(), Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id }
                if build_id == clean_build =>
            {
                build_id
            }
            VectorGenerationBeginOutcomeV1::ReplayFromStart { .. }
            | VectorGenerationBeginOutcomeV1::AlreadyPublished { .. } => {
                return Err(SemanticRuntimeScheduleFailureV1::projection(
                    "clean evaluation replay did not recover the started build",
                ));
            }
        };
        hotpath::future!(
            commit_evaluation_prepared_generation(
                &replay_store,
                &replay_build,
                clean_prepared,
                clean.code.chunks().chunks(),
                Arc::clone(&cancellation),
            ),
            label = "search_eval.projection.case.clean.graph_commit"
        )
        .await?;
        let clean_publication = hotpath::future!(
            replay_store.publish_generation(&replay_build, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.clean.graph_publish"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if !replay_store
            .published_generation_is_visible(
                &clean_publication.generation_id,
                Arc::clone(&cancellation),
            )
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation publication is not visible after publish",
            ));
        }
        // Durable idempotency: a third partition observes the published
        // generation without re-doing any work.
        let idempotent_store = evaluation_projection_case_store(&clean_retained, clean_prepared)?;
        let idempotent_started = std::time::Instant::now();
        let idempotent = hotpath::future!(
            idempotent_store.begin_generation(clean_plan, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.idempotency.graph_observe"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if !matches!(
            idempotent,
            VectorGenerationBeginOutcomeV1::AlreadyPublished {
                publication,
                ..
            } if publication == clean_publication
        ) {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation idempotent begin did not observe the published generation",
            ));
        }
        let idempotent_elapsed = elapsed_micros(idempotent_started);

        let mut samples = BTreeMap::new();
        samples.insert(
            SemanticProjectionCaseV1::Clean,
            projection_case_sample_from_prepared(
                clean_prepared,
                clean.resources.clean_projection_build_micros,
                clean.projection_input_bytes,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );
        samples.insert(
            SemanticProjectionCaseV1::IdempotencyReplay,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::Complete,
                elapsed_micros: idempotent_elapsed,
                input_bytes: 0,
                chunks_added_or_changed: 0,
                chunks_deleted: 0,
                chunks_reused: 0,
                projection_calls: 0,
            },
        );

        let clean_pointer = SemanticGenerationPointerV1 {
            generation: clean_publication.generation_id.clone(),
            source_generation: clean_prepared.request.changes.to_generation.clone(),
            projection_key: clean_prepared.request.target_projection_key.clone(),
        };
        let (one_symbol, one_symbol_elapsed, one_symbol_input) = hotpath::measure_block!(
            "search_eval.projection.case.one_symbol.prepare",
            self.prepare_projection_case(
                sources.one_symbol,
                Some(&clean_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let one_symbol_retained = graph
            .retained(&one_symbol.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let one_symbol_store = evaluation_projection_case_store(&one_symbol_retained, &one_symbol)?;
        let one_symbol_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &one_symbol_store,
                &cancellation,
                sources.one_symbol,
                &one_symbol,
                Some(clean_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.one_symbol.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::OneSymbol,
            projection_case_sample_from_prepared(
                &one_symbol,
                one_symbol_elapsed,
                one_symbol_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let one_symbol_pointer = SemanticGenerationPointerV1 {
            generation: one_symbol_publication.generation_id.clone(),
            source_generation: one_symbol.request.changes.to_generation.clone(),
            projection_key: one_symbol.request.target_projection_key.clone(),
        };
        let (no_op, no_op_elapsed, no_op_input) = hotpath::measure_block!(
            "search_eval.projection.case.no_op.prepare",
            self.prepare_projection_case(
                sources.no_op,
                Some(&one_symbol_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let no_op_retained = graph
            .retained(&no_op.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let no_op_store = evaluation_projection_case_store(&no_op_retained, &no_op)?;
        let no_op_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &no_op_store,
                &cancellation,
                sources.no_op,
                &no_op,
                Some(one_symbol_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.no_op.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::NoOp,
            projection_case_sample_from_prepared(
                &no_op,
                no_op_elapsed,
                no_op_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let no_op_pointer = SemanticGenerationPointerV1 {
            generation: no_op_publication.generation_id.clone(),
            source_generation: no_op.request.changes.to_generation.clone(),
            projection_key: no_op.request.target_projection_key.clone(),
        };
        let (deletion, deletion_elapsed, deletion_input) = hotpath::measure_block!(
            "search_eval.projection.case.deletion.prepare",
            self.prepare_projection_case(
                sources.deletion,
                Some(&no_op_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let deletion_retained = graph
            .retained(&deletion.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let deletion_store = evaluation_projection_case_store(&deletion_retained, &deletion)?;
        let _deletion_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &deletion_store,
                &cancellation,
                sources.deletion,
                &deletion,
                Some(no_op_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.deletion.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::Deletion,
            projection_case_sample_from_prepared(
                &deletion,
                deletion_elapsed,
                deletion_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let cancellation_artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            sources.deletion.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let cancellation_projection = cancellation_artifact.projection().clone();
        let cancellation_request =
            semantic_projection_request(sources.deletion, &cancellation_projection, None)?;
        let cancellation_changed = cancellation_request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let cancellation_chunks = sources
            .deletion
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| cancellation_changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        if cancellation_chunks.len() != cancellation_request.changes.added_or_changed.len() {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_input = projection_input_bytes(&cancellation_chunks)?;
        let cancellation_store = evaluation_projection_case_store_for_changes(
            &deletion_retained,
            cancellation_projection,
            &cancellation_request.changes,
        )?;
        let cancellation_started = std::time::Instant::now();
        let graph_authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        let cancellation_revision_before = cancellation_store
            .verified_revision(Arc::clone(&cancellation))
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_head_before = deletion_retained
            .runtime()
            .verified_head(&graph_authority)
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_plan =
            evaluation_projection_plan_from_request(sources.deletion, &cancellation_request, None)?;
        let cancellation_generation = VectorGenerationIdV1::new(
            generation_identity_digest(&cancellation_plan)
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
        let cancellation_build = hotpath::future!(
            cancellation_store.begin_generation(cancellation_plan, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.cancellation.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let VectorGenerationBeginOutcomeV1::ReplayFromStart {
            build_id: cancellation_build,
        } = cancellation_build
        else {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        };
        if cancellation_store
            .published_generation_is_visible(&cancellation_generation, Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_measurement = hotpath::measure_block!(
            "search_eval.projection.case.cancellation.prepare",
            measure_semantic_evaluation_projection_cancellation(
                cancellation_artifact,
                &clean.query_factory,
                cancellation_request.clone(),
                &cancellation_chunks,
                embedding_documents(sources.deletion),
                clean.projection_batch_cache.as_ref(),
                Arc::clone(&clean.cancellation),
            )
        );
        if !hotpath::future!(
            cancellation_store.cancel_generation(&cancellation_build, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.cancellation.graph_cancel"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_after_store = GraphVectorGenerationStoreV1::open(&deletion_retained)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        if cancellation_after_store
            .published_generation_is_visible(&cancellation_generation, Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_revision_after = cancellation_after_store
            .verified_revision(Arc::clone(&cancellation))
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_head_after = deletion_retained
            .runtime()
            .verified_head(&graph_authority)
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if cancellation_revision_after != cancellation_revision_before
            || cancellation_head_after != cancellation_head_before
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_measurement = cancellation_measurement?;
        if cancellation_measurement.projection_calls == 0
            || cancellation_measurement.projection_calls
                >= cancellation_measurement.chunks_added_or_changed
            || cancellation_measurement.chunks_added_or_changed
                != cancellation_request.changes.added_or_changed.len() as u64
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::Cancellation,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::CancelledWithoutPublication,
                elapsed_micros: elapsed_micros(cancellation_started),
                input_bytes: cancellation_input,
                chunks_added_or_changed: cancellation_measurement.chunks_added_or_changed,
                chunks_deleted: cancellation_request.changes.deleted.len() as u64,
                chunks_reused: cancellation_request.changes.reused.len() as u64,
                projection_calls: cancellation_measurement.projection_calls,
            },
        );

        let (incompatible, incompatible_elapsed, incompatible_input) = hotpath::measure_block!(
            "search_eval.projection.case.incompatible.prepare",
            self.prepare_projection_case(
                sources.one_symbol,
                None,
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        if incompatible.request.changes.from_generation.is_some()
            || incompatible.request.previous_projection_key.is_some()
            || incompatible.request.replay_reason
                != ProjectionReplayReasonV1::FullRebuildIncompatible
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let incompatible_plan =
            evaluation_projection_plan(sources.one_symbol, &incompatible, None)?;
        let incompatible_store =
            evaluation_projection_case_store(&one_symbol_retained, &incompatible)?;
        let incompatible_build = hotpath::future!(
            incompatible_store.rebuild_generation(incompatible_plan, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.incompatible.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();
        hotpath::future!(
            commit_evaluation_prepared_generation(
                &incompatible_store,
                &incompatible_build,
                &incompatible,
                sources.one_symbol.chunks().chunks(),
                Arc::clone(&cancellation),
            ),
            label = "search_eval.projection.case.incompatible.graph_commit"
        )
        .await?;
        if !hotpath::future!(
            incompatible_store.cancel_generation(&incompatible_build, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.incompatible.graph_cancel"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::IncompatibleState,
            projection_case_sample_from_prepared(
                &incompatible,
                incompatible_elapsed,
                incompatible_input,
                SemanticProjectionCaseOutcomeV1::FullRebuildIncompatible,
            ),
        );
        let required = BTreeSet::from([
            SemanticProjectionCaseV1::Clean,
            SemanticProjectionCaseV1::OneSymbol,
            SemanticProjectionCaseV1::Deletion,
            SemanticProjectionCaseV1::NoOp,
            SemanticProjectionCaseV1::IdempotencyReplay,
            SemanticProjectionCaseV1::Cancellation,
            SemanticProjectionCaseV1::IncompatibleState,
        ]);
        if samples.keys().copied().collect::<BTreeSet<_>>() != required {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        Ok(samples)
    }

    fn prepare_projection_case(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        current: Option<&SemanticGenerationPointerV1>,
        query_factory: &SemanticEvaluationQueryFactoryV1,
        projection_batch_cache: &Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: &Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<(PreparedVectorGenerationV1, u64, u64), SemanticRuntimeScheduleFailureV1> {
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, current)?;
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        if chunks.len() != request.changes.added_or_changed.len() {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let input_bytes = projection_input_bytes(&chunks)?;
        let started = std::time::Instant::now();
        let prepared = prepare_semantic_evaluation_projection(
            artifact,
            Some(query_factory),
            request,
            &chunks,
            embedding_documents(generation),
            evaluation_projection_resources(self.resources),
            projection_batch_cache.as_ref(),
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
            Arc::clone(cancellation),
        )?;
        Ok((prepared.prepared, elapsed_micros(started), input_bytes))
    }

    pub async fn inspect_compatible_current_generation_snapshot(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticCompatibleCurrentGenerationSnapshotV1, SemanticRuntimeBackendErrorV1> {
        let retained = hotpath::future!(
            self.graph.graph_for_current(),
            label = "semantic.evaluation.snapshot.current_graph"
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "current_graph",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = hotpath::measure_block!(
            "semantic.evaluation.snapshot.vector_store",
            GraphVectorGenerationStoreV1::read_only_generation(
                &retained,
                &required.vector_generation_id,
            )
        )
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_store",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?
        .ok_or_else(|| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_store",
                outcome = "missing",
            );
            SemanticRuntimeBackendErrorV1::Rejected
        })?;
        let verified = hotpath::future!(
            store.generation_snapshot_for(
                &required.vector_generation_id,
                &required.projection,
                source_generation,
                source_manifest_digest,
                Arc::clone(&cancellation),
            ),
            label = "semantic.evaluation.snapshot.vector_generation"
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_snapshot",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?;
        let verified = verified.ok_or_else(|| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_snapshot",
                outcome = "missing",
            );
            SemanticRuntimeBackendErrorV1::Rejected
        })?;
        let executable_lease = hotpath::future!(
            self.inspect_generation(required),
            label = "semantic.evaluation.snapshot.executable_generation"
        )
        .await
        .inspect_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "executable_generation",
                outcome = match error {
                    SemanticRuntimeBackendErrorV1::Unavailable => "unavailable",
                    SemanticRuntimeBackendErrorV1::Rejected => "rejected",
                    SemanticRuntimeBackendErrorV1::Conflict => "conflict",
                },
            );
        })?;
        // Publication identity stays i64 on the wire; the graph adapter's
        // monotonic u64 revision maps 1:1 into it and can only overflow after
        // ~9.2e18 mutations, which we treat as a rejected protocol state.
        let vector_state_revision = i64::try_from(verified.revision())
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
        Ok(SemanticCompatibleCurrentGenerationSnapshotV1 {
            executable: executable_lease.evidence().clone(),
            vector_state_revision,
            vector_generation_id: verified.generation().generation_id().clone(),
        })
    }

    /// Certify a proposed semantic evaluation target without reading accepted
    /// profile or configuration state.
    ///
    /// The candidate must bind the canonical exact-flat index, the exact
    /// current vector/source generation, installed artifact members, runtime
    /// implementation, and configured resource ceiling before a native
    /// evaluator is allowed to run.
    pub async fn inspect_verified_evaluation_target_snapshot(
        &self,
        candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        capability_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<SemanticVerifiedEvaluationTargetSnapshotV1, SemanticRuntimeBackendErrorV1> {
        check_evaluation_cancellation(cancellation.as_ref())?;
        let measured_maximum_distance_micros = hotpath::future!(
            self.measured_acceptance_distance_micros(candidate),
            label = "semantic.evaluation.snapshot.acceptance_distance"
        )
        .await;
        let certified = hotpath::measure_block!(
            "semantic.evaluation.snapshot.certify_compatibility",
            certify_evaluation_target_compatibility(
                candidate,
                source_generation,
                source_manifest_digest,
                capability_manifest_digest,
                measured_maximum_distance_micros,
            )
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "certify_compatibility",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        hotpath::measure_block!(
            "semantic.evaluation.snapshot.validate_search_index",
            validate_evaluation_target_search_index(&certified.search_index_key)
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "validate_search_index",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        let verified = hotpath::future!(
            self.inspect_compatible_current_generation_snapshot(
                &certified,
                source_generation,
                source_manifest_digest,
            ),
            label = "semantic.evaluation.snapshot.verify_generation"
        )
        .await?;
        check_evaluation_cancellation(cancellation.as_ref())?;
        if verified.vector_generation_id != certified.vector_generation_id {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        let lifecycle_verification = hotpath::measure_block!(
            "semantic.evaluation.snapshot.verify_lifecycle",
            self.evaluation_lifecycle_verification(
                certified,
                source_generation.clone(),
                source_manifest_digest.clone(),
                capability_manifest_digest.clone(),
                verified.vector_state_revision,
            )
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "verify_lifecycle",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        Ok(SemanticVerifiedEvaluationTargetSnapshotV1 {
            semantic_compatibility: lifecycle_verification.compatibility.clone(),
            vector_state_revision: verified.vector_state_revision,
            vector_generation_id: verified.vector_generation_id,
            configured_resource_ceiling: configured_semantic_resource_ceiling(self.resources),
            lifecycle_verification,
        })
    }

    /// Recheck the opaque pre-acceptance lifecycle observation immediately
    /// before publication. A changed vector/code/lifecycle target is a CAS
    /// conflict, while a malformed or foreign lease remains rejected.
    #[hotpath::measure(label = "usecases.semantic.revalidate_target", future = true)]
    pub async fn revalidate_verified_evaluation_target(
        &self,
        verification: &SemanticEvaluationLifecycleVerificationV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<(), SemanticRuntimeBackendErrorV1> {
        check_evaluation_cancellation(cancellation.as_ref())?;
        let measured_maximum_distance_micros = self
            .measured_acceptance_distance_micros(&verification.compatibility)
            .await;
        let certified = certify_evaluation_target_compatibility(
            &verification.compatibility,
            &verification.source_generation,
            &verification.source_manifest_digest,
            &verification.capability_manifest_digest,
            measured_maximum_distance_micros,
        )
        .map_err(revalidation_error)?;
        if verification.compatibility != certified {
            return Err(SemanticRuntimeBackendErrorV1::Conflict);
        }
        let verified = self
            .inspect_compatible_current_generation_snapshot(
                &verification.compatibility,
                &verification.source_generation,
                &verification.source_manifest_digest,
            )
            .await
            .map_err(revalidation_error)?;
        check_evaluation_cancellation(cancellation.as_ref())?;
        if verified.vector_state_revision != verification.vector_state_revision
            || verified.vector_generation_id != verification.compatibility.vector_generation_id
        {
            return Err(SemanticRuntimeBackendErrorV1::Conflict);
        }
        let current = self
            .evaluation_lifecycle_verification(
                verification.compatibility.clone(),
                verification.source_generation.clone(),
                verification.source_manifest_digest.clone(),
                verification.capability_manifest_digest.clone(),
                verification.vector_state_revision,
            )
            .map_err(revalidation_error)?;
        revalidate_lifecycle_verification(verification, &current)
    }

    /// Acquire the canonical lifecycle read lease after all target checks
    /// succeed. Daemon publication holds this as its final lease through
    /// commit, so lifecycle writers cannot change the evaluated model between
    /// validation and durable profile publication.
    #[hotpath::measure(label = "usecases.semantic.evaluation_lease", future = true)]
    pub async fn acquire_verified_evaluation_target_publication_lease(
        &self,
        verification: &SemanticEvaluationLifecycleVerificationV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<SemanticEvaluationPublicationLeaseV1, SemanticRuntimeBackendErrorV1> {
        self.revalidate_verified_evaluation_target(verification, Arc::clone(&cancellation))
            .await?;
        let lifecycle = self
            .lifecycle
            .acquire_verified_evaluation_publication_lease(
                &verification.lifecycle_identity,
                cancellation,
            )
            .await
            .map_err(lifecycle_publication_error)?;
        Ok(SemanticEvaluationPublicationLeaseV1 {
            _lifecycle: lifecycle,
        })
    }

    fn evaluation_lifecycle_verification(
        &self,
        compatibility: crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: CodeGenerationId,
        source_manifest_digest: ManifestDigest,
        capability_manifest_digest: ManifestDigest,
        vector_state_revision: i64,
    ) -> Result<SemanticEvaluationLifecycleVerificationV1, SemanticRuntimeBackendErrorV1> {
        let lifecycle_identity = self
            .lifecycle
            .verified_evaluation_publication_identity()
            .map_err(lifecycle_publication_error)?;
        let lifecycle_state = lifecycle_identity.state();
        if !matches!(
            lifecycle_state,
            SemanticModelLifecycleStateV1::Installed { .. }
                | SemanticModelLifecycleStateV1::Loading { .. }
                | SemanticModelLifecycleStateV1::Indexing { .. }
                | SemanticModelLifecycleStateV1::Ready { .. }
        ) || !lifecycle_artifact_matches(
            lifecycle_state,
            &compatibility.artifact_manifest_digest,
        ) {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        Ok(SemanticEvaluationLifecycleVerificationV1 {
            compatibility,
            source_generation,
            source_manifest_digest,
            capability_manifest_digest,
            vector_state_revision,
            lifecycle_identity,
        })
    }

    /// Inspect only immutable vector/source identity before native evaluation.
    /// Resource evidence does not exist yet and is therefore not fabricated
    /// from the evaluator's configured ceilings.
    #[hotpath::measure(label = "usecases.semantic.inspect_eval_snapshot", future = true)]
    pub async fn inspect_evaluation_current_generation_snapshot(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticEvaluationCurrentGenerationSnapshotV1, SemanticRuntimeBackendErrorV1> {
        let retained = self
            .graph
            .graph_for_current()
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            &required.vector_generation_id,
        )
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
        .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        let verified = store
            .generation_snapshot_for(
                &required.vector_generation_id,
                &required.projection,
                source_generation,
                source_manifest_digest,
                cancellation,
            )
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        Ok(SemanticEvaluationCurrentGenerationSnapshotV1 {
            vector_state_revision: i64::try_from(verified.revision())
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?,
            vector_generation_id: verified.generation().generation_id().clone(),
            source_manifest_digest: verified.generation().source_manifest_digest().clone(),
        })
    }

    /// Freeze vector-pointer mutation while a freshness-bound accepted profile
    /// publication commits. Every vector mutation enters this same writer
    /// lane, so a validated revision/generation remains exact for the lease.
    #[hotpath::measure(label = "usecases.semantic.vector_lease", future = true)]
    pub async fn acquire_vector_publication_lease(
        &self,
        expected_revision: i64,
        expected_generation: &VectorGenerationIdV1,
    ) -> Result<SemanticVectorPublicationLeaseV1, SemanticRuntimeBackendErrorV1> {
        let writer = Arc::clone(&self.vector_writer).lock_owned().await;
        let expected_revision = u64::try_from(expected_revision)
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
        let retained = self
            .graph
            .graph_for_current()
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        let store =
            GraphVectorGenerationStoreV1::read_only_generation(&retained, expected_generation)
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
                .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        if store
            .verified_revision(Arc::clone(retained.cancellation()))
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            != expected_revision
        {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        Ok(SemanticVectorPublicationLeaseV1 { _writer: writer })
    }

    /// Freeze every vector-mutation path without validating a revision.
    ///
    /// Code-generation retention holds this while it pins the vector
    /// inventory and deletes superseded sealed generations, so no vector
    /// publication can begin referencing a generation mid-sweep.
    pub async fn freeze_vector_mutations(&self) -> SemanticVectorPublicationLeaseV1 {
        SemanticVectorPublicationLeaseV1 {
            _writer: Arc::clone(&self.vector_writer).lock_owned().await,
        }
    }

    /// Read the immutable generation selected by committed compatibility pins.
    ///
    /// The installed runtime pointer is a cache observation only and cannot
    /// substitute another graph generation.
    #[hotpath::measure(label = "usecases.semantic.active_generation", future = true)]
    pub async fn active_vector_generation(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    ) -> Option<PublishedVectorGenerationV1> {
        let generation_id = &pins.vector_generation_id;
        let retained = self.graph.graph_for_current().await.ok()?;
        let store =
            GraphVectorGenerationStoreV1::read_only_generation(&retained, generation_id).ok()??;
        store
            .generation(generation_id, Arc::clone(retained.cancellation()))
            .await
            .ok()
            .flatten()
    }

    /// Measure the semantic acceptance bound of the generation named by
    /// `pins`.
    ///
    /// A generation that cannot be read, or that carries too few usable
    /// vectors to support a distribution, measures nothing and falls back to
    /// admitting every candidate. Abstention is the job of a measured bound,
    /// never of a missing one.
    async fn measured_acceptance_distance_micros(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    ) -> i64 {
        match self.active_vector_generation(pins).await {
            Some(generation) => {
                measure_acceptance_calibration(generation.vectors()).maximum_distance_micros
            }
            None => UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        }
    }

    pub fn cache_ready_for(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
    ) -> bool {
        let model_ready = self
            .handle
            .query_factory(
                source_generation,
                &pins.vector_generation_id,
                pins.projection.projection_key(),
            )
            .is_some();
        let vectors_ready = retained_vector_read_port(
            &self.vector_read_cache,
            &pins.vector_generation_id,
            pins.projection.projection_key(),
            &pins.search_index_key,
            source_generation,
            &pins.calibration.capability_manifest_digest,
        )
        .is_some();
        model_ready && vectors_ready
    }

    fn schedule_saved_generation_fair(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        lease: SemanticProjectionLeaseV1,
    ) -> bool {
        self.schedule_saved_generation_inner(generation, Some(lease))
    }

    #[hotpath::measure(label = "usecases.semantic.schedule_inner")]
    fn schedule_saved_generation_inner(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        fair_lease: Option<SemanticProjectionLeaseV1>,
    ) -> bool {
        let projection = match LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        ) {
            Ok(projection) => {
                crate::hotpath_observe::semantic_candidate_chunks(
                    generation.chunks().chunks().len(),
                );
                projection
            }
            Err(_) => {
                return schedule_saved_code_generation(
                    &self.handle,
                    &generation,
                    || Err(SemanticRuntimeScheduleFailureV1::Artifact),
                    move || async move {
                        drop(fair_lease);
                        Err(SemanticRuntimeScheduleFailureV1::Publication)
                    },
                );
            }
        };
        // A full projection of this corpus under this projection key may have
        // already been proven to fail terminally at publish time. Rescheduling
        // it re-embeds the whole corpus inside the shared reservation before
        // failing identically, so the memo suppresses it under backoff. This is
        // a scheduling guard only: the memo clears on anything that could
        // change the outcome (key, corpus-size class, witness, or a success).
        let failure_key = super::SemanticPublishFailureKeyV1::new(
            projection.projection_key().clone(),
            generation.chunks().chunks().len(),
        );
        let failure_witness =
            super::publish_failure_witness(&self.code_index_store_root, &self.resources);
        if let super::SemanticPublishAdmissionV1::Suppressed(suppressed) =
            super::semantic_publish_failure_memo().admit(&failure_key, &failure_witness)
        {
            tracing::warn!(
                event = "semantic_projection_schedule",
                outcome = "suppressed",
                stored_failure = %suppressed.reason,
                failures = suppressed.failures,
                retry_after_ms = u64::try_from(suppressed.retry_after.as_millis())
                    .unwrap_or(u64::MAX),
                corpus_size_class = failure_key.corpus_size_class,
                projection_kind = ?failure_key.projection_key.kind,
                "semantic publication previously failed for this projection key and \
                 corpus-size class; suppressing the full re-projection until backoff elapses"
            );
            drop(fair_lease);
            return false;
        }
        // Projection publication is independent of the process cache. Without
        // an immutable prior-generation catalog input this is truthfully a
        // full rebuild; `handle.current()` is never a delta/base authority.
        let request = match semantic_projection_request(&generation, &projection, None) {
            Ok(request) => request,
            Err(_) => return false,
        };
        let changed_ids = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<std::collections::BTreeSet<_>>();
        let canonical_chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed_ids.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let target_generation = generation.manifest().generation_id.clone();
        let expected_chunk_ids = generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        let base_generation = None;
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let document_composition = self.document_composition;
        let documents = embedding_documents(&generation);
        let total_units = request.changes.added_or_changed.len().max(1) as u64;
        // The plan is decided from the whole request before any batch runs, so
        // splitting the run never moves the generation identity: the plan's
        // source watermark and expected membership are the corpus's, not any
        // one batch's.
        let published_source_generation = request.changes.to_generation.clone();
        let published_projection_key = request.target_projection_key.clone();
        let stage_descriptor = match SemanticVectorStageDescriptorV1::from_changes(
            projection.clone(),
            &request.changes,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                crate::hotpath_observe::semantic_stage_descriptor_failed();
                tracing::warn!(
                    event = "semantic_projection_schedule",
                    outcome = "stage_descriptor_failed",
                    expected_chunks = expected_chunk_ids.len(),
                    error = %error,
                    "semantic projection stage descriptor could not be prepared"
                );
                return false;
            }
        };
        let plan = VectorGenerationPlanV1 {
            target_projection_key: published_projection_key.clone(),
            source_generation: published_source_generation.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            expected_chunk_ids: expected_chunk_ids.into(),
            base_generation: base_generation.clone(),
        };
        let search_index_key = match SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
        {
            Ok(search_index_key) => search_index_key,
            Err(_) => return false,
        };
        // Every stage of one scheduled projection — load, resume, per-batch
        // commit, stage, publish — reaches the same five handles. Bundling them
        // once means each closure clones a single `Arc` instead of restating the
        // same five clones under a stage-specific prefix.
        let handles = Arc::new(ScheduledProjectionHandlesV1 {
            graph: Arc::clone(&self.graph),
            writer: Arc::clone(&self.vector_writer),
            generation,
            commit_state: Arc::new(tokio::sync::Mutex::new(BatchCommitStateV1::default())),
            lifecycle: Arc::clone(&self.lifecycle),
            vector_read_cache: Arc::clone(&self.vector_read_cache),
        });
        let publication_failure = SemanticPublicationFailureRecorderV1::default();
        let resume_failure = publication_failure.clone();
        let commit_failure = publication_failure.clone();
        let publish_failure = publication_failure.clone();
        let fair_lease = fair_lease.map(Arc::new);
        let load_handles = Arc::clone(&handles);
        let resume_handles = Arc::clone(&handles);
        let commit_handles = Arc::clone(&handles);
        let stage_handles = handles;
        let commit_lease = fair_lease.clone();
        let _ = self.lifecycle.mark_loading();
        let _ = self.lifecycle.mark_indexing(0, total_units);
        let request = match FastEmbedSemanticGenerationRequestV1::new(
            target_generation,
            request,
            canonical_chunks,
            documents,
            SEMANTIC_EMBEDS_PER_COMMIT,
            move || {
                LoadedSemanticArtifactV1::from_lifecycle(
                    &load_handles.lifecycle,
                    &manifest,
                    resources,
                    document_composition,
                )
            },
            move || async move {
                let _writer = resume_handles.writer.lock().await;
                let retained = resume_handles
                    .graph
                    .graph_for_generation(resume_handles.generation.as_ref())
                    .await
                    .map_err(|error| resume_failure.retain_for_resume(&error))?;
                let cancellation = Arc::clone(retained.cancellation());
                let store = Arc::new(
                    GraphVectorGenerationStoreV1::open(&retained)
                        .map_err(|error| resume_failure.open_store(&error))?,
                );
                store
                    .configure_stage(stage_descriptor)
                    .map_err(|error| resume_failure.configure_stage(&error))?;
                // The build identity is a digest of the plan, so reopening the
                // same plan re-adopts the same staged build rather than
                // starting a second one.
                let resume = store
                    .begin_generation(plan, Arc::clone(&cancellation))
                    .await
                    .map_err(|error| resume_failure.begin_generation(&error))?;
                let mut state = resume_handles.commit_state.lock().await;
                state.build = Some(resume.build_id().clone());
                state.store = Some(store);
                state.checkpoint = None;
                state.published = match resume {
                    VectorGenerationBeginOutcomeV1::ReplayFromStart { .. } => None,
                    VectorGenerationBeginOutcomeV1::AlreadyPublished { publication, .. } => {
                        Some(publication)
                    }
                };
                // Pending native rows are deliberately unreadable through the
                // verified snapshot. Replay bounded source batches from zero;
                // durable stage receipts and keyed native applies make each
                // replay exact after restart.
                Ok(if state.published.is_some() {
                    SemanticProjectionResumeOutcomeV1::AlreadyPublished
                } else {
                    SemanticProjectionResumeOutcomeV1::ReplayFromStart
                })
            },
            move |prepared| {
                let handles = Arc::clone(&commit_handles);
                let lease = commit_lease.clone();
                let failure = commit_failure.clone();
                async move {
                    if lease
                        .as_deref()
                        .is_some_and(SemanticProjectionLeaseV1::is_cancelled)
                    {
                        return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
                    }
                    let mut state = handles.commit_state.lock().await;
                    let build = state
                        .build
                        .clone()
                        .ok_or_else(|| failure.missing_commit_build())?;
                    let store = state
                        .store
                        .as_ref()
                        .cloned()
                        .ok_or_else(|| failure.missing_commit_store())?;
                    let _writer = handles.writer.lock().await;
                    let retained = handles
                        .graph
                        .graph_for_generation(handles.generation.as_ref())
                        .await
                        .map_err(|error| failure.retain_for_batch(&error))?;
                    let cancellation = Arc::clone(retained.cancellation());
                    let next = store
                        .commit_batch(&build, state.checkpoint.as_ref(), prepared, cancellation)
                        .await
                        .map_err(|error| failure.commit_batch(&error))?;
                    state.checkpoint = Some(next);
                    Ok(())
                }
            },
            move || async move {
                let (build, store, published) = {
                    let state = stage_handles.commit_state.lock().await;
                    (
                        state
                            .build
                            .clone()
                            .ok_or_else(|| publish_failure.missing_publish_build())?,
                        state
                            .store
                            .as_ref()
                            .cloned()
                            .ok_or_else(|| publish_failure.missing_publish_store())?,
                        state.published.clone(),
                    )
                };
                let _ = stage_handles
                    .lifecycle
                    .mark_indexing(total_units, total_units);
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    let _publication_lease = fair_lease
                        .as_deref()
                        .map(SemanticProjectionLeaseV1::try_begin_publication)
                        .transpose()
                        .map_err(fair_schedule_failure)?;
                    let _writer = stage_handles.writer.lock().await;
                    let retained = stage_handles
                        .graph
                        .graph_for_generation(stage_handles.generation.as_ref())
                        .await
                        .map_err(|error| publish_failure.retain_for_publish(&error))?;
                    let cancellation = Arc::clone(retained.cancellation());
                    let publication = match published {
                        Some(publication) => publication,
                        None => store
                            .publish_generation(&build, Arc::clone(&cancellation))
                            .await
                            .map_err(|error| publish_failure.publish_generation(&error))?,
                    };
                    let active = store
                        .generation(&publication.generation_id, Arc::clone(&cancellation))
                        .await
                        .map_err(|error| publish_failure.publish_generation(&error))?
                        .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
                    let ann = semantic_ann_serving_index(
                        &store,
                        &active,
                        &search_index_key,
                        Arc::clone(&cancellation),
                    )
                    .map_err(|error| publish_failure.publish_generation(&error))?;
                    let port = Arc::new(
                        PublishedSemanticVectorReadPortV1::new(
                            active,
                            search_index_key.clone(),
                            stage_handles.generation.as_ref(),
                            ann,
                        )
                        .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
                    );
                    let pointer = SemanticGenerationPointerV1 {
                        generation: port.generation.clone(),
                        source_generation: published_source_generation,
                        projection_key: published_projection_key,
                    };
                    let mut cached = stage_handles
                        .vector_read_cache
                        .lock()
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
                    *cached = Some(CachedPublishedVectorsV1 {
                        generation: port.generation.clone(),
                        search_index_key,
                        source_generation: port.source_generation.clone(),
                        port,
                    });
                    Ok(pointer)
                }))
            },
        ) {
            Ok(request) => request,
            Err(_) => return false,
        };
        let scheduled = self.handle.schedule_generation(request);
        if scheduled {
            let handle = self.handle.clone();
            let lifecycle = Arc::clone(&self.lifecycle);
            tokio::spawn(async move {
                loop {
                    match handle.status() {
                        SemanticRuntimeScheduleStatusV1::Indexing {
                            completed_units,
                            total_units,
                            ..
                        } => {
                            let _ = lifecycle.mark_indexing(completed_units, total_units);
                        }
                        SemanticRuntimeScheduleStatusV1::Current { .. } => {
                            super::semantic_publish_failure_memo().record_success(&failure_key);
                            let _ = lifecycle.mark_ready();
                            break;
                        }
                        SemanticRuntimeScheduleStatusV1::Failed { reason, .. } => {
                            let detail = publication_failure.receipt().map_or_else(
                                || format!("semantic runtime {reason:?}"),
                                |receipt| receipt.detail(),
                            );
                            // Publication failure is the reproducible one: it is
                            // decided by the corpus and the projection key, not
                            // by this attempt. Memoize it so the next published
                            // generation does not pay the full re-embed again.
                            if reason.is_publication() {
                                super::semantic_publish_failure_memo().record_failure(
                                    &failure_key,
                                    &failure_witness,
                                    &detail,
                                );
                            }
                            let _ = lifecycle.mark_runtime_failed(detail, true);
                            break;
                        }
                        SemanticRuntimeScheduleStatusV1::Unavailable => break,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            });
        }
        scheduled
    }

    fn cached_vector_read_port(
        &self,
        active: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code_generation: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
    ) -> Result<Arc<PublishedSemanticVectorReadPortV1>, SemanticQueryServiceError> {
        if let Some(cached) = retained_vector_read_port(
            &self.vector_read_cache,
            active.generation_id(),
            active.projection_key(),
            &search_index_key,
            active.source_generation(),
            &code_generation.capability().manifest_digest,
        ) {
            return Ok(cached);
        }
        let port = Arc::new(
            PublishedSemanticVectorReadPortV1::new(
                active,
                search_index_key.clone(),
                code_generation,
                ann,
            )
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?,
        );
        if let Ok(mut guard) = self.vector_read_cache.lock() {
            *guard = Some(CachedPublishedVectorsV1 {
                generation: port.generation.clone(),
                search_index_key,
                source_generation: port.source_generation.clone(),
                port: Arc::clone(&port),
            });
        }
        Ok(port)
    }

    /// Real application consumer for the optional semantic lane. The exact
    /// configuration-pinned generation is loaded before composition; indexing/download never
    /// enters this request path.
    #[hotpath::measure(label = "usecases.semantic.execute_search", future = true)]
    pub async fn execute_search<C>(
        &self,
        code_generation: &CodeIndexPublishedGenerationV1,
        request: &SemanticRetrievalRequestV1<'_>,
        calibration: Option<&SemanticCalibrationProfileV1>,
        control: &C,
        mode: SemanticQueryModeV1,
        fallback: Arc<QueryFallbackSubpayload>,
    ) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
    where
        C: SemanticExecutionControl + Sync,
    {
        let source_manifest_digest =
            semantic_source_manifest_digest(code_generation.projection().request());
        if request.code_generation == code_generation.manifest().generation_id
            && request.capability_manifest_digest == code_generation.capability().manifest_digest
            && let Some(vectors) = retained_vector_read_port(
                &self.vector_read_cache,
                &request.vector_generation,
                request.projection.projection_key(),
                request.search_index_key,
                &request.code_generation,
                &request.capability_manifest_digest,
            )
        {
            let complete = CompleteSemanticGenerationV1::new(
                request.projection.projection_key().clone(),
                request.search_index_key.clone(),
                request.vector_generation.clone(),
                request.code_generation.clone(),
                request.capability_manifest_digest.clone(),
            )
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
            return compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
                handle: &self.handle,
                request,
                generation: &complete,
                calibration,
                vectors: vectors.as_ref(),
                control,
                mode,
                fallback,
            });
        }
        let Ok(retained) = self.graph.graph_for_generation(code_generation).await else {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        let cancellation = Arc::clone(retained.cancellation());
        let store = match GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            &request.vector_generation,
        ) {
            Ok(Some(store)) => store,
            Ok(None) | Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                    mode,
                    fallback,
                );
            }
        };
        let active = match store
            .generation(&request.vector_generation, Arc::clone(&cancellation))
            .await
        {
            Ok(active) => active,
            Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        let Some(active) = active else {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        let replay_digest = semantic_projection_request(code_generation, request.projection, None)
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?
            .changes
            .manifest_digest;
        if active.embedding_key() != request.projection
            || active.source_generation() != &code_generation.manifest().generation_id
            || active.source_generation() != &request.code_generation
            || (active.source_manifest_digest() != source_manifest_digest
                && active.source_manifest_digest() != &replay_digest)
        {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        }
        let complete = CompleteSemanticGenerationV1::new(
            active.projection_key().clone(),
            request.search_index_key.clone(),
            active.generation_id().clone(),
            active.source_generation().clone(),
            code_generation.capability().manifest_digest.clone(),
        )
        .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        let ann = match semantic_ann_serving_index(
            &store,
            &active,
            request.search_index_key,
            Arc::clone(&cancellation),
        ) {
            Ok(ann) => ann,
            Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        let vectors = self.cached_vector_read_port(
            active,
            request.search_index_key.clone(),
            code_generation,
            ann,
        )?;
        compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &self.handle,
            request,
            generation: &complete,
            calibration,
            vectors: vectors.as_ref(),
            control,
            mode,
            fallback,
        })
    }
}

pub struct PreparedSemanticEvaluationGenerationV1 {
    code: CodeIndexPublishedGenerationV1,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    source_generation: CodeGenerationId,
    projection: tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    vector_generation: VectorGenerationIdV1,
    prepared_projection: PreparedVectorGenerationV1,
    projection_input_bytes: u64,
    projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
    capability_manifest_digest: ManifestDigest,
    query_factory: SemanticEvaluationQueryFactoryV1,
    vectors: PublishedSemanticVectorReadPortV1,
    query_keys: RetrievalCursorKeyringV1,
    resources: ProductionCandidateNativeGenerationResourcesV1,
}

impl PreparedSemanticEvaluationGenerationV1 {
    #[allow(clippy::too_many_arguments)]
    fn new(
        code: CodeIndexPublishedGenerationV1,
        prepared: PreparedSemanticEvaluationProjectionV1,
        artifact_digest: ManifestDigest,
        artifact_bytes: InstalledArtifactMemberBytesV1,
        execution: SemanticResourceCeilings,
        clean_projection_build_micros: u64,
        projection_input_bytes: u64,
        projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let vector_generation = evaluation_vector_generation_id(&code, &prepared.prepared)?;
        let vector_bytes = prepared
            .prepared
            .vectors
            .iter()
            .try_fold(0_u64, |total, vector| {
                let bytes = u64::try_from(vector.values.len())
                    .ok()?
                    .checked_mul(std::mem::size_of::<f32>() as u64)?;
                total.checked_add(bytes)
            })
            .ok_or(SemanticRuntimeScheduleFailureV1::Projection)?;
        let source_manifest_digest = code.projection().request().changes.manifest_digest.clone();
        let source_generation = code.manifest().generation_id.clone();
        let sequence_length = prepared
            .prepared
            .embedding_key
            .embedding_key()
            .truncation_length;
        let resources = ProductionCandidateNativeGenerationResourcesV1 {
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            incremental_source_generation: source_generation.clone(),
            incremental_source_manifest_digest: source_manifest_digest,
            vector_generation: Some(vector_generation.clone()),
            artifact_digest: Some(artifact_digest),
            model_bytes: artifact_bytes.model,
            tokenizer_bytes: artifact_bytes.tokenizer,
            threads: execution.max_threads,
            max_concurrent_sessions: execution.max_concurrent_sessions,
            batch_size: execution.max_batch_size,
            sequence_length,
            load_deadline_ms: execution.load_deadline_ms,
            // Vector preparation or the first genuine query opens the one
            // request-scoped runtime. `generation_resources` observes that
            // shared runtime's cold-load duration before evidence is accepted.
            cold_model_load_micros: 0,
            vector_bytes,
            index_bytes: 0,
            cache_bytes: 0,
            clean_projection_build_micros,
            incremental_rebuild_micros: 0,
            projection_cases: BTreeMap::new(),
        };
        let projection = prepared.prepared.embedding_key.clone();
        let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let vectors = PublishedSemanticVectorReadPortV1::from_prepared(
            &prepared.prepared,
            vector_generation.clone(),
            search_index_key.clone(),
            &code,
        )
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let secret = canonical_sha256(&(
            "tracedecay.semantic-evaluation-query-key.v1",
            code.manifest().generation_id.clone(),
            vector_generation.clone(),
        ))
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
        .as_str()
        .as_bytes()
        .to_vec();
        let query_keys = RetrievalCursorKeyringV1::new(
            projection.privacy_domain().clone(),
            RetrievalCursorKeyId::new("semantic-evaluation.query-key.v1")
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?,
            projection.privacy_key_epoch(),
            secret,
            60_000_000,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        Ok(Self {
            source_generation,
            projection,
            search_index_key,
            vector_generation,
            prepared_projection: prepared.prepared,
            projection_input_bytes,
            projection_batch_cache,
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            code,
            cancellation,
            query_factory: prepared.query_factory,
            vectors,
            query_keys,
            resources,
        })
    }

    pub(crate) fn generation_resources(&self) -> ProductionCandidateNativeGenerationResourcesV1 {
        let mut resources = self.resources.clone();
        resources.cache_bytes = self.query_factory.resident_cache_bytes();
        resources.cold_model_load_micros = self.query_factory.cold_load_micros().unwrap_or(0);
        resources
    }

    pub fn projection(&self) -> &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        &self.projection
    }

    pub fn query_factory(&self) -> &SemanticEvaluationQueryFactoryV1 {
        &self.query_factory
    }

    pub fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        rerank_authority: Option<&ProductionCodeRerankAuthorityV1>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError> {
        if context.code_generation != &self.source_generation
            || context.code.manifest().generation_id != self.source_generation
        {
            return Err(CandidateOutputError::Contract(
                "native semantic evaluator generation changed".to_owned(),
            ));
        }
        let control = SemanticEvaluationExecutionControlV1 {
            started: std::time::Instant::now(),
            cancellation: Arc::clone(&self.cancellation),
        };
        let mut rerank_views =
            GenerationBoundCodeRerankViewsV1::new(context.code, context.query_view);
        let rerank = context
            .rerank_policy
            .zip(rerank_authority)
            .map(|(policy, authority)| {
                tracedecay_query::search_quality::semantic_native::SemanticNativeRerankInputV1 {
                    request: context.request,
                    policy,
                    views: &mut rerank_views as &mut _,
                    executor: authority.executor(),
                    control: &control,
                }
            });
        if context.profile.semantic_weight_ppm == 0 {
            return evaluate(ProductionCandidateNativeQueryInputsV1 {
                semantic: None,
                rerank,
            });
        }
        let query_digest = self
            .query_keys
            .digest_active_query(context.request, context.query_view)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let request = SemanticRetrievalRequestV1 {
            base: context.request.clone(),
            query_digest,
            query_view: context.query_view,
            projection: &self.projection,
            search_index_key: &self.search_index_key,
            capability_manifest_digest: self.capability_manifest_digest.clone(),
            vector_generation: self.vector_generation.clone(),
            code_generation: self.source_generation.clone(),
            budget: context.request.budget,
        };
        request
            .validate()
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let embedder = self
            .query_factory
            .create(&control, request.budget.deadline_micros);
        let scoped_vectors = ScopedSemanticEvaluationVectorReadPortV1 {
            inner: &self.vectors,
            allowed_chunks: context.semantic_allowed_chunks,
        };
        let lane = SemanticCodeRetriever::new(&embedder, &scoped_vectors, &control);
        evaluate(ProductionCandidateNativeQueryInputsV1 {
            semantic: Some(
                tracedecay_query::search_quality::semantic_native::SemanticNativeSemanticInputV1 {
                    lane: &lane,
                    request: &request,
                },
            ),
            rerank,
        })
    }
}

struct SemanticEvaluationExecutionControlV1 {
    started: std::time::Instant,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
}

impl SemanticExecutionControl for SemanticEvaluationExecutionControlV1 {
    fn is_cancelled(&self) -> bool {
        self.cancellation.interruption().is_some()
    }

    fn elapsed_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }
}

impl RerankExecutionControlV1 for SemanticEvaluationExecutionControlV1 {
    fn elapsed_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation.interruption().is_some()
    }
}

impl SemanticRuntimeGenerationInspectorV1 for ProductionSemanticRuntimeV1 {
    fn inspect_generation<'a>(
        &'a self,
        required: &'a crate::config::retrieval::SemanticCompatibilityPinsV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticExecutableGenerationLeaseV1, SemanticRuntimeBackendErrorV1>,
    > {
        Box::pin(async move {
            let retained = self
                .graph
                .graph_for_current()
                .await
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
            let cancellation = Arc::clone(retained.cancellation());
            let store = GraphVectorGenerationStoreV1::read_only_generation(
                &retained,
                &required.vector_generation_id,
            )
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
            let generation = store
                .generation(&required.vector_generation_id, cancellation)
                .await
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
                .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
            if !configured_resource_ceiling_covers(&self.resources, required.resources) {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let artifact_bytes = installed_artifact_member_bytes(&self.lifecycle)
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
            crate::hotpath_observe::semantic_model_resident_bytes(
                artifact_bytes
                    .model
                    .saturating_add(artifact_bytes.tokenizer),
            );
            if artifact_bytes.model != required.resources.model_bytes
                || artifact_bytes.tokenizer != required.resources.tokenizer_bytes
            {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let lifecycle = Arc::clone(&self.lifecycle);
            let projection = generation.embedding_key().clone();
            let resources = accepted_semantic_resources(required.resources);
            let verified = tokio::task::spawn_blocking(move || {
                LoadedSemanticArtifactV1::from_lifecycle_projection(
                    &lifecycle,
                    &projection,
                    resources,
                )
            })
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            if verified.projection() != generation.embedding_key()
                || required.projection != *generation.embedding_key()
                || required.implementation_revision.as_str() != "semantic.fastembed.production.v1"
            {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let lifecycle = self.lifecycle.status();
            let state = lifecycle
                .state
                .ok_or(SemanticRuntimeBackendErrorV1::Unavailable)?;
            let artifact_digest = state.artifact_digest();
            let expected_artifact = required.artifact_manifest_digest.as_str();
            if artifact_digest != expected_artifact
                && expected_artifact.strip_prefix("sha256:") != Some(artifact_digest)
            {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let expected_runtime_digest = canonical_sha256(&(
                "tracedecay.semantic-runtime-compatibility.v1",
                &generation.embedding_key().embedding_key().runtime_backend,
                &generation
                    .embedding_key()
                    .embedding_key()
                    .runtime_build_revision,
                generation.embedding_key().embedding_key().device_class,
                generation.embedding_key().embedding_key().precision,
            ))
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            if required.runtime_compatibility_digest != expected_runtime_digest {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let evidence = SemanticExecutableGenerationV1::new(
                required.clone(),
                required.resources,
                true,
                true,
            )
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            Ok(SemanticExecutableGenerationLeaseV1::new(
                evidence,
                (store, retained),
            ))
        })
    }
}

fn semantic_source_manifest_digest(request: &ProjectionBatchRequestV1) -> &ManifestDigest {
    &request.changes.manifest_digest
}

#[derive(Clone, Copy)]
struct InstalledArtifactMemberBytesV1 {
    model: u64,
    tokenizer: u64,
}

fn installed_artifact_member_bytes(
    lifecycle: &SemanticModelLifecycleOwnerV1,
) -> Result<InstalledArtifactMemberBytesV1, SemanticRuntimeScheduleFailureV1> {
    let status = lifecycle.status();
    let state = status
        .state
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    let model = lifecycle
        .catalog()
        .get(state.model_id())
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    let member_bytes = |role: &str| {
        model
            .members
            .get(role)
            .map(|member| member.length)
            .filter(|bytes| *bytes != 0)
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)
    };
    Ok(InstalledArtifactMemberBytesV1 {
        model: member_bytes("model")?,
        tokenizer: member_bytes("tokenizer")?,
    })
}

fn configured_resource_ceiling_covers(
    configured: &SemanticResourceCeilings,
    required: crate::config::retrieval::SemanticResourceRequirementV1,
) -> bool {
    configured.max_model_bytes >= required.model_bytes
        && configured.max_tokenizer_bytes >= required.tokenizer_bytes
        && configured.max_resident_bytes >= required.resident_bytes
        && configured.max_threads >= required.threads
        && configured.max_concurrent_sessions >= required.max_concurrent_sessions
        && configured.max_batch_size >= required.batch_size
        && configured.max_sequence_length >= required.sequence_length
        && configured.load_deadline_ms >= required.load_deadline_ms
}

fn configured_semantic_resource_ceiling(
    configured: SemanticResourceCeilings,
) -> crate::config::retrieval::SemanticResourceRequirementV1 {
    crate::config::retrieval::SemanticResourceRequirementV1 {
        model_bytes: configured.max_model_bytes,
        tokenizer_bytes: configured.max_tokenizer_bytes,
        resident_bytes: configured.max_resident_bytes,
        threads: configured.max_threads,
        max_concurrent_sessions: configured.max_concurrent_sessions,
        batch_size: configured.max_batch_size,
        sequence_length: configured.max_sequence_length,
        load_deadline_ms: configured.load_deadline_ms,
    }
}

fn evaluation_target_resource_requirement(
    configured: SemanticResourceCeilings,
    artifact: InstalledArtifactMemberBytesV1,
) -> crate::config::retrieval::SemanticResourceRequirementV1 {
    let mut requirement = configured_semantic_resource_ceiling(configured);
    requirement.model_bytes = artifact.model;
    requirement.tokenizer_bytes = artifact.tokenizer;
    requirement
}

fn evaluation_projection_resources(
    configured: SemanticResourceCeilings,
) -> SemanticEvaluationProjectionResourcesV1 {
    SemanticEvaluationProjectionResourcesV1 {
        memory_ceiling_bytes: configured.max_resident_bytes,
    }
}

fn canonical_exact_flat_search_index_key()
-> Result<SemanticSearchIndexKeyV1, SemanticRuntimeBackendErrorV1> {
    SemanticSearchIndexProfileV1::exact_flat_v1()
        .and_then(|profile| profile.index_key())
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
}

fn canonical_ann_hnsw_search_index_key()
-> Result<SemanticSearchIndexKeyV1, SemanticRuntimeBackendErrorV1> {
    SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
        .and_then(|profile| profile.index_key())
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
}

/// A pinned key qualifies only when it is one of the canonical profiles this
/// runtime can actually serve: the exact-flat scan, or HNSW candidate
/// generation with exact rescoring (which falls back to the flat scan on a
/// missing or incomplete index).
fn validate_evaluation_target_search_index(
    search_index_key: &SemanticSearchIndexKeyV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    if *search_index_key == canonical_exact_flat_search_index_key()?
        || *search_index_key == canonical_ann_hnsw_search_index_key()?
    {
        Ok(())
    } else {
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    }
}

const EVALUATION_SEMANTIC_CALIBRATION_COHORT_DOMAIN_V1: &str =
    "tracedecay.semantic.evaluation-calibration-cohort.v1";
const EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1: u64 = 0;

/// Certify a candidate against the calibration its generation actually
/// measures.
///
/// `measured_maximum_distance_micros` comes from
/// [`measure_acceptance_calibration`] over the committed generation named by
/// `candidate`. Because that measurement is deterministic in an immutable
/// generation, the proposing writer and this certifying reader derive the same
/// bound and exact equality still certifies the candidate.
fn certify_evaluation_target_compatibility(
    candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
    capability_manifest_digest: &ManifestDigest,
    measured_maximum_distance_micros: i64,
) -> Result<crate::config::retrieval::SemanticCompatibilityPinsV1, SemanticRuntimeBackendErrorV1> {
    let fusion_revision =
        ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    let calibration = canonical_evaluation_calibration(
        candidate,
        source_generation,
        source_manifest_digest,
        capability_manifest_digest,
        measured_maximum_distance_micros,
    )?;
    if candidate.fusion_revision != fusion_revision || candidate.calibration != calibration {
        return Err(SemanticRuntimeBackendErrorV1::Rejected);
    }
    let mut certified = candidate.clone();
    certified.fusion_revision = fusion_revision;
    certified.calibration = calibration;
    Ok(certified)
}

fn canonical_evaluation_calibration(
    candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
    capability_manifest_digest: &ManifestDigest,
    measured_maximum_distance_micros: i64,
) -> Result<SemanticCalibrationProfileV1, SemanticRuntimeBackendErrorV1> {
    source_generation
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    source_manifest_digest
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    capability_manifest_digest
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    let cohort_digest = canonical_sha256(&(
        EVALUATION_SEMANTIC_CALIBRATION_COHORT_DOMAIN_V1,
        source_generation,
        source_manifest_digest,
        capability_manifest_digest,
        &candidate.projection,
        &candidate.vector_generation_id,
        &candidate.artifact_manifest_digest,
    ))
    .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    Ok(SemanticCalibrationProfileV1 {
        calibration_profile_id: candidate.calibration.calibration_profile_id.clone(),
        cohort_digest,
        projection_key: candidate.projection.projection_key().clone(),
        vector_generation: candidate.vector_generation_id.clone(),
        capability_manifest_digest: capability_manifest_digest.clone(),
        maximum_distance_micros: measured_maximum_distance_micros,
        minimum_margin_micros: EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1,
    })
}

fn lifecycle_artifact_matches(
    lifecycle_state: &SemanticModelLifecycleStateV1,
    expected_artifact: &ManifestDigest,
) -> bool {
    let observed = lifecycle_state.artifact_digest();
    observed == expected_artifact.as_str()
        || expected_artifact.as_str().strip_prefix("sha256:") == Some(observed)
}

fn check_evaluation_cancellation(
    cancellation: &dyn SemanticEvaluationCancellationV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    match cancellation.interruption() {
        None => Ok(()),
        Some(_) => Err(SemanticRuntimeBackendErrorV1::Unavailable),
    }
}

fn revalidation_error(error: SemanticRuntimeBackendErrorV1) -> SemanticRuntimeBackendErrorV1 {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => SemanticRuntimeBackendErrorV1::Unavailable,
        SemanticRuntimeBackendErrorV1::Rejected | SemanticRuntimeBackendErrorV1::Conflict => {
            SemanticRuntimeBackendErrorV1::Conflict
        }
    }
}

fn lifecycle_publication_error(
    error: tracedecay_semantic::ModelLifecycleErrorV1,
) -> SemanticRuntimeBackendErrorV1 {
    match error {
        tracedecay_semantic::ModelLifecycleErrorV1::Rejected => {
            SemanticRuntimeBackendErrorV1::Conflict
        }
        tracedecay_semantic::ModelLifecycleErrorV1::Cancelled
        | tracedecay_semantic::ModelLifecycleErrorV1::CancellationCleanupQuarantined(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::CancellationCleanupFailed(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::Catalog(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::StoreUnavailable
        | tracedecay_semantic::ModelLifecycleErrorV1::DownloadFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::DownloadFailedWithReason(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::VerificationFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::RerankerUnavailable
        | tracedecay_semantic::ModelLifecycleErrorV1::InstallFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::WorkerJoinFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::ArtifactImport(_) => {
            SemanticRuntimeBackendErrorV1::Unavailable
        }
    }
}

const fn semantic_runtime_backend_outcome(error: SemanticRuntimeBackendErrorV1) -> &'static str {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => "unavailable",
        SemanticRuntimeBackendErrorV1::Rejected => "rejected",
        SemanticRuntimeBackendErrorV1::Conflict => "conflict",
    }
}

fn revalidate_lifecycle_verification(
    expected: &SemanticEvaluationLifecycleVerificationV1,
    observed: &SemanticEvaluationLifecycleVerificationV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    if expected == observed {
        Ok(())
    } else {
        Err(SemanticRuntimeBackendErrorV1::Conflict)
    }
}

fn accepted_semantic_resources(
    accepted: crate::config::retrieval::SemanticResourceRequirementV1,
) -> SemanticResourceCeilings {
    SemanticResourceCeilings {
        max_model_bytes: accepted.model_bytes,
        max_tokenizer_bytes: accepted.tokenizer_bytes,
        max_resident_bytes: accepted.resident_bytes,
        max_threads: accepted.threads,
        max_concurrent_sessions: accepted.max_concurrent_sessions,
        max_batch_size: accepted.batch_size,
        max_sequence_length: accepted.sequence_length,
        load_deadline_ms: accepted.load_deadline_ms,
    }
}

fn block_on_semantic_evaluation<Output>(
    future: impl Future<Output = Result<Output, SemanticRuntimeScheduleFailureV1>>,
) -> Result<Output, SemanticRuntimeScheduleFailureV1> {
    match tokio::runtime::Handle::try_current() {
        // Daemon evaluation owns a Tokio blocking worker. Those workers retain
        // the runtime handle even though they are outside an async task, so the
        // handle is the canonical executor for projection-case futures.
        Ok(runtime) => runtime.block_on(future),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
            .block_on(future),
    }
}

fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

struct PublishedSemanticVectorReadPortV1 {
    generation: VectorGenerationIdV1,
    projection_key: tracedecay_domain::ProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    source_generation: CodeGenerationId,
    capability_manifest_digest: ManifestDigest,
    rows: Vec<SemanticVectorRecordV1>,
    ann: PublishedSemanticAnnBindingV1,
}

/// The port's generation-bound ANN candidate authority, decided once at
/// construction against the complete resident row set.
enum PublishedSemanticAnnBindingV1 {
    /// The typed reason ANN candidates cannot serve; the lane observes it
    /// and falls back to the exact-flat scan.
    Unavailable(SemanticAnnIndexStateV1),
    /// A persisted index whose census equals the resident row count, so
    /// every index hit maps to exactly one resident row.
    Serving {
        index: SemanticAnnServingIndexV1,
        rows_by_chunk: BTreeMap<tracedecay_domain::CodeSearchChunkId, usize>,
    },
}

impl PublishedSemanticAnnBindingV1 {
    /// Binds an acquired index to the resident rows, or records the typed
    /// reason none serves. `index` must be `None` exactly when the store was
    /// consulted and held no populated index for the generation.
    fn bind(
        search_index_key: &SemanticSearchIndexKeyV1,
        index: Option<SemanticAnnServingIndexV1>,
        rows: &[SemanticVectorRecordV1],
    ) -> Result<Self, RetrievalPortError> {
        match (search_index_key.kind, index) {
            (SemanticSearchIndexKindV1::ExactFlat, None) => {
                Ok(Self::Unavailable(SemanticAnnIndexStateV1::Unsupported))
            }
            (SemanticSearchIndexKindV1::ExactFlat, Some(_)) => Err(RetrievalPortError::Contract(
                "an exact-flat semantic port must not bind an ANN index".to_owned(),
            )),
            (SemanticSearchIndexKindV1::AnnHnswExactRescore, None) => {
                Ok(Self::Unavailable(SemanticAnnIndexStateV1::Missing))
            }
            (SemanticSearchIndexKindV1::AnnHnswExactRescore, Some(index)) => {
                let resident = rows.len() as u64;
                if index.indexed() == resident {
                    let rows_by_chunk = rows
                        .iter()
                        .enumerate()
                        .map(|(ordinal, row)| (row.chunk_id.clone(), ordinal))
                        .collect();
                    Ok(Self::Serving {
                        index,
                        rows_by_chunk,
                    })
                } else {
                    // The index covers only this generation's own staged
                    // vectors; rows hydrated from base-generation reuse are
                    // resident but not indexed, so candidate generation
                    // would silently drop them.
                    Ok(Self::Unavailable(
                        SemanticAnnIndexStateV1::IncompleteCoverage {
                            indexed: index.indexed(),
                            resident,
                        },
                    ))
                }
            }
        }
    }
}

fn semantic_candidate_identity(
    chunk: &CodeSearchChunkV1,
) -> Result<(RetrievalAnchorId, LogicalEvidenceId, SourceOccurrenceId), RetrievalPortError> {
    let chunk_id = chunk.id.as_str();
    let evidence_id = chunk.anchor.symbol_occurrence_id.as_ref().map_or_else(
        || format!("code-chunk:{chunk_id}"),
        |symbol| format!("code-symbol:{}", symbol.as_str()),
    );
    Ok((
        RetrievalAnchorId::new(evidence_id.clone())
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        LogicalEvidenceId::new(evidence_id)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        SourceOccurrenceId::new(format!("code-chunk:{chunk_id}"))
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
    ))
}

struct ScopedSemanticEvaluationVectorReadPortV1<'a> {
    inner: &'a PublishedSemanticVectorReadPortV1,
    allowed_chunks: &'a BTreeSet<tracedecay_domain::CodeSearchChunkId>,
}

impl SemanticVectorReadPort for ScopedSemanticEvaluationVectorReadPortV1<'_> {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        let mut eligible = 0_u64;
        let mut scoped_visit = |row: &SemanticVectorRecordV1| {
            if self.allowed_chunks.contains(&row.chunk_id) {
                eligible = eligible.saturating_add(1);
                visit(row)?;
            }
            Ok(())
        };
        let summary = self
            .inner
            .scan_exact_flat(request, examine, &mut scoped_visit)?;
        Ok(SemanticVectorScanSummaryV1 {
            examined: summary.examined,
            eligible,
            excluded: summary.examined.saturating_sub(eligible),
            unknown: summary.unknown,
        })
    }
}

fn published_semantic_candidate_score_domain() -> Result<ScoreDomainId, RetrievalPortError> {
    ScoreDomainId::new(tracedecay_query::retrieval::QUERY_SEMANTIC_SCORE_DOMAIN_V1)
        .map_err(|error| RetrievalPortError::Contract(error.to_string()))
}

impl PublishedSemanticVectorReadPortV1 {
    fn from_prepared(
        prepared: &PreparedVectorGenerationV1,
        generation: VectorGenerationIdV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
    ) -> Result<Self, RetrievalPortError> {
        if prepared.request.changes.to_generation != code.manifest().generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.evaluation.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(prepared.vectors.len());
        for (ordinal, vector) in prepared.vectors.iter().enumerate() {
            let chunk = chunks
                .get(&vector.chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let chunk_id = &vector.chunk_id;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.evaluation.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: published_semantic_candidate_score_domain()?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: generation.clone(),
                projection_key: prepared.request.target_projection_key.clone(),
                source_generation: prepared.request.changes.to_generation.clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        Ok(Self {
            generation,
            projection_key: prepared.request.target_projection_key.clone(),
            search_index_key,
            source_generation: prepared.request.changes.to_generation.clone(),
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            rows,
            // Evaluation ports read a prepared in-memory projection that was
            // never staged into the graph store, so no persisted index can
            // exist for it.
            ann: PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported),
        })
    }

    fn new(
        vectors: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
    ) -> Result<Self, RetrievalPortError> {
        if vectors.source_generation() != &code.manifest().generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(vectors.vectors().len());
        for (ordinal, (chunk_id, vector)) in vectors.vectors().iter().enumerate() {
            let chunk = chunks
                .get(chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.daemon.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: published_semantic_candidate_score_domain()?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: vectors.generation_id().clone(),
                projection_key: vectors.projection_key().clone(),
                source_generation: vectors.source_generation().clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        let ann = PublishedSemanticAnnBindingV1::bind(&search_index_key, ann, &rows)?;
        Ok(Self {
            generation: vectors.generation_id().clone(),
            projection_key: vectors.projection_key().clone(),
            search_index_key,
            source_generation: vectors.source_generation().clone(),
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            rows,
            ann,
        })
    }
}

impl SemanticVectorReadPort for PublishedSemanticVectorReadPortV1 {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        if request.search_kind != SemanticSearchKindV1::ExactFlat
            || request.vector_generation != &self.generation
            || request.projection_key != &self.projection_key
            || request.search_index_key != &self.search_index_key
            || request.source_generation != &self.source_generation
            || request.capability_manifest_digest != &self.capability_manifest_digest
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        hotpath::gauge!("semantic_exact_flat_scan_rows").set(self.rows.len());
        hotpath::gauge!("semantic_exact_flat_scan_dimensions")
            .set(self.rows.first().map_or(0, |row| row.values.len()));
        hotpath::measure_block!("semantic.vector.scan_exact_flat", {
            for row in &self.rows {
                examine()?;
                visit(row)?;
            }
            Ok::<(), RetrievalPortError>(())
        })?;
        Ok(SemanticVectorScanSummaryV1 {
            examined: self.rows.len() as u64,
            eligible: self.rows.len() as u64,
            excluded: 0,
            unknown: 0,
        })
    }

    /// Serves `window` as the ranks past `window.skip` of one index search
    /// to `window.depth`. HNSW has no rank offset: the deeper search is the
    /// only way to reach those ranks, and its prefix may reorder relative to
    /// the shallower pass, which is why the lane tolerates re-served rows.
    fn ann_candidates(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        query: &[f32],
        window: SemanticAnnCandidateWindowV1,
    ) -> Result<SemanticAnnCandidatesV1<'_>, RetrievalPortError> {
        if request.search_kind != SemanticSearchKindV1::AnnHnswExactRescore
            || request.vector_generation != &self.generation
            || request.projection_key != &self.projection_key
            || request.search_index_key != &self.search_index_key
            || request.source_generation != &self.source_generation
            || request.capability_manifest_digest != &self.capability_manifest_digest
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        let (index, rows_by_chunk) = match &self.ann {
            PublishedSemanticAnnBindingV1::Unavailable(state) => {
                return Ok(SemanticAnnCandidatesV1::Unavailable(*state));
            }
            PublishedSemanticAnnBindingV1::Serving {
                index,
                rows_by_chunk,
            } => (index, rows_by_chunk),
        };
        let chunks = hotpath::measure_block!(
            "semantic.vector.ann_candidates",
            index
                .search(query, window.depth)
                .map_err(ann_search_port_error)
        )?;
        hotpath::gauge!("semantic_ann_candidates").set(chunks.len());
        chunks
            .into_iter()
            .skip(window.skip)
            .map(|chunk_id| {
                rows_by_chunk
                    .get(&chunk_id)
                    .map(|ordinal| &self.rows[*ordinal])
                    .ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "semantic ANN index answered with a non-resident row".to_owned(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticAnnCandidatesV1::Candidates)
    }
}

/// Consults the store for the generation's persisted ANN index exactly when
/// the pinned search profile is ANN-kinded. Exact-flat profiles never bind
/// one, and `Ok(None)` under an ANN profile is the typed "no populated
/// index" state the port reports as `Missing`.
fn semantic_ann_serving_index(
    store: &GraphVectorGenerationStoreV1,
    active: &PublishedVectorGenerationV1,
    search_index_key: &SemanticSearchIndexKeyV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<SemanticAnnServingIndexV1>, VectorGenerationStoreErrorV1> {
    match search_index_key.kind {
        SemanticSearchIndexKindV1::ExactFlat => Ok(None),
        SemanticSearchIndexKindV1::AnnHnswExactRescore => store.ann_serving_index(
            active.generation_id(),
            active.embedding_key(),
            active.vectors().keys(),
            cancellation,
        ),
    }
}

/// The ANN index search runs under the same request control as the scan, so
/// its store failures map onto the lane's typed port errors.
fn ann_search_port_error(error: VectorGenerationStoreErrorV1) -> RetrievalPortError {
    match error {
        VectorGenerationStoreErrorV1::Cancelled => RetrievalPortError::Cancelled,
        VectorGenerationStoreErrorV1::DeadlineExceeded => RetrievalPortError::BudgetExceeded,
        other => RetrievalPortError::AuthorityUnavailable(other.to_string()),
    }
}

fn semantic_projection_request(
    generation: &CodeIndexPublishedGenerationV1,
    projection: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    current: Option<&SemanticGenerationPointerV1>,
) -> Result<ProjectionBatchRequestV1, SemanticRuntimeScheduleFailureV1> {
    let source = generation.projection().request();
    let incremental = current.is_some_and(|pointer| {
        source.changes.from_generation.as_ref() == Some(&pointer.source_generation)
            && projection.projection_key() == &pointer.projection_key
    });
    let mut changes = if incremental {
        source.changes.clone()
    } else {
        ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: generation.manifest().generation_id.clone(),
            manifest_digest: source.changes.manifest_digest.clone(),
            added_or_changed: generation
                .chunks()
                .chunks()
                .iter()
                .map(|chunk| ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                })
                .collect(),
            deleted: Vec::new(),
            reused: Vec::new(),
        }
    };
    // One digest computation serves both branches: it derives the digest of a
    // freshly built full-rebuild change set, and it recomputes an incremental
    // retarget's digest so a malformed source handoff cannot cross the
    // semantic boundary.
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    let mut request = ProjectionBatchRequestV1 {
        request_digest: changes.manifest_digest.clone(),
        changes,
        previous_projection_key: incremental.then(|| projection.projection_key().clone()),
        target_projection_key: projection.projection_key().clone(),
        replay_reason: if incremental {
            ProjectionReplayReasonV1::SourceEdit
        } else {
            ProjectionReplayReasonV1::FullRebuildIncompatible
        },
    };
    request.request_digest =
        expected_request_digest(&request).map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    Ok(request)
}

fn evaluation_projection_plan(
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationPlanV1, SemanticRuntimeScheduleFailureV1> {
    evaluation_projection_plan_from_request(generation, &prepared.request, base_generation)
}

fn evaluation_projection_plan_from_request(
    generation: &CodeIndexPublishedGenerationV1,
    request: &ProjectionBatchRequestV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationPlanV1, SemanticRuntimeScheduleFailureV1> {
    if request.changes.to_generation != generation.manifest().generation_id {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(evaluation_projection_plan_from_canonical_chunks(
        generation.chunks().chunks(),
        request,
        base_generation,
    ))
}

fn evaluation_projection_plan_from_canonical_chunks(
    canonical_chunks: &[Arc<CodeSearchChunkV1>],
    request: &ProjectionBatchRequestV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> VectorGenerationPlanV1 {
    VectorGenerationPlanV1 {
        target_projection_key: request.target_projection_key.clone(),
        source_generation: request.changes.to_generation.clone(),
        source_manifest_digest: request.changes.manifest_digest.clone(),
        expected_chunk_ids: canonical_chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
        base_generation,
    }
}

fn evaluation_projection_case_store(
    retained: &RetainedSemanticVectorGraphV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<GraphVectorGenerationStoreV1, SemanticRuntimeScheduleFailureV1> {
    evaluation_projection_case_store_for_changes(
        retained,
        prepared.embedding_key.clone(),
        &prepared.request.changes,
    )
}

fn evaluation_projection_case_store_for_changes(
    retained: &RetainedSemanticVectorGraphV1,
    projection: tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    changes: &ChangedCodeChunkSetV1,
) -> Result<GraphVectorGenerationStoreV1, SemanticRuntimeScheduleFailureV1> {
    let store = GraphVectorGenerationStoreV1::open(retained)
        .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
    let descriptor = SemanticVectorStageDescriptorV1::from_changes(projection, changes)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    store
        .configure_stage(descriptor)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    Ok(store)
}

fn evaluation_vector_generation_id(
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<VectorGenerationIdV1, SemanticRuntimeScheduleFailureV1> {
    let plan = evaluation_projection_plan(generation, prepared, None)?;
    generation_identity_digest(&plan)
        .map(VectorGenerationIdV1::new)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)
}

async fn publish_evaluation_projection_case_isolated(
    store: &GraphVectorGenerationStoreV1,
    cancellation: &Arc<dyn GraphCancellation>,
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<
    crate::store::vector_generations::VectorGenerationPublicationV1,
    SemanticRuntimeScheduleFailureV1,
> {
    let plan = evaluation_projection_plan(generation, prepared, base_generation)?;
    let build = store
        .rebuild_generation(plan, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();
    commit_evaluation_prepared_generation(
        store,
        &build,
        prepared,
        generation.chunks().chunks(),
        Arc::clone(cancellation),
    )
    .await?;
    let publication = store
        .publish_generation(&build, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    if !store
        .published_generation_is_visible(&publication.generation_id, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(publication)
}

fn projection_case_sample_from_prepared(
    prepared: &PreparedVectorGenerationV1,
    elapsed_micros: u64,
    input_bytes: u64,
    outcome: SemanticProjectionCaseOutcomeV1,
) -> SemanticProjectionCaseSampleV1 {
    let mut chunks_added_or_changed = 0_u64;
    let mut chunks_deleted = 0_u64;
    let mut chunks_reused = 0_u64;
    for receipt in &prepared.receipt.receipts {
        match receipt.operation {
            ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                chunks_added_or_changed += 1;
            }
            ProjectionOperationV1::Deleted => chunks_deleted += 1,
            ProjectionOperationV1::Reused => chunks_reused += 1,
        }
    }
    SemanticProjectionCaseSampleV1 {
        outcome,
        elapsed_micros,
        input_bytes,
        chunks_added_or_changed,
        chunks_deleted,
        chunks_reused,
        projection_calls: prepared.vectors.len() as u64,
    }
}

fn execute_calibrated_semantic_query<'a, L>(
    lane: &'a L,
    readiness: SemanticLaneReadinessV1<'a>,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    L: SemanticLaneRetriever,
{
    let availability = match &readiness {
        SemanticLaneReadinessV1::Ready { .. } => RetrievalAvailabilityV1::Ready,
        SemanticLaneReadinessV1::Unavailable(state) => match state {
            SemanticIndexStateV1::Unavailable => RetrievalAvailabilityV1::Unavailable,
            SemanticIndexStateV1::Indexing => RetrievalAvailabilityV1::Indexing,
            SemanticIndexStateV1::Degraded => RetrievalAvailabilityV1::Degraded,
            SemanticIndexStateV1::Failed => RetrievalAvailabilityV1::Failed,
            SemanticIndexStateV1::Stale => RetrievalAvailabilityV1::Stale,
            SemanticIndexStateV1::Incompatible => RetrievalAvailabilityV1::Incompatible,
        },
    };
    let requirement = match mode {
        SemanticQueryModeV1::FallbackAllowed => RetrievalRequirementV1::FallbackAllowed,
        SemanticQueryModeV1::StrictSemantic => RetrievalRequirementV1::StrictSemantic,
    };
    let on_abstention = match mode {
        SemanticQueryModeV1::FallbackAllowed => SemanticAbstentionDispositionV1::UseFallback,
        SemanticQueryModeV1::StrictSemantic => SemanticAbstentionDispositionV1::RejectUnavailable,
    };
    let decision = match select_retrieval(availability, requirement) {
        RetrievalSelectionV1::Semantic => {
            SemanticQueryDecisionV1::ExecuteSemantic { on_abstention }
        }
        RetrievalSelectionV1::FrozenFallback => SemanticQueryDecisionV1::UseFallback,
        RetrievalSelectionV1::Unavailable => SemanticQueryDecisionV1::RejectUnavailable,
    };
    CalibratedSemanticQueryService::new(lane).execute(readiness, decision, fallback)
}

/// Complete input set for one application semantic-search composition.
pub struct ApplicationSemanticSearchParametersV1<'a, V, C> {
    pub handle: &'a DaemonSemanticRuntimeHandleV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub generation: &'a CompleteSemanticGenerationV1,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub vectors: &'a V,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub fallback: Arc<QueryFallbackSubpayload>,
}

/// Application search composition: admit `SemanticCodeRetriever` only through
/// [`DaemonSemanticRuntimeHandleV1::query_factory`].
///
/// Non-ready / indexing / degraded states never construct the retriever and
/// return the frozen query fallback without waiting on `FastEmbed` download or
/// projection. Exact/lexical/graph owners stay independently callable.
#[hotpath::measure(label = "usecases.semantic.search")]
pub fn compose_application_semantic_search<'a, V, C>(
    parameters: ApplicationSemanticSearchParametersV1<'a, V, C>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl + Sync,
{
    let ApplicationSemanticSearchParametersV1 {
        handle,
        request,
        generation,
        calibration,
        vectors,
        control,
        mode,
        fallback,
    } = parameters;
    let factory = handle.query_factory(
        &request.code_generation,
        &request.vector_generation,
        request.projection.projection_key(),
    );
    match factory {
        Some(factory) => {
            let embedder = factory.create(control, request.budget.deadline_micros);
            let lane = SemanticCodeRetriever::new(&embedder, vectors, control);
            execute_calibrated_semantic_query(
                &lane,
                SemanticLaneReadinessV1::Ready {
                    request,
                    generation,
                    calibration,
                },
                mode,
                fallback,
            )
        }
        None => execute_calibrated_semantic_query(
            &NeverCalledSemanticLane,
            SemanticLaneReadinessV1::Unavailable(index_state_from_status(handle.status())),
            mode,
            fallback,
        ),
    }
}

/// Project-scoped application search consumer over the retained production
/// runtime and committed configuration-selected vector generation.
#[hotpath::measure(label = "usecases.semantic.project_search", future = true)]
pub async fn compose_project_application_semantic_search<C>(
    project_root: &Path,
    code_generation: &CodeIndexPublishedGenerationV1,
    request: &SemanticRetrievalRequestV1<'_>,
    calibration: Option<&SemanticCalibrationProfileV1>,
    control: &C,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    C: SemanticExecutionControl + Sync,
{
    let Some(runtime) = project_semantic_production_runtime(project_root) else {
        return execute_calibrated_semantic_query(
            &NeverCalledSemanticLane,
            SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
            mode,
            fallback,
        );
    };
    runtime
        .execute_search(
            code_generation,
            request,
            calibration,
            control,
            mode,
            fallback,
        )
        .await
}

/// Production execution bridge for callers that already own authenticated
/// semantic request material and an authenticated frozen query composition.
///
/// MCP cannot use this bridge until its query-MAC and query composition
/// authorities are mounted; accepting the typed request here prevents that
/// boundary from inventing either input.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProductionProjectSemanticSearchBridgeV1;

/// Authenticated project-scoped inputs for production semantic search.
pub struct AuthorizedProjectSemanticSearchParametersV1<'a, C> {
    pub project_root: &'a Path,
    pub code_generation: &'a CodeIndexPublishedGenerationV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub authorized_query: &'a AuthorizedQueryFallbackV1,
}

impl ProductionProjectSemanticSearchBridgeV1 {
    pub fn execute<'a, C>(
        &'a self,
        parameters: AuthorizedProjectSemanticSearchParametersV1<'a, C>,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>>
    where
        C: SemanticExecutionControl + Sync + 'a,
    {
        let AuthorizedProjectSemanticSearchParametersV1 {
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            authorized_query,
        } = parameters;
        if request.query_digest != authorized_query.query_digest {
            return Box::pin(async { Err(SemanticQueryServiceError::InvalidFallback) });
        }
        Box::pin(compose_project_application_semantic_search(
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            Arc::clone(&authorized_query.fallback),
        ))
    }
}

struct NeverCalledSemanticLane;

impl SemanticLaneRetriever for NeverCalledSemanticLane {
    fn retrieve_semantic(
        &self,
        _request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        Err(RetrievalPortError::Contract(
            "non-ready semantic lane must never be invoked".to_owned(),
        ))
    }
}

/// Daemon backend that surfaces schedule projection through the application port.
pub struct DaemonSemanticRuntimeBackendV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    configuration: Mutex<Option<SemanticConfigurationPinV1>>,
}

impl DaemonSemanticRuntimeBackendV1 {
    #[cfg(test)]
    pub fn new(handle: DaemonSemanticRuntimeHandleV1) -> Self {
        Self {
            handle,
            configuration: Mutex::new(None),
        }
    }

    pub fn from_production(runtime: ProductionSemanticRuntimeV1) -> Self {
        Self {
            handle: runtime.handle.clone(),
            configuration: Mutex::new(None),
        }
    }

    pub fn bind_configuration(&self, pin: SemanticConfigurationPinV1) {
        *self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pin);
    }

    pub fn application_status(&self) -> SemanticRuntimeStatusV1 {
        self.application_status_with_receipt(None)
    }

    fn application_status_with_receipt(
        &self,
        activation_receipt: Option<SemanticActivationReceiptV1>,
    ) -> SemanticRuntimeStatusV1 {
        let configuration = self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        application_status_from_projection(
            &self.handle.status_projection(),
            configuration,
            activation_receipt,
        )
    }
}

impl SemanticRuntimeBackendV1 for DaemonSemanticRuntimeBackendV1 {
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(configuration.clone());
            Ok(self.application_status().state)
        })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }
}

fn index_state_from_status(status: SemanticRuntimeScheduleStatusV1) -> SemanticIndexStateV1 {
    match status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticIndexStateV1::Unavailable,
        SemanticRuntimeScheduleStatusV1::Indexing { .. } => SemanticIndexStateV1::Indexing,
        SemanticRuntimeScheduleStatusV1::Failed { .. } => SemanticIndexStateV1::Failed,
        SemanticRuntimeScheduleStatusV1::Current { .. } => SemanticIndexStateV1::Incompatible,
    }
}

/// Process-local registry so Doctor/`tracedecay_runtime` can observe the
/// daemon-private scheduler without a wire operation.
fn project_semantic_handles() -> &'static Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>> {
    static HANDLES: OnceLock<Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>>> =
        OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn project_semantic_production_runtimes()
-> &'static Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>> {
    static RUNTIMES: OnceLock<Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>>> =
        OnceLock::new();
    RUNTIMES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain a project semantic handle for status/search composition.
pub fn register_project_semantic_runtime(
    project_root: PathBuf,
    handle: DaemonSemanticRuntimeHandleV1,
) {
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, handle);
}

/// Drop a retained project semantic handle.
pub fn unregister_project_semantic_runtime(project_root: &Path) {
    super::unregister_project_semantic_redundancy_generation(project_root);
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

pub fn project_semantic_production_runtime(
    project_root: &Path,
) -> Option<ProductionSemanticRuntimeV1> {
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

pub fn unbind_project_semantic_cache_if_current(
    project_root: &Path,
    generation: &VectorGenerationIdV1,
) -> bool {
    if let Some(runtime) = project_semantic_production_runtime(project_root) {
        return runtime.unbind_cache_if_current(generation);
    }
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .is_some_and(|handle| handle.unbind_query_runtime_if_current(generation))
}

/// Source generation observed in the cache for the committed semantic pins.
///
/// Query adapters compare this identity with the exact sealed code generation
/// selected at admission. A stale or merely indexing vector generation never
/// becomes eligible for semantic composition.
pub fn project_semantic_source_generation(project_root: &Path) -> Option<CodeGenerationId> {
    project_semantic_generation_pointer(project_root).map(|pointer| pointer.source_generation)
}

pub(crate) fn project_semantic_generation_pointer(
    project_root: &Path,
) -> Option<SemanticGenerationPointerV1> {
    let pins = super::project_committed_semantic_pins(project_root)?;
    let observed = if let Some(runtime) = project_semantic_production_runtime(project_root) {
        runtime.handle.current()
    } else {
        project_semantic_handles()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(DaemonSemanticRuntimeHandleV1::current)
    }?;
    (observed.generation == pins.vector_generation_id
        && observed.projection_key == *pins.projection.projection_key())
    .then_some(observed)
}

/// Application status for a mounted project semantic scheduler, if any.
///
/// The durable activation receipt and the scheduler status projection are
/// read under one acquisition of the project activation gate. A concurrent
/// activation mutates the receipt and the installed authorities under that
/// same gate, so status can never pair a stale receipt with a newer scheduler
/// generation (reporting `Current` while semantic is unavailable) or the
/// inverse (reporting degraded after a coherent install).
pub fn project_semantic_application_status(
    project_root: &Path,
    configuration: Option<SemanticConfigurationPinV1>,
) -> Option<SemanticRuntimeStatusV1> {
    super::with_project_semantic_activation_receipt(project_root, |activation_receipt| {
        if let Some(runtime) = project_semantic_production_runtime(project_root) {
            let lifecycle = runtime.lifecycle_status();
            let backend = DaemonSemanticRuntimeBackendV1::from_production(runtime);
            if let Some(configuration) = configuration {
                backend.bind_configuration(configuration);
            }
            return Some(prefer_lifecycle_over_generic_unavailable(
                backend.application_status_with_receipt(activation_receipt),
                &lifecycle,
            ));
        }
        let handle = project_semantic_handles()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()?;
        Some(application_status_from_projection(
            &handle.status_projection(),
            configuration,
            activation_receipt,
        ))
    })
}

/// Doctor/MCP status for a seated or unseated project.
///
/// Seated scheduler views that only report generic unavailability yield to
/// the model-lifecycle owner. A mounted-but-broken runtime keeps its error.
pub fn resolve_project_semantic_runtime_status(
    project_path: Option<&Path>,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let scheduler = project_path
        .and_then(|path| project_semantic_application_status(path, configuration.clone()));
    let lifecycle = match project_path {
        Some(path) => project_or_shared_lifecycle_status(path),
        None if configuration.is_none() => None,
        None => tracedecay_semantic::default_shared_lifecycle_owner().map(|owner| owner.status()),
    };
    resolve_semantic_application_status(scheduler, lifecycle.as_ref(), configuration)
}

pub fn project_or_shared_lifecycle_status(
    project_path: &Path,
) -> Option<SemanticModelLifecycleStatusV1> {
    if let Some(runtime) = project_semantic_production_runtime(project_path) {
        return Some(runtime.lifecycle_status());
    }
    tracedecay_semantic::default_shared_lifecycle_owner().map(|owner| owner.status())
}

/// Hook invoked after a code generation publishes; must not block search.
///
/// The serving owner transfers a shared handle because one decoded generation
/// can be much larger than its captured source. Semantic retention and queued
/// projection must clone this `Arc`, never the immutable generation payload.
pub type SavedCodeGenerationScheduleHookV1 =
    Arc<dyn Fn(Arc<CodeIndexPublishedGenerationV1>) -> bool + Send + Sync>;

/// Daemon runtime retained when the saved-generation hook is constructed.
///
/// Serving publication deliberately invokes the hook from `spawn_blocking`
/// while it owns the synchronous scheduler. Looking up a current Tokio runtime
/// from that worker always fails, so the hook must carry the daemon runtime
/// across the blocking handoff instead.
#[derive(Clone)]
struct SemanticProjectionDispatchRuntimeV1 {
    handle: tokio::runtime::Handle,
}

impl SemanticProjectionDispatchRuntimeV1 {
    fn capture() -> Option<Self> {
        tokio::runtime::Handle::try_current()
            .ok()
            .map(|handle| Self { handle })
    }

    fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }
}

/// Owned authorities and identities captured by a saved-generation hook.
pub struct SavedGenerationScheduleHookParametersV1 {
    pub project_root: PathBuf,
    pub code_index_store_root: PathBuf,
    pub worktree_id: WorktreeId,
    pub handle: DaemonSemanticRuntimeHandleV1,
    /// Daemon-implemented resolution to the Grafeo code-graph runtime that
    /// owns the durable semantic-vector projection.
    pub graph: Arc<dyn SemanticVectorGraphProviderV1>,
    pub lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    pub resources: SemanticResourceCeilings,
    pub document_composition: EmbeddingDocumentCompositionV1,
    pub fair_scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
}

/// Production hook: enqueue semantic projection for each saved generation.
///
/// Artifact admission remains owned by the model lifecycle. Until a complete
/// compatible artifact is available the background task fails closed without
/// joining into exact/lexical/graph search.
pub fn production_saved_generation_schedule_hook(
    parameters: SavedGenerationScheduleHookParametersV1,
) -> SavedCodeGenerationScheduleHookV1 {
    let SavedGenerationScheduleHookParametersV1 {
        project_root,
        code_index_store_root,
        worktree_id,
        handle,
        graph,
        lifecycle,
        resources,
        document_composition,
        fair_scheduler,
    } = parameters;
    let runtime = Arc::new(ProductionSemanticRuntimeV1::new_with_code_index_store_root(
        handle,
        graph,
        code_index_store_root,
        lifecycle,
        resources,
        document_composition,
    ));
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.clone(), runtime.as_ref().clone());
    let dispatch_runtime = SemanticProjectionDispatchRuntimeV1::capture();
    Arc::new(move |generation| {
        if generation.snapshot().worktree.as_ref() != Some(&worktree_id) {
            return false;
        }
        super::register_project_semantic_redundancy_generation(
            project_root.clone(),
            Arc::clone(&generation),
        );
        let runtime = Arc::clone(&runtime);
        let Some(dispatch_runtime) = dispatch_runtime.clone() else {
            return false;
        };
        let queued_bytes = generation
            .chunks()
            .chunks()
            .iter()
            .fold(0_u64, |total, chunk| {
                total.saturating_add(
                    u64::try_from(chunk.sanitized_text.as_str().len()).unwrap_or(u64::MAX),
                )
            });
        let batch = SemanticProjectionBatchV1::new(
            worktree_id.clone(),
            generation.manifest().generation_id.clone(),
            queued_bytes,
            resources.max_resident_bytes,
        );
        let project_root = project_root.clone();
        fair_scheduler
            .enqueue_work(
                batch,
                Box::new(move |lease| {
                    dispatch_runtime.spawn(async move {
                        let lease = Arc::new(lease);
                        if lease.is_cancelled() {
                            return;
                        }
                        let Ok(lease) = Arc::try_unwrap(lease) else {
                            return;
                        };
                        if let Some(required) =
                            super::project_committed_semantic_pins(&project_root)
                            && matches!(
                                runtime
                                    .restore_current(&generation, &required.vector_generation_id)
                                    .await,
                                Ok(true)
                            )
                        {
                            return;
                        }
                        let _ = runtime.schedule_saved_generation_fair(generation, lease);
                    });
                }),
            )
            .is_ok()
    })
}

fn fair_schedule_failure(
    error: SemanticProjectionScheduleErrorV1,
) -> SemanticRuntimeScheduleFailureV1 {
    match error {
        SemanticProjectionScheduleErrorV1::Cancelled => SemanticRuntimeScheduleFailureV1::Cancelled,
        SemanticProjectionScheduleErrorV1::QueueBytesCapacity { .. }
        | SemanticProjectionScheduleErrorV1::QueueBatchCapacity { .. }
        | SemanticProjectionScheduleErrorV1::SessionMemoryReservationTooLarge { .. }
        | SemanticProjectionScheduleErrorV1::PublicationCapacity { .. }
        | SemanticProjectionScheduleErrorV1::PublicationAlreadyClaimed => {
            SemanticRuntimeScheduleFailureV1::Publication
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(feature = "semantic-fastembed")]
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use tracedecay_domain::UtcMicros;

    use tokio::sync::oneshot;
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
    use tracedecay_domain::{
        AdmittedEmbeddingProjectionKeyV1, AuthorizationRevision, BoundedSanitizedText,
        CalibrationProfileId, ChangedCodeChunkSetV1, ChunkerRevision, CodeGenerationId,
        CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
        ContentDigest, EphemeralSanitizedQueryViewV1, FallbackSubpayloadDigest, FileOccurrenceId,
        FusionProfileId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId, PrincipalId,
        ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1, PublicRetrieverStatus,
        QueryDigest, QueryMac, QueryNormalizationRevision, RepositoryId, RetrievalRequest,
        RetrievalScope, RetrievalSnapshot, RetrieverKind, SanitizerRevision, SensitivityDecision,
        SensitivityLevelV1, SingleRootScopeV1, SourceSpan, TemporalModeV1, VectorGenerationIdV1,
        VectorWatermark,
    };

    use tracedecay_semantic::{
        DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1,
        PreparedSemanticRuntimeCommitV1, SemanticRuntimeWorkV1,
    };
    use tracedecay_semantic_contracts::{
        SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1,
        SemanticRuntimeScheduleStatusV1,
    };

    use super::*;

    fn source_generation(value: char) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
    }

    fn documents(value: char) -> Arc<EmbeddingDocumentComposerV1> {
        let index = tracedecay_code_index::lineage::GenerationSymbolIndexV1::new(
            source_generation(value),
            Vec::new(),
        )
        .expect("empty symbol index");
        Arc::new(EmbeddingDocumentComposerV1::new(
            EmbeddingSymbolContextIndexV1::from_generation_symbols(&index),
        ))
    }

    fn vector_generation(value: char) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic.test.vector-generation", value)).expect("manifest digest"),
        )
    }

    #[test]
    fn configured_capacity_is_only_coverage_not_observed_resource_evidence() {
        let configured = SemanticResourceCeilings {
            max_model_bytes: 100,
            max_tokenizer_bytes: 50,
            max_resident_bytes: 500,
            max_threads: 8,
            max_concurrent_sessions: 2,
            max_batch_size: 32,
            max_sequence_length: 512,
            load_deadline_ms: 30_000,
        };
        let accepted = crate::config::retrieval::SemanticResourceRequirementV1 {
            model_bytes: 80,
            tokenizer_bytes: 40,
            resident_bytes: 400,
            threads: 4,
            max_concurrent_sessions: 1,
            batch_size: 16,
            sequence_length: 256,
            load_deadline_ms: 20_000,
        };

        assert!(configured_resource_ceiling_covers(&configured, accepted));
        assert_eq!(
            configured_semantic_resource_ceiling(configured),
            crate::config::retrieval::SemanticResourceRequirementV1 {
                model_bytes: configured.max_model_bytes,
                tokenizer_bytes: configured.max_tokenizer_bytes,
                resident_bytes: configured.max_resident_bytes,
                threads: configured.max_threads,
                max_concurrent_sessions: configured.max_concurrent_sessions,
                batch_size: configured.max_batch_size,
                sequence_length: configured.max_sequence_length,
                load_deadline_ms: configured.load_deadline_ms,
            }
        );
        let applied = accepted_semantic_resources(accepted);
        assert_eq!(applied.max_model_bytes, accepted.model_bytes);
        assert_eq!(applied.max_tokenizer_bytes, accepted.tokenizer_bytes);
        assert_eq!(applied.max_resident_bytes, accepted.resident_bytes);
        assert_eq!(
            applied.max_concurrent_sessions,
            accepted.max_concurrent_sessions
        );
        assert_ne!(applied.max_resident_bytes, configured.max_resident_bytes);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocking_evaluation_drives_async_projection_on_daemon_runtime() {
        let observed = tokio::task::spawn_blocking(|| {
            block_on_semantic_evaluation(async { Ok::<_, SemanticRuntimeScheduleFailureV1>(7) })
        })
        .await
        .expect("blocking evaluator joins");

        assert_eq!(observed, Ok(7));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn saved_generation_dispatch_retains_daemon_runtime_across_blocking_handoff() {
        let runtime = SemanticProjectionDispatchRuntimeV1::capture()
            .expect("daemon runtime is available while the hook is constructed");
        let (observed_tx, observed_rx) = oneshot::channel();

        tokio::task::spawn_blocking(move || {
            runtime.spawn(async move {
                let _ = observed_tx.send(());
            });
        })
        .await
        .expect("blocking serving-generation handoff joins");

        tokio::time::timeout(Duration::from_secs(1), observed_rx)
            .await
            .expect("captured runtime dispatch remains live")
            .expect("semantic dispatch reports completion");
    }

    #[test]
    fn evaluation_target_uses_exact_artifact_bytes_inside_configured_capacity() {
        let configured = SemanticResourceCeilings {
            max_model_bytes: 700,
            max_tokenizer_bytes: 64,
            max_resident_bytes: 2_048,
            max_threads: 8,
            max_concurrent_sessions: 4,
            max_batch_size: 32,
            max_sequence_length: 512,
            load_deadline_ms: 30_000,
        };

        let requirement = evaluation_target_resource_requirement(
            configured,
            InstalledArtifactMemberBytesV1 {
                model: 633,
                tokenizer: 5,
            },
        );

        assert_eq!(requirement.model_bytes, 633);
        assert_eq!(requirement.tokenizer_bytes, 5);
        assert_eq!(requirement.resident_bytes, configured.max_resident_bytes);
        assert_eq!(requirement.threads, configured.max_threads);
        assert!(configured_resource_ceiling_covers(&configured, requirement));
    }

    #[test]
    fn ann_binding_is_unsupported_for_exact_flat_and_missing_for_unbound_ann() {
        let exact_flat = search_index_key();
        assert!(matches!(
            PublishedSemanticAnnBindingV1::bind(exact_flat, None, &[])
                .expect("exact-flat ports bind without an index"),
            PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported)
        ));

        let ann = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
            .and_then(|profile| profile.index_key())
            .expect("ann search index key");
        assert!(matches!(
            PublishedSemanticAnnBindingV1::bind(&ann, None, &[])
                .expect("a consulted store without an index binds as missing"),
            PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Missing)
        ));
    }

    #[test]
    fn preacceptance_target_requires_a_canonical_search_index() {
        let exact_flat = search_index_key().clone();
        assert_eq!(validate_evaluation_target_search_index(&exact_flat), Ok(()));

        let ann = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
            .and_then(|profile| profile.index_key())
            .expect("ann search index key");
        assert_eq!(validate_evaluation_target_search_index(&ann), Ok(()));

        let mut wrong = exact_flat;
        wrong.schema_revision = "semantic-search-index.v0".to_owned();
        assert_eq!(
            validate_evaluation_target_search_index(&wrong),
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );
    }

    fn test_digest(value: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
            .expect("test digest")
    }

    fn evaluation_target_pins(
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        capability_manifest_digest: &ManifestDigest,
    ) -> crate::config::retrieval::SemanticCompatibilityPinsV1 {
        let projection = tracedecay_semantic::session_pool::test_support::authority()
            .projection()
            .clone();
        let vector_generation_id = vector_generation('v');
        let mut candidate = crate::config::retrieval::SemanticCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new("semantic.fastembed.production.v1")
                .expect("implementation revision"),
            fusion_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1,
            )
            .expect("canonical fusion revision"),
            artifact_manifest_digest: projection.embedding_key().model_artifact_digest.clone(),
            runtime_compatibility_digest: test_digest('b'),
            projection,
            search_index_key: search_index_key().clone(),
            vector_generation_id: vector_generation_id.clone(),
            calibration: SemanticCalibrationProfileV1 {
                calibration_profile_id: CalibrationProfileId::new(
                    "calibration.semantic.runtime.v1",
                )
                .expect("calibration profile"),
                cohort_digest: test_digest('c'),
                projection_key: projection_key(),
                vector_generation: vector_generation_id,
                capability_manifest_digest: capability_manifest_digest.clone(),
                maximum_distance_micros: UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
                minimum_margin_micros: EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1,
            },
            resources: crate::config::retrieval::SemanticResourceRequirementV1 {
                model_bytes: 1,
                tokenizer_bytes: 1,
                resident_bytes: 1,
                threads: 1,
                max_concurrent_sessions: 1,
                batch_size: 1,
                sequence_length: 1,
                load_deadline_ms: 1,
            },
        };
        candidate.calibration = canonical_evaluation_calibration(
            &candidate,
            source_generation,
            source_manifest_digest,
            capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        )
        .expect("canonical calibration");
        candidate
    }

    /// The acceptance bound must come from the generation's measurement, not
    /// from a constant baked into the calibration constructor.
    #[test]
    fn canonical_calibration_carries_the_measured_bound() {
        let source_generation = source_generation('m');
        let source_manifest_digest = test_digest('d');
        let capability_manifest_digest = test_digest('e');
        let candidate = evaluation_target_pins(
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
        );

        // Two different measurements of the same identity must produce two
        // different bounds; a constant would produce one.
        for measured in [123_456_789_i64, 987_654_321_i64] {
            let calibration = canonical_evaluation_calibration(
                &candidate,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                measured,
            )
            .expect("canonical calibration");
            assert_eq!(calibration.maximum_distance_micros, measured);
        }
    }

    /// Certification binds the candidate to what its generation measures, so a
    /// candidate carrying any other bound is rejected.
    #[test]
    fn preacceptance_rejects_a_bound_the_generation_did_not_measure() {
        let source_generation = source_generation('b');
        let source_manifest_digest = test_digest('d');
        let capability_manifest_digest = test_digest('e');
        let candidate = evaluation_target_pins(
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
        );

        // The candidate was minted against UNCALIBRATED_MAXIMUM_DISTANCE_MICROS.
        assert_eq!(
            certify_evaluation_target_compatibility(
                &candidate,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            )
            .map(|certified| certified.calibration.maximum_distance_micros),
            Ok(UNCALIBRATED_MAXIMUM_DISTANCE_MICROS)
        );

        // A generation that measures something else does not certify it.
        assert_eq!(
            certify_evaluation_target_compatibility(
                &candidate,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                UNCALIBRATED_MAXIMUM_DISTANCE_MICROS - 1,
            ),
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );
    }

    #[test]
    fn preacceptance_preserves_the_evaluated_calibration_profile() {
        let source_generation = source_generation('c');
        let source_manifest_digest = test_digest('d');
        let capability_manifest_digest = test_digest('e');
        let mut candidate = evaluation_target_pins(
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
        );
        candidate.calibration.calibration_profile_id =
            CalibrationProfileId::new("calibration.semantic.hybrid-conservative")
                .expect("evaluated calibration profile");

        let certified = certify_evaluation_target_compatibility(
            &candidate,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        )
        .expect("evaluated calibration remains independently certifiable");

        assert_eq!(
            certified.calibration.calibration_profile_id,
            candidate.calibration.calibration_profile_id
        );
    }

    /// Regression guard: the unmeasured fallback must stay inside the range the
    /// redundancy authority accepts. An out-of-range bound (the former
    /// `i64::MAX`) silently disarms the semantic redundancy tier.
    #[test]
    fn the_uncalibrated_fallback_stays_in_the_redundancy_accepted_range() {
        assert!(
            (0..=super::super::acceptance_calibration::MAX_COSINE_DISTANCE_MICROS)
                .contains(&UNCALIBRATED_MAXIMUM_DISTANCE_MICROS)
        );
    }

    #[test]
    fn preacceptance_rejects_foreign_fusion_revision() {
        let source_generation = source_generation('f');
        let source_manifest_digest = test_digest('d');
        let capability_manifest_digest = test_digest('e');
        let mut candidate = evaluation_target_pins(
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
        );
        candidate.fusion_revision =
            ComponentRevision::new("ranking.foreign.v1").expect("foreign fusion revision");

        assert_eq!(
            certify_evaluation_target_compatibility(
                &candidate,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            ),
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );
    }

    #[test]
    fn preacceptance_rejects_foreign_calibration_cohort_and_thresholds() {
        let source_generation = source_generation('c');
        let source_manifest_digest = test_digest('d');
        let capability_manifest_digest = test_digest('e');
        let candidate = evaluation_target_pins(
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
        );

        let mut wrong_cohort = candidate.clone();
        wrong_cohort.calibration.cohort_digest = test_digest('f');
        assert_eq!(
            certify_evaluation_target_compatibility(
                &wrong_cohort,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            ),
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );

        let mut wrong_thresholds = candidate;
        wrong_thresholds.calibration.minimum_margin_micros = 1;
        assert_eq!(
            certify_evaluation_target_compatibility(
                &wrong_thresholds,
                &source_generation,
                &source_manifest_digest,
                &capability_manifest_digest,
                UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            ),
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );
    }

    #[test]
    fn deadline_interrupts_semantic_and_rerank_evaluation_cooperatively() {
        struct DeadlineCancellation;

        impl tracedecay_semantic::SemanticExecutionAuthority for DeadlineCancellation {
            fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
                Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
            }
        }

        impl tracedecay_semantic::SemanticEvaluationCancellationV1 for DeadlineCancellation {}

        let control = SemanticEvaluationExecutionControlV1 {
            started: std::time::Instant::now(),
            cancellation: Arc::new(DeadlineCancellation),
        };

        assert!(SemanticExecutionControl::is_cancelled(&control));
        assert!(RerankExecutionControlV1::is_cancelled(&control));
    }

    #[test]
    fn published_semantic_candidates_use_the_seated_score_domain() {
        let domain = published_semantic_candidate_score_domain().expect("score domain");
        assert_eq!(
            domain.as_str(),
            tracedecay_query::retrieval::QUERY_SEMANTIC_EVALUATION_SCORE_DOMAIN_V1
        );
        assert_ne!(domain.as_str(), "score.semantic-distance.daemon.v1");
    }

    fn projection_key() -> ProjectionKeyV1 {
        // Derive the projection key from the same admitted authority the query
        // runtime binds against so `profile_digest` is the canonical digest the
        // embedding projection produces, rather than a non-canonical placeholder.
        tracedecay_semantic::session_pool::test_support::authority()
            .projection()
            .projection_key()
            .clone()
    }

    fn search_index_key() -> &'static SemanticSearchIndexKeyV1 {
        static KEY: std::sync::OnceLock<SemanticSearchIndexKeyV1> = std::sync::OnceLock::new();
        KEY.get_or_init(|| {
            SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .expect("exact-flat search index key")
        })
    }

    #[test]
    fn retained_vector_cache_matches_complete_query_identity() {
        let source = source_generation('r');
        let vector = vector_generation('r');
        let capability = test_digest('f');
        let port = Arc::new(PublishedSemanticVectorReadPortV1 {
            generation: vector.clone(),
            projection_key: projection_key(),
            search_index_key: search_index_key().clone(),
            source_generation: source.clone(),
            capability_manifest_digest: capability.clone(),
            rows: Vec::new(),
            ann: PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported),
        });
        let cached = CachedPublishedVectorsV1 {
            generation: vector.clone(),
            search_index_key: search_index_key().clone(),
            source_generation: source.clone(),
            port,
        };

        assert!(cached.matches(
            &vector,
            &projection_key(),
            search_index_key(),
            &source,
            &capability,
        ));
        assert!(!cached.matches(
            &vector_generation('s'),
            &projection_key(),
            search_index_key(),
            &source,
            &capability,
        ));
        assert!(!cached.matches(
            &vector,
            &projection_key(),
            search_index_key(),
            &source_generation('s'),
            &capability,
        ));
        assert!(!cached.matches(
            &vector,
            &projection_key(),
            search_index_key(),
            &source,
            &test_digest('e'),
        ));
    }

    #[test]
    fn retained_vector_cache_returns_port_without_durable_load() {
        let source = source_generation('r');
        let vector = vector_generation('r');
        let capability = test_digest('f');
        let port = Arc::new(PublishedSemanticVectorReadPortV1 {
            generation: vector.clone(),
            projection_key: projection_key(),
            search_index_key: search_index_key().clone(),
            source_generation: source.clone(),
            capability_manifest_digest: capability.clone(),
            rows: Vec::new(),
            ann: PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported),
        });
        let cache = Mutex::new(Some(CachedPublishedVectorsV1 {
            generation: vector.clone(),
            search_index_key: search_index_key().clone(),
            source_generation: source.clone(),
            port: Arc::clone(&port),
        }));

        let retained = retained_vector_read_port(
            &cache,
            &vector,
            &projection_key(),
            search_index_key(),
            &source,
            &capability,
        )
        .expect("exact retained vector port");

        assert!(Arc::ptr_eq(&retained, &port));
    }

    fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
        SemanticGenerationPointerV1 {
            generation: vector_generation(vector),
            source_generation: source_generation(source),
            projection_key: projection_key(),
        }
    }

    fn configuration_pin() -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1 {
            revision_id: ConfigurationRevisionId::try_from(
                "configuration.revision.semantic-test".to_owned(),
            )
            .expect("configuration revision"),
            snapshot_id: ConfigurationSnapshotId::try_from(
                "configuration.snapshot.semantic-test".to_owned(),
            )
            .expect("configuration snapshot"),
            effective_behavior_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("configuration digest"),
        }
    }

    fn composition_request<'a>(
        query_view: &'a EphemeralSanitizedQueryViewV1,
        projection: &'a AdmittedEmbeddingProjectionKeyV1,
        source: CodeGenerationId,
        vector: VectorGenerationIdV1,
    ) -> SemanticRetrievalRequestV1<'a> {
        let query_digest = QueryDigest::new(
            projection.privacy_domain().clone(),
            projection.privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "33".repeat(32))).expect("query MAC"),
        );
        let budget = tracedecay_domain::RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 16,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        };
        SemanticRetrievalRequestV1 {
            base: RetrievalRequest {
                principal: PrincipalId::try_from("principal.fixture".to_owned())
                    .expect("principal"),
                scope: RetrievalScope {
                    privacy_domain: projection.privacy_domain().clone(),
                    root: SingleRootScopeV1 {
                        repository: RepositoryId::try_from("repository.fixture".to_owned())
                            .expect("repository"),
                        worktree: None,
                        reference: None,
                    },
                },
                temporal_mode: TemporalModeV1::Current,
                snapshot: RetrievalSnapshot {
                    watermarks: VectorWatermark::default(),
                    freshness_digest: tracedecay_domain::FreshnessVectorDigest::try_from(format!(
                        "sha256:{}",
                        "a".repeat(64)
                    ))
                    .expect("freshness"),
                    authorization_revision: AuthorizationRevision::try_from(
                        "authorization.v1".to_owned(),
                    )
                    .expect("authorization"),
                    captured_at: UtcMicros(1),
                },
                profile_id: FusionProfileId::try_from("profile.semantic.v1".to_owned())
                    .expect("profile"),
                budget,
            },
            query_digest,
            query_view,
            projection,
            search_index_key: search_index_key(),
            capability_manifest_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("capability"),
            vector_generation: vector,
            code_generation: source,
            budget,
        }
    }

    fn composition_fallback() -> Arc<QueryFallbackSubpayload> {
        let mut fallback = QueryFallbackSubpayload {
            profile_id: FusionProfileId::try_from("profile.query.semantic-contract.v1".to_owned())
                .expect("profile"),
            ordered_candidates: Vec::new(),
            public_fallback_lane_coverage: BTreeMap::from([
                (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
                (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
                (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
            ]),
            freshness: Vec::new(),
            cursor: None,
            digest: FallbackSubpayloadDigest::new(format!("sha256:{}", "0".repeat(64)))
                .unwrap_or_else(|_| panic!("digest")),
        };
        fallback.digest = fallback.compute_digest().expect("fallback digest");
        Arc::new(fallback)
    }

    #[cfg(feature = "semantic-fastembed")]
    fn composition_calibration(
        request: &SemanticRetrievalRequestV1<'_>,
    ) -> SemanticCalibrationProfileV1 {
        SemanticCalibrationProfileV1 {
            calibration_profile_id: CalibrationProfileId::try_from(
                "calibration.semantic.fixture.v1".to_owned(),
            )
            .expect("calibration profile"),
            cohort_digest: ManifestDigest::new(format!("sha256:{}", "7".repeat(64)))
                .expect("cohort digest"),
            projection_key: request.projection.projection_key().clone(),
            vector_generation: request.vector_generation.clone(),
            capability_manifest_digest: request.capability_manifest_digest.clone(),
            maximum_distance_micros: i64::MAX,
            minimum_margin_micros: 0,
        }
    }

    fn projection_request(source: char) -> ProjectionBatchRequestV1 {
        ProjectionBatchRequestV1 {
            request_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("request digest"),
            changes: ChangedCodeChunkSetV1 {
                from_generation: None,
                to_generation: source_generation(source),
                manifest_digest: ManifestDigest::new(format!("sha256:{}", "d".repeat(64)))
                    .expect("source manifest"),
                added_or_changed: Vec::new(),
                deleted: Vec::new(),
                reused: Vec::new(),
            },
            previous_projection_key: None,
            target_projection_key: projection_key(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        }
    }

    fn canonical_chunk(source: &CodeGenerationId, value: char) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("chunk.v1.{value}")).expect("chunk id"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: source.clone(),
                file_occurrence_id: FileOccurrenceId::new(format!("{value}.rs"))
                    .expect("file occurrence"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: ContentDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("content digest"),
            language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                .expect("language descriptor"),
            chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker revision"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer revision"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("policy revision"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
        }
    }

    #[test]
    fn symbol_backed_semantic_identity_dedupes_with_lexical_evidence() {
        let source = source_generation('a');
        let mut chunk = canonical_chunk(&source, 'a');
        let symbol = tracedecay_domain::SymbolOccurrenceId::new("symbol.fixture")
            .expect("symbol occurrence");
        chunk.anchor.symbol_occurrence_id = Some(symbol.clone());

        let (semantic_anchor, semantic_logical_evidence, source_occurrence) =
            semantic_candidate_identity(&chunk).expect("semantic identity");
        let lexical_evidence = format!("code-symbol:{}", symbol.as_str());
        let lexical_anchor =
            RetrievalAnchorId::new(lexical_evidence.clone()).expect("lexical anchor");
        let mixed_anchors = BTreeSet::from([semantic_anchor.clone(), lexical_anchor]);

        assert_eq!(mixed_anchors.len(), 1);
        assert_eq!(
            semantic_anchor,
            RetrievalAnchorId::new(lexical_evidence.clone()).expect("semantic anchor")
        );
        assert_eq!(
            semantic_logical_evidence,
            LogicalEvidenceId::new(lexical_evidence).expect("logical evidence")
        );
        assert_eq!(
            source_occurrence,
            SourceOccurrenceId::new(format!("code-chunk:{}", chunk.id.as_str()))
                .expect("source occurrence")
        );
    }

    #[test]
    fn compatible_generation_uses_projection_change_manifest_digest() {
        let request = projection_request('m');

        assert_eq!(
            semantic_source_manifest_digest(&request),
            &request.changes.manifest_digest
        );
        assert_ne!(
            semantic_source_manifest_digest(&request),
            &request.request_digest,
            "the projection request receipt is not the source manifest identity"
        );
    }

    #[test]
    fn evaluation_plan_uses_canonical_generation_chunk_order() {
        let source = source_generation('o');
        let alpha = canonical_chunk(&source, 'a');
        let beta = canonical_chunk(&source, 'b');
        let request = projection_request('o');

        let plan = evaluation_projection_plan_from_canonical_chunks(
            &[Arc::new(alpha.clone()), Arc::new(beta.clone())],
            &request,
            None,
        );

        assert_eq!(
            plan.expected_chunk_ids.as_slice(),
            &[alpha.id.clone(), beta.id.clone()]
        );
    }

    #[tokio::test]
    async fn saved_edit_schedules_fastembed_without_blocking_exact_search() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let exact_ready = AtomicBool::new(false);

        let request = FastEmbedSemanticGenerationRequestV1::new(
            source_generation('a'),
            projection_request('a'),
            Vec::new(),
            documents('a'),
            SEMANTIC_EMBEDS_PER_COMMIT,
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
            || async { Ok(SemanticProjectionResumeOutcomeV1::ReplayFromStart) },
            |_prepared| async { Ok(()) },
            move || async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
        )
        .expect("saved generation request");
        assert!(handle.schedule_generation(request));
        started_rx.await.expect("background schedule started");

        // Ordinary exact search proceeds while FastEmbed work is parked.
        exact_ready.store(true, Ordering::SeqCst);
        assert!(exact_ready.load(Ordering::SeqCst));
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ));
        release_tx.send(()).expect("release artifact loader");
    }

    #[tokio::test]
    async fn runtime_reports_semantic_indexing_progress() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            4,
            move |progress| async move {
                progress.set_completed_units(2);
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        started_rx.await.expect("indexing started");
        let projection = handle.status_projection();
        let status = application_status_from_projection(&projection, None, None);
        match status.state {
            SemanticRuntimeStateV1::Indexing {
                completed_units,
                total_units,
                ..
            } => {
                assert_eq!(completed_units, 2);
                assert_eq!(total_units, 4);
            }
            other => panic!("expected indexing status, got {other:?}"),
        }
        let _ = release_tx.send(());
    }

    #[tokio::test]
    async fn runtime_reports_degraded_reason_and_prior_generation() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let prior_pointer = pointer('a', 'a');
        let prior = prior_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(prior_pointer)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prior generation published");

        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_progress| async move { Err(SemanticRuntimeScheduleFailureV1::Artifact) },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if matches!(
                    handle.status(),
                    SemanticRuntimeScheduleStatusV1::Failed { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failure observed");

        let projection = handle.status_projection();
        assert_eq!(
            projection.degraded_reason,
            Some(SemanticFallbackReasonV1::ArtifactUnavailable)
        );
        assert_eq!(projection.prior_generation.as_ref(), Some(&prior));
        let status = application_status_from_projection(&projection, None, None);
        match status.state {
            SemanticRuntimeStateV1::Degraded {
                active_generation,
                reason,
            } => {
                assert_eq!(active_generation.as_ref(), Some(&prior));
                assert_eq!(reason, SemanticFallbackReasonV1::ArtifactUnavailable);
            }
            other => panic!("expected degraded status, got {other:?}"),
        }
        // Prior generation remains queryable / current for compatible reads.
        assert_eq!(
            handle.current().map(|pointer| pointer.generation),
            Some(prior)
        );
    }

    // Binding a query runtime requires the concrete FastEmbed runtime; the
    // compiled-out stub fails compatibility verification by design.
    #[cfg(feature = "semantic-fastembed")]
    #[tokio::test]
    async fn atomically_current_generation_enables_semantic_lane() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('c', 'c');
        let source = published.source_generation.clone();
        let vector = published.generation.clone();
        let projection_key = published.projection_key.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('c'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        // Pointer alone is insufficient — application search needs a bound query runtime.
        assert!(
            handle
                .query_factory(&source, &vector, &projection_key)
                .is_none(),
            "query_factory must stay closed until the query runtime is bound"
        );
        handle
            .bind_query_runtime_for_current(std::sync::Arc::new(
                tracedecay_semantic::session_pool::test_support::authority(),
            ))
            .expect("bind query runtime for current generation");

        assert!(
            handle
                .query_factory(&source, &vector, &projection_key)
                .is_some(),
            "exact warmed committed generation must enable query_factory"
        );
        assert!(
            handle
                .query_factory(&source_generation('x'), &vector, &projection_key)
                .is_none(),
            "incompatible source must not enable semantics"
        );
        let observed_pointer = SemanticGenerationPointerV1 {
            generation: vector.clone(),
            source_generation: source.clone(),
            projection_key: projection_key.clone(),
        };
        let exact_observation = handle
            .prepare_current_observation(&observed_pointer)
            .expect("prepare exact warmed-cache observation");
        let ready_publications = AtomicUsize::new(0);
        assert!(
            commit_current_observation_and_then(&handle, exact_observation, || {
                ready_publications.fetch_add(1, Ordering::SeqCst);
            }),
            "unchanged exact cache observation must commit"
        );
        assert_eq!(
            ready_publications.load(Ordering::SeqCst),
            1,
            "successful observation CAS must publish lifecycle readiness"
        );
        let stale_observation = handle
            .prepare_current_observation(&observed_pointer)
            .expect("prepare cache observation before concurrent unbind");
        assert!(handle.unbind_query_runtime_if_current(&vector));
        assert!(
            !commit_current_observation_and_then(&handle, stale_observation, || {
                ready_publications.fetch_add(1, Ordering::SeqCst);
            }),
            "cache observation must fail CAS after a concurrent transition"
        );
        assert_eq!(
            ready_publications.load(Ordering::SeqCst),
            1,
            "stale observation CAS must not publish false lifecycle readiness"
        );

        let backend = DaemonSemanticRuntimeBackendV1::new(handle.clone());
        let status = backend.application_status();
        assert!(matches!(
            status.route(),
            crate::semantic_runtime::SemanticRuntimeRouteV1::LexicalFallback { .. }
        ));
    }

    // Binding a query runtime requires the concrete FastEmbed runtime; the
    // compiled-out stub fails compatibility verification by design.
    #[cfg(feature = "semantic-fastembed")]
    #[tokio::test]
    async fn live_request_cancellation_reaches_query_runtime_before_vector_scan() {
        struct PanicVectors;

        impl SemanticVectorReadPort for PanicVectors {
            fn scan_exact_flat(
                &self,
                _request: SemanticVectorReadRequestV1<'_>,
                _examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
                _visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
            ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
                panic!("cancelled query runtime must not scan vectors")
            }
        }

        struct CancelAtRuntimeBoundary {
            checks: AtomicUsize,
        }

        impl SemanticExecutionControl for CancelAtRuntimeBoundary {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::SeqCst) != 0
            }

            fn elapsed_micros(&self) -> u64 {
                0
            }
        }

        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('q', 'q');
        let source = published.source_generation.clone();
        let vector = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source.clone(),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");
        let authority = tracedecay_semantic::session_pool::test_support::authority();
        handle
            .bind_query_runtime_for_current(Arc::new(authority.clone()))
            .expect("bind query runtime");

        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "cancel before session acquisition",
            SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
            QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
        )
        .expect("query view");
        let request = composition_request(&query_view, authority.projection(), source, vector);
        let complete = CompleteSemanticGenerationV1::new(
            request.projection.projection_key().clone(),
            request.search_index_key.clone(),
            request.vector_generation.clone(),
            request.code_generation.clone(),
            request.capability_manifest_digest.clone(),
        )
        .expect("complete generation");
        let calibration = composition_calibration(&request);
        let control = CancelAtRuntimeBoundary {
            checks: AtomicUsize::new(0),
        };

        let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &handle,
            request: &request,
            generation: &complete,
            calibration: Some(&calibration),
            vectors: &PanicVectors,
            control: &control,
            mode: SemanticQueryModeV1::FallbackAllowed,
            fallback: composition_fallback(),
        })
        .expect("cancelled semantic composition");

        assert!(matches!(
            outcome,
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention: tracedecay_query::retrieval::semantic::SemanticAbstentionV1::Cancelled,
                ..
            }
        ));
        assert!(
            control.checks.load(Ordering::SeqCst) >= 2,
            "query runtime must poll the same live request control"
        );
    }

    #[tokio::test]
    async fn current_scheduler_pointer_without_persisted_receipt_stays_degraded() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('r', 'r');
        let generation = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('r'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        let status = application_status_from_projection(
            &handle.status_projection(),
            Some(configuration_pin()),
            None,
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let restarted_status = application_status_from_projection(
            &handle.status_projection(),
            Some(configuration_pin()),
            None,
        );
        assert_eq!(
            status.state,
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation),
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
            }
        );
        assert_eq!(
            restarted_status, status,
            "status must not synthesize a time-varying activation receipt"
        );
    }

    #[tokio::test]
    async fn remounted_scheduler_current_reattaches_the_durable_ready_receipt() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('s', 's');
        let generation = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('s'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        let pin = configuration_pin();
        let receipt = SemanticActivationReceiptV1::issue(
            &SemanticActivationCommandV1::new(
                pin.clone(),
                SemanticActivationRequestV1::new(generation.clone(), None, None)
                    .expect("activation request"),
            )
            .expect("activation command"),
            UtcMicros(10),
        )
        .expect("durable activation receipt");

        let remounted = application_status_from_projection(
            &handle.status_projection(),
            Some(pin.clone()),
            Some(receipt.clone()),
        );
        assert_eq!(
            remounted.state,
            SemanticRuntimeStateV1::Current {
                receipt: receipt.clone()
            },
            "restart remount must reattach the durable Ready receipt"
        );
        remounted.validate().expect("ready runtime status");
        assert_eq!(
            remounted.route(),
            crate::semantic_runtime::SemanticRuntimeRouteV1::Semantic {
                generation,
                activation_receipt_digest: receipt.receipt_digest,
            }
        );

        let foreign = pointer('t', 't').generation;
        let mismatched = SemanticActivationReceiptV1::issue(
            &SemanticActivationCommandV1::new(
                pin.clone(),
                SemanticActivationRequestV1::new(foreign, None, None).expect("foreign request"),
            )
            .expect("foreign command"),
            UtcMicros(11),
        )
        .expect("foreign receipt");
        let refused = application_status_from_projection(
            &handle.status_projection(),
            Some(pin),
            Some(mismatched),
        );
        assert!(
            matches!(
                refused.state,
                SemanticRuntimeStateV1::Degraded {
                    reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
                    ..
                }
            ),
            "a receipt for a different generation must stay a typed missing-Ready state"
        );
    }

    #[tokio::test]
    async fn activation_without_persisted_scheduler_receipt_is_unavailable() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('a', 'a');
        let generation = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        let pin = configuration_pin();
        let command = SemanticActivationCommandV1::new(
            pin,
            SemanticActivationRequestV1::new(generation, None, None).expect("activation request"),
        )
        .expect("activation command");
        let backend = DaemonSemanticRuntimeBackendV1::new(handle);

        assert_eq!(
            backend.activate(&command).await,
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        );
    }

    #[tokio::test]
    async fn compose_application_search_skips_retriever_while_indexing() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('i'),
            2,
            move |progress| async move {
                progress.set_completed_units(1);
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        started_rx.await.expect("indexing started");

        struct PanicVectors;
        impl SemanticVectorReadPort for PanicVectors {
            fn scan_exact_flat(
                &self,
                _request: tracedecay_query::retrieval::semantic::SemanticVectorReadRequestV1<'_>,
                _examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
                _visit: &mut dyn FnMut(
                    &tracedecay_query::retrieval::semantic::SemanticVectorRecordV1,
                ) -> Result<(), RetrievalPortError>,
            ) -> Result<
                tracedecay_query::retrieval::semantic::SemanticVectorScanSummaryV1,
                RetrievalPortError,
            > {
                panic!("indexing composition must not scan vectors")
            }
        }
        struct IdleControl;
        impl SemanticExecutionControl for IdleControl {
            fn is_cancelled(&self) -> bool {
                false
            }
            fn elapsed_micros(&self) -> u64 {
                0
            }
        }

        let authority = tracedecay_semantic::session_pool::test_support::authority();
        let source = source_generation('i');
        let vector = vector_generation('i');
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "compose while indexing",
            SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
            QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
        )
        .expect("query view");
        let request = composition_request(&query_view, authority.projection(), source, vector);
        let complete = CompleteSemanticGenerationV1::new(
            request.projection.projection_key().clone(),
            request.search_index_key.clone(),
            request.vector_generation.clone(),
            request.code_generation.clone(),
            request.capability_manifest_digest.clone(),
        )
        .expect("complete generation");

        let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &handle,
            request: &request,
            generation: &complete,
            calibration: None,
            vectors: &PanicVectors,
            control: &IdleControl,
            mode: SemanticQueryModeV1::FallbackAllowed,
            fallback: composition_fallback(),
        })
        .expect("compose while indexing");
        assert!(matches!(
            outcome,
            SemanticQueryServiceOutcomeV1::Fallback { .. }
        ));
        let _ = release_tx.send(());
    }
}
