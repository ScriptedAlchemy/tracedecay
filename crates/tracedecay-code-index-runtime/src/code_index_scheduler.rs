//! Daemon-owned scheduling and reconciliation for production code generations.
//!
//! Hook events are bounded wake-up hints only. Every run reconstructs its
//! source snapshot from gix's HEAD-tree/index/worktree status before content
//! digests decide whether publication is necessary.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, Weak,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use gix::{
    bstr::ByteSlice,
    object::tree::diff::{Action as TreeDiffAction, Change as TreeDiffChange},
};
use same_file::Handle;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::now_micros;
use tracedecay_domain::canonical_text::{
    encode_lowercase_hex, encode_tagged_lowercase_hex, sha256_hex,
};
use tracedecay_graph_db::GraphConflictContextV1;

use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, ComponentRevision, ContentDigest,
    ExactAdmissionRuleRevision, FileOccurrenceId, ManifestDigest, PolicyRevisionId,
    PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, RepositoryDirtyStateV1, RepositoryId,
    RetrievalBudget, RetrieverBatch, RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, ScoreDomainId, SnapshotFileDispositionV1, TreeId,
    WorktreeId, canonical_sha256,
};
use tracedecay_private_fs::{
    framed_log::DirectorySyncPolicy, make_private_directory, open_private_file,
    validate_private_directory,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1,
    ResidentMemoryAdmissionFailureV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryReservationV1, detected_process_resident_memory_limit_v1,
};
use tracedecay_usecases::code_index::{
    DaemonCodeIndexControlV1, ProductionCodeIndexOwnerV1, open_production_code_index_owner_v1,
};

use self::freshness_witness::RestoreFreshnessWitnessV1;
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeIndexBuildBlockedReasonV1, CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1,
    CodeIndexGenerationRecoveryServingV1, CodeIndexGenerationRecoveryV1,
};

use crate::{
    code_index::{
        chunks::content_digest,
        graph_projection::{
            CodeGraphEvidenceReader, CodeGraphProjectionError, CodeGraphProjectionStore,
        },
        languages::{LanguageRegistry, StaticLanguageRegistry},
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
            CodeIndexExecutionControlV1, CodeIndexGenerationCompatibilityV1,
            CodeIndexGenerationScopeV1, CodeIndexIgnoredSourceAdmissionV1, CodeIndexInputErrorV1,
            CodeIndexProductionConfigV1, CodeIndexProductionErrorV1,
            CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
            CodeIndexRepositoryParseIdentityV1, DAEMON_CODE_INDEX_CHUNKER_REVISION,
            SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
            SharedPhysicalCodeArtifactPoolV1, UninterruptibleCodeIndexControlV1,
            VerifiedSealedLexicalCursorRestoreErrorV1, VerifiedSealedLexicalPageBatchBoundsV1,
            VerifiedSealedLexicalPageBatchReadV1, VerifiedSealedLexicalPageSourceV1,
            VerifiedSealedLexicalSourceReceiptV1, VerifiedSealedTextGenerationMetadataV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
            ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
        },
    },
    query::retrieval::{
        exact::{
            CentralExactAdmissionAuthorityV1, ExactLane, ExactLaneEvidence, ExactLaneRequest,
            ExactLaneRetriever,
        },
        graph::{GraphLane, production_code_index_freshness},
        lexical::{
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeExactLexicalArtifactReaderV1,
            CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1,
            CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
            CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactReaderV1,
            CodeLexicalProjectionMetadataV1, LexicalLane, LexicalLaneEvidence, LexicalLaneRequest,
            LexicalLaneRetriever,
        },
        ports::RetrievalPortError,
    },
};
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationStoreLockV1, DurableCodeTextArtifactDescriptorV1, DurableGenerationCardinalityV1,
    DurableGenerationIndexEntryV1, DurablePublicationPointerV1,
    DurableSealedCodeGenerationIdentityV1, MAX_DURABLE_GENERATION_INDEX_BYTES_V1,
    MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1, acquire_code_generation_store_lock,
    attach_verified_text_artifact_under_lock, code_text_artifact_path, code_text_artifacts_root,
    durable_generation_index_digest, retain_bounded_generation_index,
    withdraw_verified_text_artifact_under_lock,
};
use tracedecay_runtime_core::privacy::CODE_SOURCE_SANITIZER_VERSION_V1;

/// Std mutex wrapped for Hotpath lock-contention accounting. Condvar-paired
/// mutexes (the generation-decode barrier and the text-projection slot)
/// cannot use this wrapper because `Condvar::wait` requires the exact std
/// guard type; those measure lock-wait and parked wait with explicit spans
/// instead.
type ProfiledStdMutex<T> = hotpath::mutexes::Mutex<T>;

const MAX_PENDING_HINTS: usize = 1_024;
const MAX_SUPERSEDED_RECONCILE_RETRIES: usize = 4;
const CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1: &str = "code-index-build-workers-v1";

const SUPERSEDED_RECONCILE_RETRY_BACKOFF: Duration = Duration::from_millis(75);
/// Freshness contract for non-git-mediated mutations (raw file writes, rsync,
/// out-of-agent saves): a query admitted after this bound since the last
/// reconciliation re-checks gix truth before serving. Git-mediated changes are
/// caught immediately by the tier-1 metadata check regardless of this bound.
const DEFAULT_STALENESS_THRESHOLD: Duration = Duration::from_secs(30);
/// Positive busy-read stat proofs may be reused only within this bound.
/// Git metadata drift and scheduler epoch advances invalidate immediately;
/// raw out-of-band writes are therefore stale for at most this interval.
const BUSY_WITNESS_MEMO_INTERVAL: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(1)
};
const MAX_DURABLE_PUBLICATION_POINTER_BYTES: u64 = 512 * 1024;
const DURABLE_GENERATION_IO_CHUNK_BYTES_V1: usize = 64 * 1024;
/// Page bounds for streaming one sealed generation into the durable lexical
/// text artifact. One page is one bounded unit of background build progress.
const TEXT_ARTIFACT_PAGE_CHUNKS_V1: usize = 128;
const TEXT_ARTIFACT_PAGE_BYTES_V1: usize = 4 * 1024 * 1024;
/// Measured on a 4379-file corpus with `journal_mode=DELETE`: each
/// `query.artifact.batch.sqlite` transaction pays a fixed commit cost
/// (journal fsync + page-cache flush) independent of its row count, and
/// postings inserts alone were 73-76% of that phase (~212-245s of a ~7min
/// batch phase). Doubling the page/work caps here roughly halves the number
/// of commits paid for the same corpus while staying far inside both the
/// 2M-row prepared-batch cap and the 1536MiB
/// `CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1` ledger (see
/// `TEXT_ARTIFACT_BATCH_BYTES_V1` below for the unchanged byte bound that
/// still caps any single batch regardless of this page count).
///
/// Re-measured on this repository's 4925-file checkout (2.6GiB staging
/// artifact, ~3900 committed pages, one `tracedecay status` sample every 10s
/// through a `scripts/ci-pr-dogfood-smoke.sh` run) before raising anything
/// further: the per-transaction cost is not fully fixed. At a constant
/// 64-page batch `last_commit_latency_micros` rose from 567ms at 64 committed
/// pages to 4881ms at 2006 in a `dev`-profile binary, and from 861ms at 115
/// pages to ~1686ms at 3678 in a `release` binary -- with `journal_mode=DELETE`
/// and a 64MiB page cache, each commit journals and re-flushes every
/// posting/exact index page the batch dirtied, and that page set widens as the
/// B-trees outgrow the cache. Even so the whole batch phase was only ~66s of
/// the 262s source phase (release) / ~118s of 403s (dev), so raising the page
/// cap again is worth at most a tenth of one phase and was deliberately not
/// done here. Both timings are dwarfed by the code-graph activation that
/// strict readiness also requires, which is where the dogfood budget actually
/// goes. Whoever does raise it must raise the caller hint with it
/// (`registry::TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1`): the sealed source
/// offers `min(hint, TEXT_ARTIFACT_BATCH_PAGES_V1)` pages, so a stale hint
/// silently keeps the old batch size. Offering more pages is otherwise safe by
/// construction -- [`CodeLexicalArtifactBuilderV1::prepare_admissible_page_prefix`]
/// returns an accepted prefix clamped against the row cap and the memory
/// ledger, so the cap is an upper offer, never a reservation.
const TEXT_ARTIFACT_BATCH_PAGES_V1: usize = 64;
const TEXT_ARTIFACT_BATCH_BYTES_V1: usize = 64 * 1024 * 1024;
/// One synchronous activation advances only this many page/finalization
/// operations. Larger caller hints are clamped so work accounting cannot
/// overflow and every expensive loop retains cancellation checkpoints.
/// Doubled alongside `TEXT_ARTIFACT_BATCH_PAGES_V1` (see its comment) so one
/// wake can still commit two full-sized batches under the raised page cap.
const TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1: usize = 128;
/// Cancellation-checkpoint cadence for a wake parked behind another wake's
/// corpus-sized verified head open. The parked wake re-checks its typed
/// cancellation state at this interval, so shutdown or supersession surfaces
/// as `Cancelled` even while the owning open is inside one long read or
/// digest call that has not yet reached its own checkpoint.
const TEXT_HEAD_OPEN_CANCELLATION_CHECK_INTERVAL_V1: Duration = Duration::from_millis(100);
/// Anti-livelock ceiling on the advances owner-warmup will drive.
///
/// A single `TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1` advance never
/// finalizes even a one-file generation, so [`LatestCodeTextGenerationV1::production_query_owners`]
/// keeps advancing until the build reports completion. Each advance is
/// guaranteed to make progress -- it either finalizes or consumes its full
/// page/finalization budget -- so this is a bound against a source that never
/// reports completion, not a work budget or a tunable. Exceeding it still
/// yields the same retryable warming error the caller already handles.
/// Activation itself stays one bounded advance so graph warm and oversized
/// hints never wait on the text projection.
#[cfg(any(test, feature = "test-helpers"))]
const TEXT_ARTIFACT_MAXIMUM_ACTIVATION_ADVANCES_V1: usize = 10_000;
/// Rows digested by one scheduler finalization operation. The builder persists
/// its exact section/row cursor after this bounded slice, avoiding both a
/// corpus-sized wake and one scheduler wake per individual `SQLite` row.
const TEXT_ARTIFACT_FINALIZATION_ROWS_PER_OPERATION_V1: usize = 4 * 1024;

#[cfg(feature = "hotpath")]
static CODE_INDEX_GENERATION_DECODES_ACTIVE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "hotpath")]
static CODE_INDEX_GENERATION_DECODE_WAITERS: AtomicUsize = AtomicUsize::new(0);

pub fn scoped_code_index_store_root(store_root: &Path, canonical_project_root: &Path) -> PathBuf {
    tracedecay_code_index_retention::code_index_generations::scoped_code_index_store_root(
        store_root,
        canonical_project_root,
    )
}

/// How the scheduler is hinted about changes.
///
/// `TraceDecay`'s edits are agent-driven, so the daemon already learns about
/// touched paths through host after-file-edit hooks; those are the primary
/// hint source and require no standing filesystem watches. gix status remains
/// the sole truth, reconciled lazily (on open, on hook receipt, and on the
/// query-admission freshness ladder).
#[derive(Clone, Copy, Debug)]
pub struct CodeIndexHintPolicyV1 {
    /// Tier-2 bounded-staleness reconcile threshold for non-git mutations.
    pub staleness_threshold: Duration,
}

impl Default for CodeIndexHintPolicyV1 {
    fn default() -> Self {
        Self {
            staleness_threshold: DEFAULT_STALENESS_THRESHOLD,
        }
    }
}

type ProductionOwner =
    ProductionCodeIndexOwnerV1<DaemonCodeIndexPublicationStoreV1, DaemonProjectionSinkV1>;

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeIndexBytePoolStatsV1 {
    pub inserted: u64,
    pub reused: u64,
    pub parse_chunk_inserted: u64,
    pub parse_chunk_reused: u64,
}

pub struct SharedCodeIndexBytePoolV1 {
    bytes: ProfiledStdMutex<BTreeMap<ContentDigest, Weak<[u8]>>>,
    physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    inserted: AtomicU64,
    reused: AtomicU64,
    /// Map length recorded after the last dead-entry prune. Weak entries whose
    /// `Arc` dropped are never removed by lookups, so `intern` prunes them once
    /// the map doubles past this baseline, bounding growth over the daemon
    /// lifetime at amortized O(1) per insert.
    last_prune_len: AtomicUsize,
}

impl Default for SharedCodeIndexBytePoolV1 {
    fn default() -> Self {
        Self {
            bytes: hotpath::mutex!(
                Mutex::new(BTreeMap::new()),
                label = "daemon.code_index.byte_pool"
            ),
            physical_artifacts: SharedPhysicalCodeArtifactPoolV1::default(),
            inserted: AtomicU64::new(0),
            reused: AtomicU64::new(0),
            last_prune_len: AtomicUsize::new(0),
        }
    }
}

impl SharedCodeIndexBytePoolV1 {
    fn intern(&self, bytes: Vec<u8>) -> (ContentDigest, Arc<[u8]>) {
        let digest = content_digest(&bytes);
        // The guarded value is a weak-reference cache keyed by content digest:
        // every critical section is a lookup or an insert, so a poisoned lock
        // can only mean an unrelated thread unwound while holding it, never
        // that the map is half-written. Recovering the guard keeps indexing
        // serving instead of turning one unrelated panic into a permanent
        // daemon-wide code-index outage.
        let mut pool = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(shared) = pool.get(&digest).and_then(Weak::upgrade) {
            self.reused.fetch_add(1, Ordering::Relaxed);
            return (digest, shared);
        }
        let shared: Arc<[u8]> = Arc::from(bytes);
        pool.insert(digest.clone(), Arc::downgrade(&shared));
        self.inserted.fetch_add(1, Ordering::Relaxed);
        if pool.len()
            > self
                .last_prune_len
                .load(Ordering::Relaxed)
                .saturating_mul(2)
        {
            pool.retain(|_, entry| entry.strong_count() > 0);
            self.last_prune_len
                .store(pool.len().max(1), Ordering::Relaxed);
        }
        (digest, shared)
    }

    #[cfg(test)]
    fn stats(&self) -> CodeIndexBytePoolStatsV1 {
        let physical_artifacts = self.physical_artifacts.stats();
        CodeIndexBytePoolStatsV1 {
            inserted: self.inserted.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            parse_chunk_inserted: physical_artifacts.inserted,
            parse_chunk_reused: physical_artifacts.reused,
        }
    }
}

/// How many non-active sealed generations stay decoded for repeat pinned or
/// cursor-paged reads. Pinned reads target one generation for the life of a
/// paged query, so a small cache converts a per-page rescan into a single load.
///
/// The ACTIVE generation is never counted against this bound: it lives in its
/// own pinned slot (see [`DecodedGenerationStateV1::active`]) because it serves
/// every unpinned query and must not be evictable by cursor traffic over
/// superseded generations.
const DECODED_GENERATION_CACHE_CAPACITY: usize = 4;

/// Whether one generation resolution may enter the single-flight sealed-decode.
///
/// Decoding a sealed generation is O(store). A query that already has a
/// complete generation it can serve must never queue behind that decode:
/// awaiting a *new* generation may not preempt serving an *old* one. Such a
/// query resolves with [`Self::AlreadyDecoded`] and abstains rather than
/// parking; only a query with nothing servable resolves with
/// [`Self::AwaitDecode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationDecodeAdmissionV1 {
    /// Join (or start) the single-flight decode of the active generation.
    AwaitDecode,
    /// Serve the active generation only if it is already decoded; never claim a
    /// decode lease and never park on the barrier.
    AlreadyDecoded,
}

/// Which sealed generation one decode lease covers.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DecodeSubjectV1 {
    /// The generation named by the durable active-publication pointer.
    Active,
    /// One immutable non-active generation, addressed by identity.
    Generation(CodeGenerationId),
}

/// Decoded-generation cache state.
///
/// Guarded by [`DecodedGenerationCacheV1::state`]. The lock is only ever held
/// for pointer-sized bookkeeping — never across a decode.
#[derive(Default)]
struct DecodedGenerationStateV1 {
    /// The pinned active generation.
    ///
    /// Held outside the LRU deque: every unpinned query serves from it, so LRU
    /// pressure from pinned or cursor-paged reads of older generations must
    /// never be able to drop it and force a re-decode on the request path.
    active: Option<Arc<CodeIndexPublishedGenerationV1>>,
    /// Bumped by every successful publication. A decode that started before a
    /// publication landed must not install its now-superseded result.
    active_epoch: u64,
    /// Already-decoded, already-verified NON-active generations, newest last.
    ///
    /// Decoding a sealed generation re-reads every generation file in the store
    /// and fully re-validates each one, so serving a pinned generation per page
    /// repeated that whole scan per access. A published generation is immutable
    /// and content-addressed by its sealed filename, so a generation that
    /// loaded once can be served again without redoing the load-time checks.
    decoded: VecDeque<Arc<CodeIndexPublishedGenerationV1>>,
    /// Decodes currently running. A caller that wants one of these parks on the
    /// condvar instead of starting a second sweep over the same bytes.
    in_flight: Vec<DecodeSubjectV1>,
}

impl DecodedGenerationStateV1 {
    fn is_in_flight(&self, subject: &DecodeSubjectV1) -> bool {
        self.in_flight.iter().any(|pending| pending == subject)
    }

    fn forget(&mut self, generation_id: &CodeGenerationId) {
        self.decoded
            .retain(|cached| cached.manifest().generation_id != *generation_id);
    }

    /// Serve an already-decoded non-active generation, refreshing its recency.
    fn cached(
        &mut self,
        generation_id: &CodeGenerationId,
    ) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        let position = self
            .decoded
            .iter()
            .position(|cached| cached.manifest().generation_id == *generation_id)?;
        let generation = self.decoded.remove(position)?;
        self.decoded.push_back(Arc::clone(&generation));
        Some(generation)
    }
}

/// Single-flight decode barrier for sealed code generations.
///
/// Decoding a sealed generation is O(store): it re-reads the sealed bytes,
/// re-mints every file's exact-extraction authority (a canonical SHA-256 over
/// every chunk), and repeats the full canonical validation sweep. On a
/// 149K-node store that is tens of seconds of pure CPU, so it belongs at
/// activation time, once per generation, and never on a request.
///
/// The barrier provides three properties the previous `Mutex<Option<_>>` could
/// not:
///
/// - the decode NEVER runs while the cache lock is held, so a reader that only
///   needs an already-decoded generation is not queued behind an unrelated
///   decode;
/// - concurrent callers wanting the SAME generation share one decode — the
///   first claims a lease, the rest park on the condvar — so a request that
///   arrives mid-decode joins the in-flight work instead of duplicating it;
/// - only success is published. A failed decode leaves no memo, so the next
///   caller re-runs the complete check and observes the same error. The
///   fail-closed gate is unchanged.
#[derive(Default)]
struct DecodedGenerationCacheV1 {
    state: Mutex<DecodedGenerationStateV1>,
    ready: Condvar,
    /// Sealed-bytes decodes actually performed by this process. Test probe for
    /// "the serving path did not re-decode".
    decodes: AtomicU64,
    #[cfg(test)]
    active_waiters: AtomicUsize,
}

impl DecodedGenerationCacheV1 {
    fn poisoned() -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(
            "daemon decoded-generation lock is poisoned".to_owned(),
        )
    }

    fn lock_state(
        &self,
    ) -> Result<MutexGuard<'_, DecodedGenerationStateV1>, CodeIndexPublicationStoreErrorV1> {
        self.state.lock().map_err(|_| Self::poisoned())
    }

    fn note_decode(&self) {
        self.decodes.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn decode_count(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    /// Retain an already-decoded non-active generation under the LRU bound.
    ///
    /// The active generation is pinned in its own slot and is deliberately not
    /// admitted here, so cursor traffic over superseded generations can never
    /// evict the generation every unpinned query serves from.
    fn remember(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut state = self.lock_state()?;
        let generation_id = generation.manifest().generation_id.clone();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.manifest().generation_id == generation_id)
        {
            return Ok(());
        }
        state.forget(&generation_id);
        state.decoded.push_back(generation);
        while state.decoded.len() > DECODED_GENERATION_CACHE_CAPACITY {
            state.decoded.pop_front();
        }
        Ok(())
    }
}

/// RAII claim on the single in-flight decode for one subject.
///
/// Dropping the lease releases the claim and wakes every parked caller, so a
/// panicking or erroring decode can never strand waiters.
struct DecodeLeaseV1<'cache> {
    cache: &'cache DecodedGenerationCacheV1,
    subject: DecodeSubjectV1,
    /// The active-slot epoch observed when this lease was claimed.
    epoch: u64,
}

impl Drop for DecodeLeaseV1<'_> {
    fn drop(&mut self) {
        {
            let mut state = self
                .cache
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.in_flight.retain(|pending| *pending != self.subject);
        }
        self.cache.ready.notify_all();
    }
}

#[cfg(feature = "hotpath")]
struct GenerationDecodeObservationV1;

#[cfg(feature = "hotpath")]
impl GenerationDecodeObservationV1 {
    fn enter() -> Self {
        let active = CODE_INDEX_GENERATION_DECODES_ACTIVE
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("code_index.generation.decode.attempts_total").inc(1_u64);
        hotpath::gauge!("code_index.generation.decode.active").set(active);
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for GenerationDecodeObservationV1 {
    fn drop(&mut self) {
        let _ = CODE_INDEX_GENERATION_DECODES_ACTIVE.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |active| active.checked_sub(1),
        );
        hotpath::gauge!("code_index.generation.decode.active")
            .set(CODE_INDEX_GENERATION_DECODES_ACTIVE.load(Ordering::Relaxed));
    }
}

#[cfg(feature = "hotpath")]
struct GenerationDecodeWaitObservationV1;

#[cfg(feature = "hotpath")]
impl GenerationDecodeWaitObservationV1 {
    fn enter() -> Self {
        let waiters = CODE_INDEX_GENERATION_DECODE_WAITERS
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("code_index.generation.decode.waiters").set(waiters);
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for GenerationDecodeWaitObservationV1 {
    fn drop(&mut self) {
        let _ = CODE_INDEX_GENERATION_DECODE_WAITERS.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |waiters| waiters.checked_sub(1),
        );
        hotpath::gauge!("code_index.generation.decode.waiters")
            .set(CODE_INDEX_GENERATION_DECODE_WAITERS.load(Ordering::Relaxed));
    }
}

/// Test-only occupation of the active decode barrier. See
/// [`DaemonCodeIndexPublicationStoreV1::hold_active_decode`].
#[cfg(test)]
pub struct HeldActiveDecodeV1 {
    cache: Arc<DecodedGenerationCacheV1>,
    restore: Option<Arc<CodeIndexPublishedGenerationV1>>,
}

#[cfg(test)]
impl Drop for HeldActiveDecodeV1 {
    fn drop(&mut self) {
        {
            let mut state = self
                .cache
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state
                .in_flight
                .retain(|pending| *pending != DecodeSubjectV1::Active);
            if state.active.is_none() {
                state.active = self.restore.take();
            }
        }
        self.cache.ready.notify_all();
    }
}

#[cfg(test)]
impl HeldActiveDecodeV1 {
    pub fn waiter_count(&self) -> usize {
        self.cache.active_waiters.load(Ordering::Acquire)
    }
}

/// Last validated publication pointer, reused when the on-disk file is unchanged.
struct PublicationPointerMemoV1 {
    mtime: Option<SystemTime>,
    size: u64,
    digest: String,
    pointer: DurablePublicationPointerV1,
}

#[derive(Clone)]
struct UndecodedActivePublicationExpectationV1 {
    generation_id: String,
    generation_file: String,
    state_digest: String,
}

impl UndecodedActivePublicationExpectationV1 {
    fn matches(&self, pointer: &DurablePublicationPointerV1) -> bool {
        self.generation_id == pointer.generation_id
            && self.generation_file == pointer.generation_file
            && self.state_digest == pointer.state_digest
    }
}

#[derive(Clone)]
pub struct DaemonCodeIndexPublicationStoreV1 {
    cache: Arc<DecodedGenerationCacheV1>,
    active_encoded_bytes: Arc<AtomicU64>,
    seal_encoded_segment_bytes: Arc<AtomicU64>,
    seal_existing_segment_bytes_read: Arc<AtomicU64>,
    seal_evidence_page_count: Arc<AtomicU64>,
    seal_evidence_durable_transaction_count: Arc<AtomicU64>,
    active_path: PathBuf,
    generations_root: PathBuf,
    segments_root: PathBuf,
    project_root: PathBuf,
    expected_sanitizer_revision: SanitizerRevision,
    disposition: CodeIndexPublicationDispositionV1,
    pointer_memo: Arc<ProfiledStdMutex<Option<PublicationPointerMemoV1>>>,
    undecoded_active_expectation: Option<UndecodedActivePublicationExpectationV1>,
    /// The canonical source-hint authority plus the exact pre-capture epoch
    /// used by a retained rebuild. Ordinary publication leaves this absent.
    reconcile_publication_fence: Option<(Arc<Mutex<PendingHintsV1>>, DaemonCodeIndexControlV1)>,
    /// Last generation handed to `publish_atomically`. A transient store
    /// failure must not drop it: the next undecoded retry republishes this
    /// candidate instead of extracting the whole worktree again.
    unpublished_candidate: Arc<Mutex<Option<Arc<CodeIndexPublishedGenerationV1>>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexPublicationDispositionV1 {
    Active,
    RetainedHistory,
}

struct TemporaryGenerationFileV1 {
    path: PathBuf,
    committed: bool,
}

struct TemporaryEvidencePackV1 {
    path: PathBuf,
    file: Option<File>,
    hasher: Sha256,
    size_bytes: u64,
    page_count: u32,
    committed: bool,
    published_path: Option<PathBuf>,
}

struct PinnedGenerationSegmentV1 {
    digest: String,
    size_bytes: u64,
    file: File,
}

impl TemporaryEvidencePackV1 {
    fn create(path: PathBuf) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        Ok(Self {
            path,
            file: Some(file),
            hasher: Sha256::new(),
            size_bytes: 0,
            page_count: 0,
            committed: false,
            published_path: None,
        })
    }

    fn append_page(
        &mut self,
        page_ordinal: u32,
        page_digest: &ManifestDigest,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if page_ordinal != self.page_count {
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "sealed evidence pages are not canonically ordered",
            ));
        }
        if DaemonCodeIndexPublicationStoreV1::state_digest(bytes) != page_digest.as_str() {
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "sealed evidence page bytes do not match their content address",
            ));
        }
        self.file
            .as_mut()
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack temporary file is already closed",
                )
            })?
            .write_all(bytes)
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        self.hasher.update(bytes);
        self.size_bytes = self
            .size_bytes
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?,
            )
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack length exceeds u64",
                )
            })?;
        self.page_count = self.page_count.checked_add(1).ok_or_else(|| {
            DaemonCodeIndexPublicationStoreV1::unavailable(
                "sealed evidence pack page count exceeds u32",
            )
        })?;
        Ok(())
    }

    fn commit(
        &mut self,
        root: &Path,
        segment_digest: &ManifestDigest,
        segment_size_bytes: u64,
        page_count: u32,
    ) -> Result<bool, CodeIndexPublicationStoreErrorV1> {
        let actual_digest = ManifestDigest::from_sha256_bytes(
            &std::mem::replace(&mut self.hasher, Sha256::new()).finalize(),
        )
        .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        if self.page_count != page_count
            || self.size_bytes != segment_size_bytes
            || &actual_digest != segment_digest
        {
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "sealed evidence pack does not match its commit identity",
            ));
        }
        let digest_hex = segment_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack digest is not sha256",
                )
            })?;
        let final_path = root.join(format!("segment-{digest_hex}.json"));
        match final_path.symlink_metadata() {
            Ok(metadata) => {
                if !metadata.file_type().is_file()
                    || metadata.len() != segment_size_bytes
                    || DaemonCodeIndexPublicationStoreV1::state_digest_file(&final_path)?
                        != segment_digest.as_str()
                {
                    return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                        "existing sealed evidence pack does not match its content address",
                    ));
                }
                drop(self.file.take());
                std::fs::remove_file(&self.path)
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
                self.committed = true;
                return Ok(false);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DaemonCodeIndexPublicationStoreV1::unavailable(error)),
        }
        self.file
            .as_ref()
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack temporary file is already closed",
                )
            })?
            .sync_all()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        drop(self.file.take());
        std::fs::rename(&self.path, &final_path)
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        self.published_path = Some(final_path);
        DaemonCodeIndexPublicationStoreV1::sync_directory(root)?;
        Ok(true)
    }

    fn attach_to_manifest(&mut self) {
        self.committed = true;
    }

    fn rollback_unattached(&mut self, root: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if self.committed {
            return Ok(());
        }
        drop(self.file.take());
        let mut removed_final = false;
        if let Some(path) = self.published_path.take() {
            std::fs::remove_file(path).map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            removed_final = true;
        }
        match self.path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&self.path)
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            }
            Ok(_) => {
                return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack temporary path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DaemonCodeIndexPublicationStoreV1::unavailable(error)),
        }
        if removed_final {
            DaemonCodeIndexPublicationStoreV1::sync_directory(root)?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for TemporaryEvidencePackV1 {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.file.take());
            if let Some(path) = self.published_path.take() {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl TemporaryGenerationFileV1 {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }

    fn rollback_uncommitted(
        &mut self,
        root: &Path,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if self.committed {
            return Ok(());
        }
        match self.path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&self.path)
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
                DaemonCodeIndexPublicationStoreV1::sync_directory(root)?;
            }
            Ok(_) => {
                return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed code-generation temporary path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DaemonCodeIndexPublicationStoreV1::unavailable(error)),
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for TemporaryGenerationFileV1 {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl DaemonCodeIndexPublicationStoreV1 {
    fn new(
        store_root: &Path,
        project_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let generations_root = store_root.join("code-generations-v1");
        std::fs::create_dir_all(&generations_root)?;
        let segments_root = store_root.join("code-generation-segments-v1");
        std::fs::create_dir_all(&segments_root)?;
        let _store_lock = acquire_code_generation_store_lock(store_root)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Self::remove_abandoned_evidence_packs(&segments_root)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            cache: Arc::new(DecodedGenerationCacheV1::default()),
            active_encoded_bytes: Arc::new(AtomicU64::new(0)),
            seal_encoded_segment_bytes: Arc::new(AtomicU64::new(0)),
            seal_existing_segment_bytes_read: Arc::new(AtomicU64::new(0)),
            seal_evidence_page_count: Arc::new(AtomicU64::new(0)),
            seal_evidence_durable_transaction_count: Arc::new(AtomicU64::new(0)),
            active_path: store_root.join("active-code-generation-v1.json"),
            generations_root,
            segments_root,
            project_root: project_root.to_path_buf(),
            expected_sanitizer_revision,
            disposition: CodeIndexPublicationDispositionV1::Active,
            pointer_memo: Arc::new(hotpath::mutex!(
                Mutex::new(None),
                label = "daemon.code_index.publication.pointer_memo"
            )),
            undecoded_active_expectation: None,
            reconcile_publication_fence: None,
            unpublished_candidate: Arc::new(Mutex::new(None)),
        })
    }

    fn for_undecoded_active_rebuild(&self, pointer: &DurablePublicationPointerV1) -> Self {
        let mut publication = self.clone();
        publication.undecoded_active_expectation = Some(UndecodedActivePublicationExpectationV1 {
            generation_id: pointer.generation_id.clone(),
            generation_file: pointer.generation_file.clone(),
            state_digest: pointer.state_digest.clone(),
        });
        publication
    }

    fn with_reconcile_publication_fence(
        mut self,
        hints: Arc<Mutex<PendingHintsV1>>,
        control: DaemonCodeIndexControlV1,
    ) -> Self {
        self.reconcile_publication_fence = Some((hints, control));
        self
    }

    fn retained_history(&self) -> Self {
        let mut retained = self.clone();
        retained.disposition = CodeIndexPublicationDispositionV1::RetainedHistory;
        retained
    }

    fn unavailable(error: impl std::fmt::Display) -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string())
    }

    fn corruption(error: impl std::fmt::Display) -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(error.to_string())
    }

    fn acquire_generation_read_lock(
        &self,
    ) -> Result<CodeGenerationStoreLockV1, CodeIndexPublicationStoreErrorV1> {
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        acquire_code_generation_store_lock(store_root).map_err(Self::unavailable)
    }

    fn remove_abandoned_evidence_packs(
        segments_root: &Path,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut removed = false;
        for entry in std::fs::read_dir(segments_root).map_err(Self::unavailable)? {
            let entry = entry.map_err(Self::unavailable)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(".evidence-pack-publication.") || !name.ends_with(".tmp") {
                continue;
            }
            let metadata = entry.path().symlink_metadata().map_err(Self::unavailable)?;
            if !metadata.file_type().is_file() {
                return Err(Self::unavailable(
                    "sealed evidence pack temporary path is not a regular file",
                ));
            }
            std::fs::remove_file(entry.path()).map_err(Self::unavailable)?;
            removed = true;
        }
        if removed {
            Self::sync_directory(segments_root)?;
        }
        Ok(())
    }

    fn sync_directory(path: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        tracedecay_private_fs::framed_log::sync_directory(path, DirectorySyncPolicy::Strict)
            .map_err(Self::unavailable)
    }

    fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(Self::unavailable)?;
        let mut file = hotpath::io!(file, label = "code_index.generation.sealing.io");
        file.write_all(bytes).map_err(Self::unavailable)?;
        file.sync_all().map_err(Self::unavailable)
    }

    #[hotpath::measure(label = "code_index.generation.publish.segment")]
    fn publish_segment_durable(
        &self,
        digest: &ManifestDigest,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let digest_hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| Self::unavailable("sealed segment digest is not sha256"))?;
        let expected_digest = digest.as_str();
        if Self::state_digest(bytes) != expected_digest {
            return Err(Self::corruption(
                "sealed segment bytes do not match their content address",
            ));
        }
        let final_path = self
            .segments_root
            .join(format!("segment-{digest_hex}.json"));
        match final_path.symlink_metadata() {
            Ok(metadata) => {
                self.seal_existing_segment_bytes_read
                    .fetch_add(metadata.len(), Ordering::Relaxed);
                if !metadata.file_type().is_file()
                    || metadata.len() != u64::try_from(bytes.len()).map_err(Self::unavailable)?
                    || Self::state_digest_file(&final_path)? != expected_digest
                {
                    return Err(Self::corruption(
                        "existing sealed segment does not match its content address",
                    ));
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Self::unavailable(error)),
        }
        let temporary_path = self.segments_root.join(format!(
            ".segment-publication.{}.{}.tmp",
            std::process::id(),
            digest_hex
        ));
        match temporary_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&temporary_path).map_err(Self::unavailable)?;
            }
            Ok(_) => {
                return Err(Self::unavailable(
                    "sealed segment temporary path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Self::unavailable(error)),
        }
        Self::write_durable(&temporary_path, bytes)?;
        std::fs::rename(&temporary_path, &final_path).map_err(Self::unavailable)?;
        Self::sync_directory(&self.segments_root)
    }

    fn state_digest_file(path: &Path) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        let mut file = File::open(path).map_err(Self::unavailable)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        loop {
            let read = file.read(&mut buffer).map_err(Self::unavailable)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(encode_tagged_lowercase_hex("sha256:", &hasher.finalize()))
    }

    fn files_equal(left: &Path, right: &Path) -> Result<bool, CodeIndexPublicationStoreErrorV1> {
        let left_metadata = left.symlink_metadata().map_err(Self::unavailable)?;
        let right_metadata = right.symlink_metadata().map_err(Self::unavailable)?;
        if !left_metadata.file_type().is_file() || !right_metadata.file_type().is_file() {
            return Err(Self::unavailable(
                "immutable code-generation path is not a regular file",
            ));
        }
        if left_metadata.len() != right_metadata.len() {
            return Ok(false);
        }
        let mut left = File::open(left).map_err(Self::unavailable)?;
        let mut right = File::open(right).map_err(Self::unavailable)?;
        let mut left_buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        let mut right_buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        loop {
            let left_read = left.read(&mut left_buffer).map_err(Self::unavailable)?;
            if left_read == 0 {
                return Ok(right
                    .read(&mut right_buffer[..1])
                    .map_err(Self::unavailable)?
                    == 0);
            }
            right
                .read_exact(&mut right_buffer[..left_read])
                .map_err(Self::unavailable)?;
            if left_buffer[..left_read] != right_buffer[..left_read] {
                return Ok(false);
            }
        }
    }

    fn state_digest(bytes: &[u8]) -> String {
        encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes))
    }

    fn generation_index_digest(
        entries: &[DurableGenerationIndexEntryV1],
        truncated: bool,
    ) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        durable_generation_index_digest(entries, truncated).map_err(Self::unavailable)
    }

    fn generation_cardinality(
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<DurableGenerationCardinalityV1, CodeIndexPublicationStoreErrorV1> {
        Ok(DurableGenerationCardinalityV1 {
            file_count: u64::try_from(generation.snapshot().files.len())
                .map_err(Self::unavailable)?,
            chunk_count: u64::try_from(generation.chunks().chunks().len())
                .map_err(Self::unavailable)?,
            symbol_count: u64::try_from(generation.symbols().symbols.len())
                .map_err(Self::unavailable)?,
        })
    }

    fn validate_generation_file(value: &str) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let path = Path::new(value);
        if value.is_empty()
            || value.contains(['/', '\\'])
            || path.file_name().and_then(|name| name.to_str()) != Some(value)
            || !value.ends_with(".json")
        {
            return Err(Self::unavailable(
                "active code-generation pointer contains an invalid generation file",
            ));
        }
        Ok(())
    }

    fn read_publication_pointer(
        &self,
    ) -> Result<Option<DurablePublicationPointerV1>, CodeIndexPublicationStoreErrorV1> {
        let metadata = match std::fs::metadata(&self.active_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                *self
                    .pointer_memo
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = None;
                return Ok(None);
            }
            Err(error) => return Err(Self::unavailable(error)),
        };
        if metadata.len() > MAX_DURABLE_PUBLICATION_POINTER_BYTES {
            return Err(Self::corruption(
                "durable code-generation index exceeds its byte bound",
            ));
        }
        let mtime = metadata.modified().ok();
        let size = metadata.len();
        {
            let memo = self
                .pointer_memo
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(memo) = memo.as_ref()
                && memo.size == size
                && memo.mtime.is_some()
                && memo.mtime == mtime
            {
                return Ok(Some(memo.pointer.clone()));
            }
        }
        let bytes = std::fs::read(&self.active_path).map_err(Self::unavailable)?;
        let digest = Self::state_digest(&bytes);
        {
            let mut memo = self
                .pointer_memo
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(memo) = memo.as_mut()
                && memo.digest == digest
            {
                memo.mtime = mtime;
                memo.size = size;
                return Ok(Some(memo.pointer.clone()));
            }
        }
        let pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                Self::corruption(format!(
                    "active code-generation pointer is corrupt: {error}"
                ))
            })?;
        Self::validate_generation_file(&pointer.generation_file)
            .map_err(|error| Self::corruption(error.to_string()))?;
        if pointer.generation_index.len() > MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 {
            return Err(Self::corruption(
                "durable code-generation index exceeds its entry bound",
            ));
        }
        match pointer.generation_index_digest.as_deref() {
            Some(digest)
                if digest
                    == Self::generation_index_digest(
                        &pointer.generation_index,
                        pointer.generation_index_truncated,
                    )? => {}
            None if pointer.generation_index.is_empty() && !pointer.generation_index_truncated => {}
            _ => {
                return Err(Self::corruption(
                    "durable code-generation index digest does not match its entries",
                ));
            }
        }
        let mut generations = BTreeSet::new();
        let mut exact_revisions = BTreeSet::new();
        let mut prior_order = None;
        for entry in &pointer.generation_index {
            Self::validate_generation_file(&entry.generation_file)
                .map_err(|error| Self::corruption(error.to_string()))?;
            CodeGenerationId::new(entry.generation_id.clone()).map_err(Self::corruption)?;
            ContentDigest::new(entry.snapshot_content_identity.clone())
                .map_err(Self::corruption)?;
            if !entry.state_digest.starts_with("sha256:")
                || entry.state_digest.len() != "sha256:".len() + 64
            {
                return Err(Self::corruption(
                    "durable code-generation index contains an invalid sealed digest",
                ));
            }
            if entry.size_bytes == 0 {
                return Err(Self::corruption(
                    "durable code-generation index contains an invalid zero byte size",
                ));
            }
            if !generations.insert(entry.generation_id.as_str()) {
                return Err(Self::corruption(
                    "durable code-generation index contains a duplicate generation",
                ));
            }
            match (
                &entry.source_reference,
                &entry.source_revision,
                &entry.source_tree,
            ) {
                (Some(reference), Some(revision), Some(tree)) => {
                    tracedecay_domain::RefId::new(reference.clone()).map_err(Self::corruption)?;
                    tracedecay_domain::GitOidV1::new(revision.clone()).map_err(Self::corruption)?;
                    tracedecay_domain::GitOidV1::new(tree.clone()).map_err(Self::corruption)?;
                    if !exact_revisions.insert((
                        reference.as_str(),
                        revision.as_str(),
                        tree.as_str(),
                    )) {
                        return Err(Self::corruption(
                            "durable code-generation index contains duplicate Git evidence",
                        ));
                    }
                }
                (None, None, None) => {}
                _ => {
                    return Err(Self::corruption(
                        "durable code-generation index contains incomplete Git evidence",
                    ));
                }
            }
            let order = (entry.sealed_at_micros, entry.generation_id.as_str());
            if prior_order.is_some_and(|prior| prior >= order) {
                return Err(Self::corruption(
                    "durable code-generation index is not canonically ordered",
                ));
            }
            prior_order = Some(order);
        }
        let Some(active_entry) = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == pointer.generation_id)
        else {
            return Err(Self::corruption(
                "durable code-generation index does not contain its active generation",
            ));
        };
        if active_entry.snapshot_content_identity != pointer.snapshot_content_identity
            || active_entry.sealed_at_micros != pointer.sealed_at_micros
            || active_entry.generation_file != pointer.generation_file
            || active_entry.state_digest != pointer.state_digest
        {
            return Err(Self::corruption(
                "durable code-generation index active entry does not match its pointer",
            ));
        }
        let mut bounded_index = pointer.generation_index.clone();
        if retain_bounded_generation_index(&mut bounded_index, &pointer.generation_id) > 0
            || bounded_index != pointer.generation_index
        {
            return Err(Self::corruption(
                "durable code-generation index exceeds its retention bounds",
            ));
        }
        *self
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(PublicationPointerMemoV1 {
            mtime,
            size,
            digest,
            pointer: pointer.clone(),
        });
        Ok(Some(pointer))
    }

    fn remember_publication_pointer(&self, pointer: &DurablePublicationPointerV1, bytes: &[u8]) {
        let metadata = match std::fs::metadata(&self.active_path) {
            Ok(metadata) => metadata,
            Err(_) => {
                *self
                    .pointer_memo
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = None;
                return;
            }
        };
        *self
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(PublicationPointerMemoV1 {
            mtime: metadata.modified().ok(),
            size: metadata.len(),
            digest: Self::state_digest(bytes),
            pointer: pointer.clone(),
        });
    }

    #[hotpath::measure(label = "code_index.generation.decode.segment")]
    fn read_partitioned_segment(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let (digest, expected_size, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest, size_bytes, 0, size_bytes)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest, size_bytes, offset, length),
        };
        if offset
            .checked_add(length)
            .is_none_or(|end| end > expected_size)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment range exceeds its manifest identity".to_owned(),
            ));
        }
        let digest_hex = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
        })?;
        let path = self
            .segments_root
            .join(format!("segment-{digest_hex}.json"));
        let metadata = path.symlink_metadata().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment is unavailable: {error}"
            ))
        })?;
        if !metadata.file_type().is_file() || metadata.len() != expected_size {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment identity does not match its manifest".to_owned(),
            ));
        }
        buffer.clear();
        let length = usize::try_from(length).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation segment range exceeds addressable memory".to_owned(),
            )
        })?;
        buffer.resize(length, 0);
        File::open(path)
            .and_then(|mut file| {
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(buffer)
            })
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment read failed: {error}"
                ))
            })
    }

    fn open_pinned_partitioned_segment(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<PinnedGenerationSegmentV1, CodeIndexProductionErrorV1> {
        let (digest, expected_size, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest.as_str(), size_bytes, 0, size_bytes)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest.as_str(), size_bytes, offset, length),
        };
        if offset
            .checked_add(length)
            .is_none_or(|end| end > expected_size)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment range exceeds its manifest identity".to_owned(),
            ));
        }
        let digest_hex = digest.strip_prefix("sha256:").ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
        })?;
        let path = self
            .segments_root
            .join(format!("segment-{digest_hex}.json"));
        let path_metadata = path.symlink_metadata().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment is unavailable: {error}"
            ))
        })?;
        if !path_metadata.file_type().is_file() || path_metadata.len() != expected_size {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment identity does not match its manifest".to_owned(),
            ));
        }
        let file = File::open(&path).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment cannot be opened: {error}"
            ))
        })?;
        let file_metadata = file.metadata().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment metadata cannot be read: {error}"
            ))
        })?;
        let file_identity = Handle::from_file(file.try_clone().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment handle cannot be cloned: {error}"
            ))
        })?)
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment handle identity cannot be read: {error}"
            ))
        })?;
        let path_identity = Handle::from_path(&path).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment path identity cannot be read: {error}"
            ))
        })?;
        if !file_metadata.is_file()
            || file_metadata.len() != expected_size
            || file_identity != path_identity
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment identity changed while it was opened".to_owned(),
            ));
        }
        Ok(PinnedGenerationSegmentV1 {
            digest: digest.to_owned(),
            size_bytes: expected_size,
            file,
        })
    }

    fn read_pinned_partitioned_segment(
        pinned: &mut PinnedGenerationSegmentV1,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let (digest, expected_size, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest.as_str(), size_bytes, 0, size_bytes)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest.as_str(), size_bytes, offset, length),
        };
        if offset
            .checked_add(length)
            .is_none_or(|end| end > expected_size)
            || digest != pinned.digest
            || expected_size != pinned.size_bytes
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence page does not match its pinned segment".to_owned(),
            ));
        }
        let length = usize::try_from(length).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation segment range exceeds addressable memory".to_owned(),
            )
        })?;
        buffer.clear();
        buffer.resize(length, 0);
        pinned
            .file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| pinned.file.read_exact(buffer))
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment read failed: {error}"
                ))
            })
    }

    #[hotpath::measure(label = "code_index.generation.validate_partitioned_manifest")]
    fn partitioned_text_metadata(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
    ) -> Result<Option<VerifiedSealedTextGenerationMetadataV1>, CodeIndexPublicationStoreErrorV1>
    {
        Self::validate_generation_file(&identity.locator)?;
        let path = self.generations_root.join(&identity.locator);
        let metadata = path.symlink_metadata().map_err(Self::unavailable)?;
        if !metadata.file_type().is_file() || metadata.len() != identity.size_bytes {
            return Err(Self::corruption(
                "partitioned generation manifest identity is corrupt",
            ));
        }
        let bytes = std::fs::read(path).map_err(Self::unavailable)?;
        if Self::state_digest(&bytes) != identity.digest.as_str() {
            return Err(Self::corruption(
                "partitioned generation manifest digest does not verify",
            ));
        }
        CodeIndexPublishedGenerationV1::partitioned_text_metadata(&bytes)
            .map_err(|error| Self::corruption(error.to_string()))
    }

    #[hotpath::measure(label = "code_index.generation.decode.bundle")]
    fn decode_generation_file(
        &self,
        file: &mut File,
        admitted_len: u64,
        expected_file_digest: &ManifestDigest,
        lifetime_lock: CodeGenerationStoreLockV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let monolithic = CodeIndexPublishedGenerationV1::decode_sealed_seek_reader(
            &mut *file,
            admitted_len,
            Some(expected_file_digest),
            &UninterruptibleCodeIndexControlV1,
        )?;
        if monolithic.is_some() {
            return Ok(monolithic);
        }
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest seek failed: {error}"
            ))
        })?;
        let mut manifest = Vec::new();
        file.read_to_end(&mut manifest).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest read failed: {error}"
            ))
        })?;
        if Self::state_digest(&manifest) != expected_file_digest.as_str() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation manifest filename digest does not match its bytes".to_owned(),
            ));
        }
        let mut lifetime_lock = Some(lifetime_lock);
        let mut pinned_evidence = None;
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(&manifest, |request, buffer| {
            match request {
                SealedGenerationSegmentReadV1::Whole { .. } => {
                    self.read_partitioned_segment(request, buffer)
                }
                SealedGenerationSegmentReadV1::Range { .. } => {
                    if pinned_evidence.is_none() {
                        pinned_evidence = Some(self.open_pinned_partitioned_segment(request)?);
                        // The store lock proves the pack pathname is live through
                        // this open. The handle then owns all remaining page reads.
                        drop(lifetime_lock.take());
                    }
                    Self::read_pinned_partitioned_segment(
                        pinned_evidence.as_mut().ok_or_else(|| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed generation evidence handle was not pinned".to_owned(),
                            )
                        })?,
                        request,
                        buffer,
                    )
                }
            }
        })
    }

    /// Serve one sealed generation by identity, decoding it at most once.
    ///
    /// The active generation answers from its pinned slot. Any other generation
    /// is served from the decoded LRU, or decoded exactly once under a lease —
    /// concurrent pinned or cursor-paged readers of the same generation join the
    /// in-flight decode instead of each rescanning the store.
    fn load_generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        // The pointer already names the active generation. Serve it from the
        // pinned slot instead of decoding the active file a second time just to
        // compare identities, and skip that decode entirely for historical pins.
        if pointer.generation_id == generation_id.as_str() {
            return self.load_active_shared();
        }
        let Some(entry) = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
        else {
            return Ok(None);
        };
        self.load_indexed_generation_shared(generation_id, entry)
    }

    /// Decode one exact indexed generation under its identity-keyed barrier.
    ///
    /// Unlike [`Self::load_generation`], this does not join the active
    /// activation barrier merely because the indexed identity is currently
    /// active. Exact Git reads are immutable and independently bounded, so a
    /// concurrent activation may duplicate this decode but may not make PR or
    /// branch tools unavailable. An already-decoded active generation is still
    /// reused immediately.
    fn load_indexed_generation_shared(
        &self,
        generation_id: &CodeGenerationId,
        entry: &DurableGenerationIndexEntryV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let subject = DecodeSubjectV1::Generation(generation_id.clone());
        let lease = loop {
            let mut state = self.cache.lock_state()?;
            if let Some(active) = state
                .active
                .as_ref()
                .filter(|active| active.manifest().generation_id == *generation_id)
            {
                return Ok(Some(Arc::clone(active)));
            }
            if let Some(cached) = state.cached(generation_id) {
                return Ok(Some(cached));
            }
            if state.is_in_flight(&subject) {
                // Another caller already owns this O(store) decode. Park on it
                // rather than starting a second sweep over the same bytes.
                #[cfg(feature = "hotpath")]
                let _waiting = GenerationDecodeWaitObservationV1::enter();
                let _parked =
                    hotpath::measure_block!("code_index.generation.decode.singleflight_wait", {
                        self.cache
                            .ready
                            .wait(state)
                            .map_err(|_| DecodedGenerationCacheV1::poisoned())
                    })?;
                continue;
            }
            let epoch = state.active_epoch;
            state.in_flight.push(subject.clone());
            drop(state);
            break DecodeLeaseV1 {
                cache: &self.cache,
                subject: subject.clone(),
                epoch,
            };
        };
        // Decoded with NO cache lock held: unrelated readers and publishers keep
        // making progress while this runs.
        let matched = self.load_indexed_generation(generation_id, entry);
        if let Ok(Some(generation)) = matched.as_ref() {
            self.cache.remember(Arc::clone(generation))?;
        }
        drop(lease);
        matched
    }

    /// Resolve one generation through the bounded durable index and read only
    /// its content-addressed sealed file.
    fn load_indexed_generation(
        &self,
        generation_id: &CodeGenerationId,
        entry: &DurableGenerationIndexEntryV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let expected_file = format!(
            "generation-{}.json",
            entry
                .state_digest
                .strip_prefix("sha256:")
                .unwrap_or(&entry.state_digest)
        );
        if entry.generation_file != expected_file {
            return Err(Self::corruption(
                "durable code-generation index file does not match its sealed digest",
            ));
        }
        let lifetime_lock = self.acquire_generation_read_lock()?;
        let path = self.generations_root.join(&entry.generation_file);
        let metadata = path.symlink_metadata().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("durable code-generation index target is missing")
            } else {
                Self::unavailable(error)
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(Self::corruption(
                "durable code-generation index target is not a file",
            ));
        }
        if metadata.len() != entry.size_bytes
            || metadata.len() > MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        {
            return Err(Self::corruption(
                "indexed code-generation byte size does not match its durable entry",
            ));
        }
        #[cfg(feature = "hotpath")]
        let _decode = GenerationDecodeObservationV1::enter();
        let expected_digest = ManifestDigest::new(entry.state_digest.clone()).map_err(|error| {
            Self::corruption(format!(
                "durable code-generation digest is not canonical: {error}"
            ))
        })?;
        let mut file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("durable code-generation index target disappeared during read")
            } else {
                Self::unavailable(error)
            }
        })?;
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("code_index.generation.decode.bytes_total").inc(entry.size_bytes);
        let decoded = hotpath::measure_block!(
            "code_index.generation.decode.file_read",
            self.decode_generation_file(
                &mut file,
                entry.size_bytes,
                &expected_digest,
                lifetime_lock,
            )
        );
        // A failing decode still swept the sealed bytes, and fail-closed
        // serving depends on that sweep re-running per request; count it
        // before propagating the error. Only the typed incompatible
        // abstention (`Ok(None)`) never was a real decode.
        if !matches!(decoded, Ok(None)) {
            self.cache.note_decode();
        }
        let Some(generation) = decoded.map_err(Self::corruption)? else {
            return Ok(None);
        };
        if let Some(cardinality) = entry.cardinality.as_ref()
            && Self::generation_cardinality(&generation)? != *cardinality
        {
            return Err(Self::corruption(
                "durable code-generation cardinality does not match its sealed generation",
            ));
        }
        if generation.manifest().generation_id != *generation_id
            || generation.snapshot().content_identity.as_str() != entry.snapshot_content_identity
            || generation.manifest().seal.sealed_at.0 != entry.sealed_at_micros
            || entry.source_reference.as_ref().is_some_and(|reference| {
                generation
                    .snapshot()
                    .reference
                    .as_ref()
                    .map(tracedecay_domain::RefId::as_str)
                    != Some(reference.as_str())
            })
            || generation
                .snapshot()
                .source_revision
                .as_ref()
                .map(tracedecay_domain::CommitId::as_str)
                != entry.source_revision.as_deref()
        {
            return Err(Self::corruption(
                "durable code-generation index does not match its sealed generation",
            ));
        }
        Ok(Some(Arc::new(generation)))
    }

    /// Serve the active generation only when it is already decoded.
    ///
    /// Pure bookkeeping: it takes the cache lock for a pointer read and returns.
    /// It never claims a decode lease, never parks on the barrier, and never
    /// reads sealed bytes, so a caller that already has something servable can
    /// resolve freshness without being preempted by an in-flight O(store)
    /// decode. `None` means "not decoded here, yet" — it is an abstention, not
    /// evidence that no generation exists, and callers must never turn it into a
    /// fail-closed verdict on its own.
    fn active_already_decoded(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self.cache.lock_state()?.active.as_ref().map(Arc::clone))
    }

    /// Prove that an already-decoded serving handle still names the durable
    /// active publication without reading or decoding its sealed payload.
    fn active_pointer_matches_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<bool, CodeIndexPublicationStoreErrorV1> {
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(false);
        };
        Ok(
            pointer.generation_id == generation.manifest().generation_id.as_str()
                && pointer.snapshot_content_identity
                    == generation.snapshot().content_identity.as_str()
                && pointer.publication_digest
                    == generation.projection().publication_digest().as_str()
                && pointer.sealed_at_micros == generation.manifest().seal.sealed_at.0,
        )
    }

    /// Prove that the durable active publication sealed exactly the same
    /// source content as an already-decoded serving handle, without reading
    /// or decoding its sealed payload. A convergence or repair republication
    /// advances the pointer to a successor generation built from unchanged
    /// bytes; that successor supersedes the seat for publishers without
    /// staling it for readers.
    fn active_pointer_covers_snapshot_content(
        &self,
        content_identity: &ContentDigest,
    ) -> Result<bool, CodeIndexPublicationStoreErrorV1> {
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(false);
        };
        Ok(pointer.snapshot_content_identity == content_identity.as_str())
    }

    /// Occupy the active-generation decode barrier exactly as a cold activation
    /// does: the pinned slot is empty and one decode is in flight. Restores both
    /// on drop, so a parked reader is never stranded.
    #[cfg(test)]
    fn hold_active_decode(&self) -> HeldActiveDecodeV1 {
        let mut state = self
            .cache
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let restore = state.active.take();
        state.in_flight.push(DecodeSubjectV1::Active);
        drop(state);
        HeldActiveDecodeV1 {
            cache: Arc::clone(&self.cache),
            restore,
        }
    }

    /// Serve the active generation, decoding it at most once per publication.
    ///
    /// The first caller claims the decode lease and pays the O(store) sweep with
    /// no cache lock held; every caller that arrives while it runs parks on the
    /// condvar and is handed the same `Arc`. Nothing is memoized on failure, so
    /// a corrupt or unreadable store still errors every request.
    #[hotpath::measure(label = "daemon.code_index.generation.load_active")]
    fn load_active_shared(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let lease = loop {
            let mut state = self.cache.lock_state()?;
            if let Some(generation) = state.active.as_ref() {
                return Ok(Some(Arc::clone(generation)));
            }
            if state.is_in_flight(&DecodeSubjectV1::Active) {
                // Another caller already owns this O(store) decode. Park on it
                // rather than starting a second sweep over the same bytes.
                #[cfg(test)]
                self.cache.active_waiters.fetch_add(1, Ordering::AcqRel);
                #[cfg(feature = "hotpath")]
                let _waiting = GenerationDecodeWaitObservationV1::enter();
                let parked = hotpath::measure_block!(
                    "code_index.generation.decode.singleflight_wait",
                    self.cache
                        .ready
                        .wait(state)
                        .map_err(|_| DecodedGenerationCacheV1::poisoned())
                );
                #[cfg(test)]
                self.cache.active_waiters.fetch_sub(1, Ordering::AcqRel);
                let _parked = parked?;
                continue;
            }
            let epoch = state.active_epoch;
            state.in_flight.push(DecodeSubjectV1::Active);
            drop(state);
            break DecodeLeaseV1 {
                cache: &self.cache,
                subject: DecodeSubjectV1::Active,
                epoch,
            };
        };
        let decoded = self.decode_active_generation();
        if let Ok(Some(generation)) = decoded.as_ref() {
            let mut state = self.cache.lock_state()?;
            if state.active_epoch == lease.epoch {
                state.forget(&generation.manifest().generation_id);
                state.active = Some(Arc::clone(generation));
            } else if let Some(active) = state.active.as_ref() {
                // A publication landed while this decode ran. The newer active
                // generation wins; the superseded decode is never installed.
                let active = Arc::clone(active);
                drop(state);
                drop(lease);
                return Ok(Some(active));
            }
        }
        drop(lease);
        decoded
    }

    /// Read, verify, and decode the generation named by the durable active
    /// pointer. Never called with the decoded-generation cache lock held.
    #[hotpath::measure(label = "daemon.code_index.generation.decode")]
    fn decode_active_generation(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let lifetime_lock = self.acquire_generation_read_lock()?;
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        let path = self.generations_root.join(&pointer.generation_file);
        let metadata = path.symlink_metadata().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("active code-generation target is missing")
            } else {
                Self::unavailable(error)
            }
        })?;
        let active_entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == pointer.generation_id)
            .ok_or_else(|| {
                Self::corruption(
                    "durable code-generation index does not contain its active generation",
                )
            })?;
        if !metadata.file_type().is_file()
            || metadata.len() != active_entry.size_bytes
            || metadata.len() > MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        {
            return Err(Self::corruption(
                "active code-generation byte size does not match its durable entry",
            ));
        }
        #[cfg(feature = "hotpath")]
        let _decode = GenerationDecodeObservationV1::enter();
        let expected_digest =
            ManifestDigest::new(pointer.state_digest.clone()).map_err(|error| {
                Self::corruption(format!(
                    "active code-generation digest is not canonical: {error}"
                ))
            })?;
        let mut file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("active code-generation target is missing")
            } else {
                Self::unavailable(error)
            }
        })?;
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("code_index.generation.decode.bytes_total").inc(metadata.len());
        let decoded = hotpath::measure_block!(
            "code_index.generation.decode.file_read",
            self.decode_generation_file(
                &mut file,
                metadata.len(),
                &expected_digest,
                lifetime_lock,
            )
        );
        // A failing decode still swept the sealed bytes, and fail-closed
        // serving depends on that sweep re-running per request; count it
        // before propagating the error. Only the typed incompatible
        // abstention (`Ok(None)`) never was a real decode.
        if !matches!(decoded, Ok(None)) {
            self.cache.note_decode();
        }
        let Some(generation) = decoded.map_err(Self::corruption)? else {
            return Ok(None);
        };
        if generation.manifest().sanitizer_revision != self.expected_sanitizer_revision {
            return Ok(None);
        }
        if generation.manifest().generation_id.as_str() != pointer.generation_id
            || generation.snapshot().content_identity.as_str() != pointer.snapshot_content_identity
            || generation.projection().publication_digest().as_str() != pointer.publication_digest
            || generation.manifest().seal.sealed_at.0 != pointer.sealed_at_micros
        {
            return Err(Self::corruption(
                "active code-generation pointer does not match the sealed generation",
            ));
        }
        let encoded_bytes = metadata.len();
        self.active_encoded_bytes
            .store(encoded_bytes, Ordering::Release);
        hotpath::gauge!("daemon.code_index.generation.decode.bytes").set(encoded_bytes);
        Ok(Some(Arc::new(generation)))
    }

    fn active_encoded_bytes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.active_encoded_bytes)
    }

    /// Sealed-bytes decodes this process has performed against this store.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn sealed_decode_count(&self) -> u64 {
        self.cache.decode_count()
    }

    fn take_unpublished(&self) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        self.unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn restore_unpublished(&self, generation: Arc<CodeIndexPublishedGenerationV1>) {
        let mut candidate = self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if candidate.is_none() {
            *candidate = Some(generation);
        }
    }
}

impl CodeIndexAtomicPublicationPort for DaemonCodeIndexPublicationStoreV1 {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        if self.undecoded_active_expectation.is_some() {
            return Ok(None);
        }
        Ok(self
            .load_active_shared()?
            .map(|generation| generation.as_ref().clone()))
    }

    #[hotpath::measure(label = "code_index.generation.publish")]
    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        *self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&generation));
        if self
            .reconcile_publication_fence
            .as_ref()
            .is_some_and(|(_, control)| control.is_cancelled())
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        if self.undecoded_active_expectation.is_none() {
            // Cold hydration takes the store lock itself. Complete it before
            // entering the publication transaction, then recheck the exact
            // expected pointer under the writer lock below.
            let _ = self.load_active_shared()?;
        }
        let _store_lock =
            acquire_code_generation_store_lock(store_root).map_err(Self::unavailable)?;
        let prior_pointer = if let Some(expected) = self.undecoded_active_expectation.as_ref() {
            if expected_active_generation.is_some() {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            let pointer = self
                .read_publication_pointer()?
                .ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?;
            if !expected.matches(&pointer) {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Some(pointer)
        } else {
            self.read_publication_pointer()?
        };
        if self.undecoded_active_expectation.is_none()
            && prior_pointer
                .as_ref()
                .map(|pointer| pointer.generation_id.as_str())
                != expected_active_generation.map(CodeGenerationId::as_str)
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let state = self.cache.lock_state()?;
        let cached_active = state
            .active
            .as_ref()
            .map(|current| &current.manifest().generation_id);
        let cache_matches = self.undecoded_active_expectation.as_ref().map_or(
            cached_active == expected_active_generation,
            |expected| {
                cached_active
                    .is_none_or(|generation| generation.as_str() == expected.generation_id.as_str())
            },
        );
        if !cache_matches {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if self.disposition == CodeIndexPublicationDispositionV1::RetainedHistory
            && state.active.is_none()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if self.disposition == CodeIndexPublicationDispositionV1::RetainedHistory
            && state.active.as_ref().is_some_and(|active| {
                active.manifest().generation_id == generation.manifest().generation_id
            })
        {
            return Err(Self::unavailable(
                "retained history generation aliases the active generation identity",
            ));
        }
        // Encode and fsync without the decoded-generation cache lock so
        // readers are not parked across the durable write.
        drop(state);
        let parent_manifest_bytes = expected_active_generation
            .filter(|expected| generation.manifest().parent_generation.as_ref() == Some(*expected))
            .and_then(|expected| {
                prior_pointer.as_ref().and_then(|pointer| {
                    pointer
                        .generation_index
                        .iter()
                        .find(|entry| entry.generation_id == expected.as_str())
                })
            })
            .map(|entry| {
                Self::validate_generation_file(&entry.generation_file)?;
                let path = self.generations_root.join(&entry.generation_file);
                let metadata = path.symlink_metadata().map_err(Self::unavailable)?;
                if !metadata.file_type().is_file() || metadata.len() != entry.size_bytes {
                    return Err(Self::corruption(
                        "parent generation manifest identity is corrupt",
                    ));
                }
                let bytes = std::fs::read(path).map_err(Self::unavailable)?;
                if Self::state_digest(&bytes) != entry.state_digest {
                    return Err(Self::corruption(
                        "parent generation manifest digest does not verify",
                    ));
                }
                Ok(bytes)
            })
            .transpose()?;
        let temporary_path = self.generations_root.join(format!(
            ".generation-publication.{}.tmp",
            std::process::id()
        ));
        match temporary_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&temporary_path).map_err(Self::unavailable)?;
            }
            Ok(_) => {
                return Err(Self::unavailable(
                    "sealed code-generation temporary path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Self::unavailable(error)),
        }
        let mut temporary = TemporaryGenerationFileV1::new(temporary_path);
        let evidence_temporary_path = self.segments_root.join(format!(
            ".evidence-pack-publication.{}.tmp",
            std::process::id()
        ));
        let mut evidence_pack = TemporaryEvidencePackV1::create(evidence_temporary_path)?;
        let mut referenced_segment_bytes = 0_u64;
        self.seal_encoded_segment_bytes.store(0, Ordering::Relaxed);
        self.seal_existing_segment_bytes_read
            .store(0, Ordering::Relaxed);
        self.seal_evidence_page_count.store(0, Ordering::Relaxed);
        self.seal_evidence_durable_transaction_count
            .store(0, Ordering::Relaxed);
        let manifest_bytes = hotpath::measure_block!(
            "code_index.generation.publish.segment_encode",
            generation.encode_partitioned_sealed_with_parent(
                parent_manifest_bytes.as_deref(),
                |publication| {
                    match publication {
                        SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                            let segment_size = u64::try_from(bytes.len()).map_err(|_| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed segment length exceeds u64".to_owned(),
                                )
                            })?;
                            self.publish_segment_durable(digest, bytes)
                                .map_err(|error| {
                                    CodeIndexProductionErrorV1::Contract(error.to_string())
                                })?;
                            referenced_segment_bytes =
                                referenced_segment_bytes.saturating_add(segment_size);
                            self.seal_encoded_segment_bytes
                                .fetch_add(segment_size, Ordering::Relaxed);
                        }
                        SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                            page_ordinal,
                            page_digest,
                            bytes,
                        } => {
                            evidence_pack
                                .append_page(page_ordinal, page_digest, bytes)
                                .map_err(|error| {
                                    CodeIndexProductionErrorV1::Contract(error.to_string())
                                })?;
                            self.seal_evidence_page_count
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                            segment_digest,
                            segment_size_bytes,
                            page_count,
                        } => {
                            if evidence_pack
                                .commit(
                                    &self.segments_root,
                                    segment_digest,
                                    segment_size_bytes,
                                    page_count,
                                )
                                .map_err(|error| {
                                    CodeIndexProductionErrorV1::Contract(error.to_string())
                                })?
                            {
                                self.seal_evidence_durable_transaction_count
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            referenced_segment_bytes =
                                referenced_segment_bytes.saturating_add(segment_size_bytes);
                        }
                    }
                    Ok(())
                },
            )
        );
        let manifest_bytes = match manifest_bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                evidence_pack.rollback_unattached(&self.segments_root)?;
                return Err(Self::unavailable(error));
            }
        };
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("code_index.generation.publish.encoded_file_segment_bytes")
                .set(self.seal_encoded_segment_bytes.load(Ordering::Relaxed));
            hotpath::gauge!("code_index.generation.publish.existing_file_segment_bytes_read").set(
                self.seal_existing_segment_bytes_read
                    .load(Ordering::Relaxed),
            );
            hotpath::gauge!("code_index.generation.publish.evidence_pages")
                .set(self.seal_evidence_page_count.load(Ordering::Relaxed));
            hotpath::gauge!("code_index.generation.publish.evidence_durable_transactions").set(
                self.seal_evidence_durable_transaction_count
                    .load(Ordering::Relaxed),
            );
        }
        let manifest_publication = (|| {
            hotpath::measure_block!("code_index.generation.publish.seal_fsync", {
                Self::write_durable(&temporary.path, &manifest_bytes)?;
                Ok::<(), CodeIndexPublicationStoreErrorV1>(())
            })?;
            let generation_size = u64::try_from(manifest_bytes.len()).map_err(Self::unavailable)?;
            if generation_size > MAX_DURABLE_GENERATION_INDEX_BYTES_V1 {
                return Err(Self::unavailable(
                    "sealed code generation exceeds the durable history byte bound",
                ));
            }
            let state_digest = hotpath::measure_block!(
                "code_index.generation.publish.state_digest",
                Self::state_digest_file(&temporary.path)
            )?;
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("code_index.generation.publish.digest_bytes")
                .set(generation_size.saturating_add(referenced_segment_bytes));
            let generation_file = format!(
                "generation-{}.json",
                state_digest
                    .strip_prefix("sha256:")
                    .unwrap_or(&state_digest)
            );
            let generation_path = self.generations_root.join(&generation_file);
            match generation_path.symlink_metadata() {
                Ok(_) => {
                    let equal = hotpath::measure_block!(
                        "code_index.generation.publish.dedupe_compare",
                        Self::files_equal(&generation_path, &temporary.path)
                    )?;
                    if !equal {
                        return Err(Self::unavailable(
                            "immutable code-generation path contains different bytes",
                        ));
                    }
                    std::fs::remove_file(&temporary.path).map_err(Self::unavailable)?;
                    temporary.commit();
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::rename(&temporary.path, &generation_path)
                        .map_err(Self::unavailable)?;
                    temporary.path = generation_path;
                    Self::sync_directory(&self.generations_root)?;
                    temporary.commit();
                }
                Err(error) => return Err(Self::unavailable(error)),
            }
            Ok((generation_size, state_digest, generation_file))
        })();
        let (generation_size, state_digest, generation_file) = match manifest_publication {
            Ok(published) => published,
            Err(error) => {
                let generation_rollback = temporary.rollback_uncommitted(&self.generations_root);
                let evidence_rollback = evidence_pack.rollback_unattached(&self.segments_root);
                generation_rollback?;
                evidence_rollback?;
                return Err(error);
            }
        };
        evidence_pack.attach_to_manifest();

        let exact_git_evidence = self.exact_git_evidence(&generation)?;
        let mut generation_index = prior_pointer
            .as_ref()
            .map(|pointer| pointer.generation_index.clone())
            .unwrap_or_default();
        generation_index.retain(|entry| {
            entry.generation_id != generation.manifest().generation_id.as_str()
                && exact_git_evidence
                    .as_ref()
                    .is_none_or(|(reference, revision, tree)| {
                        entry.source_reference.as_ref() != Some(reference)
                            || entry.source_revision.as_ref() != Some(revision)
                            || entry.source_tree.as_ref() != Some(tree)
                    })
        });
        generation_index.push(DurableGenerationIndexEntryV1 {
            generation_id: generation.manifest().generation_id.as_str().to_owned(),
            snapshot_content_identity: generation.snapshot().content_identity.as_str().to_owned(),
            sealed_at_micros: generation.manifest().seal.sealed_at.0,
            size_bytes: generation_size,
            segment_bytes: referenced_segment_bytes,
            generation_file: generation_file.clone(),
            state_digest: state_digest.clone(),
            source_reference: exact_git_evidence
                .as_ref()
                .map(|(reference, _, _)| reference.clone()),
            source_revision: exact_git_evidence
                .as_ref()
                .map(|(_, revision, _)| revision.clone()),
            source_tree: exact_git_evidence.map(|(_, _, tree)| tree),
            cardinality: Some(Self::generation_cardinality(&generation)?),
            text_artifact: None,
        });
        generation_index.sort_by(|left, right| {
            (left.sealed_at_micros, left.generation_id.as_str())
                .cmp(&(right.sealed_at_micros, right.generation_id.as_str()))
        });
        let retained_active_generation = match self.disposition {
            CodeIndexPublicationDispositionV1::Active => {
                generation.manifest().generation_id.as_str()
            }
            CodeIndexPublicationDispositionV1::RetainedHistory => prior_pointer
                .as_ref()
                .map(|pointer| pointer.generation_id.as_str())
                .ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?,
        };
        let removed =
            retain_bounded_generation_index(&mut generation_index, retained_active_generation);
        let generation_index_truncated = prior_pointer
            .as_ref()
            .is_some_and(|pointer| pointer.generation_index_truncated)
            || removed > 0;
        let generation_index_digest =
            Self::generation_index_digest(&generation_index, generation_index_truncated)?;
        let mut pointer = match self.disposition {
            CodeIndexPublicationDispositionV1::Active => DurablePublicationPointerV1 {
                generation_id: generation.manifest().generation_id.as_str().to_owned(),
                snapshot_content_identity: generation
                    .snapshot()
                    .content_identity
                    .as_str()
                    .to_owned(),
                publication_digest: generation
                    .projection()
                    .publication_digest()
                    .as_str()
                    .to_owned(),
                sealed_at_micros: generation.manifest().seal.sealed_at.0,
                generation_file,
                state_digest,
                generation_index: Vec::new(),
                generation_index_truncated: false,
                generation_index_digest: None,
            },
            CodeIndexPublicationDispositionV1::RetainedHistory => {
                prior_pointer.ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?
            }
        };
        pointer.generation_index = generation_index;
        pointer.generation_index_truncated = generation_index_truncated;
        pointer.generation_index_digest = Some(generation_index_digest);
        let bytes = serde_json::to_vec(&pointer).map_err(|error| {
            CodeIndexPublicationStoreErrorV1::Unavailable(format!(
                "publication pointer serialization failed: {error}"
            ))
        })?;
        // Serialize the final visibility boundary with observed-change wakes.
        // Immutable segment writes may finish before this point, but a stale
        // capture cannot replace the active durable pointer.
        let source_fence = if let Some((hints, control)) = self.reconcile_publication_fence.as_ref()
        {
            let guard = hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if control.is_cancelled() {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Some(guard)
        } else {
            None
        };
        let temporary = self
            .active_path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        if temporary.exists() {
            std::fs::remove_file(&temporary).map_err(Self::unavailable)?;
        }
        hotpath::measure_block!("code_index.generation.publish.pointer_commit", {
            Self::write_durable(&temporary, &bytes)?;
            std::fs::rename(&temporary, &self.active_path).map_err(Self::unavailable)?;
            Self::sync_directory(
                self.active_path
                    .parent()
                    .ok_or_else(|| Self::unavailable("active pointer has no parent directory"))?,
            )?;
            self.remember_publication_pointer(&pointer, &bytes);
            Ok::<(), CodeIndexPublicationStoreErrorV1>(())
        })?;
        drop(source_fence);
        let mut state = self.cache.lock_state()?;
        if self.undecoded_active_expectation.is_none() {
            let cached_active = state
                .active
                .as_ref()
                .map(|current| &current.manifest().generation_id);
            if cached_active != expected_active_generation {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
        }
        let generation_id = generation.manifest().generation_id.clone();
        state.forget(&generation_id);
        match self.disposition {
            CodeIndexPublicationDispositionV1::Active => {
                // The published generation is already decoded and validated in
                // memory. Bumping the epoch retires any decode that started
                // against the prior pointer so it cannot install over this one.
                state.active_epoch = state.active_epoch.wrapping_add(1);
                // Graph-off undecoded rebuild already holds the sealed
                // generation. Clearing `state.active` forced the next pass to
                // cold-decode the whole store, so a retry after a transient
                // publication failure never published the successor before its
                // deadline. Install the built generation in both dispositions.
                self.active_encoded_bytes
                    .store(generation_size, Ordering::Release);
                state.active = Some(generation);
            }
            CodeIndexPublicationDispositionV1::RetainedHistory => {
                state.decoded.push_back(generation);
                while state.decoded.len() > DECODED_GENERATION_CACHE_CAPACITY {
                    state.decoded.pop_front();
                }
            }
        }
        *self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

#[derive(Default)]
struct DaemonProjectionSinkV1;

impl CodeChunkProjectionSink for DaemonProjectionSinkV1 {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect::<Vec<_>>();
        decisions.extend(
            request
                .changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }),
        );
        decisions.extend(
            request
                .changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        decisions.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

#[derive(Default)]
struct PendingHintsV1 {
    paths: BTreeSet<PathBuf>,
    overflow: bool,
    observed_source_change: bool,
}

impl PendingHintsV1 {
    fn count(&self) -> Option<u64> {
        (!self.overflow).then(|| u64::try_from(self.paths.len()).unwrap_or(u64::MAX))
    }

    fn path(&mut self, path: PathBuf) {
        if self.paths.len() >= MAX_PENDING_HINTS {
            self.paths.clear();
            self.overflow = true;
        } else {
            self.paths.insert(path);
        }
    }

    fn overflow(&mut self) {
        self.paths.clear();
        self.overflow = true;
    }

    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    fn restore(&mut self, pending: Self) {
        self.observed_source_change |= pending.observed_source_change;
        if self.overflow {
            return;
        }
        if pending.overflow {
            self.overflow();
            return;
        }
        for path in pending.paths {
            self.path(path);
            if self.overflow {
                break;
            }
        }
    }
}

/// A drained view of the canonical pending-hint authority. Until committed,
/// every early return, typed failure, cancellation, or unwind merges the exact
/// drained paths and observed-change marker back with hints that arrived
/// during the reconcile pass.
struct DrainedPendingHintsV1 {
    authority: Arc<Mutex<PendingHintsV1>>,
    pending: Option<PendingHintsV1>,
}

struct RetainedReconcileCaptureV1 {
    captured: CapturedSnapshotV1,
    drained_hints: DrainedPendingHintsV1,
    control: DaemonCodeIndexControlV1,
    git_metadata: identity::GitMetadataFingerprintV1,
    stat_signature: Option<String>,
}

impl DrainedPendingHintsV1 {
    fn new(authority: Arc<Mutex<PendingHintsV1>>, pending: PendingHintsV1) -> Self {
        Self {
            authority,
            pending: Some(pending),
        }
    }

    fn overflow(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.overflow)
    }

    fn commit(mut self) {
        self.pending = None;
    }
}

impl Drop for DrainedPendingHintsV1 {
    fn drop(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .restore(pending);
    }
}

/// One candidate path's capture result, produced independently per file so
/// the read/sanitize/digest sweep can run at machine width.
struct CapturedCandidateV1 {
    file: SanitizedCodeFileV1,
    captured: CodeIndexCapturedFileV1,
    receipt_id: SanitizationReceiptId,
    retained: Arc<[u8]>,
    /// Charges the canonical source allocation before the candidate can join a
    /// snapshot. Production borrows this allocation until its bounded intake
    /// materialization, rather than retaining a second snapshot-wide copy.
    retained_reservation: Option<ResidentMemoryReservationV1>,
}

struct CapturedSnapshotV1 {
    snapshot: SanitizedCodeSnapshotV1,
    repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    captured_files: Vec<CodeIndexCapturedFileV1>,
    changed_paths: BTreeSet<String>,
    /// Strong references to this snapshot's interned bytes. The shared byte
    /// pool holds only weak entries; the scheduler retains its current
    /// snapshot's bytes so identical content in sibling worktrees can reuse
    /// them (physical sharing without identity aliasing).
    retained_bytes: Vec<Arc<[u8]>>,
    /// Resident charges for `retained_bytes`. Empty sources need no nonzero
    /// reservation.
    retained_reservations: Vec<ResidentMemoryReservationV1>,
}

#[derive(Clone, Debug)]
pub struct CodeIndexPublishEvidenceV1 {
    pub generation_id: CodeGenerationId,
    pub repository_id: RepositoryId,
    pub snapshot_content_identity: ContentDigest,
    /// Publication receipt evidence: asserted by determinism tests, not read
    /// on any production path.
    pub lane_digest: ManifestDigest,
    /// Publication receipt evidence: asserted by determinism tests, not read
    /// on any production path.
    pub file_occurrence_ids: Vec<FileOccurrenceId>,
    pub reextracted_files: usize,
    pub changed_chunks: usize,
    pub reused_chunks: usize,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub struct CodeIndexNoopEvidenceV1 {
    pub snapshot_content_identity: ContentDigest,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub enum CodeIndexReconcileOutcomeV1 {
    Published(CodeIndexPublishEvidenceV1),
    Noop(CodeIndexNoopEvidenceV1),
}

/// The lazily built serving caches shared by every handle bound to one sealed
/// generation: the exact/lexical/graph lane owners, the record lookup index,
/// and the retained interactive graph store. All are rebuilt only when a new
/// generation is loaded.
type GenerationServingCachesV1 = (
    CodeGenerationId,
    Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    Arc<OnceLock<queries::GenerationRecordIndexV1>>,
    Arc<CodeTextProjectionStateV1>,
    Arc<AtomicBool>,
    GenerationTextControlV1,
    Arc<ProfiledStdMutex<CodeIndexBuildProgressStateV1>>,
    u64,
    Arc<RwLock<CodeGraphActivationStateV1>>,
);

pub type CodeIndexBuildProgressSlotV1 = Arc<RwLock<CodeIndexBuildProgressSlotStateV1>>;

/// Cancellation authority for derivations of one immutable sealed generation.
///
/// Worktree freshness epochs deliberately do not participate: a hook wake can
/// make the source worktree newer, but it cannot invalidate bytes already
/// sealed under a content-addressed generation. Only daemon shutdown, owner
/// retirement, or replacement by another serving generation retires this
/// control.
#[derive(Clone)]
struct GenerationTextControlV1 {
    execution: DaemonCodeIndexControlV1,
    retirement_epoch: Arc<AtomicU64>,
    #[cfg(feature = "hotpath")]
    shutting_down: Arc<AtomicBool>,
}

#[cfg(feature = "hotpath")]
#[derive(Clone, Copy)]
enum GenerationTextCancellationSourceV1 {
    Shutdown,
    Superseded,
}

impl GenerationTextControlV1 {
    fn new(shutting_down: Arc<AtomicBool>) -> Self {
        let retirement_epoch = Arc::new(AtomicU64::new(0));
        let execution = DaemonCodeIndexControlV1::new(
            Arc::clone(&retirement_epoch),
            Arc::clone(&shutting_down),
        );
        Self {
            execution,
            retirement_epoch,
            #[cfg(feature = "hotpath")]
            shutting_down,
        }
    }

    fn retire(&self) {
        DaemonCodeIndexControlV1::advance(&self.retirement_epoch);
    }

    #[cfg(feature = "hotpath")]
    fn cancellation_source(&self) -> Option<GenerationTextCancellationSourceV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            Some(GenerationTextCancellationSourceV1::Shutdown)
        } else if self.execution.is_cancelled() {
            Some(GenerationTextCancellationSourceV1::Superseded)
        } else {
            None
        }
    }
}

impl CodeIndexExecutionControlV1 for GenerationTextControlV1 {
    fn is_cancelled(&self) -> bool {
        self.execution.is_cancelled()
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.execution.is_deadline_exceeded()
    }
}

#[derive(Default)]
pub struct CodeIndexBuildProgressSlotStateV1 {
    generation_id: Option<CodeGenerationId>,
    owner_epoch: u64,
    progress_epoch: u64,
    snapshot: Option<Arc<CodeIndexBuildProgressV1>>,
}

impl CodeIndexBuildProgressSlotStateV1 {
    fn replace_generation(&mut self, generation_id: CodeGenerationId) -> u64 {
        self.owner_epoch = self.owner_epoch.saturating_add(1).max(1);
        self.progress_epoch = self.progress_epoch.saturating_add(1).max(1);
        self.generation_id = Some(generation_id);
        self.snapshot = None;
        self.owner_epoch
    }

    fn publish(
        &mut self,
        generation_id: &CodeGenerationId,
        owner_epoch: u64,
        mut snapshot: CodeIndexBuildProgressV1,
    ) -> bool {
        if self.generation_id.as_ref() != Some(generation_id) || self.owner_epoch != owner_epoch {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("query.artifact.progress.rejected_stale_total").inc(1u64);
            return false;
        }
        self.progress_epoch = self.progress_epoch.saturating_add(1).max(1);
        snapshot.progress_epoch = self.progress_epoch;
        #[cfg(feature = "hotpath")]
        let published_phase = snapshot.phase;
        self.snapshot = Some(Arc::new(snapshot));
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("query.artifact.progress.publication_total").inc(1u64);
            match published_phase {
                CodeIndexBuildPhaseV1::SourceScan => {
                    hotpath::gauge!("query.artifact.progress.phase.source_scan_total").inc(1u64);
                }
                CodeIndexBuildPhaseV1::RelationalPreparation => {
                    hotpath::gauge!("query.artifact.progress.phase.preparation_total").inc(1u64);
                }
                CodeIndexBuildPhaseV1::BulkCommit => {
                    hotpath::gauge!("query.artifact.progress.phase.bulk_commit_total").inc(1u64);
                }
                CodeIndexBuildPhaseV1::IndexBuild => {
                    hotpath::gauge!("query.artifact.progress.phase.index_build_total").inc(1u64);
                }
                CodeIndexBuildPhaseV1::Verification => {
                    hotpath::gauge!("query.artifact.progress.phase.verification_total").inc(1u64);
                }
                CodeIndexBuildPhaseV1::Ready => {
                    hotpath::gauge!("query.artifact.progress.phase.ready_total").inc(1u64);
                }
            }
        }
        true
    }

    pub fn snapshot(&self) -> Option<Arc<CodeIndexBuildProgressV1>> {
        self.snapshot.as_ref().map(Arc::clone)
    }
}

/// Publishes an observational scan sample without delaying sealed-byte authentication.
/// Generation ownership and durable phase transitions continue to use blocking writes.
fn try_publish_build_progress(
    slot: &CodeIndexBuildProgressSlotV1,
    generation_id: &CodeGenerationId,
    owner_epoch: u64,
    snapshot: CodeIndexBuildProgressV1,
) -> bool {
    match slot.try_write() {
        Ok(mut slot) => slot.publish(generation_id, owner_epoch, snapshot),
        Err(std::sync::TryLockError::WouldBlock) => {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("query.artifact.progress.skipped_busy_total").inc(1u64);
            false
        }
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            poisoned
                .into_inner()
                .publish(generation_id, owner_epoch, snapshot)
        }
    }
}

#[derive(Clone, Copy)]
struct CodeIndexCommittedProgressSampleV1 {
    observed_at: Instant,
    completed_files: u64,
    completed_lexical_bytes: u64,
}

struct CodeIndexBuildProgressStateV1 {
    started_at: Instant,
    committed_samples: VecDeque<CodeIndexCommittedProgressSampleV1>,
}

impl CodeIndexBuildProgressStateV1 {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            committed_samples: VecDeque::with_capacity(2),
        }
    }

    fn observe_committed(&mut self, sample: CodeIndexCommittedProgressSampleV1) {
        if self.committed_samples.back().is_some_and(|previous| {
            previous.completed_files == sample.completed_files
                && previous.completed_lexical_bytes == sample.completed_lexical_bytes
        }) {
            return;
        }
        if self.committed_samples.len() == 2 {
            self.committed_samples.pop_front();
        }
        self.committed_samples.push_back(sample);
    }

    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn rates_and_eta(&self, total_lexical_bytes: u64) -> (Option<f64>, Option<f64>, Option<u64>) {
        let Some(previous) = self.committed_samples.front() else {
            return (None, None, None);
        };
        let Some(current) = self.committed_samples.back() else {
            return (None, None, None);
        };
        if self.committed_samples.len() < 2 || current.observed_at <= previous.observed_at {
            return (None, None, None);
        }
        let elapsed_seconds = current
            .observed_at
            .duration_since(previous.observed_at)
            .as_secs_f64();
        if elapsed_seconds <= 0.0 {
            return (None, None, None);
        }
        let files_per_second = current
            .completed_files
            .checked_sub(previous.completed_files)
            .filter(|delta| *delta > 0)
            .map(|delta| delta as f64 / elapsed_seconds);
        let lexical_bytes_per_second = current
            .completed_lexical_bytes
            .checked_sub(previous.completed_lexical_bytes)
            .filter(|delta| *delta > 0)
            .map(|delta| delta as f64 / elapsed_seconds);
        let estimated_remaining_seconds = lexical_bytes_per_second.and_then(|lexical_rate| {
            let remaining = total_lexical_bytes.saturating_sub(current.completed_lexical_bytes);
            let estimate = (remaining as f64 / lexical_rate).ceil();
            (estimate.is_finite() && estimate >= 0.0 && estimate <= u64::MAX as f64)
                .then_some(estimate as u64)
        });
        (
            files_per_second,
            lexical_bytes_per_second,
            estimated_remaining_seconds,
        )
    }
}

#[derive(Clone)]
pub struct LatestCompleteCodeIndexV1 {
    generation: Arc<CodeIndexPublishedGenerationV1>,
    text: LatestCodeTextGenerationV1,
    record_index: Arc<OnceLock<queries::GenerationRecordIndexV1>>,
}

#[derive(Clone)]
pub struct LatestCodeTextGenerationV1 {
    metadata: Arc<VerifiedSealedTextGenerationMetadataV1>,
    sealed_format_revision: u32,
    query_owners: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    /// Native-graph readiness for this exact sealed text generation. Status
    /// reads this authority even while an older generation still owns the
    /// graph-serving slot, so old Ready state cannot mask current Pending or
    /// terminal Unavailable state.
    graph_activation: Arc<RwLock<CodeGraphActivationStateV1>>,
    /// Generation-owned singleflight state for the durable text projection:
    /// the resumable partial build plus the head-open claim. Only the
    /// background scheduler advances it; foreground queries observe typed
    /// warming until the immutable owners are installed.
    text_projection_build: Arc<CodeTextProjectionStateV1>,
    text_projection_failed: Arc<AtomicBool>,
    text_control: GenerationTextControlV1,
    text_progress_state: Arc<ProfiledStdMutex<CodeIndexBuildProgressStateV1>>,
    text_progress_slot: CodeIndexBuildProgressSlotV1,
    text_progress_owner_epoch: u64,
    /// Durable daemon-authority epoch, shared by all scheduler owners created
    /// during one daemon invocation.
    text_progress_daemon_incarnation: u64,
    /// Registry-minted scheduler-owner epoch. It orders progress across
    /// scheduler owner replacements within one daemon incarnation.
    text_progress_producer_incarnation: u64,
    /// The durable text-artifact store for this generation's store root.
    text_artifact_store: DaemonCodeTextArtifactStoreV1,
    /// A cold graph-off bind authenticates the sealed source once and hands
    /// that same reader to the artifact build. The full generation is never
    /// decoded merely to discover text metadata or source layout.
    preopened_source: Arc<ProfiledStdMutex<Option<VerifiedSealedLexicalPageSourceV1<File>>>>,
    publication_binding: Option<Arc<DurableActiveSealedGenerationBindingV1>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DurableActiveSealedGenerationBindingV1 {
    generation_id: CodeGenerationId,
    generation_file: String,
    state_digest: ManifestDigest,
}

impl DurableActiveSealedGenerationBindingV1 {
    fn matches(&self, pointer: Option<&DurablePublicationPointerV1>) -> bool {
        pointer.is_some_and(|pointer| {
            pointer.generation_id == self.generation_id.as_str()
                && pointer.generation_file == self.generation_file
                && pointer.state_digest == self.state_digest.as_str()
        })
    }
}

impl std::ops::Deref for LatestCompleteCodeIndexV1 {
    type Target = LatestCodeTextGenerationV1;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationCodeSnapshotV1 {
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub snapshot_digest: ManifestDigest,
    /// The sealed capability authority that calibrates the live semantic
    /// evaluation target; it is not inferred from an accepted profile.
    pub capability_manifest_digest: ManifestDigest,
}

/// Production exact/lexical owners bound to one immutable published generation.
#[derive(Clone)]
pub struct ProductionCodeIndexQueryOwnersV1 {
    exact: ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactLexicalArtifactReaderV1<CentralExactAdmissionAuthorityV1>,
    >,
    lexical: LexicalLane<CodeLexicalArtifactReaderV1>,
    hydration: CodeLexicalArtifactReaderV1,
    /// Holds the complete advertised reader ceiling in the process resident-
    /// memory authority while these owners serve.
    _reader_reservation: Arc<ResidentMemoryReservationV1>,
}

impl ProductionCodeIndexQueryOwnersV1 {
    pub fn retrieve_exact(
        &self,
        request: &ExactLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.exact.retrieve_exact(request)
    }

    fn artifact(
        exact: ExactLane<
            CentralExactAdmissionAuthorityV1,
            CodeExactLexicalArtifactReaderV1<CentralExactAdmissionAuthorityV1>,
        >,
        lexical: LexicalLane<CodeLexicalArtifactReaderV1>,
        hydration: CodeLexicalArtifactReaderV1,
        reader_reservation: ResidentMemoryReservationV1,
    ) -> Self {
        Self {
            exact,
            lexical,
            hydration,
            _reader_reservation: Arc::new(reader_reservation),
        }
    }

    fn occurrence_by_binding(
        &self,
        binding: &tracedecay_query::retrieval::ports::CodeCandidateBindingV1,
    ) -> Result<
        tracedecay_query::retrieval::NativeCodeOccurrenceV1,
        tracedecay_query::retrieval::QueryExecutionContractErrorV1,
    > {
        self.hydration
            .occurrence_by_binding(binding)
            .map_err(|_| {
                tracedecay_query::retrieval::QueryExecutionContractErrorV1::RecordUnavailable
            })?
            .map(
                |occurrence| tracedecay_query::retrieval::NativeCodeOccurrenceV1 {
                    file: occurrence.file,
                    symbol: occurrence.symbol,
                    chunk: Some(occurrence.chunk),
                    path: occurrence.logical_path,
                    span: occurrence.source_span,
                },
            )
            .ok_or(tracedecay_query::retrieval::QueryExecutionContractErrorV1::RecordUnavailable)
    }

    fn occurrence_by_chunk(
        &self,
        chunk: &tracedecay_domain::CodeSearchChunkId,
    ) -> Result<
        tracedecay_query::retrieval::NativeCodeOccurrenceV1,
        tracedecay_query::retrieval::QueryExecutionContractErrorV1,
    > {
        self.hydration
            .occurrence_by_chunk(chunk)
            .map_err(|_| {
                tracedecay_query::retrieval::QueryExecutionContractErrorV1::RecordUnavailable
            })?
            .map(
                |occurrence| tracedecay_query::retrieval::NativeCodeOccurrenceV1 {
                    file: occurrence.file,
                    symbol: occurrence.symbol,
                    chunk: Some(occurrence.chunk),
                    path: occurrence.logical_path,
                    span: occurrence.source_span,
                },
            )
            .ok_or(tracedecay_query::retrieval::QueryExecutionContractErrorV1::RecordUnavailable)
    }

    fn artifact_occurrence_by_chunk(
        &self,
        chunk: &tracedecay_domain::CodeSearchChunkId,
    ) -> Result<CodeLexicalArtifactOccurrenceV1, RetrievalPortError> {
        self.hydration
            .occurrence_by_chunk(chunk)
            .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))?
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "lexical artifact row is unavailable".to_owned(),
                )
            })
    }

    pub fn retrieve_lexical(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        self.lexical.retrieve_lexical(request)
    }

    #[cfg(test)]
    pub fn is_artifact_backed(&self) -> bool {
        true
    }
}

/// Partial durable-artifact build: the staging `SQLite` builder plus the
/// verified page source over this generation's durable sealed file.
struct CodeTextArtifactBuildV1 {
    builder: CodeLexicalArtifactBuilderV1,
    source: VerifiedSealedLexicalPageSourceV1<File>,
    sealed_identity: DurableSealedCodeGenerationIdentityV1,
    source_receipt: Option<VerifiedSealedLexicalSourceReceiptV1>,
    staging_path: PathBuf,
    /// Holds the builder's advertised memory ceiling reserved in the
    /// process resident-memory authority for the lifetime of the build.
    _build_reservation: ResidentMemoryReservationV1,
}

/// Singleflight authority for one generation's durable text projection.
///
/// The slot is the generation-owned partial-state authority; the condvar
/// wakes arrivals parked behind a `HeadOpening` claim. A corpus-sized
/// verified open (the published-head reopen or the publication tail's
/// reopen — two full SHA-256 passes plus `SQLite` verification each) runs
/// with the slot lock released, so a concurrent wake parks with typed
/// cancellation instead of blocking on the mutex for the whole open. This
/// stays a plain `std::sync::Mutex` rather than `hotpath::mutex!` because
/// `Condvar::wait_timeout` requires the exact std guard type; lock-wait and
/// parked wait are measured with explicit spans instead.
struct CodeTextProjectionStateV1 {
    slot: Mutex<CodeTextProjectionSlotV1>,
    ready: Condvar,
}

enum CodeTextProjectionSlotV1 {
    /// No partial build exists and no wake owns a long open: the next wake
    /// claims the work.
    Idle,
    /// One wake owns a corpus-sized verified open with the slot lock
    /// released. Concurrent wakes park on the condvar until the claim is
    /// resolved.
    HeadOpening,
    /// The resumable staging build; each wake advances one bounded slice
    /// under the slot lock.
    Building(Box<CodeTextArtifactBuildV1>),
}

impl CodeTextProjectionStateV1 {
    fn new() -> Self {
        Self {
            slot: Mutex::new(CodeTextProjectionSlotV1::Idle),
            ready: Condvar::new(),
        }
    }

    fn lock_slot(&self) -> MutexGuard<'_, CodeTextProjectionSlotV1> {
        hotpath::measure_block!("query.artifact.head_open.lock_wait", {
            self.slot.lock().unwrap_or_else(PoisonError::into_inner)
        })
    }
}

/// One wake's exclusive claim on a corpus-sized verified head open.
///
/// Restores the slot to `Idle` and wakes every parked arrival on all exit
/// paths — success, typed failure, and unwind — so a failed open can never
/// strand concurrent wakes behind a stale `HeadOpening` marker.
struct TextHeadOpenClaimV1<'a> {
    state: &'a CodeTextProjectionStateV1,
    armed: bool,
}

impl<'a> TextHeadOpenClaimV1<'a> {
    /// The caller must already have transitioned the slot to `HeadOpening`
    /// and released the lock; this guard owns restoring it.
    fn new(state: &'a CodeTextProjectionStateV1) -> Self {
        Self { state, armed: true }
    }

    /// Install the initialized staging build and hand the locked slot back
    /// to the claiming wake so it advances the first bounded slice
    /// immediately.
    fn install_build(
        &mut self,
        build: Box<CodeTextArtifactBuildV1>,
    ) -> MutexGuard<'a, CodeTextProjectionSlotV1> {
        self.armed = false;
        let mut slot = self.state.lock_slot();
        *slot = CodeTextProjectionSlotV1::Building(build);
        self.state.ready.notify_all();
        slot
    }
}

impl Drop for TextHeadOpenClaimV1<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut slot = self.state.lock_slot();
        if matches!(&*slot, CodeTextProjectionSlotV1::HeadOpening) {
            *slot = CodeTextProjectionSlotV1::Idle;
        }
        drop(slot);
        self.state.ready.notify_all();
    }
}

/// Result of one claimed head-open pass performed with the slot lock
/// released.
enum TextHeadOpenOutcomeV1 {
    /// The published durable head reopened and verified; the immutable query
    /// owners are installed.
    Served,
    /// No published head was servable; the resumable staging build begins
    /// (or resumes) from its durable staging file.
    Build(Box<CodeTextArtifactBuildV1>),
}

fn map_text_artifact_error(error: CodeLexicalArtifactErrorV1) -> RetrievalPortError {
    match error {
        CodeLexicalArtifactErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
        ) => RetrievalPortError::Cancelled,
        CodeLexicalArtifactErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::DeadlineExceeded,
        ) => RetrievalPortError::BudgetExceeded,
        CodeLexicalArtifactErrorV1::Incompatible(_) => RetrievalPortError::IncompatibleProjection,
        CodeLexicalArtifactErrorV1::Contract(detail) => RetrievalPortError::Contract(detail),
        CodeLexicalArtifactErrorV1::Corrupt(detail) => RetrievalPortError::Contract(detail),
        CodeLexicalArtifactErrorV1::Unreserved(_)
        | CodeLexicalArtifactErrorV1::BatchTooLarge { .. } => RetrievalPortError::BudgetExceeded,
        CodeLexicalArtifactErrorV1::Io(detail) | CodeLexicalArtifactErrorV1::Missing(detail) => {
            RetrievalPortError::AuthorityUnavailable(detail)
        }
    }
}

fn map_sealed_page_source_error(error: CodeIndexProductionErrorV1) -> RetrievalPortError {
    match error {
        CodeIndexProductionErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
        ) => RetrievalPortError::Cancelled,
        CodeIndexProductionErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::DeadlineExceeded,
        ) => RetrievalPortError::BudgetExceeded,
        CodeIndexProductionErrorV1::Contract(detail) => RetrievalPortError::Contract(detail),
        error => RetrievalPortError::AuthorityUnavailable(error.to_string()),
    }
}

fn text_artifact_unavailable(error: impl std::fmt::Display) -> RetrievalPortError {
    RetrievalPortError::AuthorityUnavailable(error.to_string())
}

/// Durable text-artifact store bound to one worktree's generation store root.
///
/// Publishes finalized staging artifacts under `code-text-artifacts-v1/` and
/// attaches them to the sealed generation's durable index entry, so a restart
/// reopens the artifact head instead of rebuilding it.
#[derive(Clone)]
pub struct DaemonCodeTextArtifactStoreV1 {
    store_root: PathBuf,
    publication: DaemonCodeIndexPublicationStoreV1,
    /// Process-wide resident-memory authority: the artifact build and reader
    /// ceilings are reserved here before they are allocated, so the
    /// advertised budgets are admission-controlled, not just documented.
    resident_memory: Arc<ProcessResidentMemoryV1>,
    project_id: ProjectId,
    worktree_id: WorktreeId,
}

impl DaemonCodeTextArtifactStoreV1 {
    fn bind(
        store_root: &Path,
        publication: &DaemonCodeIndexPublicationStoreV1,
        resident_memory: &Arc<ProcessResidentMemoryV1>,
        project_id: &ProjectId,
        worktree_id: &WorktreeId,
    ) -> Self {
        Self {
            store_root: store_root.to_path_buf(),
            publication: publication.clone(),
            resident_memory: Arc::clone(resident_memory),
            project_id: project_id.clone(),
            worktree_id: worktree_id.clone(),
        }
    }

    fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// Reserve one artifact memory ceiling through the process authority.
    /// A denial (after one bounded reclaim pass) is a typed unavailability,
    /// never a silent unreserved allocation.
    fn reserve_resident_memory(
        &self,
        generation_id: &CodeGenerationId,
        component: &'static str,
        bytes: usize,
    ) -> Result<ResidentMemoryReservationV1, RetrievalPortError> {
        let component = ResidentMemoryComponentIdV1::new(component)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let requested = u64::try_from(bytes)
            .ok()
            .and_then(std::num::NonZeroU64::new)
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "text-artifact resident-memory reservation must be nonzero".to_owned(),
                )
            })?;
        self.resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id: generation_id.clone(),
                    component,
                },
                requested,
            )
            .map_err(|_| RetrievalPortError::BudgetExceeded)
    }

    /// The durably attached artifact descriptor for one retained generation,
    /// or `None` when the generation has no published text artifact yet.
    fn published_descriptor(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<DurableCodeTextArtifactDescriptorV1>, RetrievalPortError> {
        let Some(pointer) = self
            .publication
            .read_publication_pointer()
            .map_err(text_artifact_unavailable)?
        else {
            return Ok(None);
        };
        Ok(pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
            .and_then(|entry| entry.text_artifact.clone()))
    }

    /// Withdraw one exact missing/corrupt derived artifact so the immutable
    /// sealed generation can rebuild it. A corrupt regular file is moved out
    /// of the content-addressed namespace before the durable pointer is
    /// cleared; non-regular objects are preserved and refused fail-closed.
    fn withdraw_unavailable_descriptor(
        &self,
        descriptor: &DurableCodeTextArtifactDescriptorV1,
        quarantine_corrupt_file: bool,
    ) -> Result<(), RetrievalPortError> {
        let lock = acquire_code_generation_store_lock(&self.store_root)
            .map_err(text_artifact_unavailable)?;
        let pointer = self
            .publication
            .read_publication_pointer()
            .map_err(text_artifact_unavailable)?
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "durable publication pointer disappeared during artifact repair".to_owned(),
                )
            })?;
        let current = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == descriptor.generation_id.as_str())
            .and_then(|entry| entry.text_artifact.as_ref());
        if current != Some(descriptor) {
            return Err(RetrievalPortError::AuthorityUnavailable(
                "durable text-artifact attachment changed during repair".to_owned(),
            ));
        }

        let mut quarantined = None;
        if quarantine_corrupt_file {
            let path = code_text_artifact_path(&self.store_root, descriptor)
                .map_err(text_artifact_unavailable)?;
            let metadata = path.symlink_metadata().map_err(text_artifact_unavailable)?;
            if !metadata.file_type().is_file() {
                return Err(RetrievalPortError::Contract(
                    "corrupt code text artifact is not a regular file".to_owned(),
                ));
            }
            let quarantine =
                path.with_extension(format!("corrupt-{}-{}", std::process::id(), now_micros().0));
            std::fs::rename(&path, &quarantine).map_err(text_artifact_unavailable)?;
            DaemonCodeIndexPublicationStoreV1::sync_directory(path.parent().ok_or_else(|| {
                RetrievalPortError::Contract("code text artifact path has no parent".to_owned())
            })?)
            .map_err(text_artifact_unavailable)?;
            quarantined = Some(quarantine);
        }

        withdraw_verified_text_artifact_under_lock(&lock, &pointer, descriptor)
            .map_err(text_artifact_unavailable)?;
        if let Some(quarantine) = quarantined {
            std::fs::remove_file(&quarantine).map_err(text_artifact_unavailable)?;
            DaemonCodeIndexPublicationStoreV1::sync_directory(quarantine.parent().ok_or_else(
                || {
                    RetrievalPortError::Contract(
                        "quarantined code text artifact has no parent".to_owned(),
                    )
                },
            )?)
            .map_err(text_artifact_unavailable)?;
        }
        Ok(())
    }

    /// Remove one incompatible resumable staging database before rebuilding
    /// it with the current artifact format. The path is daemon-derived and the
    /// canonical store lock serializes this replacement with generation and
    /// artifact publication; non-regular collisions fail closed.
    fn discard_incompatible_staging(
        &self,
        staging_path: &Path,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), RetrievalPortError> {
        checkpoint_text_artifact_control(control)?;
        let artifacts_root = code_text_artifacts_root(&self.store_root);
        if staging_path.parent() != Some(artifacts_root.as_path()) {
            return Err(RetrievalPortError::Contract(
                "text-artifact staging path is outside its canonical root".to_owned(),
            ));
        }
        let _lock = acquire_code_generation_store_lock(&self.store_root)
            .map_err(text_artifact_unavailable)?;
        checkpoint_text_artifact_control(control)?;
        let metadata = staging_path
            .symlink_metadata()
            .map_err(text_artifact_unavailable)?;
        if !metadata.file_type().is_file() {
            return Err(RetrievalPortError::Contract(
                "incompatible text-artifact staging path is not a regular file".to_owned(),
            ));
        }
        std::fs::remove_file(staging_path).map_err(text_artifact_unavailable)?;
        DaemonCodeIndexPublicationStoreV1::sync_directory(&artifacts_root)
            .map_err(text_artifact_unavailable)
    }

    /// Resolve the immutable, content-addressed sealed file for one retained
    /// generation without re-encoding its decoded in-memory representation.
    fn sealed_identity(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<DurableSealedCodeGenerationIdentityV1, RetrievalPortError> {
        let pointer = self
            .publication
            .read_publication_pointer()
            .map_err(text_artifact_unavailable)?
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "durable code-generation index is missing".to_owned(),
                )
            })?;
        let entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(format!(
                    "durable code-generation index does not retain generation {generation_id}"
                ))
            })?;
        Ok(DurableSealedCodeGenerationIdentityV1 {
            locator: entry.generation_file.clone(),
            digest: ManifestDigest::new(entry.state_digest.clone())
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            size_bytes: entry.size_bytes,
        })
    }

    /// Open the exact durable sealed file after the caller has admitted the
    /// build's resident-memory ceiling. The lexical source verifies the whole
    /// file content address during its one bounded structural scan.
    fn open_sealed_source(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedSealedLexicalPageSourceV1<File>, RetrievalPortError> {
        DaemonCodeIndexPublicationStoreV1::validate_generation_file(&identity.locator)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let path = self.publication.generations_root.join(&identity.locator);
        let metadata = path.symlink_metadata().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RetrievalPortError::AuthorityUnavailable(
                    "durable sealed lexical source is missing".to_owned(),
                )
            } else {
                text_artifact_unavailable(error)
            }
        })?;
        if !metadata.file_type().is_file() || metadata.len() != identity.size_bytes {
            return Err(RetrievalPortError::Contract(
                "durable sealed lexical source identity is corrupt".to_owned(),
            ));
        }
        let file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RetrievalPortError::AuthorityUnavailable(
                    "durable sealed lexical source disappeared before open".to_owned(),
                )
            } else {
                text_artifact_unavailable(error)
            }
        })?;
        match VerifiedSealedLexicalPageSourceV1::open_content_addressed(
            file,
            identity.size_bytes,
            identity.digest.clone(),
            TEXT_ARTIFACT_PAGE_CHUNKS_V1,
            TEXT_ARTIFACT_PAGE_BYTES_V1,
            control,
        ) {
            Ok(source) => Ok(source),
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("format revision is incompatible") =>
            {
                let manifest_bytes = std::fs::read(&path).map_err(text_artifact_unavailable)?;
                if DaemonCodeIndexPublicationStoreV1::state_digest(&manifest_bytes)
                    != identity.digest.as_str()
                {
                    return Err(RetrievalPortError::Contract(
                        "partitioned sealed lexical manifest digest does not verify".to_owned(),
                    ));
                }
                let manifest = File::open(path).map_err(text_artifact_unavailable)?;
                VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
                    manifest,
                    &manifest_bytes,
                    identity.digest.clone(),
                    |digest, expected_size, buffer| {
                        self.publication.read_partitioned_segment(
                            SealedGenerationSegmentReadV1::Whole {
                                digest,
                                size_bytes: expected_size,
                            },
                            buffer,
                        )
                    },
                    TEXT_ARTIFACT_PAGE_CHUNKS_V1,
                    TEXT_ARTIFACT_PAGE_BYTES_V1,
                )
                .map_err(map_sealed_page_source_error)
                .and_then(|source| {
                    source.ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "partitioned sealed lexical source is incompatible".to_owned(),
                        )
                    })
                })
            }
            Err(error) => Err(map_sealed_page_source_error(error)),
        }
    }

    fn open_sealed_source_with_progress<F>(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
        mut progress: F,
    ) -> Result<VerifiedSealedLexicalPageSourceV1<File>, RetrievalPortError>
    where
        F: FnMut(u64, u64),
    {
        DaemonCodeIndexPublicationStoreV1::validate_generation_file(&identity.locator)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let path = self.publication.generations_root.join(&identity.locator);
        let metadata = path.symlink_metadata().map_err(text_artifact_unavailable)?;
        if !metadata.file_type().is_file() || metadata.len() != identity.size_bytes {
            return Err(RetrievalPortError::Contract(
                "durable sealed lexical source identity is corrupt".to_owned(),
            ));
        }
        let file = File::open(&path).map_err(text_artifact_unavailable)?;
        match VerifiedSealedLexicalPageSourceV1::open_content_addressed_with_progress(
            file,
            identity.size_bytes,
            identity.digest.clone(),
            TEXT_ARTIFACT_PAGE_CHUNKS_V1,
            TEXT_ARTIFACT_PAGE_BYTES_V1,
            control,
            &mut progress,
        ) {
            Ok(source) => Ok(source),
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("format revision is incompatible") =>
            {
                progress(0, identity.size_bytes);
                let manifest_bytes = std::fs::read(&path).map_err(text_artifact_unavailable)?;
                if DaemonCodeIndexPublicationStoreV1::state_digest(&manifest_bytes)
                    != identity.digest.as_str()
                {
                    return Err(RetrievalPortError::Contract(
                        "partitioned sealed lexical manifest digest does not verify".to_owned(),
                    ));
                }
                progress(identity.size_bytes, identity.size_bytes);
                let manifest = File::open(&path).map_err(text_artifact_unavailable)?;
                VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
                    manifest,
                    &manifest_bytes,
                    identity.digest.clone(),
                    |digest, expected_size, buffer| {
                        self.publication.read_partitioned_segment(
                            SealedGenerationSegmentReadV1::Whole {
                                digest,
                                size_bytes: expected_size,
                            },
                            buffer,
                        )
                    },
                    TEXT_ARTIFACT_PAGE_CHUNKS_V1,
                    TEXT_ARTIFACT_PAGE_BYTES_V1,
                )
                .map_err(map_sealed_page_source_error)
                .and_then(|source| {
                    source.ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "partitioned sealed lexical source is incompatible".to_owned(),
                        )
                    })
                })
            }
            Err(error) => Err(map_sealed_page_source_error(error)),
        }
    }

    /// Durably publish one finalized staging artifact: content-address it,
    /// move it into the artifacts root, fsync the directory, and attach the
    /// descriptor to the sealed generation entry under the store lock.
    fn publish(
        &self,
        staging_path: &Path,
        generation_id: &CodeGenerationId,
        sealed_identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<DurableCodeTextArtifactDescriptorV1, RetrievalPortError> {
        hotpath::measure_block!("query.artifact.store.publish", {
            let artifacts_root = code_text_artifacts_root(&self.store_root);
            ensure_private_text_artifacts_root(&artifacts_root)?;
            // Publication and artifact retention share this canonical store lock.
            // Hold it from the first staging observation until pointer attachment
            // is durable so retention cannot unlink a newly visible artifact from
            // a plan made before the descriptor was attached.
            let lock = acquire_code_generation_store_lock(&self.store_root)
                .map_err(text_artifact_unavailable)?;
            let (artifact_hex, artifact_size_bytes) = hotpath::measure_block!(
                "query.artifact.store.state_digest",
                sha256_private_file_hex_and_size(staging_path, control)
            )?;
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("query.artifact.store.digest_bytes").set(artifact_size_bytes);
            let descriptor = DurableCodeTextArtifactDescriptorV1 {
                generation_id: generation_id.clone(),
                artifact_file: format!("text-artifact-{artifact_hex}.bin"),
                artifact_digest: ManifestDigest::from_sha256_bytes(
                    &hex::decode(&artifact_hex).map_err(text_artifact_unavailable)?,
                )
                .map_err(text_artifact_unavailable)?,
                artifact_size_bytes,
            };
            let final_path = artifacts_root.join(&descriptor.artifact_file);
            match final_path.symlink_metadata() {
                Ok(_) => {
                    // A digest-derived name is not proof that an existing filesystem
                    // object contains the named bytes. Verify the stable destination
                    // before withdrawing staging evidence; a symlink, non-regular
                    // object, truncated file, or same-name collision fails closed.
                    let (existing_hex, existing_size_bytes) = hotpath::measure_block!(
                        "query.artifact.store.dedupe_compare",
                        sha256_private_file_hex_and_size(&final_path, control)
                    )?;
                    if existing_size_bytes != artifact_size_bytes {
                        return Err(RetrievalPortError::Contract(
                            "existing code text artifact does not match its content address"
                                .to_owned(),
                        ));
                    }
                    if existing_hex != artifact_hex {
                        return Err(RetrievalPortError::Contract(
                            "existing code text artifact contains different bytes".to_owned(),
                        ));
                    }
                    std::fs::remove_file(staging_path).map_err(text_artifact_unavailable)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::rename(staging_path, &final_path)
                        .map_err(text_artifact_unavailable)?;
                }
                Err(error) => return Err(text_artifact_unavailable(error)),
            }
            hotpath::measure_block!(
                "query.artifact.store.seal_fsync",
                DaemonCodeIndexPublicationStoreV1::sync_directory(&artifacts_root)
                    .map_err(text_artifact_unavailable)
            )?;
            let pointer = self
                .publication
                .read_publication_pointer()
                .map_err(text_artifact_unavailable)?
                .ok_or_else(|| {
                    RetrievalPortError::AuthorityUnavailable(
                        "no durable publication pointer exists for text-artifact attachment"
                            .to_owned(),
                    )
                })?;
            hotpath::measure_block!(
                "query.artifact.store.pointer_commit",
                attach_verified_text_artifact_under_lock(
                    &lock,
                    &pointer,
                    sealed_identity,
                    descriptor.clone(),
                )
                .map_err(text_artifact_unavailable)
            )?;
            Ok(descriptor)
        })
    }
}

struct ProductionCodeGraphServingV1 {
    pub graph: GraphLane<CodeGraphEvidenceReader>,
    store: Option<Arc<CodeGraphProjectionStore>>,
    _graph_authority: CodeGraphServingAuthorityV1,
}

enum CodeGraphActivationStateV1 {
    Pending,
    Refused(&'static str),
    Unavailable(String),
    Ready(Arc<ProductionCodeGraphServingV1>),
}

#[derive(Clone)]
enum CodeGraphServingAuthorityV1 {
    Persistent {
        /// Retained solely to keep the canonical graph store lease alive for
        /// the lifetime of the serving owners; never read.
        _lease:
            Arc<tracedecay_runtime_core::store_runtime::registry::CanonicalCodeGraphStoreLeaseV1>,
    },
    #[cfg(any(test, feature = "test-helpers"))]
    Memory,
}

impl LatestCompleteCodeIndexV1 {
    pub fn text_generation_handle(&self) -> LatestCodeTextGenerationV1 {
        self.text.clone()
    }

    /// Drive the retained text lane to completion so tests can assert exact
    /// and lexical owners without depending on a request-path warm.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn production_query_owners(
        &self,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        self.text.production_query_owners()
    }

    pub fn generation(&self) -> &CodeIndexPublishedGenerationV1 {
        self.generation.as_ref()
    }

    /// The decoded generation as a shared handle.
    ///
    /// Graph activation offers this to the code-graph manifest provider so the
    /// publication and recovery branches reuse this decode instead of reading
    /// and parsing the identical sealed payload a second time.
    pub fn generation_handle(&self) -> Arc<CodeIndexPublishedGenerationV1> {
        Arc::clone(&self.generation)
    }

    /// Point-lookup indices over this sealed generation's record vectors.
    ///
    /// Built at most once per generation and shared by every clone of this
    /// handle (and therefore by every concurrent query), the same way
    /// [`Self::production_query_owners`] shares its lane owners. Serving a
    /// query never rebuilds the indices; only loading a new generation does.
    pub fn record_index(&self) -> &queries::GenerationRecordIndexV1 {
        self.record_index
            .get_or_init(|| queries::GenerationRecordIndexV1::build(self.generation.as_ref()))
    }

    /// Build every per-generation serving derivation now, off the request path.
    ///
    /// A sealed generation is immutable, so its exact-admission sweep, record
    /// lookup indices, lane owners, and test-attribution join are pure functions
    /// of it. Each is memoized behind a `OnceLock` that would otherwise be
    /// initialized by whichever request arrives first — charging one query an
    /// O(store) canonical sweep over every chunk. Warming them where the
    /// generation is activated makes the FIRST query O(result), like every later
    /// one.
    ///
    /// Failures are deliberately discarded: this is a pre-warm, not a gate. Only
    /// success is memoized, so every serving path still runs — and still fails
    /// closed on — the exact same checks.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn warm_serving_caches(&self) {
        // Completion, not one bounded advance: the exact/lexical lane owners
        // install only when the resumable text build finishes, so activation
        // must drive the loop or the first request inherits a warming
        // abstention instead of warm owners. Each advance inside stays
        // bounded and cancellation-checkpointed.
        let _ = self.production_query_owners();
        // Mirror the persistent-graph activation warm set: the record lookup
        // indices and the test-attribution join are pure functions of the
        // sealed generation and must exist before the first request, not be
        // charged to it. Neither touches exact-admission staging, so the
        // released staging corpus stays released.
        let _ = self.record_index();
        let _ = self.generation.test_attribution_authority();
        let generation_id = self.generation.manifest().generation_id.clone();
        let Ok(freshness) = self.source_freshness() else {
            return;
        };
        let Ok(reader) = CodeGraphEvidenceReader::new(
            generation_id,
            Some(self.generation.snapshot().repository.clone()),
            freshness,
            self.generation.edges(),
            self.generation.chunks().chunks(),
        ) else {
            return;
        };
        let _ = self.install_graph_serving(reader, None, CodeGraphServingAuthorityV1::Memory);
    }
}

impl LatestCodeTextGenerationV1 {
    pub fn metadata(&self) -> &VerifiedSealedTextGenerationMetadataV1 {
        &self.metadata
    }

    pub fn uses_partitioned_manifest(&self) -> bool {
        self.sealed_format_revision
            == tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1
    }

    pub fn artifact_occurrence_by_chunk(
        &self,
        chunk: &tracedecay_domain::CodeSearchChunkId,
    ) -> Result<CodeLexicalArtifactOccurrenceV1, RetrievalPortError> {
        self.production_query_owners_with_budget(&queries::maximum_retrieval_budget())?
            .artifact_occurrence_by_chunk(chunk)
    }
}

impl LatestCompleteCodeIndexV1 {
    /// Whether the record lookup indices are already built for this generation.
    #[cfg(test)]
    fn record_index_is_warm(&self) -> bool {
        self.record_index.get().is_some()
    }
}

impl LatestCodeTextGenerationV1 {
    /// Whether the exact/lexical lane owners are already built.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn query_owners_are_warm(&self) -> bool {
        self.text_serving_is_ready()
    }

    fn text_serving_is_ready(&self) -> bool {
        self.query_owners.get().is_some()
    }

    fn text_serving_needs_work(&self) -> bool {
        !self.text_serving_is_ready() && !self.text_projection_failed.load(Ordering::Acquire)
    }

    fn same_text_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.text_projection_build, &other.text_projection_build)
    }

    fn mark_text_serving_failed(&self) {
        self.text_projection_failed.store(true, Ordering::Release);
    }
}

impl LatestCompleteCodeIndexV1 {
    fn semantic_evaluation_snapshot(&self) -> SemanticEvaluationCodeSnapshotV1 {
        SemanticEvaluationCodeSnapshotV1 {
            source_generation: self.generation.manifest().generation_id.clone(),
            source_manifest_digest: self
                .generation
                .projection()
                .request()
                .changes
                .manifest_digest
                .clone(),
            snapshot_digest: self.generation.manifest().snapshot_digest.clone(),
            capability_manifest_digest: self.generation.capability().manifest_digest.clone(),
        }
    }

    pub fn test_attribution_authority(
        &self,
    ) -> Result<
        crate::code_index::production::PublishedGenerationTestAttributionAuthorityV1,
        crate::code_index::production::CodeIndexProductionErrorV1,
    > {
        self.generation.test_attribution_authority()
    }

    #[cfg(test)]
    pub fn exact(
        &self,
    ) -> Result<
        Arc<Vec<crate::code_index::chunks::ExtractionAdmittedCodeSearchChunkV1>>,
        crate::code_index::chunks::ChunkingFailureV1,
    > {
        self.generation.admitted_chunks()
    }

    #[cfg(test)]
    pub fn lexical(&self) -> &[Arc<tracedecay_domain::CodeSearchChunkV1>] {
        self.generation.chunks().chunks()
    }

    #[cfg(test)]
    pub fn graph_edges(&self) -> &[tracedecay_domain::CanonicalRelationEdgeV1] {
        self.generation.edges()
    }

    #[cfg(test)]
    pub fn graph_abstentions(&self) -> &[crate::code_index::chunks::CodeIndexEdgeAbstentionV1] {
        self.generation.edge_abstentions()
    }
}

impl LatestCodeTextGenerationV1 {
    /// Return exact and lexical query owners bound to the latest complete
    /// published generation, driving the resumable text-artifact build to
    /// completion first.
    ///
    /// One bounded advance cannot finalize even a one-file generation, so
    /// this owner-warmup entry keeps advancing until the build reports
    /// completion. Every advance stays bounded and
    /// cancellation-checkpointed, so a shutdown or epoch bump still surfaces
    /// immediately through `?` rather than being absorbed by this loop.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn production_query_owners(
        &self,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        let mut advances = 0_usize;
        while !self.advance_text_serving(TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1)? {
            advances += 1;
            if advances >= TEXT_ARTIFACT_MAXIMUM_ACTIVATION_ADVANCES_V1 {
                return Err(RetrievalPortError::AuthorityUnavailable(
                    "code-index text serving owners are warming".to_owned(),
                ));
            }
        }
        self.production_query_owners_with_budget(&queries::maximum_retrieval_budget())
    }

    fn production_query_owners_with_budget(
        &self,
        _build_budget: &RetrievalBudget,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        if self.text_projection_failed.load(Ordering::Acquire) {
            return Err(RetrievalPortError::AuthorityUnavailable(
                "code-index text serving projection failed".to_owned(),
            ));
        }
        self.query_owners.get().map(Arc::clone).ok_or_else(|| {
            RetrievalPortError::AuthorityUnavailable(
                "code-index text serving owners are warming".to_owned(),
            )
        })
    }
}

impl LatestCodeTextGenerationV1 {
    fn production_graph_serving(
        &self,
    ) -> Result<Arc<ProductionCodeGraphServingV1>, RetrievalPortError> {
        match &*self
            .graph_activation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            CodeGraphActivationStateV1::Ready(serving) => Ok(Arc::clone(serving)),
            CodeGraphActivationStateV1::Refused(reason) => {
                Err(RetrievalPortError::Contract((*reason).to_owned()))
            }
            CodeGraphActivationStateV1::Unavailable(reason) => {
                Err(RetrievalPortError::AuthorityUnavailable(reason.clone()))
            }
            CodeGraphActivationStateV1::Pending => Err(RetrievalPortError::Contract(
                "code graph projection has not completed activation".to_owned(),
            )),
        }
    }
}

impl LatestCompleteCodeIndexV1 {
    /// Whether this generation's native graph has neither activated nor been
    /// refused.
    ///
    /// The activation state is shared by every handle bound to one sealed
    /// generation, so this answers for the handle already seated in the serving
    /// slot as much as for this one: a generation that reached serving through
    /// the exact route but has not attempted graph activation yet reports
    /// pending here while it serves text under the same generation id.
    pub fn graph_activation_is_pending(&self) -> bool {
        matches!(
            &*self
                .text
                .graph_activation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            CodeGraphActivationStateV1::Pending
        )
    }

    /// Snapshot graph-serving activation with one lock acquisition so status
    /// cannot combine states from opposite sides of an activation transition.
    pub fn code_graph_serving_readiness(
        &self,
    ) -> tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1 {
        self.text.code_graph_serving_readiness()
    }

    fn refuse_graph_activation(&self, reason: &'static str) {
        self.text.refuse_graph_activation(reason);
    }

    fn mark_graph_activation_unavailable(&self, reason: String) {
        self.text.mark_graph_activation_unavailable(reason);
    }
}

impl LatestCodeTextGenerationV1 {
    /// The retained verified-snapshot projection store for graph reads,
    /// present once persistent graph publication has completed.
    ///
    /// Occurrence-seeded adjacency reads are immediately available from the
    /// verified snapshot. Name, file, and import lookups may still report the
    /// typed catalog-warming state while their derived catalog builds in the
    /// background. Unlike the retrieval lanes there is no in-memory fallback,
    /// so an absent store is the typed not-activated state, never an empty
    /// serve.
    pub fn interactive_graph_store(
        &self,
    ) -> Result<Arc<CodeGraphProjectionStore>, RetrievalPortError> {
        let store = self
            .production_graph_serving()?
            .store
            .clone()
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "code graph projection has no persistent interactive store".to_owned(),
                )
            })?;
        Ok(store)
    }
}

impl LatestCodeTextGenerationV1 {
    pub fn code_graph_serving_readiness(
        &self,
    ) -> tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1 {
        match &*self
            .graph_activation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            CodeGraphActivationStateV1::Pending => {
                tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1::Pending
            }
            CodeGraphActivationStateV1::Refused(reason) => {
                tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1::Refused {
                    reason: (*reason).to_owned(),
                }
            }
            CodeGraphActivationStateV1::Unavailable(reason) => {
                tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1::Unavailable {
                    reason: reason.clone(),
                }
            }
            CodeGraphActivationStateV1::Ready(_) => {
                tracedecay_dashboard_api::code_index_freshness_api::CodeGraphServingReadinessV1::Ready
            }
        }
    }

    fn refuse_graph_activation(&self, reason: &'static str) {
        let mut state = self
            .graph_activation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, CodeGraphActivationStateV1::Ready(_)) {
            *state = CodeGraphActivationStateV1::Refused(reason);
        }
    }

    fn mark_graph_activation_unavailable(&self, reason: String) {
        let mut state = self
            .graph_activation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, CodeGraphActivationStateV1::Ready(_)) {
            *state = CodeGraphActivationStateV1::Unavailable(reason);
        }
    }
}

impl LatestCodeTextGenerationV1 {
    fn source_freshness(&self) -> Result<tracedecay_domain::SourceFreshness, RetrievalPortError> {
        production_code_index_freshness(
            self.metadata.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )
    }

    fn text_projection_metadata(
        &self,
    ) -> Result<CodeLexicalProjectionMetadataV1, RetrievalPortError> {
        let generation_id = self.metadata.manifest().generation_id.clone();
        let freshness = self.source_freshness()?;
        Ok(CodeLexicalProjectionMetadataV1 {
            generation: generation_id,
            repository_id: Some(self.metadata.snapshot().repository.clone()),
            logical_paths: self
                .metadata
                .snapshot()
                .files
                .iter()
                .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
                .collect(),
            freshness,
            exact_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RETRIEVER_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            lexical_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            exact_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        })
    }

    fn publish_text_progress_boundary(
        &self,
        build: &CodeTextArtifactBuildV1,
        progress: &tracedecay_query::retrieval::lexical::CodeLexicalArtifactBuildProgressV1,
        phase: CodeIndexBuildPhaseV1,
        current_batch_pages: u64,
        current_batch_payload_bytes: u64,
        last_commit_latency_micros: Option<u64>,
        observe_committed: bool,
    ) -> Result<(), RetrievalPortError> {
        let source_cursor = build.source.cursor();
        match progress.next_cursor.as_ref() {
            Some(cursor) if cursor == source_cursor => {}
            None if progress.next_page_ordinal == 0
                && progress.completed_chunks == 0
                && progress.completed_payload_bytes == 0
                && progress.completed_imports == 0
                && source_cursor.next_page_ordinal() == 0 => {}
            _ => {
                return Err(RetrievalPortError::Contract(
                    "text-artifact progress does not match the accepted sealed-source cursor"
                        .to_owned(),
                ));
            }
        }
        if progress.next_page_ordinal != source_cursor.next_page_ordinal()
            || progress.completed_chunks != source_cursor.emitted_chunks()
            || progress.completed_payload_bytes != source_cursor.emitted_payload_bytes()
            || progress.completed_imports != source_cursor.emitted_imports()
        {
            return Err(RetrievalPortError::Contract(
                "text-artifact progress counters do not match the sealed-source cursor".to_owned(),
            ));
        }
        let completed_files = build.source.completed_files();
        let completed_lexical_bytes = build
            .source
            .completed_lexical_bytes()
            .map_err(map_sealed_page_source_error)?;
        let total_lexical_bytes = build.source.total_lexical_bytes();
        let observed_at = Instant::now();
        let observed_micros = now_micros().0;
        let last_commit_latency_micros = last_commit_latency_micros.or_else(|| {
            self.text_progress_slot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .snapshot()
                .filter(|snapshot| {
                    snapshot.generation_id == self.metadata.manifest().generation_id.as_str()
                })
                .and_then(|snapshot| snapshot.last_commit_latency_micros)
        });
        let mut state = self
            .text_progress_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if observe_committed && progress.next_page_ordinal > 0 {
            state.observe_committed(CodeIndexCommittedProgressSampleV1 {
                observed_at,
                completed_files,
                completed_lexical_bytes,
            });
            #[cfg(feature = "hotpath")]
            {
                hotpath::gauge!("query.artifact.progress.committed_pages")
                    .set(progress.next_page_ordinal);
                hotpath::gauge!("query.artifact.progress.committed_lexical_bytes")
                    .set(completed_lexical_bytes);
            }
        }
        let (files_per_second, lexical_bytes_per_second, estimated_remaining_seconds) =
            state.rates_and_eta(total_lexical_bytes);
        let snapshot = CodeIndexBuildProgressV1 {
            generation_id: self.metadata.manifest().generation_id.as_str().to_owned(),
            daemon_incarnation: self.text_progress_daemon_incarnation,
            producer_incarnation: self.text_progress_producer_incarnation,
            progress_epoch: 0,
            sealed_source_digest: build.sealed_identity.digest.as_str().to_owned(),
            phase,
            committed_pages: progress.next_page_ordinal,
            committed_chunks: progress.completed_chunks,
            committed_imports: progress.completed_imports,
            committed_payload_bytes: progress.completed_payload_bytes,
            completed_files,
            total_files: build.source.total_files(),
            completed_lexical_bytes,
            total_lexical_bytes,
            current_batch_pages,
            current_batch_payload_bytes,
            elapsed_micros: state.elapsed_micros(),
            last_commit_latency_micros,
            files_per_second,
            lexical_bytes_per_second,
            estimated_remaining_seconds,
            last_progress_micros: observed_micros,
            blocked_reason: None,
        };
        drop(state);
        self.publish_text_progress_snapshot(snapshot);
        Ok(())
    }

    fn ready_text_progress_snapshot(
        &self,
        reader: &CodeLexicalArtifactReaderV1,
        sealed_identity: &DurableSealedCodeGenerationIdentityV1,
        source: &VerifiedSealedLexicalPageSourceV1<File>,
    ) -> Result<CodeIndexBuildProgressV1, RetrievalPortError> {
        let artifact = reader.verified_artifact();
        let generation_id = &self.metadata.manifest().generation_id;
        if artifact.generation() != generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let elapsed_micros = self
            .text_progress_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed_micros();
        Ok(CodeIndexBuildProgressV1 {
            generation_id: generation_id.as_str().to_owned(),
            daemon_incarnation: self.text_progress_daemon_incarnation,
            producer_incarnation: self.text_progress_producer_incarnation,
            progress_epoch: 0,
            sealed_source_digest: sealed_identity.digest.as_str().to_owned(),
            phase: CodeIndexBuildPhaseV1::Ready,
            committed_pages: artifact.page_count(),
            committed_chunks: artifact.total_chunks(),
            committed_imports: artifact.total_imports(),
            committed_payload_bytes: artifact.total_payload_bytes(),
            completed_files: source.total_files(),
            total_files: source.total_files(),
            completed_lexical_bytes: source.total_lexical_bytes(),
            total_lexical_bytes: source.total_lexical_bytes(),
            current_batch_pages: 0,
            current_batch_payload_bytes: 0,
            elapsed_micros,
            last_commit_latency_micros: None,
            files_per_second: None,
            lexical_bytes_per_second: None,
            estimated_remaining_seconds: None,
            last_progress_micros: now_micros().0,
            blocked_reason: None,
        })
    }

    fn publish_text_progress_snapshot(&self, snapshot: CodeIndexBuildProgressV1) {
        let generation_id = &self.metadata.manifest().generation_id;
        hotpath::measure_block!("query.artifact.progress.publish", {
            let _ = self
                .text_progress_slot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .publish(generation_id, self.text_progress_owner_epoch, snapshot);
        });
    }

    fn publish_text_progress_phase(
        &self,
        phase: CodeIndexBuildPhaseV1,
        current_batch_pages: u64,
        current_batch_payload_bytes: u64,
    ) {
        let generation_id = &self.metadata.manifest().generation_id;
        let elapsed_micros = self
            .text_progress_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed_micros();
        hotpath::measure_block!("query.artifact.progress.publish", {
            let mut slot = self
                .text_progress_slot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(current) = slot.snapshot() else {
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("query.artifact.progress.no_snapshot_total").inc(1u64);
                return;
            };
            let mut snapshot = current.as_ref().clone();
            snapshot.phase = phase;
            snapshot.current_batch_pages = current_batch_pages;
            snapshot.current_batch_payload_bytes = current_batch_payload_bytes;
            snapshot.elapsed_micros = elapsed_micros;
            // Entering a phase is not progress: every retry wake re-publishes
            // its phase before it re-attempts the work that was refused, so
            // clearing here erased the reason between two identical refusals
            // and status reported `blocked_reason: null` throughout a stall.
            // A committed boundary is the honest clear -- it builds a fresh
            // snapshot with no blocked reason -- and only that runs after work
            // actually landed.
            let _ = slot.publish(generation_id, self.text_progress_owner_epoch, snapshot);
        });
    }

    /// Publish the typed reason a text-artifact wake could not advance.
    ///
    /// One classifier for both halves of the build: an under-reported refusal
    /// in either the batch loop or finalization leaves status showing a phase
    /// that never changes and `blocked_reason: null`, which reads as slow
    /// progress rather than a refusal. Anything not classified here is a
    /// deterministic contract or corruption failure, which the caller
    /// surfaces as a hard error rather than a stalled phase.
    fn publish_text_artifact_block(&self, error: &CodeLexicalArtifactErrorV1) {
        match error {
            CodeLexicalArtifactErrorV1::Unreserved(_) => {
                self.publish_text_progress_blocked(CodeIndexBuildBlockedReasonV1::ResidentMemory);
            }
            CodeLexicalArtifactErrorV1::Io(_) | CodeLexicalArtifactErrorV1::Missing(_) => {
                self.publish_text_progress_blocked(
                    CodeIndexBuildBlockedReasonV1::ArtifactStoreUnavailable,
                );
            }
            _ => {}
        }
    }

    fn publish_text_progress_blocked(&self, reason: CodeIndexBuildBlockedReasonV1) {
        let generation_id = &self.metadata.manifest().generation_id;
        hotpath::measure_block!("query.artifact.progress.publish", {
            let mut slot = self
                .text_progress_slot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(current) = slot.snapshot() else {
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("query.artifact.progress.no_snapshot_total").inc(1u64);
                return;
            };
            let mut snapshot = current.as_ref().clone();
            snapshot.blocked_reason = Some(reason);
            let _ = slot.publish(generation_id, self.text_progress_owner_epoch, snapshot);
        });
    }

    #[cfg(feature = "hotpath")]
    fn text_progress_phase(&self) -> Option<CodeIndexBuildPhaseV1> {
        self.text_progress_slot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot()
            .map(|snapshot| snapshot.phase)
    }

    /// Advance at most `maximum_work` bounded page/finalization operations on
    /// this sealed generation's durable text artifact. The projection slot is
    /// both the generation-owned partial-state authority and the singleflight
    /// gate for concurrent scheduler wakes; corpus-sized verified opens run
    /// under a claimed slot with the lock released.
    fn advance_text_serving(&self, maximum_work: usize) -> Result<bool, RetrievalPortError> {
        let result = self.advance_text_serving_inner(maximum_work);
        if matches!(&result, Err(RetrievalPortError::Cancelled)) {
            #[cfg(feature = "hotpath")]
            match self.text_control.cancellation_source() {
                Some(GenerationTextCancellationSourceV1::Shutdown) => {
                    hotpath::gauge!("query.artifact.cancelled.shutdown_total").inc(1_u64);
                }
                Some(GenerationTextCancellationSourceV1::Superseded) => {
                    hotpath::gauge!("query.artifact.cancelled.superseded_total").inc(1_u64);
                }
                None => {
                    hotpath::gauge!("query.artifact.cancelled.external_total").inc(1_u64);
                }
            }
        }
        if result.as_ref().is_err_and(|error| {
            matches!(
                error,
                RetrievalPortError::CapabilityManifestRejected
                    | RetrievalPortError::GenerationMismatch
                    | RetrievalPortError::IncompatibleProjection
                    | RetrievalPortError::Contract(_)
            )
        }) {
            self.mark_text_serving_failed();
        }
        result
    }

    fn advance_text_serving_inner(&self, maximum_work: usize) -> Result<bool, RetrievalPortError> {
        if let Some(binding) = self.publication_binding.as_ref() {
            let current = self
                .text_artifact_store
                .publication
                .read_publication_pointer()
                .map_err(text_artifact_unavailable)?;
            if !binding.matches(current.as_ref()) {
                self.text_control.retire();
                return Err(RetrievalPortError::Cancelled);
            }
        }
        if self.query_owners.get().is_some() {
            return Ok(true);
        }
        let control = self.text_execution_control();
        self.advance_artifact_text_serving(maximum_work, &control)
    }

    fn text_execution_control(&self) -> GenerationTextControlV1 {
        self.text_control.clone()
    }

    fn take_preopened_source_or_open(
        &self,
        sealed_identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedSealedLexicalPageSourceV1<File>, RetrievalPortError> {
        if let Some(source) = self
            .preopened_source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Ok(source);
        }
        let mut source = self
            .text_artifact_store
            .open_sealed_source(sealed_identity, control)?;
        if let Ok(Some(published)) = self
            .text_artifact_store
            .publication
            .active_already_decoded()
            && published.manifest().generation_id == self.metadata.manifest().generation_id
        {
            let _ = source.attach_published_files(&published);
        }
        Ok(source)
    }

    /// One claimed head-open pass, run with the slot lock released: reopen
    /// the published durable head when one exists, otherwise authenticate the
    /// sealed source and begin (or resume) the staging build. Fail-closed:
    /// owners are installed only from a digest-verified reader, and a
    /// withdrawn head falls through to the resumable build rather than an
    /// empty success.
    fn open_published_head_or_begin_build(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<TextHeadOpenOutcomeV1, RetrievalPortError> {
        let store = &self.text_artifact_store;
        hotpath::gauge!("query.artifact.build_memory_budget_bytes")
            .set(CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1);
        hotpath::gauge!("query.artifact.source_batch_pages_max").set(TEXT_ARTIFACT_BATCH_PAGES_V1);
        hotpath::gauge!("query.artifact.source_batch_bytes_max").set(TEXT_ARTIFACT_BATCH_BYTES_V1);
        let generation_id = self.metadata.manifest().generation_id.clone();
        if let Some(descriptor) = store.published_descriptor(&generation_id)? {
            // Durable-head reopen: a restart serves the published
            // artifact without rebuilding it. Reserve the complete reader
            // ceiling before even resolving or touching the artifact path.
            let reader_reservation = store.reserve_resident_memory(
                &generation_id,
                "code-text-artifact-reader",
                CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            )?;
            let path = code_text_artifact_path(store.store_root(), &descriptor)
                .map_err(text_artifact_unavailable)?;
            let reader = CodeLexicalArtifactReaderV1::open_content_addressed(
                path,
                &descriptor.artifact_digest,
                descriptor.artifact_size_bytes,
                CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
                control,
            );
            match reader {
                Ok(reader) => {
                    let sealed_identity = store.sealed_identity(&generation_id)?;
                    let source = self.take_preopened_source_or_open(&sealed_identity, control)?;
                    let ready_progress =
                        self.ready_text_progress_snapshot(&reader, &sealed_identity, &source)?;
                    drop(source);
                    self.install_artifact_owners(reader, reader_reservation)?;
                    self.publish_text_progress_snapshot(ready_progress);
                    return Ok(TextHeadOpenOutcomeV1::Served);
                }
                Err(CodeLexicalArtifactErrorV1::Missing(_)) => {
                    drop(reader_reservation);
                    store.withdraw_unavailable_descriptor(&descriptor, false)?;
                }
                Err(CodeLexicalArtifactErrorV1::Corrupt(_)) => {
                    drop(reader_reservation);
                    store.withdraw_unavailable_descriptor(&descriptor, true)?;
                }
                Err(CodeLexicalArtifactErrorV1::Incompatible(_)) => {
                    drop(reader_reservation);
                    store.withdraw_unavailable_descriptor(&descriptor, false)?;
                }
                Err(error) => return Err(map_text_artifact_error(error)),
            }
        }
        // The builder's advertised memory ceiling is reserved through the
        // process resident-memory authority before the build allocates.
        let build_reservation = store.reserve_resident_memory(
            &generation_id,
            "code-text-artifact-build",
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )?;
        let sealed_identity = store.sealed_identity(&generation_id)?;
        let sealed_hex = sealed_identity
            .digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "durable sealed lexical source digest is not SHA-256".to_owned(),
                )
            })?;
        let artifacts_root = code_text_artifacts_root(store.store_root());
        ensure_private_text_artifacts_root(&artifacts_root)?;
        let staging_path = artifacts_root.join(format!(".text-artifact-{sealed_hex}.staging"));
        let mut source = self.take_preopened_source_or_open(&sealed_identity, control)?;
        let builder_budget = text_artifact_builder_budget(source.staging_window_bytes())?;
        let metadata = self.text_projection_metadata()?;
        let mut builder = if staging_path.exists() {
            match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &staging_path,
                metadata.clone(),
                builder_budget,
                control,
            ) {
                Ok(builder) => Ok(builder),
                Err(CodeLexicalArtifactErrorV1::Incompatible(_)) => {
                    store.discard_incompatible_staging(&staging_path, control)?;
                    CodeLexicalArtifactBuilderV1::create_with_memory_budget(
                        &staging_path,
                        metadata.clone(),
                        builder_budget,
                    )
                }
                Err(error) => Err(error),
            }
        } else {
            CodeLexicalArtifactBuilderV1::create_with_memory_budget(
                &staging_path,
                metadata.clone(),
                builder_budget,
            )
        }
        .map_err(map_text_artifact_error)?;
        let mut progress = builder.progress().map_err(map_text_artifact_error)?;
        if let Some(cursor) = progress.next_cursor.as_ref() {
            match source.restore_cursor_classified(cursor, control) {
                Ok(()) => {}
                Err(VerifiedSealedLexicalCursorRestoreErrorV1::IncompatiblePosition) => {
                    drop(builder);
                    store.discard_incompatible_staging(&staging_path, control)?;
                    builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
                        &staging_path,
                        metadata,
                        builder_budget,
                    )
                    .map_err(map_text_artifact_error)?;
                    progress = builder.progress().map_err(map_text_artifact_error)?;
                }
                Err(VerifiedSealedLexicalCursorRestoreErrorV1::Production(error)) => {
                    return Err(map_sealed_page_source_error(error));
                }
            }
        }
        let initialized = CodeTextArtifactBuildV1 {
            builder,
            source,
            sealed_identity,
            source_receipt: None,
            staging_path,
            _build_reservation: build_reservation,
        };
        self.publish_text_progress_boundary(
            &initialized,
            &progress,
            CodeIndexBuildPhaseV1::SourceScan,
            0,
            0,
            None,
            true,
        )?;
        Ok(TextHeadOpenOutcomeV1::Build(Box::new(initialized)))
    }

    /// The durable-artifact journey: reopen a published head when one exists,
    /// otherwise stream the sealed generation through the staging builder one
    /// bounded page window at a time, finalize, publish, and reopen.
    ///
    /// Corpus-sized verified opens (the published-head reopen and the
    /// publication tail's reopen) run under a `HeadOpening` claim with the
    /// slot lock released, so a concurrent wake parks with typed cancellation
    /// instead of blocking on the mutex for the whole open.
    #[hotpath::measure(label = "query.artifact.batch.scheduler_wake")]
    fn advance_artifact_text_serving(
        &self,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, RetrievalPortError> {
        let store = &self.text_artifact_store;
        let mut parked = false;
        let mut slot = loop {
            if self.query_owners.get().is_some() {
                return Ok(true);
            }
            // A first arrival lets the batch/open work itself observe the
            // control (its cancellation checkpoints are the historical
            // authority); a parked arrival owns no work, so it must
            // checkpoint here for its cancellation to stay typed and prompt.
            if parked {
                checkpoint_text_artifact_control(control)?;
            }
            let guard = self.text_projection_build.lock_slot();
            match &*guard {
                CodeTextProjectionSlotV1::Idle | CodeTextProjectionSlotV1::Building(_) => {
                    break guard;
                }
                CodeTextProjectionSlotV1::HeadOpening => {
                    // Another wake owns the corpus-sized verified open. Park
                    // until the claim resolves; the bounded interval keeps
                    // this wake's own cancellation typed and prompt even
                    // while the owner is inside one long read or digest call.
                    let (waited, _timed_out) = hotpath::measure_block!(
                        "query.artifact.head_open.singleflight_wait",
                        self.text_projection_build
                            .ready
                            .wait_timeout(guard, TEXT_HEAD_OPEN_CANCELLATION_CHECK_INTERVAL_V1)
                            .unwrap_or_else(PoisonError::into_inner)
                    );
                    drop(waited);
                    parked = true;
                }
            }
        };
        if matches!(&*slot, CodeTextProjectionSlotV1::Idle) {
            // Owners are installed only under a `HeadOpening` claim, so an
            // `Idle` slot with owners already set means a prior claim
            // finished between this wake's owners check and its lock.
            if self.query_owners.get().is_some() {
                return Ok(true);
            }
            *slot = CodeTextProjectionSlotV1::HeadOpening;
            drop(slot);
            let mut claim = TextHeadOpenClaimV1::new(&self.text_projection_build);
            match self.open_published_head_or_begin_build(control)? {
                TextHeadOpenOutcomeV1::Served => return Ok(true),
                TextHeadOpenOutcomeV1::Build(initialized) => {
                    slot = claim.install_build(initialized);
                }
            }
        }
        let CodeTextProjectionSlotV1::Building(artifact_build) = &mut *slot else {
            return Err(RetrievalPortError::Contract(
                "code-index text artifact build state is missing".to_owned(),
            ));
        };
        let mut remaining = maximum_work.min(TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1);
        while remaining > 0 && artifact_build.source_receipt.is_none() {
            let maximum_batch_pages = remaining.clamp(1, TEXT_ARTIFACT_BATCH_PAGES_V1);
            let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(
                maximum_batch_pages,
                TEXT_ARTIFACT_BATCH_BYTES_V1,
            )
            .map_err(map_sealed_page_source_error)?;
            #[cfg(feature = "hotpath")]
            let completed_lexical_bytes_before = artifact_build
                .source
                .completed_lexical_bytes()
                .map_err(map_sealed_page_source_error)?;
            self.publish_text_progress_phase(CodeIndexBuildPhaseV1::SourceScan, 0, 0);
            let mut durable_progress = None;
            let mut commit_latency_micros = None;
            let admitted = {
                let (source, builder) = (&mut artifact_build.source, &mut artifact_build.builder);
                source.next_page_batch_if(control, bounds, |pages| {
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("query.artifact.batch.offered_pages_total")
                        .inc(u64::try_from(pages.len()).unwrap_or(u64::MAX));
                    let offered_batch_pages = u64::try_from(pages.len()).map_err(|_| {
                        CodeLexicalArtifactErrorV1::Contract(
                            "text-artifact batch page count exceeds u64".to_owned(),
                        )
                    })?;
                    let offered_payload_bytes = pages.iter().try_fold(0_u64, |total, page| {
                        total.checked_add(page.payload_bytes()).ok_or_else(|| {
                            CodeLexicalArtifactErrorV1::Contract(
                                "text-artifact batch payload bytes overflowed".to_owned(),
                            )
                        })
                    })?;
                    self.publish_text_progress_phase(
                        CodeIndexBuildPhaseV1::RelationalPreparation,
                        offered_batch_pages,
                        offered_payload_bytes,
                    );
                    let (progress, accepted) =
                        hotpath::measure_block!("query.artifact.batch.builder", {
                            let prepared =
                                builder.prepare_admissible_page_prefix(pages, control)?;
                            let accepted = prepared.accepted_prefix();
                            let accepted_pages = &pages[..accepted.get()];
                            #[cfg(feature = "hotpath")]
                            hotpath::gauge!("query.artifact.batch.accepted_pages_total")
                                .inc(u64::try_from(accepted_pages.len()).unwrap_or(u64::MAX));
                            let batch_pages =
                                u64::try_from(accepted_pages.len()).map_err(|_| {
                                    CodeLexicalArtifactErrorV1::Contract(
                                        "text-artifact accepted page count exceeds u64".to_owned(),
                                    )
                                })?;
                            let batch_payload_bytes =
                                accepted_pages.iter().try_fold(0_u64, |total, page| {
                                    total.checked_add(page.payload_bytes()).ok_or_else(|| {
                                        CodeLexicalArtifactErrorV1::Contract(
                                            "text-artifact accepted payload bytes overflowed"
                                                .to_owned(),
                                        )
                                    })
                                })?;
                            self.publish_text_progress_phase(
                                CodeIndexBuildPhaseV1::BulkCommit,
                                batch_pages,
                                batch_payload_bytes,
                            );
                            let commit_started = Instant::now();
                            let progress = builder
                                .append_prepared_pages(prepared.prepared_pages(), control)?;
                            commit_latency_micros = Some(
                                u64::try_from(commit_started.elapsed().as_micros())
                                    .unwrap_or(u64::MAX),
                            );
                            Ok::<_, CodeLexicalArtifactErrorV1>((progress, accepted))
                        })?;
                    durable_progress = Some(progress);
                    Ok(accepted)
                })
            };
            let admitted = match admitted {
                Ok(Ok(admitted)) => admitted,
                Ok(Err(error @ CodeLexicalArtifactErrorV1::BatchTooLarge { .. })) => {
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("query.artifact.batch.refusal_total").inc(1u64);
                    return Err(map_text_artifact_error(error));
                }
                Ok(Err(error)) => {
                    self.publish_text_artifact_block(&error);
                    return Err(map_text_artifact_error(error));
                }
                Err(error) => return Err(map_sealed_page_source_error(error)),
            };
            match admitted {
                VerifiedSealedLexicalPageBatchReadV1::Pages(pages) => {
                    let page_count = pages.len();
                    let progress = durable_progress.as_ref().ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "accepted text-artifact batch has no durable builder progress"
                                .to_owned(),
                        )
                    })?;
                    let batch_payload_bytes = pages.iter().try_fold(0_u64, |total, page| {
                        total.checked_add(page.payload_bytes()).ok_or_else(|| {
                            RetrievalPortError::Contract(
                                "accepted text-artifact batch payload bytes overflowed".to_owned(),
                            )
                        })
                    })?;
                    self.publish_text_progress_boundary(
                        artifact_build,
                        progress,
                        CodeIndexBuildPhaseV1::BulkCommit,
                        u64::try_from(page_count).unwrap_or(u64::MAX),
                        batch_payload_bytes,
                        commit_latency_micros,
                        true,
                    )?;
                    #[cfg(feature = "hotpath")]
                    {
                        let committed_lexical_bytes = artifact_build
                            .source
                            .completed_lexical_bytes()
                            .map_err(map_sealed_page_source_error)?
                            .saturating_sub(completed_lexical_bytes_before);
                        hotpath::gauge!("query.artifact.batch.committed_lexical_bytes_total")
                            .inc(committed_lexical_bytes);
                        if let Some(latency_micros) = commit_latency_micros {
                            hotpath::gauge!("query.artifact.progress.latest_commit_latency_micros")
                                .set(latency_micros);
                        }
                    }
                    remaining = remaining.checked_sub(page_count).ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "accepted text-artifact batch exceeded its work budget".to_owned(),
                        )
                    })?;
                }
                VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) => {
                    artifact_build.source_receipt = Some(receipt);
                    self.publish_text_progress_phase(CodeIndexBuildPhaseV1::IndexBuild, 0, 0);
                }
            }
        }
        let Some(source_receipt) = artifact_build.source_receipt.as_ref() else {
            return Ok(false);
        };
        if remaining == 0 {
            return Ok(false);
        }
        let finalization_rows = remaining
            .checked_mul(TEXT_ARTIFACT_FINALIZATION_ROWS_PER_OPERATION_V1)
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "code text artifact finalization work budget overflowed".to_owned(),
                )
            })?;
        #[cfg(feature = "hotpath")]
        let finalized = if matches!(
            self.text_progress_phase(),
            Some(CodeIndexBuildPhaseV1::Verification)
        ) {
            hotpath::measure_block!("query.artifact.finalization.digest_verify_wake", {
                artifact_build.builder.advance_finalization(
                    source_receipt,
                    finalization_rows,
                    control,
                )
            })
        } else {
            hotpath::measure_block!("query.artifact.index.build", {
                artifact_build.builder.advance_finalization(
                    source_receipt,
                    finalization_rows,
                    control,
                )
            })
        };
        #[cfg(not(feature = "hotpath"))]
        let finalized =
            artifact_build
                .builder
                .advance_finalization(source_receipt, finalization_rows, control);
        // A finalization wake is what status reports as `index_build` and
        // `verification`. Returning its refusal bare left those phases
        // indistinguishable from progress: a build stalled on resident-memory
        // admission published `phase=verification` with `blocked_reason=null`
        // forever, which is exactly how the PR-dogfood readiness timeout
        // presented. Classify the refusal the way the batch path already does
        // so the phase says why it cannot advance.
        let finalized = match finalized {
            Ok(step) => step,
            Err(error) => {
                self.publish_text_artifact_block(&error);
                return Err(map_text_artifact_error(error));
            }
        };
        let finalization_phase = match finalized {
            CodeLexicalArtifactFinalizationStepV1::Pending { phase, .. } => {
                let phase = match phase {
                    CodeLexicalArtifactFinalizationPhaseV1::IndexBuild => {
                        CodeIndexBuildPhaseV1::IndexBuild
                    }
                    CodeLexicalArtifactFinalizationPhaseV1::Verification => {
                        CodeIndexBuildPhaseV1::Verification
                    }
                };
                let progress = artifact_build
                    .builder
                    .progress()
                    .map_err(map_text_artifact_error)?;
                self.publish_text_progress_boundary(
                    artifact_build,
                    &progress,
                    phase,
                    0,
                    0,
                    None,
                    false,
                )?;
                return Ok(false);
            }
            CodeLexicalArtifactFinalizationStepV1::Ready(_) => CodeIndexBuildPhaseV1::Verification,
        };
        let progress = artifact_build
            .builder
            .progress()
            .map_err(map_text_artifact_error)?;
        self.publish_text_progress_boundary(
            artifact_build,
            &progress,
            finalization_phase,
            0,
            0,
            None,
            false,
        )?;
        // The publication tail content-addresses the finalized staging file
        // and reopens it verified — corpus-sized digest work — so it runs
        // under a fresh `HeadOpening` claim with the slot lock released, the
        // same discipline as the durable-head reopen. On failure the claim
        // restores `Idle` and the durable staging file resumes on a later
        // wake.
        let CodeTextProjectionSlotV1::Building(finished) =
            std::mem::replace(&mut *slot, CodeTextProjectionSlotV1::HeadOpening)
        else {
            *slot = CodeTextProjectionSlotV1::Idle;
            return Err(RetrievalPortError::Contract(
                "code-index text artifact build state vanished during publication".to_owned(),
            ));
        };
        drop(slot);
        let _publish_claim = TextHeadOpenClaimV1::new(&self.text_projection_build);
        let CodeTextArtifactBuildV1 {
            builder,
            source,
            sealed_identity,
            source_receipt: _,
            staging_path,
            _build_reservation: build_reservation,
        } = *finished;
        // Close the builder's SQLite connection before content-addressing the
        // finalized staging file.
        drop(builder);
        drop(source);
        let descriptor = store.publish(
            &staging_path,
            &self.metadata.manifest().generation_id,
            &sealed_identity,
            control,
        )?;
        let reader_reservation = store.reserve_resident_memory(
            &self.metadata.manifest().generation_id,
            "code-text-artifact-reader",
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        )?;
        let final_path = code_text_artifact_path(store.store_root(), &descriptor)
            .map_err(text_artifact_unavailable)?;
        let reader = CodeLexicalArtifactReaderV1::open_content_addressed(
            final_path,
            &descriptor.artifact_digest,
            descriptor.artifact_size_bytes,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            control,
        )
        .map_err(map_text_artifact_error)?;
        self.install_artifact_owners(reader, reader_reservation)?;
        self.publish_text_progress_phase(CodeIndexBuildPhaseV1::Ready, 0, 0);
        drop(build_reservation);
        Ok(true)
    }

    fn install_artifact_owners(
        &self,
        reader: CodeLexicalArtifactReaderV1,
        reader_reservation: ResidentMemoryReservationV1,
    ) -> Result<(), RetrievalPortError> {
        if reader.retained_owned_bytes() > CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 {
            return Err(RetrievalPortError::Contract(
                "text-artifact reader exceeded its admitted resident-memory ceiling".to_owned(),
            ));
        }
        let authority = exact_serving_authority()?;
        let exact = ExactLane::new(authority.clone(), reader.exact_adapter(authority));
        let hydration = reader.clone();
        let lexical = LexicalLane::new(reader);
        let owners = Arc::new(ProductionCodeIndexQueryOwnersV1::artifact(
            exact,
            lexical,
            hydration,
            reader_reservation,
        ));
        let _ = self.query_owners.set(owners);
        Ok(())
    }
}

impl LatestCodeTextGenerationV1 {
    fn install_graph_serving(
        &self,
        graph_reader: CodeGraphEvidenceReader,
        store: Option<Arc<CodeGraphProjectionStore>>,
        graph_authority: CodeGraphServingAuthorityV1,
    ) -> Result<(), RetrievalPortError> {
        if graph_reader.generation() != &self.metadata.manifest().generation_id {
            return Err(RetrievalPortError::Contract(
                "code graph reader generation does not match sealed generation".to_owned(),
            ));
        }
        let serving = Arc::new(ProductionCodeGraphServingV1 {
            graph: GraphLane::new(graph_reader),
            store,
            _graph_authority: graph_authority,
        });
        *self
            .graph_activation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            CodeGraphActivationStateV1::Ready(serving);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CodeIndexSchedulerErrorV1 {
    #[error("code-index repository status failed: {0}")]
    Git(String),
    #[error("code-index filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("code-index identity construction failed: {0}")]
    Identity(String),
    #[error("code-index production owner failed: {0}")]
    Production(#[from] CodeIndexProductionErrorV1),
    #[error("code-index production owner configuration failed: {0}")]
    ProductionOpen(String),
    #[error("code-index privacy sanitizer failed: {0}")]
    Privacy(String),
    #[error("code-index graph projection failed: {0}")]
    GraphProjection(#[from] CodeGraphProjectionError),
    #[error("code-index graph activation failed: {0}")]
    GraphActivation(String),
    #[error("code-index graph activation refused: {0}")]
    GraphActivationRefused(&'static str),
    #[error("code-index semantic scheduling failed: {0}")]
    SemanticSchedule(String),
    #[error("code-index publication changed before serving activation: {0}")]
    PublicationConflict(String),
    #[error("code-index ignored dependency admission refused: {0}")]
    IgnoredDependency(#[from] CodeIndexIgnoredDependencyRefusalV1),
    #[error("code-index worker resident-memory admission refused: {0}")]
    WorkerMemoryAdmission(#[from] ResidentMemoryAdmissionFailureV1),
    #[error("code-index retained-source resident-memory admission refused: {0}")]
    SnapshotMemoryAdmission(ResidentMemoryAdmissionFailureV1),
    #[error("code-index retained-source resident-memory capacity is unavailable")]
    SnapshotMemoryCapacityUnavailable,
    #[error("code-index worker plan refused: {0}")]
    WorkerPlan(#[from] tracedecay_code_index::parallelism::CodeIndexWorkerPlanInstallErrorV1),
    #[cfg(not(any(test, feature = "test-helpers")))]
    #[error("code-index worker plan is not installed")]
    WorkerPlanNotInstalled,
}

impl CodeIndexSchedulerErrorV1 {
    /// An activation failure that leaves the sealed artifact intact and can
    /// succeed on a later attempt (deadline, cancellation, budget, an
    /// unavailable/saturated graph runtime, or a publication conflict). The
    /// worker retries activation of the same sealed generation with backoff
    /// for these instead of resealing a duplicate; payload corruption and
    /// identity failures stay terminal so reconcile can rebuild.
    ///
    /// `Conflict` is a lifecycle or compare-and-swap race — a graph runtime
    /// mid-close/retire, a concurrent publisher, or a superseded verified
    /// head — never evidence about the sealed payload. Classifying it
    /// terminal turned one such race into a permanent outage: the seat pass
    /// gave up stale serving, the next reconcile hit the same race, and the
    /// route answered `generation_unverified` until the daemon restarted.
    pub fn is_retryable_activation(&self) -> bool {
        match self {
            Self::GraphProjection(error) => matches!(
                error,
                CodeGraphProjectionError::Cancelled
                    | CodeGraphProjectionError::BudgetExhausted { .. }
                    | CodeGraphProjectionError::DeadlineExceeded
                    | CodeGraphProjectionError::Conflict { .. }
                    | CodeGraphProjectionError::Unavailable(_)
                    | CodeGraphProjectionError::Closed
            ),
            Self::GraphActivation(_) => true,
            Self::Git(_)
            | Self::Io(_)
            | Self::Identity(_)
            | Self::Production(_)
            | Self::ProductionOpen(_)
            | Self::Privacy(_)
            | Self::GraphActivationRefused(_)
            | Self::SemanticSchedule(_)
            | Self::PublicationConflict(_)
            | Self::IgnoredDependency(_)
            | Self::WorkerMemoryAdmission(_)
            | Self::SnapshotMemoryAdmission(_)
            | Self::SnapshotMemoryCapacityUnavailable
            | Self::WorkerPlan(_) => false,
            #[cfg(not(any(test, feature = "test-helpers")))]
            Self::WorkerPlanNotInstalled => false,
        }
    }

    /// The structured conflict verdict carried by a graph-projection
    /// activation failure, when this error is one. The seat retry loop uses
    /// it to recognize a deterministic conflict — the same guard site
    /// refusing with identical compared evidence on consecutive attempts
    /// over the same sealed generation — which no amount of backoff can
    /// outwait (issue #765).
    pub fn activation_conflict_context(&self) -> Option<&GraphConflictContextV1> {
        match self {
            Self::GraphProjection(CodeGraphProjectionError::Conflict { context }) => Some(context),
            _ => None,
        }
    }

    pub fn is_graph_activation_refusal(&self) -> bool {
        matches!(self, Self::GraphActivationRefused(_))
            || matches!(
                self,
                Self::GraphProjection(CodeGraphProjectionError::BudgetExhausted { budget, .. })
                    if budget == "resident_memory"
            )
    }

    /// A refusal that is transient *by construction*: this pass was turned away
    /// because a bounded shared resource was already fully held, and it is
    /// released by whoever holds it rather than by anything about this input.
    ///
    /// The background worker schedules its own delayed retry for exactly these,
    /// because releasing shared capacity emits no wake: a sibling worktree or
    /// artifact build finishing does not notify this worktree, so without a
    /// self-scheduled retry it stayed stale until an unrelated query or edit
    /// happened to wake it.
    ///
    /// The distinction the admission failure carries is the whole point. A
    /// request that exceeds the *entire* process limit is shaped like a
    /// capacity refusal and is not one — no other holder can release enough for
    /// it — so it is classified permanent and never self-retried. Identity
    /// failures, git and IO faults, production and privacy refusals, adjustment
    /// invariant breaks, an uninstalled worker plan, and publication conflicts
    /// likewise reproduce over the same input or already have an owner that
    /// re-drives them; self-scheduling those is precisely the unbounded-retry
    /// failure this module exists to stop.
    /// A measured over-budget refusal is transient by construction too, and for
    /// a stronger reason than a full reservation ledger: nothing about this
    /// input caused it, and the watermark that produced it clears on its own as
    /// real RSS falls back to the low watermark. Retrying is the only way the
    /// pass ever runs, because falling pressure emits no wake either.
    pub fn is_transient_capacity_failure(&self) -> bool {
        match self {
            Self::WorkerMemoryAdmission(failure) | Self::SnapshotMemoryAdmission(failure) => {
                failure.is_observed_over_budget()
                    || failure.requested_bytes() <= failure.limit_bytes()
            }
            Self::SnapshotMemoryCapacityUnavailable => true,
            Self::GraphProjection(CodeGraphProjectionError::BudgetExhausted { .. }) => true,
            _ => false,
        }
    }
}

/// Counts in-flight owner passes (retained activation or reconcile). A
/// counter rather than a flag so the background worker can hold the state
/// across an entire pass — claim of the pending wake through arrival restore —
/// while the scheduler's own entry points nest inside it without clearing the
/// in-progress signal early.
pub struct ReconcilePassGuard(Arc<AtomicUsize>);

impl ReconcilePassGuard {
    pub fn enter(passes: &Arc<AtomicUsize>) -> Self {
        passes.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(passes))
    }
}

impl Drop for ReconcilePassGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Binds the independently maintained source-freshness proof to one seated
/// generation. The freshness fence owns source proof and invalidation; this
/// witness prevents an unproven replacement seat from inheriting that proof.
#[derive(Clone, Debug)]
pub(crate) struct ServingSourceWitnessV1 {
    pub(crate) generation_id: CodeGenerationId,
}

#[derive(Clone)]
pub(crate) struct SourceFreshnessFenceV1 {
    state: Arc<Mutex<SourceFreshnessFenceStateV1>>,
    last_reconciled_at_micros: Arc<AtomicI64>,
    source_epoch: Arc<AtomicU64>,
}

#[derive(Clone)]
struct SourceFreshnessFenceStateV1 {
    git_metadata: identity::GitMetadataFingerprintV1,
    last_reconciled_at: Instant,
    last_stat_signature: Option<String>,
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    staleness_threshold: Duration,
    verified_against_source: bool,
    freshness_unknown: bool,
    reconciled_without_generation: bool,
    reconciled_source_epoch: u64,
    busy_witness_memo: Option<BusyWitnessMemoV1>,
}

#[derive(Clone)]
struct BusyWitnessMemoV1 {
    checked_at: Instant,
    verdict: bool,
}

impl SourceFreshnessFenceV1 {
    fn unverified(staleness_threshold: Duration, source_epoch: Arc<AtomicU64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SourceFreshnessFenceStateV1 {
                git_metadata: identity::GitMetadataFingerprintV1::default(),
                last_reconciled_at: Instant::now(),
                last_stat_signature: None,
                ignored_source_admissions: Vec::new(),
                staleness_threshold,
                verified_against_source: false,
                freshness_unknown: true,
                reconciled_without_generation: false,
                reconciled_source_epoch: 0,
                busy_witness_memo: None,
            })),
            last_reconciled_at_micros: Arc::new(AtomicI64::new(0)),
            source_epoch,
        }
    }

    fn snapshot(&self) -> SourceFreshnessFenceStateV1 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn mark_reconciled(
        &self,
        git_metadata: identity::GitMetadataFingerprintV1,
        stat_signature: Option<String>,
        ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
        reconciled_without_generation: bool,
    ) {
        let micros = now_micros().0;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.git_metadata = git_metadata;
        state.last_stat_signature = stat_signature;
        state.ignored_source_admissions = ignored_source_admissions.to_vec();
        state.freshness_unknown = false;
        state.last_reconciled_at = Instant::now();
        state.verified_against_source = true;
        state.reconciled_without_generation = reconciled_without_generation;
        state.reconciled_source_epoch = self.source_epoch.load(Ordering::Acquire);
        state.busy_witness_memo = None;
        self.last_reconciled_at_micros
            .store(micros, Ordering::Release);
    }

    fn refresh_monotonic_clock(&self, project_wall_clock: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.last_reconciled_at = Instant::now();
        if project_wall_clock {
            self.last_reconciled_at_micros
                .store(now_micros().0, Ordering::Release);
        }
    }

    fn last_reconciled_at_micros(&self) -> Option<i64> {
        match self.last_reconciled_at_micros.load(Ordering::Acquire) {
            0 => None,
            micros => Some(micros),
        }
    }

    fn reconciled_without_generation(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reconciled_without_generation
    }

    fn ready_without_stat(&self, project_root: &Path, shutting_down: &AtomicBool) -> bool {
        if shutting_down.load(Ordering::Acquire) {
            return false;
        }
        let state = self.snapshot();
        state.verified_against_source
            && self.source_epoch.load(Ordering::Acquire) == state.reconciled_source_epoch
            && !identity::GitMetadataFingerprintV1::capture(project_root)
                .differs_from(&state.git_metadata)
            && state.last_reconciled_at.elapsed() < state.staleness_threshold
    }

    fn exact_source_is_ready(&self, project_root: &Path, shutting_down: &AtomicBool) -> bool {
        if shutting_down.load(Ordering::Acquire) {
            return false;
        }
        let state = self.snapshot();
        if state.freshness_unknown
            || self.source_epoch.load(Ordering::Acquire) != state.reconciled_source_epoch
            || identity::GitMetadataFingerprintV1::capture(project_root)
                .differs_from(&state.git_metadata)
        {
            return false;
        }
        if let Some(memo) = state.busy_witness_memo
            && memo.checked_at.elapsed() < BUSY_WITNESS_MEMO_INTERVAL
        {
            return memo.verdict;
        }
        let matches = freshness_witness::worktree_stat_signature_for(
            project_root,
            &state.ignored_source_admissions,
        )
        .is_ok_and(|signature| state.last_stat_signature.as_ref() == Some(&signature));
        let source_still_matches = self.source_epoch.load(Ordering::Acquire)
            == state.reconciled_source_epoch
            && !identity::GitMetadataFingerprintV1::capture(project_root)
                .differs_from(&state.git_metadata);
        let mut current = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if source_still_matches
            && current.reconciled_source_epoch == state.reconciled_source_epoch
            && current.git_metadata == state.git_metadata
        {
            current.busy_witness_memo = Some(BusyWitnessMemoV1 {
                checked_at: Instant::now(),
                verdict: matches,
            });
        }
        drop(current);
        if matches && source_still_matches {
            self.refresh_monotonic_clock(false);
        }
        matches && source_still_matches
    }

    fn source_currency_witness_for(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Option<ServingSourceWitnessV1> {
        let state = self.snapshot();
        if !state.verified_against_source || state.last_stat_signature.is_none() {
            return None;
        }
        Some(ServingSourceWitnessV1 {
            generation_id: generation_id.clone(),
        })
    }
}

pub struct CodeIndexWorktreeSchedulerV1 {
    project_id: ProjectId,
    project_root: PathBuf,
    /// Scoped store root for this worktree's sealed generations. Also holds the
    /// restore-time freshness witness sidecar used to skip a redundant cold
    /// reconcile when the on-disk source still equals the restored generation.
    store_root: PathBuf,
    /// The exact indexing identity this worktree is bound to. Re-resolved
    /// before each reconciliation so a HEAD move never mis-attributes a served
    /// generation to a newer revision.
    identity: identity::IndexingIdentityV1,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    policy: CodeIndexHintPolicyV1,
    /// Independent source-freshness authority. Ready/status probes clone this
    /// handle from the mounted map and never wait for scheduler build state.
    freshness_fence: SourceFreshnessFenceV1,
    byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    /// Keeps the current snapshot's interned bytes alive in the shared pool.
    retained_snapshot_bytes: Vec<Arc<[u8]>>,
    /// Holds the measured source-byte charges for
    /// `retained_snapshot_bytes`; worker scratch is admitted separately only
    /// after capture has completed.
    _retained_snapshot_memory: Vec<ResidentMemoryReservationV1>,
    /// Deterministic reconcile fault used only by the worker-loop isolation
    /// tests; production never installs one.
    #[cfg(test)]
    reconcile_fault: Option<Arc<reconcile_panic_guard::ReconcileFaultInjectionV1>>,
    /// Process resident-memory authority artifact builds and readers reserve
    /// through. Standalone opens get a private default-limit authority; the
    /// registry rebinds its shared process authority at mount.
    resident_memory: Arc<ProcessResidentMemoryV1>,
    publication: DaemonCodeIndexPublicationStoreV1,
    production_config: CodeIndexProductionConfigV1,
    owner: ProductionOwner,
    hints: Arc<Mutex<PendingHintsV1>>,
    /// gix "unchanged" is relative to the index, while active rows may have
    /// been captured from dirty content, so the exact snapshot identity keeps
    /// those paths excluded from reuse after they are reverted.
    active_snapshot_changed_paths: Mutex<Option<(ContentDigest, BTreeSet<String>)>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    /// Number of in-flight owner passes; nonzero means activation or
    /// reconcile work is running for this worktree.
    reconcile_in_progress: Arc<AtomicUsize>,
    /// Typed owner-configuration recovery independently readable while a
    /// replacement generation is building.
    generation_recovery: Arc<RwLock<Option<CodeIndexGenerationRecoveryV1>>>,
    latest_content_identity: Option<ContentDigest>,
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    /// Set when a retained generation was refused because its ignored-source
    /// roster no longer verifies. That refusal also clears the roster, so the
    /// next pass rebuilds without it — but the refused pass has already
    /// consumed the wake that ran it, so nothing scheduled that next pass and
    /// the still-pending source stayed uncaptured with no seat at all. The
    /// worker takes this flag to re-arm exactly one pass; taking it clears it,
    /// so a refusal can never spin the worker.
    ignored_roster_refusal_requires_rebuild: bool,
    query_owners: ProfiledStdMutex<Option<GenerationServingCachesV1>>,
    /// Immutable generation-scoped build snapshot. The registry clones this
    /// slot at mount so dashboard reads never acquire the scheduler mutex.
    build_progress: CodeIndexBuildProgressSlotV1,
    /// Durable daemon-authority epoch bound by the process registry at mount.
    progress_daemon_incarnation: u64,
    /// Registry-minted scheduler-owner token. A same-daemon retire/remount gets
    /// a strictly newer token so delayed progress cannot outrank the new owner.
    progress_producer_incarnation: u64,
    /// Optional semantic hook: schedule `FastEmbed` projection without joining it.
    semantic_schedule:
        Option<tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
}

/// Immutable authority for historical-generation reads and their detached
/// query derivations.
///
/// The mounted registry retains this separately from the mutable scheduler so
/// already-sealed Git revisions remain readable while reconcile owns the
/// scheduler mutex. Its generation-local caches never replace the active
/// generation's text, progress, record-index, or graph owners.
#[derive(Clone)]
pub struct HistoricalCodeIndexGenerationOwnerV1 {
    publication: DaemonCodeIndexPublicationStoreV1,
    store_root: PathBuf,
    resident_memory: Arc<ProcessResidentMemoryV1>,
    project_id: ProjectId,
    worktree_id: WorktreeId,
    shutting_down: Arc<AtomicBool>,
    progress_daemon_incarnation: u64,
    progress_producer_incarnation: u64,
}

impl HistoricalCodeIndexGenerationOwnerV1 {
    fn bind_complete(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> LatestCompleteCodeIndexV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let mut progress_slot = CodeIndexBuildProgressSlotStateV1::default();
        let text_progress_owner_epoch = progress_slot.replace_generation(generation_id.clone());
        let metadata = Arc::new(
            VerifiedSealedTextGenerationMetadataV1::from_published_generation(&generation),
        );
        let latest = LatestCompleteCodeIndexV1 {
            generation,
            text: LatestCodeTextGenerationV1 {
                metadata,
                sealed_format_revision:
                    tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
                query_owners: Arc::new(OnceLock::new()),
                graph_activation: Arc::new(RwLock::new(CodeGraphActivationStateV1::Pending)),
                text_projection_build: Arc::new(CodeTextProjectionStateV1::new()),
                text_projection_failed: Arc::new(AtomicBool::new(false)),
                text_control: GenerationTextControlV1::new(Arc::clone(&self.shutting_down)),
                text_progress_state: Arc::new(hotpath::mutex!(
                    Mutex::new(CodeIndexBuildProgressStateV1::new()),
                    label = "query.artifact.progress.historical_state"
                )),
                text_progress_slot: Arc::new(RwLock::new(progress_slot)),
                text_progress_owner_epoch,
                text_progress_daemon_incarnation: self.progress_daemon_incarnation,
                text_progress_producer_incarnation: self.progress_producer_incarnation,
                text_artifact_store: DaemonCodeTextArtifactStoreV1::bind(
                    &self.store_root,
                    &self.publication,
                    &self.resident_memory,
                    &self.project_id,
                    &self.worktree_id,
                ),
                preopened_source: Arc::new(hotpath::mutex!(
                    Mutex::new(None),
                    label = "query.artifact.preopened_historical_source"
                )),
                publication_binding: None,
            },
            record_index: Arc::new(OnceLock::new()),
        };
        if latest
            .text
            .text_artifact_store
            .published_descriptor(&generation_id)
            .ok()
            .flatten()
            .is_some()
        {
            let control = latest.text_execution_control();
            let _ = latest.open_published_head_or_begin_build(&control);
        }
        latest
    }

    /// Load an exact durable code generation even after a newer capture has
    /// superseded every process-local retained slot.
    #[hotpath::measure(label = "daemon.code_index.historical.published_generation")]
    pub(crate) fn published_generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexSchedulerErrorV1> {
        self.publication
            .load_generation(generation_id)
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }

    /// Resolve the sealed replay binding for one already-published generation
    /// through the retained publication clone. A sealed binding is an
    /// immutable pointer-file read, so it stays answerable while a reconcile
    /// owns the scheduler mutex for its whole pass.
    #[hotpath::measure(label = "daemon.code_index.historical.replay_binding")]
    pub(crate) fn sealed_replay_binding(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1> {
        self.publication
            .sealed_replay_binding(generation_id)
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }

    fn active_publication_covers(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        if self
            .publication
            .active_pointer_matches_generation(generation)
            .map_err(CodeIndexProductionErrorV1::Publication)?
        {
            return Ok(true);
        }
        self.publication
            .active_pointer_covers_snapshot_content(&generation.snapshot().content_identity)
            .map_err(CodeIndexProductionErrorV1::Publication)
            .map_err(Into::into)
    }
}

impl CodeIndexWorktreeSchedulerV1 {
    pub fn open(
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        Self::open_with_policy(
            project_id,
            project_root,
            store_root,
            byte_pool,
            CodeIndexHintPolicyV1::default(),
        )
    }

    pub fn open_with_policy(
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
        policy: CodeIndexHintPolicyV1,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        // Resolve exact identity BEFORE any indexing work. Paths located this
        // checkout; identity authorizes what may be reused.
        let identity = identity::IndexingIdentityV1::resolve(&project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let repository_id = identity.repository_id().clone();
        let worktree_id = identity.worktree_id().clone();
        // Cold open establishes structural identity only. Repository-wide
        // freshness probes and sealed-generation decoding belong to the
        // retained background owner after the route is mounted.
        let sanitizer_revision = id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)?;
        let publication = DaemonCodeIndexPublicationStoreV1::new(
            &store_root,
            &project_root,
            sanitizer_revision.clone(),
        )?;
        let production_config = CodeIndexProductionConfigV1 {
            project_id: project_id.clone(),
            repository: repository_id.clone(),
            sanitizer_revision,
            policy_revision: id::<PolicyRevisionId>("policy.daemon.v1")?,
            // V3 retains unresolved per-file references and derives
            // conservative cross-file edges at generation sealing. V2
            // artifacts remain decodable, but cannot be reused as a current
            // graph because they never recorded that evidence.
            chunker_revision: id::<ChunkerRevision>(DAEMON_CODE_INDEX_CHUNKER_REVISION)?,
            privacy_domain: id::<PrivacyDomainId>("privacy.local-code-index")?,
            privacy_key_epoch: 1,
            max_snapshot_age_micros: None,
        };
        let owner = open_production_code_index_owner_v1(
            production_config.clone(),
            publication.clone(),
            DaemonProjectionSinkV1,
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::ProductionOpen(error.to_string()))?
        .with_physical_artifact_pool(byte_pool.physical_artifacts.clone());
        let latest_content_identity = None;
        let hints = Arc::new(Mutex::new(PendingHintsV1::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let epoch = Arc::new(AtomicU64::new(0));
        let freshness_fence =
            SourceFreshnessFenceV1::unverified(policy.staleness_threshold, Arc::clone(&epoch));
        // Nothing is decoded or served until the retained owner proves the
        // durable generation belongs to this exact identity and its freshness
        // frontier still matches the worktree.
        let scheduler = Self {
            project_id,
            project_root,
            store_root,
            identity,
            repository_id,
            worktree_id,
            policy,
            freshness_fence,
            byte_pool,
            retained_snapshot_bytes: Vec::new(),
            _retained_snapshot_memory: Vec::new(),
            #[cfg(test)]
            reconcile_fault: None,
            resident_memory: Arc::new(ProcessResidentMemoryV1::new(
                detected_process_resident_memory_limit_v1(),
            )),
            publication,
            production_config,
            owner,
            hints,
            active_snapshot_changed_paths: Mutex::new(None),
            wake,
            epoch,
            shutting_down: Arc::new(AtomicBool::new(false)),
            reconcile_in_progress: Arc::new(AtomicUsize::new(0)),
            generation_recovery: Arc::new(RwLock::new(None)),
            latest_content_identity,
            ignored_source_admissions: Vec::new(),
            ignored_roster_refusal_requires_rebuild: false,
            query_owners: hotpath::mutex!(
                Mutex::new(None),
                label = "daemon.code_index.serving_caches"
            ),
            build_progress: Arc::new(RwLock::new(CodeIndexBuildProgressSlotStateV1::default())),
            progress_daemon_incarnation: 1,
            progress_producer_incarnation: 1,
            semantic_schedule: None,
        };
        Ok(scheduler)
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    /// Rebind the shared process resident-memory authority at mount so every
    /// artifact ceiling reserves against the one process ceiling instead of
    /// this scheduler's private standalone authority.
    pub fn bind_resident_memory(&mut self, resident_memory: Arc<ProcessResidentMemoryV1>) {
        self.resident_memory = resident_memory;
    }

    pub fn bind_progress_incarnations(
        &mut self,
        daemon_incarnation: u64,
        producer_incarnation: u64,
    ) {
        self.progress_daemon_incarnation = daemon_incarnation.max(1);
        self.progress_producer_incarnation = producer_incarnation.max(1);
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub const fn progress_incarnations_for_test(&self) -> (u64, u64) {
        (
            self.progress_daemon_incarnation,
            self.progress_producer_incarnation,
        )
    }

    pub fn build_progress_slot(&self) -> CodeIndexBuildProgressSlotV1 {
        Arc::clone(&self.build_progress)
    }

    pub fn last_reconciled_at_micros_slot(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.freshness_fence.last_reconciled_at_micros)
    }

    pub(crate) fn freshness_fence(&self) -> SourceFreshnessFenceV1 {
        self.freshness_fence.clone()
    }

    pub fn historical_generation_owner(&self) -> HistoricalCodeIndexGenerationOwnerV1 {
        HistoricalCodeIndexGenerationOwnerV1 {
            publication: self.publication.clone(),
            store_root: self.store_root.clone(),
            resident_memory: Arc::clone(&self.resident_memory),
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
            shutting_down: Arc::clone(&self.shutting_down),
            progress_daemon_incarnation: self.progress_daemon_incarnation,
            progress_producer_incarnation: self.progress_producer_incarnation,
        }
    }

    /// Reserve the installed worker plan on the canonical process authority.
    /// The returned RAII guard spans source capture and the complete production
    /// build, releasing on success, typed failure, cancellation, or unwind.
    fn reserve_worker_memory(
        &self,
    ) -> Result<ResidentMemoryReservationV1, CodeIndexSchedulerErrorV1> {
        self.ensure_worker_plan()?;
        let planned_workers = tracedecay_code_index::parallelism::indexing_workers();
        let snapshot = self.resident_memory.snapshot();
        let remaining = snapshot.limit_bytes.saturating_sub(snapshot.used_bytes);
        // The process-global worker plan may have been installed against a
        // larger authority (standalone seed using detected host RAM). This
        // scheduler's remaining bytes are a different authority: the 6 GiB
        // default still has to leave the typed non-worker headroom that
        // `memory_safe_worker_count` already models. Capping to
        // `remaining / 128MiB` spends that headroom as extra workers, so a
        // later 31-byte snapshot charge sees used==limit. A remainder that
        // cannot admit one memory-safe worker still requests one so
        // admission produces the canonical denial.
        let affordable_workers =
            tracedecay_code_index::parallelism::memory_safe_worker_count(remaining);
        let workers = planned_workers.min(affordable_workers);
        let requested_bytes = NonZeroU64::new(
            tracedecay_code_index::parallelism::worker_reservation_bytes(workers.max(1)),
        )
        .ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "code-index worker resident-memory reservation must be nonzero".to_owned(),
            )
        })?;
        let component = ResidentMemoryComponentIdV1::new(CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let generation_id = CodeGenerationId::new("code-index-worker-active")
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        self.resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id,
                    component,
                },
                requested_bytes,
            )
            .map_err(CodeIndexSchedulerErrorV1::from)
    }

    /// Incremental graph-off rebuilds still go through canonical admission,
    /// but they do not need the process-global full-width worker slab. That
    /// slab was planned against host RAM; asking for it on a 6 GiB test
    /// authority (or any process already over observed RSS) refuses a
    /// changed-source publish that only needs capture scratch. The pressure
    /// floor is the largest request admitted while over budget; a 1 MiB
    /// authority still denies it.
    fn reserve_incremental_rebuild_memory(
        &self,
    ) -> Result<ResidentMemoryReservationV1, CodeIndexSchedulerErrorV1> {
        let requested_bytes = NonZeroU64::new(RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1)
            .ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "incremental rebuild resident-memory reservation must be nonzero".to_owned(),
                )
            })?;
        let component = ResidentMemoryComponentIdV1::new(CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let generation_id = CodeGenerationId::new("code-index-worker-active")
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        self.resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id,
                    component,
                },
                requested_bytes,
            )
            .map_err(CodeIndexSchedulerErrorV1::from)
    }

    fn reserve_snapshot_memory(
        &self,
        content_digest: &ContentDigest,
        retained_bytes: usize,
    ) -> Result<Option<ResidentMemoryReservationV1>, CodeIndexSchedulerErrorV1> {
        let Some(requested_bytes) = u64::try_from(retained_bytes).ok().and_then(NonZeroU64::new)
        else {
            if retained_bytes == 0 {
                return Ok(None);
            }
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index captured source charge exceeds u64".to_owned(),
            ));
        };
        let component = ResidentMemoryComponentIdV1::new("code_index.snapshot.source_bytes.v1")
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let generation_id = CodeGenerationId::new(format!(
            "code-index-snapshot-source.{}",
            content_digest.as_str()
        ))
        .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        self.resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id,
                    component,
                },
                requested_bytes,
            )
            .map(Some)
            .map_err(CodeIndexSchedulerErrorV1::SnapshotMemoryAdmission)
    }

    fn finish_snapshot_build_memory(
        _reservations: &mut [ResidentMemoryReservationV1],
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        Ok(())
    }

    fn ensure_worker_plan(&self) -> Result<(), CodeIndexSchedulerErrorV1> {
        if tracedecay_code_index::parallelism::installed_worker_status().is_some() {
            return Ok(());
        }
        // The shared scheduler test sources also compile into the composition
        // root's test binary, where this crate is a dependency built with
        // `test-helpers` instead of `cfg(test)`; both spellings are the same
        // fixture surface, so the auto-install fallback must cover both.
        #[cfg(any(test, feature = "test-helpers"))]
        {
            let snapshot = self.resident_memory.snapshot();
            tracedecay_code_index::parallelism::install_worker_plan(
                tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::Automatic {},
                snapshot.limit_bytes.saturating_sub(snapshot.used_bytes),
            )?;
            Ok(())
        }
        #[cfg(not(any(test, feature = "test-helpers")))]
        {
            Err(CodeIndexSchedulerErrorV1::WorkerPlanNotInstalled)
        }
    }

    /// Replace the semantic `schedule_generation` hook on mount/remount. The hook
    /// must return immediately; `FastEmbed` download/indexing never blocks
    /// exact/lexical/graph search. `None` retires a stale runtime.
    pub fn replace_semantic_schedule_hook(
        &mut self,
        hook: Option<tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
    ) {
        self.semantic_schedule = hook;
    }

    /// Schedule semantics only after the registry has activated and published
    /// this exact generation as serving state.
    pub fn schedule_semantic_generation(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> bool {
        let Some(schedule) = self.semantic_schedule.as_ref() else {
            return false;
        };
        let generation_id = generation.manifest().generation_id.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            hotpath::measure_block!(
                "code_index.semantic_generation_handoff",
                schedule(generation)
            )
        })) {
            Ok(scheduled) => scheduled,
            Err(_) => {
                tracing::warn!(
                    event = "code_index_semantic_schedule_panicked",
                    generation = %generation_id,
                    "code-index semantic scheduling panicked; the generation remains serving"
                );
                false
            }
        }
    }

    #[cfg(test)]
    pub fn notify_path(&self, path: PathBuf) {
        self.hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path(path);
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

    #[cfg(test)]
    pub fn notify_overflow(&self) {
        self.hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

    /// Ask the retained owner for a background pass without claiming that the
    /// worktree moved.
    ///
    /// The epoch is the cancellation authority every admitted index and text
    /// pass carries, so advancing it here cancelled bounded text projection
    /// already admitted for the current generation on every *source-neutral*
    /// wake: an incompatible-generation observation, a dirty-remount seat, an
    /// elapsed staleness tier, a retained-frontier decline. None of those
    /// observed a source change, and none of them may supersede work bound to
    /// source state that is still current.
    pub fn request_background_reconcile(&self) {
        self.request_background_reconcile_with_change(false);
    }

    /// [`Self::request_background_reconcile`] for a caller that has already
    /// proven the worktree moved.
    ///
    /// The first pending observed-change marker advances the canonical
    /// worktree-change generation diagnostics caches key on, and supersedes
    /// index work bound to the source state that change replaced. Freshness
    /// requests coalesce until reconciliation drains the marker through
    /// `take()`, so a repeated read of the same pending drift keeps the same
    /// generation. Source-neutral overflow wakes use a separate marker and
    /// cannot consume this transition.
    pub fn request_background_reconcile_for_observed_change(&self) {
        self.request_background_reconcile_with_change(true);
    }

    fn request_background_reconcile_with_change(&self, source_changed: bool) {
        {
            let mut hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let newly_observed_change = source_changed && !hints.observed_source_change;
            hints.observed_source_change |= source_changed;
            if newly_observed_change {
                // Advance while holding the hint authority: a reconciler that
                // drains the observed-change marker must also observe the
                // cancellation epoch that marker minted.
                DaemonCodeIndexControlV1::advance(&self.epoch);
            }
            hints.overflow();
        }
        // `Notify` already coalesces stored permits. Always refresh the permit:
        // a prior worker may have consumed its wake and then failed before
        // draining this overflow marker.
        self.wake.notify_one();
    }

    #[hotpath::measure(label = "code_index.generation.compatibility_observe")]
    fn observe_generation_compatibility(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let compatibility = generation.compatibility_with(&self.production_config);
        let next = if compatibility.is_reusable() {
            None
        } else {
            Some(CodeIndexGenerationRecoveryV1 {
                incompatible_generation_id: generation.manifest().generation_id.as_str().to_owned(),
                incompatibilities: compatibility
                    .incompatibilities()
                    .iter()
                    .map(|reason| reason.as_str().to_owned())
                    .collect(),
                serving: if compatibility.may_serve_while_rebuilding() {
                    CodeIndexGenerationRecoveryServingV1::Preserved
                } else {
                    CodeIndexGenerationRecoveryServingV1::Refused
                },
            })
        };
        let mut observed = self
            .generation_recovery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *observed != next {
            match next.as_ref() {
                Some(recovery) => tracing::warn!(
                    event = "code_index_generation_configuration_incompatible",
                    generation_id = recovery.incompatible_generation_id,
                    incompatibilities = recovery.incompatibilities.join(","),
                    serving = ?recovery.serving,
                    "the active generation is retired from reuse; one compatible replacement is scheduled"
                ),
                None if observed.is_some() => tracing::info!(
                    event = "code_index_generation_configuration_recovered",
                    generation_id = %generation.manifest().generation_id,
                    "the compatible replacement generation is now active"
                ),
                None => {}
            }
            *observed = next;
        }
        compatibility
    }

    fn validate_generation_identity(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let snapshot = generation.snapshot();
        if generation.manifest().project_id != self.project_id
            || snapshot.repository != self.repository_id
            || snapshot.worktree.as_ref() != Some(&self.worktree_id)
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "active code generation belongs to a different project/worktree identity"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// Activate a retained sealed generation only after its durable freshness
    /// frontier proves that the exact worktree state it describes is unchanged.
    ///
    /// This is deliberately background-only: sealed decode, gix status/index
    /// classification, and the source stat sweep are all repository-sized.
    /// Missing, corrupt, or mismatched frontier evidence simply declines the
    /// fast path so the same retained owner performs authoritative reconcile.
    fn activate_retained_generation_from_frontier(
        &mut self,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if hints.overflow || !hints.paths.is_empty() {
                return Ok(None);
            }
        }
        // Cheap witness/stat checks run before any decode. A dirty remount
        // (cancelled mid-batch, uncommitted files) used to join
        // `load_active_shared` under the scheduler lock; when activation
        // already owned that barrier the worker parked until the 45s
        // remount wait expired with `last_reconcile_micros` unset.
        let Some(pointer) = self
            .publication
            .read_publication_pointer()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        if !self.retained_frontier_is_quietly_current(&pointer) {
            return Ok(None);
        }
        let Some(generation) = self
            .publication
            .active_already_decoded()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        self.validate_generation_identity(&generation)?;
        if !self
            .observe_generation_compatibility(&generation)
            .is_reusable()
        {
            return Ok(None);
        }
        self.adopt_ignored_source_roster(&generation);
        let Some(witness) = RestoreFreshnessWitnessV1::load(&self.store_root) else {
            return Ok(None);
        };
        if witness.generation_id != generation.manifest().generation_id.as_str() {
            return Ok(None);
        }
        let ignored_source_paths = generation
            .ignored_source_admissions()
            .iter()
            .map(|admission| admission.logical_path.clone())
            .collect::<Vec<_>>();
        if !ignored_source_paths.is_empty()
            && (generation.repository_parse_identity().dirty != RepositoryDirtyStateV1::Dirty
                || generation.snapshot().source_revision.is_some())
        {
            return Ok(None);
        }
        let Ok(repository_parse_identity_digest) =
            canonical_sha256(generation.repository_parse_identity())
        else {
            return Ok(None);
        };
        if witness.ignored_source_admissions_digest
            != generation.ignored_source_admissions_digest().as_str()
            || witness.repository_parse_identity_digest != repository_parse_identity_digest.as_str()
            || witness.ignored_source_paths != ignored_source_paths
            || !self.ignored_source_roster_matches_generation(&generation)
        {
            return Ok(None);
        }
        let metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        if witness.git_metadata_signature != metadata.stable_signature() {
            return Ok(None);
        }
        let Ok(stat_signature) = self.worktree_stat_signature() else {
            return Ok(None);
        };
        if witness.stat_signature != stat_signature {
            return Ok(None);
        }
        let snapshot_content_identity = generation.snapshot().content_identity.clone();
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        self.mark_reconciled();
        Ok(Some(CodeIndexReconcileOutcomeV1::Noop(
            CodeIndexNoopEvidenceV1 {
                snapshot_content_identity,
                overflow_reconciled: false,
            },
        )))
    }

    /// Seat the already-sealed active generation when the serving slot is empty.
    ///
    /// Quiet remounts persist a freshness witness through
    /// [`Self::activate_retained_generation_from_frontier`]. A dirty remount
    /// (cancelled mid-batch, uncommitted files, overflow) fails that witness
    /// check, and falling through into a successor rebuild left restart
    /// remounts warming with `last_reconcile_micros` unset and no
    /// `code_index_serving_generation_seated` event. Historical convergence and
    /// the dirty-tree rebuild stay later passes; this pass only makes the
    /// retained artifact serve.
    ///
    /// Does not claim source freshness or persist a restore witness. It leaves
    /// an overflow wake behind so the next pass must still observe the dirty
    /// tree and publish its successor.
    fn seat_retained_generation_on_empty_serving(
        &mut self,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        if let Some(outcome) = self.activate_retained_generation_from_frontier()? {
            return Ok(Some(outcome));
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let Some(pointer) = self
            .publication
            .read_publication_pointer()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        let decoded = self
            .publication
            .active_already_decoded()
            .map_err(CodeIndexProductionErrorV1::Publication)?;
        let configuration_changed = if let Some(generation) = decoded.as_ref() {
            self.validate_generation_identity(generation)?;
            let compatibility = self.observe_generation_compatibility(generation);
            if !compatibility.may_serve_while_rebuilding() {
                self.request_background_reconcile();
                return Ok(None);
            }
            !compatibility.is_reusable()
        } else {
            false
        };
        let dirty = {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hints.overflow || !hints.paths.is_empty()
        } || configuration_changed
            || !self.retained_frontier_is_quietly_current(&pointer);
        if dirty {
            self.request_background_reconcile();
        }
        // Graph prepare decodes without the scheduler mutex. Joining
        // `load_active_shared` here parked remount on the publication
        // barrier while activation owned it, so the seated event never
        // published and the dirty successor extract never started.
        let snapshot_content_identity = if let Some(generation) = decoded {
            self.adopt_ignored_source_roster(&generation);
            generation.snapshot().content_identity.clone()
        } else {
            ContentDigest::new(pointer.snapshot_content_identity.clone())
                .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?
        };
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        Ok(Some(CodeIndexReconcileOutcomeV1::Noop(
            CodeIndexNoopEvidenceV1 {
                snapshot_content_identity,
                overflow_reconciled: false,
            },
        )))
    }

    /// Witness + git/stat fence that does not read sealed generation bytes.
    fn retained_frontier_is_quietly_current(&self, pointer: &DurablePublicationPointerV1) -> bool {
        let Some(witness) = RestoreFreshnessWitnessV1::load(&self.store_root) else {
            return false;
        };
        if witness.generation_id != pointer.generation_id {
            return false;
        }
        let metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        if witness.git_metadata_signature != metadata.stable_signature() {
            return false;
        }
        self.worktree_stat_signature()
            .is_ok_and(|signature| witness.stat_signature == signature)
    }

    #[cfg(test)]
    pub fn seat_retained_generation_on_empty_serving_for_test(
        &mut self,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        self.seat_retained_generation_on_empty_serving()
    }

    /// Verify an unchanged retained text generation without decoding the full
    /// graph-bearing generation.
    ///
    /// Graph-off mounts already authenticated the complete sealed bytes while
    /// opening their lexical page source. For an ordinary source roster, the
    /// durable freshness witness proves a quiet mount; an explicit hint is
    /// settled by one authoritative capture. An unchanged capture keeps the
    /// retained generation, while a changed ordinary source is rebuilt from
    /// that capture under an exact durable-pointer compare-and-swap. This path
    /// never decodes the graph-bearing active generation. Ignored-source
    /// rosters still require the complete reconcile path.
    pub fn republish_unpublished_retained_generation(
        &mut self,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        let Some(pointer) = self
            .publication
            .read_publication_pointer()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        let Some(pending) = self.publication.take_unpublished() else {
            return Ok(None);
        };
        let resolved = match identity::IndexingIdentityV1::resolve(&self.project_root) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.publication.restore_unpublished(pending);
                return Err(CodeIndexSchedulerErrorV1::Identity(error.to_string()));
            }
        };
        if !resolved.authorizes_reuse_of(&self.identity) {
            self.publication.restore_unpublished(pending);
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "worktree identity changed under the scheduler".to_owned(),
            ));
        }
        self.identity = resolved;
        let _worker_memory = match self.reserve_incremental_rebuild_memory() {
            Ok(reservation) => reservation,
            Err(error) => {
                self.publication.restore_unpublished(pending);
                return Err(error);
            }
        };
        let capture = match self.capture_retained_reconcile_attempt() {
            Ok(Some(capture)) => capture,
            Ok(None) => {
                self.publication.restore_unpublished(pending);
                return Ok(None);
            }
            Err(error) => {
                self.publication.restore_unpublished(pending);
                return Err(error);
            }
        };
        let RetainedReconcileCaptureV1 {
            mut captured,
            drained_hints,
            control,
            git_metadata,
            stat_signature,
        } = capture;
        if pending.snapshot().reference != captured.snapshot.reference
            || pending.snapshot().source_revision != captured.snapshot.source_revision
            || pending.snapshot().content_identity != captured.snapshot.content_identity
        {
            return Ok(None);
        }
        let scope = CodeIndexGenerationScopeV1::for_snapshot(pending.snapshot());
        let mut publication = self
            .publication
            .for_undecoded_active_rebuild(&pointer)
            .with_reconcile_publication_fence(Arc::clone(&self.hints), control);
        publication
            .publish_atomically(&scope, None, Arc::clone(&pending))
            .map_err(CodeIndexProductionErrorV1::Publication)?;
        Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
        self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
        self._retained_snapshot_memory = std::mem::take(&mut captured.retained_reservations);
        let snapshot_content_identity = pending.snapshot().content_identity.clone();
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        self.mark_reconciled_state(git_metadata.clone(), stat_signature.clone());
        let repository_parse_identity_digest =
            canonical_sha256(pending.repository_parse_identity())
                .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        if let Some(stat_signature) = stat_signature {
            RestoreFreshnessWitnessV1 {
                generation_id: pending.manifest().generation_id.as_str().to_owned(),
                git_metadata_signature: git_metadata.stable_signature(),
                stat_signature,
                repository_parse_identity_digest: repository_parse_identity_digest
                    .as_str()
                    .to_owned(),
                ignored_source_admissions_digest: pending
                    .ignored_source_admissions_digest()
                    .as_str()
                    .to_owned(),
                ignored_source_paths: Vec::new(),
            }
            .persist(&self.store_root);
        }
        let changes = &pending.projection().request().changes;
        let lane_digest = canonical_sha256(&(
            pending.snapshot().content_identity.clone(),
            pending
                .chunks()
                .chunks()
                .iter()
                .map(|chunk| (&chunk.id, &chunk.content_digest))
                .collect::<Vec<_>>(),
            pending.edges(),
        ))
        .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let outcome = CodeIndexReconcileOutcomeV1::Published(CodeIndexPublishEvidenceV1 {
            generation_id: pending.manifest().generation_id.clone(),
            repository_id: self.repository_id.clone(),
            snapshot_content_identity,
            lane_digest,
            file_occurrence_ids: pending
                .snapshot()
                .files
                .iter()
                .map(|file| file.file_occurrence_id.clone())
                .collect(),
            reextracted_files: 0,
            changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
            reused_chunks: changes.reused.len(),
            overflow_reconciled: drained_hints.overflow(),
        });
        drained_hints.commit();
        Ok(Some(outcome))
    }

    fn reconcile_retained_text_generation_with(
        &mut self,
        metadata: &VerifiedSealedTextGenerationMetadataV1,
        rebuild_changed_source_without_decode: bool,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        if let Some(outcome) = self.republish_unpublished_retained_generation()? {
            return Ok(Some(outcome));
        }
        if metadata.manifest().project_id != self.project_id
            || metadata.snapshot().repository != self.repository_id
            || metadata.snapshot().worktree.as_ref() != Some(&self.worktree_id)
        {
            return Ok(None);
        }
        let witness = RestoreFreshnessWitnessV1::load(&self.store_root);
        if witness.as_ref().is_some_and(|witness| {
            witness.generation_id != metadata.manifest().generation_id.as_str()
                || !witness.ignored_source_paths.is_empty()
        }) || !self.ignored_source_admissions.is_empty()
        {
            return Ok(None);
        }
        let resolved = identity::IndexingIdentityV1::resolve(&self.project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        if !resolved.authorizes_reuse_of(&self.identity) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "worktree identity changed under the scheduler".to_owned(),
            ));
        }
        self.identity = resolved;
        let sampled_metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let Some(sampled_signature) = self.worktree_stat_signature().ok() else {
            return Ok(None);
        };
        let has_hints = {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hints.overflow || !hints.paths.is_empty()
        };
        if !has_hints
            && let Some(witness) = witness.as_ref()
            && witness.git_metadata_signature == sampled_metadata.stable_signature()
            && witness.stat_signature == sampled_signature
        {
            let snapshot_content_identity = metadata.snapshot().content_identity.clone();
            self.latest_content_identity = Some(snapshot_content_identity.clone());
            self.mark_reconciled_retained_generation_state(
                sampled_metadata,
                Some(sampled_signature),
            );
            return Ok(Some(CodeIndexReconcileOutcomeV1::Noop(
                CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity,
                    overflow_reconciled: false,
                },
            )));
        }

        // Witness did not prove a quiet tree. Graph-on remounts fall through
        // to `reconcile_now` so the successor rebuild reuses the sealed
        // generation instead of extracting the whole worktree on this path.
        if !rebuild_changed_source_without_decode {
            return Ok(None);
        }

        let _worker_memory = self.reserve_incremental_rebuild_memory()?;
        let Some(capture) = self.capture_retained_reconcile_attempt()? else {
            return Ok(None);
        };
        self.finish_retained_reconcile(metadata, witness, capture)
    }

    fn capture_retained_reconcile_attempt(
        &self,
    ) -> Result<Option<RetainedReconcileCaptureV1>, CodeIndexSchedulerErrorV1> {
        let control =
            DaemonCodeIndexControlV1::new(Arc::clone(&self.epoch), Arc::clone(&self.shutting_down));
        let captured =
            self.capture_authoritative_snapshot_without_active_generation_reuse(Some(&control))?;
        let git_metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let stat_signature = self.worktree_stat_signature().ok();
        let drained_hints = {
            let mut hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if control.is_cancelled() {
                return Ok(None);
            }
            DrainedPendingHintsV1::new(Arc::clone(&self.hints), hints.take())
        };
        Ok(Some(RetainedReconcileCaptureV1 {
            captured,
            drained_hints,
            control,
            git_metadata,
            stat_signature,
        }))
    }

    fn finish_retained_reconcile(
        &mut self,
        metadata: &VerifiedSealedTextGenerationMetadataV1,
        witness: Option<RestoreFreshnessWitnessV1>,
        capture: RetainedReconcileCaptureV1,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        let RetainedReconcileCaptureV1 {
            mut captured,
            drained_hints,
            control,
            git_metadata,
            stat_signature,
        } = capture;
        if control.is_cancelled() {
            return Ok(None);
        }
        if captured.snapshot.reference != metadata.snapshot().reference
            || captured.snapshot.source_revision != metadata.snapshot().source_revision
            || captured.snapshot.content_identity != metadata.snapshot().content_identity
        {
            // Live ignored admissions already forced the complete path above.
            // An ordinary unequal capture is a real edit: rebuild under the
            // exact durable pointer. Do not fall through to reconcile_now —
            // that decodes the sealed generation through generations_root,
            // so a transient store failure never reaches publish and the
            // retry extracts the whole worktree again.
            let pointer = self
                .publication
                .read_publication_pointer()
                .map_err(CodeIndexProductionErrorV1::Publication)?
                .ok_or_else(|| {
                    CodeIndexSchedulerErrorV1::PublicationConflict(
                        "the retained text generation has no active durable publication".to_owned(),
                    )
                })?;
            if pointer.generation_id != metadata.manifest().generation_id.as_str()
                || pointer.snapshot_content_identity
                    != metadata.snapshot().content_identity.as_str()
            {
                return Err(CodeIndexSchedulerErrorV1::PublicationConflict(
                    "the retained text generation was superseded before rebuild".to_owned(),
                ));
            }
            let snapshot_content_identity = captured.snapshot.content_identity.clone();
            let reextracted_files = captured.changed_paths.len();
            let pending = self.publication.take_unpublished().filter(|pending| {
                pending.snapshot().reference == captured.snapshot.reference
                    && pending.snapshot().source_revision == captured.snapshot.source_revision
                    && pending.snapshot().content_identity == captured.snapshot.content_identity
            });
            let publication = self
                .publication
                .for_undecoded_active_rebuild(&pointer)
                .with_reconcile_publication_fence(Arc::clone(&self.hints), control.clone());
            let generation = if let Some(pending) = pending {
                // The previous pass already built this generation and lost
                // only the durable write. Republish it without a second
                // whole-store extract — isolated graph-off retries otherwise
                // miss their deadline waiting on a cold parser warmup.
                let scope = CodeIndexGenerationScopeV1::for_snapshot(&captured.snapshot);
                let mut publication = publication;
                publication
                    .publish_atomically(&scope, None, Arc::clone(&pending))
                    .map_err(CodeIndexProductionErrorV1::Publication)?;
                pending
            } else {
                let mut owner = open_production_code_index_owner_v1(
                    self.production_config.clone(),
                    publication,
                    DaemonProjectionSinkV1,
                )
                .map_err(|error| CodeIndexSchedulerErrorV1::ProductionOpen(error.to_string()))?
                .with_physical_artifact_pool(self.byte_pool.physical_artifacts.clone());
                owner.build_and_publish(
                    CodeIndexBuildRequestV1 {
                        snapshot: captured.snapshot,
                        captured_files: captured.captured_files,
                        changed_files: captured.changed_paths,
                        invalidations: BTreeSet::new(),
                        repository_parse_identity: captured.repository_parse_identity,
                        ignored_source_admissions: Vec::new(),
                        sealed_at: now_micros(),
                        target_projection_key: projection_key()?,
                    },
                    &control,
                )?
            };
            Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
            self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
            self._retained_snapshot_memory = std::mem::take(&mut captured.retained_reservations);
            self.latest_content_identity = Some(snapshot_content_identity);
            self.mark_reconciled_retained_generation_state(
                git_metadata.clone(),
                stat_signature.clone(),
            );
            let repository_parse_identity_digest =
                canonical_sha256(generation.repository_parse_identity())
                    .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
            if let Some(stat_signature) = stat_signature {
                RestoreFreshnessWitnessV1 {
                    generation_id: generation.manifest().generation_id.as_str().to_owned(),
                    git_metadata_signature: git_metadata.stable_signature(),
                    stat_signature,
                    repository_parse_identity_digest: repository_parse_identity_digest
                        .as_str()
                        .to_owned(),
                    ignored_source_admissions_digest: generation
                        .ignored_source_admissions_digest()
                        .as_str()
                        .to_owned(),
                    ignored_source_paths: Vec::new(),
                }
                .persist(&self.store_root);
            }
            let changes = &generation.projection().request().changes;
            let lane_digest = canonical_sha256(&(
                generation.snapshot().content_identity.clone(),
                generation
                    .chunks()
                    .chunks()
                    .iter()
                    .map(|chunk| (&chunk.id, &chunk.content_digest))
                    .collect::<Vec<_>>(),
                generation.edges(),
            ))
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
            let outcome = CodeIndexReconcileOutcomeV1::Published(CodeIndexPublishEvidenceV1 {
                generation_id: generation.manifest().generation_id.clone(),
                repository_id: self.repository_id.clone(),
                snapshot_content_identity: generation.snapshot().content_identity.clone(),
                lane_digest,
                file_occurrence_ids: generation
                    .snapshot()
                    .files
                    .iter()
                    .map(|file| file.file_occurrence_id.clone())
                    .collect(),
                reextracted_files,
                changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
                reused_chunks: changes.reused.len(),
                overflow_reconciled: drained_hints.overflow(),
            });
            drained_hints.commit();
            return Ok(Some(outcome));
        }
        drop(std::mem::take(&mut captured.captured_files));
        Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
        self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
        self._retained_snapshot_memory = std::mem::take(&mut captured.retained_reservations);
        let snapshot_content_identity = captured.snapshot.content_identity;
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        self.mark_reconciled_retained_generation_state(
            git_metadata.clone(),
            stat_signature.clone(),
        );
        if let (Some(witness), Some(stat_signature)) = (witness, stat_signature) {
            RestoreFreshnessWitnessV1 {
                generation_id: witness.generation_id,
                git_metadata_signature: git_metadata.stable_signature(),
                stat_signature,
                repository_parse_identity_digest: witness.repository_parse_identity_digest,
                ignored_source_admissions_digest: witness.ignored_source_admissions_digest,
                ignored_source_paths: witness.ignored_source_paths,
            }
            .persist(&self.store_root);
        }
        let outcome = CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
            snapshot_content_identity,
            overflow_reconciled: drained_hints.overflow(),
        });
        drained_hints.commit();
        Ok(Some(outcome))
    }

    /// Clone the immutable publication decoder so an optional graph replay can
    /// read and authenticate the O(store) sealed generation without occupying
    /// the mutable scheduler mutex. The decoded generation is not servable
    /// until [`Self::servable_decoded_retained_generation`] revalidates and
    /// binds it under the scheduler authority.
    pub fn active_generation_decoder(&self) -> Option<DaemonCodeIndexPublicationStoreV1> {
        (!self.shutting_down.load(Ordering::Acquire)).then(|| self.publication.clone())
    }

    /// Validate and bind a generation decoded through the detached immutable
    /// publication authority. Identity and ignored-source roster checks remain
    /// serialized with reconciliation; only sealed-byte I/O happens outside
    /// the scheduler mutex.
    pub fn servable_decoded_retained_generation(
        &mut self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        retained_text: Option<&LatestCodeTextGenerationV1>,
    ) -> Option<LatestCompleteCodeIndexV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        let resolved = match identity::IndexingIdentityV1::resolve(&self.project_root) {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(
                    event = "code_index_servable_identity_resolve_failed",
                    error = %error,
                    "decoded generation refused: indexing identity resolution failed"
                );
                return None;
            }
        };
        if !resolved.authorizes_reuse_of(&self.identity) {
            tracing::warn!(
                event = "code_index_servable_identity_reuse_refused",
                "decoded generation refused: live checkout identity does not \
                 authorize reuse of the scheduler's indexing identity"
            );
            return None;
        }
        if let Err(error) = self.validate_generation_identity(&generation) {
            tracing::warn!(
                event = "code_index_servable_generation_identity_invalid",
                error = %error,
                "decoded generation refused: sealed generation identity does \
                 not match this scheduler"
            );
            return None;
        }
        self.adopt_ignored_source_roster(&generation);
        if !self.ignored_source_roster_matches_generation(&generation) {
            tracing::warn!(
                event = "code_index_servable_ignored_roster_mismatch",
                "decoded generation refused: ignored-source roster disagrees \
                 with the sealed generation"
            );
            self.ignored_source_admissions.clear();
            // The cleared roster is the remedy, and the pass that must apply
            // it needs a wake this refused pass already consumed.
            self.ignored_roster_refusal_requires_rebuild = true;
            return None;
        }
        Some(self.bind_latest_complete(generation, retained_text))
    }

    /// Claim the one rebuild pass a refused ignored-source roster requires.
    ///
    /// Taking clears the claim, so the refusal arms exactly one follow-up pass
    /// no matter how many times the worker asks.
    pub fn take_ignored_roster_refusal_rebuild(&mut self) -> bool {
        std::mem::take(&mut self.ignored_roster_refusal_requires_rebuild)
    }

    /// Bind exact/lexical serving directly from the canonical active pointer.
    ///
    /// This authenticates the complete sealed content address and only decodes
    /// its bounded manifest/snapshot header. Graph, record-index, attribution,
    /// and semantic owners retain the full-generation decode path.
    pub fn servable_retained_text_generation(&mut self) -> Option<LatestCodeTextGenerationV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        let resolved = identity::IndexingIdentityV1::resolve(&self.project_root).ok()?;
        if !resolved.authorizes_reuse_of(&self.identity) {
            return None;
        }
        let pointer = self.publication.read_publication_pointer().ok().flatten()?;
        let generation_id = CodeGenerationId::new(pointer.generation_id.clone()).ok()?;
        let entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == pointer.generation_id)?;
        let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
            locator: entry.generation_file.clone(),
            digest: ManifestDigest::new(entry.state_digest.clone()).ok()?,
            size_bytes: entry.size_bytes,
        };
        let text_progress_owner_epoch = hotpath::measure_block!(
            "query.artifact.progress.publish",
            self.build_progress
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replace_generation(generation_id.clone())
        );
        let text_progress_state = Arc::new(hotpath::mutex!(
            Mutex::new(CodeIndexBuildProgressStateV1::new()),
            label = "query.artifact.progress.retained_state"
        ));
        let text_control = GenerationTextControlV1::new(Arc::clone(&self.shutting_down));
        let text_artifact_store = DaemonCodeTextArtifactStoreV1::bind(
            &self.store_root,
            &self.publication,
            &self.resident_memory,
            &self.project_id,
            &self.worktree_id,
        );
        let progress_slot = Arc::clone(&self.build_progress);
        let progress_generation = generation_id.clone();
        let progress_digest = sealed_identity.digest.as_str().to_owned();
        let progress_state = Arc::clone(&text_progress_state);
        let progress_daemon_incarnation = self.progress_daemon_incarnation;
        let progress_producer_incarnation = self.progress_producer_incarnation;
        let partitioned_metadata = text_artifact_store
            .published_descriptor(&generation_id)
            .ok()
            .flatten()
            .and_then(|_| {
                self.publication
                    .partitioned_text_metadata(&sealed_identity)
                    .ok()
                    .flatten()
            });
        let (metadata, sealed_format_revision, preopened_source) =
            if let Some(metadata) = partitioned_metadata {
                (
                    metadata,
                    tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
                    None,
                )
            } else {
                let mut source = text_artifact_store
                    .open_sealed_source_with_progress(
                        &sealed_identity,
                        &text_control,
                        move |scanned, total| {
                            let elapsed_micros = progress_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .elapsed_micros();
                            let snapshot = CodeIndexBuildProgressV1 {
                                generation_id: progress_generation.as_str().to_owned(),
                                daemon_incarnation: progress_daemon_incarnation,
                                producer_incarnation: progress_producer_incarnation,
                                progress_epoch: 0,
                                sealed_source_digest: progress_digest.clone(),
                                phase: CodeIndexBuildPhaseV1::SourceScan,
                                committed_pages: 0,
                                committed_chunks: 0,
                                committed_imports: 0,
                                committed_payload_bytes: 0,
                                completed_files: 0,
                                total_files: 0,
                                completed_lexical_bytes: scanned,
                                total_lexical_bytes: total,
                                current_batch_pages: 0,
                                current_batch_payload_bytes: 0,
                                elapsed_micros,
                                last_commit_latency_micros: None,
                                files_per_second: None,
                                lexical_bytes_per_second: None,
                                estimated_remaining_seconds: None,
                                last_progress_micros: now_micros().0,
                                blocked_reason: None,
                            };
                            let _ = try_publish_build_progress(
                                &progress_slot,
                                &progress_generation,
                                text_progress_owner_epoch,
                                snapshot,
                            );
                        },
                    )
                    .ok()?;
                if let Ok(Some(published)) = self.publication.active_already_decoded()
                    && published.manifest().generation_id == generation_id
                {
                    // Same-process successor: the builder still holds the decoded
                    // files. Re-decoding the sealed files array is how a 455 MiB
                    // cancel-batch successor spent the receipt wait in source_scan.
                    let _ = source.attach_published_files(&published);
                }
                (
                    source.metadata().clone(),
                    source.format_revision(),
                    Some(source),
                )
            };
        if metadata.manifest().project_id != self.project_id
            || metadata.manifest().generation_id != generation_id
            || metadata.snapshot().repository != self.repository_id
            || metadata.snapshot().worktree.as_ref() != Some(&self.worktree_id)
            || metadata.snapshot().content_identity.as_str() != pointer.snapshot_content_identity
        {
            text_control.retire();
            return None;
        }
        if self
            .publication
            .read_publication_pointer()
            .ok()
            .flatten()
            .as_ref()
            != Some(&pointer)
        {
            text_control.retire();
            return None;
        }
        let metadata = Arc::new(metadata);
        Some(LatestCodeTextGenerationV1 {
            metadata,
            sealed_format_revision,
            query_owners: Arc::new(OnceLock::new()),
            graph_activation: Arc::new(RwLock::new(CodeGraphActivationStateV1::Pending)),
            text_projection_build: Arc::new(CodeTextProjectionStateV1::new()),
            text_projection_failed: Arc::new(AtomicBool::new(false)),
            text_control,
            text_progress_state,
            text_progress_slot: Arc::clone(&self.build_progress),
            text_progress_owner_epoch,
            text_progress_daemon_incarnation: self.progress_daemon_incarnation,
            text_progress_producer_incarnation: self.progress_producer_incarnation,
            text_artifact_store,
            preopened_source: Arc::new(hotpath::mutex!(
                Mutex::new(preopened_source),
                label = "query.artifact.preopened_retained_source"
            )),
            publication_binding: Some(Arc::new(DurableActiveSealedGenerationBindingV1 {
                generation_id,
                generation_file: pointer.generation_file,
                state_digest: ManifestDigest::new(pointer.state_digest).ok()?,
            })),
        })
    }

    /// Canonical active publication generation id for tests that must observe
    /// the durable pointer without reading the private publication store.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active_publication_generation_id_for_test(&self) -> Option<String> {
        self.publication
            .read_publication_pointer()
            .ok()
            .flatten()
            .map(|pointer| pointer.generation_id)
    }

    /// Install a deterministic reconcile fault for one mounted worktree so a
    /// test can drive the real background worker loop over a pass that panics
    /// or fails, and count the attempts the loop actually makes.
    #[cfg(test)]
    pub fn install_reconcile_fault_for_test(
        &mut self,
        fault: Arc<reconcile_panic_guard::ReconcileFaultInjectionV1>,
    ) {
        self.reconcile_fault = Some(fault);
    }

    /// Records one attempted reconcile pass against the installed test fault.
    ///
    /// The worker loop reaches indexing through three branches — a retained
    /// text generation, retained-owner activation, and a plain reconcile — so
    /// hooking any single one of them counts a subset of the passes the loop
    /// actually makes. This is called once at the top of the loop's blocking
    /// closure instead, which is what `install_reconcile_fault_for_test`
    /// promises to count.
    #[cfg(test)]
    pub fn arrive_reconcile_fault_for_test(&self) -> Result<(), CodeIndexSchedulerErrorV1> {
        if let Some(fault) = self.reconcile_fault.clone() {
            fault.arrive()?;
        }
        Ok(())
    }

    /// Retained-owner activation entry point. Foreground reads never call this.
    #[hotpath::measure(label = "code_index.reconcile.pass")]
    pub fn activate_or_reconcile(
        &mut self,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        // The in-progress signal must cover the retained-activation branch
        // too: the worker has already claimed the pending wake, so without it
        // a failing activation pass would leave query admission unable to see
        // any in-flight owner work and misreport unverified retained state as
        // plain unavailability.
        let _reconcile_guard = ReconcilePassGuard::enter(&self.reconcile_in_progress);
        if let Some(outcome) = self.activate_retained_generation_from_frontier()? {
            return Ok(outcome);
        }
        self.reconcile_now()
    }

    #[hotpath::measure(label = "daemon.code_index.reconcile.pass")]
    pub fn reconcile_now(
        &mut self,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        self.reconcile_now_with_capture(|scheduler, control| {
            scheduler.capture_authoritative_snapshot(Some(control))
        })
    }

    fn reconcile_now_with_capture<C>(
        &mut self,
        mut capture: C,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1>
    where
        C: FnMut(
            &Self,
            &DaemonCodeIndexControlV1,
        ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1>,
    {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        self.ensure_worker_plan()?;
        let _worker_memory = self.reserve_worker_memory()?;
        let _reconcile_guard = ReconcilePassGuard::enter(&self.reconcile_in_progress);
        // Re-resolve exact identity before indexing (tier-3 backstop). The
        // worktree must still be the same structural identity this scheduler is
        // bound to; a HEAD move under the same worktree is allowed and simply
        // records a new source revision, so the served generation is never
        // mis-attributed across identities.
        let resolved = identity::IndexingIdentityV1::resolve(&self.project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        if !resolved.authorizes_reuse_of(&self.identity) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "worktree identity changed under the scheduler".to_owned(),
            ));
        }
        self.identity = resolved;
        if let Some(active) = self
            .publication
            .load_active_shared()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        {
            self.validate_generation_identity(&active)?;
            self.adopt_ignored_source_roster(&active);
        }
        // Capture may advance `.git/index` mtime (gix::open). The post-reconcile
        // witness is sampled at `mark_reconciled`, after that side effect, so
        // the next ready probe does not see this pass as stale.
        let mut overflow_reconciled = false;
        for retry in 0..=MAX_SUPERSEDED_RECONCILE_RETRIES {
            let control = DaemonCodeIndexControlV1::new(
                Arc::clone(&self.epoch),
                Arc::clone(&self.shutting_down),
            );
            let mut captured = match capture(self, &control) {
                Ok(captured) => captured,
                Err(CodeIndexSchedulerErrorV1::Production(
                    CodeIndexProductionErrorV1::Interrupted(
                        crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
                    ),
                )) if retry < MAX_SUPERSEDED_RECONCILE_RETRIES
                    && !self.shutting_down.load(Ordering::Acquire) =>
                {
                    std::thread::sleep(SUPERSEDED_RECONCILE_RETRY_BACKOFF);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let hints = {
                let mut hints = self
                    .hints
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (!control.is_cancelled()).then(|| hints.take())
            };
            let Some(hints) = hints else {
                if retry < MAX_SUPERSEDED_RECONCILE_RETRIES
                    && !self.shutting_down.load(Ordering::Acquire)
                {
                    std::thread::sleep(SUPERSEDED_RECONCILE_RETRY_BACKOFF);
                    continue;
                }
                return Err(cancelled_code_index_reconcile());
            };
            overflow_reconciled |= hints.overflow;
            let active_generation = self
                .publication
                .load_active_shared()
                .map_err(CodeIndexProductionErrorV1::Publication)?;
            if let Some(generation) = active_generation.as_ref() {
                self.validate_generation_identity(generation)?;
            }
            let active_is_reusable = active_generation.as_ref().is_none_or(|generation| {
                self.observe_generation_compatibility(generation)
                    .is_reusable()
            });
            let latest_snapshot = active_generation
                .as_ref()
                .map(|generation| generation.snapshot());
            let unchanged_source = latest_snapshot.is_some_and(|latest| {
                latest.reference == captured.snapshot.reference
                    && latest.source_revision == captured.snapshot.source_revision
            });
            let active_content_identity =
                latest_snapshot.map(|snapshot| &snapshot.content_identity);
            if active_is_reusable
                && self
                    .latest_content_identity
                    .as_ref()
                    .or(active_content_identity)
                    == Some(&captured.snapshot.content_identity)
                && unchanged_source
            {
                if control.is_cancelled() {
                    if retry < MAX_SUPERSEDED_RECONCILE_RETRIES
                        && !self.shutting_down.load(Ordering::Acquire)
                    {
                        std::thread::sleep(SUPERSEDED_RECONCILE_RETRY_BACKOFF);
                        continue;
                    }
                    return Err(cancelled_code_index_reconcile());
                }
                drop(std::mem::take(&mut captured.captured_files));
                Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
                self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
                self._retained_snapshot_memory =
                    std::mem::take(&mut captured.retained_reservations);
                self.latest_content_identity = Some(captured.snapshot.content_identity.clone());
                self.mark_reconciled();
                return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity: captured.snapshot.content_identity,
                    overflow_reconciled,
                }));
            }

            // Only the content identity and changed-path count are needed after
            // the build request takes ownership of the captured snapshot, so
            // keep those instead of cloning every file record and changed path.
            let mut snapshot_content_identity = captured.snapshot.content_identity.clone();
            let mut reextracted_files = captured.changed_paths.len();
            let mut generation = self.owner.build_and_publish(
                CodeIndexBuildRequestV1 {
                    snapshot: captured.snapshot,
                    captured_files: captured.captured_files,
                    changed_files: captured.changed_paths,
                    invalidations: BTreeSet::new(),
                    repository_parse_identity: captured.repository_parse_identity,
                    ignored_source_admissions: self.ignored_source_admissions.clone(),
                    sealed_at: now_micros(),
                    target_projection_key: projection_key()?,
                },
                &control,
            );
            if matches!(
                &generation,
                Err(CodeIndexProductionErrorV1::Input(
                    CodeIndexInputErrorV1::MissingCapturedFile
                ))
            ) {
                tracing::warn!(
                    "code-index incremental build missing captured file bytes; retrying without active-generation reuse"
                );
                captured =
                    self.capture_authoritative_snapshot_without_active_generation_reuse(None)?;
                snapshot_content_identity = captured.snapshot.content_identity.clone();
                reextracted_files = captured.changed_paths.len();
                generation = self.owner.build_and_publish(
                    CodeIndexBuildRequestV1 {
                        snapshot: captured.snapshot,
                        captured_files: captured.captured_files,
                        changed_files: captured.changed_paths,
                        invalidations: BTreeSet::new(),
                        repository_parse_identity: captured.repository_parse_identity,
                        ignored_source_admissions: self.ignored_source_admissions.clone(),
                        sealed_at: now_micros(),
                        target_projection_key: projection_key()?,
                    },
                    &control,
                );
            }
            let generation = match generation {
                Ok(generation) => generation,
                Err(CodeIndexProductionErrorV1::Interrupted(
                    crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
                )) if retry < MAX_SUPERSEDED_RECONCILE_RETRIES
                    && !self.shutting_down.load(Ordering::Acquire) =>
                {
                    std::thread::sleep(SUPERSEDED_RECONCILE_RETRY_BACKOFF);
                    continue;
                }
                Err(CodeIndexProductionErrorV1::Input(
                    CodeIndexInputErrorV1::NoExtractableFiles,
                )) => {
                    Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
                    self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
                    self._retained_snapshot_memory =
                        std::mem::take(&mut captured.retained_reservations);
                    self.latest_content_identity = Some(snapshot_content_identity.clone());
                    self.mark_reconciled();
                    return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                        snapshot_content_identity,
                        overflow_reconciled,
                    }));
                }
                Err(error) => return Err(error.into()),
            };
            let replacement_compatibility = self.observe_generation_compatibility(&generation);
            if !replacement_compatibility.is_reusable() {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "newly published generation is incompatible with its production owner"
                        .to_owned(),
                ));
            }
            Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
            self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
            self._retained_snapshot_memory = std::mem::take(&mut captured.retained_reservations);
            self.latest_content_identity = Some(snapshot_content_identity);
            self.mark_reconciled();

            let changes = &generation.projection().request().changes;
            let lane_digest = canonical_sha256(&(
                generation.snapshot().content_identity.clone(),
                generation
                    .chunks()
                    .chunks()
                    .iter()
                    .map(|chunk| (&chunk.id, &chunk.content_digest))
                    .collect::<Vec<_>>(),
                generation.edges(),
            ))
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
            return Ok(CodeIndexReconcileOutcomeV1::Published(
                CodeIndexPublishEvidenceV1 {
                    generation_id: generation.manifest().generation_id.clone(),
                    repository_id: self.repository_id.clone(),
                    snapshot_content_identity: generation.snapshot().content_identity.clone(),
                    lane_digest,
                    file_occurrence_ids: generation
                        .snapshot()
                        .files
                        .iter()
                        .map(|file| file.file_occurrence_id.clone())
                        .collect(),
                    reextracted_files,
                    changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
                    reused_chunks: changes.reused.len(),
                    overflow_reconciled,
                },
            ));
        }
        unreachable!("the bounded reconciliation loop returns on its final attempt")
    }

    fn mark_reconciled(&mut self) {
        let metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let signature = self.worktree_stat_signature().ok();
        self.mark_reconciled_state(metadata, signature);
        self.persist_freshness_witness();
    }

    fn mark_reconciled_state(
        &mut self,
        metadata: identity::GitMetadataFingerprintV1,
        signature: Option<String>,
    ) {
        let reconciled_without_generation = self
            .publication
            .load_active_shared()
            .is_ok_and(|generation| generation.is_none());
        self.freshness_fence.mark_reconciled(
            metadata,
            signature,
            &self.ignored_source_admissions,
            reconciled_without_generation,
        );
    }

    fn mark_reconciled_retained_generation_state(
        &mut self,
        metadata: identity::GitMetadataFingerprintV1,
        signature: Option<String>,
    ) {
        self.freshness_fence.mark_reconciled(
            metadata,
            signature,
            &self.ignored_source_admissions,
            false,
        );
    }

    /// Record the restore-time freshness witness for the current active
    /// generation. Called at the moment freshness is established (after a
    /// reconcile verified the worktree against gix truth) so a later open of the
    /// same worktree can prove the sealed generation still current without a full
    /// re-read. Requires an active generation AND a captured tier-2 signature;
    /// when either is absent the optimization simply defers to the next
    /// reconcile, and a write failure is non-fatal.
    fn persist_freshness_witness(&self) {
        let freshness = self.freshness_fence.snapshot();
        let Some(stat_signature) = freshness.last_stat_signature else {
            return;
        };
        let Some(latest) = self.latest_complete() else {
            return;
        };
        let Ok(repository_parse_identity_digest) =
            canonical_sha256(latest.generation.repository_parse_identity())
        else {
            return;
        };
        let witness = RestoreFreshnessWitnessV1 {
            generation_id: latest
                .generation
                .manifest()
                .generation_id
                .as_str()
                .to_owned(),
            git_metadata_signature: freshness.git_metadata.stable_signature(),
            stat_signature,
            repository_parse_identity_digest: repository_parse_identity_digest.as_str().to_owned(),
            ignored_source_admissions_digest: latest
                .generation
                .ignored_source_admissions_digest()
                .as_str()
                .to_owned(),
            ignored_source_paths: latest
                .generation
                .ignored_source_admissions()
                .iter()
                .map(|admission| admission.logical_path.clone())
                .collect(),
        };
        witness.persist(&self.store_root);
    }

    /// Admit only already-current immutable evidence. Expensive truth capture
    /// and generation publication belong to the background worker; a request
    /// that detects stale or unproven state schedules that worker and abstains.
    #[cfg(test)]
    fn latest_complete_ready_for_query(
        &mut self,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        self.latest_complete_ready_for_query_with(GenerationDecodeAdmissionV1::AwaitDecode)
    }

    /// [`Self::latest_complete_ready_for_query`] under an explicit decode
    /// admission. Unverified restore, git-metadata drift, and an elapsed
    /// staleness threshold abstain and schedule background work. They do not
    /// share [`Self::freshness_probe_requires_reconcile`]'s elapsed-threshold
    /// scan: that witness refresh belongs to the query/background ladder.
    #[cfg(test)]
    fn latest_complete_ready_for_query_with(
        &mut self,
        admission: GenerationDecodeAdmissionV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let freshness = self.freshness_fence.snapshot();
        if !freshness.verified_against_source
            || identity::GitMetadataFingerprintV1::capture(&self.project_root)
                .differs_from(&freshness.git_metadata)
            || freshness.last_reconciled_at.elapsed() >= self.policy.staleness_threshold
        {
            self.request_background_reconcile();
            return Ok(None);
        }
        Ok(self.latest_complete_with(admission))
    }

    /// Run the exact-source freshness fence without resolving a generation.
    /// Callers that already own the immutable serving handle must not consult
    /// the publication decoder cache merely to prove that handle is current.
    #[cfg(test)]
    fn exact_source_is_ready(&mut self) -> Result<bool, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let freshness = self.freshness_fence.snapshot();
        if freshness.freshness_unknown
            || identity::GitMetadataFingerprintV1::capture(&self.project_root)
                .differs_from(&freshness.git_metadata)
        {
            self.request_background_reconcile();
            return Ok(false);
        }
        match self.worktree_stat_signature() {
            Ok(signature) if freshness.last_stat_signature.as_ref() == Some(&signature) => {
                // The exact-source stat fence is stronger than the elapsed
                // tier-2 arm. Refresh only the monotonic admission clock: no
                // reconcile receipt or wall timestamp is fabricated, and a
                // clean status census cannot turn into a full capture loop.
                self.freshness_fence.refresh_monotonic_clock(false);
                Ok(true)
            }
            _ => {
                self.request_background_reconcile();
                Ok(false)
            }
        }
    }

    /// Mint the exact-source currency witness for one generation from the
    /// freshness state the last completed reconcile proved against gix truth.
    /// `None` is a typed abstention (nothing was ever proven), never a default:
    /// a busy verified read holding no witness refuses instead of serving.
    fn source_currency_witness_for(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Option<ServingSourceWitnessV1> {
        self.freshness_fence
            .source_currency_witness_for(generation_id)
    }

    /// A cheap stat-level (path, mtime, size) signature of the present source
    /// candidates. It opens gix and runs stat-based status (no byte reads, no
    /// content hashing), so it can gate the far more expensive read+hash capture
    /// on the tier-2 query path when nothing has actually changed on disk.
    fn worktree_stat_signature(&self) -> Result<String, CodeIndexSchedulerErrorV1> {
        freshness_witness::worktree_stat_signature_for(
            &self.project_root,
            &self.ignored_source_admissions,
        )
    }

    /// Deliver debounced hook hints (exact touched paths) into the incremental
    /// queue. Hints only narrow work; gix status remains the truth on reconcile.
    #[cfg(test)]
    pub fn notify_hook_paths<I>(&self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        {
            let mut hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for path in paths {
                hints.path(path);
            }
        }
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

    /// Freshness ladder run at query admission so external changes are caught
    /// without a filesystem watcher. Returns `Some(outcome)` when a
    /// reconciliation ran, or `None` when the verified clocks suppress work.
    ///
    /// - Unverified restore/open: always reconcile once before any suppression.
    /// - Tier 1 (git-mediated): `.git` metadata mtimes changed since the last
    ///   reconcile (commit/checkout/rebase/pull from any process) → reconcile.
    /// - Tier 2 (non-git mutations): the bounded-staleness threshold elapsed
    ///   (raw file writes, rsync, out-of-agent saves) → reconcile.
    /// - Tier 3 (identity backstop): reconciliation re-resolves identity, so a
    ///   served result is always attributed to its exact resolved identity.
    #[cfg(test)]
    pub fn ensure_fresh_for_query(
        &mut self,
    ) -> Result<Option<CodeIndexReconcileOutcomeV1>, CodeIndexSchedulerErrorV1> {
        let freshness = self.freshness_fence.snapshot();
        if !freshness.verified_against_source {
            // Open/restore sampled git metadata without verifying the sealed
            // generation against gix truth. Serving that generation is allowed;
            // suppressing cadence on open-time clocks is not.
            return Ok(Some(self.reconcile_now()?));
        }
        let git_changed = identity::GitMetadataFingerprintV1::capture(&self.project_root)
            .differs_from(&freshness.git_metadata);
        if git_changed {
            // Tier 1: a git-mediated mutation is authoritative evidence; reconcile.
            return Ok(Some(self.reconcile_now()?));
        }
        if freshness.last_reconciled_at.elapsed() < self.policy.staleness_threshold {
            return Ok(None);
        }
        // Tier 2: the bounded-staleness window elapsed. Gate the O(repo)
        // read+hash capture behind a cheap stat-level signature so a quiet
        // repository just resets its clock instead of re-reading every file.
        match self.worktree_stat_signature() {
            Ok(signature) if freshness.last_stat_signature.as_ref() == Some(&signature) => {
                self.freshness_fence.refresh_monotonic_clock(true);
                Ok(None)
            }
            _ => Ok(Some(self.reconcile_now()?)),
        }
    }

    /// Whether this worktree's git authority still resolves.
    ///
    /// The freshness ladder used to run inline at query admission, so a
    /// vanished or unreadable `.git` surfaced as a `reconcile_now` error and the
    /// query failed closed rather than serving retained bytes attributed to an
    /// identity nothing could confirm. Now that the rebuild is backgrounded
    /// (see [`Self::request_fresh_for_query_background`]) that error is no
    /// longer reached on the request path, so the fail-closed gate needs its own
    /// cheap probe. Opening the repository is the O(1) part of what reconcile
    /// did: it proves the authority exists without walking, hashing, or
    /// classifying anything.
    pub fn git_authority_available(&self) -> bool {
        gix::open(&self.project_root).is_ok()
    }

    /// [`Self::ensure_fresh_for_query`] with the O(store) rebuild moved off the
    /// request path.
    ///
    /// Runs the identical ladder — unverified restore, tier-1 git metadata,
    /// tier-2 bounded staleness — but where `ensure_fresh_for_query` calls
    /// `reconcile_now()` inline this only *requests* the background worker.
    /// The ladder's checks are cheap (stat-level metadata); its remedy is not,
    /// and a query must never pay for it. Unlike
    /// [`Self::latest_complete_ready_for_query_with`], this arm still scans the
    /// stat witness on an elapsed threshold so a quiet repository can reset
    /// its clock without a capture.
    ///
    /// Returns whether a reconcile was actually requested. A quiet repository
    /// must answer `false` and wake nothing: the ladder suppressing work is the
    /// common case, and waking the worker on every read would turn each query
    /// into a rebuild trigger — exactly the coupling this change removes.
    /// Decide whether the cheap Git/stat ladder requires an authoritative
    /// reconcile, without posting a worker wake. Callers that own a separate
    /// cadence authority use this split form so they can record the arrival
    /// before making the worker runnable.
    pub fn freshness_probe_requires_reconcile(&mut self) -> bool {
        let freshness = self.freshness_fence.snapshot();
        if !freshness.verified_against_source
            || identity::GitMetadataFingerprintV1::capture(&self.project_root)
                .differs_from(&freshness.git_metadata)
        {
            return true;
        }
        if freshness.last_reconciled_at.elapsed() < self.policy.staleness_threshold {
            return false;
        }
        if self
            .worktree_stat_signature()
            .is_ok_and(|signature| freshness.last_stat_signature.as_ref() == Some(&signature))
        {
            self.freshness_fence.refresh_monotonic_clock(true);
            return false;
        }
        true
    }

    pub fn request_fresh_for_query_background(&mut self) -> bool {
        if !self.freshness_probe_requires_reconcile() {
            return false;
        }
        // The ladder proved this worktree moved, so this is the wake that may
        // advance the canonical change generation and supersede index work.
        self.request_background_reconcile_for_observed_change();
        true
    }

    /// The exact identity this scheduler is currently bound to.
    pub fn identity(&self) -> &identity::IndexingIdentityV1 {
        &self.identity
    }

    #[hotpath::skip]
    pub fn last_reconciled_at_micros(&self) -> Option<i64> {
        self.freshness_fence.last_reconciled_at_micros()
    }

    #[hotpath::skip]
    pub fn verified_against_source(&self) -> bool {
        self.freshness_fence.snapshot().verified_against_source
    }

    /// True when reconciliation has verified the live worktree against source
    /// truth and that verified source publishes no code generation at all —
    /// the typed state of a project whose files are all unsupported,
    /// unextractable, or absent. Distinct from a warming scheduler, whose
    /// verification has not run yet, and from a publish failure, which leaves
    /// `verified_against_source` untouched by returning an error instead.
    pub fn reconciled_without_generation(&self) -> bool {
        self.freshness_fence.reconciled_without_generation()
    }

    pub fn pending_hint_count(&self) -> Option<u64> {
        let hints = self
            .hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hints.count()
    }

    #[cfg(test)]
    pub fn pending_hint_paths(&self) -> BTreeSet<PathBuf> {
        self.hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .paths
            .clone()
    }

    pub fn latest_complete(&self) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_with(GenerationDecodeAdmissionV1::AwaitDecode)
    }

    /// [`Self::latest_complete`] restricted to an already-decoded active
    /// generation. Abstains instead of parking on the single-flight decode.
    #[cfg(test)]
    pub fn latest_complete_already_decoded(&self) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_with(GenerationDecodeAdmissionV1::AlreadyDecoded)
    }

    fn latest_complete_with(
        &self,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let generation = match admission {
            GenerationDecodeAdmissionV1::AwaitDecode => self.publication.load_active_shared(),
            GenerationDecodeAdmissionV1::AlreadyDecoded => {
                self.publication.active_already_decoded()
            }
        }
        .ok()
        .flatten()?;
        self.validate_generation_identity(&generation).ok()?;
        if !self
            .observe_generation_compatibility(&generation)
            .may_serve_while_rebuilding()
        {
            return None;
        }
        Some(self.bind_latest_complete(generation, None))
    }

    /// Bind one decoded generation to this scheduler's per-generation serving
    /// derivations, so every reader of the same generation shares one build.
    fn bind_latest_complete(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        retained_text: Option<&LatestCodeTextGenerationV1>,
    ) -> LatestCompleteCodeIndexV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let retained_text =
            retained_text.filter(|text| text.metadata().manifest().generation_id == generation_id);
        let mut cached = self
            .query_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (
            query_owners,
            record_index,
            text_projection_build,
            text_projection_failed,
            text_control,
            text_progress_state,
            text_progress_owner_epoch,
            graph_activation,
        ) = match cached.as_ref() {
            Some((
                cached_id,
                owners,
                index,
                build,
                failed,
                control,
                progress,
                progress_epoch,
                interactive,
            )) if cached_id == &generation_id
                && retained_text.is_none_or(|text| {
                    Arc::ptr_eq(build, &text.text_projection_build)
                        && Arc::ptr_eq(interactive, &text.graph_activation)
                }) =>
            {
                (
                    Arc::clone(owners),
                    Arc::clone(index),
                    Arc::clone(build),
                    Arc::clone(failed),
                    control.clone(),
                    Arc::clone(progress),
                    *progress_epoch,
                    Arc::clone(interactive),
                )
            }
            _ => {
                if let Some((_, _, _, _, _, control, _, _, _)) = cached.as_ref() {
                    control.retire();
                }
                let same_generation_cache = cached
                    .as_ref()
                    .filter(|(cached_id, ..)| cached_id == &generation_id);
                let index = same_generation_cache.map_or_else(
                    || Arc::new(OnceLock::new()),
                    |(_, _, index, ..)| Arc::clone(index),
                );
                let graph_activation = retained_text.map_or_else(
                    || {
                        same_generation_cache.map_or_else(
                            || Arc::new(RwLock::new(CodeGraphActivationStateV1::Pending)),
                            |(_, _, _, _, _, _, _, _, graph_activation)| {
                                Arc::clone(graph_activation)
                            },
                        )
                    },
                    |text| Arc::clone(&text.graph_activation),
                );
                let (owners, build, failed, control, progress, progress_epoch) =
                    if let Some(text) = retained_text {
                        (
                            Arc::clone(&text.query_owners),
                            Arc::clone(&text.text_projection_build),
                            Arc::clone(&text.text_projection_failed),
                            text.text_control.clone(),
                            Arc::clone(&text.text_progress_state),
                            text.text_progress_owner_epoch,
                        )
                    } else {
                        let progress_epoch = hotpath::measure_block!(
                            "query.artifact.progress.publish",
                            self.build_progress
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .replace_generation(generation_id.clone())
                        );
                        (
                            Arc::new(OnceLock::new()),
                            Arc::new(CodeTextProjectionStateV1::new()),
                            Arc::new(AtomicBool::new(false)),
                            GenerationTextControlV1::new(Arc::clone(&self.shutting_down)),
                            Arc::new(hotpath::mutex!(
                                Mutex::new(CodeIndexBuildProgressStateV1::new()),
                                label = "query.artifact.progress.published_state"
                            )),
                            progress_epoch,
                        )
                    };
                *cached = Some((
                    generation_id,
                    Arc::clone(&owners),
                    Arc::clone(&index),
                    Arc::clone(&build),
                    Arc::clone(&failed),
                    control.clone(),
                    Arc::clone(&progress),
                    progress_epoch,
                    Arc::clone(&graph_activation),
                ));
                (
                    owners,
                    index,
                    build,
                    failed,
                    control,
                    progress,
                    progress_epoch,
                    graph_activation,
                )
            }
        };
        let metadata = Arc::new(
            VerifiedSealedTextGenerationMetadataV1::from_published_generation(&generation),
        );
        let text = retained_text
            .cloned()
            .unwrap_or_else(|| LatestCodeTextGenerationV1 {
                metadata,
                sealed_format_revision:
                    tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
                query_owners,
                graph_activation: Arc::clone(&graph_activation),
                text_projection_build,
                text_projection_failed,
                text_control,
                text_progress_state,
                text_progress_slot: Arc::clone(&self.build_progress),
                text_progress_owner_epoch,
                text_progress_daemon_incarnation: self.progress_daemon_incarnation,
                text_progress_producer_incarnation: self.progress_producer_incarnation,
                text_artifact_store: DaemonCodeTextArtifactStoreV1::bind(
                    &self.store_root,
                    &self.publication,
                    &self.resident_memory,
                    &self.project_id,
                    &self.worktree_id,
                ),
                preopened_source: Arc::new(hotpath::mutex!(
                    Mutex::new(None),
                    label = "query.artifact.preopened_published_source"
                )),
                publication_binding: None,
            });
        LatestCompleteCodeIndexV1 {
            generation,
            text,
            record_index,
        }
    }

    /// Decode, validate, mint, and warm the active generation eagerly.
    ///
    /// Activation — mount with an existing sealed store, or reconcile
    /// completion — is where a generation's O(store) derivations belong. Run
    /// this on a blocking worker at those points and the first query finds the
    /// decoded generation, its exact-admission sweep, its record indices, and
    /// its lane owners already built. A query that arrives while this is still
    /// running joins the in-flight decode through the publication store's
    /// single-flight barrier instead of starting a second one.
    ///
    /// Best-effort by construction: nothing here is a gate, and every failure
    /// simply leaves the work for the serving path, which still fails closed.
    #[cfg(test)]
    fn prime_serving_caches(&self) {
        if let Some(latest) = self.latest_complete() {
            latest.warm_serving_caches();
        }
    }

    /// Sealed-bytes decodes this process performed against this worktree's
    /// store. Test probe for "the serving path did not re-decode".
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn sealed_decode_count(&self) -> u64 {
        self.publication.sealed_decode_count()
    }

    /// Occupy this worktree's active-generation decode barrier, reproducing the
    /// window in which a new generation is being decoded/activated.
    #[cfg(test)]
    pub fn hold_active_decode(&self) -> HeldActiveDecodeV1 {
        self.publication.hold_active_decode()
    }

    fn generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        self.publication
            .load_generation(generation_id)
            .map(|generation| {
                generation
                    .filter(|generation| self.validate_generation_identity(generation).is_ok())
                    .map(|generation| self.historical_generation_owner().bind_complete(generation))
            })
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }

    pub fn reconcile_in_progress(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.reconcile_in_progress)
    }

    pub fn generation_recovery(&self) -> Arc<RwLock<Option<CodeIndexGenerationRecoveryV1>>> {
        Arc::clone(&self.generation_recovery)
    }

    pub fn active_generation_encoded_bytes(&self) -> Arc<AtomicU64> {
        self.publication.active_encoded_bytes()
    }

    /// Read, sanitize, intern and identify one candidate path.
    /// `Ok(None)` means the path is not an indexable source file (vanished,
    /// no extension, or no language descriptor) — the sequential loop's
    /// `continue` arms. Pure with respect to the shared byte pool: the pool
    /// is content-addressed under its own lock, so concurrent interning
    /// yields the same digests and the same shared buffers.
    fn capture_candidate(
        &self,
        registry: &StaticLanguageRegistry,
        logical_path: &str,
        control: Option<&dyn CodeIndexExecutionControlV1>,
    ) -> Result<Option<CapturedCandidateV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        ignored_dependencies::checkpoint_if_present(control)?;
        let explicitly_admitted = self
            .ignored_source_admissions
            .iter()
            .any(|admission| admission.logical_path == logical_path);
        self.capture_admitted_candidate(registry, logical_path, control, None, explicitly_admitted)
    }

    fn ignored_admission_paths(&self) -> BTreeSet<&str> {
        self.ignored_source_admissions
            .iter()
            .map(|admission| admission.logical_path.as_str())
            .collect()
    }

    fn capture_admitted_candidate(
        &self,
        registry: &StaticLanguageRegistry,
        logical_path: &str,
        control: Option<&dyn CodeIndexExecutionControlV1>,
        progress: Option<&git_tree_capture::CaptureProgressV1>,
        explicitly_admitted: bool,
    ) -> Result<Option<CapturedCandidateV1>, CodeIndexSchedulerErrorV1> {
        if !explicitly_admitted && crate::config::is_generated_path_segment(logical_path) {
            return Ok(None);
        }
        let absolute = self.project_root.join(logical_path);
        if !absolute.is_file() {
            return Ok(None);
        }
        let raw_bytes = if explicitly_admitted {
            ignored_dependencies::read_explicitly_admitted_source(
                &self.project_root,
                logical_path,
                control,
            )?
        } else {
            ignored_dependencies::read_bounded_snapshot_source(&absolute, control)?
        };
        ignored_dependencies::checkpoint_if_present(control)?;
        self.capture_candidate_bytes_with_progress(registry, logical_path, &raw_bytes, progress)
    }

    #[hotpath::measure(label = "code_index.capture.authoritative_snapshot")]
    fn capture_authoritative_snapshot(
        &self,
        control: Option<&dyn CodeIndexExecutionControlV1>,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        self.capture_authoritative_snapshot_with_active_generation_reuse(control, true)
    }

    fn capture_authoritative_snapshot_without_active_generation_reuse(
        &self,
        control: Option<&dyn CodeIndexExecutionControlV1>,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        self.capture_authoritative_snapshot_with_active_generation_reuse(control, false)
    }

    fn capture_authoritative_snapshot_with_active_generation_reuse(
        &self,
        control: Option<&dyn CodeIndexExecutionControlV1>,
        allow_active_generation_reuse: bool,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        ignored_dependencies::checkpoint_if_present(control)?;
        let repository = gix::open(&self.project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        // Classify committed/staged/unstaged/untracked/deleted/renamed paths
        // truthfully from gix. Deletions drop out of the present candidate set;
        // their tombstones flow through `changed_paths`.
        let mut retained_bytes: Vec<Arc<[u8]>> = Vec::new();
        let mut retained_reservations = Vec::new();
        let classification = classification::WorktreeChangeClassificationV1::classify(&repository)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let remembered_active_capture = self
            .active_snapshot_changed_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let reusable_active_candidate = if allow_active_generation_reuse
            && self.ignored_source_admissions.is_empty()
        {
            match self
                .publication
                .load_active_shared()
                .map_err(CodeIndexProductionErrorV1::Publication)?
            {
                Some(active) => {
                    self.validate_generation_identity(&active)?;
                    let current_scope = CodeIndexGenerationScopeV1 {
                        repository: self.repository_id.clone(),
                        reference: self.identity.head_ref().cloned(),
                        worktree: Some(self.worktree_id.clone()),
                    };
                    (active.sealed_scope() == current_scope
                        && active
                            .compatibility_with(&self.production_config)
                            .is_reusable()
                        && active.ignored_source_admissions().is_empty())
                    .then_some(active)
                    .filter(|active| {
                        active.repository_parse_identity().dirty == RepositoryDirtyStateV1::Clean
                            || remembered_active_capture.as_ref().is_some_and(
                                |(content_identity, _)| {
                                    content_identity == &active.snapshot().content_identity
                                },
                            )
                    })
                }
                None => None,
            }
        } else {
            None
        };
        let (reusable_active, tree_delta) = match reusable_active_candidate {
            Some(active) => {
                let tree_delta = match (
                    active.repository_parse_identity().tree.as_ref(),
                    self.identity.head_tree(),
                ) {
                    (Some(active_tree), Some(head_tree)) if active_tree != head_tree => {
                        changed_paths_between_trees(&repository, active_tree, head_tree)
                    }
                    (active_tree, head_tree) if active_tree == head_tree => Some(BTreeSet::new()),
                    _ => None,
                };
                match tree_delta {
                    Some(tree_delta) => (Some(active), tree_delta),
                    None => {
                        tracing::warn!(
                            active_tree = ?active.repository_parse_identity().tree,
                            head_tree = ?self.identity.head_tree(),
                            "HEAD-tree delta unavailable; capturing without active-generation reuse"
                        );
                        (None, BTreeSet::new())
                    }
                }
            }
            None => (None, BTreeSet::new()),
        };
        if reusable_active.is_none()
            && allow_active_generation_reuse
            && self.ignored_source_admissions.is_empty()
            && classification.changes().is_empty()
            && let (Some(reference), Some(revision), Some(tree)) = (
                self.identity.head_ref(),
                self.identity.head_commit(),
                self.identity.head_tree(),
            )
        {
            let captured = self
                .capture_exact_git_tree_snapshot(
                    &git_tree_capture::ExactGitTreeSourceV1 {
                        reference: reference.clone(),
                        revision: revision.clone(),
                        tree: tree.clone(),
                    },
                    &branch_generations::BranchGenerationReadControlV1 {
                        deadline: None,
                        cancellation: None,
                    },
                )
                .map_err(|reason| match reason {
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::Cancelled => {
                        cancelled_code_index_reconcile()
                    }
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable => {
                        CodeIndexSchedulerErrorV1::SnapshotMemoryCapacityUnavailable
                    }
                    _ => CodeIndexSchedulerErrorV1::Git(format!(
                        "immutable HEAD-tree capture failed: {}",
                        reason.as_str()
                    )),
                })?;
            *self
                .active_snapshot_changed_paths
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
                captured.snapshot.content_identity.clone(),
                captured.changed_paths.clone(),
            ));
            return Ok(captured);
        }
        let source_revision = (self.ignored_source_admissions.is_empty()
            && classification.changes().is_empty())
        .then(|| self.identity.head_commit().cloned())
        .flatten();
        let mut candidate_paths = classification.candidate_paths();
        let mut changed_paths = classification.changed_paths();
        changed_paths.extend(tree_delta);
        candidate_paths.extend(
            self.ignored_source_admissions
                .iter()
                .map(|admission| admission.logical_path.clone()),
        );
        changed_paths.extend(
            self.ignored_source_admissions
                .iter()
                .map(|admission| admission.logical_path.clone()),
        );
        let dirty = if !self.ignored_source_admissions.is_empty() {
            RepositoryDirtyStateV1::Dirty
        } else if classification
            .changes()
            .iter()
            .any(|change| change.class == classification::WorktreeChangeClassV1::Conflicted)
        {
            RepositoryDirtyStateV1::Conflicted
        } else if classification.changes().is_empty() {
            RepositoryDirtyStateV1::Clean
        } else {
            RepositoryDirtyStateV1::Dirty
        };

        let registry = StaticLanguageRegistry::new();
        let remembered_dirty_paths = reusable_active.as_ref().and_then(|active| {
            (active.repository_parse_identity().dirty != RepositoryDirtyStateV1::Clean)
                .then(|| {
                    remembered_active_capture
                        .as_ref()
                        .map(|(_, changed_paths)| changed_paths)
                })
                .flatten()
        });
        let active_files = reusable_active
            .as_ref()
            .map(|active| {
                active
                    .snapshot()
                    .files
                    .iter()
                    .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
                    .filter(|file| {
                        remembered_dirty_paths
                            .is_none_or(|paths| !paths.contains(&file.logical_path))
                    })
                    .map(|file| (file.logical_path.as_str(), file))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut files = candidate_paths
            .iter()
            .filter(|logical_path| !changed_paths.contains(*logical_path))
            .filter_map(|logical_path| active_files.get(logical_path.as_str()).copied())
            .cloned()
            .collect::<Vec<_>>();
        // Read + sanitize + digest is per-file pure work over independent
        // paths, so it fans out across the reserved-width indexing pool. The
        // candidate set is an ordered `BTreeSet`; results are collected in
        // that same order and the lowest-index failure is the reported one,
        // so the captured snapshot is byte-identical to the sequential sweep.
        let candidates = candidate_paths
            .into_iter()
            .filter(|logical_path| {
                changed_paths.contains(logical_path)
                    || !active_files.contains_key(logical_path.as_str())
            })
            .collect::<Vec<_>>();
        let admitted_paths = self.ignored_admission_paths();
        let progress = git_tree_capture::CaptureProgressV1::new();
        let outcomes = crate::code_index::parallelism::install(|| {
            use rayon::prelude::*;
            candidates
                .par_iter()
                .map(|logical_path| {
                    crate::code_index::parallelism::with_background_cpu_permit(|| {
                        if self.shutting_down.load(Ordering::Acquire) {
                            return Err(cancelled_code_index_reconcile());
                        }
                        ignored_dependencies::checkpoint_if_present(control)?;
                        self.capture_admitted_candidate(
                            &registry,
                            logical_path,
                            control,
                            Some(&progress),
                            admitted_paths.contains(logical_path.as_str()),
                        )
                    })
                })
                .collect::<Vec<_>>()
        })
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Production(CodeIndexProductionErrorV1::Parallelism(error))
        })?;

        let mut captured_files = Vec::new();
        let mut sanitization_receipts = BTreeSet::new();
        if let Some(active) = reusable_active.as_ref() {
            sanitization_receipts.extend(active.snapshot().sanitization_receipts.iter().cloned());
            let reused_occurrences = files
                .iter()
                .map(|file| &file.file_occurrence_id)
                .collect::<BTreeSet<_>>();
            let mut replaced_receipts = BTreeSet::new();
            for file in active.snapshot().files.iter().filter(|file| {
                file.disposition == SnapshotFileDispositionV1::Present
                    && !reused_occurrences.contains(&file.file_occurrence_id)
            }) {
                for receipt in &active.snapshot().sanitization_receipts {
                    if file_occurrence_id(
                        &self.repository_id,
                        &self.worktree_id,
                        &file.logical_path,
                        &file.content_digest,
                        receipt,
                    )? == file.file_occurrence_id
                    {
                        replaced_receipts.insert(receipt.clone());
                        break;
                    }
                }
            }
            for receipt in replaced_receipts {
                let mut still_reused = false;
                for file in &files {
                    if file_occurrence_id(
                        &self.repository_id,
                        &self.worktree_id,
                        &file.logical_path,
                        &file.content_digest,
                        &receipt,
                    )? == file.file_occurrence_id
                    {
                        still_reused = true;
                        break;
                    }
                }
                if !still_reused {
                    sanitization_receipts.remove(&receipt);
                }
            }
        }
        // A privacy refusal is evidence about one file. Withholding it keeps
        // the rest of the worktree indexable; only a genuine capture fault
        // still terminates the pass.
        let mut withheld_sources = Vec::new();
        for (logical_path, outcome) in candidates.iter().zip(outcomes) {
            let candidate = match outcome {
                Ok(Some(candidate)) => candidate,
                Ok(None) => continue,
                Err(error) => {
                    let withheld = git_tree_capture::classify_capture_failure(logical_path, error)?;
                    withheld_sources.push(withheld);
                    continue;
                }
            };
            sanitization_receipts.insert(candidate.receipt_id);
            if let Some(reservation) = candidate.retained_reservation {
                retained_reservations.push(reservation);
            }
            retained_bytes.push(candidate.retained);
            files.push(candidate.file);
            captured_files.push(candidate.captured);
        }
        git_tree_capture::report_withheld_sources(&withheld_sources);
        if files.is_empty() && !withheld_sources.is_empty() {
            return Err(CodeIndexSchedulerErrorV1::Privacy(
                "every indexable source in this worktree was withheld by the privacy boundary"
                    .to_owned(),
            ));
        }
        files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        captured_files
            .sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        let sanitization_receipts = sanitization_receipts.into_iter().collect::<Vec<_>>();
        let content_identity = snapshot_content_identity(&files, &sanitization_receipts);
        let captured = CapturedSnapshotV1 {
            repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
                tree: self.identity.head_tree().cloned(),
                dirty,
            },
            snapshot: SanitizedCodeSnapshotV1 {
                repository: self.repository_id.clone(),
                worktree: Some(self.worktree_id.clone()),
                reference: self.identity.head_ref().cloned(),
                source_revision,
                sanitizer_revision: id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)?,
                sanitization_receipts,
                content_identity,
                captured_at: now_micros(),
                files,
            },
            captured_files,
            changed_paths,
            retained_bytes,
            retained_reservations,
        };
        *self
            .active_snapshot_changed_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
            captured.snapshot.content_identity.clone(),
            captured.changed_paths.clone(),
        ));
        Ok(captured)
    }
}

/// Return the exact file-level delta, or `None` when gix cannot prove it so
/// callers can disable active-row reuse instead of guessing.
fn changed_paths_between_trees(
    repository: &gix::Repository,
    active_tree: &TreeId,
    head_tree: &TreeId,
) -> Option<BTreeSet<String>> {
    let active_tree = repository
        .find_tree(active_tree.as_str().parse::<gix::ObjectId>().ok()?)
        .ok()?;
    let head_tree = repository
        .find_tree(head_tree.as_str().parse::<gix::ObjectId>().ok()?)
        .ok()?;
    let mut changes = active_tree.changes().ok()?;
    // Rename/copy detection would compare blob contents across the whole
    // tree; a rename surfaces as deletion + addition, which already marks both
    // paths, so keep the walk proportional to the differing subtrees.
    changes.options(|options| {
        options.track_path().track_rewrites(None);
    });
    let mut paths = BTreeSet::new();
    let mut invalid_path = false;
    changes
        .for_each_to_obtain_tree(&head_tree, |change| {
            let mut insert = |path: &gix::bstr::BStr| match path.to_str() {
                Ok(path) => {
                    paths.insert(path.to_owned());
                }
                Err(_) => invalid_path = true,
            };
            match change {
                TreeDiffChange::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | TreeDiffChange::Deletion {
                    location,
                    entry_mode,
                    ..
                } if entry_mode.is_no_tree() => {
                    insert(location);
                }
                TreeDiffChange::Modification {
                    location,
                    previous_entry_mode,
                    entry_mode,
                    ..
                } if previous_entry_mode.is_no_tree() || entry_mode.is_no_tree() => {
                    insert(location);
                }
                TreeDiffChange::Rewrite {
                    source_location,
                    source_entry_mode,
                    location,
                    entry_mode,
                    ..
                } if source_entry_mode.is_no_tree() || entry_mode.is_no_tree() => {
                    insert(source_location);
                    insert(location);
                }
                _ => {}
            }
            Ok::<_, std::convert::Infallible>(TreeDiffAction::Continue(()))
        })
        .ok()?;
    (!invalid_path).then_some(paths)
}

fn cancelled_code_index_reconcile() -> CodeIndexSchedulerErrorV1 {
    CodeIndexProductionErrorV1::Interrupted(
        crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
    )
    .into()
}

impl Drop for CodeIndexWorktreeSchedulerV1 {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
    }
}

fn id<T>(value: &str) -> Result<T, CodeIndexSchedulerErrorV1>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned())
        .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))
}

fn file_occurrence_id(
    repository: &RepositoryId,
    worktree: &WorktreeId,
    logical_path: &str,
    digest: &ContentDigest,
    receipt: &SanitizationReceiptId,
) -> Result<FileOccurrenceId, CodeIndexSchedulerErrorV1> {
    id(&format!(
        "file.daemon.{}",
        sha256_hex(
            format!(
                "{}\0{}\0{logical_path}\0{}\0{}",
                repository.as_str(),
                worktree.as_str(),
                digest.as_str(),
                receipt.as_str(),
            )
            .as_bytes()
        )
    ))
}

/// The one central exact-admission authority every serving owner installs.
fn exact_serving_authority() -> Result<CentralExactAdmissionAuthorityV1, RetrievalPortError> {
    Ok(CentralExactAdmissionAuthorityV1::new(
        ExactAdmissionRuleRevision::new(tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
    ))
}

fn projection_key() -> Result<ProjectionKeyV1, CodeIndexSchedulerErrorV1> {
    Ok(ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.daemon.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "d".repeat(64)))?,
    })
}

fn snapshot_content_identity(
    files: &[SanitizedCodeFileV1],
    sanitization_receipts: &[SanitizationReceiptId],
) -> ContentDigest {
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend_from_slice(file.logical_path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(file.content_digest.as_str().as_bytes());
        bytes.push(0xff);
    }
    for receipt in sanitization_receipts {
        bytes.extend_from_slice(receipt.as_str().as_bytes());
        bytes.push(0xfe);
    }
    content_digest(&bytes)
}

/// Streaming SHA-256 of one file's bytes, as 64 lowercase hex characters.
/// Cancellation is checked before opening and after every bounded read, so a
/// shutdown or superseding generation cannot strand publication in a
/// corpus-sized uninterruptible hash.
fn sha256_private_file_hex_and_size(
    path: &Path,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(String, u64), RetrievalPortError> {
    checkpoint_text_artifact_control(control)?;
    let named_metadata = path.symlink_metadata().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RetrievalPortError::AuthorityUnavailable(
                "code text artifact file is missing".to_owned(),
            )
        } else {
            text_artifact_unavailable(error)
        }
    })?;
    if !named_metadata.file_type().is_file() {
        return Err(RetrievalPortError::Contract(
            "code text artifact path is not a regular file".to_owned(),
        ));
    }
    let mut file = open_private_file(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RetrievalPortError::AuthorityUnavailable(
                "code text artifact file disappeared before open".to_owned(),
            )
        } else {
            RetrievalPortError::Contract(format!(
                "code text artifact file is not owner-private: {error}"
            ))
        }
    })?;
    let file_metadata = file.metadata().map_err(text_artifact_unavailable)?;
    if !file_metadata.is_file() || file_metadata.len() != named_metadata.len() {
        return Err(RetrievalPortError::Contract(
            "code text artifact file identity changed before hashing".to_owned(),
        ));
    }
    let identity = Handle::from_file(file.try_clone().map_err(text_artifact_unavailable)?)
        .map_err(text_artifact_unavailable)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(text_artifact_unavailable)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        checkpoint_text_artifact_control(control)?;
    }
    let current_metadata = path.symlink_metadata().map_err(text_artifact_unavailable)?;
    let current_identity = Handle::from_path(path).map_err(text_artifact_unavailable)?;
    if !current_metadata.file_type().is_file()
        || current_metadata.len() != file_metadata.len()
        || current_identity != identity
    {
        return Err(RetrievalPortError::Contract(
            "code text artifact named file changed while hashing".to_owned(),
        ));
    }
    Ok((
        encode_lowercase_hex(&hasher.finalize()),
        file_metadata.len(),
    ))
}

fn ensure_private_text_artifacts_root(path: &Path) -> Result<(), RetrievalPortError> {
    match tracedecay_private_fs::create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let validation_error = match validate_private_directory(path) {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            // A pre-existing root that fails owner-privacy validation is most
            // often a legacy directory an older binary created under a
            // permissive umask. Ownership is the proof this process may
            // tighten it in place; a root it does not own — or that is not a
            // directory at all — stays a typed deterministic contract
            // violation for the operator instead of an endless silent retry.
            match make_private_directory(path) {
                Ok(receipt) => {
                    let previous_mode = receipt
                        .previous_unix_mode
                        .map_or_else(|| "platform-acl".to_owned(), |mode| format!("{mode:o}"));
                    tracing::info!(
                        event = "code_index_text_artifacts_root_privacy_healed",
                        previous_mode = %previous_mode,
                        "legacy code text artifacts root was re-permissioned to owner-private"
                    );
                    Ok(())
                }
                Err(heal_error) => Err(RetrievalPortError::Contract(format!(
                    "code text artifacts root '{}' is not owner-private{}: {validation_error}; \
                     self-heal refused: {heal_error}; restore owner-only access (chmod 700 and \
                     chown to the daemon user) or re-enroll the store",
                    path.display(),
                    observed_unix_mode(path)
                        .map(|mode| format!(" (mode {mode:o}, need 700)"))
                        .unwrap_or_default(),
                ))),
            }
        }
        Err(error) => Err(text_artifact_unavailable(error)),
    }
}

/// Unix permission bits currently on `path`, for typed contract messages.
#[cfg(unix)]
fn observed_unix_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    path.symlink_metadata()
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn observed_unix_mode(_path: &Path) -> Option<u32> {
    None
}

fn checkpoint_text_artifact_control(
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), RetrievalPortError> {
    if control.is_cancelled() {
        Err(RetrievalPortError::Cancelled)
    } else if control.is_deadline_exceeded() {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

/// Divide the single process reservation between the source's concurrently
/// retained decode window and the `SQLite` builder. Each component fitting the
/// ceiling independently is insufficient because both remain live while a
/// page is admitted.
fn text_artifact_builder_budget(source_window_bytes: usize) -> Result<usize, RetrievalPortError> {
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1
        .checked_sub(source_window_bytes)
        .filter(|remaining| *remaining > 0)
        .ok_or(RetrievalPortError::BudgetExceeded)
}

#[cfg(test)]
mod activation_tests;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod overlay_ephemerality_tests;
#[cfg(test)]
mod tests;

mod activation;
pub mod branch_generations;
mod cadence;
mod classification;
mod freshness_witness;
mod git_tree_capture;
mod graph_activation;
pub mod identity;
pub mod ignored_dependencies;
pub mod observability;
mod privacy;
pub mod queries;
pub mod query_runtime;
mod reconcile_panic_guard;
mod registry;
pub mod semantic_query_runtime;
pub mod semantic_vector_graph;

// The registry surface lives in `registry.rs`; re-export it so its public path
// (`code_index_scheduler::CodeIndexSchedulerRegistryV1`) and method signatures
// stay stable for the daemon and MCP server that mount and query worktrees.
pub use crate::code_graph_seat::CodeGraphReplayBindingV1;
pub use activation::{
    CodeIndexActivationHintSinkV1, CodeIndexActivationMountV1, CodeIndexActivationV1,
    CodeIndexAutomaticAdmissionV1,
};
#[cfg(test)]
pub use cadence::CodeIndexCadenceReadModelV1;
pub use cadence::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, newly_eligible_percentile,
};
pub use graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1};
pub use ignored_dependencies::{
    CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexIgnoredDependencyRefusalV1,
    CodeIndexIgnoredDependencyRequestV1,
};
pub use registry::CodeIndexSchedulerRegistryV1;
pub use registry::watch_ingress::GitStateChangeRequestV1;
pub use registry::{ServingGenerationInstallationOutcomeV1, ServingGenerationRollbackOutcomeV1};
pub type CodeIndexGenerationPublishedV1 = registry::CodeIndexGenerationPublishedV1;
