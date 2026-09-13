//! Serving handles for the latest complete generation: query owners, the
//! durable lexical text artifact, and graph activation state.
use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use same_file::Handle;
use sha2::{Digest, Sha256};
use tracedecay_application::code_index::DaemonCodeIndexControlV1;
use tracedecay_code_index_retention::code_index_generations::{
    DurableCodeTextArtifactDescriptorV1, DurablePublicationPointerV1,
    DurableSealedCodeGenerationIdentityV1, acquire_code_generation_store_lock,
    attach_verified_text_artifact_under_lock, code_text_artifact_path, code_text_artifacts_root,
    withdraw_verified_text_artifact_under_lock,
};
use tracedecay_contracts::{
    code_index_freshness::{
        CodeIndexBuildBlockedReasonV1, CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1,
    },
    now_micros,
};
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationSourceCommitmentsV1, ComponentRevision,
    ExactAdmissionRuleRevision, ManifestDigest, ProjectId, RetrievalBudget, RetrieverBatch,
    RetrieverOutcome, ScoreDomainId, WorktreeId, canonical_text::encode_lowercase_hex,
    sha256_hex_suffix,
};
use tracedecay_private_fs::{
    make_private_directory, open_private_file, validate_private_directory,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryReservationV1, sampled_process_resident_bytes_v1,
};

use crate::{
    code_index::{
        graph_projection::{CodeGraphEvidenceReader, CodeGraphProjectionStore},
        production::{
            CodeIndexExecutionControlV1, CodeIndexProductionErrorV1,
            CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
            SealedGenerationSegmentReadV1, VerifiedSealedLexicalCursorRestoreErrorV1,
            VerifiedSealedLexicalPageBatchBoundsV1, VerifiedSealedLexicalPageBatchReadV1,
            VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalSourceReceiptV1,
            VerifiedSealedTextGenerationMetadataV1,
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
            LexicalLaneRetriever, code_lexical_artifact_build_memory_budget_for,
        },
        ports::RetrievalPortError,
    },
};

use super::{DaemonCodeIndexPublicationStoreV1, ProfiledStdMutex, queries};

/// Page bounds for streaming one sealed generation into the durable lexical
/// text artifact. One page is one bounded unit of background build progress.
pub(super) const TEXT_ARTIFACT_PAGE_CHUNKS_V1: usize = 128;
const TEXT_ARTIFACT_PAGE_BYTES_V1: usize = 4 * 1024 * 1024;
const TEXT_ARTIFACT_BASE_BATCH_PAGES_V1: usize = 64;
const TEXT_ARTIFACT_BASE_BATCH_BYTES_V1: usize = 64 * 1024 * 1024;
const TEXT_ARTIFACT_MAXIMUM_BATCH_SCALE_V1: usize = 8;
/// One synchronous activation advances only this many page/finalization
/// operations. Larger caller hints are clamped so work accounting cannot
/// overflow and every expensive loop retains cancellation checkpoints. The
/// runtime narrows this ceiling to two host-sized batches, preserving 128
/// operations at the 1.5 GiB floor.
pub(super) const TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1: usize =
    2 * TEXT_ARTIFACT_BASE_BATCH_PAGES_V1 * TEXT_ARTIFACT_MAXIMUM_BATCH_SCALE_V1;

pub(super) fn text_artifact_source_batch_limits(
    build_memory_budget: usize,
) -> (usize, usize, usize) {
    let scale = (build_memory_budget / CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1)
        .clamp(1, TEXT_ARTIFACT_MAXIMUM_BATCH_SCALE_V1);
    let pages = TEXT_ARTIFACT_BASE_BATCH_PAGES_V1 * scale;
    let bytes = TEXT_ARTIFACT_BASE_BATCH_BYTES_V1 * scale;
    (pages, bytes, pages * 2)
}
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
const TEXT_ARTIFACT_MAXIMUM_ACTIVATION_ADVANCES_V1: usize = 10_000;
/// Rows digested by one scheduler finalization operation. The builder persists
/// its exact section/row cursor after this bounded slice, avoiding both a
/// corpus-sized wake and one scheduler wake per individual `SQLite` row.
const TEXT_ARTIFACT_FINALIZATION_ROWS_PER_OPERATION_V1: usize = 4 * 1024;

/// The lazily built serving caches shared by every handle bound to one sealed
/// generation: the exact/lexical/graph lane owners, the record lookup index,
/// and the retained interactive graph store. All are rebuilt only when a new
/// generation is loaded.
pub(super) type GenerationServingCachesV1 = (
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
pub(super) struct GenerationTextControlV1 {
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
    pub(super) fn new(shutting_down: Arc<AtomicBool>) -> Self {
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

    pub(super) fn retire(&self) {
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

struct GenerationTextRequestControlV1<'a> {
    generation: &'a GenerationTextControlV1,
    request: &'a dyn CodeIndexExecutionControlV1,
}

impl CodeIndexExecutionControlV1 for GenerationTextRequestControlV1<'_> {
    fn is_cancelled(&self) -> bool {
        self.generation.is_cancelled() || self.request.is_cancelled()
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.generation.is_deadline_exceeded() || self.request.is_deadline_exceeded()
    }
}

#[derive(Default)]
pub struct CodeIndexBuildProgressSlotStateV1 {
    generation_id: Option<CodeGenerationId>,
    pub(super) owner_epoch: u64,
    progress_epoch: u64,
    snapshot: Option<Arc<CodeIndexBuildProgressV1>>,
}

impl CodeIndexBuildProgressSlotStateV1 {
    pub(super) fn replace_generation(&mut self, generation_id: CodeGenerationId) -> u64 {
        self.owner_epoch = self.owner_epoch.saturating_add(1).max(1);
        self.progress_epoch = self.progress_epoch.saturating_add(1).max(1);
        self.generation_id = Some(generation_id);
        self.snapshot = None;
        self.owner_epoch
    }

    pub(super) fn publish(
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
pub(super) fn try_publish_build_progress(
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
pub(super) struct CodeIndexCommittedProgressSampleV1 {
    pub(super) observed_at: Instant,
    pub(super) completed_files: u64,
    pub(super) completed_lexical_units: u64,
}

pub(super) struct CodeIndexBuildProgressStateV1 {
    started_at: Instant,
    committed_samples: VecDeque<CodeIndexCommittedProgressSampleV1>,
}

impl CodeIndexBuildProgressStateV1 {
    pub(super) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            committed_samples: VecDeque::with_capacity(2),
        }
    }

    pub(super) fn observe_committed(&mut self, sample: CodeIndexCommittedProgressSampleV1) {
        if self.committed_samples.back().is_some_and(|previous| {
            previous.completed_files == sample.completed_files
                && previous.completed_lexical_units == sample.completed_lexical_units
        }) {
            return;
        }
        if self.committed_samples.len() == 2 {
            self.committed_samples.pop_front();
        }
        self.committed_samples.push_back(sample);
    }

    pub(super) fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    pub(super) fn rates_and_eta(
        &self,
        total_lexical_units: u64,
    ) -> (Option<f64>, Option<f64>, Option<u64>) {
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
        let lexical_units_per_second = current
            .completed_lexical_units
            .checked_sub(previous.completed_lexical_units)
            .filter(|delta| *delta > 0)
            .map(|delta| delta as f64 / elapsed_seconds);
        let estimated_remaining_seconds = lexical_units_per_second.and_then(|lexical_rate| {
            let remaining = total_lexical_units.saturating_sub(current.completed_lexical_units);
            let estimate = (remaining as f64 / lexical_rate).ceil();
            (estimate.is_finite() && estimate >= 0.0 && estimate <= u64::MAX as f64)
                .then_some(estimate as u64)
        });
        (
            files_per_second,
            lexical_units_per_second,
            estimated_remaining_seconds,
        )
    }
}

#[derive(Clone)]
pub struct LatestCompleteCodeIndexV1 {
    pub(super) generation: Arc<CodeIndexPublishedGenerationV1>,
    pub(super) text: LatestCodeTextGenerationV1,
    pub(super) record_index: Arc<OnceLock<queries::GenerationRecordIndexV1>>,
}

#[derive(Clone)]
pub struct LatestCodeTextGenerationV1 {
    pub(super) metadata: Arc<VerifiedSealedTextGenerationMetadataV1>,
    pub(super) sealed_format_revision: u32,
    pub(super) query_owners: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    /// Native-graph readiness for this exact sealed text generation. Status
    /// reads this authority even while an older generation still owns the
    /// graph-serving slot, so old Ready state cannot mask current Pending or
    /// terminal Unavailable state.
    pub(super) graph_activation: Arc<RwLock<CodeGraphActivationStateV1>>,
    /// Generation-owned singleflight state for the durable text projection:
    /// the resumable partial build plus the head-open claim. Only the
    /// background scheduler advances it; foreground queries observe typed
    /// warming until the immutable owners are installed.
    pub(super) text_projection_build: Arc<CodeTextProjectionStateV1>,
    pub(super) text_projection_failed: Arc<AtomicBool>,
    pub(super) text_control: GenerationTextControlV1,
    pub(super) text_progress_state: Arc<ProfiledStdMutex<CodeIndexBuildProgressStateV1>>,
    pub(super) text_progress_slot: CodeIndexBuildProgressSlotV1,
    pub(super) text_progress_owner_epoch: u64,
    /// Durable daemon-authority epoch, shared by all scheduler owners created
    /// during one daemon invocation.
    pub(super) text_progress_daemon_incarnation: u64,
    /// Registry-minted scheduler-owner epoch. It orders progress across
    /// scheduler owner replacements within one daemon incarnation.
    pub(super) text_progress_producer_incarnation: u64,
    /// The durable text-artifact store for this generation's store root.
    pub(super) text_artifact_store: DaemonCodeTextArtifactStoreV1,
    /// A cold graph-off bind authenticates the sealed source once and hands
    /// that same reader to the artifact build. The full generation is never
    /// decoded merely to discover text metadata or source layout.
    pub(super) preopened_source:
        Arc<ProfiledStdMutex<Option<VerifiedSealedLexicalPageSourceV1<File>>>>,
    pub(super) publication_binding: Option<Arc<DurableActiveSealedGenerationBindingV1>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DurableActiveSealedGenerationBindingV1 {
    pub(super) generation_id: CodeGenerationId,
    pub(super) generation_file: String,
    pub(super) state_digest: ManifestDigest,
}

impl DurableActiveSealedGenerationBindingV1 {
    pub(super) fn matches(&self, pointer: Option<&DurablePublicationPointerV1>) -> bool {
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

    pub(super) fn occurrence_by_binding(
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

    pub(super) fn occurrence_by_chunk(
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
pub(super) struct CodeTextArtifactBuildV1 {
    pub(super) builder: CodeLexicalArtifactBuilderV1,
    pub(super) source: VerifiedSealedLexicalPageSourceV1<File>,
    sealed_identity: DurableSealedCodeGenerationIdentityV1,
    source_receipt: Option<VerifiedSealedLexicalSourceReceiptV1>,
    pub(super) staging_path: PathBuf,
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
pub(super) struct CodeTextProjectionStateV1 {
    slot: Mutex<CodeTextProjectionSlotV1>,
    ready: Condvar,
}

pub(super) enum CodeTextProjectionSlotV1 {
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
    pub(super) fn new() -> Self {
        Self {
            slot: Mutex::new(CodeTextProjectionSlotV1::Idle),
            ready: Condvar::new(),
        }
    }

    pub(super) fn lock_slot(&self) -> MutexGuard<'_, CodeTextProjectionSlotV1> {
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
pub(super) enum TextHeadOpenOutcomeV1 {
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

pub(super) fn map_sealed_page_source_error(
    error: CodeIndexProductionErrorV1,
) -> RetrievalPortError {
    match error {
        CodeIndexProductionErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
        ) => RetrievalPortError::Cancelled,
        CodeIndexProductionErrorV1::Interrupted(
            crate::code_index::production::CodeIndexInterruptionV1::DeadlineExceeded,
        ) => RetrievalPortError::BudgetExceeded,
        CodeIndexProductionErrorV1::Contract(detail)
        | CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(detail),
        ) => RetrievalPortError::Contract(detail),
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

pub(super) fn text_artifact_resident_memory_charges(
    requested: NonZeroU64,
    unmodeled_live_bytes: u64,
    watermark_headroom: u64,
) -> Result<(NonZeroU64, NonZeroU64), RetrievalPortError> {
    let retained = requested
        .get()
        .checked_add(unmodeled_live_bytes)
        .and_then(NonZeroU64::new)
        .ok_or_else(|| {
            RetrievalPortError::Contract(
                "text-artifact resident-memory accounting overflowed".to_owned(),
            )
        })?;
    // Headroom makes the reserve call enforce the lower admission watermark,
    // but it is not memory owned by this artifact. Retaining it in every
    // overlapping build charges the same process-wide margin repeatedly.
    let accounted = retained
        .get()
        .checked_add(watermark_headroom)
        .and_then(NonZeroU64::new)
        .ok_or_else(|| {
            RetrievalPortError::Contract(
                "text-artifact resident-memory accounting overflowed".to_owned(),
            )
        })?;
    Ok((accounted, retained))
}

impl DaemonCodeTextArtifactStoreV1 {
    pub(super) fn bind(
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

    /// Reserve one artifact memory ceiling plus the freshly observed process
    /// live set not already represented by reservations for this admission.
    /// The atomic reserve also includes the process-wide high-watermark
    /// headroom, then releases that check-only margin before returning while
    /// the component ceiling and unmodeled live baseline remain charged.
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
        let snapshot = self.resident_memory.snapshot();
        let observed_bytes = sampled_process_resident_bytes_v1().map_or(0, |observed| {
            self.resident_memory
                .pressure()
                .publish_observed_resident_bytes(observed)
                .observed_bytes()
                .unwrap_or(observed)
        });
        let unmodeled_live_bytes = observed_bytes.saturating_sub(snapshot.used_bytes);
        let admission_watermark = self
            .resident_memory
            .pressure()
            .high_watermark_bytes()
            .min(snapshot.limit_bytes);
        let watermark_headroom = snapshot.limit_bytes.saturating_sub(admission_watermark);
        let (accounted, retained) = text_artifact_resident_memory_charges(
            requested,
            unmodeled_live_bytes,
            watermark_headroom,
        )?;
        hotpath::gauge!("query.artifact.admission.observed_resident_bytes")
            .set(observed_bytes as f64);
        hotpath::gauge!("query.artifact.admission.unmodeled_live_bytes")
            .set(unmodeled_live_bytes as f64);
        hotpath::gauge!("query.artifact.admission.requested_growth_bytes")
            .set(requested.get() as f64);
        hotpath::gauge!("query.artifact.admission.accounted_bytes").set(accounted.get() as f64);
        hotpath::gauge!("query.artifact.admission.retained_bytes").set(retained.get() as f64);
        let mut reservation = self
            .resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.project_id.clone(),
                    worktree_id: self.worktree_id.clone(),
                    generation_id: generation_id.clone(),
                    component,
                },
                accounted,
            )
            .map_err(|_| RetrievalPortError::BudgetExceeded)?;
        reservation.shrink_to(retained.get()).map_err(|error| {
            RetrievalPortError::Contract(format!(
                "text-artifact resident-memory headroom release failed: {error}"
            ))
        })?;
        Ok(reservation)
    }

    /// The durably attached artifact descriptor for one retained generation,
    /// or `None` when the generation has no published text artifact yet.
    pub(super) fn published_descriptor(
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
    pub(super) fn sealed_identity(
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
    pub(super) fn open_sealed_source(
        &self,
        identity: &DurableSealedCodeGenerationIdentityV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedSealedLexicalPageSourceV1<File>, RetrievalPortError> {
        self.open_sealed_source_with_progress(identity, control, |_, _| {})
    }

    pub(super) fn open_sealed_source_with_progress<F>(
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
        checkpoint_text_artifact_control(control)?;
        progress(0, identity.size_bytes);
        let manifest_bytes = std::fs::read(&path).map_err(text_artifact_unavailable)?;
        checkpoint_text_artifact_control(control)?;
        if DaemonCodeIndexPublicationStoreV1::state_digest(&manifest_bytes)
            != identity.digest.as_str()
        {
            return Err(RetrievalPortError::Contract(
                "partitioned sealed lexical manifest digest does not verify".to_owned(),
            ));
        }
        progress(identity.size_bytes, identity.size_bytes);
        let manifest = File::open(path).map_err(text_artifact_unavailable)?;
        let publication = self.publication.clone();
        let source_identity = identity.clone();
        VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
            manifest,
            &manifest_bytes,
            identity.digest.clone(),
            move |digest, expected_size, buffer| {
                publication.read_retained_partitioned_segment(
                    &source_identity,
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
        .map_err(map_sealed_page_source_error)?
        .ok_or_else(|| {
            RetrievalPortError::Contract(
                "partitioned sealed lexical source is incompatible".to_owned(),
            )
        })
    }

    /// Durably publish one finalized staging artifact: content-address it,
    /// move it into the artifacts root, fsync the directory, and attach the
    /// descriptor to the sealed generation entry under the store lock.
    pub(super) fn publish(
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
            let (artifact_sha256, artifact_size_bytes) = hotpath::measure_block!(
                "query.artifact.store.state_digest",
                sha256_private_file_and_size(staging_path, control)
            )?;
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("query.artifact.store.digest_bytes").set(artifact_size_bytes);
            let descriptor = DurableCodeTextArtifactDescriptorV1 {
                generation_id: generation_id.clone(),
                artifact_file: format!(
                    "text-artifact-{}.bin",
                    encode_lowercase_hex(&artifact_sha256)
                ),
                artifact_digest: ManifestDigest::from_sha256_bytes(&artifact_sha256)
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
                    let (existing_sha256, existing_size_bytes) = hotpath::measure_block!(
                        "query.artifact.store.dedupe_compare",
                        sha256_private_file_and_size(&final_path, control)
                    )?;
                    if existing_size_bytes != artifact_size_bytes {
                        return Err(RetrievalPortError::Contract(
                            "existing code text artifact does not match its content address"
                                .to_owned(),
                        ));
                    }
                    if existing_sha256 != artifact_sha256 {
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

pub(super) struct ProductionCodeGraphServingV1 {
    pub graph: GraphLane<CodeGraphEvidenceReader>,
    store: Option<Arc<CodeGraphProjectionStore>>,
    _graph_authority: CodeGraphServingAuthorityV1,
}

pub(super) enum CodeGraphActivationStateV1 {
    Pending,
    Refused(&'static str),
    Unavailable(String),
    Ready(Arc<ProductionCodeGraphServingV1>),
}

#[derive(Clone)]
pub(super) enum CodeGraphServingAuthorityV1 {
    Persistent {
        /// Retained solely to keep the canonical graph store lease alive for
        /// the lifetime of the serving owners; never read.
        _lease:
            Arc<tracedecay_runtime_core::shard_runtime::registry::CanonicalCodeGraphStoreLeaseV1>,
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

    pub fn source_commitments(
        &self,
    ) -> Result<&CodeGenerationSourceCommitmentsV1, CodeIndexProductionErrorV1> {
        self.metadata.source_commitments()
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
    pub(super) fn record_index_is_warm(&self) -> bool {
        self.record_index.get().is_some()
    }
}

impl LatestCodeTextGenerationV1 {
    /// Whether the exact/lexical lane owners are already built.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn query_owners_are_warm(&self) -> bool {
        self.text_serving_is_ready()
    }

    pub(super) fn text_serving_is_ready(&self) -> bool {
        self.query_owners.get().is_some()
    }

    pub(super) fn text_serving_needs_work(&self) -> bool {
        !self.text_serving_is_ready() && !self.text_projection_failed.load(Ordering::Acquire)
    }

    pub(super) fn same_text_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.text_projection_build, &other.text_projection_build)
    }

    pub(super) fn mark_text_serving_failed(&self) {
        self.text_projection_failed.store(true, Ordering::Release);
    }
}

impl LatestCompleteCodeIndexV1 {
    pub(super) fn semantic_evaluation_snapshot(&self) -> SemanticEvaluationCodeSnapshotV1 {
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

    /// Finish the durable text projection for an explicitly selected sealed
    /// generation while retaining both generation and request cancellation.
    /// Historical branch generations have no scheduler-owned background wake,
    /// so their first query owns this resumable completion.
    pub(super) fn finish_text_serving_for_request(
        &self,
        request_control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, RetrievalPortError> {
        let mut advances = 0_usize;
        while !self.advance_text_serving_for_request(
            TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1,
            request_control,
        )? {
            advances += 1;
            if advances >= TEXT_ARTIFACT_MAXIMUM_ACTIVATION_ADVANCES_V1 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn production_query_owners_with_budget(
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
    pub(super) fn production_graph_serving(
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
    ) -> tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1 {
        self.text.code_graph_serving_readiness()
    }

    pub(super) fn refuse_graph_activation(&self, reason: &'static str) {
        self.text.refuse_graph_activation(reason);
    }

    pub(super) fn mark_graph_activation_unavailable(&self, reason: String) {
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
    ) -> tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1 {
        match &*self
            .graph_activation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            CodeGraphActivationStateV1::Pending => {
                tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Pending
            }
            CodeGraphActivationStateV1::Refused(reason) => {
                tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Refused {
                    reason: (*reason).to_owned(),
                }
            }
            CodeGraphActivationStateV1::Unavailable(reason) => {
                tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Unavailable {
                    reason: reason.clone(),
                }
            }
            CodeGraphActivationStateV1::Ready(_) => {
                tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready
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
    pub(super) fn source_freshness(
        &self,
    ) -> Result<tracedecay_domain::SourceFreshness, RetrievalPortError> {
        production_code_index_freshness(
            self.metadata.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )
    }

    pub(super) fn text_projection_metadata(
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
        match build.source_receipt.as_ref() {
            // A completed source mints one terminal read that emits no record
            // and only normalizes the exhausted file position, so its live
            // cursor sits one file rollover beyond the last durably accepted
            // page whenever that page filled exactly at a file's last record.
            // The completion receipt is the accepted-source authority from
            // here on — the same one the builder seals the artifact against —
            // and it binds the source state digest, the page count, every
            // emitted counter, and both digest chains.
            Some(receipt) => receipt
                .verify_completion(progress.next_cursor.as_ref())
                .map_err(map_sealed_page_source_error)?,
            None => match progress.next_cursor.as_ref() {
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
            },
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
        let completed_lexical_units = build
            .source
            .completed_lexical_units()
            .map_err(map_sealed_page_source_error)?;
        let total_lexical_units = build.source.total_lexical_units();
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
                completed_lexical_units,
            });
            #[cfg(feature = "hotpath")]
            {
                hotpath::gauge!("query.artifact.progress.committed_pages")
                    .set(progress.next_page_ordinal);
                hotpath::gauge!("query.artifact.progress.committed_lexical_units")
                    .set(completed_lexical_units);
            }
        }
        let (files_per_second, lexical_units_per_second, estimated_remaining_seconds) =
            state.rates_and_eta(total_lexical_units);
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
            completed_lexical_units,
            total_lexical_units,
            current_batch_pages,
            current_batch_payload_bytes,
            elapsed_micros: state.elapsed_micros(),
            last_commit_latency_micros,
            files_per_second,
            lexical_units_per_second,
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
            completed_lexical_units: source.total_lexical_units(),
            total_lexical_units: source.total_lexical_units(),
            current_batch_pages: 0,
            current_batch_payload_bytes: 0,
            elapsed_micros,
            last_commit_latency_micros: None,
            files_per_second: None,
            lexical_units_per_second: None,
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

    pub(super) fn publish_text_progress_phase(
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
    pub(super) fn advance_text_serving(
        &self,
        maximum_work: usize,
    ) -> Result<bool, RetrievalPortError> {
        let control = self.text_execution_control();
        self.advance_text_serving_with_control(maximum_work, &control)
    }

    fn advance_text_serving_for_request(
        &self,
        maximum_work: usize,
        request_control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, RetrievalPortError> {
        let generation = self.text_execution_control();
        let control = GenerationTextRequestControlV1 {
            generation: &generation,
            request: request_control,
        };
        self.advance_text_serving_with_control(maximum_work, &control)
    }

    fn advance_text_serving_with_control(
        &self,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, RetrievalPortError> {
        let result = self.advance_text_serving_inner(maximum_work, control);
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

    fn advance_text_serving_inner(
        &self,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, RetrievalPortError> {
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
        self.advance_artifact_text_serving(maximum_work, control)
    }

    pub(super) fn text_execution_control(&self) -> GenerationTextControlV1 {
        self.text_control.clone()
    }

    pub(super) fn take_preopened_source_or_open(
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
    pub(super) fn open_published_head_or_begin_build(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<TextHeadOpenOutcomeV1, RetrievalPortError> {
        let store = &self.text_artifact_store;
        let build_memory_budget = code_lexical_artifact_build_memory_budget_for(
            store.resident_memory.snapshot().limit_bytes,
        );
        let (source_batch_pages, source_batch_bytes, _) =
            text_artifact_source_batch_limits(build_memory_budget);
        hotpath::gauge!("query.artifact.build_memory_budget_bytes").set(build_memory_budget);
        hotpath::gauge!("query.artifact.source_batch_pages_max").set(source_batch_pages);
        hotpath::gauge!("query.artifact.source_batch_bytes_max").set(source_batch_bytes);
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
            )
            .and_then(|reader| {
                let expected_metadata = self
                    .text_projection_metadata()
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
                if reader.metadata() != &expected_metadata {
                    return Err(CodeLexicalArtifactErrorV1::Incompatible(
                        "published lexical metadata does not match the current projection"
                            .to_owned(),
                    ));
                }
                Ok(reader)
            });
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
            build_memory_budget,
        )?;
        let sealed_identity = store.sealed_identity(&generation_id)?;
        let sealed_hex = sha256_hex_suffix(sealed_identity.digest.as_str()).ok_or_else(|| {
            RetrievalPortError::Contract(
                "durable sealed lexical source digest is not SHA-256".to_owned(),
            )
        })?;
        let artifacts_root = code_text_artifacts_root(store.store_root());
        ensure_private_text_artifacts_root(&artifacts_root)?;
        let staging_path = artifacts_root.join(format!(".text-artifact-{sealed_hex}.staging"));
        let mut source = self.take_preopened_source_or_open(&sealed_identity, control)?;
        let builder_budget =
            text_artifact_builder_budget(build_memory_budget, source.staging_window_bytes())?;
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
    pub(super) fn advance_artifact_text_serving(
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
            let outcome = hotpath::measure_block!(
                "query.artifact.head_open",
                self.open_published_head_or_begin_build(control)
            )?;
            match outcome {
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
        let build_memory_budget = code_lexical_artifact_build_memory_budget_for(
            store.resident_memory.snapshot().limit_bytes,
        );
        let (source_batch_pages, source_batch_bytes, source_work_limit) =
            text_artifact_source_batch_limits(build_memory_budget);
        let mut remaining = maximum_work.min(source_work_limit);
        while remaining > 0 && artifact_build.source_receipt.is_none() {
            let maximum_batch_pages = remaining.clamp(1, source_batch_pages);
            let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(
                maximum_batch_pages,
                source_batch_bytes,
            )
            .map_err(map_sealed_page_source_error)?;
            #[cfg(feature = "hotpath")]
            let completed_lexical_units_before = artifact_build
                .source
                .completed_lexical_units()
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
                    checkpoint_text_artifact_control(control)?;
                    if let Some((previous_page_records, tightened_page_records)) =
                        artifact_build.source.tighten_page_record_bound()
                    {
                        tracing::debug!(
                            previous_page_records,
                            tightened_page_records,
                            error = %error,
                            "subdividing refused lexical page at the unchanged source cursor"
                        );
                        return Ok(false);
                    }
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
                    hotpath::measure_block!(
                        "query.artifact.batch.progress_publish",
                        self.publish_text_progress_boundary(
                            artifact_build,
                            progress,
                            CodeIndexBuildPhaseV1::BulkCommit,
                            u64::try_from(page_count).unwrap_or(u64::MAX),
                            batch_payload_bytes,
                            commit_latency_micros,
                            true,
                        )
                    )?;
                    #[cfg(feature = "hotpath")]
                    {
                        let committed_lexical_units = artifact_build
                            .source
                            .completed_lexical_units()
                            .map_err(map_sealed_page_source_error)?
                            .saturating_sub(completed_lexical_units_before);
                        hotpath::gauge!("query.artifact.batch.committed_lexical_units_total")
                            .inc(committed_lexical_units);
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
        // The builder and source are gone, so its transient reservation no
        // longer owns bytes. Release it before sampling the reader admission;
        // the reader guard then carries the still-live unmodeled baseline.
        drop(build_reservation);
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
        Ok(true)
    }

    fn install_artifact_owners(
        &self,
        reader: CodeLexicalArtifactReaderV1,
        mut reader_reservation: ResidentMemoryReservationV1,
    ) -> Result<(), RetrievalPortError> {
        if reader.retained_owned_bytes() > CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 {
            return Err(RetrievalPortError::Contract(
                "text-artifact reader exceeded its admitted resident-memory ceiling".to_owned(),
            ));
        }
        reader_reservation
            .shrink_to(
                u64::try_from(CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1).map_err(
                    |error| {
                        RetrievalPortError::Contract(format!(
                            "text-artifact reader ceiling exceeds u64: {error}"
                        ))
                    },
                )?,
            )
            .map_err(|error| {
                RetrievalPortError::Contract(format!(
                    "text-artifact reader reservation could not release its admission baseline: \
                     {error}"
                ))
            })?;
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
    pub(super) fn install_graph_serving(
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

/// The one central exact-admission authority every serving owner installs.
fn exact_serving_authority() -> Result<CentralExactAdmissionAuthorityV1, RetrievalPortError> {
    Ok(CentralExactAdmissionAuthorityV1::new(
        ExactAdmissionRuleRevision::new(tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
    ))
}

/// Streaming SHA-256 of one file's bytes, as 64 lowercase hex characters.
/// Cancellation is checked before opening and after every bounded read, so a
/// shutdown or superseding generation cannot strand publication in a
/// corpus-sized uninterruptible hash.
pub(super) fn sha256_private_file_and_size(
    path: &Path,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<([u8; 32], u64), RetrievalPortError> {
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
    Ok((hasher.finalize().into(), file_metadata.len()))
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
pub(super) fn text_artifact_builder_budget(
    build_memory_budget: usize,
    source_window_bytes: usize,
) -> Result<usize, RetrievalPortError> {
    build_memory_budget
        .checked_sub(source_window_bytes)
        .filter(|remaining| *remaining > 0)
        .ok_or(RetrievalPortError::BudgetExceeded)
}
