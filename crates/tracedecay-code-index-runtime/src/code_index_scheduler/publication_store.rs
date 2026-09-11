//! Durable publication store for sealed code-index generations: the shared
//! byte pool, the decoded-generation cache, and the atomic publication port.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::SystemTime,
};

use same_file::Handle;
use sha2::{Digest, Sha256};
use tracedecay_application::code_index::DaemonCodeIndexControlV1;
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationStoreLockV1, DurableGenerationCardinalityV1, DurableGenerationIndexEntryV1,
    DurablePublicationPointerV1, DurableSealedCodeGenerationIdentityV1,
    MAX_DURABLE_GENERATION_INDEX_BYTES_V1, MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1,
    acquire_code_generation_store_lock, durable_generation_index_digest,
    retain_bounded_generation_index, try_acquire_code_generation_store_lock,
};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, ManifestDigest, ProjectionBatchRequestV1,
    ProjectionOperationV1, ProjectionOutcomeV1, SanitizerRevision,
    canonical_text::encode_tagged_lowercase_hex, sha256_hex_suffix,
};
use tracedecay_private_fs::framed_log::DirectorySyncPolicy;

use crate::code_index::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1,
        CodeIndexInterruptionV1, CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, SealedGenerationSegmentPublicationV1,
        SealedGenerationSegmentReadV1, SharedPhysicalCodeArtifactPoolV1,
        UninterruptibleCodeIndexControlV1, VerifiedSealedTextGenerationMetadataV1,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    },
};

use super::{CodeIndexSchedulerErrorV1, PendingHintsV1, ProfiledStdMutex};

const MAX_DURABLE_PUBLICATION_POINTER_BYTES: u64 = 512 * 1024;
const DURABLE_GENERATION_IO_CHUNK_BYTES_V1: usize = 64 * 1024;
#[cfg(feature = "hotpath")]
static CODE_INDEX_GENERATION_DECODES_ACTIVE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "hotpath")]
static CODE_INDEX_GENERATION_DECODE_WAITERS: AtomicUsize = AtomicUsize::new(0);

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
    pub(super) physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
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
    pub(super) fn intern(&self, bytes: Vec<u8>) -> (ContentDigest, Arc<[u8]>) {
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
    pub(super) fn stats(&self) -> CodeIndexBytePoolStatsV1 {
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
pub(super) const DECODED_GENERATION_CACHE_CAPACITY: usize = 4;

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
    pub(super) seal_encoded_segment_bytes: Arc<AtomicU64>,
    pub(super) seal_existing_segment_bytes_read: Arc<AtomicU64>,
    pub(super) seal_evidence_page_count: Arc<AtomicU64>,
    pub(super) seal_evidence_durable_transaction_count: Arc<AtomicU64>,
    active_path: PathBuf,
    pub(super) generations_root: PathBuf,
    segments_root: PathBuf,
    pub(super) project_root: PathBuf,
    expected_sanitizer_revision: SanitizerRevision,
    disposition: CodeIndexPublicationDispositionV1,
    pointer_memo: Arc<ProfiledStdMutex<Option<PublicationPointerMemoV1>>>,
    undecoded_active_expectation: Option<UndecodedActivePublicationExpectationV1>,
    /// The canonical source-hint authority plus the exact pre-capture epoch
    /// used by a retained rebuild. Ordinary publication leaves this absent.
    reconcile_publication_fence: Option<(Arc<Mutex<PendingHintsV1>>, DaemonCodeIndexControlV1)>,
    /// The owning worktree's shutdown flag. An initial build has no fence, so
    /// this is the only cancellation a first seal can observe.
    shutdown_signal: Option<Arc<AtomicBool>>,
    /// Test-only: observes every durably published file segment so a test can
    /// retire the shutdown signal between two segments of one seal.
    #[cfg(test)]
    seal_segment_observer: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Last generation handed to `publish_atomically`. A transient store
    /// failure must not drop it: the next undecoded retry republishes this
    /// candidate instead of extracting the whole worktree again.
    pub(super) unpublished_candidate: Arc<Mutex<Option<Arc<CodeIndexPublishedGenerationV1>>>>,
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

pub(super) struct TemporaryEvidencePackV1 {
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
    pub(super) fn create(path: PathBuf) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
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

    pub(super) fn append_page(
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
        let digest_hex = sha256_hex_suffix(segment_digest.as_str()).ok_or_else(|| {
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
    pub(super) fn new(
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
        // Scope reconciliation only sees this directory's hash; the record
        // lets it collect the scope as soon as the checkout is deleted rather
        // than after the stranding age.
        tracedecay_code_index_retention::code_index_generations::record_scope_root(
            store_root,
            project_root,
        )?;
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
            shutdown_signal: None,
            #[cfg(test)]
            seal_segment_observer: None,
            unpublished_candidate: Arc::new(Mutex::new(None)),
        })
    }

    pub(super) fn for_undecoded_active_rebuild(
        &self,
        pointer: &DurablePublicationPointerV1,
    ) -> Self {
        let mut publication = self.clone();
        publication.undecoded_active_expectation = Some(UndecodedActivePublicationExpectationV1 {
            generation_id: pointer.generation_id.clone(),
            generation_file: pointer.generation_file.clone(),
            state_digest: pointer.state_digest.clone(),
        });
        publication
    }

    pub(super) fn with_reconcile_publication_fence(
        mut self,
        hints: Arc<Mutex<PendingHintsV1>>,
        control: DaemonCodeIndexControlV1,
    ) -> Self {
        self.reconcile_publication_fence = Some((hints, control));
        self
    }

    pub(super) fn with_shutdown_signal(mut self, shutting_down: Arc<AtomicBool>) -> Self {
        self.shutdown_signal = Some(shutting_down);
        self
    }

    #[cfg(test)]
    pub(super) fn with_seal_segment_observer_for_test(
        mut self,
        observer: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.seal_segment_observer = Some(observer);
        self
    }

    /// The seal encodes and durably writes one segment per file, so a
    /// generation-sized worktree spends seconds here with no other
    /// cancellation point. Daemon shutdown retires the worktree's shutdown
    /// signal and a retained rebuild's supersession retires its fence;
    /// checking both before every segment keeps the blocking reconcile pass
    /// joinable inside the shutdown budget instead of forcing the coordinator
    /// to abandon it and the runtime teardown to wait for it again.
    fn seal_checkpoint(&self) -> Result<(), CodeIndexProductionErrorV1> {
        if self
            .shutdown_signal
            .as_ref()
            .is_some_and(|shutting_down| shutting_down.load(Ordering::Acquire))
            || self
                .reconcile_publication_fence
                .as_ref()
                .is_some_and(|(_, control)| control.is_cancelled())
        {
            return Err(CodeIndexProductionErrorV1::Interrupted(
                crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
            ));
        }
        Ok(())
    }

    pub(super) fn retained_history(&self) -> Self {
        let mut retained = self.clone();
        retained.disposition = CodeIndexPublicationDispositionV1::RetainedHistory;
        retained
    }

    pub(super) fn unavailable(error: impl std::fmt::Display) -> CodeIndexPublicationStoreErrorV1 {
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

    pub(super) fn sync_directory(path: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
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
        let digest_hex = sha256_hex_suffix(digest.as_str())
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

    pub(super) fn state_digest(bytes: &[u8]) -> String {
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

    pub(super) fn validate_generation_file(
        value: &str,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
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

    pub(super) fn read_publication_pointer(
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

    pub(super) fn read_retained_partitioned_segment(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        let _lock = try_acquire_code_generation_store_lock(root)
            .map_err(Self::unavailable)?
            .ok_or_else(|| Self::unavailable("sealed lexical source generation store is busy"))?;
        let pointer = self.read_publication_pointer()?;
        if !pointer.as_ref().is_some_and(|pointer| {
            pointer.generation_index.iter().any(|entry| {
                entry.generation_file == identity.locator
                    && entry.state_digest == identity.digest.as_str()
                    && entry.size_bytes == identity.size_bytes
            })
        }) {
            return Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::Cancelled,
            ));
        }
        self.read_partitioned_segment(request, buffer)
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
        let digest_hex = sha256_hex_suffix(digest.as_str()).ok_or_else(|| {
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
        let digest_hex = sha256_hex_suffix(digest).ok_or_else(|| {
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
    pub(super) fn partitioned_text_metadata(
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
        match CodeIndexPublishedGenerationV1::partitioned_text_metadata(&bytes) {
            Ok(metadata) => Ok(metadata),
            Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => Ok(None),
            Err(error) => Err(Self::corruption(error.to_string())),
        }
    }

    #[hotpath::measure(label = "code_index.generation.decode.bundle")]
    fn decode_generation_file(
        &self,
        file: &mut File,
        admitted_len: u64,
        expected_file_digest: &ManifestDigest,
        lifetime_lock: CodeGenerationStoreLockV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let monolithic = match CodeIndexPublishedGenerationV1::decode_sealed_seek_reader(
            &mut *file,
            admitted_len,
            Some(expected_file_digest),
            &UninterruptibleCodeIndexControlV1,
        ) {
            Ok(monolithic) => monolithic,
            // A generation is a pure function of its source tree, so an
            // envelope revision this build no longer reads is refused rather
            // than repaired: abstain the way an incompatible generation does
            // and let the scheduler rebuild it.
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)) => {
                tracing::warn!(
                    target: "tracedecay::code_index",
                    sealed_format_revision = revision,
                    "{}",
                    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                );
                return Ok(None);
            }
            Err(error @ CodeIndexProductionErrorV1::SealedRowContractRefused { revision, .. }) => {
                tracing::warn!(
                    target: "tracedecay::code_index",
                    sealed_format_revision = revision,
                    "{error}"
                );
                return Ok(None);
            }
            Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => return Ok(None),
            Err(error) => return Err(error),
        };
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
        match CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            &manifest,
            |request, buffer| {
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
            },
        ) {
            Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => Ok(None),
            // The manifest revision is refused for the same reason a retired
            // monolithic envelope is, and on the same terms: the generation is
            // re-derivable from its source tree, so the scheduler rebuilds it
            // instead of treating a shape this build no longer writes as
            // corruption.
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)) => {
                tracing::warn!(
                    target: "tracedecay::code_index",
                    sealed_format_revision = revision,
                    "{}",
                    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                );
                Ok(None)
            }
            result => result,
        }
    }

    /// Serve one sealed generation by identity, decoding it at most once.
    ///
    /// The active generation answers from its pinned slot. Any other generation
    /// is served from the decoded LRU, or decoded exactly once under a lease —
    /// concurrent pinned or cursor-paged readers of the same generation join the
    /// in-flight decode instead of each rescanning the store.
    pub(super) fn load_generation(
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
    pub(super) fn load_indexed_generation_shared(
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
            sha256_hex_suffix(&entry.state_digest).unwrap_or(&entry.state_digest)
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
    pub(super) fn active_already_decoded(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self.cache.lock_state()?.active.as_ref().map(Arc::clone))
    }

    /// Prove that an already-decoded serving handle still names the durable
    /// active publication without reading or decoding its sealed payload.
    pub(super) fn active_pointer_matches_generation(
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
    pub(super) fn active_pointer_covers_snapshot_content(
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
    pub(super) fn hold_active_decode(&self) -> HeldActiveDecodeV1 {
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
    pub(super) fn load_active_shared(
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

    pub(super) fn active_encoded_bytes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.active_encoded_bytes)
    }

    /// Sealed-bytes decodes this process has performed against this store.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn sealed_decode_count(&self) -> u64 {
        self.cache.decode_count()
    }

    pub(super) fn take_unpublished(&self) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        self.unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub(super) fn restore_unpublished(&self, generation: Arc<CodeIndexPublishedGenerationV1>) {
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
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        if self.undecoded_active_expectation.is_some() {
            return Ok(None);
        }
        self.load_active_shared()
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
        let undecoded_expectation = match self.undecoded_active_expectation.clone() {
            Some(expectation) => Some(expectation),
            None => {
                // Cold hydration takes the store lock itself. Complete it
                // before entering the publication transaction, then recheck
                // the exact expected pointer under the writer lock below.
                match self.load_active_shared()? {
                    Some(_) => None,
                    // An active pointer whose sealed generation this build
                    // abstains from decoding — a retired format revision, a
                    // superseded sanitizer — is still the incumbent this
                    // publication replaces, and its caller has no decoded
                    // generation id to expect. The compare-and-swap token is
                    // then the pointer identity the abstention observed,
                    // rechecked under the writer lock below; without it a
                    // store holding an undecodable generation could never be
                    // replaced by the rebuild that supersedes it.
                    None => self.read_publication_pointer()?.map(|pointer| {
                        UndecodedActivePublicationExpectationV1 {
                            generation_id: pointer.generation_id,
                            generation_file: pointer.generation_file,
                            state_digest: pointer.state_digest,
                        }
                    }),
                }
            }
        };
        let _store_lock =
            acquire_code_generation_store_lock(store_root).map_err(Self::unavailable)?;
        let prior_pointer = if let Some(expected) = undecoded_expectation.as_ref() {
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
        if undecoded_expectation.is_none()
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
        let cache_matches = undecoded_expectation.as_ref().map_or(
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
                    self.seal_checkpoint()?;
                    match publication {
                        SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                            let segment_size = u64::try_from(bytes.len()).map_err(|_| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed segment length exceeds u64".to_owned(),
                                )
                            })?;
                            hotpath::measure_block!(
                                "code_index.generation.publish.segment_durable",
                                self.publish_segment_durable(digest, bytes)
                            )
                            .map_err(|error| {
                                CodeIndexProductionErrorV1::Contract(error.to_string())
                            })?;
                            #[cfg(test)]
                            if let Some(observer) = self.seal_segment_observer.as_ref() {
                                observer();
                            }
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
                            hotpath::measure_block!(
                                "code_index.generation.publish.evidence_page_append",
                                evidence_pack.append_page(page_ordinal, page_digest, bytes)
                            )
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
                            if hotpath::measure_block!(
                                "code_index.generation.publish.evidence_commit",
                                evidence_pack.commit(
                                    &self.segments_root,
                                    segment_digest,
                                    segment_size_bytes,
                                    page_count,
                                )
                            )
                            .map_err(|error| {
                                CodeIndexProductionErrorV1::Contract(error.to_string())
                            })? {
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
            Err(CodeIndexProductionErrorV1::Interrupted(
                crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
            )) => {
                evidence_pack.rollback_unattached(&self.segments_root)?;
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
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
                sha256_hex_suffix(&state_digest).unwrap_or(&state_digest)
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
        if undecoded_expectation.is_none() {
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
pub(super) struct DaemonProjectionSinkV1;

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
