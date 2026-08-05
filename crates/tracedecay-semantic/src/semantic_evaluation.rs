use std::sync::Arc;

use tracedecay_domain::{CodeSearchChunkV1, ProjectionBatchRequestV1};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticExecutionControl, SemanticQueryEmbeddingPort,
    SemanticQueryEmbeddingRequestV1,
};

use super::fastembed_adapter::{CancellationSignal, FastEmbedEmbeddingRuntime};
use super::projector::{PreparedVectorGenerationV1, prepare_vector_generation};
use super::runtime_query::{PooledSemanticQueryEmbedder, PooledSemanticQueryEmbedderFactory};
use super::runtime_service::{
    SemanticRuntimeScheduleCancellationV1, SemanticRuntimeScheduleFailureV1,
    SemanticRuntimeService, SharedEmbeddingRuntimeFactory, fastembed_runtime_factory,
};
use super::session_pool::SessionPoolConfigV1;
use super::{LoadedSemanticArtifactV1, RuntimeChunkVectorEncoderV1};

#[derive(Clone)]
pub struct SemanticEvaluationQueryFactoryV1 {
    inner: Arc<PooledSemanticQueryEmbedderFactory<FastEmbedEmbeddingRuntime>>,
}

/// Isolated evaluator projection. It reuses the verified production artifact
/// and `FastEmbed` runtime, but has no durable vector pointer and cannot replace
/// a project's active generation.
pub struct PreparedSemanticEvaluationProjectionV1 {
    pub query_factory: SemanticEvaluationQueryFactoryV1,
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
        query_factory: SemanticEvaluationQueryFactoryV1::from_runtime(
            PooledSemanticQueryEmbedderFactory::new(runtime),
        ),
        prepared,
    })
}

impl SemanticEvaluationQueryFactoryV1 {
    pub(super) fn from_runtime(
        inner: Arc<PooledSemanticQueryEmbedderFactory<FastEmbedEmbeddingRuntime>>,
    ) -> Self {
        Self { inner }
    }

    pub fn create<'a, C>(&self, control: &'a C) -> SemanticEvaluationQueryEmbedderV1<'a>
    where
        C: SemanticExecutionControl + Sync,
    {
        let cancellation = Arc::new(QueryCancellationV1(control));
        SemanticEvaluationQueryEmbedderV1 {
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

pub struct SemanticEvaluationQueryEmbedderV1<'a> {
    inner: PooledSemanticQueryEmbedder<'a, FastEmbedEmbeddingRuntime>,
}

impl SemanticQueryEmbeddingPort for SemanticEvaluationQueryEmbedderV1<'_> {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        self.inner.embed_query(request)
    }
}
