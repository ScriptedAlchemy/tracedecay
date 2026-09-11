//! Semantic code runtime: artifact store, model lifecycle, embedding
//! projection, session pooling, and the daemon-callable scheduling handle.
//!
//! The crate owns the semantic implementation outright, including user-data-dir
//! lifecycle-root discovery via `tracedecay_runtime_core::config::user_data_dir`.
//! Application/Doctor status projection stays in `tracedecay-application`. Shared
//! configuration, artifact, lifecycle, and runtime-status contracts are
//! owned by `tracedecay-semantic-contracts`.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use tracedecay_code_index::embedding_document::{
    EmbeddingDocumentComposerV1, EmbeddingDocumentHeaderV1,
};
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkV1, EmbeddingDocumentCompositionV1, ProjectionBatchRequestV1,
    ProjectionKeyV1, VectorGenerationIdV1,
};
use tracedecay_semantic_contracts::configuration::{
    SemanticFallbackReasonV1, SemanticResourceCeilings,
};
use tracedecay_semantic_contracts::lifecycle::SemanticModelLifecycleStateV1;
use tracedecay_semantic_contracts::runtime_status::{
    SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

use self::embedding_backend::{ProductionEmbeddingRuntime, production_embedding_runtime_factory};
#[cfg(any(test, feature = "test-helpers"))]
pub use self::fastembed_adapter::AdmittedProjectionArtifactV1;
#[cfg(not(any(test, feature = "test-helpers")))]
use self::fastembed_adapter::AdmittedProjectionArtifactV1;
use self::fastembed_adapter::{
    BoundedSanitizedTextBatchV1, EmbedError, EmbeddingRuntime, EmbeddingSession,
};
use self::projector::{
    CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, prepare_vector_generation_async,
    split_projection_request,
};
use self::runtime_query::CurrentSemanticQueryRuntimeV1;
use self::runtime_service::{SemanticRuntimeService, SharedEmbeddingRuntimeFactory};
use self::session_pool::{
    PooledSession, SessionAcquireError, SessionPoolConfigV1, SystemMonotonicClock,
};

mod artifact_store;
mod embedding_backend;
// Paired with the `AdmittedProjectionArtifactV1` test-helper export: its
// `runtime_family()` echo names this type.
#[cfg(any(test, feature = "test-helpers"))]
pub use embedding_backend::EmbeddingRuntimeFamilyV1;
pub mod embedding_parallelism;
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
mod execution_provider;
mod fastembed_adapter;
pub use fastembed_adapter::{SemanticExecutionAuthority, SemanticExecutionInterruptionV1};
mod generation_resume;
mod hotpath_observe;
pub use generation_resume::SemanticProjectionResumeOutcomeV1;
use generation_resume::SemanticProjectionResumeV1;
use generation_resume::{completed_batch_offset, install_candidate_on_success};
mod model2vec_adapter;
mod model_catalog;
mod model_lifecycle;
pub mod projector;
pub mod rerank_adapter;
mod runtime_query;
mod runtime_service;
mod semantic_evaluation;
pub mod session_pool;
pub use model_catalog::{CatalogErrorV1, admit_production_model_selection};
// Test-support constructors. Dependent crates opt in through `test-helpers`
// exactly like the query kernel's `*_for_test` surface.
#[cfg(any(test, feature = "test-helpers"))]
pub use model_catalog::production_fastembed_catalog;
#[cfg(any(test, feature = "test-helpers"))]
pub use model_catalog::{
    CatalogedEmbeddingBackendV1, CatalogedFastEmbedModelV1, FastEmbedModelCatalogV1,
};
#[cfg(not(any(test, feature = "test-helpers")))]
use model_catalog::{CatalogedFastEmbedModelV1, FastEmbedModelCatalogV1};
#[cfg(any(test, feature = "test-helpers"))]
pub use model_lifecycle::ModelMemberSourceV1;
pub use model_lifecycle::{
    ModelLifecycleErrorV1, SemanticModelLifecycleEvaluationPublicationLeaseV1,
    SemanticModelLifecycleOwnerV1, SemanticModelLifecyclePublicationIdentityV1,
    open_local_semantic_evaluation_lifecycle,
};

pub use runtime_service::{
    PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleCancellationV1,
    SemanticRuntimeSchedulingHandleV1, SemanticRuntimeShutdownReceiptV1, SemanticRuntimeWorkV1,
};
pub use semantic_evaluation::{
    PreparedSemanticEvaluationProjectionV1, SemanticEvaluationCancellationV1,
    SemanticEvaluationProjectionBatchCacheMemoryV1, SemanticEvaluationProjectionBatchCachePolicyV1,
    SemanticEvaluationProjectionBatchCacheV1, SemanticEvaluationProjectionBatchStoreV1,
    SemanticEvaluationProjectionCancellationV1, SemanticEvaluationProjectionResourcesV1,
    SemanticEvaluationQueryEmbedderV1, SemanticEvaluationQueryFactoryV1,
    measure_semantic_evaluation_projection_cancellation, prepare_semantic_evaluation_projection,
};

/// Resolve the lifecycle store root beneath a caller-supplied user data
/// directory.
pub fn default_lifecycle_root_in(user_data_dir: &Path) -> PathBuf {
    user_data_dir.join("semantic-models")
}

/// Resolve the lifecycle store root under the process user data directory.
pub fn default_lifecycle_root() -> Option<PathBuf> {
    tracedecay_runtime_core::config::user_data_dir().map(|root| default_lifecycle_root_in(&root))
}

type SemanticProjectionStageFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    >,
>;
type SemanticProjectionCommitFutureV1 =
    Pin<Box<dyn Future<Output = Result<(), SemanticRuntimeScheduleFailureV1>> + Send + 'static>>;
/// Durably commits one prepared batch. Called once per batch, in order, and
/// the batch's vectors are dropped as soon as it returns.
type SemanticProjectionCommitV1 =
    Box<dyn FnMut(PreparedVectorGenerationV1) -> SemanticProjectionCommitFutureV1 + Send + 'static>;
/// Seals the staged build once every batch has committed.
type SemanticProjectionStageV1 =
    Box<dyn FnOnce() -> SemanticProjectionStageFutureV1 + Send + 'static>;
type FastEmbedArtifactLoaderV1 = Box<
    dyn FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
>;

pub struct LoadedSemanticArtifactV1(Arc<AdmittedProjectionArtifactV1>);

/// The lifecycle facts a loadable semantic artifact is built from: the
/// cataloged model and the verified install it was published from.
///
/// This is the single authority for which lifecycle states are loadable.
/// Exactly `Installed | Loading | Indexing | Ready` carry verified bytes on
/// disk; `SelectedNotDownloaded`, `Downloading`, and `Verifying` do not yet,
/// and `Failed` never does even though it retains model id, revision, and
/// artifact digest. Every other state — and an absent state — is the same
/// typed artifact refusal, so the three `LoadedSemanticArtifactV1` paths
/// cannot drift apart on admissibility or catalog lookup.
struct LoadableLifecycleArtifactV1<'a> {
    model: &'a CatalogedFastEmbedModelV1,
    install_path: PathBuf,
}

impl<'a> LoadableLifecycleArtifactV1<'a> {
    fn resolve(
        lifecycle: &'a SemanticModelLifecycleOwnerV1,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let artifact = Self::from_state(lifecycle.status().state, lifecycle.catalog())?;
        if !artifact.model.backend.runtime_family().is_compiled() {
            return Err(SemanticRuntimeScheduleFailureV1::Runtime);
        }
        Ok(artifact)
    }

    fn from_state(
        state: Option<SemanticModelLifecycleStateV1>,
        catalog: &'a FastEmbedModelCatalogV1,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let (model_id, install_path) = match state {
            Some(
                SemanticModelLifecycleStateV1::Installed {
                    model_id,
                    install_path,
                    ..
                }
                | SemanticModelLifecycleStateV1::Loading {
                    model_id,
                    install_path,
                    ..
                }
                | SemanticModelLifecycleStateV1::Indexing {
                    model_id,
                    install_path,
                    ..
                }
                | SemanticModelLifecycleStateV1::Ready {
                    model_id,
                    install_path,
                    ..
                },
            ) => (model_id, install_path),
            Some(
                SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. }
                | SemanticModelLifecycleStateV1::Downloading { .. }
                | SemanticModelLifecycleStateV1::Verifying { .. }
                | SemanticModelLifecycleStateV1::Failed { .. },
            )
            | None => return Err(SemanticRuntimeScheduleFailureV1::Artifact),
        };
        let model = catalog
            .get(&model_id)
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        Ok(Self {
            model,
            install_path,
        })
    }
}

impl LoadedSemanticArtifactV1 {
    pub fn from_lifecycle(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        manifest: &tracedecay_domain::CodeGenerationManifestV1,
        resources: SemanticResourceCeilings,
        document_composition: EmbeddingDocumentCompositionV1,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let loadable = LoadableLifecycleArtifactV1::resolve(lifecycle)?;
        let authority = AdmittedProjectionArtifactV1::from_lifecycle_install(
            loadable.model,
            &loadable.install_path,
            manifest.chunker_revision.clone(),
            manifest.privacy_domain.clone(),
            manifest.privacy_key_epoch,
            resources,
            document_composition,
        )
        .map_err(SemanticRuntimeScheduleFailureV1::artifact)?;
        Ok(Self(Arc::new(authority)))
    }

    pub fn from_lifecycle_projection(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        projection: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
        resources: SemanticResourceCeilings,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let loadable = LoadableLifecycleArtifactV1::resolve(lifecycle)?;
        let key = projection.embedding_key();
        let authority = AdmittedProjectionArtifactV1::from_lifecycle_install(
            loadable.model,
            &loadable.install_path,
            key.chunker_revision.clone(),
            key.privacy_domain.clone(),
            key.privacy_key_epoch,
            resources,
            key.document_composition,
        )
        .map_err(SemanticRuntimeScheduleFailureV1::artifact)?;
        if authority.projection() != projection {
            return Err(SemanticRuntimeScheduleFailureV1::artifact(
                "loaded lifecycle artifact does not match the requested projection",
            ));
        }
        Ok(Self(Arc::new(authority)))
    }

    /// Projection identity for the loadable lifecycle model. Needs no install
    /// bytes, but the model must still be in a loadable state: identity is
    /// only meaningful for an artifact this owner could actually serve.
    pub fn lifecycle_projection(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        manifest: &tracedecay_domain::CodeGenerationManifestV1,
        resources: SemanticResourceCeilings,
        document_composition: EmbeddingDocumentCompositionV1,
    ) -> Result<tracedecay_domain::AdmittedEmbeddingProjectionKeyV1, SemanticRuntimeScheduleFailureV1>
    {
        let loadable = LoadableLifecycleArtifactV1::resolve(lifecycle)?;
        AdmittedProjectionArtifactV1::lifecycle_projection(
            loadable.model,
            manifest.chunker_revision.clone(),
            manifest.privacy_domain.clone(),
            manifest.privacy_key_epoch,
            resources,
            document_composition,
        )
        .map_err(SemanticRuntimeScheduleFailureV1::artifact)
    }

    pub fn projection(&self) -> &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        self.0.projection()
    }

    fn into_authority(self) -> Arc<AdmittedProjectionArtifactV1> {
        self.0
    }
}

/// Store-neutral input for asynchronously projecting one saved code generation.
pub struct FastEmbedSemanticGenerationRequestV1 {
    target_generation: CodeGenerationId,
    projection_request: ProjectionBatchRequestV1,
    canonical_chunks: Vec<Arc<CodeSearchChunkV1>>,
    /// Composes each chunk's tensor input from the target generation's own
    /// symbol index under the admitted key's composition.
    documents: Arc<EmbeddingDocumentComposerV1>,
    max_embeds_per_batch: usize,
    load_artifact: FastEmbedArtifactLoaderV1,
    resume_projection: SemanticProjectionResumeV1,
    commit_batch: SemanticProjectionCommitV1,
    stage_projection: SemanticProjectionStageV1,
}

impl FastEmbedSemanticGenerationRequestV1 {
    #[expect(
        clippy::too_many_arguments,
        reason = "each callback is a distinct store boundary of the incremental commit flow"
    )]
    pub fn new<
        LoadArtifact,
        ResumeProjection,
        ResumeFuture,
        CommitBatch,
        CommitFuture,
        StageProjection,
        StageFuture,
    >(
        target_generation: CodeGenerationId,
        projection_request: ProjectionBatchRequestV1,
        canonical_chunks: Vec<Arc<CodeSearchChunkV1>>,
        documents: Arc<EmbeddingDocumentComposerV1>,
        max_embeds_per_batch: usize,
        load_artifact: LoadArtifact,
        resume_projection: ResumeProjection,
        commit_batch: CommitBatch,
        stage_projection: StageProjection,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1>
    where
        LoadArtifact: FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
            + Send
            + 'static,
        ResumeProjection: FnOnce() -> ResumeFuture + Send + 'static,
        ResumeFuture: Future<
                Output = Result<
                    SemanticProjectionResumeOutcomeV1,
                    SemanticRuntimeScheduleFailureV1,
                >,
            > + Send
            + 'static,
        CommitBatch: FnMut(PreparedVectorGenerationV1) -> CommitFuture + Send + 'static,
        CommitFuture:
            Future<Output = Result<(), SemanticRuntimeScheduleFailureV1>> + Send + 'static,
        StageProjection: FnOnce() -> StageFuture + Send + 'static,
        StageFuture: Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    {
        if projection_request.changes.to_generation != target_generation
            || documents.symbols().generation_id() != &target_generation
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let mut commit_batch = commit_batch;
        Ok(Self {
            target_generation,
            projection_request,
            canonical_chunks,
            documents,
            max_embeds_per_batch,
            load_artifact: Box::new(load_artifact),
            resume_projection: Box::new(move || Box::pin(resume_projection())),
            commit_batch: Box::new(move |prepared| Box::pin(commit_batch(prepared))),
            stage_projection: Box::new(move || Box::pin(stage_projection())),
        })
    }
}

/// Map a pre-install warm failure onto the scheduler's typed failure set.
///
/// Cancellation and deadline expiry keep their identities: a cancelled or
/// timed-out warm is a cancelled or expired schedule — never a runtime
/// fault, and per the plan lock never a served generation. Everything else
/// (exhaustion, ceilings, open failures, a closed pool) is a runtime
/// failure.
fn warm_failure(error: SessionAcquireError) -> SemanticRuntimeScheduleFailureV1 {
    match error {
        SessionAcquireError::Cancelled | SessionAcquireError::Open(EmbedError::Cancelled) => {
            SemanticRuntimeScheduleFailureV1::Cancelled
        }
        SessionAcquireError::DeadlineExceeded { .. }
        | SessionAcquireError::LoadDeadlineExceeded { .. }
        | SessionAcquireError::Open(EmbedError::DeadlineExceeded) => {
            SemanticRuntimeScheduleFailureV1::DeadlineExceeded
        }
        SessionAcquireError::Exhausted { .. }
        | SessionAcquireError::QueueFull { .. }
        | SessionAcquireError::MemoryCeilingExceeded { .. }
        | SessionAcquireError::ResidentCeilingExceeded { .. }
        | SessionAcquireError::Open(_)
        | SessionAcquireError::Closed => SemanticRuntimeScheduleFailureV1::Runtime,
    }
}

/// Prove a candidate runtime can serve before its pointer is installed.
///
/// Opens (or reuses) one pooled session away from retrieval executor
/// threads. A cold open reads and digest-verifies every artifact member, so
/// a same-length digest-mismatched model fails here with a typed runtime
/// failure instead of ever becoming the current serving generation.
async fn warm_candidate_for_install(
    candidate: &Arc<SemanticRuntimeService<ProductionEmbeddingRuntime>>,
) -> Result<(), SemanticRuntimeScheduleFailureV1> {
    let warmed = Arc::clone(candidate);
    hotpath::future!(
        tokio::task::spawn_blocking(move || warmed.warm_query_session()),
        label = "semantic.index.warm"
    )
    .await
    .map_err(|error| {
        if error.is_cancelled() {
            SemanticRuntimeScheduleFailureV1::Cancelled
        } else {
            SemanticRuntimeScheduleFailureV1::Runtime
        }
    })?
    .map_err(warm_failure)
}

/// The daemon-callable semantic owner. It exposes no transport operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticRuntimeSchedulingBoundsV1 {
    pub max_sessions: usize,
    pub max_projection_units: u64,
    pub memory_ceiling_bytes: u64,
}

#[derive(Clone)]
pub struct DaemonSemanticRuntimeHandleV1 {
    scheduling: SemanticRuntimeSchedulingHandleV1,
    bounds: SemanticRuntimeSchedulingBoundsV1,
    runtime: Arc<RwLock<Option<CurrentSemanticQueryRuntimeV1<ProductionEmbeddingRuntime>>>>,
    query_in_flight: Arc<AtomicBool>,
    transitions: Arc<Mutex<()>>,
    pool_config: SessionPoolConfigV1,
}

pub struct PreparedSemanticRuntimeRestoreV1 {
    pointer: SemanticGenerationPointerV1,
    runtime: CurrentSemanticQueryRuntimeV1<ProductionEmbeddingRuntime>,
    expected_current: Option<SemanticGenerationPointerV1>,
    expected_status: SemanticRuntimeScheduleStatusV1,
}

pub struct PreparedSemanticRuntimeObservationV1 {
    pointer: SemanticGenerationPointerV1,
    expected_current: Option<SemanticGenerationPointerV1>,
    expected_status: SemanticRuntimeScheduleStatusV1,
}

impl DaemonSemanticRuntimeHandleV1 {
    fn restore_snapshot(
        &self,
    ) -> (
        Option<SemanticGenerationPointerV1>,
        SemanticRuntimeScheduleStatusV1,
    ) {
        (self.scheduling.current(), self.scheduling.status())
    }

    fn restore_snapshot_is_current(
        &self,
        expected_current: &Option<SemanticGenerationPointerV1>,
        expected_status: &SemanticRuntimeScheduleStatusV1,
    ) -> bool {
        self.scheduling.current().as_ref() == expected_current.as_ref()
            && &self.scheduling.status() == expected_status
    }

    pub fn new(
        max_sessions: usize,
        max_projection_units: usize,
        memory_ceiling_bytes: u64,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        if max_sessions == 0 || max_projection_units == 0 || memory_ceiling_bytes == 0 {
            return Err(SemanticRuntimeScheduleFailureV1::Runtime);
        }
        let pool_config = SessionPoolConfigV1 {
            max_sessions,
            max_queued_waiters: 0,
            idle_timeout: std::time::Duration::from_mins(5),
            memory_ceiling_bytes,
        };
        pool_config
            .validate()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        Ok(Self {
            scheduling: SemanticRuntimeSchedulingHandleV1::new(),
            bounds: SemanticRuntimeSchedulingBoundsV1 {
                max_sessions,
                max_projection_units: max_projection_units as u64,
                memory_ceiling_bytes,
            },
            runtime: Arc::new(RwLock::new(None)),
            query_in_flight: Arc::new(AtomicBool::new(false)),
            transitions: Arc::new(Mutex::new(())),
            pool_config,
        })
    }

    pub fn status(&self) -> SemanticRuntimeScheduleStatusV1 {
        self.scheduling.status()
    }

    pub fn schedule(&self, work: SemanticRuntimeWorkV1) -> bool {
        if work.total_units() > self.bounds.max_projection_units {
            return false;
        }
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.scheduling.schedule(work)
    }

    /// Schedule one saved generation without blocking exact, lexical, or graph
    /// search on artifact loading, model startup, projection, or publication.
    pub fn schedule_generation(&self, request: FastEmbedSemanticGenerationRequestV1) -> bool {
        let total_units = request
            .projection_request
            .changes
            .added_or_changed
            .len()
            .max(1) as u64;
        if request.canonical_chunks.len() > self.bounds.max_projection_units as usize
            || total_units > self.bounds.max_projection_units
        {
            return false;
        }

        let target_generation = request.target_generation.clone();
        let projection_key = request.projection_request.target_projection_key.clone();
        let pool_config = self.pool_config.clone();
        let runtime = Arc::clone(&self.runtime);
        let query_in_flight = Arc::clone(&self.query_in_flight);
        let work = SemanticRuntimeWorkV1::new_with_projection(
            request.target_generation,
            projection_key.clone(),
            total_units,
            move |progress| async move {
                let resume = hotpath::future!(
                    (request.resume_projection)(),
                    label = "semantic.index.resume"
                )
                .await?;
                let authority = hotpath::future!(
                    tokio::task::spawn_blocking(request.load_artifact),
                    label = "semantic.index.load_artifact"
                )
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??
                .0;
                if let Some(failure) = progress.failure() {
                    return Err(failure);
                }

                let factory: SharedEmbeddingRuntimeFactory<ProductionEmbeddingRuntime> =
                    production_embedding_runtime_factory();
                let candidate =
                    SemanticRuntimeService::new_owned(Arc::clone(&authority), factory, pool_config)
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
                if resume == SemanticProjectionResumeOutcomeV1::AlreadyPublished {
                    // Publication ≠ activation: a published projection still
                    // must prove one session opens over the loaded artifact
                    // bytes (read + digest verification) before its pointer
                    // can be installed as serving, exactly as
                    // `prepare_restore` warms before commit. A same-length
                    // digest-mismatched member therefore stays Failed and
                    // never becomes Current.
                    warm_candidate_for_install(&candidate).await?;
                    if let Some(failure) = progress.failure() {
                        return Err(failure);
                    }
                    progress.set_completed_units(total_units);
                    let commit = hotpath::future!(
                        (request.stage_projection)(),
                        label = "semantic.index.stage"
                    )
                    .await?;
                    return Ok(install_candidate_on_success(
                        commit,
                        target_generation,
                        projection_key,
                        runtime,
                        candidate,
                        query_in_flight,
                    ));
                }
                // Embed and commit batch by batch. A batch's vectors are
                // durable and released before the next batch is embedded, so
                // the live float set is bounded by one batch rather than by
                // the corpus, and a crash resumes from the last committed
                // checkpoint instead of re-embedding everything.
                let batches = split_projection_request(
                    &request.projection_request,
                    &request.canonical_chunks,
                    request.max_embeds_per_batch,
                    authority.projection().embedding_key().inference_batch_size as usize,
                    authority.projection().embedding_key().inference_batch_bytes as usize,
                )
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
                drop(request.canonical_chunks);
                let committed_batches = completed_batch_offset(resume, batches.len())?
                    .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
                let mut commit_batch = request.commit_batch;
                let mut embedded_units = batches
                    .iter()
                    .take(committed_batches)
                    .map(|batch| batch.request.changes.added_or_changed.len() as u64)
                    .sum::<u64>();
                progress.set_completed_units(embedded_units.min(total_units));
                for batch in batches.into_iter().skip(committed_batches) {
                    let encoder = RuntimeChunkVectorEncoderV1::new(
                        Arc::clone(&candidate),
                        Arc::clone(&progress),
                        authority.embedding_execution_plan(),
                        Arc::clone(&request.documents),
                    );
                    let batch_units = batch.request.changes.added_or_changed.len() as u64;
                    let prepared = prepare_vector_generation_async(
                        authority.projection().clone(),
                        batch.request,
                        batch.canonical_chunks,
                        encoder,
                    )
                    .await
                    .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
                    if let Some(failure) = progress.failure() {
                        return Err(failure);
                    }
                    hotpath::future!(
                        commit_batch(prepared),
                        label = "semantic.index.commit_batch"
                    )
                    .await?;
                    embedded_units = embedded_units.saturating_add(batch_units);
                    progress.set_completed_units(embedded_units.min(total_units));
                }
                if let Some(failure) = progress.failure() {
                    return Err(failure);
                }
                // A resume that reports every batch already committed embeds
                // nothing above, so no session has opened over these bytes.
                // After real embedding this reuses the idle pooled session
                // without re-reading the artifact.
                warm_candidate_for_install(&candidate).await?;
                if let Some(failure) = progress.failure() {
                    return Err(failure);
                }
                progress.set_completed_units(total_units);

                let commit =
                    hotpath::future!((request.stage_projection)(), label = "semantic.index.stage")
                        .await?;
                Ok(install_candidate_on_success(
                    commit,
                    target_generation,
                    projection_key,
                    runtime,
                    candidate,
                    query_in_flight,
                ))
            },
        );
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.scheduling.schedule(work)
    }

    pub fn bounds(&self) -> SemanticRuntimeSchedulingBoundsV1 {
        self.bounds
    }

    pub fn current(&self) -> Option<SemanticGenerationPointerV1> {
        self.scheduling.current()
    }

    pub fn cancel(&self) -> bool {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.scheduling.cancel()
    }

    /// Drop one exact process-local semantic cache without deleting its
    /// immutable graph generation. A concurrent newer pointer is preserved.
    pub fn unbind_query_runtime_if_current(
        &self,
        expected_generation: &VectorGenerationIdV1,
    ) -> bool {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(expected) = self.current() else {
            return false;
        };
        if &expected.generation != expected_generation
            || !self.scheduling.clear_current_if(&expected)
        {
            return false;
        }
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        true
    }

    pub fn begin_shutdown(&self) -> bool {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.scheduling.begin_shutdown()
    }

    pub async fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> SemanticRuntimeShutdownReceiptV1 {
        self.begin_shutdown();
        self.scheduling.cancel_and_join_until(deadline).await
    }

    pub fn query_factory(
        &self,
        source_generation: &CodeGenerationId,
        vector_generation: &VectorGenerationIdV1,
        projection_key: &ProjectionKeyV1,
    ) -> Option<SemanticEvaluationQueryFactoryV1> {
        let current = self.current()?;
        if current.source_generation != *source_generation
            || current.generation != *vector_generation
            || current.projection_key != *projection_key
        {
            return None;
        }
        let inner = self
            .runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()?
            .factory_for(source_generation, vector_generation, projection_key)?;
        Some(SemanticEvaluationQueryFactoryV1::from_runtime(inner))
    }

    /// Query-embedder admission for a caller that has already proven exact
    /// source-content coherence between its pinned vector generation and the
    /// code generation it serves (see
    /// `semantic_source_content_coherent` in `tracedecay-application`).
    ///
    /// Generation identifiers name physical publications; a warmed query
    /// embedder is physically identified by its projection key alone. Callers
    /// without a content proof must use [`Self::query_factory`], which pins the
    /// scheduler's exact current pointer.
    pub fn query_factory_for_projection(
        &self,
        projection_key: &ProjectionKeyV1,
    ) -> Option<SemanticEvaluationQueryFactoryV1> {
        let current = self.current()?;
        if current.projection_key != *projection_key {
            return None;
        }
        let inner = self
            .runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()?
            .factory_for_projection(projection_key)?;
        Some(SemanticEvaluationQueryFactoryV1::from_runtime(inner))
    }

    /// Test-only binding for a pointer published without a production runtime.
    ///
    /// Requires `semantic-fastembed`: the handle's query runtime is concretely
    /// the FastEmbed runtime, and without that feature the compiled-out stub
    /// fails compatibility verification by design, so no binding can exist.
    #[cfg(all(
        any(test, feature = "test-helpers"),
        feature = "semantic-fastembed",
        not(windows)
    ))]
    pub fn bind_query_runtime_for_current(
        &self,
        authority: Arc<AdmittedProjectionArtifactV1>,
    ) -> Result<(), SemanticRuntimeScheduleFailureV1> {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pointer = self
            .current()
            .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
        if authority.projection().projection_key() != &pointer.projection_key {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        let factory: SharedEmbeddingRuntimeFactory<ProductionEmbeddingRuntime> =
            production_embedding_runtime_factory();
        let candidate =
            SemanticRuntimeService::new_owned(authority, factory, self.pool_config.clone())
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(CurrentSemanticQueryRuntimeV1::new_with_admission(
                pointer,
                candidate,
                Arc::clone(&self.query_in_flight),
            ));
        Ok(())
    }

    #[hotpath::measure(label = "semantic.restart.restore")]
    pub fn restore_current(
        &self,
        pointer: SemanticGenerationPointerV1,
        artifact: LoadedSemanticArtifactV1,
    ) -> Result<(), SemanticRuntimeScheduleFailureV1> {
        let prepared = self.prepare_restore(pointer, artifact)?;
        self.commit_restore_if_current(prepared)
            .then_some(())
            .ok_or(SemanticRuntimeScheduleFailureV1::Publication)
    }

    #[hotpath::measure(label = "semantic.restart.prepare")]
    pub fn prepare_restore(
        &self,
        pointer: SemanticGenerationPointerV1,
        artifact: LoadedSemanticArtifactV1,
    ) -> Result<PreparedSemanticRuntimeRestoreV1, SemanticRuntimeScheduleFailureV1> {
        let (expected_current, expected_status) = {
            let _transition = self
                .transitions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.restore_snapshot()
        };
        if matches!(
            &expected_status,
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ) {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        let authority = artifact.into_authority();
        if authority.projection().projection_key() != &pointer.projection_key {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        let factory: SharedEmbeddingRuntimeFactory<ProductionEmbeddingRuntime> =
            production_embedding_runtime_factory();
        let candidate =
            SemanticRuntimeService::new_owned(authority, factory, self.pool_config.clone())
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        hotpath::measure_block!("semantic.restart.warm", candidate.warm_query_session())
            .map_err(warm_failure)?;
        Ok(PreparedSemanticRuntimeRestoreV1 {
            runtime: CurrentSemanticQueryRuntimeV1::new_with_admission(
                pointer.clone(),
                candidate,
                Arc::clone(&self.query_in_flight),
            ),
            pointer,
            expected_current,
            expected_status,
        })
    }

    /// Publish a warmed restore only if no scheduler transition occurred while
    /// it was prepared. This prevents restart/rollback work from cancelling or
    /// overwriting a newer generation.
    pub fn commit_restore(&self, prepared: PreparedSemanticRuntimeRestoreV1) -> bool {
        self.commit_restore_if_current(prepared)
    }

    pub fn prepare_current_observation(
        &self,
        pointer: &SemanticGenerationPointerV1,
    ) -> Option<PreparedSemanticRuntimeObservationV1> {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (expected_current, expected_status) = self.restore_snapshot();
        if expected_current.as_ref() != Some(pointer)
            || self
                .query_factory(
                    &pointer.source_generation,
                    &pointer.generation,
                    &pointer.projection_key,
                )
                .is_none()
        {
            return None;
        }
        Some(PreparedSemanticRuntimeObservationV1 {
            pointer: pointer.clone(),
            expected_current,
            expected_status,
        })
    }

    pub fn commit_current_observation(
        &self,
        prepared: PreparedSemanticRuntimeObservationV1,
    ) -> bool {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.restore_snapshot_is_current(&prepared.expected_current, &prepared.expected_status)
            && self
                .query_factory(
                    &prepared.pointer.source_generation,
                    &prepared.pointer.generation,
                    &prepared.pointer.projection_key,
                )
                .is_some()
    }

    #[hotpath::measure(label = "semantic.restart.commit")]
    fn commit_restore_if_current(&self, prepared: PreparedSemanticRuntimeRestoreV1) -> bool {
        let _transition = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.restore_snapshot_is_current(&prepared.expected_current, &prepared.expected_status)
        {
            return false;
        }
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(prepared.runtime);
        self.scheduling.restore_current(prepared.pointer);
        true
    }

    pub fn status_projection(&self) -> SemanticRuntimeStatusProjectionV1 {
        let status = self.status();
        let (degraded_reason, prior_generation) = match &status {
            SemanticRuntimeScheduleStatusV1::Indexing {
                prior_generation, ..
            } => (None, prior_generation.clone()),
            SemanticRuntimeScheduleStatusV1::Failed {
                reason,
                prior_generation,
            } => (
                Some(match reason {
                    SemanticRuntimeScheduleFailureV1::Artifact
                    | SemanticRuntimeScheduleFailureV1::ArtifactDetail(_) => {
                        SemanticFallbackReasonV1::ArtifactUnavailable
                    }
                    SemanticRuntimeScheduleFailureV1::Runtime
                    | SemanticRuntimeScheduleFailureV1::Projection
                    | SemanticRuntimeScheduleFailureV1::ProjectionDetail(_)
                    | SemanticRuntimeScheduleFailureV1::Publication
                    | SemanticRuntimeScheduleFailureV1::PublicationDetail(_) => {
                        SemanticFallbackReasonV1::RuntimeFailure
                    }
                    SemanticRuntimeScheduleFailureV1::Cancelled
                    | SemanticRuntimeScheduleFailureV1::DeadlineExceeded => {
                        SemanticFallbackReasonV1::RuntimeUnavailable
                    }
                }),
                prior_generation.clone(),
            ),
            SemanticRuntimeScheduleStatusV1::Current { generation } => {
                (None, Some(generation.clone()))
            }
            SemanticRuntimeScheduleStatusV1::Unavailable => {
                (Some(SemanticFallbackReasonV1::RuntimeUnavailable), None)
            }
        };
        SemanticRuntimeStatusProjectionV1 {
            status,
            degraded_reason,
            prior_generation,
        }
    }
}

struct RuntimeChunkVectorEncoderV1<R: EmbeddingRuntime> {
    runtime: Arc<SemanticRuntimeService<R>>,
    progress: Arc<SemanticRuntimeScheduleCancellationV1>,
    /// Checked-out sessions, grown lazily up to [`Self::width`]. Index 0 is
    /// the session the single-group path uses, so a narrow host behaves
    /// exactly as it did before.
    sessions: Vec<PooledSession<R, SystemMonotonicClock>>,
    width: usize,
    intra_threads: usize,
    completed_units: u64,
    documents: Arc<EmbeddingDocumentComposerV1>,
}

impl<R> RuntimeChunkVectorEncoderV1<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    fn new(
        runtime: Arc<SemanticRuntimeService<R>>,
        progress: Arc<SemanticRuntimeScheduleCancellationV1>,
        execution: embedding_parallelism::EmbeddingExecutionPlanV1,
        documents: Arc<EmbeddingDocumentComposerV1>,
    ) -> Self {
        let intra_threads = execution.intra_threads;
        let width = execution.sessions;
        // The width the host arithmetic asks for. Compare against
        // `semantic_embed_sessions_held` to see what the pool actually
        // granted; the two diverging is the only symptom pool pressure has on
        // this path, because acquisition here never blocks.
        hotpath::gauge!("semantic_embed_session_width").set(width);
        Self {
            runtime,
            progress,
            sessions: Vec::new(),
            width,
            intra_threads,
            completed_units: 0,
            documents,
        }
    }

    /// Check out up to `wanted` sessions, stopping early on any pool refusal.
    ///
    /// The pool's own ceilings (session count, resident bytes) therefore stay
    /// the binding constraint: exhaustion narrows the width instead of failing
    /// the projection. At least one session must be obtainable.
    fn ensure_sessions(&mut self, wanted: usize) -> Result<usize, String> {
        let wanted = wanted.min(self.width).max(1);
        while self.sessions.len() < wanted {
            match self.runtime.acquire() {
                Ok(session) => self.sessions.push(session),
                Err(error) if self.sessions.is_empty() => return Err(error.to_string()),
                // The pool refused, so embedding narrows rather than failing.
                // This is the ONLY way pool pressure reaches the projection
                // path: acquisition here is non-blocking, so no caller ever
                // waits and `semantic.session_pool.wait` can never record it.
                // Counting the refusal is therefore the only way a narrowed
                // run is distinguishable from a deliberately narrow host.
                Err(_) => {
                    hotpath::gauge!("semantic_embed_session_shortfall").inc(1);
                    break;
                }
            }
        }
        hotpath::gauge!("semantic_embed_sessions_wanted").set(wanted);
        hotpath::gauge!("semantic_embed_sessions_held").set(self.sessions.len());
        Ok(self.sessions.len())
    }
}

/// One stripe's encoded groups plus the units it completed, or the reason it
/// stopped. Stripes are joined in input order, so the first `Err` here is the
/// lowest-index failure.
type EncodedStripeResultV1 = Result<(Vec<Vec<Vec<f32>>>, u64), String>;

/// Encode one already-composed group against one checked-out session.
///
/// Free-standing so the sequential and concurrent paths run byte-identical
/// code; the only difference between them is which session is used.
fn encode_group_with_session<R>(
    session: &mut PooledSession<R, SystemMonotonicClock>,
    key: &tracedecay_domain::EmbeddingProjectionKeyV1,
    chunks: &[&CodeSearchChunkV1],
    progress: &SemanticRuntimeScheduleCancellationV1,
    documents: &EmbeddingDocumentComposerV1,
) -> Result<(Vec<Vec<f32>>, u64), String>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    if chunks.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if progress.cancelled() {
        // Cancelled groups otherwise vanish from the profile: they return a
        // plain string error before any adapter-level failure classifier runs.
        hotpath::gauge!("semantic_embed_cancelled_groups").inc(1u32);
        return Err("semantic projection cancelled".to_owned());
    }
    if session.authority().projection().embedding_key() != key {
        return Err("semantic projection authority changed".to_owned());
    }
    let max_texts = session.authority().max_batch_texts() as usize;
    let max_bytes = session.authority().max_batch_bytes() as usize;
    let inference_batch_size = key.inference_batch_size as usize;
    let inference_batch_bytes = key.inference_batch_bytes as usize;
    if max_texts != inference_batch_size
        || max_bytes != inference_batch_bytes
        || chunks.len() > inference_batch_size
    {
        return Err(
            "semantic projection authority does not match its inference batch identity".to_owned(),
        );
    }
    // The one copy between canonical chunks and tensor input: every chunk is
    // composed into an owned document under the key's composition. Timed
    // separately so it cannot hide inside `semantic.embed.infer`.
    let batch = hotpath::measure_block!(
        "semantic.embed.batch_assembly",
        compose_group_documents(key, chunks, documents).and_then(|texts| {
            BoundedSanitizedTextBatchV1::try_new(texts, max_texts, max_bytes)
                .map_err(|error| error.to_string())
        })
    )?;
    let vectors = session
        .embed_batch(&batch, progress)
        .map_err(|error| error.to_string())?;
    if vectors.len() != chunks.len() {
        return Err("semantic projector returned an unexpected vector batch size".to_owned());
    }
    // Per-vector dimension/finite validation plus the move out of the
    // adapter's vector type. Also timed separately: a regression here would
    // otherwise be read as slower inference.
    let encoded = hotpath::measure_block!(
        "semantic.embed.vector_writeback",
        vectors
            .into_iter()
            .map(|vector| {
                vector.validate().map_err(|error| error.to_string())?;
                Ok(vector.values)
            })
            .collect::<Result<Vec<_>, String>>()
    )?;
    Ok((encoded, chunks.len() as u64))
}

/// Compose one encoder group's documents in group order.
///
/// A symbol-backed chunk embedded without its header is counted rather than
/// silently accepted: under the header composition that count is the only
/// visible difference between a corpus whose symbol text is canonical and one
/// where headers were withheld.
fn compose_group_documents(
    key: &tracedecay_domain::EmbeddingProjectionKeyV1,
    chunks: &[&CodeSearchChunkV1],
    documents: &EmbeddingDocumentComposerV1,
) -> Result<Vec<String>, String> {
    chunks
        .iter()
        .map(|chunk| {
            let document = documents
                .compose(key, chunk)
                .map_err(|error| error.to_string())?;
            match document.header() {
                EmbeddingDocumentHeaderV1::Rendered => {
                    hotpath::gauge!("semantic_embed_header_rendered").inc(1u32);
                }
                EmbeddingDocumentHeaderV1::Withheld(_) => {
                    hotpath::gauge!("semantic_embed_header_withheld").inc(1u32);
                }
                EmbeddingDocumentHeaderV1::NotComposed | EmbeddingDocumentHeaderV1::NoSymbol => {}
            }
            Ok(document.into_text())
        })
        .collect()
}

impl<R> CanonicalChunkVectorEncoderV1 for RuntimeChunkVectorEncoderV1<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    fn encode(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        let mut vectors = self.encode_batch(key, std::slice::from_ref(&chunk))?;
        if vectors.len() != 1 {
            return Err("semantic projector returned a non-unit vector batch".to_owned());
        }
        vectors
            .pop()
            .ok_or_else(|| "semantic projector returned a non-unit vector batch".to_owned())
    }

    #[hotpath::measure(label = "semantic.embed.encode")]
    fn encode_batch(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        if self.progress.cancelled() {
            return Err("semantic projection cancelled".to_owned());
        }
        self.ensure_sessions(1)?;
        let progress = Arc::clone(&self.progress);
        let documents = Arc::clone(&self.documents);
        let intra_threads = self.intra_threads;
        let (encoded, units) =
            tracedecay_code_index::parallelism::with_background_cpu_permits(intra_threads, || {
                encode_group_with_session(
                    &mut self.sessions[0],
                    key,
                    chunks,
                    progress.as_ref(),
                    documents.as_ref(),
                )
            })?;
        self.completed_units = self.completed_units.saturating_add(units);
        self.progress.set_completed_units(self.completed_units);
        Ok(encoded)
    }

    fn encode_concurrency(&self) -> usize {
        self.width
    }

    /// Dispatch the caller's groups across every checked-out session.
    ///
    /// Groups are split into contiguous stripes, one per session, so flattening
    /// the stripe results back in stripe order reproduces the caller's input
    /// order exactly. Each group is still one invocation over the same tensor
    /// shape it would have had sequentially, so the vectors are byte-identical
    /// at any width — the width only decides how many run at once.
    ///
    /// Failures are reported by lowest input index, matching the sequential
    /// path's first-error semantics regardless of which stripe failed first in
    /// wall-clock terms.
    #[hotpath::measure(label = "semantic.embed.encode_stripes")]
    fn encode_batches(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        groups: &[&[&CodeSearchChunkV1]],
    ) -> Result<Vec<Vec<Vec<f32>>>, String> {
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        if self.progress.cancelled() {
            return Err("semantic projection cancelled".to_owned());
        }
        let sessions = self.ensure_sessions(groups.len())?;
        if sessions <= 1 || groups.len() == 1 {
            // One stripe. Counted so a run that never widens is visible as a
            // count rather than inferred from the absence of parallel spans.
            hotpath::gauge!("semantic_embed_sequential_dispatch").inc(1);
            hotpath::gauge!("semantic_embed_encode_stripes").set(1_usize);
            let progress = Arc::clone(&self.progress);
            let documents = Arc::clone(&self.documents);
            let intra_threads = self.intra_threads;
            let mut encoded = Vec::with_capacity(groups.len());
            let mut units = 0u64;
            for group in groups {
                let (vectors, group_units) =
                    tracedecay_code_index::parallelism::with_background_cpu_permits(
                        intra_threads,
                        || {
                            encode_group_with_session(
                                &mut self.sessions[0],
                                key,
                                group,
                                progress.as_ref(),
                                documents.as_ref(),
                            )
                        },
                    )
                    .inspect_err(|_| {
                        self.completed_units = self.completed_units.saturating_add(units);
                        self.progress.set_completed_units(self.completed_units);
                    })?;
                units = units.saturating_add(group_units);
                encoded.push(vectors);
            }
            self.completed_units = self.completed_units.saturating_add(units);
            self.progress.set_completed_units(self.completed_units);
            return Ok(encoded);
        }

        let stripe_len = groups.len().div_ceil(sessions);
        let stripes = groups.chunks(stripe_len).collect::<Vec<_>>();
        hotpath::gauge!("semantic_embed_encode_stripes").set(stripes.len());
        let progress = Arc::clone(&self.progress);
        let documents = Arc::clone(&self.documents);
        let intra_threads = self.intra_threads;
        let mut stripe_results: Vec<EncodedStripeResultV1> =
            embedding_parallelism::install(|| {
                use rayon::prelude::*;
                stripes
                    .par_iter()
                    .zip(self.sessions.par_iter_mut())
                    .map(|(stripe, session)| {
                        // Per-worker service demand for one stripe, including
                        // its CPU-permit wait. `semantic.embed.encode_stripes`
                        // above stays the only wall-time authority; these
                        // per-stripe totals overlap and must never be summed
                        // into a wall figure.
                        hotpath::measure_block!("semantic.embed.stripe", {
                            tracedecay_code_index::parallelism::with_background_cpu_permits(
                                intra_threads,
                                || {
                                    let mut encoded = Vec::with_capacity(stripe.len());
                                    let mut units = 0u64;
                                    for group in stripe.iter() {
                                        let (vectors, group_units) = encode_group_with_session(
                                            session,
                                            key,
                                            group,
                                            progress.as_ref(),
                                            documents.as_ref(),
                                        )?;
                                        units = units.saturating_add(group_units);
                                        encoded.push(vectors);
                                    }
                                    Ok((encoded, units))
                                },
                            )
                        })
                    })
                    .collect()
            })?;

        let mut encoded = Vec::with_capacity(groups.len());
        let mut units = 0u64;
        let mut failure = None;
        for result in stripe_results.drain(..) {
            match result {
                Ok((stripe, stripe_units)) => {
                    units = units.saturating_add(stripe_units);
                    if failure.is_none() {
                        encoded.extend(stripe);
                    }
                }
                Err(reason) => {
                    if failure.is_none() {
                        failure = Some(reason);
                    }
                }
            }
        }
        self.completed_units = self.completed_units.saturating_add(units);
        self.progress.set_completed_units(self.completed_units);
        match failure {
            Some(reason) => Err(reason),
            None => Ok(encoded),
        }
    }
}

#[cfg(test)]
mod loadable_lifecycle_tests {
    use std::path::PathBuf;

    use tracedecay_semantic_contracts::{
        DEFAULT_FASTEMBED_MODEL_ID, SemanticModelLifecycleStateV1, SemanticResourceCeilings,
    };

    use super::model_catalog::FastEmbedModelCatalogV1;
    use super::model_lifecycle::SemanticModelLifecycleOwnerV1;
    use super::session_pool::test_support;
    use super::{
        LoadableLifecycleArtifactV1, LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1,
    };

    const INSTALL_PATH: &str = "/installs/semantic-model";

    /// Every lifecycle state for `model_id`, paired with whether a loadable
    /// artifact may be built from it. Only the four verified-install states
    /// are loadable; `Failed` retains full model identity and still is not.
    fn every_lifecycle_state(model_id: &str) -> Vec<(SemanticModelLifecycleStateV1, bool)> {
        let model_id = model_id.to_owned();
        let revision = "516f4baf13dec4ddddda8631e019b5737c8bc250".to_owned();
        let artifact_digest = "a".repeat(64);
        let install_path = PathBuf::from(INSTALL_PATH);
        vec![
            (
                SemanticModelLifecycleStateV1::SelectedNotDownloaded {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                },
                false,
            ),
            (
                SemanticModelLifecycleStateV1::Downloading {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                    bytes_received: 1,
                    bytes_total: 2,
                },
                false,
            ),
            (
                SemanticModelLifecycleStateV1::Verifying {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                },
                false,
            ),
            (
                SemanticModelLifecycleStateV1::Installed {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                    install_path: install_path.clone(),
                },
                true,
            ),
            (
                SemanticModelLifecycleStateV1::Loading {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                    install_path: install_path.clone(),
                },
                true,
            ),
            (
                SemanticModelLifecycleStateV1::Indexing {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                    install_path: install_path.clone(),
                    completed_units: 1,
                    total_units: 2,
                },
                true,
            ),
            (
                SemanticModelLifecycleStateV1::Ready {
                    model_id: model_id.clone(),
                    revision: revision.clone(),
                    artifact_digest: artifact_digest.clone(),
                    install_path,
                },
                true,
            ),
            (
                SemanticModelLifecycleStateV1::Failed {
                    model_id,
                    revision,
                    artifact_digest,
                    detail: "runtime load failed".to_owned(),
                    retryable: true,
                },
                false,
            ),
        ]
    }

    #[test]
    fn exactly_the_verified_install_states_of_a_cataloged_model_are_loadable() {
        let catalog = FastEmbedModelCatalogV1::production();
        for (state, loadable) in every_lifecycle_state(DEFAULT_FASTEMBED_MODEL_ID) {
            let label = format!("{state:?}");
            match LoadableLifecycleArtifactV1::from_state(Some(state), &catalog) {
                Ok(resolved) => {
                    assert!(loadable, "{label} must not be loadable");
                    assert_eq!(resolved.model.model_id, DEFAULT_FASTEMBED_MODEL_ID);
                    assert_eq!(
                        resolved.install_path,
                        PathBuf::from(INSTALL_PATH),
                        "{label} must surface its own install path"
                    );
                }
                Err(failure) => {
                    assert!(!loadable, "{label} must be loadable: {failure}");
                    assert_eq!(failure, SemanticRuntimeScheduleFailureV1::Artifact);
                }
            }
        }
        // A verified install of a model the catalog no longer serves is not
        // loadable either: admissibility and catalog lookup are one decision.
        for (state, _) in every_lifecycle_state("NotARealModel") {
            let label = format!("{state:?}");
            assert_eq!(
                LoadableLifecycleArtifactV1::from_state(Some(state), &catalog).err(),
                Some(SemanticRuntimeScheduleFailureV1::Artifact),
                "{label} names an uncataloged model"
            );
        }
        assert_eq!(
            LoadableLifecycleArtifactV1::from_state(None, &catalog).err(),
            Some(SemanticRuntimeScheduleFailureV1::Artifact)
        );
    }

    /// The public constructors consult the same projection: a real owner in
    /// `SelectedNotDownloaded` is refused with the typed artifact failure
    /// before any install bytes are read.
    #[test]
    fn constructors_refuse_a_selected_but_not_downloaded_owner() {
        let root = tempfile::tempdir().expect("lifecycle root");
        let owner =
            SemanticModelLifecycleOwnerV1::open_default(root.path()).expect("lifecycle owner");
        assert!(matches!(
            owner.status().state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        assert!(matches!(
            LoadableLifecycleArtifactV1::resolve(&owner),
            Err(SemanticRuntimeScheduleFailureV1::Artifact)
        ));
        let projection = test_support::authority().projection().clone();
        assert!(matches!(
            LoadedSemanticArtifactV1::from_lifecycle_projection(
                &owner,
                &projection,
                SemanticResourceCeilings::default(),
            ),
            Err(SemanticRuntimeScheduleFailureV1::Artifact)
        ));
    }
}

#[cfg(test)]
mod document_composition_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tracedecay_code_index::embedding_document::{
        EmbeddingDocumentComposerV1, EmbeddingSymbolContextIndexV1,
    };
    use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
        CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, ComplexityAnalysisV1,
        ContentDigest, EmbeddingDocumentCompositionV1, FileIdentityDigest, FileOccurrenceId,
        LanguageDescriptorRevision, PolicyRevisionId, SanitizerRevision, SensitivityDecision,
        SensitivityLevelV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
    };

    use super::RuntimeChunkVectorEncoderV1;
    use super::fastembed_adapter::{AdmittedProjectionArtifactV1, FakeEmbeddingRuntime};
    use super::projector::CanonicalChunkVectorEncoderV1;
    use super::runtime_service::{
        SemanticRuntimeScheduleCancellationV1, SemanticRuntimeService,
        SharedEmbeddingRuntimeFactory,
    };
    use super::session_pool::test_support;

    fn generation() -> CodeGenerationId {
        CodeGenerationId::new("composition.generation".to_owned()).expect("generation fixture")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn symbol_record() -> Arc<LineageSymbolRecordV1> {
        Arc::new(LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new("composition.symbol.get".to_owned())
                .expect("occurrence fixture"),
            identity: SymbolIdentityDigest::new(digest('1')).expect("identity fixture"),
            qualified_name: "Holder::get".to_owned(),
            simple_name: "get".to_owned(),
            kind: "method".to_owned(),
            visibility: "public".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: 0,
            signature: None,
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: FileIdentityDigest::new(digest('f')).expect("file identity fixture"),
            content_digest: ContentDigest::new(digest('d')).expect("content fixture"),
        })
    }

    fn documents() -> Arc<EmbeddingDocumentComposerV1> {
        let index = GenerationSymbolIndexV1::new(generation(), vec![symbol_record()])
            .expect("symbol index");
        Arc::new(EmbeddingDocumentComposerV1::new(
            EmbeddingSymbolContextIndexV1::from_generation_symbols(&index),
        ))
    }

    fn chunk(label: &str, occurrence: Option<&str>, text: &str) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("composition.chunk.{label}")).expect("chunk id"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation(),
                file_occurrence_id: FileOccurrenceId::new("holder.rs".to_owned())
                    .expect("file fixture"),
                symbol_occurrence_id: occurrence
                    .map(|value| SymbolOccurrenceId::new(value.to_owned()).expect("occurrence")),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: text.len() as u64,
                },
                grain: if occurrence.is_some() {
                    CodeSearchChunkGrainV1::SymbolBody
                } else {
                    CodeSearchChunkGrainV1::FileWindow
                },
                ordinal: 0,
            },
            content_digest: ContentDigest::new(digest('c')).expect("content fixture"),
            language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                .expect("language fixture"),
            chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker fixture"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer fixture"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("policy fixture"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new(text).expect("sanitized fixture"),
        }
    }

    fn encode_with(
        authority: AdmittedProjectionArtifactV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Vec<Vec<f32>> {
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let authority = Arc::new(authority);
        let runtime = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            test_support::config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("fake runtime service");
        let mut encoder = RuntimeChunkVectorEncoderV1::new(
            runtime,
            Arc::new(SemanticRuntimeScheduleCancellationV1::new(
                chunks.len() as u64
            )),
            authority.embedding_execution_plan(),
            documents(),
        );
        encoder
            .encode_batch(authority.projection().embedding_key(), chunks)
            .expect("encoded batch")
    }

    /// The header reaches the tensor input: under the header composition a
    /// symbol chunk embeds exactly as the sanitized composition embeds a chunk
    /// whose text already is the composed document, and differently from its
    /// bare text. The fake runtime's vectors are a pure function of the text.
    #[test]
    fn header_composition_changes_the_tensor_input_and_only_that() {
        let body =
            "pub fn get(&self, key: u32) -> Option<u32> {\n    self.map.get(&key).copied()\n}";
        let symbol_chunk = chunk("symbol", Some("composition.symbol.get"), body);
        let composed_as_text = chunk(
            "composed",
            None,
            &format!("symbol: method get\nscope: Holder\n{body}"),
        );
        let file_chunk = chunk("file", None, "use std::collections::HashMap;\n");

        let sanitized = encode_with(
            test_support::authority(),
            &[&symbol_chunk, &composed_as_text, &file_chunk],
        );
        let header = encode_with(
            test_support::authority_with_document_composition(
                EmbeddingDocumentCompositionV1::SymbolContextHeader,
            ),
            &[&symbol_chunk, &file_chunk],
        );

        assert_ne!(
            header[0], sanitized[0],
            "the header must change the symbol chunk's input"
        );
        assert_eq!(
            header[0], sanitized[1],
            "the header composition embeds exactly the composed document"
        );
        assert_eq!(
            header[1], sanitized[2],
            "a file-grain chunk carries no header and embeds its text unchanged"
        );
    }

    #[test]
    fn foreign_generation_chunks_fail_the_batch_under_the_header_composition() {
        let mut foreign = chunk("foreign", Some("composition.symbol.get"), "pub fn get() {}");
        foreign.anchor.generation_id =
            CodeGenerationId::new("composition.other".to_owned()).expect("generation fixture");
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let authority = Arc::new(test_support::authority_with_document_composition(
            EmbeddingDocumentCompositionV1::SymbolContextHeader,
        ));
        let runtime = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            test_support::config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("fake runtime service");
        let mut encoder = RuntimeChunkVectorEncoderV1::new(
            runtime,
            Arc::new(SemanticRuntimeScheduleCancellationV1::new(1)),
            authority.embedding_execution_plan(),
            documents(),
        );
        let error = encoder
            .encode_batch(authority.projection().embedding_key(), &[&foreign])
            .expect_err("a chunk from another generation cannot borrow this symbol index");
        assert!(error.contains("composition.generation"), "{error}");
    }
}

#[cfg(test)]
mod scheduling_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::oneshot;
    use tracedecay_code_index::embedding_document::{
        EmbeddingDocumentComposerV1, EmbeddingSymbolContextIndexV1,
    };
    use tracedecay_code_index::lineage::GenerationSymbolIndexV1;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, CodeGenerationId, ManifestDigest, ProjectionBatchRequestV1,
        ProjectionKeyV1, ProjectionReplayReasonV1, VectorGenerationIdV1,
    };

    use super::fastembed_adapter::EmbedError;
    use super::fastembed_adapter::lifecycle_test_support::digest_mismatched_lifecycle_authority;
    use super::session_pool::SessionAcquireError;
    use super::{
        SemanticFallbackReasonV1, SemanticGenerationPointerV1, SemanticProjectionResumeOutcomeV1,
        SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
        SemanticRuntimeSchedulingHandleV1, SemanticRuntimeWorkV1, warm_failure,
    };

    fn source_generation(value: char) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
    }

    fn documents(value: char) -> Arc<EmbeddingDocumentComposerV1> {
        let index = GenerationSymbolIndexV1::new(source_generation(value), Vec::new())
            .expect("empty symbol index");
        Arc::new(EmbeddingDocumentComposerV1::new(
            EmbeddingSymbolContextIndexV1::from_generation_symbols(&index),
        ))
    }

    fn vector_generation(value: char) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(
            ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("manifest digest"),
        )
    }

    fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
        let authority = super::session_pool::test_support::authority();
        SemanticGenerationPointerV1 {
            generation: vector_generation(vector),
            source_generation: source_generation(source),
            projection_key: authority.projection().projection_key().clone(),
        }
    }

    fn projection_request_with_key(
        source: char,
        target_projection_key: ProjectionKeyV1,
    ) -> ProjectionBatchRequestV1 {
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
            target_projection_key,
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        }
    }

    async fn wait_for_current(
        handle: &SemanticRuntimeSchedulingHandleV1,
        expected: &VectorGenerationIdV1,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if handle
                    .current()
                    .is_some_and(|current| current.generation == *expected)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("semantic generation became current");
    }

    #[tokio::test]
    async fn stale_restart_snapshot_cannot_replace_newer_indexing_work() {
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let (expected_current, expected_status) = handle.restore_snapshot();
        let (started_tx, started_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel::<()>();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_cancellation| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Cancelled)
            },
        )));
        started_rx.await.expect("newer indexing work started");

        assert!(
            !handle.restore_snapshot_is_current(&expected_current, &expected_status),
            "restart publication must compare-and-set its scheduler snapshot"
        );
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ));
    }

    #[tokio::test]
    async fn daemon_handle_rejects_projection_work_above_its_bound() {
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 2, 1 << 20).expect("bounded handle");
        let started = Arc::new(AtomicBool::new(false));
        let started_by_work = Arc::clone(&started);
        let accepted = handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            3,
            move |_cancellation| async move {
                started_by_work.store(true, Ordering::Release);
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));

        assert!(!accepted);
        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::Acquire));
        assert_eq!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Unavailable
        );
    }

    #[tokio::test]
    async fn failed_reload_keeps_the_compatible_prior_generation_current() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let prior_pointer = pointer('a', 'a');
        let prior = prior_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_cancellation| async move {
                Ok(super::PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(prior_pointer) },
                ))
            },
        ));
        wait_for_current(&handle, &prior).await;

        let (release_tx, release_rx) = oneshot::channel();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_cancellation| async move {
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Artifact)
            },
        ));
        assert_eq!(
            handle.current().map(|current| current.generation),
            Some(prior.clone())
        );
        release_tx.send(()).expect("release failed reload");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if matches!(
                    handle.status(),
                    SemanticRuntimeScheduleStatusV1::Failed {
                        reason: SemanticRuntimeScheduleFailureV1::Artifact,
                        ..
                    }
                ) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failure became observable");
        assert_eq!(
            handle.current().map(|current| current.generation),
            Some(prior)
        );
    }

    #[tokio::test]
    async fn degraded_status_retains_the_prior_generation_and_reason() {
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let prior_pointer = pointer('a', 'a');
        let prior = prior_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(super::PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(prior_pointer) },
                ))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prior semantic generation published");

        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_progress| async move { Err(SemanticRuntimeScheduleFailureV1::Artifact) },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !matches!(
                handle.status(),
                SemanticRuntimeScheduleStatusV1::Failed { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("degraded status published");

        let projection = handle.status_projection();
        assert_eq!(
            projection.degraded_reason,
            Some(SemanticFallbackReasonV1::ArtifactUnavailable)
        );
        assert_eq!(projection.prior_generation, Some(prior));
        let status = serde_json::to_value(projection).expect("serialize runtime status");
        assert_eq!(status["degraded_reason"], "artifact_unavailable");
        assert!(status["prior_generation"].is_string());
    }

    /// Falsifiable in `semantic-fastembed` builds: without the pre-install
    /// warm, the structural-only authority would stage and publish Current
    /// over digest-mismatched bytes.
    #[cfg(all(feature = "semantic-fastembed", not(windows)))]
    #[tokio::test]
    async fn already_published_resume_with_digest_mismatched_model_never_becomes_current() {
        let mismatched = digest_mismatched_lifecycle_authority();
        let projection_key = mismatched.authority.projection().projection_key().clone();
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let staged = Arc::new(AtomicBool::new(false));
        let staged_by_request = Arc::clone(&staged);
        let pointer = SemanticGenerationPointerV1 {
            generation: vector_generation('a'),
            source_generation: source_generation('a'),
            projection_key: projection_key.clone(),
        };
        let authority = Arc::new(mismatched.authority);
        let request = super::FastEmbedSemanticGenerationRequestV1::new(
            source_generation('a'),
            projection_request_with_key('a', projection_key),
            Vec::new(),
            documents('a'),
            8,
            move || Ok(super::LoadedSemanticArtifactV1(authority)),
            || async { Ok(SemanticProjectionResumeOutcomeV1::AlreadyPublished) },
            |_prepared| async { Ok(()) },
            move || async move {
                staged_by_request.store(true, Ordering::Release);
                Ok(super::PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(pointer) },
                ))
            },
        )
        .expect("already-published request");
        assert!(handle.schedule_generation(request));

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !matches!(
                handle.status(),
                SemanticRuntimeScheduleStatusV1::Failed { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("digest-mismatched publication resume fails");

        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Failed {
                reason: SemanticRuntimeScheduleFailureV1::Runtime,
                prior_generation: None,
            }
        ));
        assert_eq!(handle.current(), None, "publication is not activation");
        assert!(
            !staged.load(Ordering::Acquire),
            "a failed pre-install warm must stop before publication staging"
        );
    }

    #[test]
    fn warm_failure_mapping_preserves_cancellation_and_deadline_identities() {
        assert_eq!(
            warm_failure(SessionAcquireError::Cancelled),
            SemanticRuntimeScheduleFailureV1::Cancelled
        );
        assert_eq!(
            warm_failure(SessionAcquireError::DeadlineExceeded {
                waited: std::time::Duration::from_secs(1),
                budget: std::time::Duration::from_secs(1),
            }),
            SemanticRuntimeScheduleFailureV1::DeadlineExceeded
        );
        assert_eq!(
            warm_failure(SessionAcquireError::LoadDeadlineExceeded {
                elapsed: std::time::Duration::from_secs(2),
                deadline: std::time::Duration::from_secs(1),
            }),
            SemanticRuntimeScheduleFailureV1::DeadlineExceeded
        );
        assert_eq!(
            warm_failure(SessionAcquireError::Open(EmbedError::Cancelled)),
            SemanticRuntimeScheduleFailureV1::Cancelled
        );
        assert_eq!(
            warm_failure(SessionAcquireError::Open(EmbedError::DeadlineExceeded)),
            SemanticRuntimeScheduleFailureV1::DeadlineExceeded
        );
        assert_eq!(
            warm_failure(SessionAcquireError::Closed),
            SemanticRuntimeScheduleFailureV1::Runtime
        );
    }

    #[test]
    fn restore_with_digest_mismatched_model_never_becomes_current() {
        let mismatched = digest_mismatched_lifecycle_authority();
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let pointer = SemanticGenerationPointerV1 {
            generation: vector_generation('a'),
            source_generation: source_generation('a'),
            projection_key: mismatched.authority.projection().projection_key().clone(),
        };

        assert!(matches!(
            handle.prepare_restore(
                pointer,
                super::LoadedSemanticArtifactV1(Arc::new(mismatched.authority)),
            ),
            Err(SemanticRuntimeScheduleFailureV1::Runtime)
        ));
        assert_eq!(handle.current(), None);
        assert_eq!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Unavailable
        );
    }

    #[cfg(all(feature = "semantic-fastembed", not(windows)))]
    #[test]
    fn exact_unbind_clears_pointer_and_factory_but_preserves_newer_generation() {
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let prior = pointer('a', 'a');
        handle.scheduling.restore_current(prior.clone());
        handle
            .bind_query_runtime_for_current(
                Arc::new(super::session_pool::test_support::authority()),
            )
            .expect("bind prior query runtime");
        assert!(
            handle
                .query_factory(
                    &prior.source_generation,
                    &prior.generation,
                    &prior.projection_key,
                )
                .is_some()
        );

        assert!(!handle.unbind_query_runtime_if_current(&vector_generation('b')));
        assert_eq!(handle.current(), Some(prior.clone()));
        assert!(
            handle
                .query_factory(
                    &prior.source_generation,
                    &prior.generation,
                    &prior.projection_key,
                )
                .is_some(),
            "a delayed older disable cannot evict a different generation"
        );

        assert!(handle.unbind_query_runtime_if_current(&prior.generation));
        assert!(handle.current().is_none());
        assert!(
            handle
                .query_factory(
                    &prior.source_generation,
                    &prior.generation,
                    &prior.projection_key,
                )
                .is_none()
        );
    }

    #[tokio::test]
    async fn superseded_preparation_cannot_publish_after_the_new_generation() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let (old_started_tx, old_started_rx) = oneshot::channel();
        let old_pointer = pointer('a', 'a');
        let old = old_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |cancellation| async move {
                let _ = old_started_tx.send(());
                while !cancellation.cancelled() {
                    tokio::task::yield_now().await;
                }
                Ok(super::PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(old_pointer) },
                ))
            },
        ));
        old_started_rx.await.expect("old preparation started");

        let current_pointer = pointer('b', 'b');
        let current = current_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_cancellation| async move {
                Ok(super::PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(current_pointer) },
                ))
            },
        ));

        wait_for_current(&handle, &current).await;
        tokio::task::yield_now().await;
        assert_eq!(
            handle.current().map(|pointer| pointer.generation),
            Some(current)
        );
        assert_ne!(
            handle.current().map(|pointer| pointer.generation),
            Some(old)
        );
    }
}
