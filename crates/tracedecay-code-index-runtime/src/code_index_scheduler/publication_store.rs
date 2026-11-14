//! Durable publication store for sealed code-index generations: the shared
//! byte pool, the decoded-generation cache, and the atomic publication port.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    mem::size_of,
    num::NonZeroU64,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;

#[cfg(all(test, unix))]
thread_local! {
    static PREPARED_SEGMENT_DIGEST_OPENS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

#[cfg(not(unix))]
use same_file::Handle;
use sha2::{Digest, Sha256};
use tracedecay_application::code_index::DaemonCodeIndexControlV1;
use tracedecay_code_index_retention::code_index_generations::{
    CODE_INDEX_PREPARATION_DIRECTORY_PREFIX_V1, CodeGenerationRetentionErrorV1,
    CodeGenerationStoreLockV1, CodeIndexScopeStoreResetV1, DurableGenerationCardinalityV1,
    DurableGenerationIndexEntryV1, DurablePublicationPointerV1,
    DurableSealedCodeGenerationIdentityV1, MAX_DURABLE_GENERATION_INDEX_BYTES_V1,
    MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1, acquire_generation_segments_publication_lock,
    code_generation_segments_root, durable_generation_index_digest, reset_code_index_scope_store,
    retain_bounded_generation_index, try_acquire_code_generation_store_lock,
    try_acquire_code_generation_store_read_lock,
};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, ManifestDigest, ProjectId, SanitizerRevision, WorktreeId,
    canonical_text::encode_tagged_lowercase_hex, sha256_hex_suffix,
};
use tracedecay_graph_db::{GraphBudgetKind, GraphDbError};
use tracedecay_private_fs::framed_log::DirectorySyncPolicy;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1, ResidentMemoryComponentIdV1,
    ResidentMemoryKeyV1, ResidentMemoryOperationV1, ResidentMemoryReservationV1, ResidentOwnersV1,
    log_resident_owner_release_v1, release_process_allocator_memory_v1,
};

use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeGraphBuildBoundV1, CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1,
        CodeIndexGenerationScopeV1, CodeIndexInterruptionV1, CodeIndexPreparationPort,
        CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, CodeIndexSealedGenerationV1, CodeIndexSealedPublicationV1,
        SealedGenerationContentExtentV1, SealedGenerationContentPackV1,
        SealedGenerationPackLayoutV1, SealedGenerationPackReadFactoryV1,
        SealedGenerationPackReadPassV1, SealedGenerationSegmentIdentityV1,
        SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, SealedSegmentReaderV1,
        SharedDecodedContentPoolV1, SharedPhysicalCodeArtifactPoolV1,
        VerifiedSealedTextGenerationMetadataV1,
    },
};

use super::{CodeIndexSchedulerErrorV1, PendingHintsV1};
use crate::code_graph_seat::CodeGraphBuildAdmissionV1;

#[path = "publication_store/pack_read_pass.rs"]
mod pack_read_pass;
use pack_read_pass::{GenerationPackReadPassV1, PackReadOpenAttemptsV1};

#[cfg(all(test, unix))]
#[path = "tests/staged_segment_reads.rs"]
mod staged_segment_reads;

#[cfg(all(test, unix))]
#[path = "tests/immutable_segment_equality.rs"]
mod immutable_segment_equality;

#[cfg(all(test, unix))]
#[path = "tests/retained_segment_admission.rs"]
mod retained_segment_admission;

#[cfg(all(test, unix))]
#[path = "tests/publication_reset.rs"]
mod publication_reset;

#[cfg(all(test, unix))]
#[path = "tests/publication_pointer_validation.rs"]
mod publication_pointer_validation;

#[cfg(all(test, windows))]
#[path = "tests/publication_pointer_validation_windows.rs"]
mod publication_pointer_validation_windows;

const MAX_DURABLE_PUBLICATION_POINTER_BYTES: u64 = 512 * 1024;
const DURABLE_GENERATION_IO_CHUNK_BYTES_V1: usize = 64 * 1024;

struct SealedGraphBuildAdmissionV1(DaemonCodeIndexPublicationStoreV1);

impl CodeGraphBuildAdmissionV1 for SealedGraphBuildAdmissionV1 {
    fn admit(
        &self,
        bound: CodeGraphBuildBoundV1,
    ) -> std::result::Result<Box<dyn Send>, GraphDbError> {
        self.0
            .admit_sealed_graph_build_bound(bound)
            .map(|reservation| Box::new(reservation) as Box<dyn Send>)
            .map_err(|error| match error {
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_) => {
                    GraphDbError::budget_exhausted(
                        GraphBudgetKind::ResidentMemory,
                        tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1()
                            .get(),
                    )
                }
                error => GraphDbError::unavailable(error.to_string()),
            })
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeIndexBytePoolStatsV1 {
    pub parse_chunk_inserted: u64,
    pub parse_chunk_reused: u64,
    /// Source allocations the pool keeps, live or not.
    pub source_allocations: usize,
    /// Those some capture or generation still holds.
    pub live_sources: usize,
}

/// Captured sources by content, shared by every capture in the process.
///
/// Entries are weak, and a `Weak<[u8]>` keeps its whole allocation, bytes
/// included, after the last `Arc` dropped. Each pass therefore drops the
/// entries its capture let go of, see [`Self::release_dead_entries`].
pub struct SharedCodeIndexBytePoolV1 {
    bytes: std::sync::Mutex<BTreeMap<ContentDigest, Weak<[u8]>>>,
    pub(super) physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    /// Decoded generation pages by content, so linked worktrees that sealed
    /// identical trees hold one decode between them.
    pub(super) decoded_content: SharedDecodedContentPoolV1,
}

impl Default for SharedCodeIndexBytePoolV1 {
    fn default() -> Self {
        Self {
            bytes: Mutex::new(BTreeMap::new()),
            physical_artifacts: SharedPhysicalCodeArtifactPoolV1::default(),
            decoded_content: SharedDecodedContentPoolV1::default(),
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
            return (digest, shared);
        }
        let shared: Arc<[u8]> = Arc::from(bytes);
        pool.insert(digest.clone(), Arc::downgrade(&shared));
        (digest, shared)
    }

    /// Drop the entries no capture or generation holds any more, which frees
    /// their sources, and the physical artifact pool's likewise. A dead entry
    /// can never be reused, so this gives up nothing. Runs once a pass let go
    /// of its capture and build.
    pub(super) fn release_dead_entries(&self) {
        self.bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, entry| entry.strong_count() > 0);
        self.physical_artifacts.release_dead_entries();
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> CodeIndexBytePoolStatsV1 {
        let physical_artifacts = self.physical_artifacts.stats();
        let bytes = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        CodeIndexBytePoolStatsV1 {
            parse_chunk_inserted: physical_artifacts.inserted,
            parse_chunk_reused: physical_artifacts.reused,
            source_allocations: bytes.len(),
            live_sources: bytes
                .values()
                .filter(|entry| entry.strong_count() > 0)
                .count(),
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
/// for pointer-sized bookkeeping, never across a decode.
#[derive(Default)]
struct DecodedGenerationStateV1 {
    /// The pinned active generation.
    ///
    /// Held outside the LRU deque: every unpinned query serves from it, so LRU
    /// pressure from pinned or cursor-paged readers of older generations must
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
/// - concurrent callers wanting the SAME generation share one decode, the
///   first claims a lease, the rest park on the condvar, so a request that
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
                .retain(|subject| *subject != DecodeSubjectV1::Active);
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
    // Retain the authenticated inode, not just its digest. An ordinary
    // historical range read must not pay another endpoint-authorized open.
    file: Option<File>,
}

#[derive(Clone)]
struct UndecodedActivePublicationExpectationV1 {
    generation_id: String,
    generation_file: String,
    state_digest: String,
}

impl UndecodedActivePublicationExpectationV1 {
    fn from_pointer(pointer: &DurablePublicationPointerV1) -> Self {
        Self {
            generation_id: pointer.generation_id.clone(),
            generation_file: pointer.generation_file.clone(),
            state_digest: pointer.state_digest.clone(),
        }
    }

    fn matches(&self, pointer: &DurablePublicationPointerV1) -> bool {
        self.generation_id == pointer.generation_id
            && self.generation_file == pointer.generation_file
            && self.state_digest == pointer.state_digest
    }
}

/// The process authority and inventory a whole-generation decode is charged
/// against, bound by the owning scheduler.
#[derive(Clone)]
pub(super) struct GenerationDecodeBudgetV1 {
    pub(super) resident_memory: Arc<ProcessResidentMemoryV1>,
    pub(super) resident_owners: Arc<ResidentOwnersV1>,
    pub(super) project_id: ProjectId,
    pub(super) worktree_id: WorktreeId,
}

fn log_graph_build_bound(bound: &CodeGraphBuildBoundV1) {
    tracing::info!(
        event = "code_index_graph_build_bound",
        resolve_bytes = bound.resolve_bytes(),
        emit_bytes = bound.emit_bytes(),
        store_bytes = bound.store_bytes,
        bound = ?bound,
        "sized the sealed graph build of the active generation"
    );
}

const GENERATION_DECODE_RESIDENT_COMPONENT_V1: &str = "code-index-generation-decode-v1";
const SEALED_GRAPH_BUILD_RESIDENT_COMPONENT_V1: &str = "code-graph-sealed-build-v1";

/// What decoding one generation costs, sized on a copy this process held.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveGenerationChargesV1 {
    generation_id: CodeGenerationId,
    decode_bytes: u64,
}

/// The resident cost of materializing the active generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ActiveGenerationDecodeChargeV1 {
    /// Already in memory, or no generation is published: nothing to decode.
    Decoded,
    /// This process held it before; `bytes` is its measured resident size.
    Measured {
        generation_id: CodeGenerationId,
        bytes: u64,
    },
    /// Never held by this process, so its size is not known before decoding.
    Unmeasured,
}

#[derive(Clone)]
pub struct DaemonCodeIndexPublicationStoreV1 {
    cache: Arc<DecodedGenerationCacheV1>,
    active_encoded_bytes: Arc<AtomicU64>,
    /// Resident bytes of the active generation as last installed, kept
    /// after the decode is released: what materializing it again costs.
    active_decode_charge: Arc<Mutex<Option<ActiveGenerationChargesV1>>>,
    decode_admission: Arc<Mutex<Option<GenerationDecodeBudgetV1>>>,
    decoded_content: SharedDecodedContentPoolV1,
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
    pointer_memo: Arc<std::sync::Mutex<Option<PublicationPointerMemoV1>>>,
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
    /// Test-only: observes each batched `segments_root` directory fsync so a
    /// test can assert a single publish syncs the directory once regardless
    /// of how many new file segments it wrote.
    #[cfg(test)]
    segments_dir_sync_observer: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Retry ownership includes the staged files as well as the verified manifest.
    pub(super) unpublished_candidate: Arc<Mutex<Option<UnpublishedCodeIndexPublicationV1>>>,
    /// The active pointer's authenticated manifest, by its state digest, so
    /// builds and status over one publication read and parse it once.
    active_manifest_memo: Arc<Mutex<Option<ActiveSealedManifestV1>>>,
}

/// The durable active generation's sealed manifest and its authenticated
/// metadata, read without decoding any segment.
#[derive(Clone)]
pub(super) struct ActiveSealedManifestV1 {
    state_digest: String,
    pub(super) manifest_bytes: Arc<[u8]>,
    pub(super) metadata: Arc<AdmittedSealedMetadataV1>,
}

/// Cloned metadata shares its allocation charge and authenticated pack map.
pub(super) struct AdmittedSealedMetadataV1 {
    value: VerifiedSealedTextGenerationMetadataV1,
    layout: SealedGenerationPackLayoutV1,
    manifest_bytes: Arc<[u8]>,
    _memory: PreparedPackMemoryV1,
}

impl Deref for AdmittedSealedMetadataV1 {
    type Target = VerifiedSealedTextGenerationMetadataV1;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexPublicationDispositionV1 {
    Active,
    RetainedHistory,
}

/// A retry owns both staged bytes and the exact publication that names them.
pub(super) struct UnpublishedCodeIndexPublicationV1 {
    pub(super) preparation: DaemonCodeIndexPreparationV1,
    pub(super) publication: CodeIndexSealedPublicationV1,
}

// On Unix, directory dev/inode is the same identity used by the handle
// library, without an endpoint-authorized open for each prepare/commit check.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct UnixDirectoryIdentityV1 {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl UnixDirectoryIdentityV1 {
    fn at(path: &Path) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        let metadata = path
            .metadata()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        if !metadata.is_dir() {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "code-generation identity path is not a directory",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// An unpublished store transaction. No full store handle is retained here:
/// the current handle supplies the source fence when commit is retried.
pub struct DaemonCodeIndexPreparationV1 {
    scope: CodeIndexGenerationScopeV1,
    expected_active_generation: Option<CodeGenerationId>,
    undecoded_expectation: Option<UndecodedActivePublicationExpectationV1>,
    incumbent_bytes: Option<Vec<u8>>,
    incumbent_manifest: Option<Arc<AdmittedSealedMetadataV1>>,
    #[cfg(unix)]
    store_identity: UnixDirectoryIdentityV1,
    #[cfg(not(unix))]
    store_identity: Handle,
    #[cfg(unix)]
    segments_identity: UnixDirectoryIdentityV1,
    #[cfg(not(unix))]
    segments_identity: Handle,
    #[cfg(unix)]
    generations_identity: UnixDirectoryIdentityV1,
    #[cfg(not(unix))]
    generations_identity: Handle,
    cache: Arc<DecodedGenerationCacheV1>,
    active_path: PathBuf,
    segments_root: PathBuf,
    disposition: CodeIndexPublicationDispositionV1,
    state: Arc<Mutex<DaemonCodeIndexPreparationStateV1>>,
    shutdown_signal: Option<Arc<AtomicBool>>,
    capture_control: Option<DaemonCodeIndexControlV1>,
    encoded_segment_bytes: Arc<AtomicU64>,
    existing_segment_bytes_read: Arc<AtomicU64>,
    evidence_page_count: Arc<AtomicU64>,
    evidence_durable_transaction_count: Arc<AtomicU64>,
    #[cfg(test)]
    segment_observer: Option<Arc<dyn Fn() + Send + Sync>>,
}

enum DaemonCodeIndexPreparationStateV1 {
    Open(Box<DaemonCodeIndexPreparationFilesV1>),
    Committed(Arc<CommittedPreparationPacksV1>),
    Aborted,
}

struct BoundParentPacksV1 {
    manifest: Arc<AdmittedSealedMetadataV1>,
    read: GenerationPackReadPassV1,
    bytes_read: Arc<AtomicU64>,
}

struct CommittedPreparationPacksV1 {
    layout: Arc<AdmittedPackLayoutV1>,
    open_read_pass: Arc<SealedGenerationPackReadFactoryV1>,
}

struct AdmittedPackLayoutV1 {
    layout: SealedGenerationPackLayoutV1,
    _memory: PreparedPackMemoryV1,
}

/// Payloads close before this operation releases its shared store lease.
/// Idle factories retain only path, pointer and admitted manifest authority.
struct RetainedPackReadPassV1 {
    payloads: GenerationPackReadPassV1,
    _memory: PreparedPackMemoryV1,
    _lease: CodeGenerationStoreLockV1,
}

impl RetainedPackReadPassV1 {
    fn read(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        self.payloads.read(request, buffer)
    }
}

#[derive(Clone)]
struct GenerationReadAuthorityV1 {
    active_path: PathBuf,
    segments_root: PathBuf,
    pointer_memo: Arc<Mutex<Option<PublicationPointerMemoV1>>>,
}

fn pack_read_workspace_bytes(root: &Path, digest_bytes: usize) -> usize {
    let path_bytes = root
        .as_os_str()
        .len()
        .saturating_add(1 + "segment-".len() + 64 + ".json".len());
    // Two retained paths on every platform and cold-open pathname/string
    // temporaries can coexist. Windows identity checks also encode one path
    // as UTF-16; PathBuf growth may double its existing allocation.
    size_of::<RetainedPackReadPassV1>()
        .saturating_add(size_of::<SealedGenerationSegmentIdentityV1>().saturating_mul(2))
        .saturating_add(path_bytes.saturating_mul(8))
        .saturating_add(digest_bytes.saturating_mul(4))
}

impl GenerationReadAuthorityV1 {
    fn try_lease(
        &self,
    ) -> Result<Option<CodeGenerationStoreLockV1>, CodeIndexPublicationStoreErrorV1> {
        let root = self.active_path.parent().ok_or_else(|| {
            DaemonCodeIndexPublicationStoreV1::unavailable(
                "active code-generation pointer has no store root",
            )
        })?;
        try_acquire_code_generation_store_read_lock(root)
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)
    }

    fn open_under_lease(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        layout: &SealedGenerationPackLayoutV1,
        operation: &ResidentMemoryOperationV1,
        lease: CodeGenerationStoreLockV1,
    ) -> Result<RetainedPackReadPassV1, CodeIndexProductionErrorV1> {
        let pointer = DaemonCodeIndexPublicationStoreV1::read_publication_pointer_at(
            &self.active_path,
            &self.pointer_memo,
        )?;
        if !pointer.as_ref().is_some_and(|(pointer, _)| {
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
        let content = layout.content_pack();
        let evidence = layout.evidence_pack();
        let mut memory = PreparedPackMemoryV1::new(operation)?;
        let workspace = pack_read_workspace_bytes(
            &self.segments_root,
            content
                .digest
                .as_str()
                .len()
                .saturating_add(evidence.digest.as_str().len()),
        );
        memory.admit(workspace)?;
        let identities = [
            SealedGenerationSegmentIdentityV1 {
                digest: content.digest.clone(),
                size_bytes: content.size_bytes,
            },
            evidence.clone(),
        ];
        memory.admit(workspace.saturating_add(
            GenerationPackReadPassV1::immutable_workspace_bytes(
                &self.segments_root,
                &identities,
                None,
            )?,
        ))?;
        let mut open_attempts = PackReadOpenAttemptsV1::default();
        let payloads = GenerationPackReadPassV1::open(
            &self.segments_root,
            &identities,
            None,
            &mut open_attempts,
        )?;
        Ok(RetainedPackReadPassV1 {
            payloads,
            _memory: memory,
            _lease: lease,
        })
    }
}

struct DaemonCodeIndexPreparationFilesV1 {
    // File owners drop before the directory, including on platforms that
    // prohibit removing an open file. Retention outlives all staging cleanup.
    payload_reads: Option<GenerationPackReadPassV1>,
    parent: Option<BoundParentPacksV1>,
    logical: StagedContentV1,
    evidence: TemporaryPayloadPackV1,
    segments: StagedGenerationSegmentsV1,
    content_pack: Option<SealedGenerationContentPackV1>,
    evidence_identity: Option<SealedGenerationSegmentIdentityV1>,
    segment_read_open_attempts: PackReadOpenAttemptsV1,
    // One changed sealed pathname invalidates the entire transaction. Retrying
    // must not silently accept substituted bytes under the same digest name.
    segment_reads_poisoned: bool,
    directory: tempfile::TempDir,
    _retention: CodeGenerationStoreLockV1,
    referenced_segment_bytes: u64,
    manifest_identity: Option<String>,
    pending_pointer: Option<Vec<u8>>,
    segments_durable: bool,
    memory: PreparedPackMemoryV1,
}

impl DaemonCodeIndexPreparationFilesV1 {
    fn retained_bytes(&self) -> usize {
        #[cfg(unix)]
        let authenticated = self.segments.authenticated_segments.iter().fold(
            self.evidence
                .authenticated
                .as_ref()
                .map_or(0, |(path, _)| path.capacity()),
            |bytes, (path, _)| {
                bytes
                    .saturating_add(path.capacity())
                    .saturating_add(size_of::<(PathBuf, std::fs::Metadata)>().saturating_mul(11))
            },
        );
        #[cfg(not(unix))]
        let authenticated = 0;
        size_of::<Self>()
            .saturating_add(self.directory.path().as_os_str().len().saturating_mul(2))
            .saturating_add(self.logical.path.capacity())
            .saturating_add(self.evidence.path.capacity())
            .saturating_add(
                self.evidence
                    .published_path
                    .as_ref()
                    .map_or(0, PathBuf::capacity),
            )
            .saturating_add(
                self.parent
                    .as_ref()
                    .map_or(0, |parent| parent.read.retained_heap_bytes()),
            )
            .saturating_add(
                self.payload_reads
                    .as_ref()
                    .map_or(0, GenerationPackReadPassV1::retained_heap_bytes),
            )
            .saturating_add(self.manifest_identity.as_ref().map_or(0, String::capacity))
            .saturating_add(self.pending_pointer.as_ref().map_or(0, Vec::capacity))
            .saturating_add(authenticated)
            .saturating_add(self.logical.index_bytes(None))
            .saturating_add(
                self.content_pack
                    .as_ref()
                    .map_or(0, |pack| pack.retained_bytes()),
            )
            .saturating_add(
                self.evidence_identity
                    .as_ref()
                    .map_or(0, |pack| pack.digest.as_str().len()),
            )
            .saturating_add(self.segments.pending.iter().fold(
                0_usize,
                |bytes, (canonical, staged)| {
                    bytes
                        .saturating_add(canonical.capacity())
                        .saturating_add(staged.capacity())
                        .saturating_add(size_of::<(PathBuf, PathBuf)>().saturating_mul(11))
                },
            ))
    }

    fn admit(&mut self, additional: usize) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.memory
            .admit(self.retained_bytes().saturating_add(additional))
    }

    fn physical_identities(&self) -> Vec<SealedGenerationSegmentIdentityV1> {
        let mut identities = Vec::with_capacity(2);
        if let Some(pack) = &self.content_pack {
            identities.push(SealedGenerationSegmentIdentityV1 {
                digest: pack.digest.clone(),
                size_bytes: pack.size_bytes,
            });
        }
        if let Some(pack) = &self.evidence_identity {
            identities.push(pack.clone());
        }
        identities
    }

    fn invalidate_reads(&mut self) {
        self.segment_reads_poisoned = true;
        self.payload_reads = None;
        self.parent = None;
    }

    fn require_physical_request(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let (digest, size_bytes, offset, length) = match request {
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
        let expected = self
            .content_pack
            .as_ref()
            .filter(|pack| &pack.digest == digest)
            .map(|pack| pack.size_bytes)
            .or_else(|| {
                self.evidence_identity
                    .as_ref()
                    .filter(|pack| &pack.digest == digest)
                    .map(|pack| pack.size_bytes)
            });
        if expected != Some(size_bytes)
            || offset
                .checked_add(length)
                .is_none_or(|end| end > size_bytes)
            || usize::try_from(length).is_err()
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "prepared payload request is outside its authenticated pack identity".to_owned(),
            ));
        }
        Ok(())
    }

    fn read_physical(
        &mut self,
        root: &Path,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        buffer.clear();
        if self.segment_reads_poisoned {
            return Err(CodeIndexProductionErrorV1::Contract(
                "prepared payload identity was invalidated".to_owned(),
            ));
        }
        self.require_physical_request(request)?;
        if self.payload_reads.is_none() {
            let digest_bytes = self
                .content_pack
                .as_ref()
                .map_or(0, |pack| pack.digest.as_str().len())
                .saturating_add(
                    self.evidence_identity
                        .as_ref()
                        .map_or(0, |pack| pack.digest.as_str().len()),
                );
            let workspace = pack_read_workspace_bytes(root, digest_bytes).max(
                pack_read_workspace_bytes(self.directory.path(), digest_bytes),
            );
            self.admit(workspace)?;
            let identities = self.physical_identities();
            self.admit(workspace.saturating_add(
                GenerationPackReadPassV1::immutable_workspace_bytes(
                    root,
                    &identities,
                    Some(&self.segments.pending),
                )?,
            ))?;
            let pass = GenerationPackReadPassV1::open(
                root,
                &identities,
                Some(&self.segments.pending),
                &mut self.segment_read_open_attempts,
            );
            match pass {
                Ok(pass) => self.payload_reads = Some(pass),
                Err(error) => {
                    self.invalidate_reads();
                    return Err(error);
                }
            }
        }
        let pass = self.payload_reads.as_mut().ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("prepared payload pass is absent".to_owned())
        })?;
        let result = pass.read(request, buffer);
        if pass.is_invalidated() {
            self.invalidate_reads();
        }
        result
    }

    fn read_logical(
        &mut self,
        root: &Path,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        buffer.clear();
        if self.segment_reads_poisoned {
            return Err(CodeIndexProductionErrorV1::Contract(
                "prepared payload identity was invalidated".to_owned(),
            ));
        }
        let staged = self.logical.read(request, buffer);
        if self.logical.poisoned {
            self.invalidate_reads();
        }
        if staged? {
            return Ok(());
        }
        let digest = match request {
            SealedGenerationSegmentReadV1::Whole { digest, .. }
            | SealedGenerationSegmentReadV1::Range { digest, .. } => digest,
        };
        if self
            .evidence_identity
            .as_ref()
            .is_some_and(|pack| &pack.digest == digest)
        {
            return self.read_physical(root, request, buffer);
        }
        if let Some(parent) = self.parent.as_mut() {
            let result = parent.manifest.layout.read(request, buffer, |raw, into| {
                parent.read.read(raw, into)?;
                parent
                    .bytes_read
                    .fetch_add(into.len() as u64, Ordering::Relaxed);
                Ok(())
            });
            if parent.read.is_invalidated() {
                self.invalidate_reads();
            }
            return result;
        }
        Err(CodeIndexProductionErrorV1::Contract(
            "logical segment is absent from this preparation and its bound parent".to_owned(),
        ))
    }
}

/// The new descriptor indexes and copy scratch belong to the actual build's
/// resident-memory operation. A refused growth leaves the prior owner intact.
struct PreparedPackMemoryV1 {
    operation: ResidentMemoryOperationV1,
    component: ResidentMemoryComponentIdV1,
    charge: Option<ProcessSharedMemoryReservationV1>,
}

impl PreparedPackMemoryV1 {
    fn operation(&self) -> &ResidentMemoryOperationV1 {
        &self.operation
    }

    fn new(
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        Ok(Self {
            operation: operation.clone(),
            component: ResidentMemoryComponentIdV1::new("code_index.prepared_packs.v1")
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?,
            charge: None,
        })
    }

    fn admit(&mut self, bytes: usize) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let bytes = u64::try_from(bytes).map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        let retained = self
            .charge
            .as_ref()
            .map_or(0, |charge| charge.reserved_bytes());
        if bytes <= retained {
            return Ok(());
        }
        let additional = NonZeroU64::new(bytes - retained).ok_or_else(|| {
            DaemonCodeIndexPublicationStoreV1::unavailable("prepared pack charge overflowed")
        })?;
        let refused =
            |error: tracedecay_runtime_core::resident_memory::ResidentMemoryAdmissionFailureV1| {
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(error.to_string())
            };
        if let Some(charge) = self.charge.as_mut() {
            charge.try_grow(additional).map_err(refused)?;
        } else {
            self.charge = Some(
                self.operation
                    .reserve_process_shared(self.component, additional)
                    .map_err(refused)?,
            );
        }
        Ok(())
    }

    fn resize(&mut self, bytes: usize) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.admit(bytes)?;
        let bytes = u64::try_from(bytes).map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        if bytes == 0 {
            drop(self.charge.take());
        } else if let Some(charge) = self.charge.as_mut() {
            charge
                .shrink_to(bytes)
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct StagedContentExtentV1 {
    offset: u64,
    size_bytes: u64,
}

/// Logical writes share one unpublished append stream. Its index never names
/// a published file; finalization copies a frozen canonical roster into one
/// immutable content pack and authenticates every copied logical digest.
struct StagedContentV1 {
    file: File,
    path: PathBuf,
    extents: BTreeMap<ManifestDigest, StagedContentExtentV1>,
    digest_bytes: usize,
    size_bytes: u64,
    identity: std::fs::Metadata,
    poisoned: bool,
}

impl StagedContentV1 {
    fn create(path: PathBuf) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        let identity = file
            .metadata()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        Ok(Self {
            file,
            path,
            extents: BTreeMap::new(),
            digest_bytes: 0,
            size_bytes: 0,
            identity,
            poisoned: false,
        })
    }

    fn index_bytes(&self, next: Option<&ManifestDigest>) -> usize {
        let entries = self
            .extents
            .len()
            .saturating_add(usize::from(next.is_some()));
        // std's degree-six BTreeMap has eleven entries per node, at least
        // five occupied entries per non-root node, and twelve child slots.
        let nodes = if entries == 0 {
            0
        } else {
            entries.saturating_sub(1) / 5 + 1
        };
        let leaf = size_of::<usize>().saturating_mul(2).saturating_add(
            size_of::<(ManifestDigest, StagedContentExtentV1)>().saturating_mul(11),
        );
        nodes
            .saturating_mul(leaf)
            .saturating_add(
                (nodes.saturating_add(3) / 6)
                    .saturating_mul(12)
                    .saturating_mul(size_of::<usize>()),
            )
            .saturating_add(self.digest_bytes)
            .saturating_add(next.map_or(0, |digest| digest.as_str().len()))
    }

    fn unchanged(&self) -> bool {
        let Ok(opened) = self.file.metadata() else {
            return false;
        };
        #[cfg(unix)]
        {
            self.path.symlink_metadata().is_ok_and(|named| {
                DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(
                    &self.identity,
                    &opened,
                ) && DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(
                    &self.identity,
                    &named,
                )
            })
        }
        #[cfg(not(unix))]
        {
            opened.is_file()
                && opened.len() == self.identity.len()
                && opened.modified().ok() == self.identity.modified().ok()
        }
    }

    fn append(
        &mut self,
        digest: &ManifestDigest,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if self.poisoned || !self.unchanged() {
            self.poisoned = true;
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "logical staging stream changed before append",
            ));
        }
        let size_bytes =
            u64::try_from(bytes.len()).map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        if DaemonCodeIndexPublicationStoreV1::state_digest(bytes) != digest.as_str() {
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "logical segment bytes do not match their content address",
            ));
        }
        if let Some(existing) = self.extents.get(digest) {
            if existing.size_bytes != size_bytes {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "logical staging address has conflicting extents",
                ));
            }
            return Ok(());
        }
        let end = self.size_bytes.checked_add(size_bytes).ok_or_else(|| {
            DaemonCodeIndexPublicationStoreV1::unavailable(
                "logical staging stream length overflowed",
            )
        })?;
        self.poisoned = true;
        self.file
            .seek(SeekFrom::Start(self.size_bytes))
            .and_then(|_| self.file.write_all(bytes))
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        self.identity = self
            .file
            .metadata()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        if self.identity.len() != end || !self.unchanged() {
            return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                "logical staging stream changed during append",
            ));
        }
        self.extents.insert(
            digest.clone(),
            StagedContentExtentV1 {
                offset: self.size_bytes,
                size_bytes,
            },
        );
        self.digest_bytes = self.digest_bytes.saturating_add(digest.as_str().len());
        self.size_bytes = end;
        self.poisoned = false;
        Ok(())
    }

    fn read(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        buffer.clear();
        if self.poisoned || !self.unchanged() {
            self.poisoned = true;
            return Err(CodeIndexProductionErrorV1::Contract(
                "logical staging stream was invalidated".to_owned(),
            ));
        }
        let (digest, size_bytes, offset, length, whole) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest, size_bytes, 0, size_bytes, true)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest, size_bytes, offset, length, false),
        };
        let Some(extent) = self.extents.get(digest) else {
            return Ok(false);
        };
        if extent.size_bytes != size_bytes
            || offset
                .checked_add(length)
                .is_none_or(|end| end > size_bytes)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "logical staging range disagrees with its authenticated extent".to_owned(),
            ));
        }
        let position = extent.offset.checked_add(offset).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("logical staging offset overflowed".to_owned())
        })?;
        let length = usize::try_from(length).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "logical staging range exceeds addressable memory".to_owned(),
            )
        })?;
        buffer.resize(length, 0);
        let result = self
            .file
            .seek(SeekFrom::Start(position))
            .and_then(|_| self.file.read_exact(buffer));
        if !self.unchanged() || result.is_err() {
            buffer.clear();
            self.poisoned = true;
            return Err(CodeIndexProductionErrorV1::Contract(
                "logical staging stream changed or failed during read".to_owned(),
            ));
        }
        if whole && DaemonCodeIndexPublicationStoreV1::state_digest(buffer) != digest.as_str() {
            buffer.clear();
            self.poisoned = true;
            return Err(CodeIndexProductionErrorV1::Contract(
                "logical staged bytes do not match their content address".to_owned(),
            ));
        }
        Ok(true)
    }
}

impl DaemonCodeIndexPreparationV1 {
    fn closed() -> CodeIndexPublicationStoreErrorV1 {
        DaemonCodeIndexPublicationStoreV1::unavailable("code-index preparation is closed")
    }

    fn cache_matches(
        &self,
        state: &DecodedGenerationStateV1,
        installed_generation: Option<&CodeGenerationId>,
    ) -> bool {
        state.active.as_ref().is_none_or(|current| {
            let cached = &current.manifest().generation_id;
            installed_generation == Some(cached)
                || self.undecoded_expectation.as_ref().map_or(
                    Some(cached) == self.expected_active_generation.as_ref(),
                    |expected| cached.as_str() == expected.generation_id.as_str(),
                )
        })
    }

    #[cfg(all(test, unix))]
    pub(super) fn staged_segment_read_open_attempts_for_test(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            DaemonCodeIndexPreparationStateV1::Open(files) => {
                files.segment_read_open_attempts.payload
            }
            _ => 0,
        }
    }

    fn invalidate_uncommitted(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(&*state, DaemonCodeIndexPreparationStateV1::Open(_)) {
            // Escaped readers keep only an invalidated cell. Standard tempfile
            // ownership removes staging while this drops the retention lease.
            *state = DaemonCodeIndexPreparationStateV1::Aborted;
        }
    }

    fn abort_open(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut files = {
            let mut state = self.state.lock().map_err(|_| Self::closed())?;
            if !matches!(&*state, DaemonCodeIndexPreparationStateV1::Open(_)) {
                return Ok(());
            }
            match std::mem::replace(&mut *state, DaemonCodeIndexPreparationStateV1::Aborted) {
                DaemonCodeIndexPreparationStateV1::Open(files) => files,
                _ => return Err(Self::closed()),
            }
        };
        // Rollback can unlink a staged pack. Close its active read owners first,
        // while the owning struct still retains the directory and retention lease.
        drop(files.payload_reads.take());
        drop(files.parent.take());
        // If rollback fails, the remaining field order keeps staging cleanup
        // inside the retention lease as well.
        files.evidence.rollback_unattached(files.directory.path())?;
        let DaemonCodeIndexPreparationFilesV1 {
            directory,
            segments,
            evidence,
            logical,
            parent,
            payload_reads,
            _retention,
            ..
        } = *files;
        // Release pinned descriptors before staged cleanup so a failed
        // preparation cannot retain unlinked sealed content.
        drop(payload_reads);
        drop(parent);
        drop(logical);
        drop(evidence);
        drop(segments);
        let result = directory
            .close()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable);
        drop(_retention);
        result
    }
}

impl Drop for DaemonCodeIndexPreparationV1 {
    fn drop(&mut self) {
        self.invalidate_uncommitted();
    }
}

impl CodeIndexPreparationPort for DaemonCodeIndexPreparationV1 {
    fn bind_parent(
        &mut self,
        parent: &CodeIndexSealedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut state = self.state.lock().map_err(|_| Self::closed())?;
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *state else {
            return Err(Self::closed());
        };
        if files.parent.is_some()
            || !files.logical.extents.is_empty()
            || files.evidence.size_bytes != 0
            || files.manifest_identity.is_some()
        {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "parent binding must precede publication staging",
            ));
        }
        let manifest = self
            .incumbent_manifest
            .as_ref()
            .ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?;
        if self.expected_active_generation.as_ref() != Some(&manifest.manifest().generation_id)
            || manifest.manifest_bytes.as_ref() != parent.manifest_bytes()
            || manifest.sealed_scope() != self.scope
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let content = manifest.layout.content_pack();
        let evidence = manifest.layout.evidence_pack();
        let workspace = pack_read_workspace_bytes(
            &self.segments_root,
            content
                .digest
                .as_str()
                .len()
                .saturating_add(evidence.digest.as_str().len()),
        );
        files.admit(workspace)?;
        let identities = [
            SealedGenerationSegmentIdentityV1 {
                digest: content.digest.clone(),
                size_bytes: content.size_bytes,
            },
            evidence.clone(),
        ];
        files.admit(
            workspace.saturating_add(
                GenerationPackReadPassV1::immutable_workspace_bytes(
                    &self.segments_root,
                    &identities,
                    None,
                )
                .map_err(DaemonCodeIndexPublicationStoreV1::corruption)?,
            ),
        )?;
        // The preparation already holds the shared segment-retention lease.
        // Re-entering the main store lease here would invert commit ordering.
        let read = GenerationPackReadPassV1::open(
            &self.segments_root,
            &identities,
            None,
            &mut files.segment_read_open_attempts,
        )
        .map_err(|error| {
            files.segment_reads_poisoned = true;
            DaemonCodeIndexPublicationStoreV1::corruption(error)
        })?;
        files.parent = Some(BoundParentPacksV1 {
            manifest: Arc::clone(manifest),
            read,
            bytes_read: Arc::clone(&self.existing_segment_bytes_read),
        });
        Ok(())
    }

    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if let Err(error) = DaemonCodeIndexPublicationStoreV1::publication_checkpoint(
            self.shutdown_signal.as_deref(),
            self.capture_control.as_ref(),
        ) {
            if matches!(&error, CodeIndexPublicationStoreErrorV1::CompareAndSwap) {
                self.invalidate_uncommitted();
            }
            return Err(error);
        }
        let mut state = self.state.lock().map_err(|_| Self::closed())?;
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *state else {
            return Err(Self::closed());
        };
        if files.segment_reads_poisoned || files.logical.poisoned {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "staged sealed segment identity was invalidated",
            ));
        }
        if files.manifest_identity.is_some() || files.content_pack.is_some() {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "code-index preparation already entered commit",
            ));
        }
        let (digest, bytes, is_file, durable_span) = match publication {
            SealedGenerationSegmentPublicationV1::File { digest, bytes } => (
                digest,
                bytes,
                true,
                tracing::trace_span!("code_index.generation.publish.segment_durable"),
            ),
            SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
            | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => (
                digest,
                bytes,
                false,
                tracing::trace_span!("code_index.generation.publish.file_evidence_durable"),
            ),
            SealedGenerationSegmentPublicationV1::CodeGraphPage {
                page_digest, bytes, ..
            } => (
                page_digest,
                bytes,
                false,
                tracing::trace_span!("code_index.generation.publish.graph_page_durable"),
            ),
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal,
                page_digest,
                bytes,
            } => {
                let _span =
                    tracing::trace_span!("code_index.generation.publish.evidence_page_append")
                        .entered();
                files
                    .evidence
                    .append_page(page_ordinal, page_digest, bytes)?;
                self.evidence_page_count.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                segment_size_bytes,
                page_count,
            } => {
                let _span = tracing::trace_span!(
                    "code_index.generation.publish.evidence_commit",
                    bytes = segment_size_bytes,
                    pages = page_count,
                )
                .entered();
                if files.evidence_identity.is_some() {
                    return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                        "generation evidence is already finalized",
                    ));
                }
                files.admit(
                    segment_digest
                        .as_str()
                        .len()
                        .saturating_add(self.segments_root.as_os_str().len().saturating_mul(4))
                        .saturating_add(files.directory.path().as_os_str().len().saturating_mul(4))
                        .saturating_add(size_of::<(PathBuf, PathBuf)>().saturating_mul(11)),
                )?;
                // The existing pack authority validates every page and the
                // complete digest, but names it only inside this session.
                if files.evidence.commit(
                    &self.segments_root,
                    files.directory.path(),
                    segment_digest,
                    segment_size_bytes,
                    page_count,
                )? {
                    self.evidence_durable_transaction_count
                        .fetch_add(1, Ordering::Relaxed);
                    let suffix = sha256_hex_suffix(segment_digest.as_str()).ok_or_else(|| {
                        DaemonCodeIndexPublicationStoreV1::unavailable(
                            "sealed evidence pack digest is not sha256",
                        )
                    })?;
                    let name = format!("segment-{suffix}.json");
                    let path = files.directory.path().join(&name);
                    // commit already synced the complete pack. Other segment
                    // files use the shared batch flush before canonical naming.
                    files
                        .segments
                        .pending
                        .insert(self.segments_root.join(name), path);
                    files.evidence.attach_to_manifest();
                }
                #[cfg(unix)]
                if let Some((path, identity)) = files.evidence.authenticated.take() {
                    files.segments.authenticated_segments.insert(path, identity);
                }
                files.evidence_identity = Some(SealedGenerationSegmentIdentityV1 {
                    digest: segment_digest.clone(),
                    size_bytes: segment_size_bytes,
                });
                files.referenced_segment_bytes = segment_size_bytes;
                return Ok(());
            }
        };
        let segment_size =
            u64::try_from(bytes.len()).map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        {
            let _span = durable_span.entered();
            let additional = if files.logical.extents.contains_key(digest) {
                0
            } else {
                files
                    .logical
                    .index_bytes(Some(digest))
                    .saturating_sub(files.logical.index_bytes(None))
            };
            files.admit(additional)?;
            files.logical.append(digest, bytes)?;
        }
        if is_file {
            self.encoded_segment_bytes
                .fetch_add(segment_size, Ordering::Relaxed);
            #[cfg(test)]
            if let Some(observer) = self.segment_observer.as_ref() {
                observer();
            }
        }
        Ok(())
    }

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let state = Arc::clone(&self.state);
        let root = self.segments_root.clone();
        Arc::new(move |request, buffer: &mut Vec<u8>| {
            buffer.clear();
            let mut state = state
                .lock()
                .map_err(|_| CodeIndexProductionErrorV1::Publication(Self::closed()))?;
            match &mut *state {
                DaemonCodeIndexPreparationStateV1::Aborted => {
                    Err(CodeIndexProductionErrorV1::Publication(Self::closed()))
                }
                DaemonCodeIndexPreparationStateV1::Open(files) => {
                    files.read_logical(&root, request, buffer)
                }
                DaemonCodeIndexPreparationStateV1::Committed(committed) => {
                    let read = (committed.open_read_pass)()?;
                    committed.layout.layout.read(request, buffer, read.as_ref())
                }
            }
        })
    }

    fn finalize_content_pack(
        &mut self,
        identities: &[SealedGenerationSegmentIdentityV1],
    ) -> Result<SealedGenerationContentPackV1, CodeIndexPublicationStoreErrorV1> {
        let mut state = self.state.lock().map_err(|_| Self::closed())?;
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *state else {
            return Err(Self::closed());
        };
        if files.content_pack.is_some()
            || files.evidence_identity.is_none()
            || files.segment_reads_poisoned
            || files.logical.poisoned
        {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "content pack finalization is not admitted",
            ));
        }
        let mut total = 0_u64;
        let mut previous = None;
        let mut descriptor_bytes = size_of::<SealedGenerationContentPackV1>().saturating_add(
            identities
                .len()
                .saturating_mul(size_of::<SealedGenerationContentExtentV1>()),
        );
        for identity in identities {
            if previous.is_some_and(|prior: &ManifestDigest| prior >= &identity.digest) {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "content pack roster is not strictly canonical",
                ));
            }
            previous = Some(&identity.digest);
            total = total.checked_add(identity.size_bytes).ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable("content pack length overflowed")
            })?;
            descriptor_bytes = descriptor_bytes.saturating_add(identity.digest.as_str().len());
        }
        let scratch_bytes = usize::try_from(total.min(DURABLE_GENERATION_IO_CHUNK_BYTES_V1 as u64))
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        files.admit(
            descriptor_bytes
                .saturating_mul(2)
                .saturating_add(scratch_bytes)
                .saturating_add(self.segments_root.as_os_str().len().saturating_mul(8))
                .saturating_add(files.directory.path().as_os_str().len().saturating_mul(8)),
        )?;
        let mut content =
            TemporaryPayloadPackV1::create(files.directory.path().join("content-pack.tmp"))?;
        let mut scratch = Vec::with_capacity(scratch_bytes);
        let mut extents = Vec::with_capacity(identities.len());
        let mut offset = 0_u64;
        for identity in identities {
            DaemonCodeIndexPublicationStoreV1::publication_checkpoint(
                self.shutdown_signal.as_deref(),
                self.capture_control.as_ref(),
            )?;
            let mut digest = Sha256::new();
            let mut copied = 0_u64;
            while copied < identity.size_bytes {
                DaemonCodeIndexPublicationStoreV1::publication_checkpoint(
                    self.shutdown_signal.as_deref(),
                    self.capture_control.as_ref(),
                )?;
                let length =
                    (identity.size_bytes - copied).min(DURABLE_GENERATION_IO_CHUNK_BYTES_V1 as u64);
                files
                    .read_logical(
                        &self.segments_root,
                        SealedGenerationSegmentReadV1::Range {
                            digest: &identity.digest,
                            size_bytes: identity.size_bytes,
                            offset: copied,
                            length,
                        },
                        &mut scratch,
                    )
                    .map_err(|error| match error {
                        CodeIndexProductionErrorV1::Publication(error) => error,
                        error => DaemonCodeIndexPublicationStoreV1::corruption(error),
                    })?;
                if scratch.len() as u64 != length {
                    files.invalidate_reads();
                    return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                        "content pack copy returned an incomplete logical extent",
                    ));
                }
                digest.update(&scratch);
                content.append_bytes(&scratch)?;
                copied += length;
            }
            let actual = ManifestDigest::from_sha256_bytes(&digest.finalize())
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            if actual != identity.digest {
                files.invalidate_reads();
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "copied content extent does not match its logical digest",
                ));
            }
            extents.push(SealedGenerationContentExtentV1 {
                digest: identity.digest.clone(),
                offset,
                size_bytes: identity.size_bytes,
            });
            offset = offset.checked_add(identity.size_bytes).ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable("content pack offset overflowed")
            })?;
        }
        let descriptor = SealedGenerationContentPackV1 {
            digest: content.digest()?,
            size_bytes: total,
            extents,
        };
        descriptor
            .validate(identities)
            .map_err(DaemonCodeIndexPublicationStoreV1::corruption)?;
        let created = content.commit(
            &self.segments_root,
            files.directory.path(),
            &descriptor.digest,
            descriptor.size_bytes,
            0,
        )?;
        if created {
            let suffix = sha256_hex_suffix(descriptor.digest.as_str()).ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::corruption("content pack digest is not sha256")
            })?;
            let name = format!("segment-{suffix}.json");
            files.segments.pending.insert(
                self.segments_root.join(&name),
                files.directory.path().join(name),
            );
            content.attach_to_manifest();
        }
        #[cfg(unix)]
        if let Some((path, identity)) = content.authenticated.take() {
            files.segments.authenticated_segments.insert(path, identity);
        }
        // A prior evidence-only pass has at most one role. Replace it once
        // before the full finalized pass; never reopen per logical extent.
        files.payload_reads = None;
        files.content_pack = Some(descriptor.clone());
        files.referenced_segment_bytes = descriptor.size_bytes.saturating_add(
            files
                .evidence_identity
                .as_ref()
                .filter(|pack| pack.digest != descriptor.digest)
                .map_or(0, |pack| pack.size_bytes),
        );
        Ok(descriptor)
    }

    fn pack_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let state = Arc::clone(&self.state);
        let root = self.segments_root.clone();
        Arc::new(move |request, buffer: &mut Vec<u8>| {
            buffer.clear();
            let mut state = state
                .lock()
                .map_err(|_| CodeIndexProductionErrorV1::Publication(Self::closed()))?;
            match &mut *state {
                DaemonCodeIndexPreparationStateV1::Open(files) => {
                    files.read_physical(&root, request, buffer)
                }
                DaemonCodeIndexPreparationStateV1::Committed(committed) => {
                    let read = (committed.open_read_pass)()?;
                    read(request, buffer)
                }
                DaemonCodeIndexPreparationStateV1::Aborted => {
                    Err(CodeIndexProductionErrorV1::Publication(Self::closed()))
                }
            }
        })
    }

    fn read_pass_factory(&self) -> Arc<SealedGenerationPackReadFactoryV1> {
        let state = Arc::clone(&self.state);
        Arc::new(move || {
            let factory = {
                let state = state
                    .lock()
                    .map_err(|_| CodeIndexProductionErrorV1::Publication(Self::closed()))?;
                match &*state {
                    DaemonCodeIndexPreparationStateV1::Committed(committed) => {
                        Arc::clone(&committed.open_read_pass)
                    }
                    _ => return Err(CodeIndexProductionErrorV1::Publication(Self::closed())),
                }
            };
            factory()
        })
    }

    fn abort(self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.abort_open()
    }
}

pub(super) struct TemporaryPayloadPackV1 {
    path: PathBuf,
    file: Option<File>,
    hasher: Sha256,
    size_bytes: u64,
    page_count: u32,
    committed: bool,
    published_path: Option<PathBuf>,
    #[cfg(unix)]
    authenticated: Option<(PathBuf, std::fs::Metadata)>,
}

/// One seal's new file segments. Each keeps a temporary name until the whole
/// batch is durable, so a content-addressed segment name always holds its
/// complete bytes, even across a power loss mid-seal.
struct StagedGenerationSegmentsV1 {
    /// Temporary path of every written segment, keyed by its final name.
    pending: BTreeMap<PathBuf, PathBuf>,
    // Canonical files and newly written temporaries are authenticated by
    // their original opened handles. Commit rechecks inode identity rather
    // than reopening and hashing already authenticated segment pathnames.
    #[cfg(unix)]
    authenticated_segments: BTreeMap<PathBuf, std::fs::Metadata>,
}

impl StagedGenerationSegmentsV1 {
    fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            #[cfg(unix)]
            authenticated_segments: BTreeMap::new(),
        }
    }

    // A staged inode was authenticated before the collision read. The
    // comparator proves two CURRENT pathnames match; it cannot prove that a
    // staged pathname still names the original authenticated bytes.
    fn validate_authenticated_temporary(
        temporary: &Path,
        authenticated: Option<&std::fs::Metadata>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        #[cfg(unix)]
        if let Some(original) = authenticated {
            let current = temporary
                .symlink_metadata()
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            if !DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(original, &current) {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "staged sealed segment changed during canonical deduplication",
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = (temporary, authenticated);
        Ok(())
    }

    // A concurrent publisher can create the final name after our missing-path
    // probe. Never replace an immutable content-addressed file in that gap.
    fn publish_missing_immutable_file(
        temporary: &Path,
        final_path: &Path,
        authenticated_temporary: Option<&std::fs::Metadata>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        match tracedecay_private_fs::framed_log::rename_noreplace(temporary, final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = final_path
                    .symlink_metadata()
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
                if !metadata.file_type().is_file()
                    || !DaemonCodeIndexPublicationStoreV1::files_equal(final_path, temporary)?
                {
                    return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                        "concurrently published immutable code-index file does not match its content address",
                    ));
                }
                Self::validate_authenticated_temporary(temporary, authenticated_temporary)?;
                std::fs::remove_file(temporary)
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)
            }
            Err(error) => Err(DaemonCodeIndexPublicationStoreV1::unavailable(error)),
        }
    }

    /// Name the already-fsynced payload packs without replacing an existing
    /// content address. A retry resumes only the remaining canonical names.
    fn publish(&mut self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        while let Some((final_path, temporary)) = self
            .pending
            .first_key_value()
            .map(|(final_path, temporary)| (final_path.clone(), temporary.clone()))
        {
            // Both payload roles are fsynced before they enter pending.
            // Their original writer witness must still authorize this name.
            // Bind every pending pathname to the bytes authenticated during
            // staging before any immutable canonical name is published.
            #[cfg(unix)]
            let authenticated =
                Some(self.authenticated_segments.get(&temporary).ok_or_else(|| {
                    DaemonCodeIndexPublicationStoreV1::corruption(
                        "staged sealed segment lacks its authenticated inode",
                    )
                })?);
            #[cfg(not(unix))]
            let authenticated = None;
            Self::validate_authenticated_temporary(&temporary, authenticated)?;
            match final_path.symlink_metadata() {
                Ok(metadata) => {
                    if !metadata.file_type().is_file()
                        || !DaemonCodeIndexPublicationStoreV1::files_equal(&final_path, &temporary)?
                    {
                        return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                            "existing sealed segment does not match its content address",
                        ));
                    }
                    // The equality read can observe a new, byte-identical
                    // temporary inode after our pre-comparison witness check.
                    // Never discard the authenticated staged copy on that basis.
                    Self::validate_authenticated_temporary(&temporary, authenticated)?;
                    std::fs::remove_file(&temporary)
                        .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Self::publish_missing_immutable_file(&temporary, &final_path, authenticated)?;
                }
                Err(error) => return Err(DaemonCodeIndexPublicationStoreV1::unavailable(error)),
            }
            self.pending.remove(&final_path);
        }
        Ok(())
    }
}

impl Drop for StagedGenerationSegmentsV1 {
    fn drop(&mut self) {
        for temporary in self.pending.values() {
            let _ = std::fs::remove_file(temporary);
        }
    }
}

struct PinnedGenerationSegmentV1 {
    digest: String,
    size_bytes: u64,
    file: PinnedGenerationFileV1,
    admitted_metadata: std::fs::Metadata,
}

#[cfg(not(windows))]
type PinnedGenerationFileV1 = File;

/// Only a payload read pass uses immutable sharing. Short manifest reads
/// preserve their existing opener and do not acquire a persistent route lease.
#[cfg(windows)]
enum PinnedGenerationFileV1 {
    Shared(File),
    Immutable(tracedecay_private_fs::windows::ImmutableReadFile),
}

#[cfg(windows)]
impl std::ops::Deref for PinnedGenerationFileV1 {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Shared(file) => file,
            Self::Immutable(owner) => owner.file(),
        }
    }
}

impl TemporaryPayloadPackV1 {
    pub(super) fn create(path: PathBuf) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true).read(true);
        let file = options
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
            #[cfg(unix)]
            authenticated: None,
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
        self.append_bytes(bytes)?;
        self.page_count = self.page_count.checked_add(1).ok_or_else(|| {
            DaemonCodeIndexPublicationStoreV1::unavailable(
                "sealed evidence pack page count exceeds u32",
            )
        })?;
        Ok(())
    }

    fn append_bytes(&mut self, bytes: &[u8]) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if self.committed {
            return Err(DaemonCodeIndexPublicationStoreV1::unavailable(
                "sealed payload pack is already committed",
            ));
        }
        let next_size = self
            .size_bytes
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?,
            )
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed payload pack length exceeds u64",
                )
            })?;
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
        self.size_bytes = next_size;
        Ok(())
    }

    fn digest(&self) -> Result<ManifestDigest, CodeIndexPublicationStoreErrorV1> {
        ManifestDigest::from_sha256_bytes(&self.hasher.clone().finalize())
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)
    }

    fn commit(
        &mut self,
        root: &Path,
        staging_root: &Path,
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
        let existing = {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_lookup").entered();
            final_path.symlink_metadata()
        };
        match existing {
            Ok(metadata) => {
                let _span =
                    tracing::trace_span!("code_index.generation.publish.evidence_reuse").entered();
                // Reject non-files and mismatched extents before opening the
                // canonical name: a FIFO must never block digest verification.
                if !metadata.file_type().is_file() || metadata.len() != segment_size_bytes {
                    return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                        "existing sealed evidence pack does not match its content address",
                    ));
                }
                #[cfg(unix)]
                let actual = DaemonCodeIndexPublicationStoreV1::state_digest_existing_segment(
                    &final_path,
                    &metadata,
                )?;
                #[cfg(not(unix))]
                let actual = DaemonCodeIndexPublicationStoreV1::state_digest_file(&final_path)?;
                if actual != segment_digest.as_str() {
                    return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                        "existing sealed evidence pack does not match its content address",
                    ));
                }
                // The same opened inode verified by SHA-256 is authoritative
                // for this preparation's subsequent manifest verification.
                #[cfg(unix)]
                {
                    self.authenticated = Some((final_path, metadata));
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
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_file_sync").entered();
            self.file
                .as_ref()
                .ok_or_else(|| {
                    DaemonCodeIndexPublicationStoreV1::unavailable(
                        "sealed evidence pack temporary file is already closed",
                    )
                })?
                .sync_all()
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        }
        // On Unix, keep the original writer descriptor through the rename
        // and hash the actual published bytes. Opening the pack by pathname
        // again would trigger endpoint authorization for no additional proof.
        #[cfg(unix)]
        let before_rename = self
            .file
            .as_ref()
            .ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack writer is already closed",
                )
            })?
            .metadata()
            .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        #[cfg(not(unix))]
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_file_close").entered();
            drop(self.file.take());
        }
        let staged_path = staging_root.join(format!("segment-{digest_hex}.json"));
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_rename").entered();
            std::fs::rename(&self.path, &staged_path)
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
        }
        self.published_path = Some(staged_path.clone());
        #[cfg(unix)]
        {
            let file = self.file.as_mut().ok_or_else(|| {
                DaemonCodeIndexPublicationStoreV1::unavailable(
                    "sealed evidence pack writer is already closed",
                )
            })?;
            let opened = file
                .metadata()
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            // Rename may change ctime, but must not change the writer inode.
            if !opened.is_file()
                || opened.dev() != before_rename.dev()
                || opened.ino() != before_rename.ino()
                || opened.len() != before_rename.len()
                || opened.mtime() != before_rename.mtime()
                || opened.mtime_nsec() != before_rename.mtime_nsec()
            {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "sealed evidence pack writer changed during staging",
                ));
            }
            file.seek(SeekFrom::Start(0))
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            if DaemonCodeIndexPublicationStoreV1::state_digest_reader(file)?
                != segment_digest.as_str()
            {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "staged evidence pack bytes differ from the authenticated content address",
                ));
            }
            let after = file
                .metadata()
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            let named = staged_path
                .symlink_metadata()
                .map_err(DaemonCodeIndexPublicationStoreV1::unavailable)?;
            if !DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(&opened, &after)
                || !DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(&after, &named)
            {
                return Err(DaemonCodeIndexPublicationStoreV1::corruption(
                    "sealed evidence pack changed during writer authentication",
                ));
            }
            self.authenticated = Some((staged_path, after));
        }
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_file_close").entered();
            drop(self.file.take());
        }
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.evidence_directory_sync")
                    .entered();
            DaemonCodeIndexPublicationStoreV1::sync_directory(staging_root)?;
        }
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

impl Drop for TemporaryPayloadPackV1 {
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

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn new(
        store_root: &Path,
        project_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let generations_root = store_root.join("code-generations-v1");
        std::fs::create_dir_all(&generations_root)?;
        let segments_root = code_generation_segments_root(store_root);
        std::fs::create_dir_all(&segments_root)?;
        let _store_lock = try_acquire_code_generation_store_lock(store_root)
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .ok_or(CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::StoreLockContended,
            ))?;
        // Scope reconciliation only sees this directory's hash; the record
        // lets it collect the scope as soon as the checkout is deleted rather
        // than after the stranding age.
        tracedecay_code_index_retention::code_index_generations::record_scope_root(
            store_root,
            project_root,
        )?;
        Self::remove_abandoned_publication_temporaries(&segments_root, store_root)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            cache: Arc::new(DecodedGenerationCacheV1::default()),
            active_encoded_bytes: Arc::new(AtomicU64::new(0)),
            active_decode_charge: Arc::new(Mutex::new(None)),
            decode_admission: Arc::new(Mutex::new(None)),
            decoded_content: SharedDecodedContentPoolV1::default(),
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
            pointer_memo: Arc::new(Mutex::new(None)),
            undecoded_active_expectation: None,
            reconcile_publication_fence: None,
            shutdown_signal: None,
            #[cfg(test)]
            seal_segment_observer: None,
            #[cfg(test)]
            segments_dir_sync_observer: None,
            unpublished_candidate: Arc::new(Mutex::new(None)),
            active_manifest_memo: Arc::new(Mutex::new(None)),
        })
    }

    pub(super) fn for_undecoded_active_rebuild(
        &self,
        pointer: &DurablePublicationPointerV1,
    ) -> Self {
        let mut publication = self.clone();
        publication.undecoded_active_expectation = Some(
            UndecodedActivePublicationExpectationV1::from_pointer(pointer),
        );
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

    /// Decode through the registry's shared content pool, so this store
    /// references pages another worktree already decoded.
    pub(super) fn with_decoded_content(
        mut self,
        decoded_content: SharedDecodedContentPoolV1,
    ) -> Self {
        self.decoded_content = decoded_content;
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

    #[cfg(test)]
    pub(super) fn with_segments_dir_sync_observer_for_test(
        mut self,
        observer: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.segments_dir_sync_observer = Some(observer);
        self
    }

    /// The seal encodes and writes one segment per file, so a
    /// generation-sized worktree spends seconds here with no other
    /// cancellation point. Daemon shutdown retires the worktree's shutdown
    /// signal and a retained rebuild's supersession retires its fence;
    /// checking both before every segment keeps the blocking reconcile pass
    /// joinable inside the shutdown budget instead of forcing the coordinator
    /// to abandon it and the runtime teardown to wait for it again.
    fn seal_checkpoint(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        Self::publication_checkpoint(
            self.shutdown_signal.as_deref(),
            self.reconcile_publication_fence
                .as_ref()
                .map(|(_, control)| control),
        )
    }

    fn publication_checkpoint(
        shutdown_signal: Option<&AtomicBool>,
        control: Option<&DaemonCodeIndexControlV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if shutdown_signal.is_some_and(|signal| signal.load(Ordering::Acquire)) {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if let Some(control) = control {
            // Sampling cancellation first latches a resident-watermark trip.
            // The control preserves shutdown and source-supersession precedence
            // even when an earlier sample already refused memory.
            if control.is_cancelled() {
                return Err(if control.refused_by_resident_memory() {
                    CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(
                        "resident memory watermark refused code-index publication".to_owned(),
                    )
                } else {
                    CodeIndexPublicationStoreErrorV1::CompareAndSwap
                });
            }
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

    /// A pointer slot that is not a regular file is a corrupt authority.
    ///
    /// `read(2)` and `rename(2)` both report that shape as `EISDIR`. Mapping
    /// the OS error to `Unavailable` (or letting it surface as a raw I/O
    /// fault) misclassifies a broken publication pointer. Callers in the
    /// scheduler publication family must see reset-required corruption.
    fn corrupt_non_file_pointer() -> CodeIndexPublicationStoreErrorV1 {
        Self::corruption("active code-generation pointer is not a regular file")
    }

    fn map_pointer_io(error: std::io::Error) -> CodeIndexPublicationStoreErrorV1 {
        if error.kind() == std::io::ErrorKind::IsADirectory {
            Self::corrupt_non_file_pointer()
        } else {
            Self::unavailable(error)
        }
    }

    fn require_regular_pointer_slot(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        // A dangling or same-inode symlink is still not a pointer slot.
        // Inspect the directory entry without another endpoint-authorized open.
        match std::fs::symlink_metadata(&self.active_path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(()),
            Ok(_) => Err(Self::corrupt_non_file_pointer()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Self::map_pointer_io(error)),
        }
    }

    fn acquire_generation_read_lock(
        &self,
    ) -> Result<CodeGenerationStoreLockV1, CodeIndexPublicationStoreErrorV1> {
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        try_acquire_code_generation_store_read_lock(store_root)
            .map_err(Self::unavailable)?
            .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)
    }

    /// Only this scope's temporaries: the store lock held by the caller proves
    /// no publication of this scope is in flight, and other scopes' are theirs.
    /// A seal killed before its segment flush leaves every segment it wrote
    /// under a temporary name.
    fn remove_abandoned_publication_temporaries(
        segments_root: &Path,
        store_root: &Path,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let store_name = store_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Self::unavailable("code-generation store root has no UTF-8 name"))?;
        let prefixes = [
            format!(".evidence-pack-publication.{store_name}."),
            format!(".segment-publication.{store_name}."),
        ];
        let mut removed = false;
        for entry in std::fs::read_dir(segments_root).map_err(Self::unavailable)? {
            let entry = entry.map_err(Self::unavailable)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str()))
                || !name.ends_with(".tmp")
            {
                continue;
            }
            let metadata = entry.path().symlink_metadata().map_err(Self::unavailable)?;
            if !metadata.file_type().is_file() {
                return Err(Self::unavailable(
                    "sealed publication temporary path is not a regular file",
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

    /// Writes one new sealed segment under a temporary name in `staged`, or
    /// verifies the already-named segment with its content address. A new
    /// segment takes its name only when [`StagedGenerationSegmentsV1::publish`]
    /// has made the whole seal's segments durable.
    #[cfg(unix)]
    fn state_digest_existing_segment(
        path: &Path,
        original: &std::fs::Metadata,
    ) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        #[cfg(test)]
        PREPARED_SEGMENT_DIGEST_OPENS.with(|count| count.set(count.get().saturating_add(1)));
        let mut file = File::open(path).map_err(Self::unavailable)?;
        let opened = file.metadata().map_err(Self::unavailable)?;
        if !Self::same_unix_segment_identity(original, &opened) {
            return Err(Self::corruption(
                "existing sealed segment changed before authentication",
            ));
        }
        let digest = Self::state_digest_reader(&mut file)?;
        let after = file.metadata().map_err(Self::unavailable)?;
        let named = path.symlink_metadata().map_err(Self::unavailable)?;
        if !Self::same_unix_segment_identity(original, &after)
            || !Self::same_unix_segment_identity(original, &named)
        {
            return Err(Self::corruption(
                "existing sealed segment changed during authentication",
            ));
        }
        Ok(digest)
    }

    fn state_digest_file(path: &Path) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        #[cfg(all(test, unix))]
        PREPARED_SEGMENT_DIGEST_OPENS.with(|count| count.set(count.get().saturating_add(1)));
        let mut file = File::open(path).map_err(Self::unavailable)?;
        Self::state_digest_reader(&mut file)
    }

    fn state_digest_reader(
        reader: &mut impl Read,
    ) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        loop {
            let read = reader.read(&mut buffer).map_err(Self::unavailable)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(encode_tagged_lowercase_hex("sha256:", &hasher.finalize()))
    }

    // A canonical generation is immutable. Close the staging descriptor and
    // use the same atomic no-replace authority as sealed segments: tempfile's
    // no-clobber persist may leave two hard links after an interrupted publish.
    fn publish_missing_generation_manifest(
        temporary: tempfile::NamedTempFile,
        final_path: &Path,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let temporary = temporary.into_temp_path();
        StagedGenerationSegmentsV1::publish_missing_immutable_file(
            temporary.as_ref(),
            final_path,
            None,
        )
    }

    fn files_equal(left: &Path, right: &Path) -> Result<bool, CodeIndexPublicationStoreErrorV1> {
        // Measure the endpoint-authorized pathname opens as well as the byte
        // loop, which has its own narrower span below.
        let _pathname_span =
            tracing::trace_span!("code_index.generation.compare_immutable_pathnames").entered();
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
        let mut left_file = File::open(left).map_err(Self::unavailable)?;
        let mut right_file = File::open(right).map_err(Self::unavailable)?;
        #[cfg(unix)]
        {
            let left_opened = left_file.metadata().map_err(Self::unavailable)?;
            let right_opened = right_file.metadata().map_err(Self::unavailable)?;
            if !Self::same_unix_segment_identity(&left_metadata, &left_opened)
                || !Self::same_unix_segment_identity(&right_metadata, &right_opened)
            {
                return Err(Self::corruption(
                    "immutable code-index file changed while opening for comparison",
                ));
            }
        }
        let mut left_buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        let mut right_buffer = vec![0_u8; DURABLE_GENERATION_IO_CHUNK_BYTES_V1];
        // Content equality authorizes discarding the pending canonical copy.
        // A slow endpoint-scanned read must not authorize a swapped pathname.
        let equal = {
            let _span =
                tracing::trace_span!("code_index.generation.compare_immutable_bytes").entered();
            loop {
                let left_read = left_file
                    .read(&mut left_buffer)
                    .map_err(Self::unavailable)?;
                if left_read == 0 {
                    break right_file
                        .read(&mut right_buffer[..1])
                        .map_err(Self::unavailable)?
                        == 0;
                }
                right_file
                    .read_exact(&mut right_buffer[..left_read])
                    .map_err(Self::unavailable)?;
                if left_buffer[..left_read] != right_buffer[..left_read] {
                    break false;
                }
            }
        };
        #[cfg(unix)]
        {
            let left_named = left.symlink_metadata().map_err(Self::unavailable)?;
            let right_named = right.symlink_metadata().map_err(Self::unavailable)?;
            let left_opened = left_file.metadata().map_err(Self::unavailable)?;
            let right_opened = right_file.metadata().map_err(Self::unavailable)?;
            if !Self::same_unix_segment_identity(&left_metadata, &left_named)
                || !Self::same_unix_segment_identity(&left_metadata, &left_opened)
                || !Self::same_unix_segment_identity(&right_metadata, &right_named)
                || !Self::same_unix_segment_identity(&right_metadata, &right_opened)
            {
                return Err(Self::corruption(
                    "immutable code-index file changed during comparison",
                ));
            }
        }
        Ok(equal)
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

    fn publication_cardinality(
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<DurableGenerationCardinalityV1, CodeIndexPublicationStoreErrorV1> {
        Ok(DurableGenerationCardinalityV1 {
            file_count: u64::try_from(generation.snapshot().files.len())
                .map_err(Self::unavailable)?,
            chunk_count: generation.chunk_count(),
            symbol_count: generation.statistics().symbol_count,
        })
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
        self.read_publication_pointer_with_bytes()
            .map(|result| result.map(|(pointer, _)| pointer))
    }

    // The CAS callers need the *exact serialized bytes* of the pointer they
    // just authenticated. Return those bytes from the original file open
    // rather than making an independent endpoint-scanned pathname open.
    fn read_publication_pointer_with_bytes(
        &self,
    ) -> Result<Option<(DurablePublicationPointerV1, Vec<u8>)>, CodeIndexPublicationStoreErrorV1>
    {
        Self::read_publication_pointer_at(&self.active_path, &self.pointer_memo)
    }

    fn read_publication_pointer_at(
        active_path: &Path,
        pointer_memo: &Mutex<Option<PublicationPointerMemoV1>>,
    ) -> Result<Option<(DurablePublicationPointerV1, Vec<u8>)>, CodeIndexPublicationStoreErrorV1>
    {
        // The named pointer must itself be a regular file. Following a
        // symlink would let an equal-byte replacement inherit the memo.
        // Never follow a pointer alias when deciding whether its slot is valid.
        let named = std::fs::symlink_metadata(active_path);
        let metadata = match named {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                *pointer_memo.lock().unwrap_or_else(PoisonError::into_inner) = None;
                return Ok(None);
            }
            Err(error) => return Err(Self::map_pointer_io(error)),
        };
        if !metadata.file_type().is_file() {
            return Err(Self::corrupt_non_file_pointer());
        }
        if metadata.len() > MAX_DURABLE_PUBLICATION_POINTER_BYTES {
            return Err(Self::corruption(
                "durable code-generation index exceeds its byte bound",
            ));
        }
        let mtime = metadata.modified().ok();
        let size = metadata.len();
        // Verify a cached pointer through its existing descriptor. Metadata
        // witnesses both sides of the read, including same-byte inode swaps,
        // while hashing the current bytes detects in-place changes even on
        // filesystems with coarse timestamps. An atomic replacement misses
        // this inode and falls through to the ordinary cold validation.
        #[cfg(unix)]
        {
            let mut cached = pointer_memo.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = cached.as_mut()
                && let Some(file) = entry.file.as_mut()
            {
                let before = file.metadata().map_err(Self::map_pointer_io)?;
                if Self::same_unix_segment_identity(&metadata, &before) {
                    file.seek(SeekFrom::Start(0))
                        .map_err(Self::map_pointer_io)?;
                    let mut current = Vec::new();
                    (&mut *file)
                        .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
                        .read_to_end(&mut current)
                        .map_err(Self::map_pointer_io)?;
                    let after = file.metadata().map_err(Self::map_pointer_io)?;
                    Self::require_pointer_identity_witness(
                        active_path,
                        &metadata,
                        &before,
                        &after,
                    )?;
                    if Self::state_digest(&current) != entry.digest {
                        return Err(Self::corruption(
                            "active code-generation pointer bytes changed while pinned",
                        ));
                    }
                    entry.mtime = mtime;
                    entry.size = size;
                    return Ok(Some((entry.pointer.clone(), current)));
                }
            }
        }
        // Windows cannot inspect the pathname's stable file ID through std
        // metadata. Reuse the memo's authenticated descriptor for byte reads,
        // then use one fresh same-file pathname witness after the read. A
        // completed inode rotation invalidates the memo and is reloaded cold
        // in this call; races during cold validation still refuse.
        #[cfg(not(unix))]
        {
            let mut cached = pointer_memo.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = cached.as_mut()
                && let Some(file) = entry.file.as_mut()
            {
                match Self::read_cached_non_unix_pointer(active_path, file, &entry.digest) {
                    Ok(current) => {
                        entry.mtime = mtime;
                        entry.size = size;
                        return Ok(Some((entry.pointer.clone(), current)));
                    }
                    Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap) => {
                        // The pathname may now name a fully published successor.
                        // Drop the retired descriptor before authenticating it
                        // through the same cold path used for the first read.
                        *cached = None;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        // Size and mtime are hints, not identity. Text-artifact attach writes
        // a fixed-width pointer through another path, and a 1-second mtime
        // filesystem can leave both unchanged while the bytes move. The memo
        // is reused only when the file digest matches.
        // A cold pointer validation must not authorize a second pathname
        // open merely to populate the memo. Keep the original descriptor
        // through parsing and authenticate its bytes again before memoizing.
        let (bytes, mut opened_pointer) = {
            let mut file = File::open(active_path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    // The pointer existed at this call's first metadata probe.
                    // A concurrent atomic publisher can withdraw it before
                    // the cold open; retry without treating it as corruption.
                    CodeIndexPublicationStoreErrorV1::CompareAndSwap
                } else {
                    Self::map_pointer_io(error)
                }
            })?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(Self::map_pointer_io)?;
            if u64::try_from(bytes.len()).map_err(Self::unavailable)?
                > MAX_DURABLE_PUBLICATION_POINTER_BYTES
            {
                return Err(Self::corruption(
                    "durable code-generation index exceeds its byte bound",
                ));
            }
            (bytes, file)
        };
        let digest = Self::state_digest(&bytes);
        {
            let mut memo = pointer_memo.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(memo) = memo.as_mut()
                && memo.digest == digest
            {
                // A matching digest does not authenticate a substituted
                // pathname. Preserve the cold path's inode checks without
                // another endpoint-scanned open.
                #[cfg(unix)]
                {
                    let opened = opened_pointer.metadata().map_err(Self::map_pointer_io)?;
                    Self::require_pointer_identity_witness(
                        active_path,
                        &metadata,
                        &opened,
                        &opened,
                    )?;
                }
                #[cfg(not(unix))]
                Self::require_non_unix_pointer_witness(active_path, &mut opened_pointer, &bytes)?;
                memo.mtime = mtime;
                memo.size = size;
                memo.file = Some(opened_pointer);
                return Ok(Some((memo.pointer.clone(), bytes)));
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
        if pointer.generation_index_digest
            != Self::generation_index_digest(
                &pointer.generation_index,
                pointer.generation_index_truncated,
            )?
        {
            return Err(Self::corruption(
                "durable code-generation index digest does not match its entries",
            ));
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
        let mut memo = pointer_memo.lock().unwrap_or_else(PoisonError::into_inner);
        // Install only when the file is still the bytes just parsed. A rename
        // that landed during validation owns the memo.
        // On Unix, re-read through the authenticated descriptor rather than
        // opening the same endpoint-scanned pathname again. The two inode
        // witnesses reject a concurrent rename; the second byte comparison
        // rejects an in-place change during parsing, even on coarse clocks.
        #[cfg(unix)]
        Self::require_cold_pointer_witness(active_path, &mut opened_pointer, &metadata, &bytes)?;
        #[cfg(not(unix))]
        Self::require_non_unix_pointer_witness(active_path, &mut opened_pointer, &bytes)?;
        *memo = Some(PublicationPointerMemoV1 {
            mtime,
            size,
            digest,
            pointer: pointer.clone(),
            file: Some(opened_pointer),
        });
        Ok(Some((pointer, bytes)))
    }

    /// Fail closed if a cold pointer's pathname or bytes changed while its
    /// original descriptor was parsed. A failed witness is not a valid
    /// publication snapshot and must never be returned to a caller.
    #[cfg(unix)]
    fn require_cold_pointer_witness(
        path: &Path,
        file: &mut File,
        named_before: &std::fs::Metadata,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let opened_before = file.metadata().map_err(Self::map_pointer_io)?;
        file.seek(SeekFrom::Start(0))
            .map_err(Self::map_pointer_io)?;
        let mut current = Vec::new();
        (&mut *file)
            .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
            .read_to_end(&mut current)
            .map_err(Self::map_pointer_io)?;
        let opened_after = file.metadata().map_err(Self::map_pointer_io)?;
        Self::require_pointer_identity_witness(path, named_before, &opened_before, &opened_after)?;
        if current != bytes {
            return Err(Self::corruption(
                "active code-generation pointer bytes changed during cold validation",
            ));
        }
        Ok(())
    }

    /// Reuse a Windows pointer descriptor while preserving one post-read
    /// pathname file-ID check. Only the same-file witness opens the path.
    #[cfg(not(unix))]
    fn read_cached_non_unix_pointer(
        path: &Path,
        file: &mut File,
        expected_digest: &str,
    ) -> Result<Vec<u8>, CodeIndexPublicationStoreErrorV1> {
        file.seek(SeekFrom::Start(0))
            .map_err(Self::map_pointer_io)?;
        let mut current = Vec::new();
        (&mut *file)
            .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
            .read_to_end(&mut current)
            .map_err(Self::map_pointer_io)?;
        if current.len() as u64 > MAX_DURABLE_PUBLICATION_POINTER_BYTES {
            return Err(Self::corruption(
                "durable code-generation index exceeds its byte bound",
            ));
        }
        Self::require_non_unix_pointer_witness(path, file, &current)?;
        if Self::state_digest(&current) != expected_digest {
            return Err(Self::corruption(
                "active code-generation pointer bytes changed while pinned",
            ));
        }
        Ok(current)
    }

    /// Windows must still name the exact opened inode after a cold parse.
    /// Reuse the pathname open previously spent on byte readback for a real
    /// same-file check instead. A rotated inode is a retryable CAS; mutations
    /// to the original opened bytes are reset-required corruption.
    #[cfg(not(unix))]
    fn require_non_unix_pointer_witness(
        path: &Path,
        file: &mut File,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let before = file.metadata().map_err(Self::map_pointer_io)?;
        file.seek(SeekFrom::Start(0))
            .map_err(Self::map_pointer_io)?;
        let mut current = Vec::new();
        (&mut *file)
            .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
            .read_to_end(&mut current)
            .map_err(Self::map_pointer_io)?;
        let after = file.metadata().map_err(Self::map_pointer_io)?;
        if current != bytes
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(Self::corruption(
                "active code-generation pointer changed while validating its opened inode",
            ));
        }
        let opened = Handle::from_file(file.try_clone().map_err(Self::map_pointer_io)?)
            .map_err(Self::map_pointer_io)?;
        // Open the final name without following a reparse point and retain that
        // exact descriptor as the last authority check. A trailing metadata-only
        // probe cannot distinguish another same-length inode installed after a
        // following pathname open.
        let named_file = match tracedecay_private_fs::framed_log::open_no_follow(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                return Err(Self::corrupt_non_file_pointer());
            }
            Err(error) => return Err(Self::map_pointer_io(error)),
        };
        let named_metadata = named_file.metadata().map_err(Self::map_pointer_io)?;
        if !named_metadata.file_type().is_file() {
            return Err(Self::corrupt_non_file_pointer());
        }
        let named_handle = Handle::from_file(named_file).map_err(Self::map_pointer_io)?;
        if opened != named_handle {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if named_metadata.len() != after.len()
            || named_metadata.modified().ok() != after.modified().ok()
        {
            return Err(Self::corruption(
                "active code-generation pointer changed in place during validation",
            ));
        }
        Ok(())
    }

    /// The same inode witness controls every Unix pointer read, including
    /// warm memo hits. Atomic publication rotates the pathname and is a
    /// retryable CAS; in-place writes and symlink aliases remain corruption.
    /// All checks use stat metadata, without opening another pathname.
    #[cfg(unix)]
    fn require_pointer_identity_witness(
        path: &Path,
        named_before: &std::fs::Metadata,
        opened_before: &std::fs::Metadata,
        opened_after: &std::fs::Metadata,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let named_after = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Err(error) => return Err(Self::map_pointer_io(error)),
        };
        if !named_after.file_type().is_file() {
            return Err(Self::corrupt_non_file_pointer());
        }
        if (named_before.dev(), named_before.ino()) != (opened_before.dev(), opened_before.ino())
            || (named_after.dev(), named_after.ino()) != (opened_before.dev(), opened_before.ino())
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if !Self::same_unix_segment_identity(named_before, opened_before)
            || !Self::same_unix_segment_identity(opened_before, opened_after)
            || !Self::same_unix_segment_identity(opened_before, &named_after)
        {
            return Err(Self::corruption(
                "active code-generation pointer changed in place during validation",
            ));
        }
        Ok(())
    }

    fn remember_publication_pointer(
        &self,
        pointer: &DurablePublicationPointerV1,
        bytes: &[u8],
        persisted: &mut File,
    ) {
        let mut memo = self
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // The tempfile descriptor survives its atomic rename. Reopen-free
        // readback authenticates that exact inode, while pathname metadata
        // prevents memoizing a pointer replaced by another writer.
        #[cfg(unix)]
        let metadata = (|| -> std::io::Result<Option<std::fs::Metadata>> {
            let opened = persisted.metadata()?;
            let named_before = self.active_path.symlink_metadata()?;
            if !Self::same_unix_segment_identity(&opened, &named_before) {
                return Ok(None);
            }
            persisted.seek(SeekFrom::Start(0))?;
            let mut current = Vec::new();
            (&mut *persisted)
                .take(MAX_DURABLE_PUBLICATION_POINTER_BYTES + 1)
                .read_to_end(&mut current)?;
            let after = persisted.metadata()?;
            let named_after = self.active_path.symlink_metadata()?;
            Ok((current == bytes
                && Self::same_unix_segment_identity(&opened, &after)
                && Self::same_unix_segment_identity(&opened, &named_after))
            .then_some(named_after))
        })()
        .ok()
        .flatten();
        #[cfg(not(unix))]
        let metadata = Self::require_non_unix_pointer_witness(&self.active_path, persisted, bytes)
            .ok()
            .and_then(|()| self.active_path.symlink_metadata().ok());
        if let Some(metadata) = metadata {
            *memo = Some(PublicationPointerMemoV1 {
                mtime: metadata.modified().ok(),
                size: metadata.len(),
                digest: Self::state_digest(bytes),
                pointer: pointer.clone(),
                file: persisted.try_clone().ok(),
            });
        } else {
            // Never preserve an older memo after an unverified replacement.
            *memo = None;
        }
    }

    /// Replace the active pointer only when it is still the exact bytes this
    /// publication observed under the store lock.
    ///
    /// `rename(2)` replaces whatever occupies the path, including a truncated
    /// or rewritten pointer. The observation is the compare-and-swap token:
    /// a mismatch is a refusal, not a rewrite. `lock` is the witness that
    /// this critical section is the exclusive owner of the store.
    pub(super) fn commit_observed_pointer(
        &self,
        _lock: &CodeGenerationStoreLockV1,
        observed: Option<&[u8]>,
        pointer: &DurablePublicationPointerV1,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        // Refuse a directory (or any non-file) before the read and the
        // `rename(2)`. Reading one returns EISDIR, which is not a
        // publication-family fault.
        self.require_regular_pointer_slot()?;
        let current = match std::fs::read(&self.active_path) {
            Ok(current) => Some(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Self::unavailable(error)),
        };
        if current.as_deref() != observed {
            return Err(match current {
                Some(current)
                    if serde_json::from_slice::<DurablePublicationPointerV1>(&current).is_err() =>
                {
                    Self::corruption("active code-generation pointer is corrupt")
                }
                _ => CodeIndexPublicationStoreErrorV1::CompareAndSwap,
            });
        }
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active pointer has no parent directory"))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".active-code-generation-")
            .tempfile_in(store_root)
            .map_err(Self::unavailable)?;
        temporary.write_all(bytes).map_err(Self::unavailable)?;
        temporary.as_file().sync_all().map_err(Self::unavailable)?;
        #[cfg(windows)]
        let mut persisted = {
            // Normalize FILE_ATTRIBUTE_TEMPORARY before publication while
            // retaining cleanup ownership if normalization or rename fails.
            let mut temporary_path =
                tempfile::TempPath::try_from_path(temporary.path().to_path_buf())
                    .map_err(Self::unavailable)?;
            let (persisted, _) = temporary
                .keep()
                .map_err(|error| Self::map_pointer_io(error.error))?;
            // The memo can still hold the prior pointer open. Use the
            // durable Windows rename, including its POSIX replacement path.
            tracedecay_private_fs::framed_log::replace_via_rename(
                &temporary_path,
                &self.active_path,
            )
            .map_err(Self::map_pointer_io)?;
            temporary_path.disable_cleanup(true);
            persisted
        };
        #[cfg(not(windows))]
        let mut persisted = temporary
            .persist(&self.active_path)
            .map_err(|error| Self::map_pointer_io(error.error))?;
        Self::sync_directory(
            self.active_path
                .parent()
                .ok_or_else(|| Self::unavailable("active pointer has no parent directory"))?,
        )?;
        self.remember_publication_pointer(pointer, bytes, &mut persisted);
        Ok(())
    }

    fn read_authority(&self) -> GenerationReadAuthorityV1 {
        GenerationReadAuthorityV1 {
            active_path: self.active_path.clone(),
            segments_root: self.segments_root.clone(),
            pointer_memo: Arc::clone(&self.pointer_memo),
        }
    }

    fn require_read_control(
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if control.is_cancelled() {
            return Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::Cancelled,
            ));
        }
        if control.is_deadline_exceeded() {
            return Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::DeadlineExceeded,
            ));
        }
        Ok(())
    }

    pub(super) fn retained_pack_read_factory(
        &self,
        identity: DurableSealedCodeGenerationIdentityV1,
    ) -> Result<
        impl Fn(
            &dyn CodeIndexExecutionControlV1,
            &SealedGenerationPackLayoutV1,
        ) -> Result<Box<SealedGenerationPackReadPassV1>, CodeIndexProductionErrorV1>
        + Send
        + Sync
        + 'static,
        CodeIndexPublicationStoreErrorV1,
    > {
        let resident_memory = self.resident_memory_authority()?;
        let mut memory = PreparedPackMemoryV1::new(&resident_memory.begin_operation())?;
        memory.admit(
            size_of::<GenerationReadAuthorityV1>()
                .saturating_add(size_of::<DurableSealedCodeGenerationIdentityV1>())
                .saturating_add(size_of::<PreparedPackMemoryV1>())
                .saturating_add(self.active_path.as_os_str().len())
                .saturating_add(self.segments_root.as_os_str().len())
                .saturating_add(identity.locator.len())
                .saturating_add(identity.digest.as_str().len()),
        )?;
        let authority = self.read_authority();
        Ok(
            move |control: &dyn CodeIndexExecutionControlV1,
                  layout: &SealedGenerationPackLayoutV1| {
                let lease = loop {
                    Self::require_read_control(control)?;
                    if let Some(lease) = authority.try_lease()? {
                        break lease;
                    }
                    std::thread::park_timeout(Duration::from_millis(1));
                };
                let opened =
                    authority.open_under_lease(&identity, layout, memory.operation(), lease);
                Self::require_read_control(control)?;
                let mut pass = opened?;
                Ok(Box::new(
                    move |request: SealedGenerationSegmentReadV1<'_>,
                          buffer: &mut Vec<u8>,
                          control: &dyn CodeIndexExecutionControlV1| {
                        buffer.clear();
                        Self::require_read_control(control)?;
                        let result = pass.read(request, buffer);
                        if let Err(error) = Self::require_read_control(control) {
                            buffer.clear();
                            return Err(error);
                        }
                        result
                    },
                ) as Box<SealedGenerationPackReadPassV1>)
            },
        )
    }

    fn active_pack_read_factory(
        &self,
        active: &ActiveSealedManifestV1,
    ) -> Result<Arc<SealedGenerationPackReadFactoryV1>, CodeIndexPublicationStoreErrorV1> {
        let digest = ManifestDigest::new(active.state_digest.clone()).map_err(Self::corruption)?;
        let suffix = sha256_hex_suffix(digest.as_str())
            .ok_or_else(|| Self::corruption("sealed generation digest is not sha256"))?;
        let mut memory = PreparedPackMemoryV1::new(&active.metadata._memory.operation)?;
        memory.admit(
            size_of::<GenerationReadAuthorityV1>()
                .saturating_add(size_of::<DurableSealedCodeGenerationIdentityV1>())
                .saturating_add(size_of::<PreparedPackMemoryV1>())
                .saturating_add(size_of::<usize>().saturating_mul(6))
                .saturating_add(self.active_path.as_os_str().len())
                .saturating_add(self.segments_root.as_os_str().len())
                .saturating_add(digest.as_str().len().saturating_mul(2)),
        )?;
        let identity = DurableSealedCodeGenerationIdentityV1 {
            locator: format!("generation-{suffix}.json"),
            digest,
            size_bytes: u64::try_from(active.manifest_bytes.len()).map_err(Self::unavailable)?,
        };
        let authority = self.read_authority();
        let metadata = Arc::clone(&active.metadata);
        Ok(Arc::new(move || {
            let lease = authority
                .try_lease()?
                .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
            let pass = Mutex::new(authority.open_under_lease(
                &identity,
                &metadata.layout,
                memory.operation(),
                lease,
            )?);
            Ok(Arc::new(
                move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                    buffer.clear();
                    pass.lock()
                        .map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed pack read pass was poisoned".to_owned(),
                            )
                        })?
                        .read(request, buffer)
                },
            ) as Arc<SealedSegmentReaderV1>)
        }))
    }

    fn open_pinned_partitioned_segment_at(
        path: &Path,
        request: SealedGenerationSegmentReadV1<'_>,
        open_attempts: &mut u64,
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
        #[cfg(windows)]
        {
            // Windows pathname metadata performs a native open even when the
            // lookup fails; Unix metadata does not open a payload handle.
            *open_attempts = open_attempts.saturating_add(1);
        }
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
        *open_attempts = open_attempts.saturating_add(1);
        let file = File::open(path).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment cannot be opened: {error}"
            ))
        })?;
        let file_metadata = file.metadata().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment metadata cannot be read: {error}"
            ))
        })?;
        // On Unix the pathname/opened inode and change-time identity can be
        // compared without Handle::from_path (another endpoint-scanned open).
        #[cfg(unix)]
        let same_identity = {
            // Also re-stat the pathname after opening. Comparing just the
            // pre-open path and fd would miss a replacement after open, while
            // Handle::from_path would require another endpoint-scanned open.
            let path_after_open = path.symlink_metadata().map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment pathname cannot be rechecked: {error}"
                ))
            })?;
            Self::same_unix_segment_identity(&path_metadata, &file_metadata)
                && Self::same_unix_segment_identity(&path_after_open, &file_metadata)
        };
        #[cfg(windows)]
        let same_identity =
            Self::same_windows_segment_identity(path, &path_metadata, &file, open_attempts)?;
        #[cfg(not(any(unix, windows)))]
        let same_identity = {
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
            *open_attempts = open_attempts.saturating_add(1);
            let path_identity = Handle::from_path(path).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment path identity cannot be read: {error}"
                ))
            })?;
            file_identity == path_identity
        };
        if !file_metadata.is_file() || file_metadata.len() != expected_size || !same_identity {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment identity changed while it was opened".to_owned(),
            ));
        }
        Ok(PinnedGenerationSegmentV1 {
            digest: digest.to_owned(),
            size_bytes: expected_size,
            #[cfg(not(windows))]
            file,
            #[cfg(windows)]
            file: PinnedGenerationFileV1::Shared(file),
            admitted_metadata: path_metadata,
        })
    }

    #[cfg(windows)]
    fn same_windows_segment_metadata(
        admitted: &std::fs::Metadata,
        current: &std::fs::Metadata,
    ) -> bool {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        admitted.file_type().is_file()
            && current.file_type().is_file()
            && admitted.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && current.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && admitted.file_size() == current.file_size()
            && admitted.creation_time() == current.creation_time()
            && admitted.last_write_time() == current.last_write_time()
    }

    #[cfg(windows)]
    fn same_windows_segment_identity(
        path: &Path,
        admitted: &std::fs::Metadata,
        file: &File,
        open_attempts: &mut u64,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        let unavailable = |error: std::io::Error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation pack identity cannot be rechecked: {error}"
            ))
        };
        let same_metadata =
            |current: &std::fs::Metadata| Self::same_windows_segment_metadata(admitted, current);
        if !same_metadata(&file.metadata().map_err(unavailable)?) {
            return Ok(false);
        }
        let opened_identity =
            tracedecay_private_fs::windows_file::information(file).map_err(unavailable)?;
        // This is a real pathname open, even though it is used only to prove
        // volume/file-index equality with the retained payload handle. The
        // canonical no-follow opener lets the metadata check refuse reparse
        // aliases without relying on a preliminary pathname check.
        *open_attempts = open_attempts.saturating_add(1);
        let named = tracedecay_private_fs::framed_log::open_no_follow(path).map_err(unavailable)?;
        if !same_metadata(&named.metadata().map_err(unavailable)?) {
            return Ok(false);
        }
        let named_identity =
            tracedecay_private_fs::windows_file::information(&named).map_err(unavailable)?;
        if opened_identity.volume_serial_number != named_identity.volume_serial_number
            || opened_identity.file_index != named_identity.file_index
        {
            return Ok(false);
        }
        // std's Windows pathname metadata query opens the name again.
        *open_attempts = open_attempts.saturating_add(1);
        Ok(
            same_metadata(&path.symlink_metadata().map_err(unavailable)?)
                && same_metadata(&file.metadata().map_err(unavailable)?),
        )
    }

    #[cfg(unix)]
    fn same_unix_segment_identity(named: &std::fs::Metadata, pinned: &std::fs::Metadata) -> bool {
        named.file_type().is_file()
            && pinned.is_file()
            && named.dev() == pinned.dev()
            && named.ino() == pinned.ino()
            && named.len() == pinned.len()
            && named.mtime() == pinned.mtime()
            && named.mtime_nsec() == pinned.mtime_nsec()
            && named.ctime() == pinned.ctime()
            && named.ctime_nsec() == pinned.ctime_nsec()
    }

    fn read_pinned_partitioned_segment(
        pinned: &mut PinnedGenerationSegmentV1,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        // Errors must not leave bytes from an earlier successful range visible.
        buffer.clear();
        let (digest, expected_size, offset, length, whole) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest.as_str(), size_bytes, 0, size_bytes, true)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest.as_str(), size_bytes, offset, length, false),
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
        buffer.resize(length, 0);
        let mut file: &File = &pinned.file;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(buffer))
            .map_err(|error| {
                buffer.clear();
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment read failed: {error}"
                ))
            })?;
        // A cold pinned read must enforce the same whole-segment digest
        // contract as the uncached reader, not just its pathname and extent.
        if whole && Self::state_digest(buffer) != digest {
            buffer.clear();
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment does not match its content address".to_owned(),
            ));
        }
        Ok(())
    }

    #[tracing::instrument(
        name = "code_index.generation.validate_partitioned_manifest",
        level = "trace",
        skip_all
    )]
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
        let operation = self.resident_memory_authority()?.begin_operation();
        let mut memory = PreparedPackMemoryV1::new(&operation)?;
        let byte_len = usize::try_from(identity.size_bytes).map_err(Self::unavailable)?;
        // The raw manifest and its temporary tagged SHA-256 string coexist.
        memory.admit(byte_len.saturating_add("sha256:".len() + 64))?;
        let mut file = File::open(path).map_err(Self::unavailable)?;
        let mut bytes = vec![0_u8; byte_len];
        file.read_exact(&mut bytes).map_err(Self::unavailable)?;
        if file.metadata().map_err(Self::unavailable)?.len() != identity.size_bytes {
            return Err(Self::corruption(
                "partitioned generation manifest changed extent while reading",
            ));
        }
        if Self::state_digest(&bytes) != identity.digest.as_str() {
            return Err(Self::corruption(
                "partitioned generation manifest digest does not verify",
            ));
        }
        match CodeIndexPublishedGenerationV1::partitioned_metadata_and_layout_admitted(
            &bytes,
            &mut |needed| {
                memory
                    .admit(byte_len.saturating_add(needed))
                    .map_err(Into::into)
            },
        ) {
            Ok((metadata, layout)) => {
                // This route returns only the existing compact metadata. The new
                // packed map dies while its parse reservation is still retained.
                drop(layout);
                Ok(Some(metadata))
            }
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_)) => Ok(None),
            Err(CodeIndexProductionErrorV1::Publication(error)) => Err(error),
            Err(error) => Err(Self::corruption(error.to_string())),
        }
    }

    #[tracing::instrument(
        name = "code_index.generation.decode.bundle",
        level = "trace",
        skip_all
    )]
    fn decode_generation_file(
        &self,
        file: &mut File,
        admitted_len: u64,
        expected_file_digest: &ManifestDigest,
        lifetime_lock: CodeGenerationStoreLockV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let operation = self.resident_memory_authority()?.begin_operation();
        let byte_len = usize::try_from(admitted_len).map_err(Self::unavailable)?;
        let mut manifest_memory = PreparedPackMemoryV1::new(&operation)?;
        manifest_memory.admit(byte_len.saturating_add("sha256:".len() + 64))?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest seek failed: {error}"
            ))
        })?;
        let mut manifest = vec![0_u8; byte_len];
        file.read_exact(&mut manifest).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest read failed: {error}"
            ))
        })?;
        if Self::state_digest(&manifest) != expected_file_digest.as_str() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation manifest filename digest does not match its bytes".to_owned(),
            ));
        }
        // The canonical decoder validates the full map before issuing raw
        // requests. Lazily pin each of its at most two physical identities,
        // without a second manifest parse or a per-logical-file open.
        let _lifetime_lock = lifetime_lock;
        let mut memory = PreparedPackMemoryV1::new(&operation)?;
        let mut pinned: [Option<(ManifestDigest, GenerationPackReadPassV1)>; 2] = [None, None];
        let mut open_attempts = PackReadOpenAttemptsV1::default();
        match CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            &manifest,
            &self.decoded_content,
            |request, buffer| {
                buffer.clear();
                let (digest, size_bytes) = match request {
                    SealedGenerationSegmentReadV1::Whole { digest, size_bytes }
                    | SealedGenerationSegmentReadV1::Range {
                        digest, size_bytes, ..
                    } => (digest, size_bytes),
                };
                let index = match pinned
                    .iter()
                    .position(|slot| slot.as_ref().is_some_and(|(known, _)| known == digest))
                {
                    Some(index) => index,
                    None => {
                        let index = pinned.iter().position(Option::is_none).ok_or_else(|| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed decode requested more than two physical packs".to_owned(),
                            )
                        })?;
                        let workspace = pack_read_workspace_bytes(
                            &self.segments_root,
                            digest.as_str().len().saturating_mul(2),
                        );
                        memory.admit(workspace.saturating_mul(2))?;
                        let identity = SealedGenerationSegmentIdentityV1 {
                            digest: digest.clone(),
                            size_bytes,
                        };
                        memory.admit(
                            workspace
                                .saturating_add(
                                    GenerationPackReadPassV1::immutable_workspace_bytes(
                                        &self.segments_root,
                                        std::slice::from_ref(&identity),
                                        None,
                                    )?,
                                )
                                .saturating_mul(2),
                        )?;
                        let pass = GenerationPackReadPassV1::open(
                            &self.segments_root,
                            std::slice::from_ref(&identity),
                            None,
                            &mut open_attempts,
                        )?;
                        pinned[index] = Some((identity.digest, pass));
                        index
                    }
                };
                let (_, pass) = pinned[index].as_mut().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract("sealed decode pack is absent".to_owned())
                })?;
                pass.read(request, buffer)
            },
            &mut |needed| {
                manifest_memory
                    .admit(byte_len.saturating_add(needed))
                    .map_err(Into::into)
            },
        ) {
            // A generation is a pure function of its source tree, so a
            // manifest revision this build no longer reads is refused rather
            // than repaired: abstain and let the scheduler rebuild it instead
            // of treating a shape this build no longer writes as corruption.
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)) => {
                tracing::warn!(
                    target: "tracedecay::code_index",
                    sealed_format_revision = revision,
                    "{}",
                    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                );
                Ok(None)
            }
            result => result.map(Some),
        }
    }

    /// Serve one sealed generation by identity, decoding it at most once.
    ///
    /// The active generation answers from its pinned slot. Any other generation
    /// is served from the decoded LRU, or decoded exactly once under a lease,
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

                let _parked = {
                    let _span =
                        tracing::trace_span!("code_index.generation.decode.singleflight_wait")
                            .entered();
                    {
                        self.cache
                            .ready
                            .wait(state)
                            .map_err(|_| DecodedGenerationCacheV1::poisoned())
                    }
                }?;
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

        let decoded = {
            let _span = tracing::trace_span!("code_index.generation.decode.file_read").entered();
            self.decode_generation_file(
                &mut file,
                entry.size_bytes,
                &expected_digest,
                lifetime_lock,
            )
        };
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
        if Self::generation_cardinality(&generation)? != entry.cardinality {
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
    /// decode. `None` means "not decoded here, yet", it is an abstention, not
    /// evidence that no generation exists, and callers must never turn it into a
    /// fail-closed verdict on its own.
    pub(super) fn active_already_decoded(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self.cache.lock_state()?.active.as_ref().map(Arc::clone))
    }

    /// Drop the build-phase decoded active generation after the seal is durable.
    ///
    /// Text projection reads authenticated sealed pages. Keeping the in-memory
    /// generation pinned here is the overlapping whole-generation owner that
    /// sits beside the artifact builder for the entire pre-seat text pass.
    /// Incremental rebuild and graph seat decode on demand.
    pub(super) fn release_decoded_active_after_seal(
        &self,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let released = {
            let mut state = self.cache.lock_state()?;
            let released = state.active.take();
            if released.is_some() {
                state.active_epoch = state.active_epoch.wrapping_add(1);
            }
            self.active_encoded_bytes.store(0, Ordering::Release);
            released
        };
        drop(released);
        Ok(())
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

    fn cache_state(&self) -> MutexGuard<'_, DecodedGenerationStateV1> {
        self.cache
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The active generation's decode, if one is held.
    pub(super) fn decoded_active(&self) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        self.cache_state().active.clone()
    }

    /// Drop the active decode. The durable pointer is untouched: the next
    /// reader or publication re-decodes it, exactly as after a restart.
    pub(super) fn release_decoded_active(&self) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        self.cache_state().active.take()
    }

    /// Decoded non-active generations held for pinned and branch reads.
    pub(super) fn superseded_decodes(&self) -> Vec<Arc<CodeIndexPublishedGenerationV1>> {
        self.cache_state().decoded.iter().cloned().collect()
    }

    pub(super) fn release_superseded_decodes(&self) -> Vec<Arc<CodeIndexPublishedGenerationV1>> {
        self.cache_state().decoded.drain(..).collect()
    }

    /// The durable active generation's sealed manifest and metadata, read and
    /// authenticated against its pointer without decoding a segment. `None`
    /// without an active pointer, or for a generation this build abstains
    /// from: a retired revision, one without source commitments, or one
    /// sealed under another sanitizer.
    #[tracing::instrument(
        name = "daemon.code_index.generation.load_active_manifest",
        level = "trace",
        skip_all
    )]
    pub(super) fn load_active_manifest(
        &self,
    ) -> Result<Option<ActiveSealedManifestV1>, CodeIndexPublicationStoreErrorV1> {
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        if let Some(memo) = self
            .active_manifest_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|memo| memo.state_digest == pointer.state_digest)
        {
            return Ok(Some(memo.clone()));
        }
        Self::validate_generation_file(&pointer.generation_file)?;
        let entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == pointer.generation_id)
            .ok_or_else(|| {
                Self::corruption(
                    "durable code-generation index does not contain its active generation",
                )
            })?;
        let _read_lock = self.acquire_generation_read_lock()?;
        let path = self.generations_root.join(&pointer.generation_file);
        let metadata = path.symlink_metadata().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("active code-generation target is missing")
            } else {
                Self::unavailable(error)
            }
        })?;
        if !metadata.file_type().is_file()
            || metadata.len() != entry.size_bytes
            || metadata.len() > MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        {
            return Err(Self::corruption(
                "active code-generation byte size does not match its durable entry",
            ));
        }
        let operation = self.resident_memory_authority()?.begin_operation();
        let mut memory = PreparedPackMemoryV1::new(&operation)?;
        let byte_len = usize::try_from(entry.size_bytes).map_err(Self::unavailable)?;
        let base = size_of::<ActiveSealedManifestV1>()
            .saturating_add(size_of::<AdmittedSealedMetadataV1>())
            .saturating_add(size_of::<usize>().saturating_mul(4))
            .saturating_add(byte_len.saturating_mul(2))
            .saturating_add(pointer.state_digest.len());
        memory.admit(base)?;
        let digest = ManifestDigest::new(pointer.state_digest.clone()).map_err(Self::corruption)?;
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: entry.size_bytes,
        };
        let mut opened = Self::open_pinned_partitioned_segment_at(&path, request, &mut 0)
            .map_err(Self::corruption)?;
        let mut bytes = Vec::with_capacity(byte_len);
        Self::read_pinned_partitioned_segment(&mut opened, request, &mut bytes)
            .map_err(Self::corruption)?;
        let (manifest, layout) =
            match CodeIndexPublishedGenerationV1::partitioned_metadata_and_layout_admitted(
                &bytes,
                &mut |needed| {
                    memory
                        .admit(base.saturating_add(needed))
                        .map_err(Into::into)
                },
            ) {
                Ok(manifest) => manifest,
                Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)) => {
                    tracing::warn!(
                        target: "tracedecay::code_index",
                        sealed_format_revision = revision,
                        "{}",
                        CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                    );
                    return Ok(None);
                }
                Err(CodeIndexProductionErrorV1::Publication(error)) => return Err(error),
                Err(error) => return Err(Self::corruption(error)),
            };
        if manifest.manifest().sanitizer_revision != self.expected_sanitizer_revision {
            return Ok(None);
        }
        if manifest.manifest().generation_id.as_str() != pointer.generation_id
            || manifest.snapshot().content_identity.as_str() != pointer.snapshot_content_identity
            || manifest.manifest().seal.sealed_at.0 != pointer.sealed_at_micros
        {
            return Err(Self::corruption(
                "active code-generation pointer does not match the sealed generation",
            ));
        }
        let retained = size_of::<ActiveSealedManifestV1>()
            .saturating_add(size_of::<AdmittedSealedMetadataV1>())
            .saturating_sub(size_of::<VerifiedSealedTextGenerationMetadataV1>())
            .saturating_sub(size_of::<SealedGenerationPackLayoutV1>())
            .saturating_add(size_of::<usize>().saturating_mul(4))
            .saturating_add(manifest.retained_bytes())
            .saturating_add(layout.retained_bytes())
            .saturating_add(bytes.len())
            .saturating_add(pointer.state_digest.len());
        let manifest_bytes: Arc<[u8]> = Arc::from(bytes);
        memory.resize(retained)?;
        let metadata = Arc::new(AdmittedSealedMetadataV1 {
            value: manifest,
            layout,
            manifest_bytes: Arc::clone(&manifest_bytes),
            _memory: memory,
        });
        let active = ActiveSealedManifestV1 {
            state_digest: pointer.state_digest,
            manifest_bytes,
            metadata,
        };
        *self
            .active_manifest_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(active.clone());
        Ok(Some(active))
    }

    /// Serve the active generation, decoding it at most once per publication.
    ///
    /// The first caller claims the decode lease and pays the O(store) sweep with
    /// no cache lock held; every caller that arrives while it runs parks on the
    /// condvar and is handed the same `Arc`. Nothing is memoized on failure, so
    /// a corrupt or unreadable store still errors every request.
    #[tracing::instrument(
        name = "daemon.code_index.generation.load_active",
        level = "trace",
        skip_all
    )]
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

                let parked = {
                    let _span =
                        tracing::trace_span!("code_index.generation.decode.singleflight_wait")
                            .entered();
                    self.cache
                        .ready
                        .wait(state)
                        .map_err(|_| DecodedGenerationCacheV1::poisoned())
                };
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
        let charge = self.admit_active_decode()?;
        let decoded = self.decode_active_generation();
        drop(charge);
        if let Ok(Some(generation)) = decoded.as_ref() {
            let charges = Self::active_generation_charges(generation);
            let mut state = self.cache.lock_state()?;
            if state.active_epoch == lease.epoch {
                state.forget(&generation.manifest().generation_id);
                self.record_active_generation_charges(charges);
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
    #[tracing::instrument(
        name = "daemon.code_index.generation.decode",
        level = "trace",
        skip_all
    )]
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

        let decoded = {
            let _span = tracing::trace_span!("code_index.generation.decode.file_read").entered();
            self.decode_generation_file(&mut file, metadata.len(), &expected_digest, lifetime_lock)
        };
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
        Ok(Some(Arc::new(generation)))
    }

    pub(super) fn bind_decode_admission(&self, admission: GenerationDecodeBudgetV1) {
        *self
            .decode_admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(admission);
    }

    /// Replay and parsing must use the scheduler's exact process authority.
    /// A store without that binding cannot invent a separate memory ledger.
    pub(super) fn resident_memory_authority(
        &self,
    ) -> Result<Arc<ProcessResidentMemoryV1>, CodeIndexPublicationStoreErrorV1> {
        self.decode_admission
            .lock()
            .map_err(|_| Self::unavailable("generation decode admission was poisoned"))?
            .as_ref()
            .map(|admission| Arc::clone(&admission.resident_memory))
            .ok_or_else(|| {
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(
                    "generation store has no process resident-memory authority".to_owned(),
                )
            })
    }

    /// Charge a whole-generation decode against the process authority before
    /// it runs. A generation this process held before is charged its measured
    /// resident bytes; when they do not fit below the admission watermark,
    /// retained state is shed and headroom re-measured once, and a decode
    /// that still does not fit is refused typed instead of grown into the
    /// kill line. The reservation spans the decode; afterwards the decoded
    /// generation is measured resident like every other owner.
    fn admit_active_decode(
        &self,
    ) -> Result<Option<ResidentMemoryReservationV1>, CodeIndexPublicationStoreErrorV1> {
        self.admit_active_generation_decode()
    }

    pub(super) fn sealed_graph_build_admission(&self) -> Arc<dyn CodeGraphBuildAdmissionV1> {
        Arc::new(SealedGraphBuildAdmissionV1(self.clone()))
    }

    pub(super) fn admit_sealed_graph_build_bound(
        &self,
        bound: tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<Option<ResidentMemoryReservationV1>, CodeIndexPublicationStoreErrorV1> {
        log_graph_build_bound(&bound);
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        let generation_id =
            CodeGenerationId::new(pointer.generation_id).map_err(Self::unavailable)?;
        self.admit_measured_generation_work(
            SEALED_GRAPH_BUILD_RESIDENT_COMPONENT_V1,
            "building the code graph of",
            generation_id,
            bound.peak_bytes(),
        )
    }

    fn admit_active_generation_decode(
        &self,
    ) -> Result<Option<ResidentMemoryReservationV1>, CodeIndexPublicationStoreErrorV1> {
        let (generation_id, bytes) = match self.active_generation_charge()? {
            ActiveGenerationDecodeChargeV1::Decoded => return Ok(None),
            ActiveGenerationDecodeChargeV1::Unmeasured => {
                tracing::debug!(
                    event = "code_index_generation_decode_unmeasured",
                    "this process never held the active generation; its first decode is not charged"
                );
                return Ok(None);
            }
            ActiveGenerationDecodeChargeV1::Measured {
                generation_id,
                bytes,
            } => (generation_id, bytes),
        };
        self.admit_measured_generation_work(
            GENERATION_DECODE_RESIDENT_COMPONENT_V1,
            "decoding",
            generation_id,
            bytes,
        )
    }

    fn admit_measured_generation_work(
        &self,
        component: &'static str,
        work_label: &str,
        generation_id: CodeGenerationId,
        bytes: u64,
    ) -> Result<Option<ResidentMemoryReservationV1>, CodeIndexPublicationStoreErrorV1> {
        let Some(requested) = NonZeroU64::new(bytes) else {
            return Ok(None);
        };
        let Some(admission) = self
            .decode_admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Ok(None);
        };
        let watermark = admission.resident_memory.admission_watermark_bytes();
        let admissible = || -> Result<(), String> {
            let available = admission
                .resident_memory
                .headroom_below(watermark)
                .available_bytes;
            if requested.get() <= available {
                Ok(())
            } else {
                Err(format!(
                    "{work_label} generation {generation_id} needs {} resident bytes; {available} are \
                     available below the {watermark}-byte admission watermark",
                    requested.get()
                ))
            }
        };
        if let Err(detail) = admissible() {
            let released = admission
                .resident_owners
                .shed(requested.get(), std::time::Instant::now());
            for release in &released {
                log_resident_owner_release_v1(release);
            }
            // Freed pages sit in the pool workers' allocator heaps whether or
            // not an owner was shed, so they are returned before the
            // re-measure that decides the refusal.
            let trim = release_process_allocator_memory_v1();
            tracing::info!(
                event = "code_index_generation_decode_shed_retained_state",
                released_owners = released.len(),
                trimmed_bytes = trim.released_bytes(),
                refused = %detail,
                "generation decode shed retained state before re-measuring headroom"
            );
            admissible().map_err(|detail| {
                admission
                    .resident_memory
                    .wait_for_headroom(requested.get(), watermark);
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(detail)
            })?;
        }
        let component = ResidentMemoryComponentIdV1::new(component).map_err(Self::unavailable)?;
        admission
            .resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: admission.project_id,
                    worktree_id: admission.worktree_id,
                    generation_id,
                    component,
                },
                requested,
            )
            .map(Some)
            .map_err(|error| {
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(error.to_string())
            })
    }

    /// A decoded copy charges its next decode what this decode measured at
    /// its peak, which covers the pass transients above what it retains; a
    /// generation built in memory has only its retained bytes to go on.
    /// Process-wide growth includes other owners and cannot be attributed to
    /// this generation.
    fn active_generation_charges(
        generation: &CodeIndexPublishedGenerationV1,
    ) -> ActiveGenerationChargesV1 {
        let retained = generation.retained_bytes();
        ActiveGenerationChargesV1 {
            generation_id: generation.manifest().generation_id.clone(),
            decode_bytes: generation
                .decode_peak_growth_bytes()
                .map_or(retained, |peak| peak.max(retained)),
        }
    }

    fn record_active_generation_charges(&self, charges: ActiveGenerationChargesV1) {
        *self
            .active_decode_charge
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(charges);
    }

    /// The next decode's resident cost. A cached generation avoids another
    /// decode.
    pub(super) fn active_generation_charge(
        &self,
    ) -> Result<ActiveGenerationDecodeChargeV1, CodeIndexPublicationStoreErrorV1> {
        if self.cache.lock_state()?.active.is_some() {
            return Ok(ActiveGenerationDecodeChargeV1::Decoded);
        }
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(ActiveGenerationDecodeChargeV1::Decoded);
        };
        let charges = self
            .active_decode_charge
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(charges) = charges
            .as_ref()
            .filter(|charges| charges.generation_id.as_str() == pointer.generation_id)
        else {
            return Ok(ActiveGenerationDecodeChargeV1::Unmeasured);
        };
        Ok(ActiveGenerationDecodeChargeV1::Measured {
            generation_id: charges.generation_id.clone(),
            bytes: charges.decode_bytes,
        })
    }

    pub(super) fn active_encoded_bytes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.active_encoded_bytes)
    }

    /// Sealed-bytes decodes this process has performed against this store.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn sealed_decode_count(&self) -> u64 {
        self.cache.decode_count()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn poison_decoded_cache_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self
                .cache
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            panic!("poison decoded-generation cache");
        }));
    }

    /// Delete this scope's derived publication and forget everything decoded
    /// from it, so the next reconcile seals a fresh generation from source.
    ///
    /// The whole scope store is derived data. A pointer the reader classifies
    /// as corrupt, whether damaged on disk or sealed by a release whose durable
    /// entry shape digests differently, is deleted outright: no copy is kept
    /// and nothing is migrated. A store another owner holds is a typed busy
    /// refusal the caller retries; every other failure is reported as is.
    pub(super) fn reset_corrupt_store(
        &self,
    ) -> Result<CodeIndexScopeStoreResetV1, CodeIndexPublicationStoreErrorV1> {
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        let store_lock = try_acquire_code_generation_store_lock(store_root)
            .map_err(Self::unavailable)?
            .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
        let receipt = {
            // Release the shared pointer descriptor before any unlink, even
            // when later cleanup or reconstruction fails. Keep the memo locked
            // through deletion so a reader cannot repopulate it during reset.
            let mut memo = self
                .pointer_memo
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *memo = None;
            reset_code_index_scope_store(&store_lock).map_err(Self::unavailable)?
        };
        std::fs::create_dir_all(&self.generations_root).map_err(Self::unavailable)?;
        tracedecay_code_index_retention::code_index_generations::record_scope_root(
            store_root,
            &self.project_root,
        )
        .map_err(Self::unavailable)?;
        {
            let mut state = self.cache.lock_state()?;
            state.active = None;
            state.decoded.clear();
            state.active_epoch = state.active_epoch.wrapping_add(1);
        }
        *self
            .active_decode_charge
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.cache.ready.notify_all();
        *self
            .active_manifest_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.active_encoded_bytes.store(0, Ordering::Release);
        Ok(receipt)
    }

    pub(super) fn take_unpublished(&self) -> Option<UnpublishedCodeIndexPublicationV1> {
        self.unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub(super) fn restore_unpublished(&self, generation: UnpublishedCodeIndexPublicationV1) {
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
    type Preparation = DaemonCodeIndexPreparationV1;

    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        if self.undecoded_active_expectation.is_some() {
            return Ok(None);
        }
        let Some(active) = self.load_active_manifest()? else {
            return Ok(None);
        };
        let factory = self.active_pack_read_factory(&active)?;
        Ok(Some(CodeIndexSealedGenerationV1::new(
            Arc::clone(&active.metadata.manifest_bytes),
            factory,
        )))
    }

    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1> {
        scope.validate().map_err(Self::unavailable)?;
        self.seal_checkpoint()?;
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        let retention = match acquire_generation_segments_publication_lock(store_root, &|| {
            self.seal_checkpoint().is_err()
        }) {
            Ok(retention) => retention,
            Err(CodeGenerationRetentionErrorV1::Cancelled) => {
                self.seal_checkpoint()?;
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Err(error) => return Err(Self::unavailable(error)),
        };
        self.seal_checkpoint()?;
        // Manifest abstention still binds the exact incumbent this rebuild
        // replaces. This happens before any exclusive scope transaction.
        let incumbent_manifest = if self.undecoded_active_expectation.is_none() {
            self.load_active_manifest()?.map(|active| active.metadata)
        } else {
            None
        };
        let undecoded_expectation = match self.undecoded_active_expectation.clone() {
            Some(expectation) => Some(expectation),
            None if incumbent_manifest.is_some() => None,
            None => self
                .read_publication_pointer()?
                .as_ref()
                .map(UndecodedActivePublicationExpectationV1::from_pointer),
        };
        let (incumbent_bytes, pointer) = {
            let _read = self.acquire_generation_read_lock()?;
            match self.read_publication_pointer_with_bytes()? {
                Some((pointer, bytes)) => (Some(bytes), Some(pointer)),
                None => (None, None),
            }
        };
        match undecoded_expectation.as_ref() {
            Some(expected)
                if expected_active_generation.is_none()
                    && pointer
                        .as_ref()
                        .is_some_and(|pointer| expected.matches(pointer)) => {}
            None if pointer
                .as_ref()
                .map(|pointer| pointer.generation_id.as_str())
                == expected_active_generation.map(CodeGenerationId::as_str) => {}
            _ => return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap),
        }
        if self.disposition == CodeIndexPublicationDispositionV1::RetainedHistory
            && pointer.is_none()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        #[cfg(unix)]
        let store_identity = UnixDirectoryIdentityV1::at(store_root)?;
        #[cfg(not(unix))]
        let store_identity = Handle::from_path(store_root).map_err(Self::unavailable)?;
        #[cfg(unix)]
        let segments_identity = UnixDirectoryIdentityV1::at(&self.segments_root)?;
        #[cfg(not(unix))]
        let segments_identity =
            Handle::from_path(&self.segments_root).map_err(Self::unavailable)?;
        #[cfg(unix)]
        let generations_identity = UnixDirectoryIdentityV1::at(&self.generations_root)?;
        #[cfg(not(unix))]
        let generations_identity =
            Handle::from_path(&self.generations_root).map_err(Self::unavailable)?;
        self.seal_checkpoint()?;
        let directory = tempfile::Builder::new()
            .prefix(CODE_INDEX_PREPARATION_DIRECTORY_PREFIX_V1)
            .tempdir_in(&self.segments_root)
            .map_err(Self::unavailable)?;
        let evidence =
            TemporaryPayloadPackV1::create(directory.path().join("generation-evidence.tmp"))?;
        let mut memory = PreparedPackMemoryV1::new(operation)?;
        memory.admit(
            size_of::<DaemonCodeIndexPreparationFilesV1>()
                .saturating_add(directory.path().as_os_str().len().saturating_mul(4)),
        )?;
        let logical = StagedContentV1::create(directory.path().join("logical-content.tmp"))?;
        self.seal_encoded_segment_bytes.store(0, Ordering::Relaxed);
        self.seal_existing_segment_bytes_read
            .store(0, Ordering::Relaxed);
        self.seal_evidence_page_count.store(0, Ordering::Relaxed);
        self.seal_evidence_durable_transaction_count
            .store(0, Ordering::Relaxed);
        Ok(DaemonCodeIndexPreparationV1 {
            scope: scope.clone(),
            expected_active_generation: expected_active_generation.cloned(),
            undecoded_expectation,
            incumbent_bytes,
            incumbent_manifest,
            store_identity,
            segments_identity,
            generations_identity,
            cache: Arc::clone(&self.cache),
            active_path: self.active_path.clone(),
            segments_root: self.segments_root.clone(),
            disposition: self.disposition,
            state: Arc::new(Mutex::new(DaemonCodeIndexPreparationStateV1::Open(
                Box::new(DaemonCodeIndexPreparationFilesV1 {
                    directory,
                    segments: StagedGenerationSegmentsV1::new(),
                    evidence,
                    logical,
                    parent: None,
                    content_pack: None,
                    evidence_identity: None,
                    payload_reads: None,
                    segment_read_open_attempts: PackReadOpenAttemptsV1::default(),
                    segment_reads_poisoned: false,
                    _retention: retention,
                    referenced_segment_bytes: 0,
                    manifest_identity: None,
                    pending_pointer: None,
                    segments_durable: false,
                    memory,
                }),
            ))),
            shutdown_signal: self.shutdown_signal.clone(),
            capture_control: self
                .reconcile_publication_fence
                .as_ref()
                .map(|(_, control)| control.clone()),
            encoded_segment_bytes: Arc::clone(&self.seal_encoded_segment_bytes),
            existing_segment_bytes_read: Arc::clone(&self.seal_existing_segment_bytes_read),
            evidence_page_count: Arc::clone(&self.seal_evidence_page_count),
            evidence_durable_transaction_count: Arc::clone(
                &self.seal_evidence_durable_transaction_count,
            ),
            #[cfg(test)]
            segment_observer: self.seal_segment_observer.clone(),
        })
    }

    #[tracing::instrument(name = "code_index.generation.publish", level = "trace", skip_all)]
    fn commit(
        &mut self,
        preparation: &mut Self::Preparation,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        let result = self.commit_prepared(preparation, generation);
        if matches!(
            &result,
            Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap)
        ) {
            preparation.invalidate_uncommitted();
        }
        result
    }
}

impl DaemonCodeIndexPublicationStoreV1 {
    fn committed_preparation_owner(
        &self,
        files: &DaemonCodeIndexPreparationFilesV1,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<CommittedPreparationPacksV1>, CodeIndexPublicationStoreErrorV1> {
        let state_digest = files
            .manifest_identity
            .as_ref()
            .ok_or_else(|| Self::unavailable("prepared manifest identity is absent"))?;
        let suffix = sha256_hex_suffix(state_digest)
            .ok_or_else(|| Self::corruption("prepared manifest digest is not sha256"))?;
        let mut memory = PreparedPackMemoryV1::new(files.memory.operation())?;
        memory.admit(
            generation
                .pack_layout()
                .retained_bytes()
                .saturating_add(size_of::<AdmittedPackLayoutV1>())
                .saturating_add(size_of::<CommittedPreparationPacksV1>())
                .saturating_add(size_of::<GenerationReadAuthorityV1>())
                .saturating_add(size_of::<DurableSealedCodeGenerationIdentityV1>())
                .saturating_add(size_of::<usize>().saturating_mul(10))
                .saturating_add(self.active_path.as_os_str().len())
                .saturating_add(self.segments_root.as_os_str().len())
                .saturating_add(state_digest.len().saturating_mul(2)),
        )?;
        let identity = DurableSealedCodeGenerationIdentityV1 {
            locator: format!("generation-{suffix}.json"),
            digest: ManifestDigest::new(state_digest.clone()).map_err(Self::corruption)?,
            size_bytes: u64::try_from(generation.manifest_bytes().len())
                .map_err(Self::unavailable)?,
        };
        let layout = Arc::new(AdmittedPackLayoutV1 {
            layout: generation.pack_layout().clone(),
            _memory: memory,
        });
        let factory_layout = Arc::clone(&layout);
        let authority = self.read_authority();
        let open_read_pass: Arc<SealedGenerationPackReadFactoryV1> = Arc::new(move || {
            let lease = authority
                .try_lease()?
                .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
            let pass = Mutex::new(authority.open_under_lease(
                &identity,
                &factory_layout.layout,
                factory_layout._memory.operation(),
                lease,
            )?);
            Ok(Arc::new(
                move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                    buffer.clear();
                    pass.lock()
                        .map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed pack read pass was poisoned".to_owned(),
                            )
                        })?
                        .read(request, buffer)
                },
            ) as Arc<SealedSegmentReaderV1>)
        });
        Ok(Arc::new(CommittedPreparationPacksV1 {
            layout,
            open_read_pass,
        }))
    }

    fn commit_prepared(
        &self,
        preparation: &DaemonCodeIndexPreparationV1,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        self.seal_checkpoint()?;
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        self.validate_preparation_store(preparation, generation)?;
        let mut session = preparation
            .state
            .lock()
            .map_err(|_| DaemonCodeIndexPreparationV1::closed())?;
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *session else {
            return Err(DaemonCodeIndexPreparationV1::closed());
        };
        if files.segment_reads_poisoned || files.logical.poisoned {
            return Err(Self::unavailable(
                "staged sealed segment identity was invalidated",
            ));
        }
        let manifest_bytes = generation.manifest_bytes();
        let generation_size = u64::try_from(manifest_bytes.len()).map_err(Self::unavailable)?;
        if generation_size > MAX_DURABLE_GENERATION_INDEX_BYTES_V1 {
            return Err(Self::unavailable(
                "sealed code generation exceeds the durable history byte bound",
            ));
        }
        let manifest_identity = Self::state_digest(manifest_bytes);
        if files
            .manifest_identity
            .as_ref()
            .is_some_and(|digest| digest != &manifest_identity)
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        files.manifest_identity = Some(manifest_identity);
        // A verified manifest from another preparation proves no ownership of
        // that preparation's private bytes. Authenticate every named segment
        // against this session's staged files or the canonical shared store.
        // Stream under the shared retention lease, before the final scope and
        // cache locks; canonical names are immutable and staging is frozen.
        self.verify_prepared_segments(files, generation)?;
        // Finish all allocations and admission before either pointer commit.
        // Replacing the open state then drops staging and its larger charge.
        let committed = self.committed_preparation_owner(files, generation)?;
        let _store_lock = try_acquire_code_generation_store_lock(store_root)
            .map_err(Self::unavailable)?
            .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
        self.validate_preparation_store(preparation, generation)?;
        let (prior_pointer, prior_bytes) = match self.read_publication_pointer_with_bytes()? {
            Some((pointer, bytes)) => (Some(pointer), Some(bytes)),
            None => (None, None),
        };
        let pointer_already_installed = files.pending_pointer.as_deref().is_some()
            && files.pending_pointer.as_deref() == prior_bytes.as_deref();
        if !pointer_already_installed && prior_bytes != preparation.incumbent_bytes {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let expected_active_generation = preparation.expected_active_generation.as_ref();
        let undecoded_expectation = preparation.undecoded_expectation.as_ref();
        if !pointer_already_installed {
            if let Some(expected) = undecoded_expectation {
                if expected_active_generation.is_some()
                    || !prior_pointer
                        .as_ref()
                        .is_some_and(|pointer| expected.matches(pointer))
                {
                    return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
                }
            } else if prior_pointer
                .as_ref()
                .map(|pointer| pointer.generation_id.as_str())
                != expected_active_generation.map(CodeGenerationId::as_str)
            {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
        }
        let cache = self.cache.lock_state()?;
        if !preparation.cache_matches(
            &cache,
            pointer_already_installed.then_some(&generation.manifest().generation_id),
        ) {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if pointer_already_installed {
            // The pointer rename succeeded but its directory fsync failed.
            // Only this session's exact planned bytes authorize completing it.
            drop(cache);
            let source_fence = self.publication_source_fence()?;
            let mut cache = self.cache.lock_state()?;
            if !preparation.cache_matches(&cache, Some(&generation.manifest().generation_id)) {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Self::sync_directory(store_root)?;
            self.finish_prepared_cache(&mut cache, generation, generation_size);
            drop(source_fence);
            *session = DaemonCodeIndexPreparationStateV1::Committed(committed);
            return Ok(Arc::clone(manifest_bytes));
        }
        drop(cache);
        if self.disposition == CodeIndexPublicationDispositionV1::RetainedHistory
            && prior_pointer.is_none()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        if self.disposition == CodeIndexPublicationDispositionV1::RetainedHistory
            && prior_pointer.as_ref().is_some_and(|pointer| {
                pointer.generation_id == generation.manifest().generation_id.as_str()
            })
        {
            return Err(Self::unavailable(
                "retained history generation aliases the active generation identity",
            ));
        }
        if let Some(entry) = expected_active_generation
            .filter(|expected| generation.manifest().parent_generation.as_ref() == Some(*expected))
            .and_then(|expected| {
                prior_pointer.as_ref().and_then(|pointer| {
                    pointer
                        .generation_index
                        .iter()
                        .find(|entry| entry.generation_id == expected.as_str())
                })
            })
        {
            Self::validate_generation_file(&entry.generation_file)?;
            let path = self.generations_root.join(&entry.generation_file);
            let metadata = path.symlink_metadata().map_err(Self::unavailable)?;
            if !metadata.file_type().is_file()
                || metadata.len() != entry.size_bytes
                || Self::state_digest_file(&path)? != entry.state_digest
            {
                return Err(Self::corruption(
                    "parent generation manifest identity is corrupt",
                ));
            }
        }
        if !files.segments_durable {
            // The state mutex excludes escaped readers. All payload identity
            // and digest verification is complete; release only healthy read
            // handles before pending names are renamed or deduplicated. A
            // partial publish retry reopens from the remaining pending map.
            drop(files.payload_reads.take());
            {
                let _span =
                    tracing::trace_span!("code_index.generation.publish.segments_flush").entered();
                files.segments.publish()?;
            }
            // A reused canonical name may have just been renamed by another
            // preparation. Its contents were flushed before naming; this
            // sync makes every referenced name durable before our manifest.
            {
                let _span = tracing::trace_span!("code_index.generation.publish.segments_dir_sync")
                    .entered();
                Self::sync_directory(&self.segments_root)?;
            }
            #[cfg(test)]
            if let Some(observer) = self.segments_dir_sync_observer.as_ref() {
                observer();
            }
            files.segments_durable = true;
        }
        self.seal_checkpoint()?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".generation-publication-")
            .tempfile_in(&self.generations_root)
            .map_err(Self::unavailable)?;
        {
            let _span = tracing::trace_span!("code_index.generation.publish.seal_fsync").entered();
            temporary
                .write_all(manifest_bytes)
                .map_err(Self::unavailable)?;
            temporary.as_file().sync_all().map_err(Self::unavailable)?;
        }
        let state_digest = {
            let _span =
                tracing::trace_span!("code_index.generation.publish.state_digest").entered();
            // Read back the fsynced bytes through the writer's existing handle.
            // A second pathname open is unnecessary and expensive under EDR.
            temporary
                .as_file_mut()
                .seek(SeekFrom::Start(0))
                .map_err(Self::unavailable)?;
            let digest = Self::state_digest_reader(temporary.as_file_mut())?;
            if digest != Self::state_digest(manifest_bytes) {
                return Err(Self::corruption(
                    "staged generation bytes changed after serialization",
                ));
            }
            digest
        };
        let suffix = sha256_hex_suffix(&state_digest)
            .ok_or_else(|| Self::unavailable("sealed generation digest is not sha256"))?;
        let generation_file = format!("generation-{suffix}.json");
        let generation_path = self.generations_root.join(&generation_file);
        match generation_path.symlink_metadata() {
            Ok(metadata) => {
                let _span =
                    tracing::trace_span!("code_index.generation.publish.dedupe_compare").entered();
                if !metadata.file_type().is_file()
                    || !Self::files_equal(&generation_path, temporary.path())?
                {
                    return Err(Self::corruption(
                        "immutable code-generation path contains different bytes",
                    ));
                }
                temporary.close().map_err(Self::unavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A concurrent publisher can win after the missing-path probe.
                // Never replace its immutable generation, even transiently.
                Self::publish_missing_generation_manifest(temporary, &generation_path)?;
            }
            Err(error) => return Err(Self::unavailable(error)),
        }
        Self::sync_directory(&self.generations_root)?;
        let exact_git_evidence = self.exact_git_evidence(generation.snapshot())?;
        let mut generation_index = prior_pointer
            .as_ref()
            .map_or_default(|pointer| pointer.generation_index.clone());
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
            segment_bytes: files.referenced_segment_bytes,
            generation_file: generation_file.clone(),
            state_digest: state_digest.clone(),
            source_reference: exact_git_evidence
                .as_ref()
                .map(|(reference, _, _)| reference.clone()),
            source_revision: exact_git_evidence
                .as_ref()
                .map(|(_, revision, _)| revision.clone()),
            source_tree: exact_git_evidence.map(|(_, _, tree)| tree),
            cardinality: Self::publication_cardinality(generation)?,
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
        let pointer = match self.disposition {
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
                generation_index,
                generation_index_truncated,
                generation_index_digest,
            },
            CodeIndexPublicationDispositionV1::RetainedHistory => DurablePublicationPointerV1 {
                generation_index,
                generation_index_truncated,
                generation_index_digest,
                ..prior_pointer.ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?
            },
        };
        let bytes = serde_json::to_vec(&pointer).map_err(|error| {
            CodeIndexPublicationStoreErrorV1::Unavailable(format!(
                "publication pointer serialization failed: {error}"
            ))
        })?;
        files.pending_pointer = Some(bytes.clone());
        // The current handle owns the refreshed source fence on every attempt.
        let source_fence = self.publication_source_fence()?;
        let mut cache = self.cache.lock_state()?;
        if !preparation.cache_matches(&cache, None) {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.pointer_commit").entered();
            self.commit_observed_pointer(&_store_lock, prior_bytes.as_deref(), &pointer, &bytes)?;
        }
        drop(source_fence);
        self.finish_prepared_cache(&mut cache, generation, generation_size);
        *session = DaemonCodeIndexPreparationStateV1::Committed(committed);
        Ok(Arc::clone(manifest_bytes))
    }

    fn validate_preparation_store(
        &self,
        preparation: &DaemonCodeIndexPreparationV1,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        if self.active_path != preparation.active_path
            || !Arc::ptr_eq(&self.cache, &preparation.cache)
            || self.disposition != preparation.disposition
            || CodeIndexGenerationScopeV1::for_snapshot(generation.snapshot()) != preparation.scope
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        #[cfg(unix)]
        let same_directories = UnixDirectoryIdentityV1::at(store_root)?
            == preparation.store_identity
            && UnixDirectoryIdentityV1::at(&self.segments_root)? == preparation.segments_identity
            && UnixDirectoryIdentityV1::at(&self.generations_root)?
                == preparation.generations_identity;
        #[cfg(not(unix))]
        let same_directories = Handle::from_path(store_root).map_err(Self::unavailable)?
            == preparation.store_identity
            && Handle::from_path(&self.segments_root).map_err(Self::unavailable)?
                == preparation.segments_identity
            && Handle::from_path(&self.generations_root).map_err(Self::unavailable)?
                == preparation.generations_identity;
        if !same_directories {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        Ok(())
    }

    fn verify_prepared_segments(
        &self,
        files: &DaemonCodeIndexPreparationFilesV1,
        publication: &CodeIndexSealedPublicationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let layout = publication.pack_layout();
        if files.content_pack.as_ref() != Some(layout.content_pack())
            || files.evidence_identity.as_ref() != Some(layout.evidence_pack())
        {
            return Err(Self::corruption(
                "prepared payload packs do not match this generation's authenticated layout",
            ));
        }
        let content = layout.content_pack();
        let evidence = layout.evidence_pack();
        let mut verified: Option<(&ManifestDigest, u64)> = None;
        for (digest, size_bytes) in [
            (&content.digest, content.size_bytes),
            (&evidence.digest, evidence.size_bytes),
        ] {
            self.seal_checkpoint()?;
            if let Some((prior_digest, prior_size)) = verified
                && prior_digest == digest
            {
                if prior_size != size_bytes {
                    return Err(Self::corruption(
                        "prepared pack carries conflicting extents",
                    ));
                }
                continue;
            }
            let suffix = sha256_hex_suffix(digest.as_str())
                .ok_or_else(|| Self::corruption("prepared segment digest is not sha256"))?;
            let canonical = self.segments_root.join(format!("segment-{suffix}.json"));
            let path = files.segments.pending.get(&canonical).unwrap_or(&canonical);
            let metadata = path.symlink_metadata().map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Self::corruption("prepared generation names a missing segment")
                } else {
                    Self::unavailable(error)
                }
            })?;
            if !metadata.file_type().is_file() || metadata.len() != size_bytes {
                return Err(Self::corruption(
                    "prepared generation segment does not match its manifest identity",
                ));
            }
            #[cfg(unix)]
            if let Some(original) = files.segments.authenticated_segments.get(path) {
                if !Self::same_unix_segment_identity(original, &metadata) {
                    return Err(Self::corruption(
                        "previously verified sealed segment changed before commit",
                    ));
                }
                verified = Some((digest, size_bytes));
                continue;
            }
            if Self::state_digest_file(path)? != digest.as_str() {
                return Err(Self::corruption(
                    "prepared generation segment does not match its manifest identity",
                ));
            }
            verified = Some((digest, size_bytes));
        }
        Ok(())
    }

    fn publication_source_fence(
        &self,
    ) -> Result<Option<MutexGuard<'_, PendingHintsV1>>, CodeIndexPublicationStoreErrorV1> {
        let guard = self
            .reconcile_publication_fence
            .as_ref()
            .map(|(hints, _)| hints.lock().unwrap_or_else(PoisonError::into_inner));
        self.seal_checkpoint()?;
        Ok(guard)
    }

    fn finish_prepared_cache(
        &self,
        state: &mut DecodedGenerationStateV1,
        generation: &CodeIndexSealedPublicationV1,
        generation_size: u64,
    ) {
        state.forget(&generation.manifest().generation_id);
        if self.disposition == CodeIndexPublicationDispositionV1::Active {
            state.active_epoch = state.active_epoch.wrapping_add(1);
            state.active = None;
            self.active_encoded_bytes
                .store(generation_size, Ordering::Release);
            *self
                .active_decode_charge
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
        *self
            .active_manifest_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.cache.ready.notify_all();
    }
}
