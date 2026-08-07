use std::sync::Arc;

use tracedecay_domain::{CodeSearchChunkV1, ProjectionBatchRequestV1};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticExecutionControl, SemanticQueryEmbeddingPort,
    SemanticQueryEmbeddingRequestV1,
};

use super::fastembed_adapter::{
    FastEmbedEmbeddingRuntime, SemanticExecutionAuthority, SemanticExecutionInterruptionV1,
};
use super::projector::{PreparedVectorGenerationV1, prepare_vector_generation};
use super::runtime_query::{PooledSemanticQueryEmbedder, PooledSemanticQueryEmbedderFactory};
use super::runtime_service::{
    SemanticRuntimeScheduleCancellationV1, SemanticRuntimeScheduleFailureV1,
    SemanticRuntimeService, SharedEmbeddingRuntimeFactory, fastembed_runtime_factory,
};
use super::session_pool::SessionPoolConfigV1;
use super::{LoadedSemanticArtifactV1, RuntimeChunkVectorEncoderV1};

/// One caller-owned cancellation/deadline authority shared by every stage of
/// a semantic evaluation. Evaluator code never manufactures a replacement.
pub trait SemanticEvaluationCancellationV1: SemanticExecutionAuthority {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationProjectionCancellationV1 {
    pub projection_calls: u64,
    pub chunks_added_or_changed: u64,
}

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
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
) -> Result<PreparedSemanticEvaluationProjectionV1, SemanticRuntimeScheduleFailureV1> {
    if let Some(interruption) = cancellation.interruption() {
        return Err(schedule_interruption(interruption));
    }
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
    let progress = Arc::new(SemanticRuntimeScheduleCancellationV1::new_linked(
        request.changes.added_or_changed.len().max(1) as u64,
        cancellation,
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

/// Execute one genuine model batch and then cancel before a complete
/// evaluator projection can be returned or published.
pub fn measure_semantic_evaluation_projection_cancellation(
    artifact: LoadedSemanticArtifactV1,
    request: ProjectionBatchRequestV1,
    canonical_chunks: &[CodeSearchChunkV1],
    max_sessions: usize,
    memory_ceiling_bytes: u64,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
) -> Result<SemanticEvaluationProjectionCancellationV1, SemanticRuntimeScheduleFailureV1> {
    if request.changes.added_or_changed.is_empty() {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    if let Some(interruption) = cancellation.interruption() {
        return Err(schedule_interruption(interruption));
    }
    let chunks_added_or_changed = request.changes.added_or_changed.len() as u64;
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
    let progress = Arc::new(SemanticRuntimeScheduleCancellationV1::new_linked(
        request.changes.added_or_changed.len() as u64,
        cancellation,
    ));
    let inner = RuntimeChunkVectorEncoderV1::new(Arc::clone(&runtime), Arc::clone(&progress));
    let mut encoder = CancelAfterFirstModelBatchV1 {
        inner,
        progress: Arc::clone(&progress),
    };
    if prepare_vector_generation(
        authority.projection(),
        request,
        canonical_chunks,
        &mut encoder,
    )
    .is_ok()
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    let projection_calls = progress.completed_units();
    if projection_calls == 0 || projection_calls >= chunks_added_or_changed || !progress.cancelled()
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(SemanticEvaluationProjectionCancellationV1 {
        projection_calls,
        chunks_added_or_changed,
    })
}

fn schedule_interruption(
    interruption: SemanticExecutionInterruptionV1,
) -> SemanticRuntimeScheduleFailureV1 {
    match interruption {
        SemanticExecutionInterruptionV1::Cancelled => SemanticRuntimeScheduleFailureV1::Cancelled,
        SemanticExecutionInterruptionV1::DeadlineExceeded => {
            SemanticRuntimeScheduleFailureV1::DeadlineExceeded
        }
    }
}

struct CancelAfterFirstModelBatchV1 {
    inner: RuntimeChunkVectorEncoderV1<FastEmbedEmbeddingRuntime>,
    progress: Arc<SemanticRuntimeScheduleCancellationV1>,
}

impl super::projector::CanonicalChunkVectorEncoderV1 for CancelAfterFirstModelBatchV1 {
    fn encode(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        let encoded = self.inner.encode(key, chunk)?;
        self.progress.cancel();
        Err(if encoded.is_empty() {
            "semantic projection produced no work before cancellation".to_owned()
        } else {
            "semantic projection cancelled after observed work".to_owned()
        })
    }

    fn encode_batch(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        let encoded = self.inner.encode_batch(key, chunks)?;
        self.progress.cancel();
        Err(if encoded.is_empty() {
            "semantic projection produced no work before cancellation".to_owned()
        } else {
            "semantic projection cancelled after observed work".to_owned()
        })
    }

    fn encode_batches(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        groups: &[&[&CodeSearchChunkV1]],
    ) -> Result<Vec<Vec<Vec<f32>>>, String> {
        let first = groups
            .first()
            .ok_or_else(|| "semantic projection cancellation received no work".to_owned())?;
        let encoded = self.inner.encode_batch(key, first)?;
        self.progress.cancel();
        Err(if encoded.is_empty() {
            "semantic projection produced no work before cancellation".to_owned()
        } else {
            "semantic projection cancelled after observed work".to_owned()
        })
    }
}

impl SemanticEvaluationQueryFactoryV1 {
    pub(super) fn from_runtime(
        inner: Arc<PooledSemanticQueryEmbedderFactory<FastEmbedEmbeddingRuntime>>,
    ) -> Self {
        Self { inner }
    }

    pub fn create<'a, C>(
        &self,
        control: &'a C,
        deadline_micros: Option<u64>,
    ) -> SemanticEvaluationQueryEmbedderV1<'a>
    where
        C: SemanticExecutionControl + Sync,
    {
        let cancellation = Arc::new(QueryExecutionAuthorityV1 {
            control,
            deadline_micros,
        });
        SemanticEvaluationQueryEmbedderV1 {
            inner: self.inner.create(cancellation),
        }
    }

    pub fn resident_cache_bytes(&self) -> u64 {
        self.inner.runtime().stats().resident_bytes
    }

    pub fn cold_load_micros(&self) -> Option<u64> {
        self.inner.runtime().stats().last_cold_load_micros
    }
}

struct QueryExecutionAuthorityV1<'a, C> {
    control: &'a C,
    deadline_micros: Option<u64>,
}

impl<C> SemanticExecutionAuthority for QueryExecutionAuthorityV1<'_, C>
where
    C: SemanticExecutionControl + Sync,
{
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        if self.control.is_cancelled() {
            Some(SemanticExecutionInterruptionV1::Cancelled)
        } else if self
            .deadline_micros
            .is_some_and(|deadline| self.control.elapsed_micros() >= deadline)
        {
            Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
        } else {
            None
        }
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
