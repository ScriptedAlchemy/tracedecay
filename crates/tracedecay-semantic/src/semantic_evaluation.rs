use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use tracedecay_code_index::embedding_document::{EmbeddingDocumentComposerV1, EmbeddingDocumentV1};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkV1, EmbeddingProjectionKeyV1,
    ProjectionBatchRequestV1,
};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticExecutionControl, SemanticQueryEmbeddingPort,
    SemanticQueryEmbeddingRequestV1,
};
use tracedecay_semantic_contracts::SemanticRuntimeScheduleFailureV1;

use super::embedding_backend::{ProductionEmbeddingRuntime, production_embedding_runtime_factory};
use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, EmbeddingRuntime, SemanticExecutionAuthority,
    SemanticExecutionInterruptionV1,
};
use super::projector::{
    CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, prepare_vector_generation,
};
use super::runtime_query::{PooledSemanticQueryEmbedder, PooledSemanticQueryEmbedderFactory};
use super::runtime_service::{
    SemanticRuntimeScheduleCancellationV1, SemanticRuntimeService, SharedEmbeddingRuntimeFactory,
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

/// Bounded store of completed projection batches, owned for as long as its
/// holder chooses. It is not durable and never leaves the process: its entries
/// only bridge repeated evaluator observations that share the same admitted
/// model/runtime and exact canonical tensor input.
///
/// A daemon-lifetime owner keeps one store across qualification requests and
/// calls [`Self::release`] when it shuts down. Each request takes its own
/// [`SemanticEvaluationProjectionBatchCacheV1`] handle from
/// [`Self::request_cache`]; the retained bytes stay under `max_bytes`
/// regardless of how many requests are in flight.
pub struct SemanticEvaluationProjectionBatchStoreV1 {
    limits: SemanticEvaluationProjectionBatchCacheLimitsV1,
    state: Mutex<SemanticEvaluationProjectionBatchCacheStateV1>,
    /// Signalled whenever a build claim is resolved (installed or abandoned),
    /// so a waiter for that exact batch can re-read the entry.
    resolved: Condvar,
}

/// One request's view of a projection batch store.
///
/// The handle carries that request's fill epoch, which every projection pass
/// the request runs shares. Entries the request has touched are pinned against
/// eviction until it drops the handle, so a later pass over the same corpus
/// can never evict what an earlier pass of the same request retained.
pub struct SemanticEvaluationProjectionBatchCacheV1 {
    store: Arc<SemanticEvaluationProjectionBatchStoreV1>,
    fill_epoch: u64,
}

impl Drop for SemanticEvaluationProjectionBatchCacheV1 {
    fn drop(&mut self) {
        self.store.end_fill(self.fill_epoch);
    }
}

#[derive(Clone, Copy)]
struct SemanticEvaluationProjectionBatchCacheLimitsV1 {
    max_entries: usize,
    max_bytes: u64,
}

struct SemanticEvaluationProjectionBatchCacheEntryV1 {
    vectors: Vec<Vec<f32>>,
    retained_bytes: u64,
    /// Fill epoch of the most recent read or write. Only entries older than
    /// the caller's current fill may be evicted, so one projection scan can
    /// never evict a batch it is about to reuse.
    last_used_epoch: u64,
}

#[derive(Default)]
struct SemanticEvaluationProjectionBatchCacheStateV1 {
    entries: BTreeMap<
        SemanticEvaluationProjectionBatchCacheKeyV1,
        SemanticEvaluationProjectionBatchCacheEntryV1,
    >,
    in_flight: BTreeSet<SemanticEvaluationProjectionBatchCacheKeyV1>,
    retained_bytes: u64,
    next_epoch: u64,
    /// Fill epochs of the requests currently holding a handle. An entry last
    /// touched by one of them is pinned: evicting it would make a request that
    /// is still running re-embed its own corpus.
    active_fills: BTreeSet<u64>,
}

/// Outcome of asking the cache for one batch.
enum SemanticEvaluationProjectionBatchClaimV1<'cache> {
    /// Another pass already produced these exact vectors.
    Hit(Vec<Vec<f32>>),
    /// This caller owns construction of the batch; nobody else will build it
    /// until the guard is dropped or the vectors are installed.
    Build(SemanticEvaluationProjectionBatchBuildGuardV1<'cache>),
}

/// Exclusive right to build one cache entry. Dropping it without installing
/// vectors -- because the request was cancelled or the model failed -- releases
/// the claim and wakes the waiters, who then build it themselves. A failed or
/// cancelled request therefore never leaves another request without its batch.
struct SemanticEvaluationProjectionBatchBuildGuardV1<'cache> {
    store: &'cache SemanticEvaluationProjectionBatchStoreV1,
    /// Boxed: the key owns the batch's whole composed input, which does not
    /// belong inline in the claim enum next to a plain cache hit.
    key: Option<Box<SemanticEvaluationProjectionBatchCacheKeyV1>>,
}

impl SemanticEvaluationProjectionBatchBuildGuardV1<'_> {
    fn key(&self) -> &SemanticEvaluationProjectionBatchCacheKeyV1 {
        self.key
            .as_deref()
            .expect("a build guard owns its key until it is dropped")
    }
}

impl Drop for SemanticEvaluationProjectionBatchBuildGuardV1<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.store.lock_state().in_flight.remove(&key);
            self.store.resolved.notify_all();
        }
    }
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
    /// The exact composed documents the model would receive, in group order.
    /// Composition is keyed here as well as in the admitted projection: two
    /// generations can share chunk text yet differ in symbol context.
    ordered_documents: Vec<String>,
}

impl SemanticEvaluationProjectionBatchStoreV1 {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_limits(SemanticEvaluationProjectionBatchCacheLimitsV1 {
            max_entries: EVALUATION_BATCH_CACHE_MAX_ENTRIES,
            max_bytes: EVALUATION_BATCH_CACHE_MAX_BYTES,
        })
    }

    #[cfg(test)]
    fn with_limits_for_tests(max_entries: usize, max_bytes: u64) -> Arc<Self> {
        Self::with_limits(SemanticEvaluationProjectionBatchCacheLimitsV1 {
            max_entries,
            max_bytes,
        })
    }

    fn with_limits(limits: SemanticEvaluationProjectionBatchCacheLimitsV1) -> Arc<Self> {
        Arc::new(Self {
            limits,
            state: Mutex::new(SemanticEvaluationProjectionBatchCacheStateV1::default()),
            resolved: Condvar::new(),
        })
    }

    /// Open one request's view of this store. Every projection pass the
    /// request runs shares the returned handle's fill epoch, so the request
    /// can never evict the batches its own earlier passes retained.
    #[must_use]
    pub fn request_cache(self: &Arc<Self>) -> SemanticEvaluationProjectionBatchCacheV1 {
        let fill_epoch = {
            let mut state = self.lock_state();
            let fill_epoch = state.next_epoch.saturating_add(1);
            state.next_epoch = fill_epoch;
            state.active_fills.insert(fill_epoch);
            fill_epoch
        };
        SemanticEvaluationProjectionBatchCacheV1 {
            store: Arc::clone(self),
            fill_epoch,
        }
    }

    fn end_fill(&self, fill_epoch: u64) {
        self.lock_state().active_fills.remove(&fill_epoch);
    }

    /// Bytes currently retained by cached batches. Never above the byte bound.
    pub fn retained_bytes(&self) -> u64 {
        self.lock_state().retained_bytes
    }

    /// Cached batches currently retained.
    pub fn entry_count(&self) -> usize {
        self.lock_state().entries.len()
    }

    /// Configured byte bound.
    pub fn max_bytes(&self) -> u64 {
        self.limits.max_bytes
    }

    /// Drop every cached batch and release its bytes. A daemon-lifetime owner
    /// calls this on shutdown; in-flight build claims are untouched, and their
    /// guards resolve normally.
    pub fn release(&self) {
        let mut state = self.lock_state();
        state.entries.clear();
        state.retained_bytes = 0;
    }

    fn lock_state(
        &self,
    ) -> std::sync::MutexGuard<'_, SemanticEvaluationProjectionBatchCacheStateV1> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read one batch, or take the exclusive right to build it, waiting while
    /// another caller is already building that exact batch.
    ///
    /// `interrupted` is polled between waits so a cancelled or expired request
    /// stops waiting instead of blocking on someone else's model work.
    fn claim(
        &self,
        key: &SemanticEvaluationProjectionBatchCacheKeyV1,
        fill_epoch: u64,
        interrupted: &dyn Fn() -> Option<String>,
    ) -> Result<SemanticEvaluationProjectionBatchClaimV1<'_>, String> {
        let mut state = self.lock_state();
        loop {
            if let Some(entry) = state.entries.get_mut(key) {
                entry.last_used_epoch = fill_epoch;
                return Ok(SemanticEvaluationProjectionBatchClaimV1::Hit(
                    entry.vectors.clone(),
                ));
            }
            if !state.in_flight.contains(key) {
                state.in_flight.insert(key.clone());
                return Ok(SemanticEvaluationProjectionBatchClaimV1::Build(
                    SemanticEvaluationProjectionBatchBuildGuardV1 {
                        store: self,
                        key: Some(Box::new(key.clone())),
                    },
                ));
            }
            if let Some(error) = interrupted() {
                return Err(error);
            }
            // ponytail: fixed 250ms wait slice so cancellation is observed
            // promptly; a notified waiter wakes immediately either way.
            let (next, _) = self
                .resolved
                .wait_timeout(state, std::time::Duration::from_millis(250))
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }

    /// Install the vectors this caller built, then release its claim.
    ///
    /// Admission evicts only batches no request currently holding a handle has
    /// touched, so the bound is honoured without a running request ever
    /// evicting a batch it is still using. When nothing is evictable the batch
    /// is simply not retained.
    fn install(
        &self,
        guard: SemanticEvaluationProjectionBatchBuildGuardV1<'_>,
        vectors: &[Vec<f32>],
        fill_epoch: u64,
    ) {
        let retained_bytes = cache_entry_bytes(guard.key(), vectors);
        if self.limits.max_entries == 0
            || self.limits.max_bytes == 0
            || retained_bytes > self.limits.max_bytes
        {
            return;
        }
        let mut state = self.lock_state();
        if state.entries.contains_key(guard.key()) {
            return;
        }
        while state.entries.len() >= self.limits.max_entries
            || state.retained_bytes.saturating_add(retained_bytes) > self.limits.max_bytes
        {
            // ponytail: linear least-recently-used scan, bounded by
            // max_entries; swap in a recency index if eviction ever shows up
            // next to a 200ms model batch.
            let evictable = state
                .entries
                .iter()
                .filter(|(_, entry)| !state.active_fills.contains(&entry.last_used_epoch))
                .min_by_key(|(_, entry)| entry.last_used_epoch)
                .map(|(key, _)| key.clone());
            let Some(evictable) = evictable else {
                return;
            };
            if let Some(evicted) = state.entries.remove(&evictable) {
                state.retained_bytes = state.retained_bytes.saturating_sub(evicted.retained_bytes);
            }
        }
        state.retained_bytes = state.retained_bytes.saturating_add(retained_bytes);
        state.entries.insert(
            guard.key().clone(),
            SemanticEvaluationProjectionBatchCacheEntryV1 {
                vectors: vectors.to_vec(),
                retained_bytes,
                last_used_epoch: fill_epoch,
            },
        );
        // Release the state lock before the claim guard, which takes it again
        // to wake the waiters for this batch.
        drop(state);
    }
}

impl SemanticEvaluationProjectionBatchCacheV1 {
    /// A standalone request cache over a private store. Callers that own the
    /// whole cache for one request use this; a daemon-lifetime owner keeps a
    /// [`SemanticEvaluationProjectionBatchStoreV1`] and hands out
    /// [`SemanticEvaluationProjectionBatchStoreV1::request_cache`] handles.
    #[must_use]
    pub fn new() -> Self {
        SemanticEvaluationProjectionBatchStoreV1::new().request_cache()
    }

    #[cfg(test)]
    fn with_limits_for_tests(max_entries: usize, max_bytes: u64) -> Self {
        SemanticEvaluationProjectionBatchStoreV1::with_limits_for_tests(max_entries, max_bytes)
            .request_cache()
    }

    /// Bytes currently retained by the backing store.
    pub fn retained_bytes(&self) -> u64 {
        self.store.retained_bytes()
    }

    /// Cached batches currently retained by the backing store.
    pub fn entry_count(&self) -> usize {
        self.store.entry_count()
    }

    /// The backing store's byte bound.
    pub fn max_bytes(&self) -> u64 {
        self.store.max_bytes()
    }

    #[cfg(test)]
    fn entry_count_for_tests(&self) -> usize {
        self.entry_count()
    }

    #[cfg(test)]
    fn retained_bytes_for_tests(&self) -> u64 {
        self.retained_bytes()
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
        .ordered_documents
        .iter()
        .try_fold(0_u64, |total, input| {
            total.checked_add(u64::try_from(input.len()).ok()?)
        });
    let input_headers = u64::try_from(key.ordered_documents.len())
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
    /// This projection scan's fill epoch. Batches it touches are pinned
    /// against eviction until it finishes.
    fill_epoch: u64,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    documents: Arc<EmbeddingDocumentComposerV1>,
}

impl<'a, E> CachedSemanticEvaluationChunkEncoderV1<'a, E> {
    fn new(
        inner: E,
        artifact_authority: &AdmittedProjectionArtifactV1,
        cache: &'a SemanticEvaluationProjectionBatchCacheV1,
        cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
        documents: Arc<EmbeddingDocumentComposerV1>,
    ) -> Self {
        Self {
            inner,
            admitted_projection: artifact_authority.projection().clone(),
            max_threads: u32::try_from(artifact_authority.embedding_execution_plan().intra_threads)
                .unwrap_or(u32::MAX),
            fill_epoch: cache.fill_epoch,
            cache,
            cache_policy,
            cancellation,
            documents,
        }
    }

    fn cancellation_error(&self) -> Option<String> {
        semantic_execution_interruption_error(self.cancellation.as_ref())
    }
}

fn semantic_execution_interruption_error(
    cancellation: &dyn SemanticEvaluationCancellationV1,
) -> Option<String> {
    cancellation
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

impl<'a, E> CachedSemanticEvaluationChunkEncoderV1<'a, E> {
    fn exact_key(
        &self,
        embedding_key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<SemanticEvaluationProjectionBatchCacheKeyV1, String> {
        let ordered_documents = chunks
            .iter()
            .map(|chunk| {
                self.documents
                    .compose(embedding_key, chunk)
                    .map(EmbeddingDocumentV1::into_text)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SemanticEvaluationProjectionBatchCacheKeyV1 {
            admitted_projection: self.admitted_projection.clone(),
            max_threads: self.max_threads,
            group_len: chunks.len(),
            tensor_batch_size: embedding_key.inference_batch_size,
            tensor_dimensions: embedding_key.dimensions,
            ordered_documents,
        })
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

        let store = self.cache.store.as_ref();
        let fill_epoch = self.fill_epoch;
        let cancellation = Arc::clone(&self.cancellation);
        let interrupted = move || semantic_execution_interruption_error(cancellation.as_ref());
        let mut encoded = vec![None; groups.len()];
        let mut distinct =
            BTreeMap::<SemanticEvaluationProjectionBatchCacheKeyV1, Vec<usize>>::new();
        for (position, group) in groups.iter().enumerate() {
            distinct
                .entry(self.exact_key(key, group)?)
                .or_default()
                .push(position);
        }
        let mut unique_misses = Vec::<(
            SemanticEvaluationProjectionBatchBuildGuardV1<'_>,
            usize,
            Vec<usize>,
        )>::new();
        // Claims are taken in cache-key order. A claim may wait for another
        // request's build while this one already holds claims, so every
        // request must acquire in the same total order or two requests whose
        // batches overlap could wait on each other.
        for (cache_key, positions) in distinct {
            // One builder per distinct batch: a concurrent request that wants
            // the same batch waits here instead of running the model again.
            match store.claim(&cache_key, fill_epoch, &interrupted)? {
                SemanticEvaluationProjectionBatchClaimV1::Hit(vectors) => {
                    for position in positions {
                        encoded[position] = Some(vectors.clone());
                    }
                }
                SemanticEvaluationProjectionBatchClaimV1::Build(guard) => {
                    let first = *positions
                        .first()
                        .expect("a distinct cache key has at least one group position");
                    unique_misses.push((guard, first, positions));
                }
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
        for ((guard, _, _), vectors) in unique_misses.iter().zip(&miss_encoded) {
            if vectors.len() != guard.key().group_len {
                return Err(
                    "semantic evaluator returned an unexpected uncached vector batch size"
                        .to_owned(),
                );
            }
        }
        for ((guard, _, positions), vectors) in unique_misses.into_iter().zip(miss_encoded) {
            if let Some(error) = self.cancellation_error() {
                return Err(error);
            }
            store.install(guard, &vectors, fill_epoch);
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
    inner: Arc<PooledSemanticQueryEmbedderFactory<ProductionEmbeddingRuntime>>,
}

/// Isolated evaluator projection. It reuses the verified production artifact
/// and `FastEmbed` runtime, but has no durable vector pointer and cannot replace
/// a project's active generation.
pub struct PreparedSemanticEvaluationProjectionV1 {
    pub query_factory: SemanticEvaluationQueryFactoryV1,
    pub prepared: PreparedVectorGenerationV1,
}

/// Process-local resource ceilings for one evaluator projection runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationProjectionResourcesV1 {
    pub memory_ceiling_bytes: u64,
}

fn semantic_evaluation_runtime<R>(
    authority: Arc<AdmittedProjectionArtifactV1>,
    factory: SharedEmbeddingRuntimeFactory<R>,
    resources: SemanticEvaluationProjectionResourcesV1,
) -> Result<Arc<SemanticRuntimeService<R>>, SemanticRuntimeScheduleFailureV1>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    SemanticRuntimeService::new_owned(
        authority,
        factory,
        SessionPoolConfigV1 {
            // Qualification is one request-scoped model owner. Projection
            // groups and genuine queries reuse that one session; inheriting
            // the serving/indexing fan-out would construct another complete
            // model for the same ephemeral request.
            max_sessions: 1,
            max_queued_waiters: 0,
            idle_timeout: std::time::Duration::from_mins(5),
            memory_ceiling_bytes: resources.memory_ceiling_bytes,
        },
    )
    .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)
}

#[expect(
    clippy::too_many_arguments,
    reason = "each argument is a distinct caller-owned authority of one evaluator projection"
)]
pub fn prepare_semantic_evaluation_projection(
    artifact: LoadedSemanticArtifactV1,
    request_query_factory: Option<&SemanticEvaluationQueryFactoryV1>,
    request: ProjectionBatchRequestV1,
    canonical_chunks: &[Arc<CodeSearchChunkV1>],
    documents: Arc<EmbeddingDocumentComposerV1>,
    resources: SemanticEvaluationProjectionResourcesV1,
    cache: &SemanticEvaluationProjectionBatchCacheV1,
    cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
) -> Result<PreparedSemanticEvaluationProjectionV1, SemanticRuntimeScheduleFailureV1> {
    if let Some(interruption) = cancellation.interruption() {
        return Err(schedule_interruption(interruption));
    }
    if documents.symbols().generation_id() != &request.changes.to_generation {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    let authority = artifact.into_authority();
    let factory: SharedEmbeddingRuntimeFactory<ProductionEmbeddingRuntime> =
        production_embedding_runtime_factory();
    let (runtime, query_factory) = match request_query_factory {
        Some(query_factory) => {
            let (_, active_authority, _) = query_factory.inner.runtime().active_snapshot();
            if active_authority.as_ref() != authority.as_ref() {
                return Err(SemanticRuntimeScheduleFailureV1::Projection);
            }
            (
                Arc::clone(query_factory.inner.runtime()),
                query_factory.clone(),
            )
        }
        None => {
            let runtime = semantic_evaluation_runtime(Arc::clone(&authority), factory, resources)?;
            // Open this request's one model session up front and let the pool
            // time it. The cold load is acceptance evidence -- the request's
            // own observation that the admitted runtime opens within its
            // deadline -- so it cannot depend on whether a projection batch
            // happened to miss the exact-batch cache. The session returns to
            // the pool immediately and every later projection group and query
            // reuses it, so the request still opens exactly one session.
            runtime
                .warm_query_session()
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
            let query_factory = SemanticEvaluationQueryFactoryV1::from_runtime(
                PooledSemanticQueryEmbedderFactory::new(Arc::clone(&runtime)),
            );
            (runtime, query_factory)
        }
    };
    let progress = Arc::new(SemanticRuntimeScheduleCancellationV1::new_linked(
        request.changes.added_or_changed.len().max(1) as u64,
        Arc::clone(&cancellation),
    ));
    let inner = RuntimeChunkVectorEncoderV1::new(
        Arc::clone(&runtime),
        progress,
        authority.embedding_execution_plan(),
        Arc::clone(&documents),
    );
    let mut encoder = CachedSemanticEvaluationChunkEncoderV1::new(
        inner,
        authority.as_ref(),
        cache,
        cache_policy,
        cancellation,
        documents,
    );
    let prepared = prepare_vector_generation(
        authority.projection(),
        request,
        canonical_chunks,
        &mut encoder,
    )
    .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    drop(encoder);
    Ok(PreparedSemanticEvaluationProjectionV1 {
        query_factory,
        prepared,
    })
}

/// Execute one genuine model batch and then cancel before a complete
/// evaluator projection can be returned or published.
pub fn measure_semantic_evaluation_projection_cancellation(
    artifact: LoadedSemanticArtifactV1,
    query_factory: &SemanticEvaluationQueryFactoryV1,
    request: ProjectionBatchRequestV1,
    canonical_chunks: &[Arc<CodeSearchChunkV1>],
    documents: Arc<EmbeddingDocumentComposerV1>,
    cache: &SemanticEvaluationProjectionBatchCacheV1,
    cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
) -> Result<SemanticEvaluationProjectionCancellationV1, SemanticRuntimeScheduleFailureV1> {
    if request.changes.added_or_changed.is_empty()
        || documents.symbols().generation_id() != &request.changes.to_generation
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    if let Some(interruption) = cancellation.interruption() {
        return Err(schedule_interruption(interruption));
    }
    let chunks_added_or_changed = request.changes.added_or_changed.len() as u64;
    let authority = artifact.into_authority();
    let (_, active_authority, _) = query_factory.inner.runtime().active_snapshot();
    if active_authority.as_ref() != authority.as_ref() {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    let runtime = Arc::clone(query_factory.inner.runtime());
    let progress = Arc::new(SemanticRuntimeScheduleCancellationV1::new_linked(
        request.changes.added_or_changed.len() as u64,
        Arc::clone(&cancellation),
    ));
    let inner = RuntimeChunkVectorEncoderV1::new(
        Arc::clone(&runtime),
        Arc::clone(&progress),
        authority.embedding_execution_plan(),
        Arc::clone(&documents),
    );
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
        documents,
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
    inner: RuntimeChunkVectorEncoderV1<ProductionEmbeddingRuntime>,
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
        inner: Arc<PooledSemanticQueryEmbedderFactory<ProductionEmbeddingRuntime>>,
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

    /// Number of model sessions opened by this request-scoped runtime.
    pub fn model_open_count(&self) -> usize {
        self.inner.runtime().stats().sessions_opened
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
    inner: PooledSemanticQueryEmbedder<'a, ProductionEmbeddingRuntime>,
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
    use tracedecay_code_index::embedding_document::{
        EmbeddingDocumentComposerV1, EmbeddingSymbolContextIndexV1,
    };
    use tracedecay_code_index::lineage::GenerationSymbolIndexV1;
    use tracedecay_domain::{
        BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
        CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
        ContentDigest, EmbeddingDocumentCompositionV1, EphemeralSanitizedQueryViewV1,
        FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId,
        ProjectionBatchRequestV1, ProjectionReplayReasonV1, QueryDigest, QueryMac,
        QueryNormalizationRevision, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
        SourceSpan,
    };
    use tracedecay_query::retrieval::semantic::{
        SemanticQueryEmbeddingPort, SemanticQueryEmbeddingRequestV1,
    };
    use tracedecay_semantic_contracts::SemanticResourceCeilings;

    use super::{
        CachedSemanticEvaluationChunkEncoderV1, CanonicalChunkVectorEncoderV1, CodeSearchChunkV1,
        EmbeddingProjectionKeyV1, SemanticEvaluationCancellationV1,
        SemanticEvaluationProjectionBatchCachePolicyV1, SemanticEvaluationProjectionBatchCacheV1,
        SemanticEvaluationProjectionBatchStoreV1, SemanticEvaluationProjectionResourcesV1,
        SemanticExecutionAuthority, SemanticExecutionInterruptionV1, prepare_vector_generation,
        semantic_evaluation_runtime,
    };
    use crate::AdmittedProjectionArtifactV1;
    use crate::RuntimeChunkVectorEncoderV1;
    use crate::embedding_parallelism::{
        EmbeddingExecutionPlanV1, EmbeddingSessionLimitingReasonV1,
    };
    use crate::fastembed_adapter::{FakeEmbeddingRuntime, ManualCancellation};
    use crate::model_catalog::{
        CatalogMemberPinV1, CatalogSourceV1, CatalogedEmbeddingBackendV1, CatalogedFastEmbedModelV1,
    };
    use crate::runtime_query::PooledSemanticQueryEmbedderFactory;
    use crate::runtime_service::{
        SemanticRuntimeScheduleCancellationV1, SharedEmbeddingRuntimeFactory,
    };

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

    /// A symbol-free composer. Every fixture projection here embeds sanitized
    /// text, which consults no symbol index.
    fn documents() -> Arc<EmbeddingDocumentComposerV1> {
        let generation = CodeGenerationId::new("evaluation-cache.generation".to_owned())
            .expect("generation fixture");
        let index =
            GenerationSymbolIndexV1::new(generation, Vec::new()).expect("empty symbol index");
        Arc::new(EmbeddingDocumentComposerV1::new(
            EmbeddingSymbolContextIndexV1::from_generation_symbols(&index),
        ))
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
    ) -> Vec<Arc<CodeSearchChunkV1>> {
        (0..count)
            .map(|ordinal| {
                let label = format!("{case}.{ordinal:05}");
                Arc::new(CodeSearchChunkV1 {
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
                })
            })
            .collect()
    }

    fn projection_case_request(
        generation: &CodeGenerationId,
        from_generation: Option<CodeGenerationId>,
        chunks: &[Arc<CodeSearchChunkV1>],
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
            documents(),
        )
    }

    #[test]
    fn native_qualification_projection_and_query_share_exactly_one_model_session() {
        let authority = Arc::new(crate::session_pool::test_support::authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1_024)));
        let runtime = semantic_evaluation_runtime(
            Arc::clone(&authority),
            factory,
            SemanticEvaluationProjectionResourcesV1 {
                memory_ceiling_bytes: 1 << 20,
            },
        )
        .expect("evaluation runtime");
        let first = chunk('a', "first native qualification group");
        let second = chunk('b', "second native qualification group");
        let first_group = [&first];
        let second_group = [&second];
        let mut encoder = RuntimeChunkVectorEncoderV1::new(
            Arc::clone(&runtime),
            Arc::new(SemanticRuntimeScheduleCancellationV1::new(2)),
            EmbeddingExecutionPlanV1 {
                intra_threads: 1,
                sessions: 2,
                limiting_reason: EmbeddingSessionLimitingReasonV1::ConfiguredMaximum,
            },
            documents(),
        );
        encoder
            .encode_batches(
                authority.projection().embedding_key(),
                &[first_group.as_slice(), second_group.as_slice()],
            )
            .expect("multi-group qualification projection");
        drop(encoder);

        let query_factory = PooledSemanticQueryEmbedderFactory::new(Arc::clone(&runtime));
        let query = query_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "reuse the qualification model",
            SanitizerRevision::new("sanitizer.v1").expect("sanitizer fixture"),
            QueryNormalizationRevision::new("normalizer.v1").expect("normalizer fixture"),
        )
        .expect("bounded query");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "11".repeat(32))).expect("query MAC"),
        );
        query
            .embed_query(SemanticQueryEmbeddingRequestV1 {
                query_digest: &query_digest,
                query_view: &query_view,
                projection: authority.projection(),
            })
            .expect("genuine qualification query");

        assert_eq!(
            runtime.stats().sessions_opened,
            1,
            "one qualification request must construct one model session even when the admitted runtime can fan out",
        );
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
            backend: CatalogedEmbeddingBackendV1::FastEmbedOrt {
                fastembed_enum: "JinaEmbeddingsV2BaseCode".to_owned(),
            },
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
            EmbeddingDocumentCompositionV1::SanitizedText,
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

    /// A daemon-lifetime owner keeps the cache across requests; each request
    /// builds its own encoder over it, exactly as one activation's projection
    /// scan does.
    fn request_encoder<'a>(
        cache: &'a SemanticEvaluationProjectionBatchCacheV1,
    ) -> CachedSemanticEvaluationChunkEncoderV1<'a, CountingEncoderV1> {
        cached_encoder(
            CountingEncoderV1::healthy(),
            cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        )
    }

    #[test]
    fn a_second_request_over_a_retained_cache_projects_the_identical_vectors() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let chunks = ['a', 'b', 'c'].map(|label| chunk(label, &format!("corpus {label}")));
        let singles = chunks.each_ref().map(|chunk| [chunk]);
        let groups = singles
            .iter()
            .map(|group| &group[..])
            .collect::<Vec<&[&CodeSearchChunkV1]>>();

        let cold_cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut cold = cached_encoder(
            CountingEncoderV1::healthy(),
            &cold_cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::Bypass,
        );
        let cold_vectors = cold
            .encode_batches(&embedding_key, &groups)
            .expect("cold projection");
        assert_eq!(cold.inner.group_invocations, 3);

        // One daemon-lifetime store, two successive request handles.
        let store = SemanticEvaluationProjectionBatchStoreV1::new();
        let first_request = store.request_cache();
        let mut first = request_encoder(&first_request);
        let first_vectors = first
            .encode_batches(&embedding_key, &groups)
            .expect("first request projection");
        assert_eq!(first.inner.group_invocations, 3);
        drop(first);
        drop(first_request);

        let second_request = store.request_cache();
        let mut second = request_encoder(&second_request);
        let second_vectors = second
            .encode_batches(&embedding_key, &groups)
            .expect("second request projection");
        assert_eq!(
            second.inner.group_invocations, 0,
            "a retained batch must not be re-embedded for the next request"
        );
        assert_eq!(first_vectors, cold_vectors);
        assert_eq!(
            second_vectors, cold_vectors,
            "the cached path must produce byte-identical vectors, so the \
             acceptance decision computed from them is identical too"
        );
        assert!(store.retained_bytes() <= store.max_bytes());
    }

    #[test]
    fn a_changed_privacy_partition_or_workload_input_misses_the_retained_cache() {
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let admitted = chunk('a', "shared corpus text");
        let admitted_group = [&admitted];

        let first_authority =
            crate::session_pool::test_support::authority_with_privacy("domain-a", 7);
        let mut first = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &first_authority,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        first
            .encode_batches(
                first_authority.projection().embedding_key(),
                &[&admitted_group],
            )
            .expect("first partition projection");
        assert_eq!(first.inner.group_invocations, 1);
        drop(first);

        // Same text, different privacy partition: the admitted projection
        // identity is part of the key, so this must not read the other
        // partition's vectors.
        let other_partition =
            crate::session_pool::test_support::authority_with_privacy("domain-b", 7);
        let mut across_partition = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &other_partition,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        across_partition
            .encode_batches(
                other_partition.projection().embedding_key(),
                &[&admitted_group],
            )
            .expect("other partition projection");
        assert_eq!(
            across_partition.inner.group_invocations, 1,
            "a different privacy partition must miss"
        );
        drop(across_partition);

        // Same partition, different key epoch: still a different admitted
        // projection identity.
        let rotated = crate::session_pool::test_support::authority_with_privacy("domain-a", 8);
        let mut rotated_encoder = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &rotated,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        rotated_encoder
            .encode_batches(rotated.projection().embedding_key(), &[&admitted_group])
            .expect("rotated key epoch projection");
        assert_eq!(
            rotated_encoder.inner.group_invocations, 1,
            "a rotated privacy key epoch must miss"
        );
        drop(rotated_encoder);

        // Same identity, different workload content.
        let changed = chunk('a', "changed corpus text");
        let changed_group = [&changed];
        let mut changed_encoder = cached_encoder_with_authority(
            CountingEncoderV1::healthy(),
            &first_authority,
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        changed_encoder
            .encode_batches(
                first_authority.projection().embedding_key(),
                &[&changed_group],
            )
            .expect("changed workload projection");
        assert_eq!(
            changed_encoder.inner.group_invocations, 1,
            "changed composed embedding input must miss"
        );
        drop(changed_encoder);

        // And the original entry is still exactly one hit away.
        let mut replay = request_encoder(&cache);
        replay
            .encode_batches(
                first_authority.projection().embedding_key(),
                &[&admitted_group],
            )
            .expect("replay projection");
        assert_eq!(replay.inner.group_invocations, 0);
        assert_eq!(cache.entry_count(), 4);
    }

    #[test]
    fn an_abandoned_build_does_not_poison_the_next_requests_batch() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let subject = chunk('a', "abandoned then rebuilt");
        let group = [&subject];

        let mut failing = cached_encoder(
            CountingEncoderV1::failing(),
            &cache,
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
        );
        failing
            .encode_batches(&embedding_key, &[&group])
            .expect_err("injected encoder failure");
        drop(failing);
        assert_eq!(
            cache.entry_count(),
            0,
            "an abandoned build must retain nothing"
        );

        let mut recovered = request_encoder(&cache);
        recovered
            .encode_batches(&embedding_key, &[&group])
            .expect("the next request must be able to build the same batch");
        assert_eq!(recovered.inner.group_invocations, 1);
        assert_eq!(cache.entry_count(), 1);
    }

    #[test]
    fn concurrent_fills_of_one_batch_run_the_model_once() {
        use std::sync::mpsc;

        struct GatedEncoderV1 {
            inner: CountingEncoderV1,
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }

        impl CanonicalChunkVectorEncoderV1 for GatedEncoderV1 {
            fn encode(
                &mut self,
                key: &EmbeddingProjectionKeyV1,
                chunk: &CodeSearchChunkV1,
            ) -> Result<Vec<f32>, String> {
                self.inner.encode(key, chunk)
            }

            fn encode_batch(
                &mut self,
                key: &EmbeddingProjectionKeyV1,
                chunks: &[&CodeSearchChunkV1],
            ) -> Result<Vec<Vec<f32>>, String> {
                self.inner.encode_batch(key, chunks)
            }

            fn encode_batches(
                &mut self,
                key: &EmbeddingProjectionKeyV1,
                groups: &[&[&CodeSearchChunkV1]],
            ) -> Result<Vec<Vec<Vec<f32>>>, String> {
                let _ = self.entered.send(());
                let _ = self.release.recv();
                self.inner.encode_batches(key, groups)
            }
        }

        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let contested = chunk('a', "contested batch");
        let group = [&contested];
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let authority = crate::session_pool::test_support::authority();

        std::thread::scope(|scope| {
            let builder = scope.spawn(|| {
                let mut builder = CachedSemanticEvaluationChunkEncoderV1::new(
                    GatedEncoderV1 {
                        inner: CountingEncoderV1::healthy(),
                        entered: entered_tx,
                        release: release_rx,
                    },
                    &authority,
                    &cache,
                    SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
                    cancellation(),
                    documents(),
                );
                let vectors = builder
                    .encode_batches(&embedding_key, &[&group])
                    .expect("builder projection");
                (vectors, builder.inner.inner.group_invocations)
            });
            entered_rx.recv().expect("builder reached the model");
            let waiter = scope.spawn(|| {
                let mut waiter = request_encoder(&cache);
                let vectors = waiter
                    .encode_batches(&embedding_key, &[&group])
                    .expect("waiter projection");
                (vectors, waiter.inner.group_invocations)
            });
            // The waiter is parked on the build claim; releasing the builder
            // is what lets it finish.
            std::thread::sleep(std::time::Duration::from_millis(50));
            release_tx.send(()).expect("release the builder");
            let (built, built_invocations) = builder.join().expect("builder thread");
            let (waited, waited_invocations) = waiter.join().expect("waiter thread");
            assert_eq!(built_invocations, 1);
            assert_eq!(
                waited_invocations, 0,
                "a concurrent request for the same batch must wait for the one \
                 builder instead of running the model again"
            );
            assert_eq!(built, waited);
        });
        assert_eq!(cache.entry_count(), 1);
        assert!(cache.retained_bytes() <= cache.max_bytes());
    }

    #[test]
    fn a_later_request_evicts_stale_batches_to_stay_under_the_byte_bound() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        // Equal-length texts so both batches cost the same retained bytes and
        // the bound below admits exactly one of them.
        let first = chunk('a', "alpha");
        let second = chunk('b', "bravo");
        let entry_bytes = {
            let sizing = SemanticEvaluationProjectionBatchCacheV1::new();
            let mut encoder = request_encoder(&sizing);
            encoder
                .encode_batches(&embedding_key, &[&[&first][..]])
                .expect("sizing projection");
            sizing.retained_bytes()
        };
        assert!(entry_bytes > 0);

        // Room for exactly one batch.
        let store = SemanticEvaluationProjectionBatchStoreV1::with_limits_for_tests(
            usize::MAX,
            entry_bytes,
        );
        let earlier_request = store.request_cache();
        let mut earlier = request_encoder(&earlier_request);
        earlier
            .encode_batches(&embedding_key, &[&[&first][..]])
            .expect("earlier request");
        assert_eq!(store.entry_count(), 1);
        assert!(store.retained_bytes() <= store.max_bytes());

        // A second pass of the SAME request must not evict what the first
        // pass retained; there is no room, so its batch is simply not kept.
        let mut same_request_second_pass = request_encoder(&earlier_request);
        same_request_second_pass
            .encode_batches(&embedding_key, &[&[&second][..]])
            .expect("same request second pass");
        assert_eq!(store.entry_count(), 1);
        let mut same_request_replay = request_encoder(&earlier_request);
        same_request_replay
            .encode_batches(&embedding_key, &[&[&first][..]])
            .expect("same request replay");
        assert_eq!(
            same_request_replay.inner.group_invocations, 0,
            "a running request must never evict the batches its own earlier \
             passes retained"
        );
        drop(same_request_replay);
        drop(same_request_second_pass);
        drop(earlier);
        drop(earlier_request);

        // A later request needs the bytes; nothing pins the earlier batch any
        // more, so it is evicted rather than refused.
        let later_request = store.request_cache();
        let mut later = request_encoder(&later_request);
        later
            .encode_batches(&embedding_key, &[&[&second][..]])
            .expect("later request");
        assert_eq!(store.entry_count(), 1);
        assert!(
            store.retained_bytes() <= store.max_bytes(),
            "eviction must keep the store under its byte bound"
        );
        later
            .encode_batches(&embedding_key, &[&[&second][..]])
            .expect("later request replay");
        assert_eq!(
            later.inner.group_invocations, 1,
            "the batch this request just retained must not have been evicted"
        );
        drop(later);

        let mut evicted_replay = request_encoder(&later_request);
        evicted_replay
            .encode_batches(&embedding_key, &[&[&first][..]])
            .expect("evicted batch replay");
        assert_eq!(
            evicted_replay.inner.group_invocations, 1,
            "the evicted batch must be rebuilt rather than served stale"
        );
    }

    #[test]
    fn release_returns_every_retained_byte() {
        let projection = projection();
        let embedding_key = projection.embedding_key().clone();
        let cache = SemanticEvaluationProjectionBatchCacheV1::new();
        let mut encoder = request_encoder(&cache);
        for label in ['a', 'b', 'c'] {
            let retained = chunk(label, &format!("retained {label}"));
            encoder
                .encode_batches(&embedding_key, &[&[&retained][..]])
                .expect("retained projection");
        }
        assert_eq!(cache.entry_count(), 3);
        assert!(cache.retained_bytes() > 0);
        drop(encoder);

        cache.store.release();
        assert_eq!(cache.entry_count(), 0);
        assert_eq!(
            cache.retained_bytes(),
            0,
            "shutdown must return the cache's bytes"
        );

        // The cache is still usable after a release; it simply starts cold.
        let mut after = request_encoder(&cache);
        let rebuilt = chunk('a', "retained a");
        after
            .encode_batches(&embedding_key, &[&[&rebuilt][..]])
            .expect("post-release projection");
        assert_eq!(after.inner.group_invocations, 1);
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
            documents(),
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
