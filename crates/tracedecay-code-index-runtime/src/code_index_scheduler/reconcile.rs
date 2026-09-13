//! The per-worktree reconcile core: pending hints, captured snapshots, the
//! freshness fence, and `CodeIndexWorktreeSchedulerV1` itself.
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError, RwLock,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gix::{
    bstr::ByteSlice,
    object::tree::diff::{Action as TreeDiffAction, Change as TreeDiffChange},
};
use thiserror::Error;
use tracedecay_application::{
    code_index::{
        DaemonCodeIndexControlV1, ProductionCodeIndexOwnerV1, open_production_code_index_owner_v1,
    },
    semantic_runtime::SavedGenerationScheduleOutcomeV1,
};
use tracedecay_code_index_retention::code_index_generations::{
    DurablePublicationPointerV1, DurableSealedCodeGenerationIdentityV1,
};
use tracedecay_contracts::{
    code_index_freshness::{
        CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1, CodeIndexGenerationRecoveryServingV1,
        CodeIndexGenerationRecoveryV1,
    },
    now_micros,
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, ContentDigest, FileOccurrenceId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, RepositoryDirtyStateV1, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, TreeId, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::GraphConflictContextV1;
use tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1,
    ResidentMemoryAdmissionFailureV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryReservationV1, detected_process_resident_memory_limit_v1,
};

use crate::code_index::{
    graph_projection::CodeGraphProjectionError,
    languages::StaticLanguageRegistry,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationCompatibilityV1,
        CodeIndexGenerationScopeV1, CodeIndexIgnoredSourceAdmissionV1, CodeIndexInputErrorV1,
        CodeIndexProductionConfigV1, CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1,
        CodeIndexRepositoryParseIdentityV1, DAEMON_CODE_INDEX_CHUNKER_REVISION,
        VerifiedSealedTextGenerationMetadataV1,
    },
};

use super::freshness_witness::{
    ReconciledSourceWitnessV1, RestoreFreshnessWitnessV1, SourceContentManifestV1,
};
use super::{
    CodeGraphActivationStateV1, CodeGraphReplayBindingV1, CodeIndexBuildProgressSlotStateV1,
    CodeIndexBuildProgressSlotV1, CodeIndexBuildProgressStateV1, CodeIndexHintPolicyV1,
    CodeIndexIgnoredDependencyRefusalV1, CodeTextProjectionStateV1,
    DaemonCodeIndexPublicationStoreV1, DaemonCodeTextArtifactStoreV1, DaemonProjectionSinkV1,
    DurableActiveSealedGenerationBindingV1, GenerationDecodeAdmissionV1, GenerationServingCachesV1,
    GenerationTextControlV1, LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1,
    ProfiledStdMutex, SharedCodeIndexBytePoolV1, branch_generations, classification,
    file_occurrence_id, freshness_witness, git_tree_capture, id, identity, ignored_dependencies,
    projection_key, snapshot_content_identity, try_publish_build_progress,
};
#[cfg(test)]
use super::{HeldActiveDecodeV1, reconcile_panic_guard};

const MAX_PENDING_HINTS: usize = 1_024;
const MAX_SUPERSEDED_RECONCILE_RETRIES: usize = 4;
pub(super) const CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1: &str = "code-index-build-workers-v1";

const SUPERSEDED_RECONCILE_RETRY_BACKOFF: Duration = Duration::from_millis(75);
type ProductionOwner =
    ProductionCodeIndexOwnerV1<DaemonCodeIndexPublicationStoreV1, DaemonProjectionSinkV1>;

#[derive(Default)]
pub(super) struct PendingHintsV1 {
    pub(super) paths: BTreeSet<PathBuf>,
    pub(super) overflow: bool,
    observed_source_change: bool,
}

impl PendingHintsV1 {
    pub(super) fn count(&self) -> Option<u64> {
        (!self.overflow).then(|| u64::try_from(self.paths.len()).unwrap_or(u64::MAX))
    }

    pub(super) fn path(&mut self, path: PathBuf) {
        if self.paths.len() >= MAX_PENDING_HINTS {
            self.paths.clear();
            self.overflow = true;
        } else {
            self.paths.insert(path);
        }
    }

    pub(super) fn overflow(&mut self) {
        self.paths.clear();
        self.overflow = true;
    }

    pub(super) fn take(&mut self) -> Self {
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

pub(super) struct RetainedReconcileCaptureV1 {
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
pub(super) struct CapturedCandidateV1 {
    pub(super) file: SanitizedCodeFileV1,
    pub(super) captured: CodeIndexCapturedFileV1,
    pub(super) receipt_id: SanitizationReceiptId,
    pub(super) retained: Arc<[u8]>,
    /// Charges the canonical source allocation before the candidate can join a
    /// snapshot. Production borrows this allocation until its bounded intake
    /// materialization, rather than retaining a second snapshot-wide copy.
    pub(super) retained_reservation: Option<ResidentMemoryReservationV1>,
}

pub(super) struct CapturedSnapshotV1 {
    pub(super) snapshot: SanitizedCodeSnapshotV1,
    pub(super) repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    pub(super) captured_files: Vec<CodeIndexCapturedFileV1>,
    pub(super) changed_paths: BTreeSet<String>,
    /// Strong references to this snapshot's interned bytes. The shared byte
    /// pool holds only weak entries; the scheduler retains its current
    /// snapshot's bytes so identical content in sibling worktrees can reuse
    /// them (physical sharing without identity aliasing).
    pub(super) retained_bytes: Vec<Arc<[u8]>>,
    /// Resident charges for `retained_bytes`. Empty sources need no nonzero
    /// reservation.
    pub(super) retained_reservations: Vec<ResidentMemoryReservationV1>,
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

    /// The typed interruption a reconcile pass stopped on, when it did.
    ///
    /// An interrupted pass is not a failed pass: the epoch that cancelled it
    /// was advanced by an observed source change (or shutdown) whose caller
    /// also posted the wake that re-runs the pass, so the worker attributes
    /// it as superseded instead of reporting the served generation stale.
    pub fn reconcile_interruption(
        &self,
    ) -> Option<crate::code_index::production::CodeIndexInterruptionV1> {
        match self {
            Self::Production(CodeIndexProductionErrorV1::Interrupted(interruption)) => {
                Some(*interruption)
            }
            _ => None,
        }
    }

    pub fn is_graph_activation_refusal(&self) -> bool {
        matches!(self, Self::GraphActivationRefused(_))
            || matches!(
                self,
                Self::GraphProjection(CodeGraphProjectionError::BudgetExhausted { budget, .. })
                    if budget == tracedecay_graph_db::GraphBudgetKind::ResidentMemory.as_str()
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
    pub(super) state: Arc<Mutex<SourceFreshnessFenceStateV1>>,
    last_reconciled_at_micros: Arc<AtomicI64>,
    source_epoch: Arc<AtomicU64>,
}

#[derive(Clone)]
pub(super) struct SourceFreshnessFenceStateV1 {
    git_metadata: identity::GitMetadataFingerprintV1,
    pub(super) last_reconciled_at: Instant,
    /// The stat signature (negative cache) and sealed file digests (proof)
    /// the last completed reconcile established; `None` until one has.
    source_witness: Option<ReconciledSourceWitnessV1>,
    pub(super) staleness_threshold: Duration,
    verified_against_source: bool,
    freshness_unknown: bool,
    reconciled_without_generation: bool,
    reconciled_source_epoch: u64,
}

impl SourceFreshnessFenceV1 {
    fn unverified(staleness_threshold: Duration, source_epoch: Arc<AtomicU64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SourceFreshnessFenceStateV1 {
                git_metadata: identity::GitMetadataFingerprintV1::default(),
                last_reconciled_at: Instant::now(),
                source_witness: None,
                staleness_threshold,
                verified_against_source: false,
                freshness_unknown: true,
                reconciled_without_generation: false,
                reconciled_source_epoch: 0,
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
        source_witness: Option<ReconciledSourceWitnessV1>,
        reconciled_without_generation: bool,
    ) {
        let micros = now_micros().0;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.git_metadata = git_metadata;
        state.source_witness = source_witness;
        state.freshness_unknown = false;
        state.last_reconciled_at = Instant::now();
        state.verified_against_source = true;
        state.reconciled_without_generation = reconciled_without_generation;
        state.reconciled_source_epoch = self.source_epoch.load(Ordering::Acquire);
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

    pub(super) fn reconciled_without_generation(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reconciled_without_generation
    }

    /// Whether canonical source input has advanced beyond the last completed
    /// proof. An expired proof alone leaves the epochs equal: its background
    /// pass is verification, not evidence that a replacement is being built.
    pub(super) fn source_change_pending(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.source_epoch.load(Ordering::Acquire) != state.reconciled_source_epoch
    }

    /// Whether Git metadata proves that the checkout moved past the last
    /// completed source verification. An unverified mount has no baseline and
    /// therefore cannot turn its first verification into an observed change.
    pub(super) fn verified_git_metadata_moved(&self, project_root: &Path) -> bool {
        let state = self.snapshot();
        state.verified_against_source
            && identity::GitMetadataFingerprintV1::capture(project_root)
                .differs_from(&state.git_metadata)
    }

    pub(super) fn ready_without_stat(
        &self,
        project_root: &Path,
        shutting_down: &AtomicBool,
    ) -> bool {
        let state = self.snapshot();
        self.snapshot_is_recently_verified(&state, project_root, shutting_down)
    }

    fn snapshot_is_recently_verified(
        &self,
        state: &SourceFreshnessFenceStateV1,
        project_root: &Path,
        shutting_down: &AtomicBool,
    ) -> bool {
        if shutting_down.load(Ordering::Acquire) {
            return false;
        }
        state.verified_against_source
            && self.source_epoch.load(Ordering::Acquire) == state.reconciled_source_epoch
            && !identity::GitMetadataFingerprintV1::capture(project_root)
                .differs_from(&state.git_metadata)
            && state.last_reconciled_at.elapsed() < state.staleness_threshold
    }

    pub(super) fn source_currency_witness_for(
        &self,
        generation_id: &CodeGenerationId,
        snapshot_content_identity: &ContentDigest,
    ) -> Option<ServingSourceWitnessV1> {
        let state = self.snapshot();
        if !state.verified_against_source
            || !state.source_witness.as_ref().is_some_and(|witness| {
                witness
                    .content_manifest
                    .describes_snapshot(snapshot_content_identity)
            })
        {
            return None;
        }
        Some(ServingSourceWitnessV1 {
            generation_id: generation_id.clone(),
        })
    }

    /// Whether the last bounded source proof still admits this exact sealed
    /// snapshot without walking the worktree. Once that proof ages out, reads
    /// report the retained owner stale and let the canonical worker renew it.
    pub(super) fn serves_recently_verified_source(
        &self,
        snapshot_content_identity: &ContentDigest,
        project_root: &Path,
        shutting_down: &AtomicBool,
    ) -> bool {
        let state = self.snapshot();
        state.verified_against_source
            && state.source_witness.as_ref().is_some_and(|witness| {
                witness
                    .content_manifest
                    .describes_snapshot(snapshot_content_identity)
            })
            && self.snapshot_is_recently_verified(&state, project_root, shutting_down)
    }
}

/// What the cheap Git/stat freshness ladder concluded about the retained
/// owner's source. `Unverified` and `Moved` both require a reconcile, but only
/// `Moved` is evidence: an owner no pass has verified yet has not been observed
/// to change, so nothing may be minted from it as an observed source change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FreshnessProbeVerdictV1 {
    /// The last reconcile's proof still describes the live worktree.
    Current,
    /// No reconcile has verified this owner against source truth yet; the
    /// worker's pending pass is the remedy.
    Unverified,
    /// Git metadata or the sealed-digest witness proves the worktree moved
    /// since the last reconcile.
    Moved,
}

pub(super) enum RetainedTextGenerationRestoreV1 {
    Servable(LatestCodeTextGenerationV1),
    Refused(VerifiedSealedTextGenerationMetadataV1),
}

pub struct CodeIndexWorktreeSchedulerV1 {
    pub(super) project_id: ProjectId,
    pub(super) project_root: PathBuf,
    /// Scoped store root for this worktree's sealed generations. Also holds the
    /// restore-time freshness witness sidecar used to skip a redundant cold
    /// reconcile when the on-disk source still equals the restored generation.
    store_root: PathBuf,
    /// The exact indexing identity this worktree is bound to. Re-resolved
    /// before each reconciliation so a HEAD move never mis-attributes a served
    /// generation to a newer revision.
    pub(super) identity: identity::IndexingIdentityV1,
    pub(super) repository_id: RepositoryId,
    pub(super) worktree_id: WorktreeId,
    pub(super) policy: CodeIndexHintPolicyV1,
    /// Independent source-freshness authority. Ready/status probes clone this
    /// handle from the mounted map and never wait for scheduler build state.
    freshness_fence: SourceFreshnessFenceV1,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    /// Keeps the current snapshot's interned bytes alive in the shared pool.
    pub(super) retained_snapshot_bytes: Vec<Arc<[u8]>>,
    /// Holds the measured source-byte charges for
    /// `retained_snapshot_bytes`; worker scratch is admitted separately only
    /// after capture has completed.
    pub(super) _retained_snapshot_memory: Vec<ResidentMemoryReservationV1>,
    /// Deterministic reconcile fault used only by the worker-loop isolation
    /// tests; production never installs one.
    #[cfg(test)]
    reconcile_fault: Option<Arc<reconcile_panic_guard::ReconcileFaultInjectionV1>>,
    /// Process resident-memory authority artifact builds and readers reserve
    /// through. Standalone opens get a private default-limit authority; the
    /// registry rebinds its shared process authority at mount.
    resident_memory: Arc<ProcessResidentMemoryV1>,
    pub(super) publication: DaemonCodeIndexPublicationStoreV1,
    pub(super) production_config: CodeIndexProductionConfigV1,
    pub(super) owner: ProductionOwner,
    pub(super) hints: Arc<Mutex<PendingHintsV1>>,
    /// gix "unchanged" is relative to the index, while active rows may have
    /// been captured from dirty content, so the exact snapshot identity keeps
    /// those paths excluded from reuse after they are reverted.
    active_snapshot_changed_paths: Mutex<Option<(ContentDigest, BTreeSet<String>)>>,
    pub(super) wake: Arc<tokio::sync::Notify>,
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) shutting_down: Arc<AtomicBool>,
    /// Number of in-flight owner passes; nonzero means activation or
    /// reconcile work is running for this worktree.
    pub(super) reconcile_in_progress: Arc<AtomicUsize>,
    /// Typed owner-configuration recovery independently readable while a
    /// replacement generation is building.
    generation_recovery: Arc<RwLock<Option<CodeIndexGenerationRecoveryV1>>>,
    pub(super) latest_content_identity: Option<ContentDigest>,
    pub(super) ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    /// Set when a retained generation was refused because its ignored-source
    /// roster no longer verifies. That refusal also clears the roster, so the
    /// next pass rebuilds without it — but the refused pass has already
    /// consumed the wake that ran it, so nothing scheduled that next pass and
    /// the still-pending source stayed uncaptured with no seat at all. The
    /// worker takes this flag to re-arm exactly one pass; taking it clears it,
    /// so a refusal can never spin the worker.
    ignored_roster_refusal_requires_rebuild: bool,
    /// Admitted paths whose own admission proof no longer holds - the source
    /// is tracked by git again, or its entrypoint now resolves outside the
    /// package it was admitted from.
    ///
    /// Clearing the roster at the refusal was never enough: every reconcile
    /// entry point re-adopts the roster from the active generation, so the
    /// rebuild captured the identical refused paths, sealed the identical
    /// content identity, answered `Noop`, and left the refusal to reproduce
    /// forever against an empty serving slot. Adoption skips these paths, so
    /// the successor is published without them and typed re-admission is the
    /// only way back in. A path whose proof still holds is *not* listed: that
    /// refusal is stale bytes, and its successor must re-capture it under the
    /// same roster.
    pub(super) refused_ignored_source_paths: BTreeSet<String>,
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
    pub(super) semantic_schedule:
        Option<tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
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
    pub(super) publication: DaemonCodeIndexPublicationStoreV1,
    store_root: PathBuf,
    resident_memory: Arc<ProcessResidentMemoryV1>,
    pub(super) project_id: ProjectId,
    worktree_id: WorktreeId,
    shutting_down: Arc<AtomicBool>,
    progress_daemon_incarnation: u64,
    progress_producer_incarnation: u64,
}

impl HistoricalCodeIndexGenerationOwnerV1 {
    fn bind_text(
        &self,
        metadata: VerifiedSealedTextGenerationMetadataV1,
        sealed_format_revision: u32,
    ) -> LatestCodeTextGenerationV1 {
        let generation_id = metadata.manifest().generation_id.clone();
        let mut progress_slot = CodeIndexBuildProgressSlotStateV1::default();
        let text_progress_owner_epoch = progress_slot.replace_generation(generation_id);
        LatestCodeTextGenerationV1 {
            metadata: Arc::new(metadata),
            sealed_format_revision,
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
        }
    }

    pub(super) fn bind_complete(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> LatestCompleteCodeIndexV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let text = self.bind_text(
            VerifiedSealedTextGenerationMetadataV1::from_published_generation(&generation),
            tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
        );
        let latest = LatestCompleteCodeIndexV1 {
            generation,
            text,
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

    pub(crate) fn published_text_generation(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<LatestCodeTextGenerationV1>, CodeIndexSchedulerErrorV1> {
        let pointer = self
            .publication
            .read_publication_pointer()
            .map_err(CodeIndexProductionErrorV1::Publication)?
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "historical text generation has no publication pointer".to_owned(),
                )
            })?;
        let Some(entry) = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
        else {
            return Ok(None);
        };
        let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
            locator: entry.generation_file.clone(),
            digest: ManifestDigest::new(entry.state_digest.clone())
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
            size_bytes: entry.size_bytes,
        };
        let Some(metadata) = self
            .publication
            .partitioned_text_metadata(&sealed_identity)
            .map_err(CodeIndexProductionErrorV1::Publication)?
        else {
            return Ok(None);
        };
        if metadata.manifest().project_id != self.project_id
            || metadata.manifest().generation_id != *generation_id
            || metadata.snapshot().worktree.as_ref() != Some(&self.worktree_id)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "historical text generation identity does not match its retained owner".to_owned(),
            )
            .into());
        }
        Ok(Some(self.bind_text(
            metadata,
            tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
        )))
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

    pub(super) fn active_publication_covers(
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
        let shutting_down = Arc::new(AtomicBool::new(false));
        let publication = DaemonCodeIndexPublicationStoreV1::new(
            &store_root,
            &project_root,
            sanitizer_revision.clone(),
        )?
        .with_shutdown_signal(Arc::clone(&shutting_down));
        let production_config = CodeIndexProductionConfigV1 {
            project_id: project_id.clone(),
            repository: repository_id.clone(),
            sanitizer_revision,
            policy_revision: id::<PolicyRevisionId>("policy.daemon.v1")?,
            // A persisted generation whose chunker revision does not match
            // is a typed rebuild, not a silent reuse. V2 artifacts remain
            // decodable but never recorded unresolved per-file references.
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
            shutting_down,
            reconcile_in_progress: Arc::new(AtomicUsize::new(0)),
            generation_recovery: Arc::new(RwLock::new(None)),
            latest_content_identity,
            ignored_source_admissions: Vec::new(),
            ignored_roster_refusal_requires_rebuild: false,
            refused_ignored_source_paths: BTreeSet::new(),
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
    pub(super) fn reserve_worker_memory(
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

    pub(super) fn reserve_snapshot_memory(
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

    pub(super) fn finish_snapshot_build_memory(
        _reservations: &mut [ResidentMemoryReservationV1],
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        Ok(())
    }

    pub(super) fn ensure_worker_plan(&self) -> Result<(), CodeIndexSchedulerErrorV1> {
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
        hook: Option<tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
    ) {
        self.semantic_schedule = hook;
    }

    /// Schedule semantics only after the registry has activated and published
    /// this exact generation as serving state.
    ///
    /// Every outcome is typed and recorded. This is the one boundary a sealed
    /// generation crosses on its way to projection, and a bare `false` here
    /// left an operator with a runtime parked at `installed` and no evidence
    /// of why later generations never re-triggered projection (#753).
    pub fn schedule_semantic_generation(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> SavedGenerationScheduleOutcomeV1 {
        let generation_id = generation.manifest().generation_id.clone();
        let Some(schedule) = self.semantic_schedule.as_ref() else {
            return Self::record_semantic_schedule_outcome(
                &generation_id,
                SavedGenerationScheduleOutcomeV1::RuntimeNotMounted,
            );
        };
        let outcome = match catch_unwind(AssertUnwindSafe(|| {
            hotpath::measure_block!(
                "code_index.semantic_generation_handoff",
                schedule(generation)
            )
        })) {
            Ok(outcome) => outcome,
            Err(_) => SavedGenerationScheduleOutcomeV1::HookPanicked,
        };
        Self::record_semantic_schedule_outcome(&generation_id, outcome)
    }

    /// Name every non-scheduled handoff so silence never stands in for a
    /// reason. A scheduled handoff is reported by the runtime itself.
    fn record_semantic_schedule_outcome(
        generation_id: &CodeGenerationId,
        outcome: SavedGenerationScheduleOutcomeV1,
    ) -> SavedGenerationScheduleOutcomeV1 {
        if !outcome.is_scheduled() {
            tracing::warn!(
                event = "code_index_semantic_schedule_declined",
                outcome = outcome.as_str(),
                generation = %generation_id,
                "code-index did not hand this generation to semantic projection"
            );
        }
        outcome
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
            Self::record_background_reconcile_hint(&mut hints, &self.epoch, source_changed);
        }
        // `Notify` already coalesces stored permits. Always refresh the permit:
        // a prior worker may have consumed its wake and then failed before
        // draining this overflow marker.
        self.wake.notify_one();
    }

    /// Record one source-neutral or source-moving reconcile hint through the
    /// canonical hint and cancellation authorities. Registry reads use this
    /// while the scheduler mutex is owned by a blocking reconcile so Git drift
    /// cannot disappear behind unrelated in-flight work.
    pub(super) fn record_background_reconcile_hint(
        hints: &mut PendingHintsV1,
        epoch: &AtomicU64,
        source_changed: bool,
    ) {
        let newly_observed_change = source_changed && !hints.observed_source_change;
        hints.observed_source_change |= source_changed;
        if newly_observed_change {
            // Advance while holding the hint authority: a reconciler that
            // drains the observed-change marker must also observe the
            // cancellation epoch that marker minted.
            DaemonCodeIndexControlV1::advance(epoch);
        }
        hints.overflow();
    }

    #[hotpath::measure(label = "code_index.generation.compatibility_observe")]
    fn observe_generation_compatibility(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let compatibility = generation.compatibility_with(&self.production_config);
        self.observe_compatibility(&generation.manifest().generation_id, compatibility)
    }

    fn observe_retained_text_compatibility(
        &self,
        metadata: &VerifiedSealedTextGenerationMetadataV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let compatibility = metadata.manifest_compatibility_with(&self.production_config);
        self.observe_compatibility(&metadata.manifest().generation_id, compatibility)
    }

    fn observe_compatibility(
        &self,
        generation_id: &CodeGenerationId,
        compatibility: CodeIndexGenerationCompatibilityV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let next = if compatibility.is_reusable() {
            None
        } else {
            Some(CodeIndexGenerationRecoveryV1 {
                incompatible_generation_id: generation_id.as_str().to_owned(),
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
                    generation_id = %generation_id,
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
    /// frontier proves that the exact worktree state it describes is unchanged:
    /// Git metadata and the stat signature as the negative cache, then the
    /// generation's sealed file digests against the bytes on disk as the proof.
    ///
    /// This is deliberately background-only: sealed decode, gix status/index
    /// classification, the source stat sweep, and the digest comparison are
    /// all repository-sized. Missing, corrupt, or mismatched frontier evidence
    /// simply declines the fast path so the same retained owner performs
    /// authoritative reconcile.
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
        if self.retained_frontier_stat_sweep(&pointer).is_none() {
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
        // Re-swept under the adopted ignored-source roster. Equal metadata is
        // only the negative cache; the retained generation is current only if
        // the bytes on disk still carry the file digests it sealed.
        let Ok(sweep) = self.worktree_stat_sweep() else {
            return Ok(None);
        };
        if witness.stat_signature != sweep.signature {
            return Ok(None);
        }
        let source_manifest = SourceContentManifestV1::for_snapshot(generation.snapshot());
        if !sweep.content_matches(&self.project_root, &source_manifest, &self.shutting_down) {
            return Ok(None);
        }
        let snapshot_content_identity = generation.snapshot().content_identity.clone();
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        self.mark_reconciled(source_manifest);
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
    pub(super) fn seat_retained_generation_on_empty_serving(
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
        // A quiet stat sweep is only the negative cache. With the generation
        // already decoded its sealed file digests settle the question here;
        // without one the follow-up pass settles it, and this seat never
        // claims currency either way.
        let quietly_current = self
            .retained_frontier_stat_sweep(&pointer)
            .is_some_and(|sweep| {
                decoded.as_ref().is_none_or(|generation| {
                    sweep.content_matches(
                        &self.project_root,
                        &SourceContentManifestV1::for_snapshot(generation.snapshot()),
                        &self.shutting_down,
                    )
                })
            });
        let dirty = {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hints.overflow || !hints.paths.is_empty()
        } || configuration_changed
            || !quietly_current;
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
    /// `Some` is the negative cache only — the metadata the witness recorded
    /// has not moved — and hands back the sweep so the caller can settle
    /// currency against the retained generation's sealed file digests.
    fn retained_frontier_stat_sweep(
        &self,
        pointer: &DurablePublicationPointerV1,
    ) -> Option<freshness_witness::WorktreeStatSweepV1> {
        let witness = RestoreFreshnessWitnessV1::load(&self.store_root)?;
        if witness.generation_id != pointer.generation_id {
            return None;
        }
        let metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        if witness.git_metadata_signature != metadata.stable_signature() {
            return None;
        }
        self.worktree_stat_sweep()
            .ok()
            .filter(|sweep| witness.stat_signature == sweep.signature)
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
        self.mark_reconciled_state(
            git_metadata.clone(),
            stat_signature
                .clone()
                .map(|signature| ReconciledSourceWitnessV1::new(signature, pending.snapshot())),
        );
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

    pub(super) fn reconcile_retained_text_generation_with(
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
        let retained_is_reusable = self
            .observe_retained_text_compatibility(metadata)
            .is_reusable();
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
        let Some(sampled_sweep) = self.worktree_stat_sweep().ok() else {
            return Ok(None);
        };
        let has_hints = {
            let hints = self
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hints.overflow || !hints.paths.is_empty()
        };
        // The witness proves a quiet tree only through the retained
        // generation's sealed file digests; its matching stat signature is
        // the negative cache that lets a moved tree skip the byte comparison.
        let source_manifest = SourceContentManifestV1::for_snapshot(metadata.snapshot());
        if retained_is_reusable
            && !has_hints
            && let Some(witness) = witness.as_ref()
            && witness.git_metadata_signature == sampled_metadata.stable_signature()
            && witness.stat_signature == sampled_sweep.signature
            && sampled_sweep.content_matches(
                &self.project_root,
                &source_manifest,
                &self.shutting_down,
            )
        {
            let snapshot_content_identity = metadata.snapshot().content_identity.clone();
            self.latest_content_identity = Some(snapshot_content_identity.clone());
            self.mark_reconciled_retained_generation_state(
                sampled_metadata,
                Some(ReconciledSourceWitnessV1 {
                    stat_signature: sampled_sweep.signature,
                    content_manifest: source_manifest,
                }),
            );
            return Ok(Some(CodeIndexReconcileOutcomeV1::Noop(
                CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity,
                    overflow_reconciled: false,
                },
            )));
        }

        // A compatible generation whose witness did not prove a quiet tree
        // falls through to the full graph-on reconcile. An incompatible
        // lightweight owner rebuilds here without decoding the retained graph.
        if retained_is_reusable && !rebuild_changed_source_without_decode {
            return Ok(None);
        }

        let _worker_memory = self.reserve_incremental_rebuild_memory()?;
        let Some(capture) = self.capture_retained_reconcile_attempt()? else {
            return Ok(None);
        };
        self.finish_retained_reconcile(metadata, witness, capture)
    }

    pub(super) fn capture_retained_reconcile_attempt(
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

    pub(super) fn finish_retained_reconcile(
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
        let rebuild_incompatible_generation = !self
            .observe_retained_text_compatibility(metadata)
            .is_reusable();
        if rebuild_incompatible_generation
            || captured.snapshot.reference != metadata.snapshot().reference
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
            if !self
                .observe_generation_compatibility(&generation)
                .is_reusable()
            {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "newly published generation is incompatible with its production owner"
                        .to_owned(),
                ));
            }
            Self::finish_snapshot_build_memory(&mut captured.retained_reservations)?;
            self.retained_snapshot_bytes = std::mem::take(&mut captured.retained_bytes);
            self._retained_snapshot_memory = std::mem::take(&mut captured.retained_reservations);
            self.latest_content_identity = Some(snapshot_content_identity);
            self.mark_reconciled_retained_generation_state(
                git_metadata.clone(),
                stat_signature.clone().map(|signature| {
                    ReconciledSourceWitnessV1::new(signature, generation.snapshot())
                }),
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
        let source_witness = stat_signature
            .clone()
            .map(|signature| ReconciledSourceWitnessV1::new(signature, &captured.snapshot));
        let snapshot_content_identity = captured.snapshot.content_identity;
        self.latest_content_identity = Some(snapshot_content_identity.clone());
        self.mark_reconciled_retained_generation_state(git_metadata.clone(), source_witness);
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
            self.retire_unprovable_ignored_source_admissions(&generation);
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
    pub(super) fn restore_retained_text_generation(
        &mut self,
    ) -> Option<RetainedTextGenerationRestoreV1> {
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
                                completed_lexical_units: scanned,
                                total_lexical_units: total,
                                current_batch_pages: 0,
                                current_batch_payload_bytes: 0,
                                elapsed_micros,
                                last_commit_latency_micros: None,
                                files_per_second: None,
                                lexical_units_per_second: None,
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
        let compatibility = self.observe_retained_text_compatibility(&metadata);
        if !compatibility.may_serve_while_rebuilding() {
            text_control.retire();
            self.request_background_reconcile();
            return Some(RetainedTextGenerationRestoreV1::Refused(metadata));
        }
        if !compatibility.is_reusable() {
            self.request_background_reconcile();
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
        Some(RetainedTextGenerationRestoreV1::Servable(
            LatestCodeTextGenerationV1 {
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
            },
        ))
    }

    pub fn servable_retained_text_generation(&mut self) -> Option<LatestCodeTextGenerationV1> {
        match self.restore_retained_text_generation()? {
            RetainedTextGenerationRestoreV1::Servable(generation) => Some(generation),
            RetainedTextGenerationRestoreV1::Refused(_) => None,
        }
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

    pub(super) fn reconcile_now_with_capture<C>(
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
                self.mark_reconciled(SourceContentManifestV1::for_snapshot(&captured.snapshot));
                return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity: captured.snapshot.content_identity,
                    overflow_reconciled,
                }));
            }

            // Only the content identity, the file manifest, and the
            // changed-path count are needed after the build request takes
            // ownership of the captured snapshot, so keep those instead of
            // cloning every file record and changed path.
            let mut snapshot_content_identity = captured.snapshot.content_identity.clone();
            let mut source_manifest = SourceContentManifestV1::for_snapshot(&captured.snapshot);
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
                source_manifest = SourceContentManifestV1::for_snapshot(&captured.snapshot);
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
                    self.mark_reconciled(source_manifest);
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
            self.mark_reconciled(source_manifest);

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

    /// Record that the worktree was just reconciled against `source_manifest`,
    /// the per-file digests of the snapshot this pass captured or verified.
    pub(super) fn mark_reconciled(&mut self, source_manifest: SourceContentManifestV1) {
        let metadata = identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let source_witness =
            self.worktree_stat_signature()
                .ok()
                .map(|stat_signature| ReconciledSourceWitnessV1 {
                    stat_signature,
                    content_manifest: source_manifest,
                });
        self.mark_reconciled_state(metadata, source_witness);
        self.persist_freshness_witness();
    }

    fn mark_reconciled_state(
        &mut self,
        metadata: identity::GitMetadataFingerprintV1,
        source_witness: Option<ReconciledSourceWitnessV1>,
    ) {
        let reconciled_without_generation = self
            .publication
            .load_active_shared()
            .is_ok_and(|generation| generation.is_none());
        self.freshness_fence.mark_reconciled(
            metadata,
            source_witness,
            reconciled_without_generation,
        );
    }

    fn mark_reconciled_retained_generation_state(
        &mut self,
        metadata: identity::GitMetadataFingerprintV1,
        source_witness: Option<ReconciledSourceWitnessV1>,
    ) {
        self.freshness_fence
            .mark_reconciled(metadata, source_witness, false);
    }

    /// Record the restore-time freshness witness for the current active
    /// generation. Called at the moment freshness is established (after a
    /// reconcile verified the worktree against gix truth) so a later open of the
    /// same worktree can skip straight to reconcile when the stat metadata
    /// moved, and otherwise verify the sealed generation's file digests without
    /// re-extracting. Requires an active generation AND a captured stat
    /// signature; when either is absent the optimization simply defers to the
    /// next reconcile, and a write failure is non-fatal.
    fn persist_freshness_witness(&self) {
        let freshness = self.freshness_fence.snapshot();
        let Some(stat_signature) = freshness
            .source_witness
            .map(|witness| witness.stat_signature)
        else {
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
    pub(super) fn latest_complete_ready_for_query(
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
    pub(super) fn exact_source_is_ready(&mut self) -> Result<bool, CodeIndexSchedulerErrorV1> {
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
        if self.source_witness_matches_worktree(&freshness) {
            // The exact-source content fence is stronger than the elapsed
            // tier-2 arm. Refresh only the monotonic admission clock: no
            // reconcile receipt or wall timestamp is fabricated, and a
            // clean status census cannot turn into a full capture loop.
            self.freshness_fence.refresh_monotonic_clock(false);
            Ok(true)
        } else {
            self.request_background_reconcile();
            Ok(false)
        }
    }

    /// Whether the last reconcile's source witness still describes the
    /// worktree: unchanged stat metadata (the negative cache) and, only then,
    /// every candidate's content digest equal to the sealed file manifest.
    fn source_witness_matches_worktree(&self, freshness: &SourceFreshnessFenceStateV1) -> bool {
        freshness.source_witness.as_ref().is_some_and(|witness| {
            witness.matches_worktree(
                &self.project_root,
                &self.ignored_source_admissions,
                &self.shutting_down,
            )
        })
    }

    /// Mint the exact-source currency witness for one generation from the
    /// freshness state the last completed reconcile proved against gix truth.
    /// `None` is a typed abstention (nothing was ever proven), never a default:
    /// a busy verified read holding no witness refuses instead of serving.
    pub(super) fn source_currency_witness_for(
        &self,
        generation_id: &CodeGenerationId,
        snapshot_content_identity: &ContentDigest,
    ) -> Option<ServingSourceWitnessV1> {
        self.freshness_fence
            .source_currency_witness_for(generation_id, snapshot_content_identity)
    }

    /// A cheap stat-level (path, mtime, size) signature of the present source
    /// candidates. It opens gix and runs stat-based status (no byte reads, no
    /// content hashing). A changed signature skips straight to reconcile; an
    /// unchanged one proves nothing on its own and is always followed by the
    /// sealed-digest comparison in [`freshness_witness::WorktreeStatSweepV1`].
    pub(super) fn worktree_stat_signature(&self) -> Result<String, CodeIndexSchedulerErrorV1> {
        self.worktree_stat_sweep().map(|sweep| sweep.signature)
    }

    fn worktree_stat_sweep(
        &self,
    ) -> Result<freshness_witness::WorktreeStatSweepV1, CodeIndexSchedulerErrorV1> {
        freshness_witness::worktree_stat_sweep(&self.project_root, &self.ignored_source_admissions)
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
        // Tier 2: the bounded-staleness window elapsed. A moved stat signature
        // skips straight to capture; an unchanged one is settled against the
        // sealed file digests, so a quiet repository resets its clock without
        // re-extracting and a metadata-preserving rewrite still reconciles.
        if self.source_witness_matches_worktree(&freshness) {
            self.freshness_fence.refresh_monotonic_clock(true);
            Ok(None)
        } else {
            Ok(Some(self.reconcile_now()?))
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

    /// Run the cheap Git/stat ladder — unverified restore, tier-1 git
    /// metadata, tier-2 bounded staleness with the source witness — without
    /// posting a worker wake.
    ///
    /// The ladder judges movement from source truth only: Git metadata and the
    /// stat witness. It deliberately does not compare the cancellation epoch
    /// against the last reconciled epoch — every epoch advance is paired with
    /// its own worker wake (a hook hint, an overflow, an observed change), so
    /// that pending pass is already the remedy. Treating a hint-advanced epoch
    /// as movement here made a concurrent query escalate the targeted hint
    /// pass into an overflow rescan and relabel the arrival as its own.
    pub(super) fn freshness_probe_verdict(&mut self) -> FreshnessProbeVerdictV1 {
        let freshness = self.freshness_fence.snapshot();
        if !freshness.verified_against_source {
            return FreshnessProbeVerdictV1::Unverified;
        }
        if identity::GitMetadataFingerprintV1::capture(&self.project_root)
            .differs_from(&freshness.git_metadata)
        {
            return FreshnessProbeVerdictV1::Moved;
        }
        if freshness.last_reconciled_at.elapsed() < self.policy.staleness_threshold {
            return FreshnessProbeVerdictV1::Current;
        }
        if self.source_witness_matches_worktree(&freshness) {
            self.freshness_fence.refresh_monotonic_clock(true);
            return FreshnessProbeVerdictV1::Current;
        }
        FreshnessProbeVerdictV1::Moved
    }

    /// Decide whether the cheap Git/stat ladder requires an authoritative
    /// reconcile, without posting a worker wake. Callers that own a separate
    /// cadence authority use this split form so they can record the arrival
    /// before making the worker runnable.
    pub fn freshness_probe_requires_reconcile(&mut self) -> bool {
        self.freshness_probe_verdict() != FreshnessProbeVerdictV1::Current
    }

    /// [`Self::ensure_fresh_for_query`] with the O(store) rebuild moved off the
    /// request path.
    ///
    /// Runs the identical ladder — unverified restore, tier-1 git metadata,
    /// tier-2 bounded staleness — but where `ensure_fresh_for_query` calls
    /// `reconcile_now()` inline this only *requests* the background worker.
    /// The ladder's checks never extract or publish; its remedy does, and a
    /// query must never pay for it. Unlike
    /// [`Self::latest_complete_ready_for_query_with`], this arm still sweeps
    /// the source witness on an elapsed threshold — stat metadata first, then
    /// the sealed file digests when the metadata is unchanged — so a quiet
    /// repository can reset its clock without a capture.
    ///
    /// Returns whether a reconcile was actually requested. A quiet repository
    /// must answer `false` and wake nothing: the ladder suppressing work is the
    /// common case, and waking the worker on every read would turn each query
    /// into a rebuild trigger — exactly the coupling this change removes.
    ///
    /// Only proven movement is recorded as an observed source change. An owner
    /// nothing has verified yet — a fresh mount or restart whose first pass is
    /// still pending — answers "not current" so the caller posts its plain
    /// query-admission wake, but nothing was observed to move, so no overflow
    /// hint, observed-change marker, or cancellation epoch is minted for it.
    /// Fabricating that overflow made the restart's own verifying pass skip
    /// the sealed-digest witness a quiet tree would have satisfied and fall
    /// into the full sealed-generation replay the revision-7 verified-head
    /// recovery exists to avoid.
    pub fn request_fresh_for_query_background(&mut self) -> bool {
        match self.freshness_probe_verdict() {
            FreshnessProbeVerdictV1::Current => false,
            FreshnessProbeVerdictV1::Unverified => true,
            FreshnessProbeVerdictV1::Moved => {
                // The ladder proved this worktree moved, so this is the wake
                // that may advance the canonical change generation and
                // supersede index work.
                self.request_background_reconcile_for_observed_change();
                true
            }
        }
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

    pub(super) fn latest_complete_with(
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
    pub(super) fn bind_latest_complete(
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
    pub(super) fn prime_serving_caches(&self) {
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

    pub(super) fn generation(
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
    pub(super) fn capture_candidate(
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
    pub(super) fn capture_authoritative_snapshot(
        &self,
        control: Option<&dyn CodeIndexExecutionControlV1>,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        self.capture_authoritative_snapshot_with_active_generation_reuse(control, true)
    }

    pub(super) fn capture_authoritative_snapshot_without_active_generation_reuse(
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
            match self.capture_exact_git_tree_snapshot(
                    &git_tree_capture::ExactGitTreeSourceV1 {
                        reference: reference.clone(),
                        revision: revision.clone(),
                        tree: tree.clone(),
                    },
                    &branch_generations::BranchGenerationReadControlV1 {
                        deadline: None,
                        cancellation: None,
                    },
                ) {
                Ok(captured) => {
                    *self
                        .active_snapshot_changed_paths
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
                        captured.snapshot.content_identity.clone(),
                        captured.changed_paths.clone(),
                    ));
                    return Ok(captured);
                }
                Err(
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                ) => {}
                Err(
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::Cancelled,
                ) => return Err(cancelled_code_index_reconcile()),
                Err(
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable,
                ) => return Err(CodeIndexSchedulerErrorV1::SnapshotMemoryCapacityUnavailable),
                Err(reason) => {
                    return Err(CodeIndexSchedulerErrorV1::Git(format!(
                        "immutable HEAD-tree capture failed: {}",
                        reason.as_str()
                    )));
                }
            }
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

pub(super) fn cancelled_code_index_reconcile() -> CodeIndexSchedulerErrorV1 {
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
