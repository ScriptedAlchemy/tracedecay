//! Daemon-owned registry of mounted per-worktree code-index schedulers.
//!
//! Owns the map of live worktree schedulers, their reconciliation worker tasks,
//! and the shared content-addressed byte pool. The registry is the async-facing
//! surface: hook-hint delivery, query-admission freshness, and lifecycle
//! (mount/shutdown). The synchronous per-worktree indexing logic lives on
//! [`CodeIndexWorktreeSchedulerV1`]; this module never runs it while holding the
//! registry map lock.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::Condvar;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, WorktreeId};
use tracedecay_lsp::LspRuntimeFailure;

use super::graph_activation::CodeGraphActivationAuthorityV1;
use super::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, CodeIndexNoopEvidenceV1,
    CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexControlV1, GenerationDecodeAdmissionV1,
    LatestCompleteCodeIndexV1, PendingHintsV1, SharedCodeIndexBytePoolV1,
    newly_eligible_percentile, now_micros,
};
#[cfg(test)]
use super::{CodeIndexBytePoolStatsV1, CodeIndexCadenceReadModelV1};

mod ignored_dependencies;
mod lsp_projection;
#[cfg(test)]
mod runtime_generation_census_tests;
mod scope_identity;

use self::ignored_dependencies::exact_activated_serving_generation;
pub(super) use scope_identity::latest_matches_scope_identity;

const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;

/// Bounded exponential backoff between activation retries of the same sealed
/// generation. Activation of a large artifact is minutes of real work, so the
/// floor stays above the query staleness threshold and the ceiling keeps a
/// persistently failing artifact from being retried more than a few times an
/// hour while never resealing it. Tests shrink the clock, not the shape.
const ACTIVATION_RETRY_BACKOFF_FLOOR: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(30)
};
const ACTIVATION_RETRY_BACKOFF_CEILING: Duration = if cfg!(test) {
    Duration::from_millis(400)
} else {
    Duration::from_mins(10)
};

#[cfg(test)]
struct ColdMountFinalCommitGateV1 {
    project_root: PathBuf,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
fn cold_mount_final_commit_gate() -> &'static Mutex<Option<ColdMountFinalCommitGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<Option<ColdMountFinalCommitGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
struct ExistingSemanticScheduleReplacementGateV1 {
    project_root: PathBuf,
    entered: tokio::sync::oneshot::Sender<()>,
}

#[cfg(test)]
fn existing_semantic_schedule_replacement_gate()
-> &'static Mutex<Option<ExistingSemanticScheduleReplacementGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<Option<ExistingSemanticScheduleReplacementGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(None))
}

mod resident_memory;
pub(super) mod watch_ingress;

/// Bounded daemon-wide concurrency for expensive background reconciles and
/// mounts. A single global permit serialized EVERY project/worktree cold build
/// across the whole daemon, turning independent opens into an N-way queue.
///
/// The bound is 2, not 4. Per-file extraction now fans out across the shared
/// reserved-width indexing pool (`tracedecay_code_index::parallelism`), so a
/// SINGLE worktree already saturates every non-reserved core. Admitting more
/// worktrees cannot add throughput — the pool is the same pool — it only
/// interleaves them, so every worktree's index lands N times later and every
/// worktree's snapshot bytes sit in RSS N times longer. Race-to-idle: run a
/// worktree at full width, finish it, take the next one.
///
/// Two rather than one because a reconcile is not pure CPU: gix
/// classification, store writes and publication are I/O and lock phases that
/// do not touch the indexing pool, so a second admitted worktree overlaps
/// those with the first one's extraction at negligible CPU cost.
///
/// Same-store (same-worktree) exclusion does NOT depend on this bound: each
/// mounted worktree owns exactly one reconcile worker task that dequeues wakes
/// one at a time, and every reconcile additionally runs under that worktree's
/// per-scheduler `Mutex`. Raising the global bound therefore only lets DISTINCT
/// worktrees (which write to path-scoped stores) reconcile in parallel; it can
/// never overlap two reconciles for the same worktree/store.
const MAX_CONCURRENT_RECONCILE_WORKTREES: usize = 2;

fn bounded_daemon_admission_permits() -> usize {
    std::thread::available_parallelism().map_or(1, |cores| {
        cores.get().min(MAX_CONCURRENT_RECONCILE_WORKTREES)
    })
}

#[cfg(test)]
fn cold_mount_admission_barriers() -> &'static Mutex<BTreeMap<PathBuf, Arc<tokio::sync::Barrier>>> {
    static BARRIERS: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, Arc<tokio::sync::Barrier>>>> =
        std::sync::OnceLock::new();
    BARRIERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
struct ColdMountPostCheckTestControlV1 {
    reached: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
fn cold_mount_post_check_controls()
-> &'static Mutex<BTreeMap<PathBuf, Arc<ColdMountPostCheckTestControlV1>>> {
    static CONTROLS: std::sync::OnceLock<
        Mutex<BTreeMap<PathBuf, Arc<ColdMountPostCheckTestControlV1>>>,
    > = std::sync::OnceLock::new();
    CONTROLS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColdMountOpenEventV1 {
    Started,
    Finished,
}

#[cfg(test)]
struct ColdMountOpenTestControlV1 {
    blocks_open: bool,
    released: Mutex<bool>,
    release: Condvar,
    events: Mutex<Vec<ColdMountOpenEventV1>>,
    changed: tokio::sync::watch::Sender<usize>,
    followers: AtomicUsize,
}

#[cfg(test)]
impl ColdMountOpenTestControlV1 {
    fn new(blocks_open: bool) -> Self {
        let (changed, _) = tokio::sync::watch::channel(0);
        Self {
            blocks_open,
            released: Mutex::new(false),
            release: Condvar::new(),
            events: Mutex::new(Vec::new()),
            changed,
            followers: AtomicUsize::new(0),
        }
    }

    fn record(&self, event: ColdMountOpenEventV1) {
        let count = {
            let mut events = self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            events.push(event);
            events.len()
        };
        self.changed.send_replace(count);
    }

    fn record_follower(&self) {
        self.followers.fetch_add(1, Ordering::AcqRel);
        let count = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        self.changed.send_replace(count);
    }
}

#[cfg(test)]
fn cold_mount_open_controls() -> &'static Mutex<BTreeMap<PathBuf, Arc<ColdMountOpenTestControlV1>>>
{
    static CONTROLS: std::sync::OnceLock<
        Mutex<BTreeMap<PathBuf, Arc<ColdMountOpenTestControlV1>>>,
    > = std::sync::OnceLock::new();
    CONTROLS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
struct QueryAdmissionTestControlV1 {
    lookup_gate: tokio::sync::Mutex<()>,
    rendezvous: tokio::sync::Barrier,
    pauses_after_claim: AtomicBool,
    claim_reached: AtomicBool,
    claim_entered: tokio::sync::Notify,
    claim_release: tokio::sync::Notify,
}

#[cfg(test)]
fn query_admission_controls()
-> &'static Mutex<BTreeMap<WorktreeId, Arc<QueryAdmissionTestControlV1>>> {
    static CONTROLS: std::sync::OnceLock<
        Mutex<BTreeMap<WorktreeId, Arc<QueryAdmissionTestControlV1>>>,
    > = std::sync::OnceLock::new();
    CONTROLS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Deterministically holds a cancelling query's wake claim while it owns the
/// canonical wake state. A foreign producer announces before it contends on
/// that state lock, which exercises the old split-CAS interleaving without a
/// timing race.
#[cfg(test)]
struct PendingWakeDropGateTestV1 {
    drop_reached: AtomicBool,
    drop_entered: tokio::sync::Notify,
    drop_released: Mutex<bool>,
    drop_release: Condvar,
    foreign_attempted: AtomicBool,
    foreign_entered: tokio::sync::Notify,
}

#[cfg(test)]
impl PendingWakeDropGateTestV1 {
    fn new() -> Self {
        Self {
            drop_reached: AtomicBool::new(false),
            drop_entered: tokio::sync::Notify::new(),
            drop_released: Mutex::new(false),
            drop_release: Condvar::new(),
            foreign_attempted: AtomicBool::new(false),
            foreign_entered: tokio::sync::Notify::new(),
        }
    }
}

/// One mounted worktree's code scope identity and serving generation, read
/// without touching the scheduler mutex.
pub(in crate::daemon) struct CodeIndexServingScopeV1 {
    pub(in crate::daemon) repository_id: RepositoryId,
    pub(in crate::daemon) worktree_id: WorktreeId,
    pub(in crate::daemon) shutting_down: Arc<AtomicBool>,
    pub(in crate::daemon) serving_generation: Option<Arc<CodeIndexPublishedGenerationV1>>,
}

/// Outcome of retiring the retained generation from a failed branch
/// publication. A no-match preserves a newer generation that won the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum ServingGenerationRollbackOutcomeV1 {
    Cleared,
    NoMatch,
}

/// Slot-local claim kept independently from its RAII lease.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServingGenerationInstallationClaimV1 {
    token: u64,
    serving_epoch: u64,
    generation_id: CodeGenerationId,
}

/// Non-clone ownership of one exact serving-slot installation. Dropping an
/// unfinished lease releases only its matching claim; it never clears or
/// mutates the serving generation, so cancellation cannot strand a later
/// exact replay behind an abandoned same-epoch claim.
#[derive(Debug)]
#[must_use = "an installation lease must be committed, retired, or dropped"]
pub(in crate::daemon) struct ServingGenerationInstallationV1 {
    claim: ServingGenerationInstallationClaimV1,
    active_installation: Arc<Mutex<Option<ServingGenerationInstallationClaimV1>>>,
}

impl Drop for ServingGenerationInstallationV1 {
    fn drop(&mut self) {
        let mut active = self
            .active_installation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.as_ref() == Some(&self.claim) {
            *active = None;
        }
    }
}

/// Result of claiming one exact serving generation for a branch publication.
#[derive(Debug)]
pub(in crate::daemon) enum ServingGenerationInstallationOutcomeV1 {
    Installed(ServingGenerationInstallationV1),
    NoMatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexGenerationPublishedV1 {
    pub project_root: PathBuf,
    pub repository_id: RepositoryId,
    pub generation_id: CodeGenerationId,
    pub snapshot_content_identity: tracedecay_domain::ContentDigest,
    pub observation_time_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::daemon) struct QueryActivationAttemptV1 {
    revision: ConfigurationRevisionId,
    token: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodeIndexSchedulerMemoryStatsV1 {
    pub mounted_worktrees: u64,
    pub reconciling_worktrees: u64,
    pub retained_generation_encoded_bytes: u64,
}

pub(super) struct MountedCodeIndexWorktreeV1 {
    pub(super) repository_id: RepositoryId,
    pub(super) worktree_id: WorktreeId,
    pub(super) query_authority: Option<(
        ManifestDigest,
        Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    )>,
    pub(super) semantic_query_authority: Option<(
        ManifestDigest,
        Arc<super::semantic_query_runtime::SemanticQueryAuthorityV1>,
    )>,
    pub(super) query_activation_revision: Option<ConfigurationRevisionId>,
    pub(super) query_activation_epoch: Option<i64>,
    pub(super) query_activation_transition_digest: Option<ManifestDigest>,
    pub(super) query_activation_attempt: u64,
    pub(super) query_activation_redundancy:
        Option<tracedecay_usecases::semantic_runtime::PreparedSemanticRedundancyAuthorityV1>,
    pub(super) semantic_vector_graph_provider:
        Option<Arc<dyn tracedecay_usecases::semantic_runtime::SemanticVectorGraphProviderV1>>,
    pub(super) scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    pub(super) serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    /// Monotonic replacement epoch for the serving slot. It invalidates a
    /// branch-publication token even if a future worker re-seats an equal id.
    serving_generation_epoch: Arc<AtomicU64>,
    /// One in-flight branch publication may own a serving-slot installation.
    /// It is paired with `serving_generation_epoch` under the slot CAS.
    serving_generation_installation: Arc<Mutex<Option<ServingGenerationInstallationClaimV1>>>,
    graph_activation: CodeGraphActivationAuthorityV1,
    ignored_dependency_admissions: Arc<
        Mutex<
            BTreeMap<
                ignored_dependencies::AdmissionFlightKeyV1,
                Arc<ignored_dependencies::AdmissionFlightV1>,
            >,
        >,
    >,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    /// The exact pending wake state. Its one lock linearizes ownership, arrival
    /// timestamp, and trigger so a cancelling query cannot erase a coalesced
    /// foreign wake between independent atomic updates.
    pending_wake: Arc<PendingWakeV1>,
    /// Canonical Plan 26 observability lane, installed once after project open
    /// mounts the project-bound producer. Empty means this worktree records no
    /// canonical index or retrieval observations (never a fabricated zero).
    index_observability: Arc<OnceLock<super::observability::CodeIndexObservabilityV1>>,
    shutting_down: Arc<AtomicBool>,
    /// Count of in-flight owner passes; nonzero means activation or reconcile
    /// work is running for this worktree.
    reconcile_in_progress: Arc<AtomicUsize>,
    /// Live handle to the publication's encoded-byte counter; observed only by
    /// test memory accounting today.
    _active_generation_encoded_bytes: Arc<AtomicU64>,
    pub(super) semantic_evaluation_publication_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

pub(in crate::daemon) struct CodeIndexSemanticEvaluationPublicationLeaseV1 {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// A cold-mount reservation publishes no runtime. Its sole authority is to
/// make one caller open a canonical root while followers wait to re-read the
/// mounted runtime that caller may publish.
struct ColdMountReservationSlotV1 {
    completion: tokio::sync::watch::Sender<()>,
    cancellation: tokio::sync::watch::Sender<()>,
    cancelled: AtomicBool,
    retired: AtomicBool,
    completed: AtomicBool,
}

impl ColdMountReservationSlotV1 {
    fn cancel(&self, retiring: bool) {
        if retiring {
            self.retired.store(true, Ordering::Release);
        }
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancellation.send_replace(());
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
}

struct ColdMountReservationV1 {
    project_root: PathBuf,
    slot: Arc<ColdMountReservationSlotV1>,
    reservations: Arc<Mutex<BTreeMap<PathBuf, Arc<ColdMountReservationSlotV1>>>>,
}

impl Drop for ColdMountReservationV1 {
    fn drop(&mut self) {
        let mut reservations = self
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_reservation = reservations
            .get(&self.project_root)
            .is_some_and(|current| Arc::ptr_eq(current, &self.slot));
        if owns_reservation {
            if !self.slot.is_retired() {
                reservations.remove(&self.project_root);
            }
            self.slot.completed.store(true, Ordering::Release);
            self.slot.completion.send_replace(());
        }
    }
}

enum ColdMountAdmissionV1 {
    Owner(ColdMountReservationV1),
    Follower(tokio::sync::watch::Receiver<()>),
}

/// One exact worktree's pending worker wake. `micros == 0` means no pending
/// arrival, and every nonzero arrival is held by one nonzero owner token.
struct PendingWakeStateV1 {
    micros: u64,
    trigger: u64,
    owner: u64,
    next_owner: u64,
}

/// The single synchronization authority for one worktree's coalesced wake.
/// The state lock makes timestamp, trigger, and claim ownership one
/// linearizable transition: no producer can arrive between a claim's owner
/// release and its marker release.
struct PendingWakeV1 {
    state: Mutex<PendingWakeStateV1>,
    #[cfg(test)]
    drop_gate: Mutex<Option<Arc<PendingWakeDropGateTestV1>>>,
}

impl Default for PendingWakeV1 {
    fn default() -> Self {
        Self {
            state: Mutex::new(PendingWakeStateV1::default()),
            #[cfg(test)]
            drop_gate: Mutex::new(None),
        }
    }
}

impl PendingWakeV1 {
    #[cfg(test)]
    fn note_foreign_wake_attempt_for_test(&self) {
        let gate = self
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(gate) = gate
            && gate.drop_reached.load(Ordering::Acquire)
        {
            gate.foreign_attempted.store(true, Ordering::Release);
            gate.foreign_entered.notify_waiters();
        }
    }

    #[cfg(test)]
    fn pause_claim_drop_for_test(&self) {
        let gate = self
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(gate) = gate {
            gate.drop_reached.store(true, Ordering::Release);
            gate.drop_entered.notify_waiters();
            let mut released = gate
                .drop_released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = gate
                    .drop_release
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

impl Default for PendingWakeStateV1 {
    fn default() -> Self {
        Self {
            micros: 0,
            trigger: 0,
            owner: 0,
            next_owner: 1,
        }
    }
}

impl PendingWakeStateV1 {
    fn next_owner(&mut self) -> u64 {
        let owner = self.next_owner;
        self.next_owner = self.next_owner.wrapping_add(1);
        if self.next_owner == 0 {
            self.next_owner = 1;
        }
        owner
    }
}

/// Owns one exact pending wake marker until worker dispatch succeeds or the
/// request is cancelled/rejected. Dropping a claim releases its owner and
/// marker under the same state lock, so it cannot erase a foreign wake.
struct PendingWakeClaimV1 {
    pending_wake: Arc<PendingWakeV1>,
    claimed_micros: u64,
    owner: u64,
    settled: bool,
}

impl PendingWakeClaimV1 {
    fn claim(pending_wake: Arc<PendingWakeV1>) -> Option<Self> {
        let mut state = pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.micros != 0 {
            return None;
        }
        let claimed_micros = u64::try_from(now_micros().0).unwrap_or(u64::MAX);
        let owner = state.next_owner();
        state.micros = claimed_micros;
        state.owner = owner;
        drop(state);
        Some(Self {
            pending_wake,
            claimed_micros,
            owner,
            settled: false,
        })
    }

    fn settle(mut self) {
        self.settled = true;
    }
}

impl Drop for PendingWakeClaimV1 {
    fn drop(&mut self) {
        if !self.settled {
            let mut state = self
                .pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            #[cfg(test)]
            self.pending_wake.pause_claim_drop_for_test();
            if state.owner == self.owner && state.micros == self.claimed_micros {
                state.micros = 0;
                state.trigger = 0;
                state.owner = 0;
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct CodeIndexSchedulerRegistryV1 {
    pub(super) max_worktrees: usize,
    /// Retained process-memory sampler handle; observed only by tests today.
    pub(super) _resident_memory: Arc<resident_memory::ProcessResidentMemoryV1>,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub(super) mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    /// Owners whose project was retired (remote deletion, replacement) but whose
    /// reconcile task has not finished draining. A root parked here must never
    /// re-mount: a fresh owner would race the dying one over the same store.
    pub(super) retiring: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    /// Exact roots currently opening a scheduler. This contains no runtime;
    /// followers wake and resolve through `mounted` after the owner settles.
    cold_mount_reservations: Arc<Mutex<BTreeMap<PathBuf, Arc<ColdMountReservationSlotV1>>>>,
    background_reconcile_admission: Arc<tokio::sync::Semaphore>,
    serving_generation_installation_tokens: Arc<AtomicU64>,
    generation_publications: tokio::sync::broadcast::Sender<CodeIndexGenerationPublishedV1>,
    cadence_telemetry: Arc<Mutex<CodeIndexCadenceTelemetryV1>>,
    activations: Arc<Mutex<BTreeMap<ManifestDigest, Weak<super::CodeIndexActivationV1>>>>,
    test_attribution_authorities: Arc<
        RwLock<
            BTreeMap<
                PathBuf,
                (
                    CodeGenerationId,
                    crate::code_index::production::PublishedGenerationTestAttributionAuthorityV1,
                ),
            >,
        >,
    >,
}

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) fn register_activation(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        activation: &Arc<super::CodeIndexActivationV1>,
    ) -> bool {
        if scope.validate().is_err() {
            return false;
        }
        if activation.identity().is_none() {
            return true;
        }
        if !activation.authorizes_scope(scope) {
            return false;
        }
        let mut activations = self
            .activations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        activations.retain(|_, activation| activation.strong_count() > 0);
        let scope_digest = scope.scope_digest.clone();
        let registered = Arc::downgrade(activation);
        activations.insert(scope_digest.clone(), registered.clone());
        drop(activations);
        let activations = Arc::clone(&self.activations);
        activation.install_retirement(Box::new(move || {
            let mut activations = activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if activations
                .get(&scope_digest)
                .is_some_and(|current| Weak::ptr_eq(current, &registered))
            {
                activations.remove(&scope_digest);
            }
        }));
        true
    }

    fn activate_for_scope(&self, scope: &tracedecay_application::ResolvedScope) -> bool {
        let activation = {
            let mut activations = self
                .activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let activation = activations.get(&scope.scope_digest).and_then(Weak::upgrade);
            if activation
                .as_ref()
                .is_none_or(|activation| !activation.authorizes_scope(scope))
            {
                activations.remove(&scope.scope_digest);
                None
            } else {
                activation
            }
        };
        activation.is_some_and(|activation| activation.activate())
    }

    #[cfg(test)]
    pub(super) fn activation_count(&self) -> usize {
        self.activations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(super) async fn pause_next_cold_mount_before_final_commit(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (released, release) = tokio::sync::oneshot::channel();
        let mut gate = cold_mount_final_commit_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gate.is_none(),
            "only one cold mount final-commit gate may be armed at a time"
        );
        *gate = Some(ColdMountFinalCommitGateV1 {
            project_root,
            entered,
            release,
        });
        (entered_observed, released)
    }

    #[cfg(test)]
    async fn wait_for_cold_mount_final_commit_gate(project_root: &Path) {
        let gate = {
            let mut armed = cold_mount_final_commit_gate()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches_root = armed
                .as_ref()
                .is_some_and(|gate| gate.project_root == project_root);
            if matches_root { armed.take() } else { None }
        };
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }

    #[cfg(test)]
    pub(super) async fn observe_next_existing_semantic_schedule_replacement(
        &self,
        project_root: PathBuf,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let mut gate = existing_semantic_schedule_replacement_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gate.is_none(),
            "only one existing semantic schedule replacement gate may be armed at a time"
        );
        *gate = Some(ExistingSemanticScheduleReplacementGateV1 {
            project_root,
            entered,
        });
        entered_observed
    }

    #[cfg(test)]
    fn observe_existing_semantic_schedule_replacement(project_root: &Path) {
        let gate = {
            let mut armed = existing_semantic_schedule_replacement_gate()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches_root = armed
                .as_ref()
                .is_some_and(|gate| gate.project_root == project_root);
            if matches_root { armed.take() } else { None }
        };
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
        }
    }

    /// Construct a registry with an explicit background-reconcile permit count so
    /// tests can deterministically exercise the bounded-admission behavior
    /// (parallelism across distinct stores vs. serialization at a bound of one)
    /// independent of the host's core count.
    #[cfg(test)]
    pub(super) fn with_background_reconcile_permits(max_worktrees: usize, permits: usize) -> Self {
        let mut registry = Self::new(max_worktrees);
        registry.background_reconcile_admission = Arc::new(tokio::sync::Semaphore::new(permits));
        registry
    }

    /// The bounded background-reconcile admission, so a test can occupy it and
    /// hold the worker at its dequeue point while asserting on the pending wake.
    #[cfg(test)]
    pub(super) fn background_reconcile_admission(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.background_reconcile_admission)
    }

    /// Test-only observation of an exact mounted worktree's active owner pass.
    #[cfg(test)]
    pub(super) async fn reconcile_in_progress_for_test(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let reconcile_in_progress = self
            .mounted
            .lock()
            .await
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.reconcile_in_progress));
        reconcile_in_progress
            .is_some_and(|reconcile_in_progress| reconcile_in_progress.load(Ordering::Acquire) != 0)
    }

    #[cfg(test)]
    pub(super) fn install_cold_mount_admission_barrier(&self, project_root: &Path, callers: usize) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let barrier = Arc::new(tokio::sync::Barrier::new(callers));
        let replaced = cold_mount_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, barrier);
        assert!(replaced.is_none(), "cold-mount barrier already installed");
    }

    #[cfg(test)]
    pub(super) fn install_cold_mount_post_check_gate(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let replaced = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root,
                Arc::new(ColdMountPostCheckTestControlV1 {
                    reached: AtomicBool::new(false),
                    entered: tokio::sync::Notify::new(),
                    release: tokio::sync::Notify::new(),
                }),
            );
        assert!(
            replaced.is_none(),
            "cold-mount post-check gate already installed"
        );
    }

    #[cfg(test)]
    pub(super) async fn wait_for_cold_mount_post_check(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let control = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .cloned()
            .expect("cold-mount post-check gate");
        let entered = control.entered.notified();
        if !control.reached.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub(super) fn release_cold_mount_post_check(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .expect("cold-mount post-check gate")
            .release
            .notify_one();
    }

    #[cfg(test)]
    pub(super) fn install_cold_mount_open_gate(&self, project_root: &Path) {
        Self::install_cold_mount_open_control(project_root, true);
    }

    #[cfg(test)]
    pub(super) fn install_cold_mount_open_observer(&self, project_root: &Path) {
        Self::install_cold_mount_open_control(project_root, false);
    }

    #[cfg(test)]
    fn install_cold_mount_open_control(project_root: &Path, blocks_open: bool) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let replaced = cold_mount_open_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root,
                Arc::new(ColdMountOpenTestControlV1::new(blocks_open)),
            );
        assert!(
            replaced.is_none(),
            "cold-mount open control already installed"
        );
    }

    #[cfg(test)]
    pub(super) async fn wait_for_cold_mount_open_events(&self, project_root: &Path, events: usize) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut changed = control.changed.subscribe();
        loop {
            let observed = control
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len();
            if observed >= events {
                return;
            }
            let _ = changed.changed().await;
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_for_cold_mount_follower(&self, project_root: &Path) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut changed = control.changed.subscribe();
        loop {
            if control.followers.load(Ordering::Acquire) != 0 {
                return;
            }
            let _ = changed.changed().await;
        }
    }

    #[cfg(test)]
    pub(super) fn release_cold_mount_open_gate(&self, project_root: &Path) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut released = control
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        control.release.notify_all();
    }

    #[cfg(test)]
    pub(super) fn cold_mount_open_events(&self, project_root: &Path) -> Vec<ColdMountOpenEventV1> {
        Self::cold_mount_open_control_for_test(project_root)
            .expect("cold-mount open control")
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(super) fn subscribe_cold_mount_cancellation(
        &self,
        project_root: &Path,
    ) -> Option<tokio::sync::watch::Receiver<()>> {
        let project_root = project_root.canonicalize().ok()?;
        self.cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .map(|slot| slot.cancellation.subscribe())
    }

    #[cfg(test)]
    pub(super) fn install_query_admission_barrier(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        callers: usize,
    ) {
        let control = Arc::new(QueryAdmissionTestControlV1 {
            lookup_gate: tokio::sync::Mutex::new(()),
            rendezvous: tokio::sync::Barrier::new(callers),
            pauses_after_claim: AtomicBool::new(false),
            claim_reached: AtomicBool::new(false),
            claim_entered: tokio::sync::Notify::new(),
            claim_release: tokio::sync::Notify::new(),
        });
        let replaced = query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.worktree_id.clone(), control);
        assert!(
            replaced.is_none(),
            "query-admission barrier already installed"
        );
    }

    #[cfg(test)]
    pub(super) fn install_query_claim_gate(&self, scope: &tracedecay_application::ResolvedScope) {
        let control = Arc::new(QueryAdmissionTestControlV1 {
            lookup_gate: tokio::sync::Mutex::new(()),
            rendezvous: tokio::sync::Barrier::new(1),
            pauses_after_claim: AtomicBool::new(true),
            claim_reached: AtomicBool::new(false),
            claim_entered: tokio::sync::Notify::new(),
            claim_release: tokio::sync::Notify::new(),
        });
        let replaced = query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.worktree_id.clone(), control);
        assert!(replaced.is_none(), "query-claim gate already installed");
    }

    #[cfg(test)]
    pub(super) async fn wait_for_query_claim(&self, scope: &tracedecay_application::ResolvedScope) {
        let control = Self::query_admission_control_for_test(scope).expect("query-claim gate");
        let entered = control.claim_entered.notified();
        if !control.claim_reached.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub(super) fn release_query_claim(&self, scope: &tracedecay_application::ResolvedScope) {
        Self::query_admission_control_for_test(scope)
            .expect("query-claim gate")
            .claim_release
            .notify_one();
    }

    /// Reserve one cold open for an exact canonical root. A follower must
    /// re-resolve `mounted` after completion because a failed or cancelled
    /// owner publishes no runtime.
    fn admit_cold_mount(
        &self,
        project_root: &Path,
        mounted_worktrees: usize,
    ) -> Result<ColdMountAdmissionV1, CodeIndexSchedulerErrorV1> {
        let mut reservations = self
            .cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Shutdown closes this same semaphore before it waits for outstanding
        // reservations. Checking while the reservation lock is held is the
        // admission linearization point: no caller that observed it open
        // earlier can reserve a new cold open after close.
        if self.background_reconcile_admission.is_closed() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler is shutting down".to_owned(),
            ));
        }
        if let Some(slot) = reservations.get(project_root) {
            return if slot.is_retired() {
                Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler owner is still retiring".to_owned(),
                ))
            } else {
                Ok(ColdMountAdmissionV1::Follower(slot.completion.subscribe()))
            };
        }
        if mounted_worktrees.saturating_add(reservations.len()) >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        let (completion, _) = tokio::sync::watch::channel(());
        let (cancellation, _) = tokio::sync::watch::channel(());
        let slot = Arc::new(ColdMountReservationSlotV1 {
            completion,
            cancellation,
            cancelled: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            completed: AtomicBool::new(false),
        });
        reservations.insert(project_root.to_path_buf(), Arc::clone(&slot));
        Ok(ColdMountAdmissionV1::Owner(ColdMountReservationV1 {
            project_root: project_root.to_path_buf(),
            slot,
            reservations: Arc::clone(&self.cold_mount_reservations),
        }))
    }

    fn cancel_cold_mount_reservations(&self) {
        let reservations = self
            .cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for slot in reservations.values() {
            slot.cancel(false);
        }
    }

    fn cold_mount_reservation_completions(&self) -> Vec<tokio::sync::watch::Receiver<()>> {
        self.cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|slot| !slot.completed.load(Ordering::Acquire))
            .map(|slot| slot.completion.subscribe())
            .collect()
    }

    fn retire_cold_mount_reservations(
        &self,
        project_roots: &BTreeSet<PathBuf>,
    ) -> (
        Vec<(PathBuf, tokio::sync::watch::Receiver<()>)>,
        BTreeSet<PathBuf>,
    ) {
        let reservations = self
            .cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut waiting = Vec::new();
        let mut completed = BTreeSet::new();
        for root in project_roots {
            let Some(slot) = reservations.get(root) else {
                continue;
            };
            slot.cancel(true);
            if slot.completed.load(Ordering::Acquire) {
                completed.insert(root.clone());
            } else {
                waiting.push((root.clone(), slot.completion.subscribe()));
            }
        }
        (waiting, completed)
    }

    fn release_completed_retired_cold_mount_reservations(&self, project_roots: &BTreeSet<PathBuf>) {
        let mut reservations = self
            .cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reservations.retain(|root, slot| {
            !project_roots.contains(root)
                || !slot.is_retired()
                || !slot.completed.load(Ordering::Acquire)
        });
    }

    #[cfg(test)]
    async fn pause_cold_mount_admission_for_test(project_root: &Path) {
        let barrier = cold_mount_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }

    #[cfg(test)]
    async fn pause_cold_mount_after_outer_check_for_test(project_root: &Path) {
        let control = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned();
        let Some(control) = control else {
            return;
        };
        control.reached.store(true, Ordering::Release);
        control.entered.notify_waiters();
        control.release.notified().await;
    }

    #[cfg(test)]
    fn cold_mount_open_control_for_test(
        project_root: &Path,
    ) -> Option<Arc<ColdMountOpenTestControlV1>> {
        let project_root = project_root.canonicalize().ok()?;
        cold_mount_open_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .cloned()
    }

    #[cfg(test)]
    fn pause_cold_mount_open_for_test(project_root: &Path) {
        let Some(control) = Self::cold_mount_open_control_for_test(project_root) else {
            return;
        };
        control.record(ColdMountOpenEventV1::Started);
        if control.blocks_open {
            let mut released = control
                .released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = control
                    .release
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    #[cfg(test)]
    fn finish_cold_mount_open_for_test(project_root: &Path) {
        if let Some(control) = Self::cold_mount_open_control_for_test(project_root) {
            control.record(ColdMountOpenEventV1::Finished);
        }
    }

    #[cfg(test)]
    fn note_cold_mount_follower_for_test(project_root: &Path) {
        if let Some(control) = Self::cold_mount_open_control_for_test(project_root) {
            control.record_follower();
        }
    }

    #[cfg(test)]
    fn query_admission_control_for_test(
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<QueryAdmissionTestControlV1>> {
        query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope.worktree_id)
            .cloned()
    }

    #[cfg(test)]
    async fn pending_wake_for_scope_for_test(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<PendingWakeV1>> {
        let mounted = self.mounted.lock().await;
        {
            let mut matched = mounted.values().filter(|worktree| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            });
            let pending_wake = matched
                .next()
                .map(|worktree| Arc::clone(&worktree.pending_wake))?;
            matched.next().is_none().then_some(pending_wake)
        }
    }

    #[cfg(test)]
    pub(super) async fn install_pending_wake_drop_gate(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let mounted = self.mounted.lock().await;
        let pending_wake = mounted
            .values()
            .find(|worktree| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|worktree| Arc::clone(&worktree.pending_wake))
            .expect("mounted worktree");
        let replaced = pending_wake
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(Arc::new(PendingWakeDropGateTestV1::new()));
        assert!(
            replaced.is_none(),
            "pending-wake drop gate already installed"
        );
    }

    #[cfg(test)]
    async fn pending_wake_drop_gate_for_test(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Arc<PendingWakeDropGateTestV1> {
        let pending_wake = self
            .pending_wake_for_scope_for_test(scope)
            .await
            .expect("mounted worktree");
        pending_wake
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("pending-wake drop gate")
    }

    #[cfg(test)]
    pub(super) async fn wait_for_pending_wake_claim_drop(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        let entered = gate.drop_entered.notified();
        if !gate.drop_reached.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_for_foreign_pending_wake_attempt(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        let entered = gate.foreign_entered.notified();
        if !gate.foreign_attempted.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub(super) async fn release_pending_wake_claim_drop(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        let mut released = gate
            .drop_released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        gate.drop_release.notify_all();
    }

    /// The pending-wake slot for one exact scope's worktree, in unix micros;
    /// `0` means no wake is outstanding.
    #[cfg(test)]
    pub(super) async fn pending_wake_micros_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<u64> {
        let mounted = self.mounted.lock().await;
        mounted
            .values()
            .find(|worktree| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|worktree| {
                worktree
                    .pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
            })
    }

    /// Clear the pending-wake slot so a test starts from a known due window.
    #[cfg(test)]
    pub(super) async fn clear_pending_wake_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let mounted = self.mounted.lock().await;
        for worktree in mounted.values() {
            if worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
            {
                let mut pending_wake = worktree
                    .pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pending_wake.micros = 0;
                pending_wake.owner = 0;
                pending_wake.trigger = 0;
            }
        }
    }

    /// Drop the retained serving generation, reproducing a mount whose restore
    /// produced nothing servable.
    #[cfg(test)]
    pub(super) async fn clear_serving_generation_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) {
        let mounted = self.mounted.lock().await;
        for worktree in mounted.values() {
            if worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
            {
                let mut serving = worktree
                    .serving_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *serving = None;
                worktree
                    .serving_generation_epoch
                    .fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Atomically marks the exact current serving generation as owned by one
    /// branch publication. A subsequent serving-slot replacement invalidates
    /// this token before rollback can observe it.
    pub(in crate::daemon) async fn install_exact_serving_generation(
        &self,
        project_root: &Path,
        expected: &Arc<CodeIndexPublishedGenerationV1>,
    ) -> ServingGenerationInstallationOutcomeV1 {
        let Ok(project_root) = project_root.canonicalize() else {
            return ServingGenerationInstallationOutcomeV1::NoMatch;
        };
        let (serving_generation, serving_epoch, installation_slot) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return ServingGenerationInstallationOutcomeV1::NoMatch;
            };
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.serving_generation_epoch),
                Arc::clone(&worktree.serving_generation_installation),
            )
        };
        let serving = serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(current) = serving.as_ref() else {
            return ServingGenerationInstallationOutcomeV1::NoMatch;
        };
        if !Arc::ptr_eq(&current.generation, expected) {
            return ServingGenerationInstallationOutcomeV1::NoMatch;
        }
        let serving_epoch = serving_epoch.load(Ordering::Acquire);
        let token = match self.serving_generation_installation_tokens.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_add(1),
        ) {
            Ok(token) => token,
            Err(_) => return ServingGenerationInstallationOutcomeV1::NoMatch,
        };
        let claim = ServingGenerationInstallationClaimV1 {
            token,
            serving_epoch,
            generation_id: current.generation().manifest().generation_id.clone(),
        };
        let mut active_installation = installation_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active_installation
            .as_ref()
            .is_some_and(|existing| existing.serving_epoch == serving_epoch)
        {
            return ServingGenerationInstallationOutcomeV1::NoMatch;
        }
        *active_installation = Some(claim.clone());
        drop(active_installation);
        ServingGenerationInstallationOutcomeV1::Installed(ServingGenerationInstallationV1 {
            claim,
            active_installation: installation_slot,
        })
    }

    /// Completes an exact serving-slot installation after its matching branch
    /// metadata CAS commits. A no-match means a foreign publication replaced
    /// the slot, so the caller must roll its metadata back without clearing
    /// the foreign serving generation.
    pub(in crate::daemon) async fn commit_serving_generation_installation(
        &self,
        project_root: &Path,
        installation: ServingGenerationInstallationV1,
    ) -> ServingGenerationRollbackOutcomeV1 {
        self.resolve_serving_generation_installation(project_root, &installation.claim, false)
            .await
    }

    /// Retires the serving generation only when this operation's metadata
    /// rollback succeeded and its exact installation token is still current.
    #[cfg(test)]
    pub(in crate::daemon) async fn retire_owned_serving_generation(
        &self,
        project_root: &Path,
        installation: ServingGenerationInstallationV1,
    ) -> ServingGenerationRollbackOutcomeV1 {
        self.resolve_serving_generation_installation(project_root, &installation.claim, true)
            .await
    }

    async fn resolve_serving_generation_installation(
        &self,
        project_root: &Path,
        installation: &ServingGenerationInstallationClaimV1,
        retire: bool,
    ) -> ServingGenerationRollbackOutcomeV1 {
        let Ok(project_root) = project_root.canonicalize() else {
            return ServingGenerationRollbackOutcomeV1::NoMatch;
        };
        let (serving_generation, serving_epoch, active_installation) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return ServingGenerationRollbackOutcomeV1::NoMatch;
            };
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.serving_generation_epoch),
                Arc::clone(&worktree.serving_generation_installation),
            )
        };
        let mut serving = serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if serving_epoch.load(Ordering::Acquire) != installation.serving_epoch
            || serving.as_ref().is_none_or(|current| {
                current.generation().manifest().generation_id != installation.generation_id
            })
        {
            return ServingGenerationRollbackOutcomeV1::NoMatch;
        }
        let mut active_installation = active_installation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active_installation.as_ref() != Some(installation) {
            return ServingGenerationRollbackOutcomeV1::NoMatch;
        }
        *active_installation = None;
        if retire {
            *serving = None;
            serving_epoch.fetch_add(1, Ordering::AcqRel);
        }
        ServingGenerationRollbackOutcomeV1::Cleared
    }

    fn pack_trigger(trigger: CodeIndexCadenceTriggerV1) -> u64 {
        match trigger {
            CodeIndexCadenceTriggerV1::Mount => 1,
            CodeIndexCadenceTriggerV1::HookHint => 2,
            CodeIndexCadenceTriggerV1::Overflow => 3,
            CodeIndexCadenceTriggerV1::QueryAdmission => 4,
            CodeIndexCadenceTriggerV1::BusyFollowUp => 5,
            CodeIndexCadenceTriggerV1::GitWatcher => 6,
        }
    }

    fn unpack_trigger(packed: u64) -> CodeIndexCadenceTriggerV1 {
        match packed {
            2 => CodeIndexCadenceTriggerV1::HookHint,
            3 => CodeIndexCadenceTriggerV1::Overflow,
            4 => CodeIndexCadenceTriggerV1::QueryAdmission,
            5 => CodeIndexCadenceTriggerV1::BusyFollowUp,
            6 => CodeIndexCadenceTriggerV1::GitWatcher,
            _ => CodeIndexCadenceTriggerV1::Mount,
        }
    }

    fn note_wake(
        pending_wake: &PendingWakeV1,
        wake: &tokio::sync::Notify,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let wake_micros = u64::try_from(now_micros().0).unwrap_or(u64::MAX);
        #[cfg(test)]
        pending_wake.note_foreign_wake_attempt_for_test();
        let mut state = pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.owner = state.next_owner();
        if state.micros == 0 {
            state.micros = wake_micros;
        }
        state.trigger = Self::pack_trigger(trigger);
        drop(state);
        wake.notify_one();
    }

    /// Claim the pending wake as one reconcile's arrival, at the instant the
    /// scheduler dequeues it.
    ///
    /// A reconcile with no pending wake — a follow-up pass draining work an
    /// earlier wake already claimed — has no attributable arrival. Reporting the
    /// dequeue or terminal instant instead would publish a fabricated zero queue
    /// delay, so the absence stays typed.
    fn take_pending_arrival(
        pending_wake: &PendingWakeV1,
        default_trigger: CodeIndexCadenceTriggerV1,
    ) -> (CodeIndexArrivalV1, CodeIndexCadenceTriggerV1) {
        let (wake_micros, packed_trigger) = {
            let mut state = pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let wake_micros = state.micros;
            let packed_trigger = state.trigger;
            state.micros = 0;
            state.trigger = 0;
            state.owner = 0;
            (wake_micros, packed_trigger)
        };
        if wake_micros == 0 {
            return (CodeIndexArrivalV1::Unavailable, default_trigger);
        }
        let trigger = Self::unpack_trigger(packed_trigger);
        match i64::try_from(wake_micros) {
            Ok(wake_micros) => (CodeIndexArrivalV1::Observed { wake_micros }, trigger),
            // An out-of-range clock reading is an unobserved arrival, not an
            // arrival equal to the terminal instant.
            Err(_) => (CodeIndexArrivalV1::Unavailable, trigger),
        }
    }

    /// Return a claimed arrival to the pending slot when the reconcile produced
    /// no receipt, keeping the earliest pending arrival so the wait a wake
    /// really took is never shortened by a failed attempt.
    fn restore_pending_arrival(
        pending_wake: &PendingWakeV1,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let Some(wake_micros) = arrival.wake_micros() else {
            return;
        };
        let Ok(wake_micros) = u64::try_from(wake_micros) else {
            return;
        };
        let mut state = pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A wake that arrived while this pass ran is newer, so the restored
        // arrival remains the earliest and stays authoritative.
        if state.micros != 0 && state.micros <= wake_micros {
            return;
        }
        state.owner = state.next_owner();
        state.micros = wake_micros;
        state.trigger = Self::pack_trigger(trigger);
    }

    /// Returns the pass's service time so the caller can attach the same
    /// measurement to the canonical index-lifecycle observation.
    fn record_reconcile_receipt(
        telemetry: &Mutex<CodeIndexCadenceTelemetryV1>,
        project_root: PathBuf,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
        started_micros: i64,
        outcome: &CodeIndexReconcileOutcomeV1,
    ) -> u64 {
        let ready_micros = now_micros().0;
        let (cadence_outcome, overflow_reconciled) = match outcome {
            CodeIndexReconcileOutcomeV1::Published(evidence) => (
                CodeIndexCadenceOutcomeV1::Published {
                    generation_id: evidence.generation_id.clone(),
                    reextracted_files: evidence.reextracted_files,
                    changed_chunks: evidence.changed_chunks,
                    reused_chunks: evidence.reused_chunks,
                },
                evidence.overflow_reconciled,
            ),
            CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                snapshot_content_identity,
                overflow_reconciled,
            }) => (
                CodeIndexCadenceOutcomeV1::Noop {
                    snapshot_content_identity: snapshot_content_identity.clone(),
                },
                *overflow_reconciled,
            ),
        };
        let receipt = CodeIndexEventToReadyReceiptV1::new(
            project_root,
            trigger,
            arrival,
            started_micros,
            ready_micros,
            cadence_outcome,
            overflow_reconciled,
        );
        // A successful publication is the terminal outcome operators need to see
        // to know a rebuild window actually closed, so it is `info`, not `debug`:
        // the cadence receipt below is debug-level and was invisible in the
        // journal during the live search outage. Identifiers and counters only —
        // no project path.
        if let CodeIndexReconcileOutcomeV1::Published(evidence) = outcome {
            tracing::info!(
                event = "code_index_generation_published",
                generation_id = evidence.generation_id.as_str(),
                reextracted_files = evidence.reextracted_files,
                changed_chunks = evidence.changed_chunks,
                service_micros = receipt.service_micros(),
                "code-index published a new generation"
            );
        }
        // Bounded, redacted cadence observability: labels and durations only.
        // The project root stays out of telemetry.
        tracing::debug!(
            event = "code_index_event_to_ready",
            trigger = receipt.trigger.label(),
            outcome = receipt.outcome_label(),
            arrival = receipt.arrival.label(),
            queue_delay_micros = ?receipt.queue_delay_micros(),
            service_micros = receipt.service_micros(),
            event_to_ready_micros = ?receipt.event_to_ready_micros(),
            overflow_reconciled = receipt.overflow_reconciled,
            "code-index reconcile reached a terminal outcome"
        );
        // `service_micros` is clamped non-negative by construction, so the
        // widening cast is exact.
        let service_micros = receipt.service_micros().max(0) as u64;
        let mut telemetry = telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        telemetry.record(receipt);
        // Emit the aggregate exactly when a percentile first becomes eligible,
        // so aggregate lines stay bounded to a few per ring cycle.
        if let Some(percentile) = newly_eligible_percentile(telemetry.latency_sample_count()) {
            let read_model = telemetry.read_model();
            tracing::debug!(
                event = "code_index_cadence_read_model",
                newly_eligible = percentile,
                retained_count = read_model.retained_count,
                capacity = read_model.capacity,
                latency_sample_count = read_model.latency_sample_count,
                arrival_unavailable_count = read_model.arrival_unavailable_count,
                published_count = read_model.published_count,
                noop_count = read_model.noop_count,
                event_to_ready_p50_micros = ?read_model.event_to_ready_micros.p50.value,
                event_to_ready_p95_micros = ?read_model.event_to_ready_micros.p95.value,
                event_to_ready_p99_micros = ?read_model.event_to_ready_micros.p99.value,
                queue_delay_p50_micros = ?read_model.queue_delay_micros.p50.value,
                queue_delay_p95_micros = ?read_model.queue_delay_micros.p95.value,
                queue_delay_p99_micros = ?read_model.queue_delay_micros.p99.value,
                "code-index cadence percentile became eligible"
            );
        }
        service_micros
    }

    /// Latest completed event-to-ready receipt for this registry, if any.
    #[cfg(test)]
    pub(in crate::daemon) fn latest_event_to_ready_receipt(
        &self,
    ) -> Option<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest()
            .cloned()
    }

    /// Every retained event-to-ready receipt, oldest first.
    #[cfg(test)]
    pub(in crate::daemon) fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts()
            .cloned()
            .collect()
    }

    /// Bounded truthful cadence read model over the retained receipts.
    ///
    /// Percentiles are withheld until the retained population reaches the floor
    /// each one declares, and receipts with an unobservable arrival are reported
    /// as unavailable rather than counted as zero-latency samples.
    #[cfg(test)]
    pub(in crate::daemon) fn cadence_read_model(&self) -> CodeIndexCadenceReadModelV1 {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_model()
    }

    pub(crate) fn subscribe_generation_publications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1> {
        self.generation_publications.subscribe()
    }

    fn publish_generation(
        sender: &tokio::sync::broadcast::Sender<CodeIndexGenerationPublishedV1>,
        project_root: PathBuf,
        evidence: &CodeIndexPublishEvidenceV1,
    ) {
        let _ = sender.send(CodeIndexGenerationPublishedV1 {
            project_root,
            repository_id: evidence.repository_id.clone(),
            generation_id: evidence.generation_id.clone(),
            snapshot_content_identity: evidence.snapshot_content_identity.clone(),
            observation_time_micros: now_micros().0,
        });
    }

    #[cfg(test)]
    pub(in crate::daemon) fn open_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<CodeIndexWorktreeSchedulerV1, CodeIndexSchedulerErrorV1> {
        if self.max_worktrees == 0 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is zero".to_owned(),
            ));
        }
        CodeIndexWorktreeSchedulerV1::open(
            project_id,
            project_root,
            store_root,
            Arc::clone(&self.byte_pool),
        )
    }

    #[cfg(test)]
    pub(in crate::daemon) fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

    #[cfg(test)]
    pub async fn memory_stats(&self) -> CodeIndexSchedulerMemoryStatsV1 {
        let mounted = self.mounted.lock().await;
        CodeIndexSchedulerMemoryStatsV1 {
            mounted_worktrees: u64::try_from(mounted.len()).unwrap_or(u64::MAX),
            reconciling_worktrees: u64::try_from(
                mounted
                    .values()
                    .filter(|worktree| worktree.reconcile_in_progress.load(Ordering::Acquire) != 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            retained_generation_encoded_bytes: mounted.values().fold(0_u64, |total, worktree| {
                total.saturating_add(
                    worktree
                        ._active_generation_encoded_bytes
                        .load(Ordering::Acquire),
                )
            }),
        }
    }

    pub(in crate::daemon) async fn mount_worktree_with_graph_runtime(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        graph_runtime: Arc<
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
        >,
        project_database: Arc<crate::db::Database>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Persistent {
                runtime: graph_runtime,
                project_database,
            },
        )
        .await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn mount_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Memory,
        )
        .await
    }

    async fn replace_existing_semantic_schedule(
        &self,
        project_root: &Path,
        scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
        serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
        project_id: ProjectId,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        // Reconcile holds this mutex; wait in the blocking pool so remount
        // never parks a runtime worker or admission for other lanes.
        let incumbent = Arc::clone(&scheduler);
        tokio::task::spawn_blocking(move || {
            let mut scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if scheduler.project_id() != &project_id {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "mounted worktree belongs to a different project identity".to_owned(),
                ));
            }
            scheduler.replace_semantic_schedule_hook(semantic_schedule);
            if let Some(latest) = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                let _ = scheduler.schedule_semantic_generation(latest.generation());
            }
            Ok(())
        })
        .await
        .map_err(|_error| {
            CodeIndexSchedulerErrorV1::SemanticSchedule("hook task failed".to_owned())
        })??;

        let retiring = self.retiring.lock().await;
        if retiring.contains_key(project_root) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner was retired while semantic schedule update waited; remount must retry"
                    .to_owned(),
            ));
        }
        let mounted = self.mounted.lock().await;
        if !mounted
            .get(project_root)
            .is_some_and(|current| Arc::ptr_eq(&current.scheduler, &incumbent))
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner changed while semantic schedule update waited; remount must retry"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    async fn mount_worktree_inner(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        graph_activation: CodeGraphActivationAuthorityV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        #[cfg(test)]
        Self::pause_cold_mount_admission_for_test(&project_root).await;
        let cold_mount_reservation = loop {
            if self.background_reconcile_admission.is_closed() {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler is shutting down".to_owned(),
                ));
            }
            #[cfg(test)]
            Self::pause_cold_mount_after_outer_check_for_test(&project_root).await;
            // A retiring owner still holds the store: admitting a fresh mount
            // here would race the dying reconcile task over the same physical
            // shard.
            let retiring = self.retiring.lock().await;
            if retiring.contains_key(&project_root) {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler owner is still retiring".to_owned(),
                ));
            }
            let mounted = self.mounted.lock().await;
            if let Some(existing) = mounted.get(&project_root) {
                let scheduler = Arc::clone(&existing.scheduler);
                let serving_generation = Arc::clone(&existing.serving_generation);
                drop(mounted);
                drop(retiring);
                #[cfg(test)]
                Self::observe_existing_semantic_schedule_replacement(&project_root);
                self.replace_existing_semantic_schedule(
                    &project_root,
                    scheduler,
                    serving_generation,
                    project_id,
                    semantic_schedule,
                )
                .await?;
                return Ok(false);
            }
            let admission = self.admit_cold_mount(&project_root, mounted.len())?;
            drop(mounted);
            drop(retiring);
            match admission {
                ColdMountAdmissionV1::Owner(reservation) => break reservation,
                ColdMountAdmissionV1::Follower(mut completion) => {
                    #[cfg(test)]
                    Self::note_cold_mount_follower_for_test(&project_root);
                    let _ = completion.changed().await;
                }
            }
        };
        // Keep CPU-bound cold-open identity setup off runtime workers.
        let scoped_store_root = super::scoped_code_index_store_root(&store_root, &project_root);
        let open_project_id = project_id.clone();
        let open_project_root = project_root.clone();
        let open_byte_pool = Arc::clone(&self.byte_pool);
        let open_semantic_schedule = semantic_schedule.clone();
        let (opened, cold_mount_reservation) = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            Self::pause_cold_mount_open_for_test(&open_project_root);
            let opened = CodeIndexWorktreeSchedulerV1::open(
                open_project_id,
                &open_project_root,
                scoped_store_root,
                open_byte_pool,
            );
            #[cfg(test)]
            Self::finish_cold_mount_open_for_test(&open_project_root);
            let mut opened = opened?;
            opened.replace_semantic_schedule_hook(open_semantic_schedule);
            Ok::<_, CodeIndexSchedulerErrorV1>((opened, cold_mount_reservation))
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!("code-index mount task failed: {error}"))
        })??;
        let repository_id = opened.identity().repository_id().clone();
        let worktree_id = opened.identity().worktree_id().clone();
        let reconcile_in_progress = opened.reconcile_in_progress();
        let active_generation_encoded_bytes = opened.active_generation_encoded_bytes();
        // Cold mount publishes only the exact route. The worker may seat a
        // complete identity-valid generation as stale serving before refresh
        // claims freshness; missing Git authority still leaves this empty.
        let serving_generation = Arc::new(RwLock::new(None));
        let serving_generation_epoch = Arc::new(AtomicU64::new(0));
        let serving_generation_installation = Arc::new(Mutex::new(None));
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let scheduler = Arc::new(Mutex::new(opened));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let ignored_dependency_admissions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_wake = Arc::new(PendingWakeV1::default());
        let index_observability =
            Arc::new(OnceLock::<super::observability::CodeIndexObservabilityV1>::new());
        let worker_index_observability = Arc::clone(&index_observability);
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_reconcile_in_progress = Arc::clone(&reconcile_in_progress);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_serving_generation_epoch = Arc::clone(&serving_generation_epoch);
        let worker_wake = Arc::clone(&wake);
        let worker_pending_wake = Arc::clone(&pending_wake);
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_semantic_evaluation_publication_gate =
            Arc::clone(&semantic_evaluation_publication_gate);
        let worker_background_reconcile_admission =
            Arc::clone(&self.background_reconcile_admission);
        let worker_generation_publications = self.generation_publications.clone();
        let worker_project_root = project_root.clone();
        let worker_project_id = project_id;
        let worker_repository_id = repository_id.clone();
        let worker_worktree_id = worktree_id.clone();
        let worker_graph_activation = graph_activation.clone();
        #[cfg(test)]
        Self::wait_for_cold_mount_final_commit_gate(&project_root).await;
        // Reacquire the lifecycle fences before publication. Once acquired,
        // worker spawn and insertion contain no await point, so cancellation
        // cannot leave a detached worker that was never made canonical.
        let retiring = self.retiring.lock().await;
        let mut mounted = self.mounted.lock().await;
        if retiring.contains_key(&project_root) || cold_mount_reservation.slot.is_retired() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner is still retiring".to_owned(),
            ));
        }
        if self.background_reconcile_admission.is_closed()
            || cold_mount_reservation.slot.is_cancelled()
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler is shutting down".to_owned(),
            ));
        }
        if let Some(existing) = mounted.get(&project_root) {
            // The scheduler Arc is the mounted owner's exact identity. It is
            // rechecked after the asynchronous update so a retirement or
            // replacement cannot turn this remount into a success for a
            // detached worker.
            let scheduler = Arc::clone(&existing.scheduler);
            let serving_generation = Arc::clone(&existing.serving_generation);
            drop(mounted);
            drop(retiring);
            #[cfg(test)]
            Self::observe_existing_semantic_schedule_replacement(&project_root);
            self.replace_existing_semantic_schedule(
                &project_root,
                scheduler,
                serving_generation,
                worker_project_id,
                semantic_schedule,
            )
            .await?;
            return Ok(false);
        }
        let at_capacity = mounted.len() >= self.max_worktrees;
        let entry = match mounted.entry(project_root) {
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler owner changed before final mount commit; remount must retry"
                        .to_owned(),
                ));
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                if at_capacity {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "code-index scheduler capacity is exhausted".to_owned(),
                    ));
                }
                entry
            }
        };
        let task = tokio::spawn(async move {
            // Bounded retry state for activating an already-sealed complete
            // generation. The sealed artifact is immutable and retryable, so a
            // retryable activation failure must not fall through into a
            // rebuild+reseal of an equivalent generation.
            let mut seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
            let mut next_seat_attempt_at: Option<Instant> = None;
            loop {
                worker_wake.notified().await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let _semantic_evaluation_publication =
                    worker_semantic_evaluation_publication_gate.lock().await;
                let Ok(_background_reconcile_admission) =
                    Arc::clone(&worker_background_reconcile_admission)
                        .acquire_owned()
                        .await
                else {
                    return;
                };
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let scheduler = Arc::clone(&worker_scheduler);
                let serving_generation = Arc::clone(&worker_serving_generation);
                let serving_generation_epoch = Arc::clone(&worker_serving_generation_epoch);
                // Cover wake claim through failed-arrival restoration so admission
                // never misreads in-flight owner work as plain unavailability.
                let _reconcile_pass =
                    super::ReconcilePassGuard::enter(&worker_reconcile_in_progress);
                // Admission is held: queue wait ends and service time begins.
                let started_micros = now_micros().0;
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake,
                    CodeIndexCadenceTriggerV1::Mount,
                );
                // Serve-during-refresh: seat the last complete compatible
                // generation before rebuild. A cancelled refresh or branch
                // split must not hide a sealed generation for the duration
                // of reconcile. Stale is truthful; do not mark_reconciled.
                let mut seat_retry_pending = false;
                if serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                {
                    let remount_scheduler = Arc::clone(&scheduler);
                    let remount = tokio::task::spawn_blocking(move || {
                        let mut scheduler = remount_scheduler
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let retained = scheduler.servable_retained_generation()?;
                        let replay_binding = scheduler.code_graph_replay_binding(
                            &retained.generation().manifest().generation_id,
                        );
                        Some((retained, replay_binding))
                    })
                    .await;
                    if let Ok(Some((retained, replay_binding))) = remount {
                        if next_seat_attempt_at.is_some_and(|at| Instant::now() < at) {
                            // A sealed complete generation exists and its
                            // activation is backing off; hold this pass so the
                            // scheduled retry activates the same artifact.
                            seat_retry_pending = true;
                        } else {
                            let activation = match replay_binding {
                                Ok(replay_binding) => {
                                    worker_graph_activation
                                        .activate(
                                            &worker_project_id,
                                            &worker_repository_id,
                                            &worker_worktree_id,
                                            retained.clone(),
                                            replay_binding,
                                            Arc::clone(&worker_shutting_down),
                                        )
                                        .await
                                }
                                Err(error) => Err(error),
                            };
                            match activation {
                                Err(error) => {
                                    let retryable = error.is_retryable_activation();
                                    tracing::warn!(
                                        event = "code_index_retained_seat_failed",
                                        path = "background_worker",
                                        retryable,
                                        error = %error,
                                        "code-index retained generation did not activate; refresh continues without stale serving"
                                    );
                                    if retryable {
                                        next_seat_attempt_at =
                                            Some(Instant::now() + seat_retry_backoff);
                                        let retry_wake = Arc::clone(&worker_wake);
                                        let retry_delay = seat_retry_backoff;
                                        tokio::spawn(async move {
                                            tokio::time::sleep(retry_delay).await;
                                            retry_wake.notify_one();
                                        });
                                        seat_retry_backoff = seat_retry_backoff
                                            .saturating_mul(2)
                                            .min(ACTIVATION_RETRY_BACKOFF_CEILING);
                                        seat_retry_pending = true;
                                    } else {
                                        next_seat_attempt_at = None;
                                        seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                                    }
                                }
                                Ok(()) => {
                                    next_seat_attempt_at = None;
                                    seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                                    let swap_scheduler = Arc::clone(&scheduler);
                                    let swap_serving = Arc::clone(&serving_generation);
                                    let swap_serving_epoch = Arc::clone(&serving_generation_epoch);
                                    let _ = tokio::task::spawn_blocking(move || {
                                        let scheduler = swap_scheduler
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                                        if scheduler
                                            .active_publication_matches(&retained)
                                            .unwrap_or(false)
                                        {
                                            let mut serving = swap_serving
                                                .write()
                                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                                            *serving = Some(retained.clone());
                                            swap_serving_epoch.fetch_add(1, Ordering::AcqRel);
                                            drop(serving);
                                            let _ = scheduler.schedule_semantic_generation(
                                                retained.generation(),
                                            );
                                        }
                                    })
                                    .await;
                                }
                            }
                        }
                    }
                }
                if seat_retry_pending {
                    // Rebuilding here would seal a duplicate of an artifact
                    // that only failed to activate. Restore the arrival so the
                    // next pass measures this wake's full queue wait, then let
                    // the scheduled retry wake re-attempt activation.
                    Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                    if worker_shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    continue;
                }
                let mut result = tokio::task::spawn_blocking(move || {
                    let mut scheduler = scheduler
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut result = scheduler.activate_or_reconcile();
                    // A terminal outcome may publish a newer complete generation;
                    // swap serving to that after graph activation below.
                    let mut latest = result
                        .as_ref()
                        .ok()
                        .and_then(|_| scheduler.latest_complete());
                    let replay_binding = latest.as_ref().map(|latest| {
                        scheduler.code_graph_replay_binding(
                            &latest.generation().manifest().generation_id,
                        )
                    });
                    let replay_binding = match replay_binding.transpose() {
                        Ok(binding) => binding,
                        Err(error) => {
                            result = Err(error);
                            latest = None;
                            None
                        }
                    };
                    (result, latest, replay_binding)
                })
                .await;
                if let Ok((Ok(_), Some(latest), Some(replay_binding))) = &result {
                    let activation = worker_graph_activation
                        .activate(
                            &worker_project_id,
                            &worker_repository_id,
                            &worker_worktree_id,
                            latest.clone(),
                            replay_binding.clone(),
                            Arc::clone(&worker_shutting_down),
                        )
                        .await;
                    if let Err(error) = activation {
                        // The generation just sealed is complete; a retryable
                        // activation failure arms the same seat backoff so the
                        // next passes retry this artifact instead of resealing.
                        if error.is_retryable_activation() {
                            next_seat_attempt_at = Some(Instant::now() + seat_retry_backoff);
                            let retry_wake = Arc::clone(&worker_wake);
                            let retry_delay = seat_retry_backoff;
                            tokio::spawn(async move {
                                tokio::time::sleep(retry_delay).await;
                                retry_wake.notify_one();
                            });
                            seat_retry_backoff = seat_retry_backoff
                                .saturating_mul(2)
                                .min(ACTIVATION_RETRY_BACKOFF_CEILING);
                        }
                        result = Ok((Err(error), None, None));
                    }
                }
                if let Ok((Ok(_), Some(latest), _)) = &result {
                    let scheduler = Arc::clone(&worker_scheduler);
                    let serving_generation = Arc::clone(&worker_serving_generation);
                    let serving_generation_epoch = Arc::clone(&worker_serving_generation_epoch);
                    let latest = latest.clone();
                    let serving_swap = tokio::task::spawn_blocking(move || {
                        let scheduler = scheduler
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !scheduler.active_publication_matches(&latest)? {
                            return Err(CodeIndexSchedulerErrorV1::PublicationConflict(
                                "the reconciled generation is no longer the active durable publication"
                                    .to_owned(),
                            ));
                        }
                        let mut serving = serving_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        *serving = Some(latest.clone());
                        serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
                        drop(serving);
                        let _ = scheduler.schedule_semantic_generation(latest.generation());
                        Ok::<_, CodeIndexSchedulerErrorV1>(())
                    })
                    .await;
                    match serving_swap {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => result = Ok((Err(error), None, None)),
                        Err(error) => {
                            result = Ok((
                                Err(CodeIndexSchedulerErrorV1::SemanticSchedule(format!(
                                    "serving-swap task failed: {error}"
                                ))),
                                None,
                                None,
                            ));
                        }
                    }
                }
                if let Ok((Ok(outcome), _, _)) = &result {
                    if let CodeIndexReconcileOutcomeV1::Published(evidence) = outcome {
                        Self::publish_generation(
                            &worker_generation_publications,
                            worker_project_root.clone(),
                            evidence,
                        );
                    }
                    let service_micros = Self::record_reconcile_receipt(
                        &worker_cadence_telemetry,
                        worker_project_root.clone(),
                        arrival,
                        trigger,
                        started_micros,
                        outcome,
                    );
                    if let Some(observability) = worker_index_observability.get() {
                        // The pending slot coalesces at most one waiting wake,
                        // so the queue behind this pass is empty or singular.
                        let queue_depth_bucket = {
                            let state = worker_pending_wake
                                .state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if state.micros == 0 {
                                tracedecay_domain::QueueDepthBucketV1::Zero
                            } else {
                                tracedecay_domain::QueueDepthBucketV1::OneToEight
                            }
                        };
                        observability
                            .record_reconcile_outcome(outcome, service_micros, queue_depth_bucket)
                            .await;
                    }
                } else {
                    // Surface bounded non-terminal failure without new project-path data.
                    match &result {
                        Ok((Err(error), _, _)) => tracing::warn!(
                            event = "code_index_reconcile_failed",
                            path = "background_worker",
                            error = %error,
                            "code-index background reconcile failed; the served generation stays stale"
                        ),
                        Err(error) => tracing::warn!(
                            event = "code_index_reconcile_failed",
                            path = "background_worker",
                            error = %error,
                            "code-index background reconcile task did not complete"
                        ),
                        Ok((Ok(_), _, _)) => {}
                    }
                    // Restore arrival so the next pass measures this wake's full queue wait.
                    Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                // The next coalesced hint wakes this worker after a contained panic.
                let _ = result;
            }
        });
        entry.insert(MountedCodeIndexWorktreeV1 {
            repository_id,
            worktree_id,
            query_authority: None,
            semantic_query_authority: None,
            query_activation_revision: None,
            query_activation_epoch: None,
            query_activation_transition_digest: None,
            query_activation_attempt: 0,
            query_activation_redundancy: None,
            semantic_vector_graph_provider: None,
            scheduler,
            serving_generation,
            serving_generation_epoch,
            serving_generation_installation,
            graph_activation,
            ignored_dependency_admissions,
            hints,
            wake: Arc::clone(&wake),
            epoch,
            pending_wake: Arc::clone(&pending_wake),
            index_observability,
            shutting_down,
            reconcile_in_progress,
            _active_generation_encoded_bytes: active_generation_encoded_bytes,
            semantic_evaluation_publication_gate,
            task,
        });
        // Until retained decode/truth verification completes, reads see warming
        // instead of serving unproven bytes.
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Mount);
        Ok(true)
    }

    /// Mount the accepted query profile and query/cursor key owner for one exact
    /// admitted scope. The authority cannot be inherited by another project,
    /// repository, worktree, or ref.
    pub(in crate::daemon) async fn mount_query_authority(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        authority: Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount query authority before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        if worktree.query_activation_revision.is_some() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "standalone query authority cannot replace a committed authority pair".to_owned(),
            ));
        }
        worktree.query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(())
    }

    /// Install the canonical Plan 26 observability lane for one mounted
    /// worktree. Installation is once per mount: a repeated install against
    /// the same mounted worktree keeps the incumbent lane, and a worktree that
    /// is not mounted is a typed error so the caller can log the absence.
    pub(in crate::daemon) async fn install_index_observability(
        &self,
        project_root: &Path,
        observability: super::observability::CodeIndexObservabilityV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot install index observability before its worktree".to_owned(),
            )
        })?;
        // A remount creates a fresh empty slot, so an ignored second set here
        // can only be a same-mount duplicate carrying the same project lane.
        let _ = worktree.index_observability.set(observability);
        Ok(())
    }

    /// The installed observability lane for one exact admitted scope, if the
    /// worktree is mounted and the lane was installed.
    pub(in crate::daemon) async fn index_observability_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::observability::CodeIndexObservabilityV1> {
        let mounted = self.mounted.try_lock().ok()?;
        let mut matched = None;
        for worktree in mounted.values() {
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                continue;
            }
            let Some(observability) = worktree.index_observability.get() else {
                continue;
            };
            if matched.is_some() {
                return None;
            }
            matched = Some(observability.clone());
        }
        matched
    }

    /// Install the core and optional semantic query routes as one committed
    /// configuration observation. The provider CAS is repeated while the
    /// mounted-worktree lock is held, so a delayed observer cannot publish a
    /// stale authority pair after a newer committed revision.
    pub(in crate::daemon) async fn begin_committed_query_activation(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        epoch: i64,
        result_revision: &ConfigurationRevisionId,
        transition_digest: &ManifestDigest,
        prepared_redundancy: &tracedecay_usecases::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
    ) -> Result<QueryActivationAttemptV1, CodeIndexSchedulerErrorV1> {
        if epoch <= 0 || prepared_redundancy.configuration_revision() != result_revision {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared redundancy revision does not match query activation".to_owned(),
            ));
        }
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot begin query activation before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query activation scope does not match the mounted worktree".to_owned(),
            ));
        }
        if let Some(desired_epoch) = worktree.query_activation_epoch {
            let advances = epoch > desired_epoch;
            let exact_retry = epoch == desired_epoch
                && worktree.query_activation_revision.as_ref() == Some(result_revision)
                && worktree.query_activation_transition_digest.as_ref() == Some(transition_digest)
                && worktree.query_activation_redundancy.as_ref() == Some(prepared_redundancy);
            if !advances && !exact_retry {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "query activation is older than the desired configuration fence".to_owned(),
                ));
            }
        }
        let activation =
            tracedecay_usecases::semantic_runtime::project_semantic_activation_gate(&project_root);
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        worktree.query_activation_attempt = worktree
            .query_activation_attempt
            .checked_add(1)
            .ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "query activation attempt sequence is exhausted".to_owned(),
                )
            })?;
        worktree.query_activation_revision = Some(result_revision.clone());
        worktree.query_activation_epoch = Some(epoch);
        worktree.query_activation_transition_digest = Some(transition_digest.clone());
        worktree.query_activation_redundancy = Some(prepared_redundancy.clone());
        worktree.semantic_query_authority = None;
        tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
            project_root,
            prepared_redundancy,
            false,
        );
        Ok(QueryActivationAttemptV1 {
            revision: result_revision.clone(),
            token: worktree.query_activation_attempt,
        })
    }

    pub(in crate::daemon) async fn install_committed_query_authorities(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        provider: &crate::daemon::query_authority_provider::DaemonQueryAuthorityProviderV1,
        prepared: crate::daemon::query_authority_provider::PreparedQueryActivationV1,
        semantic_authority: Option<Arc<super::semantic_query_runtime::SemanticQueryAuthorityV1>>,
        prepared_cache: Option<
            tracedecay_usecases::semantic_runtime::PreparedProductionSemanticCacheCommitV1,
        >,
        disabled_cache_generation: Option<&tracedecay_domain::VectorGenerationIdV1>,
        prepared_redundancy: tracedecay_usecases::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
        attempt: &QueryActivationAttemptV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        if prepared.scope() != scope {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared query activation scope does not match the committed scope".to_owned(),
            ));
        }
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot install query authorities before their worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation =
            tracedecay_usecases::semantic_runtime::project_semantic_activation_gate(&project_root);
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() != Some(&attempt.revision)
            || worktree.query_activation_attempt != attempt.token
            || prepared.configuration_revision() != &attempt.revision
            || prepared_redundancy.configuration_revision() != &attempt.revision
            || worktree.query_activation_redundancy.as_ref() != Some(&prepared_redundancy)
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared query activation attempt is no longer desired".to_owned(),
            ));
        }
        if let Some(prepared_cache) = prepared_cache {
            if !prepared_cache.commit() {
                worktree.semantic_query_authority = None;
                worktree.query_activation_revision =
                    Some(prepared.configuration_revision().clone());
                tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                    project_root.clone(),
                    &prepared_redundancy,
                    false,
                );
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "prepared semantic cache became stale before coherent installation".to_owned(),
                ));
            }
        } else if semantic_authority.is_none()
            && let Some(generation) = disabled_cache_generation
        {
            tracedecay_usecases::semantic_runtime::unbind_project_semantic_cache_if_current(
                &project_root,
                generation,
            );
        }
        if let Err(error) = provider.commit_prepared_activation(&prepared) {
            worktree.semantic_query_authority = None;
            worktree.query_activation_revision = Some(prepared.configuration_revision().clone());
            tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                project_root.clone(),
                &prepared_redundancy,
                false,
            );
            return Err(CodeIndexSchedulerErrorV1::Identity(error.to_string()));
        }
        tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
            project_root.clone(),
            &prepared_redundancy,
            semantic_authority.is_some(),
        );
        worktree.query_authority = Some((
            scope.scope_digest.clone(),
            Arc::clone(prepared.query_authority()),
        ));
        worktree.semantic_query_authority =
            semantic_authority.map(|authority| (scope.scope_digest.clone(), authority));
        worktree.query_activation_revision = Some(prepared.configuration_revision().clone());
        Ok(())
    }

    /// Revoke a failed committed transition without letting a delayed observer
    /// erase a different revision that already installed coherently.
    pub(in crate::daemon) async fn clear_failed_query_activation(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        cache_generation: Option<&tracedecay_domain::VectorGenerationIdV1>,
        failed_redundancy: tracedecay_usecases::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
        attempt: &QueryActivationAttemptV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot clear query authorities before their worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "failed query activation scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation =
            tracedecay_usecases::semantic_runtime::project_semantic_activation_gate(&project_root);
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() == Some(&attempt.revision)
            && worktree.query_activation_attempt == attempt.token
            && failed_redundancy.configuration_revision() == &attempt.revision
            && worktree.query_activation_redundancy.as_ref() == Some(&failed_redundancy)
        {
            worktree.semantic_query_authority = None;
            tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                project_root.clone(),
                &failed_redundancy,
                false,
            );
            if let Some(generation) = cache_generation {
                tracedecay_usecases::semantic_runtime::unbind_project_semantic_cache_if_current(
                    &project_root,
                    generation,
                );
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub(in crate::daemon) async fn query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<tracedecay_query::retrieval::QueryAuthorityV1>> {
        self.activate_for_scope(scope);
        let mounted = self.mounted.try_lock().ok()?;
        let mut matched = None;
        for worktree in mounted.values() {
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                continue;
            }
            let Some((_scope_digest, authority)) = &worktree.query_authority else {
                // Defensive only: real mounts key the registry and derive the
                // worktree ID from the same canonical root, so this identity
                // cannot have an authority-bearing sibling.
                continue;
            };
            if matched.is_some() {
                return None;
            }
            // Same worktree isolation as `latest_matches_scope_identity`: a
            // mid-session ref switch keeps the mounted ranking authority until
            // the route remounts. Exact digest is a remount key, not a reason
            // to deny search after HEAD moved.
            matched = Some(Arc::clone(authority));
        }
        matched
    }

    #[cfg(test)]
    pub(crate) async fn has_query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        self.query_authority_for_scope(scope).await.is_some()
    }

    #[cfg(test)]
    pub(crate) async fn query_authority_installation_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<(bool, bool, Option<ConfigurationRevisionId>)> {
        let mounted = self.mounted.lock().await;
        let mut matches = mounted.values().filter(|worktree| {
            worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
        });
        let worktree = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some((
            worktree
                .query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest == &scope.scope_digest),
            worktree
                .semantic_query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest == &scope.scope_digest),
            worktree.query_activation_revision.clone(),
        ))
    }

    /// Whether a worktree is currently mounted for `project_root`. Read-only
    /// map membership used by the Doctor code-index mount adapter to distinguish
    /// an unmounted worktree from a mounted-but-still-indexing one. Returns
    /// `false` when the path cannot be canonicalized (a path Doctor could never
    /// have mounted under).
    pub async fn is_worktree_mounted(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        self.mounted.lock().await.contains_key(&project_root)
    }

    /// Complete bounded snapshot of roots protected by a live mounted
    /// scheduler lease. Scope retention folds this into its revision-bound
    /// proof; returning every profile mount is deliberately conservative.
    pub(in crate::daemon) async fn scope_retention_mounted_roots(
        &self,
    ) -> Result<BTreeSet<PathBuf>, &'static str> {
        let mounted = self.mounted.lock().await;
        if mounted.len() > self.max_worktrees {
            return Err("mounted_root_inventory_exceeds_bound");
        }
        Ok(mounted.keys().cloned().collect())
    }

    #[cfg(test)]
    pub async fn notify_path(&self, project_root: &Path, path: PathBuf) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, epoch, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path(path);
        DaemonCodeIndexControlV1::advance(&epoch);
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::HookHint);
        true
    }

    /// Primary hint path: deliver the exact touched paths carried by a host
    /// after-file-edit hook into the mounted worktree's incremental queue.
    /// `rel_paths` are repository-relative; they are resolved against the
    /// project root. Returns `true` when a worktree was mounted to receive them.
    pub async fn notify_hook_paths(&self, project_root: &Path, rel_paths: &[String]) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, epoch, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake),
            )
        };
        let absolute = rel_paths
            .iter()
            .map(|rel| project_root.join(rel))
            .collect::<Vec<_>>();
        {
            let mut hints = hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for path in absolute {
                hints.path(path);
            }
        }
        DaemonCodeIndexControlV1::advance(&epoch);
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::HookHint);
        true
    }

    /// Preserve correctness when the pre-mount activation queue exceeds its
    /// bounded exact-path capacity. Overflow requests one authoritative scan for
    /// this exact mounted worktree; it never aliases a sibling worktree.
    pub async fn notify_hook_overflow(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, epoch, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        DaemonCodeIndexControlV1::advance(&epoch);
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Overflow);
        true
    }

    /// Queue an authoritative source scan without invalidating work that is
    /// already reconstructing an authoritative snapshot.
    ///
    /// Background read/startup reconciliation carries no changed-path
    /// evidence. If it arrives during a reconcile, the stored wake guarantees
    /// a follow-up scan; advancing the epoch would only discard the in-flight
    /// complete snapshot and restart the same work. Hook overflow remains the
    /// source-invalidation path and still advances the epoch above.
    pub(in crate::daemon) async fn request_authoritative_reconcile(
        &self,
        project_root: &Path,
    ) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Overflow);
        true
    }

    /// Mounted scope identity plus the currently serving generation for one
    /// project. Daemon authorities that must retain this scope's code-graph
    /// runtime (semantic vectors, generation retention) resolve through this
    /// read instead of re-deriving repository/worktree identity themselves.
    pub(in crate::daemon) async fn serving_code_scope(
        &self,
        project_root: &Path,
    ) -> Option<CodeIndexServingScopeV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (repository_id, worktree_id, shutting_down, serving) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.serving_generation),
            )
        };
        let serving_generation = serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| Arc::clone(&latest.generation));
        Some(CodeIndexServingScopeV1 {
            repository_id,
            worktree_id,
            shutting_down,
            serving_generation,
        })
    }

    pub(in crate::daemon) async fn install_semantic_vector_graph_provider(
        &self,
        project_root: &Path,
        provider: Arc<dyn tracedecay_usecases::semantic_runtime::SemanticVectorGraphProviderV1>,
    ) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mut mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get_mut(&project_root) else {
            return false;
        };
        worktree.semantic_vector_graph_provider = Some(provider);
        true
    }

    pub(in crate::daemon) async fn semantic_vector_graph_provider(
        &self,
        project_root: &Path,
    ) -> Option<Arc<dyn tracedecay_usecases::semantic_runtime::SemanticVectorGraphProviderV1>> {
        let project_root = project_root.canonicalize().ok()?;
        self.mounted
            .lock()
            .await
            .get(&project_root)?
            .semantic_vector_graph_provider
            .clone()
    }

    pub(in crate::daemon) async fn code_graph_replay_binding(
        &self,
        project_root: &Path,
        generation: &CodeGenerationId,
    ) -> Option<Result<super::CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1>> {
        let project_root = project_root.canonicalize().ok()?;
        let scheduler = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.scheduler)
        };
        Some(
            scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .code_graph_replay_binding(generation),
        )
    }

    pub async fn latest_generation_id(&self, project_root: &Path) -> Option<CodeGenerationId> {
        let project_root = project_root.canonicalize().ok()?;
        // Read the O(1) serving slot instead of the scheduler mutex. This used
        // to take `scheduler.lock()` — a blocking std mutex held by any
        // in-flight reconcile — while still holding the `mounted` async mutex,
        // so one warmup/dashboard call during a rebuild parked a runtime worker
        // for the reconcile's whole duration AND serialized every code-index
        // query behind it: a silent, daemon-wide code-index outage.
        let serving = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            Arc::clone(&worktree.serving_generation)
        };
        let latest = serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        Some(latest.generation.manifest().generation_id.clone())
    }

    /// Exact bounded dashboard projection for one mounted worktree.
    ///
    /// This is a status read, not a query-admission boundary: it reports the
    /// last scheduler execution state and never runs a freshness probe, opens
    /// Git, scans the worktree, publishes a generation, or posts a wake.
    /// Generation and scope fields are copied from the last sealed generation,
    /// never reconstructed from the dashboard's display path.
    pub(in crate::daemon) async fn dashboard_freshness(
        &self,
        project_root: &Path,
    ) -> Option<crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let (scheduler, reconcile_in_progress, serving_generation) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.serving_generation),
            )
        };
        tokio::task::spawn_blocking(move || {
            let refreshing = reconcile_in_progress.load(Ordering::Acquire) != 0;
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let latest = serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let (
                        repository_id,
                        worktree_id,
                        source_reference,
                        source_revision,
                        generation_id,
                        content_identity,
                        sealed,
                    ) = latest.as_ref().map_or(
                        (None, None, None, None, None, None, None),
                        |latest| {
                            let generation = &latest.generation;
                            let snapshot = generation.snapshot();
                            (
                                Some(snapshot.repository.as_str().to_owned()),
                                snapshot
                                    .worktree
                                    .as_ref()
                                    .map(|worktree| worktree.as_str().to_owned()),
                                snapshot
                                    .reference
                                    .as_ref()
                                    .map(|reference| reference.as_str().to_owned()),
                                snapshot
                                    .source_revision
                                    .as_ref()
                                    .map(|revision| revision.as_str().to_owned()),
                                Some(generation.manifest().generation_id.as_str().to_owned()),
                                Some(snapshot.content_identity.as_str().to_owned()),
                                Some(generation.manifest().seal.sealed_at.0),
                            )
                        },
                    );
                    return crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: canonical_root.display().to_string(),
                        repository_id,
                        worktree_id,
                        source_reference,
                        source_revision,
                        latest_generation_id: generation_id,
                        snapshot_content_identity: content_identity,
                        sealed_at_micros: sealed,
                        last_reconcile_micros: None,
                        staleness_state: Some(
                            if latest.is_some() {
                                "refreshing"
                            } else {
                                "indexing"
                            }
                            .to_owned(),
                        ),
                        hook_hint_count: None,
                        coverage: "partial_refresh_in_progress".to_owned(),
                    };
                }
            };
            let verified = scheduler.verified_against_source();
            let stale = !verified || scheduler.freshness_window_elapsed();
            let latest = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let hook_hint_count = scheduler.pending_hint_count();
            let (
                repository_id,
                worktree_id,
                source_reference,
                source_revision,
                generation_id,
                content_identity,
                sealed,
            ) = latest
                .as_ref()
                .map_or((None, None, None, None, None, None, None), |latest| {
                    let generation = &latest.generation;
                    let snapshot = generation.snapshot();
                    (
                        Some(snapshot.repository.as_str().to_owned()),
                        snapshot
                            .worktree
                            .as_ref()
                            .map(|worktree| worktree.as_str().to_owned()),
                        snapshot
                            .reference
                            .as_ref()
                            .map(|reference| reference.as_str().to_owned()),
                        snapshot
                            .source_revision
                            .as_ref()
                            .map(|revision| revision.as_str().to_owned()),
                        Some(generation.manifest().generation_id.as_str().to_owned()),
                        Some(snapshot.content_identity.as_str().to_owned()),
                        Some(generation.manifest().seal.sealed_at.0),
                    )
                });
            let staleness_state = if refreshing {
                if latest.is_some() {
                    "refreshing"
                } else {
                    "indexing"
                }
            } else if stale || hook_hint_count != Some(0) {
                if latest.is_some() {
                    "stale"
                } else {
                    "indexing"
                }
            } else if latest.is_some() {
                "fresh"
            } else {
                "indexing"
            };
            crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                worktree_root: canonical_root.display().to_string(),
                repository_id,
                worktree_id,
                source_reference,
                source_revision,
                latest_generation_id: generation_id,
                snapshot_content_identity: content_identity,
                sealed_at_micros: sealed,
                last_reconcile_micros: scheduler.last_reconciled_at_micros(),
                staleness_state: Some(staleness_state.to_owned()),
                hook_hint_count,
                coverage: if !verified {
                    "partial_unverified_restore"
                } else if hook_hint_count.is_some() {
                    "complete"
                } else {
                    "partial_hook_hint_overflow"
                }
                .to_owned(),
            }
        })
        .await
        .ok()
    }

    /// Query-admission entry point: serve only an already-decoded generation
    /// whose exact identity authority still resolves. Freshness verification and
    /// any rebuild remain retained background work.
    pub(in crate::daemon) async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Clone the per-worktree handle under a short map lock, then drop the
        // registry guard before checking the mounted route.
        let (scheduler, serving_generation, wake, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        // When the background worker already owns the scheduler, preserve the
        // last complete immutable generation instead of joining its work.
        let authority_root = project_root.clone();
        let latest = crate::daemon::park_admission(tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    // Serve prior generation without waiting, but schedule a
                    // follow-up verification so busy refresh cannot strand
                    // cadence indefinitely.
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::BusyFollowUp,
                    );
                    return serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                }
            };
            // Serve-old-first, continued: winning the scheduler lock must not
            // mean paying for the rebuild. `ensure_fresh_for_query` reconciles
            // inline, and that reconcile is O(store) with no bound of its own —
            // a live `tracedecay_context` call sat on this exact line for 900
            // seconds while the daemon ground a failing semantic publish loop,
            // and only the client's own timeout ended it. The ladder's checks
            // are cheap; its remedy belongs to the background worker.
            //
            // The git authority is still proven inline, because serving
            // retained bytes under an identity nothing can confirm is the one
            // thing the old inline reconcile fail-closed on.
            if !scheduler.git_authority_available() {
                return None;
            }
            let servable = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(latest) = servable {
                // Something is servable, so freshness is a background concern.
                // Only record an arrival when the ladder actually asked for a
                // reconcile; a quiet repository must not turn every read into
                // a wake, and an unattributed arrival would fabricate a
                // cadence sample for work that never ran.
                if scheduler.request_fresh_for_query_background() {
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::QueryAdmission,
                    );
                }
                return Some(latest);
            }
            // Cold open has no servable generation. Verification and any
            // rebuild stay with the retained owner; reads only request the
            // wake and return typed unavailable/unverified.
            if pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .micros
                == 0
            {
                scheduler.request_background_reconcile();
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            None
        }))
        .await
        .ok()
        .flatten()?;
        if let Ok(authority) = latest.test_attribution_authority() {
            let mut authorities = self
                .test_attribution_authorities
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            authorities.insert(
                authority_root,
                (
                    latest.generation.manifest().generation_id.clone(),
                    authority,
                ),
            );
        }
        Some(latest)
    }

    /// Query-admission entry point for latency-sensitive application paths.
    /// It serves only a generation whose freshness is already proven; stale,
    /// restored-unverified, or busy schedulers abstain after scheduling the
    /// background worker instead of reconciling on the caller.
    pub(in crate::daemon) async fn latest_complete_ready(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_with(project_root, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// [`Self::latest_complete_ready`] under an explicit decode admission.
    async fn latest_complete_ready_with(
        &self,
        project_root: &Path,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (scheduler, serving_generation) = {
            let mounted = self.mounted.try_lock().ok()?;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
            )
        };
        let latest = tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            };
            let latest = scheduler
                .latest_complete_ready_for_query_with(admission)
                .ok()
                .flatten()?;
            exact_activated_serving_generation(&serving_generation, &latest)
        })
        .await
        .ok()
        .flatten()?;
        if let Ok(authority) = latest.test_attribution_authority() {
            let mut authorities = self
                .test_attribution_authorities
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            authorities.insert(
                project_root,
                (
                    latest.generation.manifest().generation_id.clone(),
                    authority,
                ),
            );
        }
        Some(latest)
    }

    /// Resolve one mounted root by the exact admitted repository/worktree/ref
    /// scope, then run that root's freshness ladder. A request never inherits
    /// whichever mounted worktree sorts first.
    pub(in crate::daemon) async fn latest_complete_fresh_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for (root, worktree) in mounted.iter() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(root.clone());
                }
            }
            matched?
        };
        let latest = self.latest_complete_fresh(&root).await?;
        // Relaxed identity gate, not the exact one. `latest_complete_fresh` is
        // itself a serve-old-first ladder: it returns whatever complete
        // generation is retained and only *requests* the reconcile. Post-checking
        // the exact reference here discarded that retained generation the moment
        // HEAD moved, so grep/context/callers went `Unavailable` after every
        // restart-following-a-commit even though a complete generation was in
        // hand. Attribution is generation-bound (see
        // [`latest_matches_scope_identity`]), and the ladder has already
        // scheduled the rebuild that will replace this generation.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Resolve one exact scope and admit only an already-current generation.
    pub(in crate::daemon) async fn latest_complete_ready_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_for_scope_with(scope, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// [`Self::latest_complete_ready_for_scope`] restricted to an
    /// already-decoded generation.
    ///
    /// This is the freshness probe for a caller that *already* has a complete
    /// generation it can serve. It runs the same ready gate, but abstains
    /// instead of parking when the active generation is mid-decode, so awaiting
    /// a new generation can never preempt serving the old one.
    pub(in crate::daemon) async fn latest_complete_ready_decoded_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_for_scope_with(
            scope,
            GenerationDecodeAdmissionV1::AlreadyDecoded,
        )
        .await
    }

    fn current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (scheduler, serving_generation) = {
            let mounted = self.mounted.try_lock().ok()?;
            let worktree = mounted.get(&project_root)?;
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                return None;
            }
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
            )
        };
        let mut scheduler = match scheduler.try_lock() {
            Ok(scheduler) => scheduler,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let latest = scheduler
            .latest_complete_ready_for_exact_source_with(
                GenerationDecodeAdmissionV1::AlreadyDecoded,
            )
            .ok()
            .flatten()?;
        // Checkout-identity gate: the ready probe above already proved the
        // generation current against the live worktree, and the sealed
        // reference label is attribution, not identity (see
        // [`latest_matches_scope_identity`]).
        if !latest_matches_scope_identity(&latest, scope) {
            return None;
        }
        exact_activated_serving_generation(&serving_generation, &latest)
    }

    /// Report an already-decoded current generation for one exact mounted root
    /// and scope without mounting, decoding, or reconciling.
    pub(in crate::daemon) fn has_current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        self.current_ready_decoded_for_root_scope(project_root, scope)
            .is_some()
    }

    /// Return the exact ready generation without blocking the async executor
    /// on the bounded synchronous freshness probe.
    pub(in crate::daemon) async fn latest_complete_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let registry = self.clone();
        let project_root = project_root.to_path_buf();
        let scope = scope.clone();
        tokio::task::spawn_blocking(move || {
            registry.current_ready_decoded_for_root_scope(&project_root, &scope)
        })
        .await
        .ok()
        .flatten()
    }

    async fn latest_complete_ready_for_scope_with(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        // MCP search resolves its generation before it asks for query authority,
        // so this is the first authenticated demand boundary on that path.
        self.activate_for_scope(scope);
        let root = {
            let mounted = self.mounted.try_lock().ok()?;
            let mut matched = None;
            for (root, worktree) in mounted.iter() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(root.clone());
                }
            }
            matched?
        };
        let latest = self.latest_complete_ready_with(&root, admission).await?;
        // Checkout-identity gate: the ready ladder verified currency against
        // the live worktree, so a scope whose branch label was resolved on
        // the other side of a `git switch` must still be served its own
        // checkout's generation (see [`latest_matches_scope_identity`]).
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Resolve one exact scope and serve the last complete generation already
    /// held for that worktree, without running the freshness ladder.
    ///
    /// This is the stale-while-revalidate arm of query admission. The
    /// per-worktree `serving_generation` is seeded at mount from the restored
    /// generation and rewritten by every publication, so the read is O(1) and
    /// never blocks on reconcile, gix status, or the scheduler mutex. A caller
    /// that takes this arm is serving an older complete generation and must
    /// mark its lanes stale; it must never present the result as current.
    pub(in crate::daemon) async fn latest_complete_serving_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(Arc::clone(&worktree.serving_generation));
                }
            }
            matched?
        };
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        // Relaxed identity gate: this arm is stale by construction, so a moved
        // reference is exactly the condition it exists to survive.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Whether an exact mounted route has no admissible generation because its
    /// retained owner is still verifying or rebuilding it.
    pub(super) async fn generation_is_unverified_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let mounted = self.mounted.lock().await;
        let mut matched = mounted.values().filter(|worktree| {
            worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
        });
        let Some(worktree) = matched.next() else {
            return false;
        };
        if matched.next().is_some() {
            return false;
        }
        worktree
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
            && (worktree.reconcile_in_progress.load(Ordering::Acquire) != 0
                || worktree
                    .pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
                    != 0)
    }

    /// Ask the background worker for a reconcile on behalf of a query admission
    /// that found nothing servable, then return whether a wake was posted.
    ///
    /// This never reconciles inline and never parks: it runs only the ladder's
    /// cheap checks (`request_fresh_for_query_background`) and hands the O(store)
    /// remedy to the worker. It exists because the search path had no remedy at
    /// all — the freshness ladder lives in `latest_complete_fresh`, which search
    /// deliberately does not call, so a search that resolved to nothing returned
    /// its typed failure forever without ever asking anyone to rebuild.
    ///
    /// A quiet repository must not turn every read into a wake, so two
    /// suppressions apply. First, an already-pending, unclaimed wake *is* the
    /// remedy this admission would ask for, so it is reused rather than
    /// duplicated — that is what keeps a rebuild window's worth of failing
    /// searches from becoming a wake storm and from each fabricating its own
    /// cadence arrival. Second, when a generation is servable the ladder's own
    /// suppression decides, exactly as it does on the grep/context/callers path.
    pub(in crate::daemon) async fn request_query_background_reconcile(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        #[cfg(test)]
        let test_control = Self::query_admission_control_for_test(scope);
        #[cfg(test)]
        let lookup_guard = if let Some(test_control) = test_control.as_ref() {
            Some(test_control.lookup_gate.lock().await)
        } else {
            None
        };
        let (scheduler, serving_generation, wake, pending_wake) = {
            let Ok(mounted) = self.mounted.try_lock() else {
                return false;
            };
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id != scope.repository_id
                    || worktree.worktree_id != scope.worktree_id
                {
                    continue;
                }
                if matched.is_some() {
                    return false;
                }
                matched = Some((
                    Arc::clone(&worktree.scheduler),
                    Arc::clone(&worktree.serving_generation),
                    Arc::clone(&worktree.wake),
                    Arc::clone(&worktree.pending_wake),
                ));
            }
            let Some(matched) = matched else {
                return false;
            };
            matched
        };
        #[cfg(test)]
        drop(lookup_guard);
        #[cfg(test)]
        if let Some(test_control) = test_control.as_ref() {
            test_control.rendezvous.wait().await;
        }
        // Atomically reserve the existing pending-wake slot before scheduling
        // the blocking freshness probe. A pending or concurrently claimed wake
        // already supplies this query's remedy.
        let Some(wake_claim) = PendingWakeClaimV1::claim(Arc::clone(&pending_wake)) else {
            return false;
        };
        #[cfg(test)]
        if let Some(test_control) = test_control.as_ref()
            && test_control.pauses_after_claim.load(Ordering::Acquire)
        {
            test_control.claim_reached.store(true, Ordering::Release);
            test_control.claim_entered.notify_waiters();
            test_control.claim_release.notified().await;
        }
        tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    // A reconcile (or another query) owns the scheduler. Never
                    // queue on it from a query; schedule the follow-up pass
                    // instead, exactly as the grep/context/callers ladder does,
                    // so a busy refresh cannot strand cadence.
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::BusyFollowUp,
                    );
                    wake_claim.settle();
                    return true;
                }
            };
            let nothing_servable = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none();
            // Nothing is servable at all, so the ladder's suppression cannot
            // apply: a reconcile is the only thing that can ever make this scope
            // answerable, and no other caller on this path will ask for it.
            if nothing_servable {
                scheduler.request_background_reconcile();
            } else if !scheduler.request_fresh_for_query_background() {
                return false;
            }
            Self::note_wake(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
            wake_claim.settle();
            true
        })
        .await
        .unwrap_or(false)
    }

    pub(in crate::daemon) async fn semantic_evaluation_snapshot_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::SemanticEvaluationCodeSnapshotV1> {
        self.latest_complete_fresh_for_scope(scope)
            .await
            .map(|latest| latest.semantic_evaluation_snapshot())
    }

    pub(in crate::daemon) async fn acquire_semantic_evaluation_publication_lease(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        expected: &super::SemanticEvaluationCodeSnapshotV1,
    ) -> Option<CodeIndexSemanticEvaluationPublicationLeaseV1> {
        let gate = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id != scope.repository_id
                    || worktree.worktree_id != scope.worktree_id
                {
                    continue;
                }
                if matched.is_some() {
                    return None;
                }
                matched = Some(Arc::clone(&worktree.semantic_evaluation_publication_gate));
            }
            matched?
        };
        let guard = gate.lock_owned().await;
        if self
            .semantic_evaluation_snapshot_for_scope(scope)
            .await
            .as_ref()
            != Some(expected)
        {
            return None;
        }
        Some(CodeIndexSemanticEvaluationPublicationLeaseV1 { _guard: guard })
    }

    /// The per-worktree scheduler handle, cloned out of the registry map. Test
    /// support for proving that holding one worktree's scheduler lock does not
    /// block another worktree's freshness query on the registry map.
    #[cfg(test)]
    pub(super) async fn scheduler_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<Mutex<CodeIndexWorktreeSchedulerV1>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.scheduler))
    }

    pub async fn shutdown(&self) {
        self.cancel();
        let cold_mount_completions = self.cold_mount_reservation_completions();
        let mut retiring_guard = self.retiring.lock().await;
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        let retiring = std::mem::take(&mut *retiring_guard);
        drop(retiring_guard);
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for worktree in mounted.values() {
            worktree.shutting_down.store(true, Ordering::Release);
            worktree.wake.notify_one();
        }
        for (_, worktree) in mounted {
            let _ = worktree.task.await;
        }
        for (_, worktree) in retiring {
            let _ = worktree.task.await;
        }
        for mut completion in cold_mount_completions {
            let _ = completion.changed().await;
        }
    }

    pub(in crate::daemon) async fn retire_project_roots(
        &self,
        project_roots: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        self.retire_project_roots_with_deadline(
            project_roots,
            super::super::DAEMON_TASK_ABORT_DEADLINE,
        )
        .await
    }

    pub(super) async fn retire_project_roots_with_deadline(
        &self,
        project_roots: &std::collections::BTreeSet<PathBuf>,
        timeout: std::time::Duration,
    ) -> bool {
        let mut retiring = self.retiring.lock().await;
        let (retired, cold_mount_waiting, mut completed_cold_mounts) = {
            let mut mounted = self.mounted.lock().await;
            let cold_mounts = self.retire_cold_mount_reservations(project_roots);
            let retired = project_roots
                .iter()
                .filter_map(|root| {
                    mounted
                        .remove(root)
                        .map(|worktree| (root.clone(), worktree))
                })
                .collect::<Vec<_>>();
            (retired, cold_mounts.0, cold_mounts.1)
        };
        {
            let mut authorities = match self.test_attribution_authorities.write() {
                Ok(authorities) => authorities,
                Err(poisoned) => poisoned.into_inner(),
            };
            for root in project_roots {
                authorities.remove(root);
            }
        }
        for (root, worktree) in retired {
            worktree.shutting_down.store(true, Ordering::Release);
            worktree.wake.notify_one();
            retiring.insert(root, worktree);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut drained = true;
        // A cold owner needs `retiring` for its final cancellation fence before
        // it can drop the reservation that this wait observes. The retired
        // reservation itself blocks remounts while this guard is released.
        drop(retiring);
        for (root, mut completion) in cold_mount_waiting {
            match tokio::time::timeout_at(deadline, completion.changed()).await {
                Ok(_) => {
                    completed_cold_mounts.insert(root);
                }
                Err(_) => {
                    drained = false;
                }
            }
        }
        let mut retiring = self.retiring.lock().await;
        let mut joined = BTreeSet::new();
        for root in project_roots {
            let Some(worktree) = retiring.get_mut(root) else {
                continue;
            };
            match tokio::time::timeout_at(deadline, &mut worktree.task).await {
                Ok(_) => {
                    joined.insert(root.clone());
                }
                Err(_) => {
                    drained = false;
                }
            }
        }
        retiring.retain(|root, _| !joined.contains(root));
        self.release_completed_retired_cold_mount_reservations(&completed_cold_mounts);
        drained
    }

    #[cfg(test)]
    pub(super) async fn retiring_owner_count(&self) -> usize {
        self.retiring.lock().await.len()
    }

    pub fn cancel(&self) {
        self.background_reconcile_admission.close();
        self.cancel_cold_mount_reservations();
        if let Ok(mounted) = self.mounted.try_lock() {
            for worktree in mounted.values() {
                worktree.shutting_down.store(true, Ordering::Release);
                worktree.wake.notify_one();
            }
        }
    }
}

impl tracedecay_usecases::feedback::cycle_production::ProductionFeedbackDocumentIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
        document_uri: Option<String>,
    ) -> tracedecay_usecases::feedback::cycle_production::ProductionFeedbackDocumentIdentityFuture
    {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("feedback-code-index-root-unavailable"))?;
            let current = registry.latest_complete_ready(&root).await.ok_or_else(|| {
                LspRuntimeFailure::new("feedback-code-index-generation-unavailable")
            })?;
            let generation = &current.generation;
            let snapshot = generation.snapshot();
            let file = match document_uri {
                Some(uri) => {
                    let logical_path = feedback_document_logical_path(&root, &uri)?;
                    snapshot
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("feedback-code-index-document-unavailable")
                        })?
                }
                None => snapshot
                    .files
                    .iter()
                    .find(|file| {
                        Path::new(&file.logical_path)
                            .extension()
                            .and_then(|ext| ext.to_str())
                            == Some("rs")
                    })
                    .ok_or_else(|| {
                        LspRuntimeFailure::new("feedback-code-index-rust-document-unavailable")
                    })?,
            };
            let generation_digest =
                ManifestDigest::new(generation.manifest().snapshot_digest.as_str().to_owned())
                    .map_err(|_| {
                        LspRuntimeFailure::new("feedback-code-index-generation-invalid")
                    })?;
            Ok(
                tracedecay_usecases::feedback::cycle_production::ProductionFeedbackDocumentIdentityV1 {
                    generation_id: generation.manifest().generation_id.clone(),
                    generation_digest,
                    file: file.file_occurrence_id.clone(),
                    content_digest: file.content_digest.clone(),
                },
            )
        })
    }
}

/// The registry is the single mint for file and generation identity, so every
/// diagnostic producer resolves through here instead of inventing its own.
///
/// Without this, a producer had no way to reach the authority and fell back to
/// a repository-relative path; the LSP feedback projection then refused each
/// published record with `ImpactTargetFileMismatch` / `GenerationMismatch`,
/// because the saved-edit cycle's impact target is minted here as
/// `file.daemon.<digest>` under this generation.
impl crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
    ) -> crate::diagnostics_publication::CodeIndexPublicationIdentityFuture<'_> {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root.canonicalize().ok()?;
            let current = registry.latest_complete_ready(&root).await?;
            let snapshot = current.generation.snapshot();
            Some(
                crate::diagnostics_publication::CodeIndexPublicationIdentityV1::new(
                    current.generation.manifest().generation_id.clone(),
                    current.generation.manifest().seal.sealed_at,
                    snapshot.repository.clone(),
                    snapshot.worktree.clone(),
                    snapshot.reference.clone(),
                    snapshot.source_revision.clone(),
                    snapshot.files.iter().map(|file| {
                        (
                            file.logical_path.clone(),
                            file.file_occurrence_id.clone(),
                            file.content_digest.clone(),
                        )
                    }),
                ),
            )
        })
    }
}

impl crate::code_index::provider::GenerationTestAttributionJoinReadPort
    for CodeIndexSchedulerRegistryV1
{
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> crate::code_index::provider::GenerationProviderReadV1<
        crate::code_index::test_attribution::GenerationTestJoinV1,
    > {
        let authorities = self
            .test_attribution_authorities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut matching = authorities
            .values()
            .filter(|(candidate, _)| candidate == generation);
        let Some((_, authority)) = matching.next() else {
            return crate::code_index::provider::GenerationProviderReadV1::new(
                tracedecay_domain::ProviderEvaluationStateV1::Unavailable,
                crate::code_index::provider::GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static unavailable attribution read"));
        };
        if matching.next().is_some() {
            return crate::code_index::provider::GenerationProviderReadV1::new(
                tracedecay_domain::ProviderEvaluationStateV1::Unavailable,
                crate::code_index::provider::GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static ambiguous attribution read"));
        }
        crate::code_index::provider::GenerationTestAttributionJoinReadPort::read_test_attribution(
            authority, generation,
        )
    }
}

fn feedback_document_logical_path(
    project_root: &Path,
    document_uri: &str,
) -> Result<String, LspRuntimeFailure> {
    let url = url::Url::parse(document_uri)
        .map_err(|_| LspRuntimeFailure::new("feedback-document-uri-invalid"))?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return Err(LspRuntimeFailure::new("feedback-document-uri-invalid"));
    }
    let path = url
        .to_file_path()
        .map_err(|()| LspRuntimeFailure::new("feedback-document-uri-invalid"))?;
    let relative = path
        .strip_prefix(project_root)
        .map_err(|_| LspRuntimeFailure::new("feedback-document-outside-root"))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LspRuntimeFailure::new("feedback-document-uri-invalid"));
    }
    relative
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| LspRuntimeFailure::new("feedback-document-path-unavailable"))
}
