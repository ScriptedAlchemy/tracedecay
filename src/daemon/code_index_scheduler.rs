//! Daemon-owned scheduling and reconciliation for production code generations.
//!
//! Hook events are bounded wake-up hints only. Every run reconstructs its
//! source snapshot from gix's HEAD-tree/index/worktree status before content
//! digests decide whether publication is necessary.
#![allow(dead_code)] // Plan 25 code-intelligence indexing — reconciliation surface staged

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use thiserror::Error;
use tracedecay_application::{DirectorySyncPolicy, now_micros};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, ContentDigest, FileOccurrenceId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    WorktreeId, canonical_sha256,
};
#[cfg(test)]
use tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryReclaimerRegistrationV1, ResidentMemoryReservationV1,
};

use crate::{
    application::code_index::{
        DaemonCodeIndexControlV1, ProductionCodeIndexOwnerV1, open_production_code_index_owner_v1,
    },
    code_index::{
        chunks::{ExtractionAdmittedCodeSearchChunkV1, content_digest},
        languages::{LanguageRegistry, StaticLanguageRegistry},
        production::{
            CodeIndexActiveSlotV1, CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1,
            CodeIndexCapturedFileV1, CodeIndexInputErrorV1, CodeIndexProductionConfigV1,
            CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
            CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
            SharedPhysicalCodeArtifactPoolV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1,
            build_batch_receipt,
        },
    },
    privacy::CODE_SOURCE_SANITIZER_VERSION_V1,
    retention::code_index_generations::{
        DurablePublicationPointerV1, acquire_code_generation_store_lock,
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

#[derive(Clone)]
struct DaemonCodeIndexPublicationStoreV1 {
    cache: Arc<DecodedGenerationCacheV1>,
    resident_memory: Arc<ProcessResidentMemoryV1>,
    project_id: ProjectId,
    worktree_id: WorktreeId,
    active_encoded_bytes: Arc<AtomicU64>,
    sealed_read_attempts: Arc<AtomicU64>,
    _reclaimer: Arc<ResidentMemoryReclaimerRegistrationV1>,
    active_path: PathBuf,
    generations_root: PathBuf,
    expected_sanitizer_revision: SanitizerRevision,
}

impl DaemonCodeIndexPublicationStoreV1 {
    #[cfg(test)]
    fn new(
        store_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        Self::new_with_resident_memory(
            store_root,
            expected_sanitizer_revision,
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
            id::<ProjectId>("project.code-index-publication-test")?,
            id::<WorktreeId>("worktree.code-index-publication-test")?,
        )
    }

    fn new_with_resident_memory(
        store_root: &Path,
        expected_sanitizer_revision: SanitizerRevision,
        resident_memory: Arc<ProcessResidentMemoryV1>,
        project_id: ProjectId,
        worktree_id: WorktreeId,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let generations_root = store_root.join("code-generations-v1");
        std::fs::create_dir_all(&generations_root)?;
        let cache = Arc::new(DecodedGenerationCacheV1::default());
        let weak_cache = Arc::downgrade(&cache);
        let reclaimer = resident_memory
            .register_reclaimer(
                100,
                Arc::new(move |_| {
                    if let Some(cache) = weak_cache.upgrade() {
                        cache.reclaim_inactive();
                    }
                }),
            )
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        Ok(Self {
            cache,
            resident_memory,
            project_id,
            worktree_id,
            active_encoded_bytes: Arc::new(AtomicU64::new(0)),
            sealed_read_attempts: Arc::new(AtomicU64::new(0)),
            _reclaimer: Arc::new(reclaimer),
            active_path: store_root.join("active-code-generation-v1.json"),
            generations_root,
            expected_sanitizer_revision,
        })
    }

    fn unavailable(error: impl std::fmt::Display) -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string())
    }

    fn reserve_authenticated_generation(
        &self,
        generation_id: CodeGenerationId,
        sealed_bytes: u64,
    ) -> Result<ResidentMemoryReservationV1, CodeIndexPublicationStoreErrorV1> {
        let upper_bound =
            CodeIndexPublishedGenerationV1::sealed_resident_memory_upper_bound(sealed_bytes)
                .map_err(Self::unavailable)?;
        self.reserve_generation_component(
            generation_id,
            "code_index.canonical_generation.v1",
            upper_bound,
        )
    }

    fn reserve_generation_component(
        &self,
        generation_id: CodeGenerationId,
        component: &'static str,
        requested_bytes: u64,
    ) -> Result<ResidentMemoryReservationV1, CodeIndexPublicationStoreErrorV1> {
        let requested_bytes = NonZeroU64::new(requested_bytes)
            .ok_or_else(|| Self::unavailable("generation resident estimate is zero"))?;
        let component = ResidentMemoryComponentIdV1::new(component).map_err(Self::unavailable)?;
        let key = ResidentMemoryKeyV1 {
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
            generation_id,
            component,
        };
        let reservation =
            self.resident_memory
                .reserve(key, requested_bytes)
                .map_err(
                    |failure| CodeIndexPublicationStoreErrorV1::ResidentMemoryUnavailable {
                        used_bytes: failure.used_bytes,
                        requested_bytes: failure.requested_bytes,
                        limit_bytes: failure.limit_bytes,
                    },
                )?;
        Ok(reservation)
    }

    fn finish_resident_generation(
        &self,
        generation: CodeIndexPublishedGenerationV1,
        sealed_bytes: u64,
        reservation: ResidentMemoryReservationV1,
    ) -> Result<Arc<ResidentPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(Arc::new(ResidentPublishedGenerationV1 {
            generation: Arc::new(generation),
            resident_memory: Arc::clone(&self.resident_memory),
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
            sealed_bytes,
            _reservation: reservation,
        }))
    }

    #[cfg(test)]
    fn decode_authenticated_generation(
        &self,
        bytes: &[u8],
    ) -> Result<Arc<ResidentPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let estimate = CodeIndexPublishedGenerationV1::sealed_resident_memory_estimate(bytes)
            .map_err(Self::unavailable)?;
        let sealed_bytes = u64::try_from(bytes.len())
            .map_err(|_| Self::unavailable("sealed generation byte length exceeds u64"))?;
        let reservation =
            self.reserve_authenticated_generation(estimate.generation_id().clone(), sealed_bytes)?;
        self.cache.note_decode();
        let generation =
            CodeIndexPublishedGenerationV1::decode_sealed(bytes).map_err(Self::unavailable)?;
        if generation.manifest().generation_id != *estimate.generation_id() {
            return Err(Self::unavailable(
                "sealed generation resident probe identity does not match decoded identity",
            ));
        }
        self.finish_resident_generation(generation, sealed_bytes, reservation)
    }

    fn decode_generation_file(
        &self,
        path: &Path,
        expected_generation_id: &CodeGenerationId,
        expected_state_digest: &str,
    ) -> Result<Arc<ResidentPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let metadata = path.symlink_metadata().map_err(Self::unavailable)?;
        if !metadata.file_type().is_file() {
            return Err(Self::unavailable(
                "sealed code-generation path is not a regular file",
            ));
        }
        let sealed_bytes = metadata.len();
        let reservation =
            self.reserve_authenticated_generation(expected_generation_id.clone(), sealed_bytes)?;
        self.sealed_read_attempts.fetch_add(1, Ordering::Relaxed);
        let file = std::fs::File::open(path).map_err(Self::unavailable)?;
        let mut reader = BufReader::with_capacity(64 * 1024, DigestingReaderV1::new(file));
        self.cache.note_decode();
        let generation = CodeIndexPublishedGenerationV1::decode_sealed_reader(&mut reader)
            .map_err(Self::unavailable)?;
        let digesting = reader.into_inner();
        if digesting.bytes_read != sealed_bytes {
            return Err(Self::unavailable(
                "sealed code-generation changed while it was decoded",
            ));
        }
        if digesting.state_digest() != expected_state_digest {
            return Err(Self::unavailable(
                "sealed code-generation bytes do not match their authenticated digest",
            ));
        }
        self.finish_resident_generation(generation, sealed_bytes, reservation)
    }

    fn sync_directory(path: &Path) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict)
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

    /// Serve one sealed generation by identity, decoding it at most once.
    ///
    /// The active generation answers from its pinned slot. Any other generation
    /// is served from the decoded LRU, or decoded exactly once under a lease —
    /// concurrent pinned or cursor-paged readers of the same generation join the
    /// in-flight decode instead of each rescanning the store.
    fn load_generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<Arc<ResidentPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
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
        let matched = self.scan_sealed_generations(generation_id);
        if let Ok(Some(generation)) = matched.as_ref() {
            self.cache.remember(Arc::clone(generation))?;
        }
        drop(lease);
        matched
    }

    /// Read every sealed generation file until the requested identity matches.
    ///
    /// Fail-closed: filename/content digest mismatch, duplicate identity claims,
    /// and decode failures all error instead of serving.
    fn scan_sealed_generations(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<Arc<ResidentPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let mut paths = std::fs::read_dir(&self.generations_root)
            .map_err(Self::unavailable)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        let mut matched = None;
        for path in paths {
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(encoded_digest) = file_name
                .strip_prefix("generation-")
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if encoded_digest.len() != 64
                || !encoded_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                || !path
                    .symlink_metadata()
                    .map_err(Self::unavailable)?
                    .file_type()
                    .is_file()
            {
                continue;
            }
            let generation = self.decode_generation_file(
                &path,
                generation_id,
                &format!("sha256:{encoded_digest}"),
            )?;
            if generation.manifest().generation_id != *generation_id {
                continue;
            }
            if matched.replace(generation).is_some() {
                return Err(Self::unavailable(
                    "multiple immutable code-generation files claim one generation identity",
                ));
            }
        }
        Ok(matched)
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
    ) -> Result<Option<Arc<ResidentPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
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
    ) -> Result<Option<Arc<ResidentPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
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
    ) -> Result<Option<Arc<ResidentPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let pointer_bytes = match std::fs::read(&self.active_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Self::unavailable(error)),
        };
        let pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&pointer_bytes).map_err(|error| {
                Self::unavailable(format!(
                    "active code-generation pointer is corrupt: {error}"
                ))
            })?;
        Self::validate_generation_file(&pointer.generation_file)?;
        let expected_generation_id =
            CodeGenerationId::new(pointer.generation_id.clone()).map_err(Self::unavailable)?;
        let generation_path = self.generations_root.join(&pointer.generation_file);
        let generation = self.decode_generation_file(
            &generation_path,
            &expected_generation_id,
            &pointer.state_digest,
        )?;
        if generation.manifest().sanitizer_revision != self.expected_sanitizer_revision {
            return Ok(None);
        }
        if generation.manifest().generation_id.as_str() != pointer.generation_id
            || generation.snapshot().content_identity.as_str() != pointer.snapshot_content_identity
            || generation.projection().publication_digest().as_str() != pointer.publication_digest
            || generation.manifest().seal.sealed_at.0 != pointer.sealed_at_micros
        {
            return Err(Self::unavailable(
                "active code-generation pointer does not match the sealed generation",
            ));
        }
        let encoded_bytes = generation_path
            .symlink_metadata()
            .map_err(Self::unavailable)?
            .len();
        self.active_encoded_bytes
            .store(encoded_bytes, Ordering::Release);
        Ok(Some(generation))
    }

    fn active_encoded_bytes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.active_encoded_bytes)
    }

    /// Sealed-bytes decodes this process has performed against this store.
    #[cfg(test)]
    fn sealed_decode_count(&self) -> u64 {
        self.cache.decode_count()
    }

    #[cfg(test)]
    fn sealed_read_attempt_count(&self) -> u64 {
        self.sealed_read_attempts.load(Ordering::Relaxed)
    }
}

impl CodeIndexAtomicPublicationPort for DaemonCodeIndexPublicationStoreV1 {
    fn load_active(
        &self,
        _scope: &CodeIndexActiveSlotV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .load_active_shared()?
            .map(|generation| generation.generation.as_ref().clone()))
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexActiveSlotV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let generation_id = generation.manifest().generation_id.clone();
        // Admit the encode and retained canonical value before serialization
        // allocates its sealed buffer. Opaque allocator overhead is deliberately
        // overcharged and never shrunk.
        let structural_bytes = generation
            .structural_resident_memory_bytes(0)
            .map_err(Self::unavailable)?;
        let pre_encode_upper_bound = structural_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(8 * 1024 * 1024))
            .ok_or_else(|| Self::unavailable("publication resident upper bound exceeds u64"))?;
        let encode_probe_reservation = self.reserve_generation_component(
            generation_id.clone(),
            "code_index.canonical_encode_probe.v1",
            pre_encode_upper_bound,
        )?;
        let encoded_len = generation.encoded_sealed_len().map_err(Self::unavailable)?;
        let reservation =
            self.reserve_authenticated_generation(generation_id.clone(), encoded_len)?;
        let generation_bytes = generation.encode_sealed().map_err(Self::unavailable)?;
        let sealed_bytes = u64::try_from(generation_bytes.len())
            .map_err(|_| Self::unavailable("sealed generation byte length exceeds u64"))?;
        if sealed_bytes != encoded_len {
            return Err(Self::unavailable(
                "sealed generation count and encoded length disagree",
            ));
        }
        let generation = self.finish_resident_generation(generation, sealed_bytes, reservation)?;
        drop(encode_probe_reservation);
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

        let pointer = DurablePublicationPointerV1 {
            generation_id: generation.manifest().generation_id.as_str().to_owned(),
            snapshot_content_identity: generation.snapshot().content_identity.as_str().to_owned(),
            publication_digest: generation
                .projection()
                .publication_digest()
                .as_str()
                .to_owned(),
            sealed_at_micros: generation.manifest().seal.sealed_at.0,
            generation_file,
            state_digest,
        };
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
        self.active_encoded_bytes.store(
            u64::try_from(generation_bytes.len()).unwrap_or(u64::MAX),
            Ordering::Release,
        );
        // The published generation is already decoded and validated in memory,
        // so activation costs nothing: no request ever re-reads these bytes.
        // Bumping the epoch retires any decode that started against the prior
        // pointer so it cannot install itself over this newer generation.
        state.active_epoch = state.active_epoch.wrapping_add(1);
        state.forget(&generation_id);
        state.active = Some(generation);
        Ok(())
    }
}

#[derive(Default)]
struct DaemonProjectionSinkV1;

impl CodeChunkProjectionSink for DaemonProjectionSinkV1 {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
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
        build_batch_receipt(&request, &decisions)
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
    pub lane_digest: ManifestDigest,
    pub file_occurrence_ids: Vec<FileOccurrenceId>,
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

#[derive(Clone)]
pub(in crate::daemon) struct LatestCompleteCodeIndexV1 {
    generation: Arc<ResidentPublishedGenerationV1>,
    serving: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwners>>>,
    /// Single-flight gate for the O(store) lane-owner build. Without it every
    /// query that raced the activation warm rebuilt the full lexical/exact
    /// projection inline — N concurrent cold queries did N store-sized builds,
    /// each blowing its own dispatch deadline while the warm was still running.
    query_owners_build_gate: Arc<Mutex<()>>,
    warm_control: Arc<ServingWarmControl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SemanticEvaluationCodeSnapshotV1 {
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub snapshot_digest: ManifestDigest,
}

impl LatestCompleteCodeIndexV1 {
    pub(in crate::daemon) fn generation(&self) -> &CodeIndexPublishedGenerationV1 {
        self.generation.generation.as_ref()
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
        }
    }

    /// The sealed generation identity as a display string. Used by the dashboard
    /// code-index freshness read port and Doctor code-index mapping.
    pub(in crate::daemon) fn generation_id_string(&self) -> String {
        self.generation.manifest().generation_id.as_str().to_owned()
    }

    /// The sealed-at watermark (microseconds since the Unix epoch) of this
    /// generation. Serves as the last-reconcile watermark the dashboard reports.
    pub(in crate::daemon) fn sealed_at_micros(&self) -> i64 {
        self.generation.manifest().seal.sealed_at.0
    }

    pub(in crate::daemon) fn snapshot_digest(&self) -> &tracedecay_domain::ManifestDigest {
        &self.generation.manifest().snapshot_digest
    }

    pub(in crate::daemon) fn test_attribution_authority(
        &self,
    ) -> Result<
        crate::code_index::production::PublishedGenerationTestAttributionAuthorityV1,
        crate::code_index::production::CodeIndexProductionErrorV1,
    > {
        self.generation.test_attribution_authority()
    }

    pub fn exact(
        &self,
    ) -> Result<
        Arc<Vec<ExtractionAdmittedCodeSearchChunkV1>>,
        crate::code_index::chunks::ChunkingFailureV1,
    > {
        self.generation.admitted_chunks()
    }

    pub fn lexical(&self) -> &[tracedecay_domain::CodeSearchChunkV1] {
        self.generation.chunks().chunks()
    }

    pub fn graph_edges(&self) -> &[tracedecay_domain::CanonicalRelationEdgeV1] {
        self.generation.edges()
    }

    pub fn graph_abstentions(&self) -> &[crate::code_index::chunks::CodeIndexEdgeAbstentionV1] {
        self.generation.edge_abstentions()
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
    #[error("code-index serving lanes failed: {0}")]
    Serving(String),
    /// Mount admission did not free a permit within its deadline. The store is
    /// healthy and the decode is merely queued behind other worktrees, so this
    /// is retryable — unlike every other variant here.
    #[error("code-index mount admission is warming: no permit within {waited_ms}ms; retry")]
    MountAdmissionWarming { waited_ms: u64 },
}

impl CodeIndexSchedulerErrorV1 {
    /// Whether retrying the same mount against this daemon can succeed.
    pub(super) fn is_retryable(&self) -> bool {
        matches!(self, Self::MountAdmissionWarming { .. })
    }
}

struct AtomicFlagReset(Arc<AtomicBool>);

impl Drop for AtomicFlagReset {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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
    retained_snapshot_reservation: Option<ResidentMemoryReservationV1>,
    publication: DaemonCodeIndexPublicationStoreV1,
    owner: ProductionOwner,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    reconcile_in_progress: Arc<AtomicBool>,
    latest_content_identity: Option<ContentDigest>,
    query_owners: Arc<Mutex<BTreeMap<CodeGenerationId, Arc<GenerationServingCaches>>>>,
    _serving_reclaimer: Arc<ResidentMemoryReclaimerRegistrationV1>,
    /// Optional semantic hook: schedule `FastEmbed` projection without joining it.
    semantic_schedule:
        Option<crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
}

impl CodeIndexWorktreeSchedulerV1 {
    #[cfg(test)]
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

    pub(in crate::daemon) fn open_with_resident_memory(
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        Self::open_with_policy_and_resident_memory(
            project_id,
            project_root,
            store_root,
            byte_pool,
            CodeIndexHintPolicyV1::default(),
            resident_memory,
        )
    }

    #[cfg(test)]
    pub fn open_with_policy(
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
        policy: CodeIndexHintPolicyV1,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        Self::open_with_policy_and_resident_memory(
            project_id,
            project_root,
            store_root,
            byte_pool,
            policy,
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
        )
    }

    fn open_with_policy_and_resident_memory(
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
        policy: CodeIndexHintPolicyV1,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        // Resolve exact identity BEFORE any indexing work. Paths located this
        // checkout; identity authorizes what may be reused.
        let identity = identity::IndexingIdentityV1::resolve(&project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let repository_id = identity.repository_id().clone();
        let worktree_id = identity.worktree_id().clone();
        let git_metadata = identity::GitMetadataFingerprintV1::capture(&project_root);
        let sanitizer_revision = id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)?;
        let publication = DaemonCodeIndexPublicationStoreV1::new_with_resident_memory(
            &store_root,
            sanitizer_revision.clone(),
            resident_memory,
            project_id.clone(),
            worktree_id.clone(),
        )?;
        let owner = open_production_code_index_owner_v1(
            CodeIndexProductionConfigV1 {
                project_id: project_id.clone(),
                repository: repository_id.clone(),
                sanitizer_revision,
                policy_revision: id::<PolicyRevisionId>("policy.daemon.v1")?,
                chunker_revision: id::<ChunkerRevision>("chunker.daemon.v1")?,
                privacy_domain: id::<PrivacyDomainId>("privacy.local-code-index")?,
                privacy_key_epoch: 1,
                max_snapshot_age_micros: None,
            },
            publication.clone(),
            DaemonProjectionSinkV1,
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::ProductionOpen(error.to_string()))?
        .with_physical_artifact_pool(byte_pool.physical_artifacts.clone());
        let restored = publication
            .load_active_shared()
            .map_err(CodeIndexProductionErrorV1::Publication)?;
        // Identity backstop: a restored generation may only be adopted when it
        // was produced under this exact repository AND worktree. A matching path
        // or branch label is never sufficient; cross-worktree reuse is refused.
        if let Some(generation) = &restored {
            let snapshot = generation.snapshot();
            let same_project = generation.manifest().project_id == project_id;
            let same_worktree = snapshot.worktree.as_ref() == Some(&worktree_id);
            let same_repository = snapshot.repository == repository_id;
            if !same_project || !same_worktree || !same_repository {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "active code generation belongs to a different project/worktree identity"
                        .to_owned(),
                ));
            }
        }
        // Restore-time freshness witness (P2). A durable witness records the
        // tier-1 git-metadata and tier-2 stat signatures the restored generation
        // was reconciled against. When BOTH still match the current on-disk
        // source, the sealed generation provably equals the working tree, so the
        // scheduler may adopt it as verified and skip the forced cold reconcile
        // (a whole-repo read+sanitize+hash over every file). Any mismatch, a
        // generation-id mismatch, or an absent/corrupt witness keeps the
        // conservative unverified state and the worker performs a full reconcile,
        // so the witness only ever SKIPS redundant work and never serves stale.
        let restore_verified_stat = restored.as_ref().and_then(|generation| {
            let witness = RestoreFreshnessWitnessV1::load(&store_root)?;
            if witness.generation_id != generation.manifest().generation_id.as_str() {
                return None;
            }
            if witness.git_metadata_signature != git_metadata.stable_signature() {
                return None;
            }
            let current_stat = worktree_stat_signature_for(&project_root).ok()?;
            (witness.stat_signature == current_stat).then_some(current_stat)
        });
        let verified_against_source = restore_verified_stat.is_some();
        let freshness_unknown = restored.is_some() && !verified_against_source;
        let last_reconciled_at_micros = verified_against_source.then(|| now_micros().0);
        let latest_content_identity = restored
            .as_ref()
            .map(|generation| generation.snapshot().content_identity.clone());
        let hints = Arc::new(Mutex::new(PendingHintsV1::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let epoch = Arc::new(AtomicU64::new(0));
        // Restoring a sealed generation authorizes serve-prior-generation, not a
        // freshness claim. Cadence must verify against gix before tier-1/tier-2
        // clocks may suppress reconciliation, EXCEPT when the restore-time
        // witness above already proved the generation current.
        let query_owners = Arc::new(Mutex::new(BTreeMap::<
            CodeGenerationId,
            Arc<GenerationServingCaches>,
        >::new()));
        let weak_query_owners = Arc::downgrade(&query_owners);
        let serving_reclaimer = publication
            .resident_memory
            .register_reclaimer(
                90,
                Arc::new(move |_| {
                    let Some(query_owners) = weak_query_owners.upgrade() else {
                        return;
                    };
                    let mut query_owners =
                        query_owners.lock().unwrap_or_else(PoisonError::into_inner);
                    for caches in query_owners.values() {
                        caches.cancel();
                    }
                    query_owners.clear();
                }),
            )
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
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
            last_stat_signature: restore_verified_stat,
            verified_against_source,
            freshness_unknown,
            byte_pool,
            retained_snapshot_bytes: Vec::new(),
            retained_snapshot_reservation: None,
            publication,
            owner,
            hints,
            wake,
            epoch,
            shutting_down: Arc::new(AtomicBool::new(false)),
            reconcile_in_progress: Arc::new(AtomicBool::new(false)),
            latest_content_identity,
            query_owners,
            _serving_reclaimer: Arc::new(serving_reclaimer),
            semantic_schedule: None,
        };
        Ok(scheduler)
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    /// Replace the semantic `schedule_generation` hook on mount/remount. The hook
    /// must return immediately; `FastEmbed` download/indexing never blocks
    /// exact/lexical/graph search. `None` retires a stale runtime.
    pub fn replace_semantic_schedule_hook(
        &mut self,
        hook: Option<crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
    ) {
        self.semantic_schedule = hook;
    }

    pub fn notify_path(&self, path: PathBuf) {
        self.hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path(path);
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

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

    pub fn reconcile_now(
        &mut self,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        self.reconcile_in_progress.store(true, Ordering::Release);
        let _reconcile_guard = AtomicFlagReset(Arc::clone(&self.reconcile_in_progress));
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
            let mut captured = self.capture_authoritative_snapshot()?;
            self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
            self.retained_snapshot_reservation = Some(captured.reservation);
            let latest_snapshot = self
                .publication
                .load_active_shared()
                .map_err(CodeIndexProductionErrorV1::Publication)?
                .map(|generation| generation.snapshot().clone());
            let unchanged_source = latest_snapshot.as_ref().is_some_and(|latest| {
                latest.reference == captured.snapshot.reference
                    && latest.source_revision == captured.snapshot.source_revision
            });
            if self.latest_content_identity.as_ref() == Some(&captured.snapshot.content_identity)
                && unchanged_source
            {
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
            let published_generation = self
                .publication
                .load_active_shared()
                .map_err(CodeIndexProductionErrorV1::Publication)?
                .ok_or_else(|| {
                    CodeIndexSchedulerErrorV1::Identity(
                        "published code generation is absent from the publication cache".to_owned(),
                    )
                })?;
            if published_generation.manifest().generation_id != generation.manifest().generation_id
            {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "published code generation cache does not match the completed build".to_owned(),
                ));
            }
            drop(generation);
            let generation = published_generation;
            self.latest_content_identity = Some(captured.snapshot.content_identity.clone());
            self.mark_reconciled(sampled_metadata.clone(), sampled_signature.clone());

            // SEMANTIC: enqueue FastEmbed projection without waiting on download/index.
            if let Some(schedule) = &self.semantic_schedule {
                let _scheduled = schedule(&generation);
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
        let Ok(Some(latest)) = self.try_latest_complete() else {
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
        };
        witness.persist(&self.store_root);
    }

    /// Admit only already-current immutable evidence. Expensive truth capture
    /// and generation publication belong to the background worker; a request
    /// that detects stale or unproven state schedules that worker and abstains.
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
        self.latest_complete_with(admission)
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
        worktree_stat_signature_for(&self.project_root)
    }

    /// Deliver debounced hook hints (exact touched paths) into the incremental
    /// queue. Hints only narrow work; gix status remains the truth on reconcile.
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

    pub fn try_latest_complete(
        &self,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        self.latest_complete_with(GenerationDecodeAdmissionV1::AwaitDecode)
    }

    #[cfg(test)]
    pub fn latest_complete(&self) -> Option<LatestCompleteCodeIndexV1> {
        match self.try_latest_complete() {
            Ok(latest) => latest,
            Err(error) => panic!("latest code generation restore failed: {error}"),
        }
    }

    /// [`Self::try_latest_complete`] restricted to an already-decoded active
    /// generation. Abstains instead of parking on the single-flight decode.
    pub(super) fn latest_complete_already_decoded(
        &self,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        self.latest_complete_with(GenerationDecodeAdmissionV1::AlreadyDecoded)
    }

    fn latest_complete_with(
        &self,
        admission: GenerationDecodeAdmissionV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        let generation = match admission {
            GenerationDecodeAdmissionV1::AwaitDecode => self.publication.load_active_shared(),
            GenerationDecodeAdmissionV1::AlreadyDecoded => {
                self.publication.active_already_decoded()
            }
        }
        .map_err(CodeIndexProductionErrorV1::Publication)?;
        Ok(generation.map(|generation| self.bind_latest_complete(generation)))
    }

    /// Bind one decoded generation to this scheduler's per-generation serving
    /// derivations, so every reader of the same generation shares one build.
    fn bind_latest_complete(
        &self,
        generation: Arc<ResidentPublishedGenerationV1>,
    ) -> LatestCompleteCodeIndexV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let mut cached = self
            .query_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let caches = cached
            .entry(generation_id.clone())
            .or_insert_with(|| Arc::new(GenerationServingCaches::new()))
            .clone();
        while cached.len() > DECODED_GENERATION_CACHE_CAPACITY + 1 {
            let evicted = cached
                .keys()
                .find(|candidate| **candidate != generation_id)
                .cloned();
            let Some(evicted) = evicted else {
                break;
            };
            if let Some(caches) = cached.remove(&evicted) {
                caches.cancel();
            }
        }
        let (serving, build_gate, control) = (
            Arc::clone(&caches.serving),
            Arc::clone(&caches.build_gate),
            Arc::clone(&caches.control),
        );
        drop(cached);
        LatestCompleteCodeIndexV1 {
            generation,
            serving,
            query_owners_build_gate: build_gate,
            warm_control: control,
        }
    }

    fn retire_serving_caches(&mut self) {
        let mut cached = self
            .query_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for cached in cached.values() {
            cached.cancel();
        }
        cached.clear();
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
    fn hold_active_decode(&self) -> HeldActiveDecodeV1 {
        self.publication.hold_active_decode()
    }

    fn generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSchedulerErrorV1> {
        self.publication
            .load_generation(generation_id)
            .map(|generation| generation.map(|generation| self.bind_latest_complete(generation)))
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }

    pub(super) fn reconcile_in_progress(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.reconcile_in_progress)
    }

    pub(super) fn active_generation_encoded_bytes(&self) -> Arc<AtomicU64> {
        self.publication.active_encoded_bytes()
    }

    /// Read, sanitize, intern and identify one candidate path.
    ///
    /// `Ok(None)` means the path is not an indexable source file (vanished,
    /// no extension, or no language descriptor) — the sequential loop's
    /// `continue` arms. Pure with respect to the shared byte pool: the pool
    /// is content-addressed under its own lock, so concurrent interning
    /// yields the same digests and the same shared buffers.
    fn capture_candidate(
        &self,
        registry: &StaticLanguageRegistry,
        logical_path: &str,
    ) -> Result<Option<CapturedCandidateV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let absolute = self.project_root.join(logical_path);
        if !absolute.is_file() {
            return Ok(None);
        }
        let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
            return Ok(None);
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            return Ok(None);
        };
        let raw_bytes = std::fs::read(&absolute)?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let (sanitized_bytes, sensitivity_level, receipt_id) =
            privacy::sanitize_code_file(&raw_bytes)?;
        let (digest, shared) = self.byte_pool.intern(sanitized_bytes);
        let occurrence = file_occurrence_id(
            &self.repository_id,
            &self.worktree_id,
            logical_path,
            &digest,
            &receipt_id,
        )?;
        Ok(Some(CapturedCandidateV1 {
            file: SanitizedCodeFileV1 {
                file_occurrence_id: occurrence.clone(),
                logical_path: logical_path.to_owned(),
                language: Some(descriptor.language.clone()),
                content_digest: digest,
                disposition: SnapshotFileDispositionV1::Present,
            },
            captured: CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence,
                sanitized_bytes: shared.to_vec(),
                sensitivity_level,
            },
            receipt_id,
            retained: shared,
        }))
    }

    fn capture_authoritative_snapshot(
        &self,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
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
        let candidate_paths = classification.candidate_paths();
        let changed_paths = classification.changed_paths();
        let dirty = if classification
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
        let source_bytes = candidates.iter().try_fold(0_u64, |total, logical_path| {
            let path = self.project_root.join(logical_path);
            let bytes = match path.metadata() {
                Ok(metadata) if metadata.is_file() => metadata.len(),
                Ok(_) => 0,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(CodeIndexSchedulerErrorV1::Io(error)),
            };
            total.checked_add(bytes).ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "code-index capture byte estimate exceeds u64".to_owned(),
                )
            })
        })?;
        let candidate_count = u64::try_from(candidates.len()).map_err(|_| {
            CodeIndexSchedulerErrorV1::Identity(
                "code-index capture candidate count exceeds u64".to_owned(),
            )
        })?;
        let requested_bytes = source_bytes
            .checked_mul(6)
            .and_then(|bytes| {
                candidate_count
                    .checked_mul(64 * 1024)
                    .and_then(|entries| bytes.checked_add(entries))
            })
            .and_then(|bytes| bytes.checked_add(8 * 1024 * 1024))
            .and_then(NonZeroU64::new)
            .ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "code-index capture resident estimate exceeds u64".to_owned(),
                )
            })?;
        let reservation = self
            .publication
            .resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id: id::<CodeGenerationId>("generation.capture.v1")?,
                    component: ResidentMemoryComponentIdV1::new(
                        "code_index.capture_working_set.v1",
                    )
                    .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?,
                },
                requested_bytes,
            )
            .map_err(|failure| {
                CodeIndexProductionErrorV1::Publication(
                    CodeIndexPublicationStoreErrorV1::ResidentMemoryUnavailable {
                        used_bytes: failure.used_bytes,
                        requested_bytes: failure.requested_bytes,
                        limit_bytes: failure.limit_bytes,
                    },
                )
            })?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let outcomes = crate::code_index::parallelism::install(|| {
            use rayon::prelude::*;
            candidates
                .par_iter()
                .map(|logical_path| self.capture_candidate(&registry, logical_path))
                .collect::<Vec<_>>()
        });

        let mut files = Vec::new();
        let mut captured_files = Vec::new();
        let mut sanitization_receipts = BTreeSet::new();
        for outcome in outcomes {
            let Some(candidate) = outcome? else {
                continue;
            };
            sanitization_receipts.insert(candidate.receipt_id);
            retained_bytes.push(candidate.retained);
            files.push(candidate.file);
            captured_files.push(candidate.captured);
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
                source_revision: self.identity.head_commit().cloned(),
                sanitizer_revision: id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)?,
                sanitization_receipts,
                content_identity,
                captured_at: now_micros(),
                files,
            },
            captured_files,
            changed_paths,
            retained_bytes,
            reservation,
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
        self.retire_serving_caches();
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
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// A cheap stat-level (path, mtime, size) signature over the present language
/// source candidates. Opens gix and runs stat-based status (no byte reads, no
/// content hashing), so it can gate the far more expensive read+hash capture
/// when nothing has actually changed on disk. Shared by the query-admission
/// tier-2 prefilter and the restore-time freshness witness.
fn worktree_stat_signature_for(project_root: &Path) -> Result<String, CodeIndexSchedulerErrorV1> {
    let repository = gix::open(project_root)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let classification = classification::WorktreeChangeClassificationV1::classify(&repository)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let registry = StaticLanguageRegistry::new();
    let mut buf = Vec::new();
    for logical_path in classification.candidate_paths() {
        let absolute = project_root.join(&logical_path);
        let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if registry
            .descriptor_for_extension(&extension.to_lowercase())
            .is_none()
        {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&absolute) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0u128, |elapsed| elapsed.as_nanos());
        buf.extend_from_slice(logical_path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&metadata.len().to_le_bytes());
        buf.extend_from_slice(&mtime_nanos.to_le_bytes());
        buf.push(0xff);
    }
    Ok(format!("sha256:{}", sha256_hex(&buf)))
}

/// File name of the restore-time freshness witness inside the scoped store root.
const FRESHNESS_WITNESS_FILE_NAME: &str = "freshness_witness.v1";

/// A durable record of the tier-1 git-metadata + tier-2 stat signatures that a
/// specific sealed generation was reconciled against.
///
/// On restore this lets the scheduler PROVE, without re-reading and re-hashing
/// the whole worktree, that the on-disk source still equals the sealed
/// generation: the witness is bound to `generation_id`, and both signatures are
/// recomputed and compared. A match means no git-mediated change (tier-1) and
/// no working-tree change under the standard (path, mtime, size) content proxy
/// (tier-2) has occurred since seal — the same soundness bar the steady-state
/// tier-2 query-admission suppression already relies on. Any mismatch, a
/// generation-id mismatch, or an absent/corrupt witness falls through to a full
/// reconcile, so the witness can only ever SKIP redundant work, never serve a
/// stale index.
struct RestoreFreshnessWitnessV1 {
    generation_id: String,
    git_metadata_signature: String,
    stat_signature: String,
}

impl RestoreFreshnessWitnessV1 {
    fn witness_path(store_root: &Path) -> PathBuf {
        store_root.join(FRESHNESS_WITNESS_FILE_NAME)
    }

    /// Encode as three newline-delimited fields. Deliberately trivial and
    /// versioned by file name so a format change is a new witness file (and the
    /// old one simply fails to parse, forcing a safe full reconcile).
    fn encode(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.generation_id, self.git_metadata_signature, self.stat_signature
        )
    }

    fn decode(contents: &str) -> Option<Self> {
        let mut lines = contents.lines();
        let generation_id = lines.next()?.to_owned();
        let git_metadata_signature = lines.next()?.to_owned();
        let stat_signature = lines.next()?.to_owned();
        if generation_id.is_empty()
            || git_metadata_signature.is_empty()
            || stat_signature.is_empty()
        {
            return None;
        }
        Some(Self {
            generation_id,
            git_metadata_signature,
            stat_signature,
        })
    }

    fn load(store_root: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(Self::witness_path(store_root)).ok()?;
        Self::decode(&contents)
    }

    /// Persist atomically via a temp file + rename so a concurrent restore never
    /// observes a torn witness. A write failure is non-fatal: the next reconcile
    /// simply rewrites it, and its absence only costs a full reconcile.
    fn persist(&self, store_root: &Path) {
        let path = Self::witness_path(store_root);
        let temp = store_root.join(format!("{FRESHNESS_WITNESS_FILE_NAME}.tmp"));
        if std::fs::write(&temp, self.encode()).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
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
mod cadence;
mod classification;
pub(crate) mod identity;
mod privacy;
pub(in crate::daemon) mod queries;
pub(in crate::daemon) mod query_runtime;
mod record_index;
mod registry;
mod resident_generation;
pub(crate) mod semantic_query_runtime;
mod serving_memory;

use resident_generation::*;
use serving_memory::*;

// The registry surface lives in `registry.rs`; re-export it so its public path
// (`code_index_scheduler::CodeIndexSchedulerRegistryV1`) and method signatures
// stay stable for the daemon and MCP server that mount and query worktrees.
pub(in crate::daemon) use activation::{
    CodeIndexActivationHintSinkV1, CodeIndexActivationMountV1, CodeIndexActivationV1,
};
pub(crate) use cadence::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceReadModelV1,
    CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1,
    newly_eligible_percentile,
};
pub(crate) use registry::CodeIndexSchedulerRegistryV1;
pub(crate) type CodeIndexGenerationPublishedV1 = registry::CodeIndexGenerationPublishedV1;
pub(crate) type CodeIndexSchedulerMemoryStatsV1 = registry::CodeIndexSchedulerMemoryStatsV1;
