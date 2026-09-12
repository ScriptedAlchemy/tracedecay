//! Production bridge between daemon semantic scheduling and application search.
//!
//! Saved code generations call [`schedule_saved_code_generation`] without waiting
//! for `FastEmbed` download/indexing. Application search admits a semantic lane
//! only through [`query_factory`] once the committed configuration's complete
//! generation is present in the exact warmed cache. Status projection carries indexing progress, degraded
//! reason, and prior generation for Doctor/`tracedecay_runtime`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracedecay_code_index::embedding_document::{
    EmbeddingDocumentComposerV1, EmbeddingSymbolContextIndexV1,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, EmbeddingDocumentCompositionV1, ManifestDigest,
    SemanticSearchIndexKeyV1, VectorGenerationIdV1, canonical_sha256, sha256_hex_suffix,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::db::Database;
#[cfg(test)]
use tracedecay_semantic::SemanticExecutionInterruptionV1;
use tracedecay_semantic::{
    DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticRuntimeCommitV1, PreparedSemanticRuntimeObservationV1,
    PreparedSemanticRuntimeRestoreV1, SemanticEvaluationCancellationV1,
    SemanticModelLifecycleEvaluationPublicationLeaseV1, SemanticModelLifecycleOwnerV1,
    SemanticModelLifecyclePublicationIdentityV1, SemanticProjectionResumeOutcomeV1,
};
#[cfg(test)]
use tracedecay_semantic_contracts::SemanticFallbackReasonV1;
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticLifecycleVerifiedReadyEventV1,
    SemanticModelLifecycleStatusV1, SemanticResourceCeilings, SemanticRuntimeScheduleFailureV1,
};

use super::acceptance_calibration::{
    UNCALIBRATED_MAXIMUM_DISTANCE_MICROS, measure_acceptance_calibration,
};
use super::graph_provider::SemanticVectorGraphProviderV1;
#[cfg(test)]
use super::ports::SemanticActivationRequestV1;
use super::ports::{
    SemanticExecutableGenerationLeaseV1, SemanticExecutableGenerationV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1,
    SemanticRuntimeRefusalV1,
};
use crate::store::vector_generations::{GraphVectorGenerationStoreV1, PublishedVectorGenerationV1};

mod application_status;
mod daemon_backend;
mod evaluation;
mod evaluation_generation;
mod evaluation_support;
mod project_registry;
mod publication_failure;
mod published_vector_read;
mod saved_generation_schedule;
mod scheduling;
mod search;
mod search_composition;
mod source_coherence;
#[cfg(test)]
mod tests;
mod vector_projection_support;

pub use application_status::{
    lifecycle_to_runtime_state, prefer_lifecycle_over_generic_unavailable,
    resolve_semantic_application_status,
};
pub use evaluation_generation::PreparedSemanticEvaluationGenerationV1;
use evaluation_support::{
    accepted_semantic_resources, configured_resource_ceiling_covers,
    installed_artifact_member_bytes,
};
pub use project_registry::{
    RetiredProjectSemanticRuntimeV1, project_lifecycle_status, project_semantic_application_status,
    project_semantic_production_runtime, project_semantic_source_generation,
    register_project_semantic_runtime, resolve_project_semantic_runtime_status,
    unbind_project_semantic_cache_if_current, unregister_project_semantic_runtime,
};
use published_vector_read::PublishedSemanticVectorReadPortV1;
pub use saved_generation_schedule::{
    SavedCodeGenerationScheduleHookV1, SavedGenerationScheduleHookParametersV1,
    SavedGenerationScheduleOutcomeV1, production_saved_generation_schedule_hook,
};
pub use search_composition::{
    ApplicationSemanticSearchParametersV1, AuthorizedProjectSemanticSearchParametersV1,
    ProductionProjectSemanticSearchBridgeV1, compose_project_application_semantic_search,
};
pub use source_coherence::{
    SemanticSourceCoherenceOutcomeV1, SemanticSourceCoherenceV1, SemanticSourceMismatchV1,
    SemanticSourceUnavailableV1, semantic_source_coherence, semantic_source_content_coherent,
};
use vector_projection_support::BatchCommitStateV1;

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

pub struct PreparedProductionSemanticRuntimeCommitV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    prepared: PreparedProductionSemanticRuntimeActionV1,
}

enum PreparedProductionSemanticRuntimeActionV1 {
    Observation {
        prepared: Box<PreparedSemanticRuntimeObservationV1>,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    },
    Restore {
        prepared: Box<PreparedSemanticRuntimeRestoreV1>,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    },
}

impl PreparedProductionSemanticRuntimeCommitV1 {
    pub fn commit(self) -> bool {
        match self.prepared {
            PreparedProductionSemanticRuntimeActionV1::Observation {
                prepared,
                lifecycle,
            } => commit_current_observation_and_then(&self.handle, *prepared, || {
                let _ = lifecycle.mark_ready();
            }),
            PreparedProductionSemanticRuntimeActionV1::Restore {
                prepared,
                lifecycle,
            } => {
                let committed = self.handle.commit_restore(*prepared);
                if !committed {
                    return false;
                }
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
        generation: &CodeGenerationManifestV1,
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
        generation: &CodeGenerationManifestV1,
        required_generation: &VectorGenerationIdV1,
    ) -> Result<Option<PreparedProductionSemanticRuntimeCommitV1>, SemanticRuntimeScheduleFailureV1>
    {
        // Every step below answers a failure with the same `Publication`
        // category, and this is the stage a rollback's activation is installed
        // through. A bare category leaves an operator unable to tell a missing
        // graph from a retired generation from an unreadable index, so keep the
        // step and the store's own reason.
        let retained = self.graph.graph_for_current().await.map_err(|error| {
            SemanticRuntimeScheduleFailureV1::publication(format!("restore.retain_graph: {error}"))
        })?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = match GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            required_generation,
        )
        .await
        .map_err(|error| {
            SemanticRuntimeScheduleFailureV1::publication(format!(
                "restore.published_generation: {error}"
            ))
        })? {
            Some(store) => store,
            None => return Ok(None),
        };
        let projection = LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation,
            self.resources,
            self.document_composition,
        )?;
        let active = store
            .generation(required_generation, Arc::clone(&cancellation))
            .await
            .map_err(|error| {
                SemanticRuntimeScheduleFailureV1::publication(format!(
                    "restore.cataloged_generation: {error}"
                ))
            })?;
        let Some(active) = active else {
            return Ok(None);
        };
        // Restore binds the runtime pointer to the generation queries will
        // actually pin: the supplied (serving) publication. Whether these
        // vectors may attach to it is `semantic_source_coherence`'s question
        // alone, on either arm.
        if active.generation_id() != required_generation
            || active.embedding_key() != &projection
            || !semantic_source_content_coherent(&active, generation)
        {
            return Ok(None);
        }
        let pointer = SemanticGenerationPointerV1 {
            generation: active.generation_id().clone(),
            source_generation: generation.generation_id.clone(),
            projection_key: active.projection_key().clone(),
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let manifest = generation.clone();
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
        Ok(Some(PreparedProductionSemanticRuntimeCommitV1 {
            handle,
            prepared: PreparedProductionSemanticRuntimeActionV1::Restore {
                prepared: Box::new(prepared),
                lifecycle: Arc::clone(&self.lifecycle),
            },
        }))
    }

    pub fn prepare_current_runtime_observation(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
    ) -> Option<PreparedProductionSemanticRuntimeCommitV1> {
        let pointer = SemanticGenerationPointerV1 {
            generation: pins.vector_generation_id.clone(),
            source_generation: source_generation.clone(),
            projection_key: pins.projection.projection_key().clone(),
        };
        let prepared = self.handle.prepare_current_observation(&pointer)?;
        Some(PreparedProductionSemanticRuntimeCommitV1 {
            handle: self.handle.clone(),
            prepared: PreparedProductionSemanticRuntimeActionV1::Observation {
                prepared: Box::new(prepared),
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
                .await
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
                .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        if store
            .verified_revision(Arc::clone(retained.cancellation()))
            .await
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
        let store = GraphVectorGenerationStoreV1::read_only_generation(&retained, generation_id)
            .await
            .ok()??;
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

    pub fn runtime_ready_for(
        &self,
        pins: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
    ) -> bool {
        self.handle
            .query_factory(
                source_generation,
                &pins.vector_generation_id,
                pins.projection.projection_key(),
            )
            .is_some()
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
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .ok_or(SemanticRuntimeBackendErrorV1::RejectedAt(
                SemanticRuntimeRefusalV1::at("inspect_generation.unpublished_generation"),
            ))?;
            let generation = store
                .generation(&required.vector_generation_id, cancellation)
                .await
                .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
                .ok_or(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.uncataloged_generation"),
                ))?;
            if !configured_resource_ceiling_covers(&self.resources, required.resources) {
                return Err(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.resource_ceiling"),
                ));
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
                return Err(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.artifact_member_bytes"),
                ));
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
            .map_err(|_| {
                SemanticRuntimeBackendErrorV1::RejectedAt(SemanticRuntimeRefusalV1::at(
                    "inspect_generation.load_artifact",
                ))
            })?;
            if verified.projection() != generation.embedding_key()
                || required.projection != *generation.embedding_key()
                || required.implementation_revision.as_str() != "semantic.fastembed.production.v1"
            {
                return Err(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.projection_identity"),
                ));
            }
            let lifecycle = self.lifecycle.status();
            let state = lifecycle
                .state
                .ok_or(SemanticRuntimeBackendErrorV1::Unavailable)?;
            let artifact_digest = state.artifact_digest();
            let expected_artifact = required.artifact_manifest_digest.as_str();
            if artifact_digest != expected_artifact
                && sha256_hex_suffix(expected_artifact) != Some(artifact_digest)
            {
                return Err(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.artifact_digest"),
                ));
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
            .map_err(|_| {
                SemanticRuntimeBackendErrorV1::RejectedAt(SemanticRuntimeRefusalV1::at(
                    "inspect_generation.runtime_compatibility_digest",
                ))
            })?;
            if required.runtime_compatibility_digest != expected_runtime_digest {
                return Err(SemanticRuntimeBackendErrorV1::RejectedAt(
                    SemanticRuntimeRefusalV1::at("inspect_generation.runtime_compatibility"),
                ));
            }
            let evidence = SemanticExecutableGenerationV1::new(
                required.clone(),
                required.resources,
                true,
                true,
            )
            .map_err(|cause| {
                SemanticRuntimeBackendErrorV1::RejectedAt(SemanticRuntimeRefusalV1::contract(
                    "inspect_generation.executable_evidence",
                    cause,
                ))
            })?;
            Ok(SemanticExecutableGenerationLeaseV1::new(
                evidence,
                (store, retained),
            ))
        })
    }
}
