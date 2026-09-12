use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use tracedecay_code_index::embedding_document::{EmbeddingDocumentComposerV1, EmbeddingDocumentV1};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkV1, EmbeddingExecutionProviderV1,
    EmbeddingProjectionKeyV1, ProjectionBatchRequestV1,
};
use tracedecay_query::retrieval::ports::{RetrievalExecutionControl, RetrievalPortError};
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticQueryEmbeddingPort, SemanticQueryEmbeddingRequestV1,
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
const EVALUATION_BATCH_CACHE_MAX_RETAINED_BYTES: u64 = 80 * 1024 * 1024;
const EVALUATION_BATCH_CACHE_ENTRY_OVERHEAD_BYTES: u64 = 4_096;
const EVALUATION_BATCH_CACHE_KEY_OWNER_OVERHEAD_BYTES: u64 = 512;

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
/// [`Self::request_cache`]; completed batches stay under
/// [`Self::max_retained_bytes`], while [`Self::memory_usage`] also reports
/// in-flight keys and request-owned warm-hit clones.
pub struct SemanticEvaluationProjectionBatchStoreV1 {
    limits: SemanticEvaluationProjectionBatchCacheLimitsV1,
    state: Mutex<SemanticEvaluationProjectionBatchCacheStateV1>,
    /// Signalled whenever a build claim is resolved (installed or abandoned),
    /// so a waiter for that exact batch can re-read the entry.
    resolved: Condvar,
}

/// One request's view of a projection batch store.
///
/// The handle carries that request's borrower identity, which every projection
/// pass the request runs shares. Every entry records all current borrowers, so
/// one request ending cannot erase another request's pin.
pub struct SemanticEvaluationProjectionBatchCacheV1 {
    store: Arc<SemanticEvaluationProjectionBatchStoreV1>,
    request_id: u64,
}

impl Drop for SemanticEvaluationProjectionBatchCacheV1 {
    fn drop(&mut self) {
        self.store.end_request(self.request_id);
    }
}

#[derive(Clone, Copy)]
struct SemanticEvaluationProjectionBatchCacheLimitsV1 {
    max_entries: usize,
    max_retained_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticEvaluationProjectionBatchCacheMemoryV1 {
    /// Whether this store has crossed its irreversible retirement fence.
    pub retired: bool,
    /// Completed cache entries, including exact keys, vectors, and container
    /// overhead.
    pub retained_batch_bytes: u64,
    /// Exact composed-input storage conservatively attributed to build claims.
    pub in_flight_key_bytes: u64,
    /// Exact composed-input keys currently probing this store.
    pub active_lookup_key_bytes: u64,
    /// Vector copies reserved before allocation and held through install or refusal.
    pub pending_install_vector_bytes: u64,
    /// Warm-hit output copies and the shared batch, conservatively charged until
    /// their request ends (including a shared batch outliving retirement).
    pub active_hit_vector_bytes: u64,
    /// Sum of all currently cache-associated categories above.
    pub total_accounted_bytes: u64,
    /// Highest total plus short-lived clone reservations observed by this
    /// store.
    pub peak_accounted_bytes: u64,
}

struct SemanticEvaluationProjectionBatchCacheEntryV1 {
    vectors: Arc<Vec<Vec<f32>>>,
    vector_bytes: u64,
    retained_bytes: u64,
    last_used_sequence: u64,
    borrowers: BTreeSet<u64>,
}

#[derive(Default)]
struct SemanticEvaluationProjectionBatchRequestMemoryV1 {
    lookup_key_bytes: u64,
    hit_vector_bytes: u64,
}

#[derive(Default)]
struct SemanticEvaluationProjectionBatchCacheStateV1 {
    entries: BTreeMap<
        Arc<SemanticEvaluationProjectionBatchCacheKeyV1>,
        SemanticEvaluationProjectionBatchCacheEntryV1,
    >,
    in_flight: BTreeSet<Arc<SemanticEvaluationProjectionBatchCacheKeyV1>>,
    retained_bytes: u64,
    in_flight_key_bytes: u64,
    active_lookup_key_bytes: u64,
    pending_install_vector_bytes: u64,
    active_hit_vector_bytes: u64,
    peak_accounted_bytes: u64,
    next_request_id: u64,
    next_use_sequence: u64,
    active_requests: BTreeMap<u64, SemanticEvaluationProjectionBatchRequestMemoryV1>,
    retired: bool,
}

impl SemanticEvaluationProjectionBatchCacheStateV1 {
    fn total_accounted_bytes(&self) -> u64 {
        self.retained_bytes
            .saturating_add(self.in_flight_key_bytes)
            .saturating_add(self.active_lookup_key_bytes)
            .saturating_add(self.pending_install_vector_bytes)
            .saturating_add(self.active_hit_vector_bytes)
    }

    fn observe_peak(&mut self, additional_transient_bytes: u64) {
        self.peak_accounted_bytes = self.peak_accounted_bytes.max(
            self.total_accounted_bytes()
                .saturating_add(additional_transient_bytes),
        );
    }

    fn memory_usage(&self) -> SemanticEvaluationProjectionBatchCacheMemoryV1 {
        SemanticEvaluationProjectionBatchCacheMemoryV1 {
            retired: self.retired,
            retained_batch_bytes: self.retained_bytes,
            in_flight_key_bytes: self.in_flight_key_bytes,
            active_lookup_key_bytes: self.active_lookup_key_bytes,
            pending_install_vector_bytes: self.pending_install_vector_bytes,
            active_hit_vector_bytes: self.active_hit_vector_bytes,
            total_accounted_bytes: self.total_accounted_bytes(),
            peak_accounted_bytes: self.peak_accounted_bytes,
        }
    }
}

/// Outcome of asking the cache for one batch.
enum SemanticEvaluationProjectionBatchClaimV1<'cache> {
    /// Another pass already produced these exact vectors.
    Hit(Arc<Vec<Vec<f32>>>),
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
    /// Share exact composed input with the lookup, in-flight set, and entry.
    key: Arc<SemanticEvaluationProjectionBatchCacheKeyV1>,
    accounted_bytes: u64,
}

impl SemanticEvaluationProjectionBatchBuildGuardV1<'_> {
    fn key(&self) -> &SemanticEvaluationProjectionBatchCacheKeyV1 {
        &self.key
    }
}

impl Drop for SemanticEvaluationProjectionBatchBuildGuardV1<'_> {
    fn drop(&mut self) {
        let mut state = self.store.lock_state();
        if state.in_flight.remove(self.key.as_ref()) {
            state.in_flight_key_bytes = state
                .in_flight_key_bytes
                .saturating_sub(self.accounted_bytes);
        }
        drop(state);
        self.store.resolved.notify_all();
    }
}

struct SemanticEvaluationProjectionBatchStagingGuardV1<'cache> {
    store: &'cache SemanticEvaluationProjectionBatchStoreV1,
    request_id: u64,
    accounted_bytes: u64,
    install_bytes: u64,
}

impl Drop for SemanticEvaluationProjectionBatchStagingGuardV1<'_> {
    fn drop(&mut self) {
        self.store
            .end_staging(self.request_id, self.accounted_bytes, self.install_bytes);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticEvaluationProjectionBatchCacheKeyV1 {
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
    /// FastEmbed's intra-op width can change floating-point numerics even
    /// with an otherwise identical admitted projection and tensor input.
    max_threads: u32,
    execution_provider: EmbeddingExecutionProviderV1,
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
            max_retained_bytes: EVALUATION_BATCH_CACHE_MAX_RETAINED_BYTES,
        })
    }

    #[cfg(test)]
    fn with_limits_for_tests(max_entries: usize, max_retained_bytes: u64) -> Arc<Self> {
        Self::with_limits(SemanticEvaluationProjectionBatchCacheLimitsV1 {
            max_entries,
            max_retained_bytes,
        })
    }

    fn with_limits(limits: SemanticEvaluationProjectionBatchCacheLimitsV1) -> Arc<Self> {
        Arc::new(Self {
            limits,
            state: Mutex::new(SemanticEvaluationProjectionBatchCacheStateV1::default()),
            resolved: Condvar::new(),
        })
    }

    /// Open one request's view of this store. A handle opened after retirement
    /// remains unusable because retirement is irreversible.
    #[must_use]
    pub fn request_cache(self: &Arc<Self>) -> SemanticEvaluationProjectionBatchCacheV1 {
        let request_id = {
            let mut state = self.lock_state();
            let request_id = state.next_request_id.saturating_add(1);
            state.next_request_id = request_id;
            if !state.retired {
                state.active_requests.insert(
                    request_id,
                    SemanticEvaluationProjectionBatchRequestMemoryV1::default(),
                );
            }
            request_id
        };
        SemanticEvaluationProjectionBatchCacheV1 {
            store: Arc::clone(self),
            request_id,
        }
    }

    fn end_request(&self, request_id: u64) {
        let mut state = self.lock_state();
        if let Some(memory) = state.active_requests.remove(&request_id) {
            state.active_lookup_key_bytes = state
                .active_lookup_key_bytes
                .saturating_sub(memory.lookup_key_bytes);
            state.active_hit_vector_bytes = state
                .active_hit_vector_bytes
                .saturating_sub(memory.hit_vector_bytes);
        }
        for entry in state.entries.values_mut() {
            entry.borrowers.remove(&request_id);
        }
    }

    /// Bytes currently retained by cached batches. Never above the byte bound.
    pub fn retained_bytes(&self) -> u64 {
        self.lock_state().retained_bytes
    }

    /// Cache-associated memory currently owned or conservatively charged to
    /// live request handles, plus the process-lifetime peak for this store.
    pub fn memory_usage(&self) -> SemanticEvaluationProjectionBatchCacheMemoryV1 {
        self.lock_state().memory_usage()
    }

    /// Cached batches currently retained.
    pub fn entry_count(&self) -> usize {
        self.lock_state().entries.len()
    }

    /// Configured bound for completed batches retained by the store. Transient
    /// and request-borrowed allocations are reported separately by
    /// [`Self::memory_usage`].
    pub fn max_retained_bytes(&self) -> u64 {
        self.limits.max_retained_bytes
    }

    /// Irreversibly retire this store and drop every completed batch.
    ///
    /// Existing builders keep their claim only long enough to settle it; a
    /// late install is refused under the same state lock. Waiters are woken so
    /// they observe retirement instead of waiting for an outliving worker.
    pub fn release(&self) {
        {
            let mut state = self.lock_state();
            state.retired = true;
            state.entries.clear();
            state.retained_bytes = 0;
        }
        self.resolved.notify_all();
    }

    fn lock_state(
        &self,
    ) -> std::sync::MutexGuard<'_, SemanticEvaluationProjectionBatchCacheStateV1> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin_staging(
        &self,
        request_id: u64,
        accounted_bytes: u64,
        install_bytes: u64,
    ) -> Result<SemanticEvaluationProjectionBatchStagingGuardV1<'_>, String> {
        let mut state = self.lock_state();
        if state.retired || !state.active_requests.contains_key(&request_id) {
            return Err("semantic evaluation projection batch cache is retired".to_owned());
        }
        let Some(request) = state.active_requests.get_mut(&request_id) else {
            return Err("semantic evaluation projection batch cache is retired".to_owned());
        };
        request.lookup_key_bytes = request.lookup_key_bytes.saturating_add(accounted_bytes);
        state.active_lookup_key_bytes = state
            .active_lookup_key_bytes
            .saturating_add(accounted_bytes);
        state.pending_install_vector_bytes = state
            .pending_install_vector_bytes
            .saturating_add(install_bytes);
        state.observe_peak(0);
        Ok(SemanticEvaluationProjectionBatchStagingGuardV1 {
            store: self,
            request_id,
            accounted_bytes,
            install_bytes,
        })
    }

    fn end_staging(&self, request_id: u64, accounted_bytes: u64, install_bytes: u64) {
        let mut state = self.lock_state();
        state.pending_install_vector_bytes = state
            .pending_install_vector_bytes
            .saturating_sub(install_bytes);
        if let Some(request) = state.active_requests.get_mut(&request_id) {
            request.lookup_key_bytes = request.lookup_key_bytes.saturating_sub(accounted_bytes);
        }
        state.active_lookup_key_bytes = state
            .active_lookup_key_bytes
            .saturating_sub(accounted_bytes);
    }

    /// Read one batch, or take the exclusive right to build it, waiting while
    /// another caller is already building that exact batch.
    ///
    /// `interrupted` is polled between waits so a cancelled or expired request
    /// stops waiting instead of blocking on someone else's model work.
    fn claim(
        &self,
        key: &Arc<SemanticEvaluationProjectionBatchCacheKeyV1>,
        request_id: u64,
        hit_copies: usize,
        interrupted: &dyn Fn() -> Option<String>,
    ) -> Result<SemanticEvaluationProjectionBatchClaimV1<'_>, String> {
        let mut build_key_bytes = None;
        let mut state = self.lock_state();
        loop {
            if state.retired || !state.active_requests.contains_key(&request_id) {
                return Err("semantic evaluation projection batch cache is retired".to_owned());
            }
            state.next_use_sequence = state.next_use_sequence.saturating_add(1);
            let use_sequence = state.next_use_sequence;
            if state.entries.contains_key(key) {
                let hit_vector_bytes = state
                    .entries
                    .get(key)
                    .map(|entry| {
                        entry.vector_bytes.saturating_mul(
                            u64::try_from(hit_copies)
                                .unwrap_or(u64::MAX)
                                .saturating_add(1),
                        )
                    })
                    .unwrap_or(0);
                if let Some(request) = state.active_requests.get_mut(&request_id) {
                    request.hit_vector_bytes =
                        request.hit_vector_bytes.saturating_add(hit_vector_bytes);
                }
                state.active_hit_vector_bytes = state
                    .active_hit_vector_bytes
                    .saturating_add(hit_vector_bytes);
                state.observe_peak(0);
                let Some(entry) = state.entries.get_mut(key) else {
                    return Err("semantic evaluator cache lost a completed vector group".to_owned());
                };
                entry.last_used_sequence = use_sequence;
                entry.borrowers.insert(request_id);
                return Ok(SemanticEvaluationProjectionBatchClaimV1::Hit(Arc::clone(
                    &entry.vectors,
                )));
            }
            if !state.in_flight.contains(key) {
                let Some(accounted_bytes) = build_key_bytes else {
                    // Size traversal is only needed for a miss, outside the lock.
                    // Recheck admission after reacquiring it: a builder or retirement
                    // may have won while the exact identity was measured.
                    drop(state);
                    build_key_bytes = Some(cache_key_bytes(key).saturating_add(
                        EVALUATION_BATCH_CACHE_KEY_OWNER_OVERHEAD_BYTES.saturating_mul(2),
                    ));
                    state = self.lock_state();
                    continue;
                };
                state.in_flight.insert(Arc::clone(key));
                state.in_flight_key_bytes =
                    state.in_flight_key_bytes.saturating_add(accounted_bytes);
                state.observe_peak(0);
                return Ok(SemanticEvaluationProjectionBatchClaimV1::Build(
                    SemanticEvaluationProjectionBatchBuildGuardV1 {
                        store: self,
                        key: Arc::clone(key),
                        accounted_bytes,
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
        request_id: u64,
    ) {
        let retained_bytes = cache_entry_bytes(guard.key(), vectors);
        if self.limits.max_entries == 0
            || self.limits.max_retained_bytes == 0
            || retained_bytes > self.limits.max_retained_bytes
        {
            return;
        }
        // The encoder owns its output; retain one immutable copy without holding
        // the cache lock. Retirement is rechecked before this copy can install.
        let vector_bytes = cache_vector_bytes(vectors);
        let Ok(_install_memory) = self.begin_staging(
            request_id,
            0,
            vector_bytes.saturating_add(EVALUATION_BATCH_CACHE_KEY_OWNER_OVERHEAD_BYTES),
        ) else {
            return;
        };
        // Declared after the reservation so every refused copy drops before its charge.
        let vectors = Arc::new(vectors.to_vec());
        let mut state = self.lock_state();
        if state.retired || !state.active_requests.contains_key(&request_id) {
            return;
        }
        if state.entries.contains_key(guard.key()) {
            return;
        }
        while state.entries.len() >= self.limits.max_entries
            || state.retained_bytes.saturating_add(retained_bytes) > self.limits.max_retained_bytes
        {
            // ponytail: linear least-recently-used scan, bounded by
            // max_entries; swap in a recency index if eviction ever shows up
            // next to a 200ms model batch.
            let evictable = state
                .entries
                .iter()
                .filter(|(_, entry)| entry.borrowers.is_empty())
                .min_by_key(|(_, entry)| entry.last_used_sequence)
                .map(|(key, _)| key.clone());
            let Some(evictable) = evictable else {
                return;
            };
            if let Some(evicted) = state.entries.remove(&evictable) {
                state.retained_bytes = state.retained_bytes.saturating_sub(evicted.retained_bytes);
            }
        }
        state.next_use_sequence = state.next_use_sequence.saturating_add(1);
        let use_sequence = state.next_use_sequence;
        state.retained_bytes = state.retained_bytes.saturating_add(retained_bytes);
        state.observe_peak(0);
        state.entries.insert(
            Arc::clone(&guard.key),
            SemanticEvaluationProjectionBatchCacheEntryV1 {
                vectors,
                vector_bytes,
                retained_bytes,
                last_used_sequence: use_sequence,
                borrowers: BTreeSet::from([request_id]),
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
    fn with_limits_for_tests(max_entries: usize, max_retained_bytes: u64) -> Self {
        SemanticEvaluationProjectionBatchStoreV1::with_limits_for_tests(
            max_entries,
            max_retained_bytes,
        )
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

    /// The backing store's completed-batch retention bound.
    pub fn max_retained_bytes(&self) -> u64 {
        self.store.max_retained_bytes()
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
    cache_key_bytes(key)
        .checked_add(cache_vector_bytes(vectors))
        .and_then(|bytes| bytes.checked_add(EVALUATION_BATCH_CACHE_ENTRY_OVERHEAD_BYTES))
        .unwrap_or(u64::MAX)
}

fn cache_key_bytes(key: &SemanticEvaluationProjectionBatchCacheKeyV1) -> u64 {
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
            total.checked_add(u64::try_from(input.capacity()).ok()?)
        });
    let input_headers = u64::try_from(key.ordered_documents.capacity())
        .ok()
        .and_then(|count| count.checked_mul(u64::try_from(std::mem::size_of::<String>()).ok()?));
    identity_bytes
        .and_then(|identity| identity.checked_add(input_bytes?))
        .and_then(|bytes| bytes.checked_add(input_headers?))
        .and_then(|bytes| {
            bytes.checked_add(
                u64::try_from(std::mem::size_of::<
                    SemanticEvaluationProjectionBatchCacheKeyV1,
                >())
                .ok()?,
            )
        })
        .unwrap_or(u64::MAX)
}

fn cache_vector_bytes(vectors: &[Vec<f32>]) -> u64 {
    let vector_bytes = vectors.iter().try_fold(0_u64, |total, vector| {
        let bytes = u64::try_from(vector.capacity())
            .ok()?
            .checked_mul(u64::try_from(std::mem::size_of::<f32>()).ok()?)?;
        total.checked_add(bytes)
    });
    let container_headers = u64::try_from(std::mem::size_of::<Vec<Vec<f32>>>())
        .ok()
        .and_then(|bytes| {
            bytes.checked_add(
                u64::try_from(vectors.len())
                    .ok()?
                    .checked_mul(u64::try_from(std::mem::size_of::<Vec<f32>>()).ok()?)?,
            )
        });
    vector_bytes
        .and_then(|bytes| bytes.checked_add(container_headers?))
        .unwrap_or(u64::MAX)
}

struct CachedSemanticEvaluationChunkEncoderV1<'a, E> {
    inner: E,
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
    max_threads: u32,
    execution_provider: EmbeddingExecutionProviderV1,
    cache: &'a SemanticEvaluationProjectionBatchCacheV1,
    cache_policy: SemanticEvaluationProjectionBatchCachePolicyV1,
    request_id: u64,
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
            execution_provider: artifact_authority.execution_provider(),
            request_id: cache.request_id,
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
    ) -> Result<Arc<SemanticEvaluationProjectionBatchCacheKeyV1>, String> {
        let ordered_documents = chunks
            .iter()
            .map(|chunk| {
                self.documents
                    .compose(embedding_key, chunk)
                    .map(EmbeddingDocumentV1::into_text)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(SemanticEvaluationProjectionBatchCacheKeyV1 {
            admitted_projection: self.admitted_projection.clone(),
            max_threads: self.max_threads,
            execution_provider: self.execution_provider,
            group_len: chunks.len(),
            tensor_batch_size: embedding_key.inference_batch_size,
            tensor_dimensions: embedding_key.dimensions,
            ordered_documents,
        }))
    }
}

impl<E> crate::projector::CanonicalChunkTokenLengthsV1
    for CachedSemanticEvaluationChunkEncoderV1<'_, E>
where
    E: CanonicalChunkVectorEncoderV1,
{
    /// The cache stores vectors, not lengths, so this is the wrapped
    /// encoder's own tokenizer either way.
    fn document_token_lengths(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<usize>, String> {
        self.inner.document_token_lengths(key, chunks)
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
        let request_id = self.request_id;
        let cancellation = Arc::clone(&self.cancellation);
        let interrupted = move || semantic_execution_interruption_error(cancellation.as_ref());
        let mut encoded = vec![None; groups.len()];
        let mut distinct =
            BTreeMap::<Arc<SemanticEvaluationProjectionBatchCacheKeyV1>, Vec<usize>>::new();
        for (position, group) in groups.iter().enumerate() {
            distinct
                .entry(self.exact_key(key, group)?)
                .or_default()
                .push(position);
        }
        let lookup_key_bytes = distinct.keys().fold(0_u64, |total, key| {
            total.saturating_add(cache_key_bytes(key))
        });
        let _lookup_memory = store.begin_staging(request_id, lookup_key_bytes, 0)?;
        let mut unique_misses = Vec::<(
            SemanticEvaluationProjectionBatchBuildGuardV1<'_>,
            usize,
            Vec<usize>,
        )>::new();
        // Every request takes claims in this total order, so overlapping batch
        // sets cannot form a wait cycle while the owner-level permit bounds
        // concurrent model construction.
        for (cache_key, positions) in distinct {
            match store.claim(&cache_key, request_id, positions.len(), &interrupted)? {
                SemanticEvaluationProjectionBatchClaimV1::Hit(vectors) => {
                    // The encoder API requires owned vectors. Copy once per output
                    // here, after the shared cache lock has been released.
                    for position in positions {
                        encoded[position] = Some(vectors.as_ref().clone());
                    }
                }
                SemanticEvaluationProjectionBatchClaimV1::Build(guard) => {
                    let first = positions.first().copied().ok_or_else(|| {
                        "semantic evaluator cache found an empty batch position set".to_owned()
                    })?;
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
            store.install(guard, &vectors, request_id);
            let mut positions = positions.into_iter();
            let Some(first) = positions.next() else {
                return Err("semantic evaluator cache lost an uncached batch position".to_owned());
            };
            for position in positions {
                encoded[position] = Some(vectors.clone());
            }
            encoded[first] = Some(vectors);
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

/// Fixture tokenizer: one token per whitespace-separated word, capped at the
/// admitted truncation length. The double has no model; grouping only needs a
/// length that varies with the document and can be predicted from a fixture.
impl super::projector::CanonicalChunkTokenLengthsV1 for CancelAfterFirstModelBatchV1 {
    fn document_token_lengths(
        &mut self,
        key: &tracedecay_domain::EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<usize>, String> {
        let truncation_length = key.truncation_length as usize;
        Ok(chunks
            .iter()
            .map(|chunk| {
                chunk
                    .sanitized_text
                    .as_str()
                    .split_whitespace()
                    .count()
                    .clamp(1, truncation_length)
            })
            .collect())
    }
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
        C: RetrievalExecutionControl + Sync,
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
    C: RetrievalExecutionControl + Sync,
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
mod tests;
