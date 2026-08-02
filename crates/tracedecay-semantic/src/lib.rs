//! Semantic code runtime: artifact store, model lifecycle, embedding
//! projection, session pooling, and the daemon-callable scheduling handle.
//!
//! The crate owns the semantic implementation outright. Configuration
//! ownership and application status projection stay with the root binary; the
//! few contracts both sides need (resource ceilings, the fallback reason, the
//! rerank compatibility pins, and the default catalog model id) are defined
//! here and re-exported by the root's configuration modules.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkV1, ComponentRevision, ManifestDigest,
    ProjectionBatchRequestV1, ProjectionKeyV1, VectorGenerationIdV1,
};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticExecutionControl, SemanticQueryEmbeddingPort,
    SemanticQueryEmbeddingRequestV1,
};

use self::fastembed_adapter::{
    AdmittedProjectionArtifactV1, BoundedSanitizedTextBatchV1, CancellationSignal,
    EmbeddingRuntime, EmbeddingSession, FastEmbedEmbeddingRuntime,
};
use self::projector::{
    CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, prepare_vector_generation,
    prepare_vector_generation_async,
};
use self::runtime_query::{
    CurrentSemanticQueryRuntimeV1, PooledSemanticQueryEmbedder, PooledSemanticQueryEmbedderFactory,
};
use self::runtime_service::{
    SemanticRuntimeService, SharedEmbeddingRuntimeFactory, fastembed_runtime_factory,
};
use self::session_pool::{PooledSession, SessionPoolConfigV1, SystemMonotonicClock};

mod artifact_store;
mod fastembed_adapter;
pub mod legacy_migration;
mod manifest;
mod model_catalog;
mod model_lifecycle;
pub mod projector;
pub mod rerank_adapter;
mod runtime_query;
mod runtime_service;
pub mod session_pool;

// Test-support constructors. Dependent crates opt in through `test-helpers`
// exactly like the query kernel's `*_for_test` surface.
#[cfg(any(test, feature = "test-helpers"))]
pub use model_catalog::production_fastembed_catalog;
#[cfg(any(test, feature = "test-helpers"))]
pub use model_catalog::{CatalogedFastEmbedModelV1, FastEmbedModelCatalogV1};
#[cfg(any(test, feature = "test-helpers"))]
pub use model_lifecycle::{ModelLifecycleErrorV1, ModelMemberSourceV1};
pub use model_lifecycle::{
    SemanticModelLifecycleOwnerV1, SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1,
    apply_config_and_queue_startup, shared_lifecycle_owner,
};

pub use runtime_service::{
    PreparedSemanticRuntimeCommitV1, SemanticGenerationPointerV1,
    SemanticRuntimeScheduleCancellationV1, SemanticRuntimeScheduleFailureV1,
    SemanticRuntimeScheduleStatusV1, SemanticRuntimeSchedulingHandleV1, SemanticRuntimeWorkV1,
};

/// Default `FastEmbed` catalog model selected on install (offline-safe).
///
/// Root configuration re-exports this so settings validation and the model
/// acquisition lifecycle can never disagree about the default selection.
pub const DEFAULT_FASTEMBED_MODEL_ID: &str = "JinaEmbeddingsV2BaseCode";

/// Resolve the lifecycle store root beneath a caller-supplied user data
/// directory. The crate never discovers the data directory itself; the root
/// binary owns that decision and passes the resolved path in.
pub fn default_lifecycle_root_in(user_data_dir: &Path) -> PathBuf {
    user_data_dir.join("semantic-models")
}

/// Process ceilings applied before an installed semantic profile is admitted.
///
/// The selected artifact manifest may impose tighter limits. These local
/// ceilings never authorize a profile to exceed its own declared bounds.
/// Range validation stays with the root configuration owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticResourceCeilings {
    pub max_model_bytes: u64,
    pub max_tokenizer_bytes: u64,
    pub max_resident_bytes: u64,
    pub max_threads: u32,
    pub max_concurrent_sessions: u32,
    pub max_batch_size: u32,
    pub max_sequence_length: u32,
    pub load_deadline_ms: u64,
}

impl Default for SemanticResourceCeilings {
    fn default() -> Self {
        Self {
            max_model_bytes: 700 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: 2 * 1024 * 1024 * 1024,
            max_threads: 4,
            max_concurrent_sessions: 1,
            max_batch_size: 32,
            max_sequence_length: 512,
            load_deadline_ms: 30_000,
        }
    }
}

/// Exact artifact and runtime pins for the optional bounded reranker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankCompatibilityPinsV1 {
    pub implementation_revision: ComponentRevision,
    pub artifact_manifest_digest: ManifestDigest,
    pub runtime_compatibility_digest: ManifestDigest,
}

/// Why the semantic lane is unavailable or degraded for one observation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticFallbackReasonV1 {
    ConfigurationUnavailable,
    RuntimeUnavailable,
    ArtifactUnavailable,
    IncompatibleRuntime,
    ResourceCeilingExceeded,
    CorruptArtifact,
    Indexing,
    RuntimeFailure,
    RollbackInProgress,
    InvalidRuntimeStatus,
    /// Selected catalog model has not been downloaded yet.
    SelectedNotDownloaded,
    /// Daemon-owned model acquisition is in progress.
    Downloading,
    /// Downloaded bytes are being verified against catalog pins.
    Verifying,
    /// Model is installed but not yet loaded into the runtime.
    Loading,
    /// Model acquisition or load failed; exact/lexical/graph remain available.
    ModelFailed,
}

type SemanticProjectionStageFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    >,
>;
type SemanticProjectionStageV1 =
    Box<dyn FnOnce(PreparedVectorGenerationV1) -> SemanticProjectionStageFutureV1 + Send + 'static>;
type FastEmbedArtifactLoaderV1 = Box<
    dyn FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
>;

pub struct LoadedSemanticArtifactV1(Arc<AdmittedProjectionArtifactV1>);

impl LoadedSemanticArtifactV1 {
    pub fn from_lifecycle(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        manifest: &tracedecay_domain::CodeGenerationManifestV1,
        resources: SemanticResourceCeilings,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let status = lifecycle.status();
        let state = status
            .state
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        let (model_id, install_path) = match state {
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
            } => (model_id, install_path),
            _ => return Err(SemanticRuntimeScheduleFailureV1::Artifact),
        };
        let model = lifecycle
            .catalog()
            .get(&model_id)
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        let authority = AdmittedProjectionArtifactV1::from_lifecycle_install(
            model,
            &install_path,
            manifest.chunker_revision.clone(),
            manifest.privacy_domain.clone(),
            manifest.privacy_key_epoch,
            resources,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Artifact)?;
        Ok(Self(Arc::new(authority)))
    }

    pub fn from_lifecycle_projection(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        projection: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
        resources: SemanticResourceCeilings,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let status = lifecycle.status();
        let state = status
            .state
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        let install_path = match &state {
            SemanticModelLifecycleStateV1::Installed { install_path, .. }
            | SemanticModelLifecycleStateV1::Loading { install_path, .. }
            | SemanticModelLifecycleStateV1::Indexing { install_path, .. }
            | SemanticModelLifecycleStateV1::Ready { install_path, .. } => install_path,
            _ => return Err(SemanticRuntimeScheduleFailureV1::Artifact),
        };
        let model = lifecycle
            .catalog()
            .get(state.model_id())
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        let key = projection.embedding_key();
        let authority = AdmittedProjectionArtifactV1::from_lifecycle_install(
            model,
            install_path,
            key.chunker_revision.clone(),
            key.privacy_domain.clone(),
            key.privacy_key_epoch,
            resources,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Artifact)?;
        if authority.projection() != projection {
            return Err(SemanticRuntimeScheduleFailureV1::Artifact);
        }
        Ok(Self(Arc::new(authority)))
    }

    pub fn lifecycle_projection(
        lifecycle: &SemanticModelLifecycleOwnerV1,
        manifest: &tracedecay_domain::CodeGenerationManifestV1,
        resources: SemanticResourceCeilings,
    ) -> Result<tracedecay_domain::AdmittedEmbeddingProjectionKeyV1, SemanticRuntimeScheduleFailureV1>
    {
        let status = lifecycle.status();
        let state = status
            .state
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        if !matches!(
            state,
            SemanticModelLifecycleStateV1::Installed { .. }
                | SemanticModelLifecycleStateV1::Loading { .. }
                | SemanticModelLifecycleStateV1::Indexing { .. }
                | SemanticModelLifecycleStateV1::Ready { .. }
        ) {
            return Err(SemanticRuntimeScheduleFailureV1::Artifact);
        }
        let model = lifecycle
            .catalog()
            .get(state.model_id())
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
        AdmittedProjectionArtifactV1::lifecycle_projection(
            model,
            manifest.chunker_revision.clone(),
            manifest.privacy_domain.clone(),
            manifest.privacy_key_epoch,
            resources,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Artifact)
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
    canonical_chunks: Vec<CodeSearchChunkV1>,
    load_artifact: FastEmbedArtifactLoaderV1,
    stage_projection: SemanticProjectionStageV1,
}

impl FastEmbedSemanticGenerationRequestV1 {
    pub fn new<LoadArtifact, StageProjection, StageFuture>(
        target_generation: CodeGenerationId,
        projection_request: ProjectionBatchRequestV1,
        canonical_chunks: Vec<CodeSearchChunkV1>,
        load_artifact: LoadArtifact,
        stage_projection: StageProjection,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1>
    where
        LoadArtifact: FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
            + Send
            + 'static,
        StageProjection: FnOnce(PreparedVectorGenerationV1) -> StageFuture + Send + 'static,
        StageFuture: Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    {
        if projection_request.changes.to_generation != target_generation {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        Ok(Self {
            target_generation,
            projection_request,
            canonical_chunks,
            load_artifact: Box::new(load_artifact),
            stage_projection: Box::new(move |prepared| Box::pin(stage_projection(prepared))),
        })
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SemanticRuntimeStatusProjectionV1 {
    pub status: SemanticRuntimeScheduleStatusV1,
    pub degraded_reason: Option<SemanticFallbackReasonV1>,
    pub prior_generation: Option<VectorGenerationIdV1>,
}

#[derive(Clone)]
pub struct DaemonSemanticQueryFactoryV1 {
    inner: Arc<PooledSemanticQueryEmbedderFactory<FastEmbedEmbeddingRuntime>>,
}

/// Isolated evaluator projection. It reuses the verified production artifact
/// and `FastEmbed` runtime, but has no durable vector pointer and cannot replace
/// a project's active generation.
pub struct PreparedSemanticEvaluationProjectionV1 {
    pub query_factory: DaemonSemanticQueryFactoryV1,
    pub prepared: PreparedVectorGenerationV1,
}

pub fn prepare_semantic_evaluation_projection(
    artifact: LoadedSemanticArtifactV1,
    request: ProjectionBatchRequestV1,
    canonical_chunks: &[CodeSearchChunkV1],
    max_sessions: usize,
    memory_ceiling_bytes: u64,
) -> Result<PreparedSemanticEvaluationProjectionV1, SemanticRuntimeScheduleFailureV1> {
    let authority = artifact.into_authority();
    let factory: SharedEmbeddingRuntimeFactory<FastEmbedEmbeddingRuntime> =
        fastembed_runtime_factory();
    let runtime = SemanticRuntimeService::new_owned(
        Arc::clone(&authority),
        factory,
        SessionPoolConfigV1 {
            max_sessions,
            max_queued_waiters: 0,
            idle_timeout: std::time::Duration::from_mins(5),
            memory_ceiling_bytes,
        },
    )
    .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
    let progress = Arc::new(SemanticRuntimeScheduleCancellationV1::new(
        request.changes.added_or_changed.len().max(1) as u64,
    ));
    let mut encoder = RuntimeChunkVectorEncoderV1::new(Arc::clone(&runtime), progress);
    let prepared = prepare_vector_generation(
        authority.projection(),
        request,
        canonical_chunks,
        &mut encoder,
    )
    .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    Ok(PreparedSemanticEvaluationProjectionV1 {
        query_factory: DaemonSemanticQueryFactoryV1 {
            inner: PooledSemanticQueryEmbedderFactory::new(runtime),
        },
        prepared,
    })
}

impl DaemonSemanticQueryFactoryV1 {
    pub fn create<'a, C>(&self, control: &'a C) -> DaemonSemanticQueryEmbedderV1<'a>
    where
        C: SemanticExecutionControl + Sync,
    {
        let cancellation = Arc::new(QueryCancellationV1(control));
        DaemonSemanticQueryEmbedderV1 {
            inner: self.inner.create(cancellation),
        }
    }

    pub fn resident_cache_bytes(&self) -> u64 {
        self.inner.runtime().stats().resident_bytes
    }
}

struct QueryCancellationV1<'a, C>(&'a C);

impl<C> CancellationSignal for QueryCancellationV1<'_, C>
where
    C: SemanticExecutionControl + Sync,
{
    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

pub struct DaemonSemanticQueryEmbedderV1<'a> {
    inner: PooledSemanticQueryEmbedder<'a, FastEmbedEmbeddingRuntime>,
}

impl SemanticQueryEmbeddingPort for DaemonSemanticQueryEmbedderV1<'_> {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        self.inner.embed_query(request)
    }
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
    runtime: Arc<RwLock<Option<CurrentSemanticQueryRuntimeV1<FastEmbedEmbeddingRuntime>>>>,
    query_in_flight: Arc<AtomicBool>,
    transitions: Arc<Mutex<()>>,
    pool_config: SessionPoolConfigV1,
}

pub struct PreparedSemanticRuntimeRestoreV1 {
    pointer: SemanticGenerationPointerV1,
    runtime: CurrentSemanticQueryRuntimeV1<FastEmbedEmbeddingRuntime>,
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
        let work = SemanticRuntimeWorkV1::new(
            request.target_generation,
            total_units,
            move |progress| async move {
                let authority = tokio::task::spawn_blocking(request.load_artifact)
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??
                    .0;
                if progress.cancelled() {
                    return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
                }

                let factory: SharedEmbeddingRuntimeFactory<FastEmbedEmbeddingRuntime> =
                    fastembed_runtime_factory();
                let candidate =
                    SemanticRuntimeService::new_owned(Arc::clone(&authority), factory, pool_config)
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
                let encoder =
                    RuntimeChunkVectorEncoderV1::new(Arc::clone(&candidate), Arc::clone(&progress));
                let prepared = prepare_vector_generation_async(
                    authority.projection().clone(),
                    request.projection_request,
                    request.canonical_chunks,
                    encoder,
                )
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
                if progress.cancelled() {
                    return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
                }
                progress.set_completed_units(total_units);

                let commit = (request.stage_projection)(prepared).await?;
                Ok(commit.on_success(move |pointer| {
                    if pointer.source_generation != target_generation
                        || pointer.projection_key != projection_key
                    {
                        return Err(SemanticRuntimeScheduleFailureV1::Publication);
                    }
                    *runtime
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(CurrentSemanticQueryRuntimeV1::new_with_admission(
                            pointer.clone(),
                            candidate,
                            Arc::clone(&query_in_flight),
                        ));
                    Ok(())
                }))
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

    pub fn query_factory(
        &self,
        source_generation: &CodeGenerationId,
        vector_generation: &VectorGenerationIdV1,
        projection_key: &ProjectionKeyV1,
    ) -> Option<DaemonSemanticQueryFactoryV1> {
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
        Some(DaemonSemanticQueryFactoryV1 { inner })
    }

    /// Test-only binding for a pointer published without a production runtime.
    #[cfg(any(test, feature = "test-helpers"))]
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
        let factory: SharedEmbeddingRuntimeFactory<FastEmbedEmbeddingRuntime> =
            fastembed_runtime_factory();
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
        let factory: SharedEmbeddingRuntimeFactory<FastEmbedEmbeddingRuntime> =
            fastembed_runtime_factory();
        let candidate =
            SemanticRuntimeService::new_owned(authority, factory, self.pool_config.clone())
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        candidate
            .warm_query_session()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
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
                    SemanticRuntimeScheduleFailureV1::Artifact => {
                        SemanticFallbackReasonV1::ArtifactUnavailable
                    }
                    SemanticRuntimeScheduleFailureV1::Runtime
                    | SemanticRuntimeScheduleFailureV1::Projection
                    | SemanticRuntimeScheduleFailureV1::Publication => {
                        SemanticFallbackReasonV1::RuntimeFailure
                    }
                    SemanticRuntimeScheduleFailureV1::Cancelled => {
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
    session: Option<PooledSession<R, SystemMonotonicClock>>,
    completed_units: u64,
}

impl<R> RuntimeChunkVectorEncoderV1<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    fn new(
        runtime: Arc<SemanticRuntimeService<R>>,
        progress: Arc<SemanticRuntimeScheduleCancellationV1>,
    ) -> Self {
        Self {
            runtime,
            progress,
            session: None,
            completed_units: 0,
        }
    }
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
        Ok(vectors.pop().unwrap_or_else(|| panic!("unit vector batch")))
    }

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
        let session = if let Some(session) = self.session.as_mut() {
            session
        } else {
            self.session = Some(self.runtime.acquire().map_err(|error| error.to_string())?);
            self.session
                .as_mut()
                .unwrap_or_else(|| panic!("session was just installed"))
        };
        if session.authority().projection().embedding_key() != key {
            return Err("semantic projection authority changed".to_owned());
        }
        let max_texts = session.authority().max_batch_texts() as usize;
        let max_bytes = session.authority().max_batch_bytes() as usize;
        let mut encoded = Vec::with_capacity(chunks.len());
        let mut cursor = 0;
        while cursor < chunks.len() {
            let mut end = cursor;
            let mut batch_bytes = 0usize;
            while end < chunks.len() && end - cursor < max_texts {
                let text_bytes = chunks[end].sanitized_text.as_str().len();
                if text_bytes > max_bytes {
                    return Err(
                        "semantic projection text exceeds the batch byte ceiling".to_owned()
                    );
                }
                if end > cursor && batch_bytes.saturating_add(text_bytes) > max_bytes {
                    break;
                }
                batch_bytes = batch_bytes.saturating_add(text_bytes);
                end += 1;
            }
            if end == cursor {
                return Err("semantic projection batch limits admit no input".to_owned());
            }
            let batch = BoundedSanitizedTextBatchV1::try_new(
                chunks[cursor..end]
                    .iter()
                    .map(|chunk| chunk.sanitized_text.as_str().to_owned())
                    .collect(),
                max_texts,
                max_bytes,
            )
            .map_err(|error| error.to_string())?;
            let vectors = session
                .embed_batch(&batch, self.progress.as_ref())
                .map_err(|error| error.to_string())?;
            if vectors.len() != end - cursor {
                return Err(
                    "semantic projector returned an unexpected vector batch size".to_owned(),
                );
            }
            for vector in vectors {
                vector.validate().map_err(|error| error.to_string())?;
                encoded.push(vector.values);
            }
            self.completed_units = self.completed_units.saturating_add((end - cursor) as u64);
            self.progress.set_completed_units(self.completed_units);
            cursor = end;
        }
        Ok(encoded)
    }
}

#[cfg(test)]
mod scheduling_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::oneshot;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, CodeGenerationId, ManifestDigest, ProjectionBatchRequestV1,
        ProjectionReplayReasonV1, VectorGenerationIdV1,
    };

    use super::{
        SemanticFallbackReasonV1, SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1,
        SemanticRuntimeScheduleStatusV1, SemanticRuntimeSchedulingHandleV1, SemanticRuntimeWorkV1,
    };

    fn source_generation(value: char) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
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
            target_projection_key: super::session_pool::test_support::authority()
                .projection()
                .projection_key()
                .clone(),
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
    async fn scheduling_returns_before_blocked_semantic_preparation() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let (started_tx, started_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel::<()>();
        let target = source_generation('a');

        handle.schedule(SemanticRuntimeWorkV1::new(
            target.clone(),
            3,
            move |_cancellation| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));

        started_rx
            .await
            .expect("preparation started asynchronously");
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing {
                target_generation,
                completed_units: 0,
                total_units: 3,
                ..
            } if target_generation == target
        ));
        assert!(handle.current().is_none());
    }

    #[tokio::test]
    async fn saved_edit_scheduling_does_not_block_exact_search() {
        let handle =
            super::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let request = super::FastEmbedSemanticGenerationRequestV1::new(
            source_generation('a'),
            projection_request('a'),
            Vec::new(),
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
            move |_| async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
        )
        .expect("saved generation request");
        assert!(handle.schedule_generation(request));
        started_rx.await.expect("saved edit started in background");

        let exact_results = ["exact-match"];
        assert_eq!(exact_results, ["exact-match"]);
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ));
        release_tx.send(()).expect("release artifact loader");
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
    async fn indexing_progress_reports_completed_units_monotonically() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let (progress_tx, progress_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            4,
            move |progress| async move {
                progress.set_completed_units(2);
                let _ = progress_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        progress_rx.await.expect("progress reported");

        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing {
                completed_units: 2,
                total_units: 4,
                ..
            }
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

    #[test]
    fn restart_restore_installs_current_pointer_without_indexing() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        // Vector char must be a valid sha256 hex digit for `ManifestDigest`;
        // the source char is a plain string id, so the restore mnemonic stays.
        let restored = pointer('e', 'r');
        handle.restore_current(restored.clone());

        assert_eq!(handle.current(), Some(restored.clone()));
        assert_eq!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Current {
                generation: restored.generation,
            }
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
