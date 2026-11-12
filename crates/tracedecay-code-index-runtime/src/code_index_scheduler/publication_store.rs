//! Durable publication store for sealed code-index generations: the shared
//! byte pool, the decoded-generation cache, and the atomic publication port.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::SystemTime,
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use sha2::{Digest, Sha256};
use tracedecay_application::code_index::DaemonCodeIndexControlV1;
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationStoreLockV1, CodeIndexScopeStoreResetV1,
    DurableGenerationCardinalityV1, DurableGenerationIndexEntryV1, DurablePublicationPointerV1,
    DurableSealedCodeGenerationIdentityV1,
    MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1, durable_generation_index_digest,
    retain_bounded_generation_index, try_acquire_code_generation_store_lock,
};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, ManifestDigest, ProjectId, SanitizerRevision, WorktreeId,
    canonical_text::encode_tagged_lowercase_hex, sha256_hex_suffix,
};
use tracedecay_graph_db::{GraphBudgetKind, GraphDbError};
use tracedecay_private_fs::framed_log::DirectorySyncPolicy;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryReservationV1, ResidentOwnersV1, log_resident_owner_release_v1,
    release_process_allocator_memory_v1,
};
use tracedecay_runtime_core::db::{
    CODE_INDEX_REGION_BYTES, CodeIndexGenerationKey, CodeIndexPayloadIdentity,
    CodeIndexPublicationStage, CodeIndexPublishOutcome, CodeIndexSqliteStorage,
    CodeIndexStorageError, CodeIndexStoredBytes, Database,
};
use tracedecay_store::StoreOperationIdV1;

use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeGraphBuildBoundV1, CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1,
        CodeIndexGenerationScopeV1, CodeIndexInterruptionV1, CodeIndexProductionErrorV1,
        CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
        CodeIndexSealedGenerationV1, CodeIndexSealedPublicationV1,
        SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, SealedSegmentReaderV1,
        SharedDecodedContentPoolV1, SharedPhysicalCodeArtifactPoolV1,
        VerifiedSealedTextGenerationMetadataV1,
    },
};

use super::{CodeIndexSchedulerErrorV1, PendingHintsV1};
use crate::code_graph_seat::CodeGraphBuildAdmissionV1;

const MAX_DURABLE_PUBLICATION_POINTER_BYTES: u64 = 512 * 1024;

/// This owner lives inside the existing blocking publication boundary. Cleanup
/// only removes its own staged references; a completed commit has no staging.
struct PublicationStageCleanup {
    storage: CodeIndexSqliteStorage,
    operation: StoreOperationIdV1,
    published: bool,
}

impl Drop for PublicationStageCleanup {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if let Err(error) = self.storage.abandon_stage(&self.operation) {
            tracing::warn!(%error, operation = %self.operation, "code-index staging cleanup failed");
        }
    }
}

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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, entry| entry.strong_count() > 0);
        self.physical_artifacts.release_dead_entries();
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> CodeIndexBytePoolStatsV1 {
        let physical_artifacts = self.physical_artifacts.stats();
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    project_database: Arc<Database>,
    worktree_id: WorktreeId,
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
    /// Last generation handed to `publish_atomically`. A transient store
    /// failure must not drop it: the next undecoded retry republishes this
    /// candidate instead of extracting the whole worktree again.
    pub(super) unpublished_candidate: Arc<Mutex<Option<CodeIndexSealedPublicationV1>>>,
    /// The active pointer's authenticated manifest, by its state digest, so
    /// builds and status over one publication read and parse it once.
    active_manifest_memo: Arc<Mutex<Option<ActiveSealedManifestV1>>>,
}

/// The durable active generation's sealed manifest and its authenticated
/// metadata, read without decoding any segment.
#[derive(Clone)]
pub(super) struct ActiveSealedManifestV1 {
    state_digest: String,
    pub(super) manifest_bytes: Arc<CodeIndexStoredBytes>,
    pub(super) metadata: Arc<VerifiedSealedTextGenerationMetadataV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexPublicationDispositionV1 {
    Active,
    RetainedHistory,
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn new(
        store_root: &Path,
        project_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
        project_database: Arc<Database>,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let generations_root = store_root.join("code-generations-v1");
        tracedecay_private_fs::create_private_directory(store_root)?;
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
        if store_root.join("active-code-generation-v1.json").try_exists()?
            || generations_root.try_exists()?
        {
            return Err(CodeIndexProductionErrorV1::Publication(Self::corruption(
                "loose code-index storage requires explicit derived-store reset; project memory is preserved",
            )).into());
        }
        Ok(Self {
            project_database,
            worktree_id: super::identity::worktree_id_for(project_root)
                .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?,
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

    pub(super) fn sql_storage(&self) -> Result<CodeIndexSqliteStorage, CodeIndexPublicationStoreErrorV1> {
        let admission = self.decode_admission.lock().unwrap_or_else(PoisonError::into_inner)
            .clone().ok_or_else(|| Self::unavailable("code-index storage has no resident authority"))?;
        self.project_database.project_code_index_storage(
            admission.resident_memory, &admission.project_id,
        ).map_err(Self::unavailable)
    }

    fn sql_error(error: CodeIndexStorageError) -> CodeIndexPublicationStoreErrorV1 {
        match error {
            CodeIndexStorageError::Corrupt(_) => Self::corruption(error),
            CodeIndexStorageError::AllocationRefused(_) => {
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(error.to_string())
            }
            _ => Self::unavailable(error),
        }
    }

    pub(super) fn sql_generation(&self, generation: &CodeGenerationId) -> CodeIndexGenerationKey {
        CodeIndexGenerationKey {
            worktree_id: self.worktree_id.clone(),
            generation_id: generation.clone(),
        }
    }

    pub(super) fn read_sql_manifest(
        &self, generation: &CodeGenerationId, digest: &ManifestDigest,
    ) -> Result<CodeIndexStoredBytes, CodeIndexPublicationStoreErrorV1> {
        self.sql_storage()?.read_manifest(&self.sql_generation(generation), digest)
            .map_err(Self::sql_error)
    }

    fn read_sql_segment(
        &self, generation: &CodeGenerationId, request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<CodeIndexStoredBytes, CodeIndexProductionErrorV1> {
        let (digest, size_bytes, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, size_bytes, 0, size_bytes),
            SealedGenerationSegmentReadV1::Range { digest, size_bytes, offset, length } =>
                (digest, size_bytes, offset, length),
        };
        self.sql_storage()?.read_range(
            &self.sql_generation(generation), &CodeIndexPayloadIdentity { digest: digest.clone(), size_bytes },
            offset, length,
        ).map_err(|error| match error {
            CodeIndexStorageError::Retired => CodeIndexProductionErrorV1::SealedGenerationRetired,
            error => CodeIndexProductionErrorV1::Publication(Self::sql_error(error)),
        })
    }

    fn stage_sql_payload(
        &self,
        storage: &CodeIndexSqliteStorage,
        stage: &CodeIndexPublicationStage,
        digest: &ManifestDigest,
        bytes: &[u8],
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let payload = CodeIndexPayloadIdentity { digest: digest.clone(), size_bytes: bytes.len() as u64 };
        storage.stage_payload(&stage.operation_id, &payload).map_err(Self::sql_error)?;
        for (ordinal, region) in bytes.chunks(CODE_INDEX_REGION_BYTES).enumerate() {
            self.seal_checkpoint().map_err(Self::unavailable)?;
            storage.put_region(&stage.operation_id, &payload, ordinal as u64, region)
                .map_err(Self::sql_error)?;
        }
        storage.seal_payload(&stage.operation_id, &payload).map_err(Self::sql_error)
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
                tracedecay_code_index::production::CodeIndexInterruptionV1::Cancelled,
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

    pub(super) fn sync_directory(path: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        tracedecay_private_fs::framed_log::sync_directory(path, DirectorySyncPolicy::Strict)
            .map_err(Self::unavailable)
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
        let bytes = match self.sql_storage()?.read_head(&self.worktree_id).map_err(Self::sql_error)? {
            Some(bytes) => bytes,
            None => {
                *self
                    .pointer_memo
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = None;
                return Ok(None);
            }
        };
        if bytes.len() as u64 > MAX_DURABLE_PUBLICATION_POINTER_BYTES {
            return Err(Self::corruption(
                "durable code-generation index exceeds its byte bound",
            ));
        }
        let mtime = None;
        let size = bytes.len() as u64;
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
        let mut memo = self
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Install only when the file is still the bytes just parsed. A rename
        // that landed during validation owns the memo.
        if self.sql_storage()?.read_head(&self.worktree_id).map_err(Self::sql_error)?
            .as_ref().is_some_and(|head| head.as_bytes() == bytes.as_bytes()) {
            *memo = Some(PublicationPointerMemoV1 {
                mtime,
                size,
                digest,
                pointer: pointer.clone(),
            });
        }
        Ok(Some(pointer))
    }

    fn remember_publication_pointer(&self, pointer: &DurablePublicationPointerV1, bytes: &[u8]) {
        let mut memo = self
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // The memo and the file it names are one critical section. A publisher
        // that observed older bytes must not install them over a newer file.
        match self.sql_storage().and_then(|storage| storage.read_head(&self.worktree_id).map_err(Self::sql_error)) {
            Ok(Some(current)) if current.as_bytes() == bytes => {
                *memo = Some(PublicationPointerMemoV1 {
                    mtime: None,
                    size: current.len() as u64,
                    digest: Self::state_digest(bytes),
                    pointer: pointer.clone(),
                });
            }
            Ok(_) => {}
            Err(_) => *memo = None,
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
        let current = self.sql_storage()?.read_head(&self.worktree_id).map_err(Self::sql_error)?;
        if current.as_ref().map(CodeIndexStoredBytes::as_bytes) != observed {
            return Err(match current {
                Some(current)
                    if serde_json::from_slice::<DurablePublicationPointerV1>(&current).is_err() =>
                {
                    Self::corruption("active code-generation pointer is corrupt")
                }
                _ => CodeIndexPublicationStoreErrorV1::CompareAndSwap,
            });
        }
        let generation = CodeGenerationId::new(pointer.generation_id.clone()).map_err(Self::corruption)?;
        let expected = observed.ok_or(CodeIndexPublicationStoreErrorV1::CompareAndSwap)?;
        if !self.sql_storage()?.compare_and_swap_head(&self.sql_generation(&generation), expected, bytes)
            .map_err(Self::sql_error)? {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        self.remember_publication_pointer(pointer, bytes);
        Ok(())
    }

    pub(super) fn read_retained_partitioned_segment(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        request: SealedGenerationSegmentReadV1<'_>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexStoredBytes, CodeIndexProductionErrorV1> {
        if control.is_cancelled() {
            return Err(CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled));
        }
        if control.is_deadline_exceeded() {
            return Err(CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded));
        }
        let pointer = self.read_publication_pointer()?;
        let generation = pointer.as_ref().and_then(|pointer| {
            pointer.generation_index.iter().find(|entry| {
                entry.generation_file == identity.locator
                    && entry.state_digest == identity.digest.as_str()
                    && entry.size_bytes == identity.size_bytes
            }).map(|entry| entry.generation_id.clone())
        }).ok_or(CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled))?;
        let generation = CodeGenerationId::new(generation).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(error.to_string())
        })?;
        self.read_sql_segment(&generation, request)
    }

    /// A reader over this store's segments that outlives any one borrow of
    /// the store, for a build over the sealed active generation.
    pub(super) fn segment_reader(&self, generation: CodeGenerationId) -> Arc<SealedSegmentReaderV1> {
        let store = self.clone();
        Arc::new(move |request| store.read_sql_segment(&generation, request))
    }

    pub(super) fn encoded_read_memory(&self) -> Result<Arc<ProcessResidentMemoryV1>, CodeIndexProductionErrorV1> {
        self.decode_admission.lock().unwrap_or_else(PoisonError::into_inner)
            .as_ref().map(|admission| Arc::clone(&admission.resident_memory))
            .ok_or_else(|| CodeIndexProductionErrorV1::Publication(
                Self::unavailable("encoded reader has no mounted resident-memory authority"),
            ))
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
        let pointer = self.read_publication_pointer()?.ok_or_else(|| Self::corruption("missing code-index head"))?;
        let entry = pointer.generation_index.iter().find(|entry| {
            entry.generation_file == identity.locator && entry.state_digest == identity.digest.as_str()
        }).ok_or_else(|| Self::corruption("manifest is not retained by its exact scope"))?;
        let generation = CodeGenerationId::new(entry.generation_id.clone()).map_err(Self::corruption)?;
        let bytes = self.read_sql_manifest(&generation, &identity.digest)?;
        if bytes.len() as u64 != identity.size_bytes {
            return Err(Self::corruption(
                "partitioned generation manifest identity is corrupt",
            ));
        }
        if Self::state_digest(&bytes) != identity.digest.as_str() {
            return Err(Self::corruption(
                "partitioned generation manifest digest does not verify",
            ));
        }
        match CodeIndexPublishedGenerationV1::partitioned_text_metadata(&bytes) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(
                CodeIndexProductionErrorV1::SourceCommitmentsUnavailable
                | CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_),
            ) => Ok(None),
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
        generation_id: &CodeGenerationId,
        expected_file_digest: &ManifestDigest,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let manifest = self.read_sql_manifest(generation_id, expected_file_digest)?;
        match CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            &manifest,
            &self.decoded_content,
            |request| self.read_sql_segment(generation_id, request),
        ) {
            Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => Ok(None),
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
        let expected_digest = ManifestDigest::new(entry.state_digest.clone()).map_err(|error| {
            Self::corruption(format!(
                "durable code-generation digest is not canonical: {error}"
            ))
        })?;
        let decoded = {
            let _span = tracing::trace_span!("code_index.generation.decode.file_read").entered();
            self.decode_generation_file(
                generation_id,
                &expected_digest,
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
        let generation_id = CodeGenerationId::new(pointer.generation_id.clone()).map_err(Self::corruption)?;
        let digest = ManifestDigest::new(pointer.state_digest.clone()).map_err(Self::corruption)?;
        let bytes = self.read_sql_manifest(&generation_id, &digest)?;
        if bytes.len() as u64 != entry.size_bytes {
            return Err(Self::corruption(
                "active code-generation byte size does not match its durable entry",
            ));
        }
        if Self::state_digest(&bytes) != pointer.state_digest {
            return Err(Self::corruption(
                "active code-generation manifest digest does not verify",
            ));
        }
        let manifest = match CodeIndexPublishedGenerationV1::partitioned_text_metadata(&bytes) {
            Ok(manifest) => manifest,
            Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => return Ok(None),
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)) => {
                tracing::warn!(
                    target: "tracedecay::code_index",
                    sealed_format_revision = revision,
                    "{}",
                    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                );
                return Ok(None);
            }
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
        let active = ActiveSealedManifestV1 {
            state_digest: pointer.state_digest,
            manifest_bytes: Arc::new(bytes),
            metadata: Arc::new(manifest),
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
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        let active_entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == pointer.generation_id)
            .ok_or_else(|| {
                Self::corruption(
                    "durable code-generation index does not contain its active generation",
                )
            })?;
        let expected_digest =
            ManifestDigest::new(pointer.state_digest.clone()).map_err(|error| {
                Self::corruption(format!(
                    "active code-generation digest is not canonical: {error}"
                ))
            })?;
        let generation_id = CodeGenerationId::new(pointer.generation_id.clone()).map_err(Self::corruption)?;
        let decoded = {
            let _span = tracing::trace_span!("code_index.generation.decode.file_read").entered();
            self.decode_generation_file(&generation_id, &expected_digest)
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
        let encoded_bytes = active_entry.size_bytes;
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
        let _store_lock = try_acquire_code_generation_store_lock(store_root)
            .map_err(Self::unavailable)?
            .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
        let storage = self.sql_storage()?;
        let head = storage.read_head(&self.worktree_id).map_err(Self::sql_error)?;
        let receipt = match head {
            Some(head) => {
                if !storage.reset_head(&self.worktree_id, &head).map_err(Self::sql_error)? {
                    return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
                }
                CodeIndexScopeStoreResetV1 {
                    removed_entries: 1,
                    removed_bytes: head.len() as u64,
                }
            }
            None => CodeIndexScopeStoreResetV1::default(),
        };
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
            .pointer_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
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

    pub(super) fn take_unpublished(&self) -> Option<CodeIndexSealedPublicationV1> {
        self.unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub(super) fn restore_unpublished(&self, generation: CodeIndexSealedPublicationV1) {
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
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        if self.undecoded_active_expectation.is_some() {
            return Ok(None);
        }
        Ok(self.load_active_manifest()?.map(|active| {
            let generation = active.metadata.manifest().generation_id.clone();
            CodeIndexSealedGenerationV1::new(active.manifest_bytes, self.segment_reader(generation))
        }))
    }

    #[tracing::instrument(name = "code_index.generation.publish", level = "trace", skip_all)]
    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<CodeIndexStoredBytes>, CodeIndexPublicationStoreErrorV1> {
        *self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(generation.clone());
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
                // Reading the active manifest takes the store lock itself.
                // Complete it before entering the publication transaction,
                // then recheck the exact expected pointer under the writer
                // lock below.
                match self.load_active_manifest()? {
                    Some(_) => None,
                    // An active pointer whose sealed generation this build
                    // abstains from decoding, a retired format revision, a
                    // superseded sanitizer, is still the incumbent this
                    // publication replaces, and its caller has no decoded
                    // generation id to expect. The compare-and-swap token is
                    // then the pointer identity the abstention observed,
                    // rechecked under the writer lock below; without it a
                    // store holding an undecodable generation could never be
                    // replaced by the rebuild that supersedes it.
                    None => self
                        .read_publication_pointer()?
                        .as_ref()
                        .map(UndecodedActivePublicationExpectationV1::from_pointer),
                }
            }
        };
        let _store_lock = try_acquire_code_generation_store_lock(store_root)
            .map_err(Self::unavailable)?
            .ok_or(CodeIndexPublicationStoreErrorV1::StoreLockContended)?;
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
        // The bytes behind `prior_pointer`, captured under the store lock.
        // The commit below refuses to rename unless the file is still these
        // exact bytes, so a pointer that changed after this observation is
        // not overwritten.
        let prior_bytes = self.sql_storage()?.read_head(&self.worktree_id).map_err(Self::sql_error)?;
        if undecoded_expectation.is_none()
            && prior_pointer
                .as_ref()
                .map(|pointer| pointer.generation_id.as_str())
                != expected_active_generation.map(CodeGenerationId::as_str)
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        // A decode cached for another generation than the pointer names is
        // a stale read the publication must not race; an empty cache is the
        // ordinary state after a sparse successor or a released decode.
        let state = self.cache.lock_state()?;
        let cached_active = state
            .active
            .as_ref()
            .map(|current| &current.manifest().generation_id);
        let cache_matches = cached_active.is_none_or(|cached| {
            undecoded_expectation
                .as_ref()
                .map_or(Some(cached) == expected_active_generation, |expected| {
                    cached.as_str() == expected.generation_id.as_str()
                })
        });
        if !cache_matches {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        drop(state);
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
                let generation = CodeGenerationId::new(entry.generation_id.clone()).map_err(Self::corruption)?;
                let digest = ManifestDigest::new(entry.state_digest.clone()).map_err(Self::corruption)?;
                self.read_sql_manifest(&generation, &digest)
            })
            .transpose()?;
        let storage = self.sql_storage()?;
        let encode_memory = self.encoded_read_memory().map_err(Self::unavailable)?;
        let operation = StoreOperationIdV1::new(format!(
            "code-index:{}:{}", self.worktree_id, generation.manifest().generation_id,
        )).map_err(Self::unavailable)?;
        let stage = CodeIndexPublicationStage {
            operation_id: operation,
            generation: self.sql_generation(&generation.manifest().generation_id),
            expected_head: prior_bytes.map(Arc::new),
        };
        storage.begin_stage(&stage).map_err(Self::sql_error)?;
        let mut stage_cleanup = PublicationStageCleanup {
            storage: storage.clone(),
            operation: stage.operation_id.clone(),
            published: false,
        };
        let mut referenced_segment_bytes = 0_u64;
        self.seal_encoded_segment_bytes.store(0, Ordering::Relaxed);
        self.seal_existing_segment_bytes_read
            .store(0, Ordering::Relaxed);
        self.seal_evidence_page_count.store(0, Ordering::Relaxed);
        self.seal_evidence_durable_transaction_count
            .store(0, Ordering::Relaxed);
        let manifest_bytes = {
            let _span =
                tracing::trace_span!("code_index.generation.publish.segment_encode").entered();
            generation.encode(parent_manifest_bytes.as_ref().map(CodeIndexStoredBytes::as_bytes), &encode_memory, |publication| {
                self.seal_checkpoint()?;
                match publication {
                    SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                        let segment_size = u64::try_from(bytes.len()).map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed segment length exceeds u64".to_owned(),
                            )
                        })?;
                        {
                            let _span = tracing::trace_span!(
                                "code_index.generation.publish.segment_durable"
                            )
                            .entered();
                            self.stage_sql_payload(&storage, &stage, digest, bytes)
                        }
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                        #[cfg(test)]
                        if let Some(observer) = self.seal_segment_observer.as_ref() {
                            observer();
                        }
                        referenced_segment_bytes =
                            referenced_segment_bytes.saturating_add(segment_size);
                        self.seal_encoded_segment_bytes
                            .fetch_add(segment_size, Ordering::Relaxed);
                    }
                    SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
                    | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => {
                        let segment_size = u64::try_from(bytes.len()).map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed file evidence length exceeds u64".to_owned(),
                            )
                        })?;
                        {
                            let _span = tracing::trace_span!(
                                "code_index.generation.publish.file_evidence_durable"
                            )
                            .entered();
                            self.stage_sql_payload(&storage, &stage, digest, bytes)
                        }
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                        referenced_segment_bytes =
                            referenced_segment_bytes.saturating_add(segment_size);
                    }
                    SealedGenerationSegmentPublicationV1::CodeGraphPage {
                        page_digest,
                        bytes,
                        ..
                    } => {
                        let segment_size = u64::try_from(bytes.len()).map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed graph page length exceeds u64".to_owned(),
                            )
                        })?;
                        {
                            let _span = tracing::trace_span!(
                                "code_index.generation.publish.graph_page_durable"
                            )
                            .entered();
                            self.stage_sql_payload(&storage, &stage, page_digest, bytes)
                        }
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                        referenced_segment_bytes =
                            referenced_segment_bytes.saturating_add(segment_size);
                    }
                    SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                        page_ordinal,
                        page_digest,
                        bytes,
                    } => {
                        {
                            let _span = tracing::trace_span!(
                                "code_index.generation.publish.evidence_page_append"
                            )
                            .entered();
                            storage.stage_evidence_page(&stage.operation_id, u64::from(page_ordinal), page_digest, bytes)
                                .map_err(Self::sql_error)
                        }
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                        self.seal_evidence_page_count
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                        segment_digest,
                        segment_size_bytes,
                        page_count,
                    } => {
                        {
                            let _span = tracing::trace_span!(
                                "code_index.generation.publish.evidence_commit"
                            )
                            .entered();
                            storage.commit_evidence_pages(&stage.operation_id, &CodeIndexPayloadIdentity {
                                digest: segment_digest.clone(), size_bytes: segment_size_bytes,
                            }, u64::from(page_count)).map_err(Self::sql_error)
                        }
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                        self.seal_evidence_durable_transaction_count.fetch_add(1, Ordering::Relaxed);
                        referenced_segment_bytes =
                            referenced_segment_bytes.saturating_add(segment_size_bytes);
                    }
                }
                Ok(())
            })
        };
        let manifest_bytes = match manifest_bytes {
            Ok(bytes) => bytes,
            Err(CodeIndexProductionErrorV1::Interrupted(
                tracedecay_code_index::production::CodeIndexInterruptionV1::Cancelled,
            )) => {
                storage.abandon_stage(&stage.operation_id).map_err(Self::sql_error)?;
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            Err(error) => {
                storage.abandon_stage(&stage.operation_id).map_err(Self::sql_error)?;
                return Err(Self::unavailable(error));
            }
        };
        let generation_size = manifest_bytes.len() as u64;
        let state_digest = Self::state_digest(&manifest_bytes);
        let manifest_digest = ManifestDigest::new(state_digest.clone()).map_err(Self::corruption)?;
        self.stage_sql_payload(&storage, &stage, &manifest_digest, &manifest_bytes)?;
        for payload in CodeIndexPublishedGenerationV1::partitioned_segment_identities(&manifest_bytes)
            .map_err(Self::corruption)? {
            storage.stage_existing_reference(&stage.operation_id, &CodeIndexPayloadIdentity {
                digest: payload.digest, size_bytes: payload.size_bytes,
            }).map_err(Self::sql_error)?;
        }
        let generation_file = format!("generation-{}.json",
            sha256_hex_suffix(&state_digest).ok_or_else(|| Self::corruption("invalid manifest digest"))?,
        );

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
            cardinality: Some(Self::publication_cardinality(generation)?),
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
        let bytes = storage.encode_json(&pointer).map_err(Self::sql_error)?;
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
        {
            let _span =
                tracing::trace_span!("code_index.generation.publish.pointer_commit").entered();
            storage.publish_head(&stage, &CodeIndexPayloadIdentity {
                digest: manifest_digest, size_bytes: generation_size,
            }, &bytes, &CodeGenerationId::new(pointer.generation_id.clone()).map_err(Self::corruption)?)
                .map_err(Self::sql_error)
                .and_then(|outcome| match outcome {
                    CodeIndexPublishOutcome::Published | CodeIndexPublishOutcome::AlreadyPublished => Ok(()),
                    CodeIndexPublishOutcome::HeadConflict => Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap),
                })
        }?;
        stage_cleanup.published = true;
        drop(source_fence);
        let charges = generation
            .decoded()
            .filter(|_| matches!(self.disposition, CodeIndexPublicationDispositionV1::Active))
            .map(|decoded| Self::active_generation_charges(decoded));
        let mut state = self.cache.lock_state()?;
        if undecoded_expectation.is_none() {
            let cached_active = state
                .active
                .as_ref()
                .map(|current| &current.manifest().generation_id);
            if cached_active.is_some_and(|cached| Some(cached) != expected_active_generation) {
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
                if let Some(charges) = charges {
                    self.record_active_generation_charges(charges);
                }
                state.active = generation.decoded().map(Arc::clone);
            }
            CodeIndexPublicationDispositionV1::RetainedHistory => {
                if let Some(decoded) = generation.decoded() {
                    state.decoded.push_back(Arc::clone(decoded));
                }
                while state.decoded.len() > DECODED_GENERATION_CACHE_CAPACITY {
                    state.decoded.pop_front();
                }
            }
        }
        drop(state);
        *self
            .active_manifest_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self
            .unpublished_candidate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        Ok(Arc::new(manifest_bytes))
    }
}
