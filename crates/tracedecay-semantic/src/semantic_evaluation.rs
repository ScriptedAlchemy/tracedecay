use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkV1, EmbeddingProjectionKeyV1,
    ProjectionBatchRequestV1,
};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticExecutionControl, SemanticQueryEmbeddingPort,
    SemanticQueryEmbeddingRequestV1,
};

use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, FastEmbedEmbeddingRuntime, SemanticExecutionAuthority,
    SemanticExecutionInterruptionV1,
};
use super::projector::{
    CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, prepare_vector_generation,
};
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

const EVALUATION_BATCH_CACHE_MAX_ENTRIES: usize = 3_072;
const EVALUATION_BATCH_CACHE_MAX_BYTES: u64 = 80 * 1024 * 1024;
const EVALUATION_BATCH_CACHE_ENTRY_OVERHEAD_BYTES: u64 = 4_096;

/// Controls whether one projection reaches the request-local exact-batch
/// cache. The cancellation probe must execute a real model batch even when a
/// clean projection has already produced identical input, so it bypasses both
/// cache lookup and insertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticEvaluationProjectionBatchCachePolicyV1 {
    ReuseCompletedBatches,
    Bypass,
}

/// Bounded, caller-owned cache for one semantic evaluation request. It is
/// intentionally neither global nor durable: its entries only bridge repeated
/// evaluator observations that share the same admitted model/runtime and
/// exact canonical tensor input.
pub struct SemanticEvaluationProjectionBatchCacheV1 {
    limits: SemanticEvaluationProjectionBatchCacheLimitsV1,
    state: Mutex<SemanticEvaluationProjectionBatchCacheStateV1>,
}

#[derive(Clone, Copy)]
struct SemanticEvaluationProjectionBatchCacheLimitsV1 {
    max_entries: usize,
    max_bytes: u64,
}

#[derive(Default)]
struct SemanticEvaluationProjectionBatchCacheStateV1 {
    entries: BTreeMap<SemanticEvaluationProjectionBatchCacheKeyV1, Vec<Vec<f32>>>,
    retained_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticEvaluationProjectionBatchCacheKeyV1 {
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
    /// FastEmbed's intra-op width can change floating-point numerics even
    /// with an otherwise identical admitted projection and tensor input.
    max_threads: u32,
    group_len: usize,
    tensor_batch_size: u32,
    tensor_dimensions: u32,
    ordered_sanitized_inputs: Vec<String>,
}

impl SemanticEvaluationProjectionBatchCacheV1 {
    pub fn new() -> Self {
        Self::with_limits(SemanticEvaluationProjectionBatchCacheLimitsV1 {
            max_entries: EVALUATION_BATCH_CACHE_MAX_ENTRIES,
            max_bytes: EVALUATION_BATCH_CACHE_MAX_BYTES,
        })
    }

    #[cfg(test)]
    fn with_limits_for_tests(max_entries: usize, max_bytes: u64) -> Self {
        Self::with_limits(SemanticEvaluationProjectionBatchCacheLimitsV1 {
            max_entries,
            max_bytes,
        })
    }

    fn with_limits(limits: SemanticEvaluationProjectionBatchCacheLimitsV1) -> Self {
        Self {
            limits,
            state: Mutex::new(SemanticEvaluationProjectionBatchCacheStateV1::default()),
        }
    }

    fn lock_state(
        &self,
    ) -> std::sync::MutexGuard<'_, SemanticEvaluationProjectionBatchCacheStateV1> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lookup(&self, key: &SemanticEvaluationProjectionBatchCacheKeyV1) -> Option<Vec<Vec<f32>>> {
        self.lock_state().entries.get(key).cloned()
    }

    fn insert(&self, key: SemanticEvaluationProjectionBatchCacheKeyV1, vectors: Vec<Vec<f32>>) {
        let retained_bytes = cache_entry_bytes(&key, &vectors);
        if self.limits.max_entries == 0
            || retained_bytes > self.limits.max_bytes
            || self.limits.max_bytes == 0
        {
            return;
        }
        let mut state = self.lock_state();
        if state.entries.contains_key(&key)
            || state.entries.len() >= self.limits.max_entries
            || state.retained_bytes.saturating_add(retained_bytes) > self.limits.max_bytes
        {
            return;
        }
        state.retained_bytes = state.retained_bytes.saturating_add(retained_bytes);
        state.entries.insert(key, vectors);
    }

    #[cfg(test)]
    fn entry_count_for_tests(&self) -> usize {
        self.lock_state().entries.len()
    }

    #[cfg(test)]
    fn retained_bytes_for_tests(&self) -> u64 {
        self.lock_state().retained_bytes
    }
}

impl Default for SemanticEvaluationProjectionBatchCacheV1 {
    fn default() -> Self {
        Self::new()
    }
}

fn cache_entry_bytes(
    key: &SemanticEvaluationProjectionBatchCacheKeyV1,
    vectors: &[Vec<f32>],
) -> u64 {
    // The map retains the complete identity. Its canonical JSON representation
    // conservatively includes every owned identity string plus field names, so
    // it bounds the retained identity without relying on a digest match.
    let identity_bytes = serde_json::to_vec(&key.admitted_projection)
        .ok()
        .and_then(|identity| u64::try_from(identity.len()).ok());
    // The map key owns the exact ordered input strings. Count their bytes and
    // allocation headers, then add the vector buffers at their actual
    // capacities rather than their logical lengths.
    let input_bytes = key
        .ordered_sanitized_inputs
        .iter()
        .try_fold(0_u64, |total, input| {
            total.checked_add(u64::try_from(input.len()).ok()?)
        });
    let input_headers = u64::try_from(key.ordered_sanitized_inputs.len())
        .ok()
        .and_then(|count| count.checked_mul(u64::try_from(std::mem::size_of::<String>()).ok()?));
    let vector_bytes = vectors.iter().try_fold(0_u64, |total, vector| {
        let bytes = u64::try_from(vector.capacity())
            .ok()?
            .checked_mul(u64::try_from(std::mem::size_of::<f32>()).ok()?)?;
        total.checked_add(bytes)
    });
    let container_headers = u64::try_from(std::mem::size_of::<
        SemanticEvaluationProjectionBatchCacheKeyV1,
    >())
    .ok()
    .and_then(|bytes| bytes.checked_add(u64::try_from(std::mem::size_of::<Vec<Vec<f32>>>()).ok()?))
    .and_then(|bytes| {
        bytes.checked_add(
            u64::try_from(vectors.len())
                .ok()?
                .checked_mul(u64::try_from(std::mem::size_of::<Vec<f32>>()).ok()?)?,
        )
    });
    identity_bytes
        .and_then(|identity| identity.checked_add(input_bytes?))
        .and_then(|bytes| bytes.checked_add(input_headers?))
        .and_then(|bytes| bytes.checked_add(vector_bytes?))
        .and_then(|bytes| bytes.checked_add(container_headers?))
        .and_then(|bytes| bytes.checked_add(EVALUATION_BATCH_CACHE_ENTRY_OVERHEAD_BYTES))
        .unwrap_or(u64::MAX)
}

struct CachedSemanticEvaluationChunkEncoderV1<'a, E> {
    inner: E,
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
    max_threads: u32,
    cache: &'a SemanticEvaluationProjectionBatchCacheV1,
    cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
}

impl<'a, E> CachedSemanticEvaluationChunkEncoderV1<'a, E> {
    fn new(
        inner: E,
        artifact_authority: &AdmittedProjectionArtifactV1,
        cache: &'a SemanticEvaluationProjectionBatchCacheV1,
        cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Self {
        Self {
            inner,
            admitted_projection: artifact_authority.projection().clone(),
            max_threads: artifact_authority.execution_max_threads(),
            cache,
            cache_policy,
            cancellation,
        }
    }

    fn cancellation_error(&self) -> Option<String> {
        self.cancellation
            .interruption()
            .map(|interruption| match interruption {
                SemanticExecutionInterruptionV1::Cancelled => {
                    "semantic projection cancelled".to_owned()
                }
                SemanticExecutionInterruptionV1::DeadlineExceeded => {
                    "semantic projection deadline exceeded".to_owned()
                }
            })
    }

    fn exact_key(
        &self,
        embedding_key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> SemanticEvaluationProjectionBatchCacheKeyV1 {
        SemanticEvaluationProjectionBatchCacheKeyV1 {
            admitted_projection: self.admitted_projection.clone(),
            max_threads: self.max_threads,
            group_len: chunks.len(),
            tensor_batch_size: embedding_key.inference_batch_size,
            tensor_dimensions: embedding_key.dimensions,
            ordered_sanitized_inputs: chunks
                .iter()
                .map(|chunk| chunk.sanitized_text.as_str().to_owned())
                .collect(),
        }
    }
}

impl<E> CanonicalChunkVectorEncoderV1 for CachedSemanticEvaluationChunkEncoderV1<'_, E>
where
    E: CanonicalChunkVectorEncoderV1,
{
    fn encode(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        let mut vectors = self.encode_batch(key, std::slice::from_ref(&chunk))?;
        if vectors.len() != 1 {
            return Err("semantic evaluator cache returned a non-unit vector batch".to_owned());
        }
        vectors
            .pop()
            .ok_or_else(|| "semantic evaluator cache returned an empty vector batch".to_owned())
    }

    fn encode_batch(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        let groups = [chunks];
        let mut encoded = self.encode_batches(key, &groups)?;
        if encoded.len() != 1 {
            return Err(
                "semantic evaluator cache returned an unexpected batch group count".to_owned(),
            );
        }
        encoded
            .pop()
            .ok_or_else(|| "semantic evaluator cache returned no batch group".to_owned())
    }

    fn encode_batches(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        groups: &[&[&CodeSearchChunkV1]],
    ) -> Result<Vec<Vec<Vec<f32>>>, String> {
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        if self.admitted_projection.embedding_key() != key {
            return Err("semantic projection authority changed".to_owned());
        }
        if let Some(error) = self.cancellation_error() {
            return Err(error);
        }
        if self.cache_policy == SemanticEvaluationProjectionBatchCachePolicyV1::Bypass {
            return self.inner.encode_batches(key, groups);
        }

        let mut encoded = vec![None; groups.len()];
        let mut unique_miss_indices =
            BTreeMap::<SemanticEvaluationProjectionBatchCacheKeyV1, usize>::new();
        let mut unique_misses = Vec::<(
            SemanticEvaluationProjectionBatchCacheKeyV1,
            usize,
            Vec<usize>,
        )>::new();
        for (position, group) in groups.iter().enumerate() {
            let cache_key = self.exact_key(key, group);
            if let Some(vectors) = self.cache.lookup(&cache_key) {
                encoded[position] = Some(vectors);
            } else if let Some(miss_index) = unique_miss_indices.get(&cache_key) {
                unique_misses[*miss_index].2.push(position);
            } else {
                unique_miss_indices.insert(cache_key.clone(), unique_misses.len());
                unique_misses.push((cache_key, position, vec![position]));
            }
        }
        if unique_misses.is_empty() {
            if let Some(error) = self.cancellation_error() {
                return Err(error);
            }
            return encoded
                .into_iter()
                .map(|group| {
                    group.ok_or_else(|| {
                        "semantic evaluator cache lost a completed vector group".to_owned()
                    })
                })
                .collect();
        }

        let miss_groups = unique_misses
            .iter()
            .map(|(_, position, _)| groups[*position])
            .collect::<Vec<_>>();
        let miss_encoded = self.inner.encode_batches(key, &miss_groups)?;
        if miss_encoded.len() != unique_misses.len() {
            return Err(
                "semantic evaluator returned an unexpected uncached vector group count".to_owned(),
            );
        }
        if let Some(error) = self.cancellation_error() {
            return Err(error);
        }
        for ((cache_key, _, _), vectors) in unique_misses.iter().zip(&miss_encoded) {
            if vectors.len() != cache_key.group_len {
                return Err(
                    "semantic evaluator returned an unexpected uncached vector batch size"
                        .to_owned(),
                );
            }
        }
        for ((cache_key, _, positions), vectors) in unique_misses.into_iter().zip(miss_encoded) {
            if let Some(error) = self.cancellation_error() {
                return Err(error);
            }
            self.cache.insert(cache_key, vectors.clone());
            for position in positions {
                encoded[position] = Some(vectors.clone());
            }
        }
        encoded
            .into_iter()
            .map(|group| {
                group.ok_or_else(|| {
                    "semantic evaluator cache lost an uncached vector group".to_owned()
                })
            })
            .collect()
    }

    fn encode_concurrency(&self) -> usize {
        self.inner.encode_concurrency()
    }
}

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
    cache: &SemanticEvaluationProjectionBatchCacheV1,
    cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
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
        Arc::clone(&cancellation),
    ));
    let inner = RuntimeChunkVectorEncoderV1::new(Arc::clone(&runtime), progress);
    let mut encoder = CachedSemanticEvaluationChunkEncoderV1::new(
        inner,
        authority.as_ref(),
        cache,
        cache_policy,
        cancellation,
    );
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
    cache: &SemanticEvaluationProjectionBatchCacheV1,
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
        Arc::clone(&cancellation),
    ));
    let inner = RuntimeChunkVectorEncoderV1::new(Arc::clone(&runtime), Arc::clone(&progress));
    let inner = CancelAfterFirstModelBatchV1 {
        inner,
        progress: Arc::clone(&progress),
    };
    let mut encoder = CachedSemanticEvaluationChunkEncoderV1::new(
        inner,
        authority.as_ref(),
        cache,
        SemanticEvaluationProjectionBatchCachePolicyV1::Bypass,
        cancellation,
    );
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use sha2::{Digest, Sha256};
    use tracedecay_domain::{
        BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
        CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
        ContentDigest, FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest,
        PolicyRevisionId, ProjectionBatchRequestV1, ProjectionReplayReasonV1, SanitizerRevision,
        SensitivityDecision, SensitivityLevelV1, SourceSpan,
    };

    use super::{
        CachedSemanticEvaluationChunkEncoderV1, CanonicalChunkVectorEncoderV1, CodeSearchChunkV1,
        EmbeddingProjectionKeyV1, SemanticEvaluationCancellationV1,
        SemanticEvaluationProjectionBatchCachePolicyV1, SemanticEvaluationProjectionBatchCacheV1,
        SemanticExecutionAuthority, SemanticExecutionInterruptionV1, prepare_vector_generation,
    };
    use crate::model_catalog::{CatalogMemberPinV1, CatalogSourceV1, CatalogedFastEmbedModelV1};
    use crate::{AdmittedProjectionArtifactV1, SemanticResourceCeilings};

    struct ActiveCancellation;

    impl SemanticExecutionAuthority for ActiveCancellation {
        fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
            None
        }
    }

    impl SemanticEvaluationCancellationV1 for ActiveCancellation {}

    struct TriggeredCancellation {
        cancelled: AtomicBool,
    }

    impl TriggeredCancellation {
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    impl SemanticExecutionAuthority for TriggeredCancellation {
        fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
            self.cancelled
                .load(Ordering::Acquire)
                .then_some(SemanticExecutionInterruptionV1::Cancelled)
        }
    }

    impl SemanticEvaluationCancellationV1 for TriggeredCancellation {}

    struct CountingEncoderV1 {
        group_invocations: usize,
        attempted_group_invocations: usize,
        fail: bool,
    }

    impl CountingEncoderV1 {
        fn healthy() -> Self {
            Self {
                group_invocations: 0,
                attempted_group_invocations: 0,
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                group_invocations: 0,
                attempted_group_invocations: 0,
                fail: true,
            }
        }
    }

    impl CanonicalChunkVectorEncoderV1 for CountingEncoderV1 {
        fn encode(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunk: &CodeSearchChunkV1,
        ) -> Result<Vec<f32>, String> {
            self.attempted_group_invocations = self.attempted_group_invocations.saturating_add(1);
            if self.fail {
                return Err("injected encoder failure".to_owned());
            }
            self.group_invocations = self.group_invocations.saturating_add(1);
            Ok(test_vector(key, chunk))
        }

        fn encode_batch(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunks: &[&CodeSearchChunkV1],
        ) -> Result<Vec<Vec<f32>>, String> {
            self.attempted_group_invocations = self.attempted_group_invocations.saturating_add(1);
            if self.fail {
                return Err("injected encoder failure".to_owned());
            }
            self.group_invocations = self.group_invocations.saturating_add(1);
            Ok(chunks.iter().map(|chunk| test_vector(key, chunk)).collect())
        }

        fn encode_batches(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            groups: &[&[&CodeSearchChunkV1]],
        ) -> Result<Vec<Vec<Vec<f32>>>, String> {
            self.attempted_group_invocations = self
                .attempted_group_invocations
                .saturating_add(groups.len());
            if self.fail {
                return Err("injected encoder failure".to_owned());
            }
            self.group_invocations = self.group_invocations.saturating_add(groups.len());
            Ok(groups
                .iter()
                .map(|group| group.iter().map(|chunk| test_vector(key, chunk)).collect())
                .collect())
        }
    }

    fn test_vector(key: &EmbeddingProjectionKeyV1, chunk: &CodeSearchChunkV1) -> Vec<f32> {
        vec![
            chunk.sanitized_text.as_str().len() as f32;
            usize::try_from(key.dimensions).expect("fixture dimensions")
        ]
    }

    struct CancellingEncoderV1 {
        inner: CountingEncoderV1,
        cancellation: Arc<TriggeredCancellation>,
    }

    impl CanonicalChunkVectorEncoderV1 for CancellingEncoderV1 {
        fn encode(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunk: &CodeSearchChunkV1,
        ) -> Result<Vec<f32>, String> {
            let encoded = self.inner.encode(key, chunk);
            self.cancellation.cancel();
            encoded
        }

        fn encode_batch(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunks: &[&CodeSearchChunkV1],
        ) -> Result<Vec<Vec<f32>>, String> {
            let encoded = self.inner.encode_batch(key, chunks);
            self.cancellation.cancel();
            encoded
        }

        fn encode_batches(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            groups: &[&[&CodeSearchChunkV1]],
        ) -> Result<Vec<Vec<Vec<f32>>>, String> {
            let encoded = self.inner.encode_batches(key, groups);
            self.cancellation.cancel();
            encoded
        }
    }

    fn cancellation() -> Arc<dyn SemanticEvaluationCancellationV1> {
        Arc::new(ActiveCancellation)
    }

    fn projection() -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        crate::session_pool::test_support::authority()
            .projection()
            .clone()
    }

    fn chunk(label: char, text: &str) -> CodeSearchChunkV1 {
        let generation = CodeGenerationId::new("evaluation-cache.generation".to_owned())
            .expect("generation fixture");
        CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("evaluation-cache.chunk.{label}"))
                .expect("chunk fixture"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation,
                file_occurrence_id: FileOccurrenceId::new(format!("{label}.rs"))
                    .expect("file fixture"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: u64::try_from(text.len()).expect("fixture source length"),
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: ContentDigest::new(format!("sha256:{}", label.to_string().repeat(64)))
                .expect("content fixture"),
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

    fn projection_case_chunks(
        generation: &CodeGenerationId,
        case: &str,
        count: usize,
    ) -> Vec<CodeSearchChunkV1> {
        (0..count)
            .map(|ordinal| {
                let label = format!("{case}.{ordinal:05}");
                CodeSearchChunkV1 {
                    id: CodeSearchChunkId::new(format!("evaluation-cache.chunk.{label}"))
                        .expect("chunk fixture"),
                    anchor: CodeSearchChunkAnchorV1 {
                        generation_id: generation.clone(),
                        file_occurrence_id: FileOccurrenceId::new(format!("{label}.rs"))
                            .expect("file fixture"),
                        symbol_occurrence_id: None,
                        parent_chunk_id: None,
                        source_span: SourceSpan {
                            start_byte: 0,
                            end_byte: 45,
                        },
                        grain: CodeSearchChunkGrainV1::FileWindow,
                        ordinal: u32::try_from(ordinal).expect("fixture ordinal"),
                    },
                    content_digest: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                        .expect("content fixture"),
                    language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                        .expect("language fixture"),
                    chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker fixture"),
                    sanitizer_revision: SanitizerRevision::new("sanitizer.v1")
                        .expect("sanitizer fixture"),
                    sensitivity: SensitivityDecision {
                        level: SensitivityLevelV1::Public,
                        policy_revision: PolicyRevisionId::new("policy.v1")
                            .expect("policy fixture"),
                    },
                    exact_terms: Vec::new(),
                    subtokens: Vec::new(),
                    sanitized_text: BoundedSanitizedText::new(
                        "identical canonical FastEmbed group input",
                    )
                    .expect("sanitized fixture"),
                }
            })
            .collect()
    }

    fn projection_case_request(
        generation: &CodeGenerationId,
        from_generation: Option<CodeGenerationId>,
        chunks: &[CodeSearchChunkV1],
        projection: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
        replay_reason: ProjectionReplayReasonV1,
    ) -> ProjectionBatchRequestV1 {
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation,
            to_generation: generation.clone(),
            manifest_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("manifest fixture"),
            added_or_changed: chunks
                .iter()
                .map(|chunk| ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                })
                .collect(),
            deleted: Vec::new(),
            reused: Vec::new(),
        };
        changes.manifest_digest = changes.compute_digest().expect("changed chunk digest");
        let mut request = ProjectionBatchRequestV1 {
            request_digest: changes.manifest_digest.clone(),
            changes,
            previous_projection_key: match replay_reason {
                ProjectionReplayReasonV1::SourceEdit => Some(projection.projection_key().clone()),
                _ => None,
            },
            target_projection_key: projection.projection_key().clone(),
            replay_reason,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("projection request digest");
        request
    }

    fn cached_encoder<'a>(
        inner: CountingEncoderV1,
        cache: &'a SemanticEvaluationProjectionBatchCacheV1,
        policy: SemanticEvaluationProjectionBatchCachePolicyV1,
    ) -> CachedSemanticEvaluationChunkEncoderV1<'a, CountingEncoderV1> {
        let authority = crate::session_pool::test_support::authority();
        cached_encoder_with_authority(inner, &authority, cache, policy)
    }

    fn cached_encoder_with_authority<'a>(
        inner: CountingEncoderV1,
        artifact_authority: &AdmittedProjectionArtifactV1,
        cache: &'a SemanticEvaluationProjectionBatchCacheV1,
        policy: SemanticEvaluationProjectionBatchCachePolicyV1,
    ) -> CachedSemanticEvaluationChunkEncoderV1<'a, CountingEncoderV1> {
        CachedSemanticEvaluationChunkEncoderV1::new(
            inner,
            artifact_authority,
            cache,
            policy,
            cancellation(),
        )
    }

    struct LifecycleAuthorityFixtureV1 {
        authority: AdmittedProjectionArtifactV1,
        _install: tempfile::TempDir,
    }

    fn lifecycle_authority_with_threads(max_threads: u32) -> LifecycleAuthorityFixtureV1 {
        let install = tempfile::tempdir().expect("lifecycle install");
        let mut members = BTreeMap::new();
        for (role, path, bytes) in [
            ("model", "model.onnx", b"model".as_slice()),
            ("tokenizer", "tokenizer.json", b"tokenizer".as_slice()),
            ("config", "config.json", b"config".as_slice()),
            (
                "special_tokens_map",
                "special_tokens_map.json",
                b"special-tokens-map".as_slice(),
            ),
            (
                "tokenizer_config",
                "tokenizer_config.json",
                b"tokenizer-config".as_slice(),
            ),
        ] {
            std::fs::write(install.path().join(path), bytes).expect("fixture member");
            members.insert(
                role.to_owned(),
                CatalogMemberPinV1 {
                    path: path.to_owned(),
                    upstream_path: path.to_owned(),
                    length: u64::try_from(bytes.len()).expect("fixture member length"),
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            );
        }
        let model = CatalogedFastEmbedModelV1 {
            model_id: "evaluation-cache-model".to_owned(),
            fastembed_enum: "JinaEmbeddingsV2BaseCode".to_owned(),
            model_code: "fixture/evaluation-cache-model".to_owned(),
            source: CatalogSourceV1 {
                upstream: "https://example.invalid/evaluation-cache".to_owned(),
                revision: "fixture-revision".to_owned(),
                license: "Apache-2.0".to_owned(),
                license_url: "https://example.invalid/license".to_owned(),
                provenance: "fixture".to_owned(),
            },
            expected_dimensions: 8,
            max_length: 512,
            members,
        };
        let authority = AdmittedProjectionArtifactV1::from_lifecycle_install(
            &model,
            install.path(),
            ChunkerRevision::new("chunker.v1").expect("chunker fixture"),
            tracedecay_domain::PrivacyDomainId::new("privacy.domain-a".to_owned())
                .expect("privacy fixture"),
            7,
            SemanticResourceCeilings {
                max_model_bytes: 1024,
                max_tokenizer_bytes: 1024,
                max_resident_bytes: 64 * 1024 * 1024,
                max_threads,
                max_concurrent_sessions: 1,
                max_batch_size: 8,
                max_sequence_length: 512,
                load_deadline_ms: 1_000,
            },
        )
        .expect("verified lifecycle authority");
        LifecycleAuthorityFixtureV1 {
            authority,
            _install: install,
        }
    }

    #[test]
    fn evaluator_projection_cases_reuse_exact_batches_byte_for_byte() {
        let projection = projection();
        let current_generation = CodeGenerationId::new("evaluation-cache.current".to_owned())
            .expect("current generation");
        let ten_x_generation =
            CodeGenerationId::new("evaluation-cache.ten-x".to_owned()).expect("ten-x generation");
        let incompatible_generation =
            CodeGenerationId::new("evaluation-cache.incompatible".to_owned())
                .expect("incompatible generation");
        let current_chunks = projection_case_chunks(&current_generation, "current", 8);
        let ten_x_chunks = projection_case_chunks(&ten_x_generation, "ten-x", 80);
        let incompatible_chunks =
            projection_case_chunks(&incompatible_generation, "incompatible", 8);
        let current_request = projection_case_request(
            &current_generation,
            None,
            &current_chunks,
            &projection,
            ProjectionReplayReasonV1::InitialProjection,
        );
        let ten_x_request = projection_case_request(
            &ten_x_generation,
            Some(current_generation.clone()),
            &ten_x_chunks,
            &projection,
            ProjectionReplayReasonV1::SourceEdit,
        );
        let incompatible_request = projection_case_request(
            &incompatible_generation,
            None,
            &incompatible_chunks,
            &projection,
            ProjectionReplayReasonV1::FullRebuildIncompatible,
        );

        let baseline_cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut baseline = cached_encoder(
            CountingEncoderV1::healthy(),
            &baseline_cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::Bypass,
        );
        let current_baseline = prepare_vector_generation(
            &projection,
            current_request.clone(),
            &current_chunks,
            &mut baseline,
        )
        .expect("current baseline projection");
        assert_eq!(baseline.inner.group_invocations, 1);
        let ten_x_baseline = prepare_vector_generation(
            &projection,
            ten_x_request.clone(),
            &ten_x_chunks,
            &mut baseline,
        )
        .expect("ten-x baseline projection");
        assert_eq!(baseline.inner.group_invocations, 11);
        let incompatible_baseline = prepare_vector_generation(
            &projection,
            incompatible_request.clone(),
            &incompatible_chunks,
            &mut baseline,
        )
        .expect("incompatible baseline projection");
        assert_eq!(baseline.inner.group_invocations, 12);

        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut cached = cached_encoder(
            CountingEncoderV1::healthy(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        let current_cached =
            prepare_vector_generation(&projection, current_request, &current_chunks, &mut cached)
                .expect("current cached projection");
        assert_eq!(cached.inner.group_invocations, 1);
        let ten_x_cached =
            prepare_vector_generation(&projection, ten_x_request, &ten_x_chunks, &mut cached)
                .expect("ten-x cached projection");
        assert_eq!(cached.inner.group_invocations, 1);
        let incompatible_cached = prepare_vector_generation(
            &projection,
            incompatible_request,
            &incompatible_chunks,
            &mut cached,
        )
        .expect("incompatible cached projection");

        assert_eq!(cached.inner.group_invocations, 1);
        assert_eq!(
            baseline.inner.group_invocations - cached.inner.group_invocations,
            11
        );
        assert_eq!(current_cached, current_baseline);
        assert_eq!(ten_x_cached, ten_x_baseline);
        assert_eq!(incompatible_cached, incompatible_baseline);
        assert_eq!(current_cached.vectors.len(), current_baseline.vectors.len());
        assert_eq!(ten_x_cached.vectors.len(), ten_x_baseline.vectors.len());
        assert_eq!(
            incompatible_cached.vectors.len(),
            incompatible_baseline.vectors.len()
        );
        assert_eq!(current_cached.vectors.len(), 8);
        assert_eq!(ten_x_cached.vectors.len(), 80);
        assert_eq!(incompatible_cached.vectors.len(), 8);
        assert_eq!(current_cached.receipt, current_baseline.receipt);
        assert_eq!(ten_x_cached.receipt, ten_x_baseline.receipt);
        assert_eq!(incompatible_cached.receipt, incompatible_baseline.receipt);
        assert_eq!(
            serde_json::to_vec(&current_cached.receipt).expect("current receipt bytes"),
            serde_json::to_vec(&current_baseline.receipt).expect("current baseline receipt bytes"),
        );
        assert_eq!(
            serde_json::to_vec(&ten_x_cached.receipt).expect("ten-x receipt bytes"),
            serde_json::to_vec(&ten_x_baseline.receipt).expect("ten-x baseline receipt bytes"),
        );
        assert_eq!(
            serde_json::to_vec(&incompatible_cached.receipt).expect("incompatible receipt bytes"),
            serde_json::to_vec(&incompatible_baseline.receipt)
                .expect("incompatible baseline receipt bytes"),
        );
        assert_eq!(
            serde_json::to_vec(&current_cached.vectors).expect("current output bytes"),
            serde_json::to_vec(&current_baseline.vectors).expect("current baseline output bytes"),
        );
        assert_eq!(
            serde_json::to_vec(&ten_x_cached.vectors).expect("ten-x output bytes"),
            serde_json::to_vec(&ten_x_baseline.vectors).expect("ten-x baseline output bytes"),
        );
        assert_eq!(
            serde_json::to_vec(&incompatible_cached.vectors).expect("incompatible output bytes"),
            serde_json::to_vec(&incompatible_baseline.vectors)
                .expect("incompatible baseline output bytes"),
        );
        assert_eq!(cache.entry_count_for_tests(), 1);
    }

    #[test]
    fn failures_are_not_cached() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let chunk = chunk('a', "fails");
        let group = [&chunk];
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut cached = cached_encoder(
            CountingEncoderV1::failing(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );

        assert!(
            cached
                .encode_batches(&embedding_key, &[group.as_slice()])
                .is_err()
        );
        assert!(
            cached
                .encode_batches(&embedding_key, &[group.as_slice()])
                .is_err()
        );
        assert_eq!(cached.inner.attempted_group_invocations, 2);
        assert_eq!(cached.inner.group_invocations, 0);
        assert_eq!(cache.entry_count_for_tests(), 0);
    }

    #[test]
    fn changed_fastembed_intra_op_threads_force_an_exact_batch_cache_miss() {
        let one_thread_authority = lifecycle_authority_with_threads(1);
        let four_thread_authority = lifecycle_authority_with_threads(4);
        assert_eq!(
            one_thread_authority.authority.projection(),
            four_thread_authority.authority.projection(),
            "the admitted projection identity must stay fixed while only the verified runtime width changes",
        );
        assert_eq!(one_thread_authority.authority.execution_max_threads(), 1);
        assert_eq!(four_thread_authority.authority.execution_max_threads(), 4);
        let embedding_key = one_thread_authority
            .authority
            .projection()
            .embedding_key()
            .clone();
        let chunk = chunk('a', "same tensor with a different numerics width");
        let group = [&chunk];
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut one_thread = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &one_thread_authority.authority,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        one_thread
            .encode_batches(&embedding_key, &[group.as_slice()])
            .expect("one-thread batch");
        let mut four_threads = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &four_thread_authority.authority,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        four_threads
            .encode_batches(&embedding_key, &[group.as_slice()])
            .expect("four-thread batch must not reuse one-thread numerics");

        assert_eq!(one_thread.inner.group_invocations, 1);
        assert_eq!(four_threads.inner.group_invocations, 1);
        assert_eq!(cache.entry_count_for_tests(), 2);
    }

    #[test]
    fn cache_capacity_refuses_new_batches_without_scan_thrashing_retained_entries() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let cache = SemanticEvaluationProjectionBatchCacheV1::with_limits_for_tests(2, u64::MAX);
        let mut cached = cached_encoder(
            CountingEncoderV1::healthy(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        let first = chunk('a', "first");
        let second = chunk('b', "second");
        let third = chunk('c', "third");
        for chunk in [&first, &second, &third] {
            cached
                .encode_batches(&embedding_key, &[std::slice::from_ref(&chunk)])
                .expect("bounded cache encode");
        }

        assert_eq!(cache.entry_count_for_tests(), 2);
        assert!(cache.retained_bytes_for_tests() > 0);
        assert_eq!(cached.inner.group_invocations, 3);

        for chunk in [&first, &second] {
            cached
                .encode_batches(&embedding_key, &[std::slice::from_ref(&chunk)])
                .expect("retained cache hit");
        }
        assert_eq!(
            cached.inner.group_invocations, 3,
            "a full cache must retain earlier batches instead of evicting them during a scan"
        );

        let third_group = [&third];
        cached
            .encode_batches(&embedding_key, &[&third_group])
            .expect("non-admitted batch retry");
        assert_eq!(cached.inner.group_invocations, 4);
        assert_eq!(cache.entry_count_for_tests(), 2);
    }

    #[test]
    fn cancellation_after_a_real_batch_does_not_cache_the_completed_vectors() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let chunk = chunk('a', "cancel after model work");
        let group = [&chunk];
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let artifact_authority = crate::session_pool::test_support::authority();
        let cancellation = Arc::new(TriggeredCancellation {
            cancelled: AtomicBool::new(false),
        });
        let mut cancelled = CachedSemanticEvaluationChunkEncoderV1::new(
            CancellingEncoderV1 {
                inner: CountingEncoderV1::healthy(),
                cancellation: Arc::clone(&cancellation),
            },
            &artifact_authority,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
            cancellation as Arc<dyn SemanticEvaluationCancellationV1>,
        );

        assert!(
            cancelled
                .encode_batches(&embedding_key, &[group.as_slice()])
                .is_err()
        );
        assert_eq!(cancelled.inner.inner.group_invocations, 1);
        assert_eq!(cache.entry_count_for_tests(), 0);

        let mut retry = cached_encoder(
            CountingEncoderV1::healthy(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        retry
            .encode_batches(&embedding_key, &[group.as_slice()])
            .expect("uncached retry after cancellation");
        assert_eq!(retry.inner.group_invocations, 1);
    }

    #[test]
    fn cancellation_bypass_executes_the_model_despite_an_exact_completed_entry() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let chunk = chunk('a', "cancelled probe input");
        let group = [&chunk];
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut cached = cached_encoder(
            CountingEncoderV1::healthy(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        cached
            .encode_batches(&embedding_key, &[group.as_slice()])
            .expect("completed cache entry");
        assert_eq!(cached.inner.group_invocations, 1);

        let mut cancellation_probe = cached_encoder(
            CountingEncoderV1::healthy(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::Bypass,
        );
        cancellation_probe
            .encode_batches(&embedding_key, &[group.as_slice()])
            .expect("cancellation probe must execute its own model batch");
        assert_eq!(cancellation_probe.inner.attempted_group_invocations, 1);
        assert_eq!(cancellation_probe.inner.group_invocations, 1);
        assert_eq!(cache.entry_count_for_tests(), 1);
    }
}
