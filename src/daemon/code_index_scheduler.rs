//! Daemon-owned scheduling and reconciliation for production code generations.
//!
//! Hook events are bounded wake-up hints only. Every run reconstructs its
//! source snapshot from gix's HEAD-tree/index/worktree status before content
//! digests decide whether publication is necessary.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use same_file::Handle;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::now_micros;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, ComponentRevision, ContentDigest,
    ExactAdmissionRuleRevision, FileOccurrenceId, ManifestDigest, PolicyRevisionId,
    PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, RepositoryDirtyStateV1, RepositoryId,
    RetrievalBudget, RetrieverBatch, RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, ScoreDomainId, SnapshotFileDispositionV1,
    WorktreeId, canonical_sha256,
};
use tracedecay_private_fs::{
    framed_log::DirectorySyncPolicy, open_private_file, validate_private_directory,
};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1, ResidentMemoryComponentIdV1,
    ResidentMemoryKeyV1, ResidentMemoryReservationV1,
};
use tracedecay_usecases::code_index::{
    DaemonCodeIndexControlV1, ProductionCodeIndexOwnerV1, open_production_code_index_owner_v1,
};

use self::freshness_witness::RestoreFreshnessWitnessV1;

use crate::{
    code_index::{
        chunks::content_digest,
        graph_projection::{
            CodeGraphEvidenceReader, CodeGraphProjectionError, CodeGraphProjectionStore,
        },
        languages::{LanguageRegistry, StaticLanguageRegistry},
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
            CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1,
            CodeIndexIgnoredSourceAdmissionV1, CodeIndexInputErrorV1, CodeIndexProductionConfigV1,
            CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
            CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
            SharedPhysicalCodeArtifactPoolV1, VerifiedSealedLexicalPageReadV1,
            VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalSourceReceiptV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
            ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
        },
    },
    privacy::CODE_SOURCE_SANITIZER_VERSION_V1,
    query::retrieval::{
        exact::{
            CentralExactAdmissionAuthorityV1, ExactLane, ExactLaneEvidence, ExactLaneRequest,
            ExactLaneRetriever,
        },
        graph::{GraphLane, production_code_index_freshness},
        lexical::{
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeExactLexicalArtifactReaderV1,
            CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1, CodeLexicalArtifactReaderV1,
            CodeLexicalProjectionMetadataV1, LexicalLane, LexicalLaneEvidence, LexicalLaneRequest,
            LexicalLaneRetriever,
        },
        ports::RetrievalPortError,
    },
    retention::code_index_generations::{
        DurableCodeTextArtifactDescriptorV1, DurableGenerationIndexEntryV1,
        DurablePublicationPointerV1, DurableSealedCodeGenerationIdentityV1,
        MAX_DURABLE_GENERATION_INDEX_BYTES_V1, MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1,
        acquire_code_generation_store_lock, attach_verified_text_artifact_under_lock,
        code_text_artifact_path, code_text_artifacts_root, durable_generation_index_digest,
        retain_bounded_generation_index, withdraw_verified_text_artifact_under_lock,
    },
};

const MAX_PENDING_HINTS: usize = 1_024;
const MAX_SUPERSEDED_RECONCILE_RETRIES: usize = 4;

const SUPERSEDED_RECONCILE_RETRY_BACKOFF: Duration = Duration::from_millis(75);
/// Freshness contract for non-git-mediated mutations (raw file writes, rsync,
/// out-of-agent saves): a query admitted after this bound since the last
/// reconciliation re-checks gix truth before serving. Git-mediated changes are
/// caught immediately by the tier-1 metadata check regardless of this bound.
const DEFAULT_STALENESS_THRESHOLD: Duration = Duration::from_secs(30);
const MAX_DURABLE_PUBLICATION_POINTER_BYTES: u64 = 512 * 1024;
/// Page bounds for streaming one sealed generation into the durable lexical
/// text artifact. One page is one bounded unit of background build progress.
const TEXT_ARTIFACT_PAGE_CHUNKS_V1: usize = 128;
const TEXT_ARTIFACT_PAGE_BYTES_V1: usize = 4 * 1024 * 1024;
/// One synchronous activation advances only this many page/finalization
/// operations. Larger caller hints are clamped so work accounting cannot
/// overflow and every expensive loop retains cancellation checkpoints.
const TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1: usize = 64;
/// Rows digested by one scheduler finalization operation. The builder persists
/// its exact section/row cursor after this bounded slice, avoiding both a
/// corpus-sized wake and one scheduler wake per individual `SQLite` row.
const TEXT_ARTIFACT_FINALIZATION_ROWS_PER_OPERATION_V1: usize = 4 * 1024;

pub(in crate::daemon) fn scoped_code_index_store_root(
    store_root: &Path,
    canonical_project_root: &Path,
) -> PathBuf {
    crate::retention::code_index_generations::scoped_code_index_store_root(
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
pub(super) struct CodeIndexHintPolicyV1 {
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
pub(super) struct CodeIndexBytePoolStatsV1 {
    pub inserted: u64,
    pub reused: u64,
    pub parse_chunk_inserted: u64,
    pub parse_chunk_reused: u64,
}

#[derive(Default)]
pub(super) struct SharedCodeIndexBytePoolV1 {
    bytes: Mutex<BTreeMap<ContentDigest, Weak<[u8]>>>,
    physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    inserted: AtomicU64,
    reused: AtomicU64,
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
pub(super) enum GenerationDecodeAdmissionV1 {
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

    #[cfg(test)]
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
pub(super) struct HeldActiveDecodeV1 {
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

#[derive(Clone)]
struct DaemonCodeIndexPublicationStoreV1 {
    cache: Arc<DecodedGenerationCacheV1>,
    active_encoded_bytes: Arc<AtomicU64>,
    active_path: PathBuf,
    generations_root: PathBuf,
    project_root: PathBuf,
    expected_sanitizer_revision: SanitizerRevision,
    disposition: CodeIndexPublicationDispositionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexPublicationDispositionV1 {
    Active,
    RetainedHistory,
}

impl DaemonCodeIndexPublicationStoreV1 {
    fn new(
        store_root: &Path,
        project_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let generations_root = store_root.join("code-generations-v1");
        std::fs::create_dir_all(&generations_root)?;
        Ok(Self {
            cache: Arc::new(DecodedGenerationCacheV1::default()),
            active_encoded_bytes: Arc::new(AtomicU64::new(0)),
            active_path: store_root.join("active-code-generation-v1.json"),
            generations_root,
            project_root: project_root.to_path_buf(),
            expected_sanitizer_revision,
            disposition: CodeIndexPublicationDispositionV1::Active,
        })
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

    fn sync_directory(path: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        tracedecay_private_fs::framed_log::sync_directory(path, DirectorySyncPolicy::Strict)
            .map_err(Self::unavailable)
    }

    fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(Self::unavailable)?;
        file.write_all(bytes).map_err(Self::unavailable)?;
        file.sync_all().map_err(Self::unavailable)
    }

    fn state_digest(bytes: &[u8]) -> String {
        format!("sha256:{}", sha256_hex(bytes))
    }

    fn generation_index_digest(
        entries: &[DurableGenerationIndexEntryV1],
        truncated: bool,
    ) -> Result<String, CodeIndexPublicationStoreErrorV1> {
        durable_generation_index_digest(entries, truncated).map_err(Self::unavailable)
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Self::unavailable(error)),
        };
        if metadata.len() > MAX_DURABLE_PUBLICATION_POINTER_BYTES {
            return Err(Self::corruption(
                "durable code-generation index exceeds its byte bound",
            ));
        }
        let bytes = std::fs::read(&self.active_path).map_err(Self::unavailable)?;
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
        Ok(Some(pointer))
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
        if !pointer
            .generation_index
            .iter()
            .any(|entry| entry.generation_id == generation_id.as_str())
        {
            return Ok(None);
        }
        if let Some(active) = self.load_active_shared()?
            && active.manifest().generation_id == *generation_id
        {
            return Ok(Some(active));
        }
        let subject = DecodeSubjectV1::Generation(generation_id.clone());
        let lease = loop {
            let mut state = self.cache.lock_state()?;
            if let Some(cached) = state.cached(generation_id) {
                return Ok(Some(cached));
            }
            if state.is_in_flight(&subject) {
                // Another caller already owns this O(store) decode. Park on it
                // rather than starting a second sweep over the same bytes.
                let _parked = self
                    .cache
                    .ready
                    .wait(state)
                    .map_err(|_| DecodedGenerationCacheV1::poisoned())?;
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
        let matched = self.load_indexed_generation(generation_id);
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
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let Some(pointer) = self.read_publication_pointer()? else {
            return Ok(None);
        };
        let Some(entry) = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
        else {
            return Ok(None);
        };
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
        let bytes = std::fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("durable code-generation index target disappeared during read")
            } else {
                Self::unavailable(error)
            }
        })?;
        let actual_size = u64::try_from(bytes.len()).map_err(Self::unavailable)?;
        if actual_size != entry.size_bytes {
            return Err(Self::corruption(
                "indexed code-generation byte size does not match its durable entry",
            ));
        }
        if Self::state_digest(&bytes) != entry.state_digest {
            return Err(Self::corruption(
                "indexed code-generation bytes do not match their sealed digest",
            ));
        }
        if !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&bytes)
            .map_err(Self::unavailable)?
        {
            return Ok(None);
        }
        self.cache.note_decode();
        let generation =
            CodeIndexPublishedGenerationV1::decode_sealed(&bytes).map_err(Self::corruption)?;
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
                let _parked = self
                    .cache
                    .ready
                    .wait(state)
                    .map_err(|_| DecodedGenerationCacheV1::poisoned())?;
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
    fn decode_active_generation(
        &self,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
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
        let generation_bytes = std::fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Self::corruption("active code-generation target is missing")
            } else {
                Self::unavailable(error)
            }
        })?;
        if Self::state_digest(&generation_bytes) != pointer.state_digest {
            return Err(Self::corruption(
                "sealed code-generation bytes do not match the active pointer digest",
            ));
        }
        if !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&generation_bytes)
            .map_err(Self::unavailable)?
        {
            return Ok(None);
        }
        self.cache.note_decode();
        let generation = CodeIndexPublishedGenerationV1::decode_sealed(&generation_bytes)
            .map_err(Self::corruption)?;
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
        let encoded_bytes = u64::try_from(generation_bytes.len()).unwrap_or(u64::MAX);
        self.active_encoded_bytes
            .store(encoded_bytes, Ordering::Release);
        Ok(Some(Arc::new(generation)))
    }

    fn active_encoded_bytes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.active_encoded_bytes)
    }

    /// Sealed-bytes decodes this process has performed against this store.
    #[cfg(test)]
    fn sealed_decode_count(&self) -> u64 {
        self.cache.decode_count()
    }
}

impl CodeIndexAtomicPublicationPort for DaemonCodeIndexPublicationStoreV1 {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .load_active_shared()?
            .map(|generation| generation.as_ref().clone()))
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let store_root = self
            .active_path
            .parent()
            .ok_or_else(|| Self::unavailable("active code-generation pointer has no store root"))?;
        let _store_lock =
            acquire_code_generation_store_lock(store_root).map_err(Self::unavailable)?;
        let _ = self.load_active_shared()?;
        let mut state = self.cache.lock_state()?;
        if state
            .active
            .as_ref()
            .map(|current| &current.manifest().generation_id)
            != expected_active_generation
        {
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
        let generation_bytes = generation.encode_sealed().map_err(Self::unavailable)?;
        let generation_size = u64::try_from(generation_bytes.len()).map_err(Self::unavailable)?;
        if generation_size > MAX_DURABLE_GENERATION_INDEX_BYTES_V1 {
            return Err(Self::unavailable(
                "sealed code generation exceeds the durable history byte bound",
            ));
        }
        let state_digest = Self::state_digest(&generation_bytes);
        let generation_file = format!(
            "generation-{}.json",
            state_digest
                .strip_prefix("sha256:")
                .unwrap_or(&state_digest)
        );
        let generation_path = self.generations_root.join(&generation_file);
        if generation_path.exists() {
            let existing = std::fs::read(&generation_path).map_err(Self::unavailable)?;
            if existing != generation_bytes {
                return Err(Self::unavailable(
                    "immutable code-generation path contains different bytes",
                ));
            }
        } else {
            let temporary = self
                .generations_root
                .join(format!(".{generation_file}.{}.tmp", std::process::id()));
            if temporary.exists() {
                std::fs::remove_file(&temporary).map_err(Self::unavailable)?;
            }
            Self::write_durable(&temporary, &generation_bytes)?;
            std::fs::rename(&temporary, &generation_path).map_err(Self::unavailable)?;
            Self::sync_directory(&self.generations_root)?;
        }

        let prior_pointer = self.read_publication_pointer()?;
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
            generation_file: generation_file.clone(),
            state_digest: state_digest.clone(),
            source_reference: exact_git_evidence
                .as_ref()
                .map(|(reference, _, _)| reference.clone()),
            source_revision: exact_git_evidence
                .as_ref()
                .map(|(_, revision, _)| revision.clone()),
            source_tree: exact_git_evidence.map(|(_, _, tree)| tree),
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
        let temporary = self
            .active_path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        if temporary.exists() {
            std::fs::remove_file(&temporary).map_err(Self::unavailable)?;
        }
        Self::write_durable(&temporary, &bytes)?;
        std::fs::rename(&temporary, &self.active_path).map_err(Self::unavailable)?;
        Self::sync_directory(
            self.active_path
                .parent()
                .ok_or_else(|| Self::unavailable("active pointer has no parent directory"))?,
        )?;
        let generation_id = generation.manifest().generation_id.clone();
        state.forget(&generation_id);
        match self.disposition {
            CodeIndexPublicationDispositionV1::Active => {
                self.active_encoded_bytes
                    .store(generation_size, Ordering::Release);
                // The published generation is already decoded and validated in
                // memory. Bumping the epoch retires any decode that started
                // against the prior pointer so it cannot install over this one.
                state.active_epoch = state.active_epoch.wrapping_add(1);
                state.active = Some(generation);
            }
            CodeIndexPublicationDispositionV1::RetainedHistory => {
                state.decoded.push_back(generation);
                while state.decoded.len() > DECODED_GENERATION_CACHE_CAPACITY {
                    state.decoded.pop_front();
                }
            }
        }
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
}

impl PendingHintsV1 {
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
}

/// One candidate path's capture result, produced independently per file so
/// the read/sanitize/digest sweep can run at machine width.
struct CapturedCandidateV1 {
    file: SanitizedCodeFileV1,
    captured: CodeIndexCapturedFileV1,
    receipt_id: SanitizationReceiptId,
    retained: Arc<[u8]>,
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
}

#[derive(Clone, Debug)]
pub(super) struct CodeIndexPublishEvidenceV1 {
    pub generation_id: CodeGenerationId,
    pub repository_id: RepositoryId,
    pub snapshot_content_identity: ContentDigest,
    /// Publication receipt evidence: asserted by determinism tests, not read
    /// on any production path.
    pub _lane_digest: ManifestDigest,
    /// Publication receipt evidence: asserted by determinism tests, not read
    /// on any production path.
    pub _file_occurrence_ids: Vec<FileOccurrenceId>,
    pub reextracted_files: usize,
    pub changed_chunks: usize,
    pub reused_chunks: usize,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub(super) struct CodeIndexNoopEvidenceV1 {
    pub snapshot_content_identity: ContentDigest,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub(super) enum CodeIndexReconcileOutcomeV1 {
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
    Arc<Mutex<Option<CodeTextArtifactBuildV1>>>,
    Arc<AtomicBool>,
    Arc<RwLock<CodeGraphActivationStateV1>>,
);

#[derive(Clone)]
pub(in crate::daemon) struct LatestCompleteCodeIndexV1 {
    generation: Arc<CodeIndexPublishedGenerationV1>,
    query_owners: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    record_index: Arc<OnceLock<queries::GenerationRecordIndexV1>>,
    /// Generation-owned partial durable text-artifact build. Only the
    /// background scheduler advances it;
    /// foreground queries observe typed warming until the immutable owners
    /// are installed.
    text_projection_build: Arc<Mutex<Option<CodeTextArtifactBuildV1>>>,
    text_projection_failed: Arc<AtomicBool>,
    /// The durable text-artifact store for this generation's store root.
    text_artifact_store: DaemonCodeTextArtifactStoreV1,
    /// Exact scheduler authorities for shutdown and superseding source epochs.
    /// Each bounded pass captures the then-current epoch before touching the
    /// durable source or artifact.
    text_control_epoch: Arc<AtomicU64>,
    text_control_shutdown: Arc<AtomicBool>,
    graph_activation: Arc<RwLock<CodeGraphActivationStateV1>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SemanticEvaluationCodeSnapshotV1 {
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub snapshot_digest: ManifestDigest,
    /// The sealed capability authority that calibrates the live semantic
    /// evaluation target; it is not inferred from an accepted profile.
    pub capability_manifest_digest: ManifestDigest,
}

/// Production exact/lexical owners bound to one immutable published generation.
#[derive(Clone)]
pub(super) struct ProductionCodeIndexQueryOwnersV1 {
    exact: ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactLexicalArtifactReaderV1<CentralExactAdmissionAuthorityV1>,
    >,
    lexical: LexicalLane<CodeLexicalArtifactReaderV1>,
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
        reader_reservation: ResidentMemoryReservationV1,
    ) -> Self {
        Self {
            exact,
            lexical,
            _reader_reservation: Arc::new(reader_reservation),
        }
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
        CodeLexicalArtifactErrorV1::Unreserved(_) => RetrievalPortError::BudgetExceeded,
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
pub(super) struct DaemonCodeTextArtifactStoreV1 {
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
        VerifiedSealedLexicalPageSourceV1::open_content_addressed(
            file,
            identity.size_bytes,
            identity.digest.clone(),
            TEXT_ARTIFACT_PAGE_CHUNKS_V1,
            TEXT_ARTIFACT_PAGE_BYTES_V1,
            control,
        )
        .map_err(map_sealed_page_source_error)
    }

    /// Durably publish one finalized staging artifact: content-address it,
    /// move it into the artifacts root, fsync the directory, and attach the
    /// descriptor to the sealed generation entry under the store lock.
    fn publish(
        &self,
        staging_path: &Path,
        generation: &CodeIndexPublishedGenerationV1,
        sealed_identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<DurableCodeTextArtifactDescriptorV1, RetrievalPortError> {
        let artifacts_root = code_text_artifacts_root(&self.store_root);
        ensure_private_text_artifacts_root(&artifacts_root)?;
        // Publication and artifact retention share this canonical store lock.
        // Hold it from the first staging observation until pointer attachment
        // is durable so retention cannot unlink a newly visible artifact from
        // a plan made before the descriptor was attached.
        let lock = acquire_code_generation_store_lock(&self.store_root)
            .map_err(text_artifact_unavailable)?;
        let (artifact_hex, artifact_size_bytes) =
            sha256_private_file_hex_and_size(staging_path, control)?;
        let descriptor = DurableCodeTextArtifactDescriptorV1 {
            generation_id: generation.manifest().generation_id.clone(),
            artifact_file: format!("text-artifact-{artifact_hex}.bin"),
            artifact_digest: ManifestDigest::new(format!("sha256:{artifact_hex}"))
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
                let (existing_hex, existing_size_bytes) =
                    sha256_private_file_hex_and_size(&final_path, control)?;
                if existing_size_bytes != artifact_size_bytes {
                    return Err(RetrievalPortError::Contract(
                        "existing code text artifact does not match its content address".to_owned(),
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
                std::fs::rename(staging_path, &final_path).map_err(text_artifact_unavailable)?;
            }
            Err(error) => return Err(text_artifact_unavailable(error)),
        }
        DaemonCodeIndexPublicationStoreV1::sync_directory(&artifacts_root)
            .map_err(text_artifact_unavailable)?;
        let pointer = self
            .publication
            .read_publication_pointer()
            .map_err(text_artifact_unavailable)?
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "no durable publication pointer exists for text-artifact attachment".to_owned(),
                )
            })?;
        attach_verified_text_artifact_under_lock(
            &lock,
            &pointer,
            sealed_identity,
            descriptor.clone(),
        )
        .map_err(text_artifact_unavailable)?;
        Ok(descriptor)
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
    #[cfg(test)]
    Memory,
}

impl LatestCompleteCodeIndexV1 {
    pub(in crate::daemon) fn generation(&self) -> &CodeIndexPublishedGenerationV1 {
        self.generation.as_ref()
    }

    /// Point-lookup indices over this sealed generation's record vectors.
    ///
    /// Built at most once per generation and shared by every clone of this
    /// handle (and therefore by every concurrent query), the same way
    /// [`Self::production_query_owners`] shares its lane owners. Serving a
    /// query never rebuilds the indices; only loading a new generation does.
    pub(in crate::daemon) fn record_index(&self) -> &queries::GenerationRecordIndexV1 {
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
    #[cfg(test)]
    pub(in crate::daemon) fn warm_serving_caches(&self) {
        let _ = self.activate_text_serving();
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

    fn activate_text_serving(&self) -> Result<(), RetrievalPortError> {
        if !self.advance_text_serving(TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1)? {
            return Err(RetrievalPortError::AuthorityUnavailable(
                "code-index text serving owners are warming".to_owned(),
            ));
        }
        let _ = self.record_index();
        let _ = self.generation.test_attribution_authority();
        Ok(())
    }

    /// Whether the record lookup indices are already built for this generation.
    #[cfg(test)]
    fn record_index_is_warm(&self) -> bool {
        self.record_index.get().is_some()
    }

    /// Whether the exact/lexical lane owners are already built.
    #[cfg(test)]
    fn query_owners_are_warm(&self) -> bool {
        self.text_serving_is_ready()
    }

    fn text_serving_is_ready(&self) -> bool {
        self.query_owners.get().is_some()
    }

    fn text_serving_needs_work(&self) -> bool {
        !self.text_serving_is_ready() && !self.text_projection_failed.load(Ordering::Acquire)
    }

    fn mark_text_serving_failed(&self) {
        self.text_projection_failed.store(true, Ordering::Release);
    }

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

    pub(in crate::daemon) fn test_attribution_authority(
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
    pub fn lexical(&self) -> &[tracedecay_domain::CodeSearchChunkV1] {
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

    /// Return exact and lexical query owners bound to the latest complete
    /// published generation.
    #[cfg(test)]
    pub fn production_query_owners(
        &self,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        self.activate_text_serving()?;
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
            CodeGraphActivationStateV1::Pending => Err(RetrievalPortError::Contract(
                "code graph projection has not completed activation".to_owned(),
            )),
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

    /// The retained verified-snapshot projection store for interactive graph
    /// reads, present once persistent graph activation has completed.
    ///
    /// Interactive reads require the persistent Grafeo activation: unlike the
    /// retrieval lanes there is no in-memory fallback, so a cold store is the
    /// typed not-activated state, never an empty serve.
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
        match store.interactive_catalog_is_warm() {
            Ok(true) => Ok(store),
            Ok(false) => Err(RetrievalPortError::Contract(
                "code graph interactive catalog has not completed activation".to_owned(),
            )),
            Err(error) => Err(RetrievalPortError::Contract(error.to_string())),
        }
    }

    fn source_freshness(&self) -> Result<tracedecay_domain::SourceFreshness, RetrievalPortError> {
        production_code_index_freshness(
            self.generation.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )
    }

    fn text_projection_metadata(
        &self,
    ) -> Result<CodeLexicalProjectionMetadataV1, RetrievalPortError> {
        let generation_id = self.generation.manifest().generation_id.clone();
        let freshness = self.source_freshness()?;
        Ok(CodeLexicalProjectionMetadataV1 {
            generation: generation_id,
            repository_id: Some(self.generation.snapshot().repository.clone()),
            logical_paths: self
                .generation
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

    /// Advance at most `maximum_work` bounded page/finalization operations on
    /// this sealed generation's durable text artifact. The mutex is both the
    /// generation-owned partial-state authority and the single-flight gate for
    /// concurrent scheduler wakes.
    fn advance_text_serving(&self, maximum_work: usize) -> Result<bool, RetrievalPortError> {
        let result = self.advance_text_serving_inner(maximum_work);
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
        if self.query_owners.get().is_some() {
            return Ok(true);
        }
        let mut build = self
            .text_projection_build
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.query_owners.get().is_some() {
            return Ok(true);
        }
        self.advance_artifact_text_serving(&mut build, maximum_work)
    }

    /// The durable-artifact journey: reopen a published head when one exists,
    /// otherwise stream the sealed generation through the staging builder one
    /// bounded page window at a time, finalize, publish, and reopen.
    fn advance_artifact_text_serving(
        &self,
        build: &mut Option<CodeTextArtifactBuildV1>,
        maximum_work: usize,
    ) -> Result<bool, RetrievalPortError> {
        let store = &self.text_artifact_store;
        let control = DaemonCodeIndexControlV1::new(
            Arc::clone(&self.text_control_epoch),
            Arc::clone(&self.text_control_shutdown),
        );
        if build.is_none() {
            let generation_id = self.generation.manifest().generation_id.clone();
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
                    &control,
                );
                match reader {
                    Ok(reader) => {
                        self.install_artifact_owners(reader, reader_reservation)?;
                        return Ok(true);
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
            let mut source = store.open_sealed_source(&sealed_identity, &control)?;
            let builder_budget = text_artifact_builder_budget(source.staging_window_bytes())?;
            let metadata = self.text_projection_metadata()?;
            let builder = if staging_path.exists() {
                match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                    &staging_path,
                    metadata.clone(),
                    builder_budget,
                    &control,
                ) {
                    Ok(builder) => Ok(builder),
                    Err(CodeLexicalArtifactErrorV1::Incompatible(_)) => {
                        store.discard_incompatible_staging(&staging_path, &control)?;
                        CodeLexicalArtifactBuilderV1::create_with_memory_budget(
                            &staging_path,
                            metadata,
                            builder_budget,
                        )
                    }
                    Err(error) => Err(error),
                }
            } else {
                CodeLexicalArtifactBuilderV1::create_with_memory_budget(
                    &staging_path,
                    metadata,
                    builder_budget,
                )
            }
            .map_err(map_text_artifact_error)?;
            let progress = builder.progress().map_err(map_text_artifact_error)?;
            if let Some(cursor) = progress.next_cursor.as_ref() {
                source
                    .restore_cursor(cursor, &control)
                    .map_err(map_sealed_page_source_error)?;
            }
            *build = Some(CodeTextArtifactBuildV1 {
                builder,
                source,
                sealed_identity,
                source_receipt: None,
                staging_path,
                _build_reservation: build_reservation,
            });
        }
        let artifact_build = build.as_mut().ok_or_else(|| {
            RetrievalPortError::Contract(
                "code-index text artifact build state is missing".to_owned(),
            )
        })?;
        let mut remaining = maximum_work.min(TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1);
        while remaining > 0 && artifact_build.source_receipt.is_none() {
            let (source, builder) = (&mut artifact_build.source, &mut artifact_build.builder);
            let admitted = source
                .next_page_if(&control, |page| {
                    builder.append_page(page, &control).map(|_| ())
                })
                .map_err(map_sealed_page_source_error)?
                .map_err(map_text_artifact_error)?;
            match admitted {
                VerifiedSealedLexicalPageReadV1::Page(page) => {
                    let _ = page;
                    remaining -= 1;
                }
                VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                    artifact_build.source_receipt = Some(receipt);
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
        let finalized = artifact_build
            .builder
            .advance_finalization(source_receipt, finalization_rows, &control)
            .map_err(map_text_artifact_error)?;
        if !matches!(
            finalized,
            tracedecay_query::retrieval::lexical::CodeLexicalArtifactFinalizationStepV1::Ready(_)
        ) {
            return Ok(false);
        }
        let finished = build.take().ok_or_else(|| {
            RetrievalPortError::Contract(
                "code-index text artifact build state vanished during publication".to_owned(),
            )
        })?;
        let CodeTextArtifactBuildV1 {
            builder,
            source,
            sealed_identity,
            source_receipt: _,
            staging_path,
            _build_reservation: build_reservation,
        } = finished;
        // Close the builder's SQLite connection before content-addressing the
        // finalized staging file.
        drop(builder);
        drop(source);
        let descriptor = store.publish(
            &staging_path,
            self.generation.as_ref(),
            &sealed_identity,
            &control,
        )?;
        let reader_reservation = store.reserve_resident_memory(
            &self.generation.manifest().generation_id,
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
            &control,
        )
        .map_err(map_text_artifact_error)?;
        self.install_artifact_owners(reader, reader_reservation)?;
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
        let lexical = LexicalLane::new(reader);
        let owners = Arc::new(ProductionCodeIndexQueryOwnersV1::artifact(
            exact,
            lexical,
            reader_reservation,
        ));
        let _ = self.query_owners.set(owners);
        let _ = self.record_index();
        let _ = self.generation.test_attribution_authority();
        Ok(())
    }

    fn install_graph_serving(
        &self,
        graph_reader: CodeGraphEvidenceReader,
        store: Option<Arc<CodeGraphProjectionStore>>,
        graph_authority: CodeGraphServingAuthorityV1,
    ) -> Result<(), RetrievalPortError> {
        if graph_reader.generation() != &self.generation.manifest().generation_id {
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
pub(super) enum CodeIndexSchedulerErrorV1 {
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
    pub(super) fn is_retryable_activation(&self) -> bool {
        match self {
            Self::GraphProjection(error) => matches!(
                error,
                CodeGraphProjectionError::Cancelled
                    | CodeGraphProjectionError::BudgetExhausted { .. }
                    | CodeGraphProjectionError::DeadlineExceeded
                    | CodeGraphProjectionError::Conflict
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
            | Self::IgnoredDependency(_) => false,
        }
    }

    pub(super) fn is_graph_activation_refusal(&self) -> bool {
        matches!(self, Self::GraphActivationRefused(_))
            || matches!(
                self,
                Self::GraphProjection(CodeGraphProjectionError::BudgetExhausted { budget, .. })
                    if budget == "resident_memory"
            )
    }
}

/// Counts in-flight owner passes (retained activation or reconcile). A
/// counter rather than a flag so the background worker can hold the state
/// across an entire pass — claim of the pending wake through arrival restore —
/// while the scheduler's own entry points nest inside it without clearing the
/// in-progress signal early.
pub(super) struct ReconcilePassGuard(Arc<AtomicUsize>);

impl ReconcilePassGuard {
    pub(super) fn enter(passes: &Arc<AtomicUsize>) -> Self {
        passes.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(passes))
    }
}

impl Drop for ReconcilePassGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) struct CodeIndexWorktreeSchedulerV1 {
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
    /// Tier-1 cheap staleness signal: `.git` metadata mtimes at last reconcile.
    git_metadata: identity::GitMetadataFingerprintV1,
    /// Tier-2 bounded-staleness clock: when truth was last reconciled.
    last_reconciled_at: Instant,
    /// Wall-clock companion to `last_reconciled_at` for read-model projection.
    /// `None` until the first verified reconcile after open/restore.
    last_reconciled_at_micros: Option<i64>,
    /// Tier-2 cheap prefilter: stat-level (path, mtime, size) signature of the
    /// present source candidates at last reconcile. A quiet repository whose
    /// signature is unchanged resets the staleness clock without paying the
    /// O(repo) read+hash capture.
    last_stat_signature: Option<String>,
    /// Whether `git_metadata` / staleness clocks were established by a completed
    /// reconcile against gix truth. Open/restore alone must not claim freshness.
    verified_against_source: bool,
    /// A restored generation has not yet been reconciled against the current
    /// worktree bytes. It may remain available for rollback/history, but
    /// request admission must fail closed and schedule background truth.
    freshness_unknown: bool,
    byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    /// Keeps the current snapshot's interned bytes alive in the shared pool.
    retained_snapshot_bytes: Vec<Arc<[u8]>>,
    /// Process resident-memory authority artifact builds and readers reserve
    /// through. Standalone opens get a private default-limit authority; the
    /// registry rebinds its shared process authority at mount.
    resident_memory: Arc<ProcessResidentMemoryV1>,
    publication: DaemonCodeIndexPublicationStoreV1,
    production_config: CodeIndexProductionConfigV1,
    owner: ProductionOwner,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    /// Number of in-flight owner passes; nonzero means activation or
    /// reconcile work is running for this worktree.
    reconcile_in_progress: Arc<AtomicUsize>,
    latest_content_identity: Option<ContentDigest>,
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    query_owners: Mutex<Option<GenerationServingCachesV1>>,
    /// Optional semantic hook: schedule `FastEmbed` projection without joining it.
    semantic_schedule:
        Option<tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
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
        let git_metadata = identity::GitMetadataFingerprintV1::default();
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
            chunker_revision: id::<ChunkerRevision>("chunker.daemon.v2")?,
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
        let verified_against_source = false;
        let freshness_unknown = true;
        let last_reconciled_at_micros = None;
        let latest_content_identity = None;
        let hints = Arc::new(Mutex::new(PendingHintsV1::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let epoch = Arc::new(AtomicU64::new(0));
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
            git_metadata,
            last_reconciled_at: Instant::now(),
            last_reconciled_at_micros,
            last_stat_signature: None,
            verified_against_source,
            freshness_unknown,
            byte_pool,
            retained_snapshot_bytes: Vec::new(),
            resident_memory: Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
            publication,
            production_config,
            owner,
            hints,
            wake,
            epoch,
            shutting_down: Arc::new(AtomicBool::new(false)),
            reconcile_in_progress: Arc::new(AtomicUsize::new(0)),
            latest_content_identity,
            ignored_source_admissions: Vec::new(),
            query_owners: Mutex::new(None),
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
    pub(super) fn bind_resident_memory(&mut self, resident_memory: Arc<ProcessResidentMemoryV1>) {
        self.resident_memory = resident_memory;
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
    pub(super) fn schedule_semantic_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> bool {
        let Some(schedule) = self.semantic_schedule.as_ref() else {
            return false;
        };
        match catch_unwind(AssertUnwindSafe(|| schedule(generation))) {
            Ok(scheduled) => scheduled,
            Err(_) => {
                tracing::warn!(
                    event = "code_index_semantic_schedule_panicked",
                    generation = %generation.manifest().generation_id,
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

    pub(super) fn request_background_reconcile(&self) {
        {
            let mut hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hints.overflow();
        }
        // `Notify` already coalesces stored permits. Always refresh the permit:
        // a prior worker may have consumed its wake and then failed before
        // draining this overflow marker.
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
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
        let Some(generation) = self
            .publication
            .load_active_shared()
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        self.validate_generation_identity(&generation)?;
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
        self.mark_reconciled(metadata, Some(stat_signature));
        Ok(Some(CodeIndexReconcileOutcomeV1::Noop(
            CodeIndexNoopEvidenceV1 {
                snapshot_content_identity,
                overflow_reconciled: false,
            },
        )))
    }

    /// Load a complete identity-valid generation for stale serving.
    ///
    /// This does not claim freshness: a cancelled refresh or live ref switch
    /// leaves the worktree ahead of the sealed generation, and that split is a
    /// truthful stale serving state. Worktree identity must still resolve so a
    /// missing Git authority stays unverified rather than a stale answer.
    /// An ignored-source roster is revalidated against the live worktree
    /// before seating: a tracked or retargeted admission must not become
    /// serving, and the scheduler must not keep that roster.
    pub(super) fn servable_retained_generation(&mut self) -> Option<LatestCompleteCodeIndexV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        let resolved = identity::IndexingIdentityV1::resolve(&self.project_root).ok()?;
        if !resolved.authorizes_reuse_of(&self.identity) {
            return None;
        }
        let generation = self.publication.load_active_shared().ok().flatten()?;
        self.validate_generation_identity(&generation).ok()?;
        self.adopt_ignored_source_roster(&generation);
        if !self.ignored_source_roster_matches_generation(&generation) {
            self.ignored_source_admissions.clear();
            return None;
        }
        Some(self.bind_latest_complete(generation))
    }

    /// Retained-owner activation entry point. Foreground reads never call this.
    pub(super) fn activate_or_reconcile(
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

    pub fn reconcile_now(
        &mut self,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
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
        // Sample tier-1 git metadata and the tier-2 stat signature for the state
        // we are reconciling to; stored on return so the next query-admission
        // check compares against them.
        let sampled_metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let sampled_signature = self.worktree_stat_signature().ok();
        let mut overflow_reconciled = false;
        for retry in 0..=MAX_SUPERSEDED_RECONCILE_RETRIES {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            overflow_reconciled |= hints.overflow;
            let mut captured = self.capture_authoritative_snapshot(None)?;
            self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
            let active_generation = self
                .publication
                .load_active_shared()
                .map_err(CodeIndexProductionErrorV1::Publication)?;
            if let Some(generation) = active_generation.as_ref() {
                self.validate_generation_identity(generation)?;
            }
            let latest_snapshot = active_generation
                .as_ref()
                .map(|generation| generation.snapshot().clone());
            let unchanged_source = latest_snapshot.as_ref().is_some_and(|latest| {
                latest.reference == captured.snapshot.reference
                    && latest.source_revision == captured.snapshot.source_revision
            });
            let active_content_identity = latest_snapshot
                .as_ref()
                .map(|snapshot| &snapshot.content_identity);
            if self
                .latest_content_identity
                .as_ref()
                .or(active_content_identity)
                == Some(&captured.snapshot.content_identity)
                && unchanged_source
            {
                self.latest_content_identity = Some(captured.snapshot.content_identity.clone());
                self.mark_reconciled(sampled_metadata, sampled_signature);
                return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity: captured.snapshot.content_identity,
                    overflow_reconciled,
                }));
            }

            let control = DaemonCodeIndexControlV1::new(
                Arc::clone(&self.epoch),
                Arc::clone(&self.shutting_down),
            );
            let changed_files = captured.changed_paths.clone();
            let generation = self.owner.build_and_publish(
                CodeIndexBuildRequestV1 {
                    snapshot: captured.snapshot.clone(),
                    captured_files: captured.captured_files,
                    changed_files,
                    invalidations: BTreeSet::new(),
                    repository_parse_identity: captured.repository_parse_identity,
                    ignored_source_admissions: self.ignored_source_admissions.clone(),
                    sealed_at: now_micros(),
                    target_projection_key: projection_key()?,
                },
                &control,
            );
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
                    self.latest_content_identity = Some(captured.snapshot.content_identity.clone());
                    self.mark_reconciled(sampled_metadata, sampled_signature);
                    return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                        snapshot_content_identity: captured.snapshot.content_identity,
                        overflow_reconciled,
                    }));
                }
                Err(error) => return Err(error.into()),
            };
            self.latest_content_identity = Some(captured.snapshot.content_identity.clone());
            self.mark_reconciled(sampled_metadata.clone(), sampled_signature.clone());

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
                    _lane_digest: lane_digest,
                    _file_occurrence_ids: generation
                        .snapshot()
                        .files
                        .iter()
                        .map(|file| file.file_occurrence_id.clone())
                        .collect(),
                    reextracted_files: captured.changed_paths.len(),
                    changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
                    reused_chunks: changes.reused.len(),
                    overflow_reconciled,
                },
            ));
        }
        unreachable!("the bounded reconciliation loop returns on its final attempt")
    }

    fn mark_reconciled(
        &mut self,
        metadata: identity::GitMetadataFingerprintV1,
        signature: Option<String>,
    ) {
        self.git_metadata = metadata;
        self.last_stat_signature = signature;
        self.freshness_unknown = false;
        self.last_reconciled_at = Instant::now();
        self.last_reconciled_at_micros = Some(now_micros().0);
        self.verified_against_source = true;
        self.persist_freshness_witness();
    }

    /// Record the restore-time freshness witness for the current active
    /// generation. Called at the moment freshness is established (after a
    /// reconcile verified the worktree against gix truth) so a later open of the
    /// same worktree can prove the sealed generation still current without a full
    /// re-read. Requires an active generation AND a captured tier-2 signature;
    /// when either is absent the optimization simply defers to the next
    /// reconcile, and a write failure is non-fatal.
    fn persist_freshness_witness(&self) {
        let Some(stat_signature) = self.last_stat_signature.clone() else {
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
            git_metadata_signature: self.git_metadata.stable_signature(),
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
    /// admission. The freshness checks are identical; only whether an
    /// undecoded active generation is awaited or abstained on differs.
    fn latest_complete_ready_for_query_with(
        &mut self,
        admission: GenerationDecodeAdmissionV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        if self.freshness_unknown
            || identity::GitMetadataFingerprintV1::capture(&self.project_root)
                .differs_from(&self.git_metadata)
        {
            self.request_background_reconcile();
            return Ok(None);
        }
        if self.last_reconciled_at.elapsed() >= self.policy.staleness_threshold {
            self.request_background_reconcile();
            return Ok(None);
        }
        Ok(self.latest_complete_with(admission))
    }

    /// Admit a generation only when the current worktree stat signature still
    /// matches the signature sealed by the last reconcile. Workspace-wide
    /// completeness needs this stronger fence because a file can be added
    /// inside the ordinary bounded-staleness window.
    fn latest_complete_ready_for_exact_source_with(
        &mut self,
        admission: GenerationDecodeAdmissionV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        let latest = self.latest_complete_ready_for_query_with(admission)?;
        if latest.is_none() {
            return Ok(None);
        }
        match self.worktree_stat_signature() {
            Ok(signature) if self.last_stat_signature.as_ref() == Some(&signature) => Ok(latest),
            _ => {
                self.request_background_reconcile();
                Ok(None)
            }
        }
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
        if !self.verified_against_source {
            // Open/restore sampled git metadata without verifying the sealed
            // generation against gix truth. Serving that generation is allowed;
            // suppressing cadence on open-time clocks is not.
            return Ok(Some(self.reconcile_now()?));
        }
        let git_changed = identity::GitMetadataFingerprintV1::capture(&self.project_root)
            .differs_from(&self.git_metadata);
        if git_changed {
            // Tier 1: a git-mediated mutation is authoritative evidence; reconcile.
            return Ok(Some(self.reconcile_now()?));
        }
        if self.last_reconciled_at.elapsed() < self.policy.staleness_threshold {
            return Ok(None);
        }
        // Tier 2: the bounded-staleness window elapsed. Gate the O(repo)
        // read+hash capture behind a cheap stat-level signature so a quiet
        // repository just resets its clock instead of re-reading every file.
        match self.worktree_stat_signature() {
            Ok(signature) if self.last_stat_signature.as_ref() == Some(&signature) => {
                self.last_reconciled_at = Instant::now();
                self.last_reconciled_at_micros = Some(now_micros().0);
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
    pub(super) fn git_authority_available(&self) -> bool {
        gix::open(&self.project_root).is_ok()
    }

    /// [`Self::ensure_fresh_for_query`] with the O(store) rebuild moved off the
    /// request path.
    ///
    /// Runs the identical ladder — unverified restore, tier-1 git metadata,
    /// tier-2 bounded staleness — but where `ensure_fresh_for_query` calls
    /// `reconcile_now()` inline this only *requests* the background worker.
    /// The ladder's checks are cheap (stat-level metadata); its remedy is not,
    /// and a query must never pay for it. This mirrors what
    /// [`Self::latest_complete_ready_for_query_with`] already does for the
    /// latency-sensitive application paths.
    ///
    /// Returns whether a reconcile was actually requested. A quiet repository
    /// must answer `false` and wake nothing: the ladder suppressing work is the
    /// common case, and waking the worker on every read would turn each query
    /// into a rebuild trigger — exactly the coupling this change removes.
    pub(super) fn request_fresh_for_query_background(&mut self) -> bool {
        if !self.verified_against_source
            || identity::GitMetadataFingerprintV1::capture(&self.project_root)
                .differs_from(&self.git_metadata)
            || self.last_reconciled_at.elapsed() >= self.policy.staleness_threshold
        {
            self.request_background_reconcile();
            return true;
        }
        false
    }

    /// The exact identity this scheduler is currently bound to.
    pub fn identity(&self) -> &identity::IndexingIdentityV1 {
        &self.identity
    }

    pub(super) const fn last_reconciled_at_micros(&self) -> Option<i64> {
        self.last_reconciled_at_micros
    }

    pub(super) const fn verified_against_source(&self) -> bool {
        self.verified_against_source
    }

    /// Whether the last execution-owned source observation is older than the
    /// configured freshness window. This only inspects scheduler state; it does
    /// not reopen Git, scan the worktree, enqueue a wake, or mutate a watermark.
    pub(super) fn freshness_window_elapsed(&self) -> bool {
        self.last_reconciled_at.elapsed() >= self.policy.staleness_threshold
    }

    pub(super) fn pending_hint_count(&self) -> Option<u64> {
        let hints = self
            .hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!hints.overflow).then(|| u64::try_from(hints.paths.len()).unwrap_or(u64::MAX))
    }

    #[cfg(test)]
    pub(super) fn pending_hint_paths(&self) -> BTreeSet<PathBuf> {
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
    pub(super) fn latest_complete_already_decoded(&self) -> Option<LatestCompleteCodeIndexV1> {
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
        Some(self.bind_latest_complete(generation))
    }

    /// Bind one decoded generation to this scheduler's per-generation serving
    /// derivations, so every reader of the same generation shares one build.
    fn bind_latest_complete(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> LatestCompleteCodeIndexV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let mut cached = self
            .query_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (
            query_owners,
            record_index,
            text_projection_build,
            text_projection_failed,
            graph_activation,
        ) = match cached.as_ref() {
            Some((cached_id, owners, index, build, failed, interactive))
                if cached_id == &generation_id =>
            {
                (
                    Arc::clone(owners),
                    Arc::clone(index),
                    Arc::clone(build),
                    Arc::clone(failed),
                    Arc::clone(interactive),
                )
            }
            _ => {
                let owners = Arc::new(OnceLock::new());
                let index = Arc::new(OnceLock::new());
                let build = Arc::new(Mutex::new(None));
                let failed = Arc::new(AtomicBool::new(false));
                let graph_activation = Arc::new(RwLock::new(CodeGraphActivationStateV1::Pending));
                *cached = Some((
                    generation_id,
                    Arc::clone(&owners),
                    Arc::clone(&index),
                    Arc::clone(&build),
                    Arc::clone(&failed),
                    Arc::clone(&graph_activation),
                ));
                (owners, index, build, failed, graph_activation)
            }
        };
        LatestCompleteCodeIndexV1 {
            generation,
            query_owners,
            record_index,
            text_projection_build,
            text_projection_failed,
            text_artifact_store: DaemonCodeTextArtifactStoreV1::bind(
                &self.store_root,
                &self.publication,
                &self.resident_memory,
                &self.project_id,
                &self.worktree_id,
            ),
            text_control_epoch: Arc::clone(&self.epoch),
            text_control_shutdown: Arc::clone(&self.shutting_down),
            graph_activation,
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
    #[cfg(test)]
    fn sealed_decode_count(&self) -> u64 {
        self.publication.sealed_decode_count()
    }

    /// Occupy this worktree's active-generation decode barrier, reproducing the
    /// window in which a new generation is being decoded/activated.
    #[cfg(test)]
    pub(super) fn hold_active_decode(&self) -> HeldActiveDecodeV1 {
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
                    .map(|generation| LatestCompleteCodeIndexV1 {
                        generation,
                        query_owners: Arc::new(OnceLock::new()),
                        record_index: Arc::new(OnceLock::new()),
                        text_projection_build: Arc::new(Mutex::new(None)),
                        text_projection_failed: Arc::new(AtomicBool::new(false)),
                        text_artifact_store: DaemonCodeTextArtifactStoreV1::bind(
                            &self.store_root,
                            &self.publication,
                            &self.resident_memory,
                            &self.project_id,
                            &self.worktree_id,
                        ),
                        text_control_epoch: Arc::clone(&self.epoch),
                        text_control_shutdown: Arc::clone(&self.shutting_down),
                        graph_activation: Arc::new(RwLock::new(
                            CodeGraphActivationStateV1::Pending,
                        )),
                    })
            })
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }

    pub(super) fn reconcile_in_progress(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.reconcile_in_progress)
    }

    pub(super) fn active_generation_encoded_bytes(&self) -> Arc<AtomicU64> {
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
        self.capture_candidate_bytes(registry, logical_path, &raw_bytes)
    }

    fn capture_authoritative_snapshot(
        &self,
        control: Option<&dyn CodeIndexExecutionControlV1>,
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
        let classification = classification::WorktreeChangeClassificationV1::classify(&repository)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        if self.ignored_source_admissions.is_empty()
            && classification.changes().is_empty()
            && let (Some(reference), Some(revision), Some(tree)) = (
                self.identity.head_ref(),
                self.identity.head_commit(),
                self.identity.head_tree(),
            )
        {
            return self
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
                    _ => CodeIndexSchedulerErrorV1::Git(format!(
                        "immutable HEAD-tree capture failed: {}",
                        reason.as_str()
                    )),
                });
        }
        let source_revision = (self.ignored_source_admissions.is_empty()
            && classification.changes().is_empty())
        .then(|| self.identity.head_commit().cloned())
        .flatten();
        let mut candidate_paths = classification.candidate_paths();
        let mut changed_paths = classification.changed_paths();
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
        // Read + sanitize + digest is per-file pure work over independent
        // paths, so it fans out across the reserved-width indexing pool. The
        // candidate set is an ordered `BTreeSet`; results are collected in
        // that same order and the lowest-index failure is the reported one,
        // so the captured snapshot is byte-identical to the sequential sweep.
        let candidates = candidate_paths.into_iter().collect::<Vec<_>>();
        let outcomes = crate::code_index::parallelism::install(|| {
            use rayon::prelude::*;
            candidates
                .par_iter()
                .map(|logical_path| self.capture_candidate(&registry, logical_path, control))
                .collect::<Vec<_>>()
        });

        let mut files = Vec::new();
        let mut captured_files = Vec::new();
        let mut sanitization_receipts = BTreeSet::new();
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
        Ok(CapturedSnapshotV1 {
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
        })
    }
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

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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
    Ok((hex::encode(hasher.finalize()), file_metadata.len()))
}

fn ensure_private_text_artifacts_root(path: &Path) -> Result<(), RetrievalPortError> {
    match tracedecay_private_fs::create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_private_directory(path).map_err(|error| {
                RetrievalPortError::Contract(format!(
                    "code text artifacts root is not owner-private: {error}"
                ))
            })
        }
        Err(error) => Err(text_artifact_unavailable(error)),
    }
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
mod ignored_dependencies_tests;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod overlay_ephemerality_tests;
#[cfg(test)]
mod tests;

mod activation;
pub(super) mod branch_generations;
mod cadence;
mod classification;
mod freshness_witness;
mod git_tree_capture;
mod graph_activation;
pub(crate) mod identity;
mod ignored_dependencies;
pub(in crate::daemon) mod observability;
mod privacy;
pub(in crate::daemon) mod queries;
pub(in crate::daemon) mod query_runtime;
mod registry;
pub(crate) mod semantic_query_runtime;
pub(crate) mod semantic_vector_graph;

// The registry surface lives in `registry.rs`; re-export it so its public path
// (`code_index_scheduler::CodeIndexSchedulerRegistryV1`) and method signatures
// stay stable for the daemon and MCP server that mount and query worktrees.
pub(in crate::daemon) use activation::{
    CodeIndexActivationHintSinkV1, CodeIndexActivationMountV1, CodeIndexActivationV1,
};
#[cfg(test)]
pub(crate) use cadence::CodeIndexCadenceReadModelV1;
pub(crate) use cadence::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, newly_eligible_percentile,
};
pub(in crate::daemon) use graph_activation::CodeGraphActivationPolicyV1;
pub(crate) use graph_activation::CodeGraphReplayBindingV1;
pub(in crate::daemon) use ignored_dependencies::{
    CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexIgnoredDependencyRefusalV1,
    CodeIndexIgnoredDependencyRequestV1,
};
pub(crate) use registry::CodeIndexSchedulerRegistryV1;
pub(in crate::daemon) use registry::watch_ingress::GitStateChangeRequestV1;
pub(in crate::daemon) use registry::{
    ServingGenerationInstallationOutcomeV1, ServingGenerationRollbackOutcomeV1,
};
pub(crate) type CodeIndexGenerationPublishedV1 = registry::CodeIndexGenerationPublishedV1;
