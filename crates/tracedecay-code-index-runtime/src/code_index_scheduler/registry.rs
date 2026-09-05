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
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::Condvar;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeGraphServingReadinessV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, WorktreeId, host_cpu_target,
};
use tracedecay_lsp::LspRuntimeFailure;

use super::graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1};
use super::reconcile_panic_guard::{
    ReconcileCapacityRetryV1, ReconcilePanicDecisionV1, ReconcilePanicGuardV1,
};
use super::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, CodeIndexNoopEvidenceV1,
    CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexControlV1, GenerationDecodeAdmissionV1,
    LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1, PendingHintsV1,
    SharedCodeIndexBytePoolV1, newly_eligible_percentile, now_micros,
};
#[cfg(test)]
use super::{CodeIndexBytePoolStatsV1, CodeIndexCadenceReadModelV1};

#[cfg(test)]
mod cold_read_wake_tests;
#[cfg(all(test, unix))]
mod convergence_park_tests;
mod ignored_dependencies;
mod lsp_projection;
#[cfg(test)]
mod reconcile_failure_isolation_tests;
mod scope_identity;

pub use scope_identity::{latest_matches_scope_identity, text_matches_scope_identity};

const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;
/// Page/finalization operations one background worker pass hints to the text
/// projection.
///
/// This is the caller hint, and it is what actually sizes the work: the
/// advance clamps it to `TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1` and the
/// sealed source then offers `min(hint, TEXT_ARTIFACT_BATCH_PAGES_V1)` pages
/// per commit and `hint * TEXT_ARTIFACT_FINALIZATION_ROWS_PER_OPERATION_V1`
/// rows per finalization slice. Pinning it to the advance ceiling keeps the
/// two in step; as a hardcoded 64 it silently capped every wake at one
/// 64-page batch, so the ceiling's own "one wake can still commit two
/// full-sized batches" contract never held and each batch paid a separate
/// worker round trip (`spawn_blocking`, admission permit, publication lock).
const TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1: usize =
    super::TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1;

/// Bounded exponential backoff between activation retries of the same sealed
/// generation. Activation of a large artifact is minutes of real work, so the
/// floor stays above the query staleness threshold and the ceiling keeps a
/// persistently failing artifact from being retried more than a few times an
/// hour while never resealing it. Tests shrink the clock, not the shape.
const ACTIVATION_RETRY_BACKOFF_FLOOR: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(30)
};
const ACTIVATION_RETRY_BACKOFF_CEILING: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(400)
} else {
    Duration::from_mins(10)
};

#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "code_index.graph_seat.noop_follow_up")
)]
pub(crate) fn retained_noop_requires_follow_up_wake(
    serving_empty: bool,
    activation_deferred: bool,
    consumed_external_arrival: bool,
    source_is_noop: bool,
) -> bool {
    serving_empty && !activation_deferred && consumed_external_arrival && source_is_noop
}

/// Whether this activation failure repeats the previous attempt's conflict
/// verdict for the same sealed generation. A first Conflict can be a race
/// with a concurrent publisher and retries like any transient failure, but
/// the same guard site refusing with identical compared evidence on the very
/// next attempt over the same immutable sealed inputs is deterministic:
/// retrying re-runs minutes of replay to reach the identical refusal, so the
/// seat loop converts it into the terminal typed refusal instead of backing
/// off forever (issue #765).
pub(crate) fn is_repeated_conflict_verdict(
    error: &CodeIndexSchedulerErrorV1,
    seat_generation_id: &tracedecay_domain::CodeGenerationId,
    last_seat_conflict: Option<&(
        tracedecay_domain::CodeGenerationId,
        tracedecay_graph_db::GraphConflictContextV1,
    )>,
) -> bool {
    error.activation_conflict_context().is_some_and(|context| {
        last_seat_conflict.is_some_and(|(prior_generation, prior_context)| {
            prior_generation == seat_generation_id && prior_context == context
        })
    })
}

/// How many bounded text-projection slices one pass may run to completion
/// before the optional graph decode. The projection is finite in the sealed
/// generation's document count, so this only bounds a non-progressing builder:
/// reaching it defers graph seating to a later pass instead of spinning.
const TEXT_PROJECTION_MAXIMUM_ACTIVATION_ADVANCES_V1: usize = 10_000;

/// Whether a reconcile pass may prepare the sealed generation for graph
/// serving, and when it may not, why.
///
/// Graph seating used to demand a `Noop` outcome - tree quiescence - which a
/// shared checkout with peers editing never offers: every pass published a new
/// generation, so a complete sealed generation sat on disk with zero seat
/// attempts and no log line, because a missing prepare is not a refusal. The
/// gate is now the text owner, not the tree: a publication seats on its own
/// pass once its lightweight text owner has reopened and finished, and an
/// unchanged pass seats the retained owner's generation. Every skip names
/// itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphSeatGateV1 {
    /// Prepare, decode, activate, and swap this generation into serving.
    Prepare,
    /// Graph activation is off for this worktree by configuration.
    Disabled,
    /// This pass produced no terminal reconcile outcome to seat.
    ReconcileUnfinished,
    /// A retryable activation failure holds seating until its scheduled retry.
    ActivationDeferred,
    /// A publication whose replacement text owner did not reopen or finish.
    PublishedTextOwnerUnavailable,
    /// An unchanged pass whose retained text owner is still projecting.
    RetainedTextOwnerWarming,
}

impl GraphSeatGateV1 {
    #[hotpath::skip]
    pub const fn decide(
        activation_enabled: bool,
        activation_deferred: bool,
        reconcile_is_terminal: bool,
        published_pass: bool,
        retained_text_is_ready: bool,
        published_text_is_ready: bool,
    ) -> Self {
        if !activation_enabled {
            return Self::Disabled;
        }
        if !reconcile_is_terminal {
            return Self::ReconcileUnfinished;
        }
        if activation_deferred {
            return Self::ActivationDeferred;
        }
        if published_pass {
            // The tree may have moved on already; what must hold is that this
            // publication's own text owner reopened and finished, so exact and
            // lexical serving never inherit graph activation latency.
            if published_text_is_ready {
                return Self::Prepare;
            }
            return Self::PublishedTextOwnerUnavailable;
        }
        if retained_text_is_ready {
            return Self::Prepare;
        }
        Self::RetainedTextOwnerWarming
    }

    /// The typed reason this pass seated nothing, if it seated nothing.
    ///
    /// `Disabled` is not a skip: a worktree with graph activation off is not
    /// waiting for a seat, and logging one per pass would be noise.
    #[hotpath::skip]
    pub const fn skip_reason(self) -> Option<&'static str> {
        match self {
            Self::Prepare | Self::Disabled => None,
            Self::ReconcileUnfinished => Some("reconcile_unfinished"),
            Self::ActivationDeferred => Some("activation_deferred"),
            Self::PublishedTextOwnerUnavailable => Some("published_text_owner_unavailable"),
            Self::RetainedTextOwnerWarming => Some("retained_text_owner_warming"),
        }
    }
}

/// Whether a prepared generation still owes native graph activation.
///
/// Preparation binds the complete generation and hands it to the serving
/// swap; activation installs its native graph. The two used to share one
/// gate, so refusing a redundant activation also refused the seat — a restart
/// that restored an owner whose graph was already Ready therefore left the
/// serving slot empty forever while status read the same owner and reported
/// Ready. Every arm here refuses activation only; the seat always happens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphActivationGateV1 {
    /// Install this generation's native graph.
    Activate,
    /// The bound owner already serves a native graph. Replaying activation
    /// reopened the persistent graph, so shutdown cancelled the duplicate
    /// projection and then conflicted closing the live reconciliation owner.
    AlreadyServing,
    /// Nothing new to install: the serving slot already holds this exact
    /// generation and its graph is terminal (Ready, Refused, or Unavailable).
    UnchangedGraph,
    /// A generation whose graph is still Pending gets exactly one further
    /// attempt per worker; this one is already spent.
    PendingAttemptSpent,
}

impl GraphActivationGateV1 {
    /// Decide activation for a generation this pass prepared and will seat.
    ///
    /// `graph_already_serves` is the retained/restored text owner's own
    /// readiness, not the bound generation's: a restored owner carries its
    /// Ready graph across the bind, and that owner is the authority status
    /// reads.
    #[hotpath::skip]
    pub const fn decide(
        graph_already_serves: bool,
        replaces_serving_generation: bool,
        graph_activation_is_pending: bool,
        pending_attempt_spent: bool,
    ) -> Self {
        if graph_already_serves {
            return Self::AlreadyServing;
        }
        if replaces_serving_generation {
            return Self::Activate;
        }
        if !graph_activation_is_pending {
            return Self::UnchangedGraph;
        }
        if pending_attempt_spent {
            return Self::PendingAttemptSpent;
        }
        Self::Activate
    }

    #[hotpath::skip]
    pub const fn activates(self) -> bool {
        matches!(self, Self::Activate)
    }
}

/// What the serving swap did with a reconciled generation.
///
/// The swap is the only writer of the serving slot, so every arm here is a
/// distinct answer to "does a complete sealed generation serve now?" and each
/// one is named in the log rather than collapsing into a bare success or a
/// generic reconcile failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServingSwapOutcomeV1 {
    /// The generation is the active durable publication and now serves.
    Seated,
    /// The durable pointer already names a successor, but nothing was serving:
    /// a stale seat beats an empty route and the successor supersedes it.
    SeatedStale,
    /// The durable pointer already names a successor and a generation is
    /// already serving, so the slot keeps what it has.
    Superseded,
    /// The generation already serves; only semantic admission was re-offered.
    Offered,
}

impl ServingSwapOutcomeV1 {
    /// Decide what the swap does with a generation that finished activating.
    ///
    /// Extracted from the swap so every arm is directly assertable: the stale
    /// arm exists because activation of a large generation outlives the
    /// checkout it sealed from, and refusing that seat left the graph route
    /// serving nothing at all rather than serving something stale.
    #[hotpath::skip]
    pub const fn decide(publication_matches: bool, serving_is_seated: bool, replace: bool) -> Self {
        if !publication_matches {
            if serving_is_seated {
                // Something already serves; a superseded generation must not
                // move the slot backwards.
                return Self::Superseded;
            }
            return Self::SeatedStale;
        }
        if replace { Self::Seated } else { Self::Offered }
    }

    /// Whether this outcome writes the serving slot.
    #[hotpath::skip]
    pub const fn installs(self) -> bool {
        matches!(self, Self::Seated | Self::SeatedStale)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
struct ColdMountFinalCommitGateV1 {
    project_root: PathBuf,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(any(test, feature = "test-helpers"))]
fn cold_mount_final_commit_gate() -> &'static Mutex<Option<ColdMountFinalCommitGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<Option<ColdMountFinalCommitGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(None))
}

#[cfg(any(test, feature = "test-helpers"))]
struct RetainedGraphRecoverySuccessorGateV1 {
    project_root: PathBuf,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(any(test, feature = "test-helpers"))]
fn retained_graph_recovery_successor_gate()
-> &'static Mutex<Option<RetainedGraphRecoverySuccessorGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<Option<RetainedGraphRecoverySuccessorGateV1>>> =
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
pub mod watch_ingress;

/// At most two distinct worktrees may reconcile concurrently. Each reconcile
/// already saturates the shared indexing pool during extraction; the second
/// permit overlaps its I/O and publication phases without admitting an
/// unbounded number of full-width indexing owners.
const MAX_CONCURRENT_RECONCILE_WORKTREES: usize = 2;

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
pub enum ColdMountOpenEventV1 {
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
pub struct CodeIndexServingScopeV1 {
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub shutting_down: Arc<AtomicBool>,
    pub serving_generation: Option<Arc<CodeIndexPublishedGenerationV1>>,
}

/// Outcome of retiring the retained generation from a failed branch
/// publication. A no-match preserves a newer generation that won the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingGenerationRollbackOutcomeV1 {
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
pub struct ServingGenerationInstallationV1 {
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
pub enum ServingGenerationInstallationOutcomeV1 {
    Installed(ServingGenerationInstallationV1),
    NoMatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexGenerationPublishedV1 {
    pub project_root: PathBuf,
    pub repository_id: RepositoryId,
    pub generation_id: CodeGenerationId,
    pub snapshot_content_identity: tracedecay_domain::ContentDigest,
    pub observation_time_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryActivationAttemptV1 {
    revision: ConfigurationRevisionId,
    token: u64,
    preserves_existing_authority: bool,
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeIndexSchedulerMemoryStatsV1 {
    pub mounted_worktrees: u64,
    pub reconciling_worktrees: u64,
    pub retained_generation_encoded_bytes: u64,
}

pub struct MountedCodeIndexWorktreeV1 {
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub query_authority: Option<(
        ManifestDigest,
        Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    )>,
    pub semantic_query_authority: Option<(
        ManifestDigest,
        Arc<super::semantic_query_runtime::SemanticQueryAuthorityV1>,
    )>,
    pub query_activation_revision: Option<ConfigurationRevisionId>,
    pub query_activation_epoch: Option<i64>,
    pub query_activation_transition_digest: Option<ManifestDigest>,
    pub query_activation_attempt: u64,
    pub query_activation_redundancy:
        Option<tracedecay_usecases::semantic_runtime::PreparedSemanticRedundancyAuthorityV1>,
    pub semantic_vector_graph_provider:
        Option<Arc<dyn tracedecay_usecases::semantic_runtime::SemanticVectorGraphProviderV1>>,
    pub scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    /// Explicit same-store build/publication invariant shared by source
    /// reconcile, ignored-dependency publication, and historical generation
    /// minting. Async owners acquire this before entering blocking scheduler
    /// work so competing builds wait without occupying a blocking-pool thread.
    pub(super) build_publication_lock: Arc<tokio::sync::Mutex<()>>,
    pub historical_generation_owner: super::HistoricalCodeIndexGenerationOwnerV1,
    pub serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    /// Source-freshness state is independent from scheduler build state so
    /// readiness probes remain available throughout a long publication.
    source_freshness: super::SourceFreshnessFenceV1,
    /// Lock-free last-reconcile timestamp. `0` means none. Dashboard freshness
    /// reads this when the scheduler mutex would block.
    pub last_reconciled_at_micros: Arc<AtomicI64>,
    pub text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>>,
    /// Deterministic contract violation currently parking background
    /// convergence. The worker stamps it when a text-projection pass fails on
    /// a violation unchanged input reproduces (for example a store path that
    /// is not owner-private) and clears it when a pass progresses, so status
    /// and doctor report a typed parked state instead of indefinite warming.
    convergence_park: Arc<RwLock<Option<CodeIndexConvergenceParkedV1>>>,
    /// Owner-configuration recovery observed by the scheduler. This stays
    /// readable while a replacement build owns the scheduler mutex.
    generation_recovery: Arc<
        RwLock<
            Option<
                tracedecay_dashboard_api::code_index_freshness_api::CodeIndexGenerationRecoveryV1,
            >,
        >,
    >,
    /// The exact-source currency witness for the seated generation, readable
    /// without the scheduler mutex. Armed when the quiet exact-source probe
    /// passes or when a generation extracted this pass seats as the active
    /// publication; cleared when the probe fails or the slot is rewritten
    /// with an unproven generation. A background reconcile owns the scheduler
    /// mutex for its whole pass — sealing a production-scale corpus holds it
    /// for minutes — and verified graph reads re-prove the witness against
    /// the live checkout through that window instead of refusing.
    serving_source_witness: Arc<RwLock<Option<super::ServingSourceWitnessV1>>>,
    /// Immutable progress snapshot independently readable while the scheduler
    /// owns a long reconcile or text-artifact transaction.
    pub build_progress: super::CodeIndexBuildProgressSlotV1,
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
    /// Observability lane, installed once after project open mounts the
    /// project-bound producer. Empty means this worktree records no
    /// canonical index or retrieval observations (never a fabricated zero).
    index_observability: Arc<OnceLock<super::observability::CodeIndexObservabilityV1>>,
    shutting_down: Arc<AtomicBool>,
    /// Count of in-flight owner passes; nonzero means activation or reconcile
    /// work is running for this worktree.
    reconcile_in_progress: Arc<AtomicUsize>,
    /// Live handle to the publication's encoded-byte counter; observed only by
    /// test memory accounting today.
    _active_generation_encoded_bytes: Arc<AtomicU64>,
    pub semantic_evaluation_publication_gate: Arc<tokio::sync::Mutex<()>>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Unique mounted worktree for one admitted repo+worktree scope.
///
/// Real mounts key the registry from the same canonical root that derives the
/// worktree ID, so identity is unique. Missing and ambiguous matches stay
/// distinct so generation reads can fail closed on a collision instead of
/// collapsing it into a silent miss.
pub enum UniqueMountedWorktree<'a> {
    None,
    Ambiguous,
    One {
        root: &'a PathBuf,
        worktree: &'a MountedCodeIndexWorktreeV1,
    },
}

impl<'a> UniqueMountedWorktree<'a> {
    pub fn unique(self) -> Option<(&'a PathBuf, &'a MountedCodeIndexWorktreeV1)> {
        match self {
            Self::One { root, worktree } => Some((root, worktree)),
            Self::None | Self::Ambiguous => None,
        }
    }
}

pub fn unique_mounted_for_scope<'a>(
    mounted: &'a BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>,
    scope: &tracedecay_application::ResolvedScope,
) -> UniqueMountedWorktree<'a> {
    let mut matched = None;
    for (root, worktree) in mounted {
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            continue;
        }
        if matched.is_some() {
            return UniqueMountedWorktree::Ambiguous;
        }
        matched = Some((root, worktree));
    }
    match matched {
        Some((root, worktree)) => UniqueMountedWorktree::One { root, worktree },
        None => UniqueMountedWorktree::None,
    }
}

/// Remediation reported beside a parked deterministic contract violation.
/// The reason names the exact violation (path, observed mode, required mode);
/// this names the operator journey and the automatic recovery cadence.
const CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1: &str = "fix the named contract violation \
     (for example restore owner-only access on the named path), then run `tracedecay sync` and \
     re-check `tracedecay status`; an incompatible derived lexical cursor is replaced \
     automatically without resetting project identity, sessions, memory, or configuration; \
     `storage reset-project-store` is only for a reported schema reset requirement";

/// Remediation reported when the text-projection task itself failed
/// abnormally. Unchanged input reproduces the failure, so only changed input
/// (a new sealed generation) retries it.
const CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1: &str = "inspect the daemon log for the \
     abnormal text-projection failure; indexing retries when a new generation seals over \
     changed input";

/// Record one observation of a deterministic contract violation on a mounted
/// worktree's park slot. The first observation stamps the park, an identical
/// reason increments the pass counter, and a different reason replaces the
/// park so the surfaced state always names the current obstacle.
fn park_convergence(
    slot: &RwLock<Option<CodeIndexConvergenceParkedV1>>,
    reason: String,
    remediation: &str,
    retries_on_wake: bool,
) {
    let mut slot = slot
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match slot.as_mut() {
        Some(parked) if parked.reason == reason => {
            parked.observed_passes = parked.observed_passes.saturating_add(1);
        }
        _ => {
            *slot = Some(CodeIndexConvergenceParkedV1 {
                reason,
                remediation: remediation.to_owned(),
                parked_at_micros: now_micros().0,
                observed_passes: 1,
                retries_on_wake,
            });
        }
    }
}

/// Whether the current park re-checks on every wake (a contract violation an
/// operator fix clears in place), as opposed to a terminal task failure.
fn convergence_park_retries_on_wake(slot: &RwLock<Option<CodeIndexConvergenceParkedV1>>) -> bool {
    slot.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|parked| parked.retries_on_wake)
}

/// Clear the park after a pass progressed or completed: the previously parked
/// violation is no longer the current convergence obstacle.
fn clear_convergence_park(slot: &RwLock<Option<CodeIndexConvergenceParkedV1>>) {
    if slot
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_none()
    {
        return;
    }
    *slot
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The sealed-generation identity half of a freshness reading. Every other
/// field is left at its default so callers can fill in the observation half
/// with struct-update syntax, which keeps these seven — six of them
/// `Option<String>` — matched by name rather than by position.
fn dashboard_freshness_identity(
    latest: Option<&LatestCompleteCodeIndexV1>,
) -> tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
    let mut identity =
        tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1::default();
    if let Some(latest) = latest {
        let generation = &latest.generation;
        let snapshot = generation.snapshot();
        identity.repository_id = Some(snapshot.repository.as_str().to_owned());
        identity.worktree_id = snapshot
            .worktree
            .as_ref()
            .map(|worktree| worktree.as_str().to_owned());
        identity.source_reference = snapshot
            .reference
            .as_ref()
            .map(|reference| reference.as_str().to_owned());
        identity.source_revision = snapshot
            .source_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned());
        identity.latest_generation_id =
            Some(generation.manifest().generation_id.as_str().to_owned());
        identity.snapshot_content_identity = Some(snapshot.content_identity.as_str().to_owned());
        identity.sealed_at_micros = Some(generation.manifest().seal.sealed_at.0);
    }
    identity
}

/// Project the graph-serving state without warming any serving derivation.
pub(super) fn dashboard_code_graph_serving(
    latest: Option<&LatestCompleteCodeIndexV1>,
    text: Option<&LatestCodeTextGenerationV1>,
    graph_activation_enabled: bool,
) -> Option<CodeGraphServingReadinessV1> {
    if !graph_activation_enabled {
        return Some(CodeGraphServingReadinessV1::Unavailable {
            reason: "graph_activation_disabled".to_owned(),
        });
    }
    if let Some(text) = text {
        return Some(text.code_graph_serving_readiness());
    }
    Some(latest.map_or_else(
        || CodeGraphServingReadinessV1::Unavailable {
            reason: "generation_unavailable".to_owned(),
        },
        LatestCompleteCodeIndexV1::code_graph_serving_readiness,
    ))
}

/// Whether status may report this worktree as terminal (`fresh` / `current`).
///
/// Refused graph activation remains terminal for text serving, preserving the
/// existing status behavior; strict dogfood can distinguish it from Ready via
/// the separate typed projection.
fn dashboard_generation_is_ready(
    latest: Option<&LatestCompleteCodeIndexV1>,
    text_ready: bool,
    graph_activation_enabled: bool,
    code_graph_serving: &Option<CodeGraphServingReadinessV1>,
) -> bool {
    if graph_activation_enabled {
        text_ready
            && matches!(
                code_graph_serving,
                Some(
                    CodeGraphServingReadinessV1::Ready
                        | CodeGraphServingReadinessV1::Refused { .. }
                        | CodeGraphServingReadinessV1::Unavailable { .. }
                )
            )
    } else {
        latest.is_some() || text_ready
    }
}

fn dashboard_text_freshness_identity(
    latest: Option<&LatestCodeTextGenerationV1>,
) -> tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
    let mut identity =
        tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1::default();
    if let Some(latest) = latest {
        let metadata = latest.metadata();
        let snapshot = metadata.snapshot();
        identity.repository_id = Some(snapshot.repository.as_str().to_owned());
        identity.worktree_id = snapshot
            .worktree
            .as_ref()
            .map(|worktree| worktree.as_str().to_owned());
        identity.source_reference = snapshot
            .reference
            .as_ref()
            .map(|reference| reference.as_str().to_owned());
        identity.source_revision = snapshot
            .source_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned());
        identity.latest_generation_id = Some(metadata.manifest().generation_id.as_str().to_owned());
        identity.snapshot_content_identity = Some(snapshot.content_identity.as_str().to_owned());
        identity.sealed_at_micros = Some(metadata.manifest().seal.sealed_at.0);
    }
    identity
}

pub struct CodeIndexSemanticEvaluationPublicationLeaseV1 {
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
    fn has_pending_arrival(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .micros
            != 0
    }

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

/// Seat handles the ready probe validates outside the mounted-map lock.
type ReadyProbeServingPartsV1 = (
    Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    super::SourceFreshnessFenceV1,
    super::HistoricalCodeIndexGenerationOwnerV1,
    Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    Arc<RwLock<Option<super::ServingSourceWitnessV1>>>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
);

#[derive(Clone)]
pub struct CodeIndexSchedulerRegistryV1 {
    pub max_worktrees: usize,
    /// Durable daemon-authority epoch shared by every progress producer in
    /// this registry. This is never derived from wall-clock time.
    pub progress_daemon_incarnation: u64,
    /// Next scheduler-owner token within `progress_daemon_incarnation`.
    /// Cloned registries share this authority, so same-daemon retire/remounts
    /// cannot reuse a progress ordering key.
    pub next_progress_producer_incarnation: Arc<AtomicU64>,
    pub resident_memory: Arc<resident_memory::ProcessResidentMemoryV1>,
    pub byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    /// Owners whose project was retired (remote deletion, replacement) but whose
    /// reconcile task has not finished draining. A root parked here must never
    /// re-mount: a fresh owner would race the dying one over the same store.
    pub retiring: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
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
    fn incomplete_text_slice_may_continue(pending_wake: &PendingWakeV1) -> bool {
        !pending_wake.has_pending_arrival()
    }

    fn mint_progress_producer_incarnation(&self) -> Result<u64, CodeIndexSchedulerErrorV1> {
        self.next_progress_producer_incarnation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| {
                CodeIndexSchedulerErrorV1::Identity(
                    "code-index progress producer incarnation authority is exhausted".to_owned(),
                )
            })
    }

    #[hotpath::measure(label = "daemon.code_index.registry.register_activation")]
    pub fn register_activation(
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

    fn activation_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<super::CodeIndexActivationV1>> {
        {
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
        }
    }

    pub fn automatic_admission_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::CodeIndexAutomaticAdmissionV1> {
        self.activation_for_scope(scope)
            .map(|activation| activation.automatic_admission())
    }

    fn activate_for_scope(&self, scope: &tracedecay_application::ResolvedScope) -> bool {
        self.activation_for_scope(scope)
            .is_some_and(|activation| activation.activate())
    }

    #[cfg(test)]
    pub fn activation_count(&self) -> usize {
        self.activations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn pause_next_cold_mount_before_final_commit(
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

    #[cfg(any(test, feature = "test-helpers"))]
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

    /// Pause a revision-7 retained-head recovery after it is queryable and
    /// before its dirty-checkout successor starts. This makes the recovery
    /// boundary observable without admitting the successor's partition decode.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn pause_next_retained_graph_recovery_before_successor(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (released, release) = tokio::sync::oneshot::channel();
        let mut gate = retained_graph_recovery_successor_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gate.is_none(),
            "only one retained graph recovery successor gate may be armed at a time"
        );
        *gate = Some(RetainedGraphRecoverySuccessorGateV1 {
            project_root,
            entered,
            release,
        });
        (entered_observed, released)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    async fn wait_for_retained_graph_recovery_successor_gate(project_root: &Path) {
        let gate = {
            let mut armed = retained_graph_recovery_successor_gate()
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
    pub async fn observe_next_existing_semantic_schedule_replacement(
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
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_background_reconcile_permits(max_worktrees: usize, permits: usize) -> Self {
        let mut registry = Self::new(max_worktrees);
        registry.background_reconcile_admission = Arc::new(tokio::sync::Semaphore::new(permits));
        registry
    }

    /// The bounded background-reconcile admission, so a test can occupy it and
    /// hold the worker at its dequeue point while asserting on the pending wake.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn background_reconcile_admission(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.background_reconcile_admission)
    }

    /// Share the bounded background scheduler admission with semantic
    /// evaluation so native model work cannot bypass the project-wide limit.
    pub fn semantic_evaluation_admission(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.background_reconcile_admission)
    }

    /// Test-only observation of an exact mounted worktree's active owner pass.
    #[cfg(test)]
    pub async fn reconcile_in_progress_for_test(&self, project_root: &Path) -> bool {
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

    /// Serving slot only — no Git open, no freshness ladder, no wake.
    #[cfg(test)]
    pub async fn latest_complete_serving_for_test(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let serving = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.serving_generation)
        };
        serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub fn install_cold_mount_admission_barrier(&self, project_root: &Path, callers: usize) {
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
    pub fn install_cold_mount_post_check_gate(&self, project_root: &Path) {
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
    pub async fn wait_for_cold_mount_post_check(&self, project_root: &Path) {
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
    pub fn release_cold_mount_post_check(&self, project_root: &Path) {
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
    pub fn install_cold_mount_open_gate(&self, project_root: &Path) {
        Self::install_cold_mount_open_control(project_root, true);
    }

    #[cfg(test)]
    pub fn install_cold_mount_open_observer(&self, project_root: &Path) {
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
    pub async fn wait_for_cold_mount_open_events(&self, project_root: &Path, events: usize) {
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
    pub async fn wait_for_cold_mount_follower(&self, project_root: &Path) {
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
    pub fn release_cold_mount_open_gate(&self, project_root: &Path) {
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
    pub fn cold_mount_open_events(&self, project_root: &Path) -> Vec<ColdMountOpenEventV1> {
        Self::cold_mount_open_control_for_test(project_root)
            .expect("cold-mount open control")
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub fn subscribe_cold_mount_cancellation(
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
    pub fn install_query_admission_barrier(
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
    pub fn install_query_claim_gate(&self, scope: &tracedecay_application::ResolvedScope) {
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
    pub async fn wait_for_query_claim(&self, scope: &tracedecay_application::ResolvedScope) {
        let control = Self::query_admission_control_for_test(scope).expect("query-claim gate");
        let entered = control.claim_entered.notified();
        if !control.claim_reached.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub fn release_query_claim(&self, scope: &tracedecay_application::ResolvedScope) {
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
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .map(|(_, worktree)| Arc::clone(&worktree.pending_wake))
    }

    #[cfg(test)]
    pub async fn install_pending_wake_drop_gate(
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
    pub async fn wait_for_pending_wake_claim_drop(
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
    pub async fn wait_for_foreign_pending_wake_attempt(
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
    pub async fn release_pending_wake_claim_drop(
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
    pub async fn pending_wake_micros_for_scope(
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
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn clear_pending_wake_for_scope(
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

    /// The exact-source currency witness for one mounted root, so tests can
    /// stage the unproven-seat state a restart restore leaves behind.
    #[cfg(test)]
    pub(crate) async fn serving_source_witness_for_root(
        &self,
        project_root: &Path,
    ) -> Option<Arc<RwLock<Option<super::ServingSourceWitnessV1>>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.serving_source_witness))
    }

    /// Drop the retained serving generation, reproducing a mount whose restore
    /// produced nothing servable.
    #[cfg(test)]
    pub async fn clear_serving_generation_for_scope(
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
                *worktree
                    .text_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                *worktree
                    .serving_source_witness
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                worktree
                    .serving_generation_epoch
                    .fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Atomically marks the exact current serving generation as owned by one
    /// branch publication. A subsequent serving-slot replacement invalidates
    /// this token before rollback can observe it.
    #[hotpath::measure(label = "daemon.code_index.registry.install_serving", future = true)]
    pub async fn install_exact_serving_generation(
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
    pub async fn commit_serving_generation_installation(
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
    pub async fn retire_owned_serving_generation(
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
        let (
            serving_generation,
            serving_epoch,
            active_installation,
            text_generation,
            serving_source_witness,
        ) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return ServingGenerationRollbackOutcomeV1::NoMatch;
            };
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.serving_generation_epoch),
                Arc::clone(&worktree.serving_generation_installation),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.serving_source_witness),
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
            // A retired seat has no currency to witness; a busy read must not
            // serve a slot the rollback just cleared.
            *serving_source_witness
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            // `latest_generation_id` falls through to the text slot when
            // serving is empty. Leaving the retired generation there would
            // keep it publicly addressable after the exact rollback token
            // cleared the serving seat.
            let mut text = text_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if text.as_ref().is_some_and(|current| {
                current.metadata().manifest().generation_id == installation.generation_id
            }) {
                *text = None;
            }
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

    /// Acquire the per-worktree scheduler mutex without parking shutdown behind
    /// a holder. `lock()` would wait out an in-flight test/peer owner; polling
    /// `try_lock` lets the worker observe `shutting_down` and return cancelled.
    fn lock_scheduler_unless_shutting_down<'a>(
        scheduler: &'a Mutex<CodeIndexWorktreeSchedulerV1>,
        shutting_down: &AtomicBool,
    ) -> Result<std::sync::MutexGuard<'a, CodeIndexWorktreeSchedulerV1>, CodeIndexSchedulerErrorV1>
    {
        loop {
            if shutting_down.load(Ordering::Acquire) {
                return Err(super::cancelled_code_index_reconcile());
            }
            match scheduler.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    return Ok(poisoned.into_inner());
                }
            }
        }
    }

    /// Returns the pass's service time so the caller can attach the same
    /// measurement to the canonical index-lifecycle observation.
    /// Project one terminal source-reconcile outcome onto the installed
    /// index lane. Graph seating is optional follow-up: a published generation
    /// must leave this observation even when native-graph activation later
    /// refuses, retries, or is cancelled by shutdown.
    fn record_source_reconcile_observation(
        observability: Option<&super::observability::CodeIndexObservabilityV1>,
        pending_wake: &PendingWakeV1,
        outcome: &CodeIndexReconcileOutcomeV1,
        started_micros: i64,
    ) {
        let Some(observability) = observability else {
            return;
        };
        let service_micros = now_micros().0.saturating_sub(started_micros).max(0) as u64;
        // The pending slot coalesces at most one waiting wake, so the queue
        // behind this pass is empty or singular.
        let queue_depth_bucket = {
            let state = pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.micros == 0 {
                tracedecay_domain::QueueDepthBucketV1::Zero
            } else {
                tracedecay_domain::QueueDepthBucketV1::OneToEight
            }
        };
        observability.record_reconcile_outcome(outcome, service_micros, queue_depth_bucket);
    }

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
        // The cadence receipt is created only after the serving-generation
        // swap, so this is the truthful end-to-end wake-to-queryable sample.
        // An un-attributable follow-up pass remains absent rather than
        // fabricating a zero-latency sample.
        #[cfg(feature = "hotpath")]
        if let Some(ttfq_micros) = receipt.event_to_ready_micros() {
            hotpath::gauge!("daemon.code_index.reconcile.wake_to_queryable_micros")
                .set(ttfq_micros as f64);
        } else {
            hotpath::gauge!("daemon.code_index.reconcile.wake_without_arrival_total").inc(1_u64);
        }
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
    pub fn latest_event_to_ready_receipt(&self) -> Option<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest()
            .cloned()
    }

    /// Every retained event-to-ready receipt, oldest first.
    #[cfg(test)]
    pub fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceiptV1> {
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
    pub fn cadence_read_model(&self) -> CodeIndexCadenceReadModelV1 {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_model()
    }

    pub fn subscribe_generation_publications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1> {
        self.generation_publications.subscribe()
    }

    /// Announce a durable publication.
    ///
    /// Announcing is not seating: the durable pointer has moved, but the
    /// generation becomes addressable through [`Self::latest_generation_id`]
    /// only once a swap installs it in a serving slot.
    fn broadcast_generation_publication(
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
    pub fn open_worktree(
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
        let producer_incarnation = self.mint_progress_producer_incarnation()?;
        CodeIndexWorktreeSchedulerV1::open(
            project_id,
            project_root,
            store_root,
            Arc::clone(&self.byte_pool),
        )
        .map(|mut scheduler| {
            scheduler.bind_resident_memory(Arc::clone(&self.resident_memory));
            scheduler
                .bind_progress_incarnations(self.progress_daemon_incarnation, producer_incarnation);
            scheduler
        })
    }

    #[cfg(test)]
    pub fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
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

    pub async fn mount_worktree_with_graph_runtime(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        graph_runtime: Arc<dyn crate::code_graph_seat::CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        graph_activation_policy: CodeGraphActivationPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Persistent {
                runtime: graph_runtime,
                project_database,
                policy: Arc::new(AtomicBool::new(graph_activation_policy.is_enabled())),
            },
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn mount_worktree(
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
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(true)),
            },
        )
        .await
    }

    #[cfg(test)]
    pub async fn mount_worktree_with_graph_policy(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        policy: CodeGraphActivationPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(policy.is_enabled())),
            },
        )
        .await
    }

    async fn replace_existing_semantic_schedule(
        &self,
        project_root: &Path,
        scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
        serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
        pending_wake: Arc<PendingWakeV1>,
        shutting_down: Arc<AtomicBool>,
        project_id: ProjectId,
        semantic_schedule: Option<
            tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        // Reconcile and tests may hold this mutex. `lock()` would park remount
        // behind that holder and lose the retiring identity: retirement now
        // cancels the worker via try_lock, drains, and leaves remount seeing
        // only "owner changed". Poll try_lock and abort as soon as the owner
        // is shutting down so remount observes "retired while … waited".
        let incumbent = Arc::clone(&scheduler);
        tokio::task::spawn_blocking(move || {
            loop {
                if shutting_down.load(Ordering::Acquire) {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "code-index scheduler owner was retired while semantic schedule update waited; remount must retry"
                            .to_owned(),
                    ));
                }
                let mut scheduler = match scheduler.try_lock() {
                    Ok(guard) => guard,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                };
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
                    let _ = scheduler.schedule_semantic_generation(latest.generation_handle());
                }
                // A replaced hook must not leave the worker parked: the next
                // reconcile (including an edit that raced the remount) needs a
                // wake even when this pass already finished text.
                Self::note_wake(
                    pending_wake.as_ref(),
                    scheduler.wake.as_ref(),
                    CodeIndexCadenceTriggerV1::BusyFollowUp,
                );
                return Ok(());
            }
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
                existing
                    .graph_activation
                    .update_policy(graph_activation.policy());
                let scheduler = Arc::clone(&existing.scheduler);
                let serving_generation = Arc::clone(&existing.serving_generation);
                let pending_wake = Arc::clone(&existing.pending_wake);
                let shutting_down = Arc::clone(&existing.shutting_down);
                drop(mounted);
                drop(retiring);
                #[cfg(test)]
                Self::observe_existing_semantic_schedule_replacement(&project_root);
                self.replace_existing_semantic_schedule(
                    &project_root,
                    scheduler,
                    serving_generation,
                    pending_wake,
                    shutting_down,
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
        let open_resident_memory = Arc::clone(&self.resident_memory);
        let progress_daemon_incarnation = self.progress_daemon_incarnation;
        let progress_producer_incarnation = self.mint_progress_producer_incarnation()?;
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
            opened.bind_resident_memory(open_resident_memory);
            opened.bind_progress_incarnations(
                progress_daemon_incarnation,
                progress_producer_incarnation,
            );
            Ok::<_, CodeIndexSchedulerErrorV1>((opened, cold_mount_reservation))
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!("code-index mount task failed: {error}"))
        })??;
        let repository_id = opened.identity().repository_id().clone();
        let worktree_id = opened.identity().worktree_id().clone();
        let reconcile_in_progress = opened.reconcile_in_progress();
        let generation_recovery = opened.generation_recovery();
        let active_generation_encoded_bytes = opened.active_generation_encoded_bytes();
        let build_progress = opened.build_progress_slot();
        let historical_generation_owner = opened.historical_generation_owner();
        let source_freshness = opened.freshness_fence();
        let last_reconciled_at_micros = opened.last_reconciled_at_micros_slot();
        // Cold mount publishes only the exact route. The worker may seat a
        // complete identity-valid generation as stale serving before refresh
        // claims freshness; missing Git authority still leaves this empty.
        let serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>> =
            Arc::new(RwLock::new(None));
        let text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>> =
            Arc::new(RwLock::new(None));
        let convergence_park: Arc<RwLock<Option<CodeIndexConvergenceParkedV1>>> =
            Arc::new(RwLock::new(None));
        let serving_source_witness: Arc<RwLock<Option<super::ServingSourceWitnessV1>>> =
            Arc::new(RwLock::new(None));
        let serving_generation_epoch = Arc::new(AtomicU64::new(0));
        let serving_generation_installation = Arc::new(Mutex::new(None));
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let scheduler = Arc::new(Mutex::new(opened));
        let build_publication_lock = Arc::new(tokio::sync::Mutex::new(()));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let ignored_dependency_admissions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_wake = Arc::new(PendingWakeV1::default());
        let index_observability =
            Arc::new(OnceLock::<super::observability::CodeIndexObservabilityV1>::new());
        let worker_index_observability = Arc::clone(&index_observability);
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_reconcile_in_progress = Arc::clone(&reconcile_in_progress);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_text_generation = Arc::clone(&text_generation);
        let worker_convergence_park = Arc::clone(&convergence_park);
        let worker_serving_source_witness = Arc::clone(&serving_source_witness);
        let worker_serving_generation_epoch = Arc::clone(&serving_generation_epoch);
        let worker_wake = Arc::clone(&wake);
        // The code-index control epoch. It advances exactly when new input is
        // announced (hook hints, watch paths, overflow), so it is the signal a
        // quarantined worker uses to decide that the bytes which panicked it
        // are no longer the bytes it is being asked to index.
        let worker_control_epoch = Arc::clone(&epoch);
        let worker_pending_wake = Arc::clone(&pending_wake);
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_build_publication_lock = Arc::clone(&build_publication_lock);
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
        #[cfg(any(test, feature = "test-helpers"))]
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
            existing
                .graph_activation
                .update_policy(graph_activation.policy());
            let scheduler = Arc::clone(&existing.scheduler);
            let serving_generation = Arc::clone(&existing.serving_generation);
            let pending_wake = Arc::clone(&existing.pending_wake);
            let shutting_down = Arc::clone(&existing.shutting_down);
            drop(mounted);
            drop(retiring);
            #[cfg(test)]
            Self::observe_existing_semantic_schedule_replacement(&project_root);
            self.replace_existing_semantic_schedule(
                &project_root,
                scheduler,
                serving_generation,
                pending_wake,
                shutting_down,
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
        // Boxed at definition on purpose: this worker's state machine is the
        // largest future in the daemon (reconcile + text advance + decode +
        // activation + swap inline), and an unboxed `let` materializes the
        // whole machine on the spawning runtime worker's stack before
        // `tokio::spawn` can move it to the heap - measured tonight as an
        // instant stack overflow at project mount. `Box::pin` around the
        // async block constructs it into the allocation instead (the same
        // pattern as the `_inner` boxed fns from the 37MB-future fix).
        let worker_loop = Box::pin(async move {
            // Bounded retry state for activating an already-sealed complete
            // generation. The sealed artifact is immutable and retryable, so a
            // retryable activation failure must not fall through into a
            // rebuild+reseal of an equivalent generation.
            let mut seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
            let mut next_seat_attempt_at: Option<Instant> = None;
            // The generation this worker last offered to graph activation
            // outside the replace gate. It bounds the recovery attempt for a
            // generation that serves text without a native graph to one per
            // generation, so a permanently unactivatable seal cannot spin.
            let mut graph_seat_attempted: Option<tracedecay_domain::CodeGenerationId> = None;
            // A retained revision-7 graph gets one verified-head attempt
            // before ordinary source reconciliation owns any repair. A failed
            // verification falls through to one canonical replay of the same
            // sealed generation rather than repeating the head read forever.
            let mut retained_graph_head_recovery_attempted = false;
            // The conflict verdict of the previous failed seat attempt. A
            // Conflict can be a race (a concurrent publisher advanced the
            // head) and is retried once like any transient failure, but the
            // same guard site refusing with identical compared evidence for
            // the same sealed generation on the very next attempt is a
            // deterministic verdict that no backoff can outwait. One repeat
            // converts the retry into the terminal typed refusal below, so
            // the backoff ceiling can never become an infinite conflict loop
            // (issue #765).
            let mut last_seat_conflict: Option<(
                tracedecay_domain::CodeGenerationId,
                tracedecay_graph_db::GraphConflictContextV1,
            )> = None;
            // Bounded retry state for a reconcile whose blocking task unwound.
            // Arbitrary user source runs through the indexing pool, so a panic
            // there is an input fault, not a programmer-contract break: it
            // reproduces on every pass over the same bytes. Without this the
            // loop re-dispatched the identical unit on every wake forever.
            let mut panic_guard = ReconcilePanicGuardV1::new();
            // Bounded retry state for a reconcile refused because shared
            // process capacity was momentarily held by a sibling worktree or
            // artifact build. Releasing that capacity emits no wake, so this
            // worker must schedule its own.
            let mut capacity_retry = ReconcileCapacityRetryV1::new();
            // The last arrival this worker restored for a nothing-seated
            // warming outcome. A quiet remount's seat pass restores its
            // arrival exactly once so the next pass can restore and warm the
            // retained text owner; a worktree with nothing restorable (for
            // example no extractable sources and no publication) reproduces
            // the identical warming Noop on every pass, and restoring the
            // same arrival again would spin this worker forever.
            let mut warming_restore_arrival: Option<i64> = None;
            // Whether the optional graph prepare already yielded to a pending
            // arrival since it last actually ran. One yield lets a landed
            // arrival's exact/lexical pass go first; a second consecutive
            // yield would make "no arrival pending" a seat precondition, which
            // on a busy checkout is the quiet-tree starvation the seat gate
            // rework removed.
            let mut graph_prepare_yielded_to_arrival = false;
            loop {
                hotpath::future!(
                    worker_wake.notified(),
                    label = "daemon.code_index.wake_wait"
                )
                .await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                // A quarantined or backing-off panic unit must not consume the
                // pending arrival: the wake stays outstanding so a later
                // eligible pass still measures its full queue wait.
                if panic_guard.suppresses_pass(
                    tokio::time::Instant::now(),
                    worker_control_epoch.load(Ordering::Acquire),
                ) {
                    tracing::debug!(
                        event = "code_index_reconcile_panic_suppressed",
                        path = "background_worker",
                        consecutive_panics = panic_guard.consecutive_panics(),
                        "code-index reconcile is suppressed after repeated panics over unchanged input"
                    );
                    continue;
                }
                let Ok(_background_reconcile_admission) = hotpath::future!(
                    Arc::clone(&worker_background_reconcile_admission).acquire_owned(),
                    label = "daemon.code_index.admission_wait"
                )
                .await
                else {
                    return;
                };
                let _semantic_evaluation_publication =
                    worker_semantic_evaluation_publication_gate.lock().await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let mut build_publication =
                    std::pin::pin!(Arc::clone(&worker_build_publication_lock).lock_owned());
                let _build_publication = loop {
                    tokio::select! {
                        guard = &mut build_publication => break guard,
                        () = tokio::time::sleep(Duration::from_millis(5)) => {
                            if worker_shutting_down.load(Ordering::Acquire) {
                                return;
                            }
                        }
                    }
                };
                let scheduler = Arc::clone(&worker_scheduler);
                let graph_activation_enabled = worker_graph_activation.policy().is_enabled();
                // A coalesced text-slice wake can outlive the graph-off pass
                // that drained its final overflow. Do not project that bare
                // Notify permit as a second refresh: no arrival and no hint
                // means there is no source work left, while a scheduler-owned
                // raw overflow still reaches the worker through `None` here.
                let graph_off_text_is_settled = !graph_activation_enabled
                    && !worker_pending_wake.has_pending_arrival()
                    && worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready)
                    && match scheduler.try_lock() {
                        Ok(scheduler) => scheduler.pending_hint_count() == Some(0),
                        Err(std::sync::TryLockError::Poisoned(error)) => {
                            error.into_inner().pending_hint_count() == Some(0)
                        }
                        Err(std::sync::TryLockError::WouldBlock) => false,
                    };
                if graph_off_text_is_settled {
                    continue;
                }
                // Cover wake claim through failed-arrival restoration so admission
                // never misreads in-flight owner work as plain unavailability.
                let _reconcile_pass =
                    super::ReconcilePassGuard::enter(&worker_reconcile_in_progress);
                let mut text_generation = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                // A query-driven advance can finish the build between worker
                // passes; a park observed earlier must not outlive the
                // violation it named.
                if text_generation
                    .as_ref()
                    .is_some_and(super::LatestCodeTextGenerationV1::text_serving_is_ready)
                {
                    clear_convergence_park(&worker_convergence_park);
                } else if convergence_park_retries_on_wake(&worker_convergence_park)
                    && text_generation
                        .as_ref()
                        .is_some_and(|latest| !latest.text_serving_needs_work())
                {
                    // A deterministic contract violation latched this text
                    // handle failed, and a latched handle never advances
                    // again. Withdraw it so the ordinary restore below
                    // re-creates a fresh handle from the durable pointer:
                    // every external wake re-checks the parked violation, and
                    // an operator fix (for example chmod 700 on the artifacts
                    // root) is picked up without a remount. The park itself
                    // never self-schedules a wake, so this stays one bounded
                    // retry per ordinary wake, not a hot loop.
                    if let Some(withdrawn) = text_generation.take() {
                        let mut current = worker_text_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if current
                            .as_ref()
                            .is_some_and(|current| current.same_text_owner(&withdrawn))
                        {
                            *current = None;
                        }
                    }
                }
                let mut text_slice_incomplete = false;
                if let Some(latest) = text_generation
                    && latest.text_serving_needs_work()
                {
                    let failed_latest = latest.clone();
                    let build = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            latest.advance_text_serving(TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1)
                        }),
                        label = "daemon.code_index.text_projection"
                    )
                    .await;
                    match build {
                        Ok(Ok(true)) => {
                            clear_convergence_park(&worker_convergence_park);
                        }
                        Ok(Ok(false)) => {
                            clear_convergence_park(&worker_convergence_park);
                            worker_wake.notify_one();
                            text_slice_incomplete = true;
                        }
                        Ok(Err(error)) => {
                            if matches!(
                                &error,
                                tracedecay_query::retrieval::RetrievalPortError::Cancelled
                            ) {
                                let mut current = worker_text_generation
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if current
                                    .as_ref()
                                    .is_some_and(|current| current.same_text_owner(&failed_latest))
                                {
                                    *current = None;
                                }
                            }
                            if error.is_deterministic_contract() {
                                // Unchanged input reproduces this on every
                                // wake, so an unparked WARN would mask it as
                                // warming forever. Park it typed; the wake
                                // cadence still re-checks, so an operator fix
                                // is picked up without a restart.
                                park_convergence(
                                    &worker_convergence_park,
                                    error.to_string(),
                                    CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
                                    true,
                                );
                                tracing::warn!(
                                    event = "code_index_convergence_parked",
                                    path = "background_worker",
                                    error = %error,
                                    "code-index text projection parked on a deterministic \
                                     contract violation; status reports it typed and every \
                                     wake re-checks"
                                );
                            } else {
                                tracing::warn!(
                                    event = "code_index_text_projection_failed",
                                    error = %error,
                                    "code-index background text projection failed"
                                );
                            }
                        }
                        Err(error) => {
                            failed_latest.mark_text_serving_failed();
                            park_convergence(
                                &worker_convergence_park,
                                format!("code text projection task failed abnormally: {error}"),
                                CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                false,
                            );
                            tracing::warn!(
                                event = "code_index_text_projection_task_failed",
                                error = %error,
                                "code-index background text projection task failed"
                            );
                        }
                    }
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                if text_slice_incomplete {
                    if !graph_activation_enabled {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("daemon.code_index.artifact.slice.continue_total")
                            .inc(1_u64);
                        continue;
                    }
                    if Self::incomplete_text_slice_may_continue(&worker_pending_wake) {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("daemon.code_index.artifact.slice.continue_total")
                            .inc(1_u64);
                        continue;
                    }
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.code_index.artifact.slice.yield_to_reconcile_total")
                        .inc(1_u64);
                }
                if worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                {
                    let text_scheduler = Arc::clone(&scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let retained_text = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            Self::lock_scheduler_unless_shutting_down(
                                &text_scheduler,
                                &shutting_down,
                            )
                            .map(|mut scheduler| scheduler.servable_retained_text_generation())
                        }),
                        label = "daemon.code_index.text_restore"
                    )
                    .await;
                    if worker_shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    if let Ok(Ok(Some(retained_text))) = retained_text {
                        *worker_text_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(retained_text);
                        worker_wake.notify_one();
                        continue;
                    }
                }
                let text_serving_ready = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
                // Admission is held: queue wait ends and service time begins.
                let started_micros = now_micros().0;
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake,
                    CodeIndexCadenceTriggerV1::Mount,
                );
                // A retryable native-graph failure defers only graph
                // activation. Reconcile and finish the lightweight text owner
                // before opening the full generation: a large graph replay
                // must never become exact/lexical time-to-ready. During
                // backoff, the scheduled retry is the only pass that may open
                // the immutable full generation again.
                let graph_activation_deferred =
                    next_seat_attempt_at.is_some_and(|at| Instant::now() < at);
                let retained_text = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let serving_empty = worker_serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none();
                let retained_text_uses_partitioned_manifest = retained_text
                    .as_ref()
                    .is_some_and(LatestCodeTextGenerationV1::uses_partitioned_manifest);
                // A revision-7 owner can restore its retained persistent graph
                // directly from the verified head. Give that recovery exactly
                // one empty-slot pass before the dirty source capture creates
                // its successor. Once the retained graph is Ready, this guard
                // falls through to the ordinary reconciliation path instead of
                // repeatedly consuming the successor's wake as a Noop.
                let retained_partitioned_graph_recovery_pending = graph_activation_enabled
                    && !graph_activation_deferred
                    && serving_empty
                    && !retained_graph_head_recovery_attempted
                    && retained_text.as_ref().is_some_and(|text| {
                        text.uses_partitioned_manifest() && text.interactive_graph_store().is_err()
                    });
                let retained_text_metadata =
                    retained_text.as_ref().map(|text| text.metadata().clone());
                let shutting_down = Arc::clone(&worker_shutting_down);
                let source_result = hotpath::future!(
                    tokio::task::spawn_blocking(move || {
                        let mut scheduler =
                            Self::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down)?;
                        // One arrival per attempted pass, before the branch: the
                        // three reconcile entry points below are alternatives, so
                        // hooking them individually would under- or double-count.
                        #[cfg(test)]
                        scheduler.arrive_reconcile_fault_for_test()?;
                        // A prior pass may have built the successor and lost
                        // only the durable write. Republish before choosing a
                        // reconcile branch: graph-off with no text owner goes
                        // to reconcile_now and would otherwise extract again.
                        if let Some(outcome) =
                            scheduler.republish_unpublished_retained_generation()?
                        {
                            return Ok(outcome);
                        }
                        // Legacy graph recovery requires a decoded complete
                        // generation in the serving slot before a dirty-tree
                        // rebuild. Revision 7 deliberately leaves that slot
                        // empty: the retained manifest owner validates and
                        // seats Grafeo below without opening partition bytes.
                        // Graph-off and deferred passes also fall through to
                        // retained text reconciliation.
                        if graph_activation_enabled
                            && !graph_activation_deferred
                            && serving_empty
                            && text_serving_ready
                            && !retained_text_uses_partitioned_manifest
                            && let Some(outcome) =
                                scheduler.seat_retained_generation_on_empty_serving()?
                        {
                            return Ok(outcome);
                        }
                        if retained_partitioned_graph_recovery_pending
                            && let Some(metadata) = retained_text_metadata.as_ref()
                        {
                            // Reserve this pass for verified-head recovery.
                            // `Noop` here means no successor was published;
                            // it never claims source currency. The successor
                            // pass below captures the source state itself so
                            // a quiet remount is not fabricated as dirty.
                            return Ok(CodeIndexReconcileOutcomeV1::Noop(
                                CodeIndexNoopEvidenceV1 {
                                    snapshot_content_identity: metadata
                                        .snapshot()
                                        .content_identity
                                        .clone(),
                                    overflow_reconciled: false,
                                },
                            ));
                        }
                        if let Some(metadata) = retained_text_metadata {
                            match scheduler.reconcile_retained_text_generation_with(
                                &metadata,
                                !graph_activation_enabled,
                            ) {
                                Ok(Some(outcome)) => Ok(outcome),
                                Ok(None) if graph_activation_enabled => {
                                    scheduler.activate_or_reconcile()
                                }
                                Ok(None) => scheduler.reconcile_now(),
                                Err(error) => Err(error),
                            }
                        } else if graph_activation_enabled {
                            scheduler.activate_or_reconcile()
                        } else {
                            scheduler.reconcile_now()
                        }
                    }),
                    // Sealing moved inside this blocking reconcile pipeline.
                    // Keep the outer future labeled so default reports retain
                    // the end-to-end seal path even when short synchronous
                    // inner spans fall below the functions-timing row limit.
                    label = "daemon.code_index.reconcile_or_seal"
                )
                .await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                if let Ok(Ok(CodeIndexReconcileOutcomeV1::Published(evidence))) = &source_result {
                    // Announce only. This pass has not seated anything yet:
                    // the replacement text owner reopens below and the serving
                    // swap runs after graph work, so recording the id here
                    // made `latest_generation_id` name a generation every
                    // serving arm still answered the *previous* id for.
                    Self::broadcast_generation_publication(
                        &worker_generation_publications,
                        worker_project_root.clone(),
                        evidence,
                    );
                }
                // A retained-generation Noop on an empty serving slot consumed
                // the mount wake, so the dirty-checkout successor rebuild never
                // started. Follow-up notify starts that pass, with two
                // bounds. Not during graph-activation backoff: the scheduled
                // retry is the only self-wake then, while external source
                // hints still wake normally. And only for a consumed external
                // arrival: a self-woken pass with no arrival reproduces the
                // identical Noop, and re-notifying it spins a generation-less
                // worktree forever.
                if retained_noop_requires_follow_up_wake(
                    serving_empty,
                    graph_activation_deferred,
                    arrival.wake_micros().is_some(),
                    matches!(&source_result, Ok(Ok(CodeIndexReconcileOutcomeV1::Noop(_)))),
                ) {
                    worker_wake.notify_one();
                }
                if let Ok(Ok(outcome)) = &source_result {
                    Self::record_source_reconcile_observation(
                        worker_index_observability.get(),
                        &worker_pending_wake,
                        outcome,
                        started_micros,
                    );
                }
                // Source reconciliation is complete: release the background
                // admission permit before HeadOpening / graph work so sibling
                // stores can start. Keep `_reconcile_pass` through seating —
                // dropping it made `reconcile_in_progress` lie while this
                // worker still owned graph try_lock, which deadlocked tests
                // that hold the scheduler mutex and wait for that flag.
                drop(_background_reconcile_admission);
                // A publication must first reopen and finish its own
                // lightweight text owner: publication moved the durable
                // pointer, so the prior owner is no longer authoritative even
                // while the new lightweight handle is opening. Withdraw it
                // first - a failed or delayed reopen must report warming, never
                // keep serving the superseded generation indefinitely.
                let published_pass = matches!(
                    &source_result,
                    Ok(Ok(CodeIndexReconcileOutcomeV1::Published(_)))
                );
                let mut graph_text = retained_text.clone();
                if published_pass {
                    *worker_text_generation
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    let text_scheduler = Arc::clone(&worker_scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let published_text = tokio::task::spawn_blocking(move || {
                        Self::lock_scheduler_unless_shutting_down(&text_scheduler, &shutting_down)
                            .map(|mut scheduler| scheduler.servable_retained_text_generation())
                    })
                    .await;
                    if worker_shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    graph_text = if let Ok(Ok(Some(published_text))) = published_text {
                        *worker_text_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(published_text.clone());
                        Some(published_text)
                    } else {
                        None
                    };
                    // The replacement text owner finishes its bounded
                    // projection here, before the optional O(store) graph
                    // decode: exact and lexical serving must never inherit
                    // graph activation latency. Yielding back to the loop
                    // instead would hand the next pass a checkout that has
                    // already moved, and on a shared repository that pass
                    // publishes again - which is exactly how a sealed
                    // generation stayed unseated forever.
                    if graph_activation_enabled
                        && !graph_activation_deferred
                        && let Some(text) = graph_text.clone()
                    {
                        let mut advances = 0_usize;
                        while text.text_serving_needs_work() {
                            if worker_shutting_down.load(Ordering::Acquire) {
                                return;
                            }
                            advances += 1;
                            if advances > TEXT_PROJECTION_MAXIMUM_ACTIVATION_ADVANCES_V1 {
                                tracing::warn!(
                                    event = "code_index_text_projection_advance_bound_reached",
                                    advances = TEXT_PROJECTION_MAXIMUM_ACTIVATION_ADVANCES_V1,
                                    "published text projection did not complete within its \
                                     bounded advance budget; graph seating waits for a later pass"
                                );
                                break;
                            }
                            let advancing = text.clone();
                            match hotpath::future!(
                                tokio::task::spawn_blocking(move || advancing
                                    .advance_text_serving(TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1)),
                                label = "daemon.code_index.text_projection"
                            )
                            .await
                            {
                                Ok(Ok(true)) => {
                                    clear_convergence_park(&worker_convergence_park);
                                    break;
                                }
                                Ok(Ok(false)) => {
                                    clear_convergence_park(&worker_convergence_park);
                                }
                                Ok(Err(error)) => {
                                    if error.is_deterministic_contract() {
                                        park_convergence(
                                            &worker_convergence_park,
                                            error.to_string(),
                                            CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
                                            true,
                                        );
                                        tracing::warn!(
                                            event = "code_index_convergence_parked",
                                            path = "background_worker",
                                            error = %error,
                                            "published text projection parked on a deterministic \
                                             contract violation before graph seating; status \
                                             reports it typed and every wake re-checks"
                                        );
                                    } else {
                                        tracing::warn!(
                                            event = "code_index_text_projection_failed",
                                            error = %error,
                                            "published text projection failed before graph seating"
                                        );
                                    }
                                    break;
                                }
                                Err(error) => {
                                    text.mark_text_serving_failed();
                                    park_convergence(
                                        &worker_convergence_park,
                                        format!(
                                            "code text projection task failed abnormally: {error}"
                                        ),
                                        CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                        false,
                                    );
                                    tracing::warn!(
                                        event = "code_index_text_projection_task_failed",
                                        error = %error,
                                        "published text projection task failed before graph seating"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    let published_text_finished = graph_text
                        .as_ref()
                        .is_some_and(|text| !text.text_serving_needs_work());
                    if !published_text_finished {
                        worker_wake.notify_one();
                    }
                }
                // Graph seating must not wait for the checkout to hold still.
                // Requiring a `Noop` outcome made tree quiescence the seat
                // condition, and a shared checkout with peers editing never
                // offers that window: every pass published a new generation, so
                // three full seals produced zero seat attempts and every exact
                // graph read answered "not ready" while a complete sealed
                // generation sat on disk. A publication now seats on its own
                // pass, once its lightweight text owner has reopened and
                // finished; it is stale by construction and superseded by the
                // next publication. An unchanged pass still seats the retained
                // Ready text owner's generation. Source reconciliation is
                // complete either way: release its public freshness guard
                // before the optional O(store) full decode and native graph
                // activation begin. Keep the pass through text seating so
                // `reconcile_in_progress` stays truthful while this worker
                // still owns source/text work; optional graph must not.
                drop(_reconcile_pass);
                let gate = GraphSeatGateV1::decide(
                    graph_activation_enabled,
                    graph_activation_deferred,
                    matches!(&source_result, Ok(Ok(_))),
                    published_pass,
                    text_serving_ready,
                    graph_text
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready),
                );
                // An arrival that landed during this retained pass is
                // exact/lexical work waiting for the worker. The optional
                // graph prepare parks this worker on an O(store) sealed
                // decode — tens of seconds on a cold large repository — and
                // serving that arrival must never queue behind it. The
                // arrival's own notify re-runs this worker and the follow-up
                // pass re-gates Prepare from its own terminal outcome, so
                // seating is deferred, never lost. Two bounds keep this a
                // deferral rather than a quiet-wake seat precondition: a
                // published pass never yields (its seat is the pass's own
                // product, and a continuously edited tree has an arrival
                // pending on nearly every pass), and a retained pass yields at
                // most once per prepare so an arrival storm cannot starve
                // seating indefinitely.
                let arrival_pending_before_graph_prepare = gate == GraphSeatGateV1::Prepare
                    && !published_pass
                    && !graph_prepare_yielded_to_arrival
                    && worker_pending_wake.has_pending_arrival();
                if arrival_pending_before_graph_prepare {
                    graph_prepare_yielded_to_arrival = true;
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason = "arrival_pending",
                        published_pass,
                        "an arrival is pending; the optional graph decode yields this pass"
                    );
                }
                let mut prepare_graph =
                    gate == GraphSeatGateV1::Prepare && !arrival_pending_before_graph_prepare;
                if prepare_graph {
                    graph_prepare_yielded_to_arrival = false;
                }
                if gate == GraphSeatGateV1::RetainedTextOwnerWarming && !published_pass {
                    let text_empty = worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    let serving_empty = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    if text_empty
                        && serving_empty
                        && arrival.wake_micros().is_some()
                        && warming_restore_arrival != arrival.wake_micros()
                    {
                        // Restore the arrival so warming is not terminal, then
                        // fall through to record this Noop. `continue` skipped
                        // the receipt and left latest_generation_id observers
                        // without event-to-ready evidence. One restore per
                        // arrival: a second identical warming pass proves
                        // nothing became restorable, so the arrival drains
                        // instead of respinning this worker.
                        warming_restore_arrival = arrival.wake_micros();
                        Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                        worker_wake.notify_one();
                    }
                }
                if let Some(reason) = gate.skip_reason() {
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason,
                        published_pass,
                        "graph seating skipped this pass; the sealed generation stays unseated"
                    );
                }
                if !prepare_graph {
                    let serving_empty = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    let text_empty = worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    // A warming terminal source outcome with nothing seated is
                    // not done: restore the arrival so the next pass can finish
                    // text instead of sleeping until an unrelated hint. Failed
                    // source outcomes already restore in the error arm; waking
                    // those here would spin while Git is unavailable. The same
                    // one-restore-per-arrival bound applies: when the follow-up
                    // pass restored nothing and reconciled to the identical
                    // outcome, the arrival drains rather than respinning.
                    if serving_empty
                        && text_empty
                        && arrival.wake_micros().is_some()
                        && warming_restore_arrival != arrival.wake_micros()
                        && matches!(&source_result, Ok(Ok(_)))
                    {
                        warming_restore_arrival = arrival.wake_micros();
                        Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                        worker_wake.notify_one();
                    }
                }
                // A text owner that already serves a native graph must not be
                // activated a second time, but preparation is not activation.
                // Clearing `prepare_graph` here also skipped the bind and the
                // serving swap, so a restart that restored a Ready retained
                // graph left `serving_generation` empty forever: every
                // complete-generation demand (`latest_complete_ready`,
                // `latest_complete_fresh`, `latest_complete_serving_for_scope`)
                // answered unavailable while `dashboard_code_graph_serving`
                // read the same text owner and reported Ready. Prepare, bind
                // and seat as usual; only the activation call is suppressed.
                let graph_already_serves = graph_text
                    .as_ref()
                    .is_some_and(|retained| retained.interactive_graph_store().is_ok());
                if prepare_graph
                    && !published_pass
                    && !retained_graph_head_recovery_attempted
                    && let Some(retained) = graph_text
                        .as_ref()
                        .filter(|retained| retained.uses_partitioned_manifest())
                        .cloned()
                {
                    retained_graph_head_recovery_attempted = true;
                    let generation_id = retained.metadata().manifest().generation_id.clone();
                    let replay_scheduler = Arc::clone(&worker_scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let replay_binding = tokio::task::spawn_blocking(move || {
                        Self::lock_scheduler_unless_shutting_down(
                            &replay_scheduler,
                            &shutting_down,
                        )?
                        .code_graph_replay_binding(&generation_id)
                    })
                    .await;
                    match replay_binding {
                        Ok(Ok(replay_binding)) => {
                            match worker_graph_activation
                                .recover_verified_head(
                                    &worker_project_id,
                                    &worker_repository_id,
                                    &worker_worktree_id,
                                    retained,
                                    replay_binding,
                                    Arc::clone(&worker_shutting_down),
                                )
                                .await
                            {
                                Ok(true) => {
                                    prepare_graph = false;
                                    tracing::info!(
                                        event = "code_index_graph_head_recovered",
                                        "revision-7 manifest matched the durable verified graph \
                                         head; startup seated graph reads without replay"
                                    );
                                    #[cfg(any(test, feature = "test-helpers"))]
                                    Self::wait_for_retained_graph_recovery_successor_gate(
                                        &worker_project_root,
                                    )
                                    .await;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::warn!(
                                        event = "code_index_graph_head_recovery_degraded",
                                        error = %error,
                                        "verified graph head did not match the revision-7 \
                                         manifest; replay the exact sealed generation to \
                                         repair its quarantined graph projection"
                                    );
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "code_index_graph_head_recovery_binding_unavailable",
                                error = %error,
                                "revision-7 replay binding is unavailable; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "code_index_graph_head_recovery_task_failed",
                                error = %error,
                                "revision-7 replay binding task failed; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
                    }
                    // The reserved pass deliberately did not capture the
                    // checkout, and it consumed whatever wake ran it. Schedule
                    // the successor for every outcome of the attempt, not only
                    // the recovered one: an abstaining or degraded attempt
                    // leaves exactly the same uncaptured source behind, and
                    // the reservation is armed once per worker, so a quiet
                    // reserved pass that answered `Noop` for a checkout with
                    // hook hints already pending stranded them with no wake at
                    // all and never published the successor generation. The
                    // `retained_graph_head_recovery_attempted` guard above is
                    // now false for every later pass, so this cannot spin
                    // another retained-recovery Noop.
                    worker_wake.notify_one();
                }
                let mut result = match source_result {
                    Ok(mut outcome) if prepare_graph => {
                        let graph_scheduler = Arc::clone(&worker_scheduler);
                        let graph_text = graph_text.clone();
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        match hotpath::future!(
                            tokio::task::spawn_blocking(move || {
                                let decoder = Self::lock_scheduler_unless_shutting_down(
                                    &graph_scheduler,
                                    &shutting_down,
                                )?
                                .active_generation_decoder();
                                if decoder.is_none() {
                                    tracing::warn!(
                                        event = "code_index_graph_prepare_no_decoder",
                                        "graph prepare found no active generation decoder; \
                                         the sealed generation cannot seat"
                                    );
                                }
                                let generation = decoder.and_then(|decoder| {
                                    match decoder.load_active_shared() {
                                        Ok(generation) => generation,
                                        Err(error) => {
                                            tracing::warn!(
                                                event = "code_index_graph_prepare_load_failed",
                                                error = %error,
                                                "active generation decode failed; \
                                                 the sealed generation cannot seat"
                                            );
                                            None
                                        }
                                    }
                                });
                                let latest = match generation {
                                    Some(generation) => Self::lock_scheduler_unless_shutting_down(
                                        &graph_scheduler,
                                        &shutting_down,
                                    )?
                                    .servable_decoded_retained_generation(
                                        generation,
                                        graph_text.as_ref(),
                                    ),
                                    None => None,
                                };
                                let replay_binding = match latest.as_ref() {
                                    Some(latest) => Some(
                                        Self::lock_scheduler_unless_shutting_down(
                                            &graph_scheduler,
                                            &shutting_down,
                                        )?
                                        .code_graph_replay_binding(
                                            &latest.generation().manifest().generation_id,
                                        ),
                                    ),
                                    None => None,
                                };
                                replay_binding.transpose().map(|binding| (latest, binding))
                            }),
                            label = "daemon.code_index.graph_prepare"
                        )
                        .await
                        {
                            Ok(Ok((latest, replay_binding))) => {
                                if latest.is_none() {
                                    tracing::warn!(
                                        event = "code_index_graph_prepare_no_servable_generation",
                                        published_pass,
                                        "graph prepare produced no servable generation; \
                                         the sealed generation cannot seat"
                                    );
                                }
                                Ok((outcome, latest, replay_binding))
                            }
                            Ok(Err(error)) => {
                                outcome = Err(error);
                                Ok((outcome, None, None))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Ok(outcome) => Ok((outcome, None, None)),
                    Err(error) => Err(error),
                };
                let replace_serving_generation = match &result {
                    Ok((Ok(CodeIndexReconcileOutcomeV1::Noop(_)), Some(latest), Some(_))) => {
                        let serving = worker_serving_generation
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        serving.as_ref().is_none_or(|serving| {
                            serving.generation().manifest().generation_id
                                != latest.generation().manifest().generation_id
                        })
                    }
                    Ok((Ok(_), Some(_), Some(_))) => true,
                    _ => false,
                };
                // Activation is decided independently of the seat: every arm
                // below refuses only the native graph call, and the serving
                // swap still installs the prepared generation.
                let activate_graph = match &result {
                    Ok((Ok(_), Some(latest), Some(_))) => GraphActivationGateV1::decide(
                        graph_already_serves,
                        replace_serving_generation,
                        latest.graph_activation_is_pending(),
                        graph_seat_attempted.as_ref()
                            == Some(&latest.generation().manifest().generation_id),
                    )
                    .activates(),
                    _ => false,
                };
                if activate_graph && let Ok((Ok(_), Some(latest), Some(replay_binding))) = &result {
                    graph_seat_attempted =
                        Some(latest.generation().manifest().generation_id.clone());
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
                    match activation {
                        Ok(()) => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
                        }
                        Err(error) if error.is_graph_activation_refusal() => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
                            tracing::warn!(
                                event = "code_index_graph_activation_refused",
                                error = %error,
                                "code-index generation remains text-serving without native graph"
                            );
                        }
                        Err(error) => {
                            let seat_generation_id =
                                latest.generation().manifest().generation_id.clone();
                            let repeated_conflict = is_repeated_conflict_verdict(
                                &error,
                                &seat_generation_id,
                                last_seat_conflict.as_ref(),
                            );
                            // The generation just sealed is complete; a retryable
                            // activation failure arms the same seat backoff so the
                            // next passes retry this artifact instead of resealing.
                            // A conflict verdict identical to the previous
                            // attempt's for this same generation is deterministic
                            // and falls through to the terminal arm instead.
                            if error.is_retryable_activation() && !repeated_conflict {
                                last_seat_conflict = error
                                    .activation_conflict_context()
                                    .map(|context| (seat_generation_id, context.clone()));
                                next_seat_attempt_at = Some(Instant::now() + seat_retry_backoff);
                                let retry_wake = Arc::clone(&worker_wake);
                                let retry_delay = seat_retry_backoff;
                                tokio::spawn(async move {
                                    tokio::time::sleep(retry_delay).await;
                                    retry_wake.notify_one();
                                });
                                tracing::warn!(
                                    event = "code_index_graph_activation_retry_scheduled",
                                    retry_delay_micros = retry_delay.as_micros() as u64,
                                    error = %error,
                                    "graph activation failed retryably; the sealed generation \
                                     stays unseated until the scheduled retry"
                                );
                                hotpath::gauge!("daemon.code_index.graph_seat.retry_total")
                                    .inc(1_u64);
                                hotpath::gauge!(
                                    "daemon.code_index.graph_seat.retry_backoff_micros"
                                )
                                .set(retry_delay.as_micros() as u64);
                                seat_retry_backoff = seat_retry_backoff
                                    .saturating_mul(2)
                                    .min(ACTIVATION_RETRY_BACKOFF_CEILING);
                                // The scheduled retry is the seat attempt, so it
                                // must not be turned away as already attempted.
                                graph_seat_attempted = None;
                                result = Ok((Err(error), None, None));
                            } else {
                                next_seat_attempt_at = None;
                                seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                                last_seat_conflict = None;
                                latest.mark_graph_activation_unavailable(error.to_string());
                                if repeated_conflict {
                                    tracing::warn!(
                                        event = "code_index_graph_activation_conflict_terminal",
                                        error = %error,
                                        "graph activation repeated an identical conflict \
                                         verdict for the same sealed generation; retrying \
                                         cannot succeed, so the generation serves exact and \
                                         lexical with typed graph unavailability"
                                    );
                                } else {
                                    tracing::warn!(
                                        event = "code_index_graph_activation_failed",
                                        error = %error,
                                        "graph activation failed terminally; exact and lexical \
                                         serving remain available with typed graph unavailability"
                                    );
                                }
                            }
                        }
                    }
                }
                if let Ok((Ok(_), Some(latest), _)) = &result {
                    let scheduler = Arc::clone(&worker_scheduler);
                    let serving_generation = Arc::clone(&worker_serving_generation);
                    let serving_generation_epoch = Arc::clone(&worker_serving_generation_epoch);
                    let serving_source_witness = Arc::clone(&worker_serving_source_witness);
                    let text_generation = Arc::clone(&worker_text_generation);
                    let text_latest = latest.clone();
                    let latest = latest.clone();
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let serving_swap = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            let scheduler = Self::lock_scheduler_unless_shutting_down(
                                &scheduler,
                                &shutting_down,
                            )?;
                            // A generation that sealed while the checkout kept
                            // moving is stale the moment it completes, and the
                            // durable pointer may already name its successor.
                            // Refusing the swap outright then left the route
                            // serving nothing at all, so an empty serving slot
                            // takes the stale seat and the next publication
                            // supersedes it; a slot that already serves keeps what
                            // it has rather than moving backwards.
                            let publication_matches =
                                scheduler.active_publication_matches(&latest)?;
                            let mut serving = serving_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let outcome = ServingSwapOutcomeV1::decide(
                                publication_matches,
                                serving.is_some(),
                                replace_serving_generation,
                            );
                            if outcome.installs() {
                                *serving = Some(latest.clone());
                                serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
                                *text_generation
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(latest.text_generation_handle());
                                // Only a generation extracted from the live
                                // checkout this pass and seated as the active
                                // publication carries its pass's freshness
                                // proof into the witness. A stale seat and a
                                // retained/restored seat stay unproven until
                                // the quiet exact-source probe passes, so busy
                                // verified reads never serve bytes no proof
                                // has vouched for.
                                *serving_source_witness
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    if published_pass
                                        && matches!(outcome, ServingSwapOutcomeV1::Seated)
                                    {
                                        scheduler.source_currency_witness_for(
                                            &latest.generation().manifest().generation_id,
                                        )
                                    } else {
                                        None
                                    };
                            }
                            drop(serving);
                            // Semantic admission is independently retryable. A
                            // prior attempt may have lost bounded queue capacity,
                            // so an unchanged reconcile must offer the already-
                            // serving generation again without reinstalling it.
                            let _ =
                                scheduler.schedule_semantic_generation(latest.generation_handle());
                            Ok::<_, CodeIndexSchedulerErrorV1>(outcome)
                        }),
                        label = "daemon.code_index.serving_swap"
                    )
                    .await;
                    match serving_swap {
                        Ok(Ok(outcome)) => {
                            let generation_id = text_latest
                                .generation()
                                .manifest()
                                .generation_id
                                .as_str()
                                .to_owned();
                            match outcome {
                                ServingSwapOutcomeV1::Seated => tracing::debug!(
                                    event = "code_index_serving_generation_seated",
                                    generation_id,
                                    "the sealed generation now serves"
                                ),
                                ServingSwapOutcomeV1::SeatedStale => tracing::info!(
                                    event = "code_index_serving_generation_seated_stale",
                                    generation_id,
                                    "the sealed generation seats stale: the durable pointer \
                                     already names its successor"
                                ),
                                ServingSwapOutcomeV1::Superseded => tracing::warn!(
                                    event = "code_index_serving_swap_superseded",
                                    generation_id,
                                    "the reconciled generation is no longer the active durable \
                                     publication; the seated generation keeps serving"
                                ),
                                ServingSwapOutcomeV1::Offered => {}
                            }
                            if text_latest.text_serving_needs_work() {
                                worker_wake.notify_one();
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "code_index_serving_swap_failed",
                                error = %error,
                                "the serving swap refused the reconciled generation"
                            );
                            result = Ok((Err(error), None, None));
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "code_index_serving_swap_task_failed",
                                error = %error,
                                "the serving-swap task failed; the sealed generation stays unseated"
                            );
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
                    // A pass that ran to a terminal outcome proves neither the
                    // panicking input nor the capacity contention is still
                    // reproducing, so both bounded retry states restart.
                    panic_guard.record_progress();
                    capacity_retry.record_progress();
                    let _service_micros = Self::record_reconcile_receipt(
                        &worker_cadence_telemetry,
                        worker_project_root.clone(),
                        arrival,
                        trigger,
                        started_micros,
                        outcome,
                    );
                } else {
                    // Surface bounded non-terminal failure without new project-path data.
                    match &result {
                        Ok((Err(error), _, _)) => {
                            // The pass completed; whatever refused it was not an
                            // unwind, so panic accounting restarts.
                            panic_guard.record_progress();
                            let transient_capacity = error.is_transient_capacity_failure();
                            tracing::warn!(
                                event = "code_index_reconcile_failed",
                                path = "background_worker",
                                transient_capacity,
                                error = %error,
                                "code-index background reconcile failed; the served generation stays stale"
                            );
                            if transient_capacity {
                                // Shared process capacity was held by another
                                // holder when this pass asked for it. Releasing
                                // it emits no wake, so without a self-scheduled
                                // retry this worktree stayed stale until some
                                // unrelated query or edit happened to wake it.
                                // Permanent refusals deliberately never reach
                                // here: retrying those forever is the failure
                                // this loop already had.
                                match capacity_retry.record_capacity_failure() {
                                    Some(delay) => {
                                        let retry_wake = Arc::clone(&worker_wake);
                                        tokio::spawn(async move {
                                            tokio::time::sleep(delay).await;
                                            retry_wake.notify_one();
                                        });
                                    }
                                    None => tracing::warn!(
                                        event = "code_index_reconcile_capacity_retry_exhausted",
                                        path = "background_worker",
                                        consecutive = capacity_retry.consecutive(),
                                        "code-index reconcile stopped retrying a capacity refusal; the next hint retries"
                                    ),
                                }
                            } else {
                                capacity_retry.record_progress();
                            }
                        }
                        Err(error) if error.is_panic() => {
                            // Arbitrary user source runs through the indexing
                            // pool, so an unwind here is malformed input that
                            // reproduces byte-for-byte on every later pass.
                            // Bound it instead of re-dispatching the identical
                            // unit on every wake.
                            capacity_retry.record_progress();
                            let decision = panic_guard.record_panic(
                                tokio::time::Instant::now(),
                                worker_control_epoch.load(Ordering::Acquire),
                            );
                            match decision {
                                ReconcilePanicDecisionV1::RetryAfter(delay) => {
                                    tracing::warn!(
                                        event = "code_index_reconcile_panicked",
                                        path = "background_worker",
                                        consecutive_panics = panic_guard.consecutive_panics(),
                                        error = %error,
                                        "code-index background reconcile panicked; retrying the same input with backoff"
                                    );
                                    let retry_wake = Arc::clone(&worker_wake);
                                    tokio::spawn(async move {
                                        tokio::time::sleep(delay).await;
                                        retry_wake.notify_one();
                                    });
                                }
                                ReconcilePanicDecisionV1::Quarantine => tracing::warn!(
                                    event = "code_index_reconcile_quarantined",
                                    path = "background_worker",
                                    consecutive_panics = panic_guard.consecutive_panics(),
                                    error = %error,
                                    "code-index background reconcile is quarantined after repeated panics; changed input or a progressing pass resumes it"
                                ),
                            }
                        }
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
                let _ = result;
            }
        });
        let task = tokio::spawn(hotpath::future!(
            worker_loop,
            label = "daemon.code_index.scheduler_worker"
        ));
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
            build_publication_lock,
            historical_generation_owner,
            serving_generation,
            source_freshness,
            last_reconciled_at_micros,
            text_generation,
            convergence_park,
            generation_recovery,
            serving_source_witness,
            build_progress,
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
    pub async fn mount_query_authority(
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

    /// Seat the core query fallback while an exact committed semantic
    /// activation is still warming. Unlike a standalone mount, this preserves
    /// the committed revision fence and never replaces an already-usable query
    /// authority (including one installed by a completed activation).
    pub async fn mount_query_authority_for_committed_fallback(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        expected_revision: &ConfigurationRevisionId,
        authority: Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount committed query fallback before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "committed query fallback scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation =
            tracedecay_usecases::semantic_runtime::project_semantic_activation_gate(&project_root);
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() != Some(expected_revision) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "committed query fallback revision is no longer desired".to_owned(),
            ));
        }
        if worktree.query_authority.is_none() {
            worktree.query_authority = Some((scope.scope_digest.clone(), authority));
        }
        Ok(())
    }

    /// Install the observability lane for one mounted worktree. Installation
    /// is once per mount: a repeated install against the same mounted worktree
    /// keeps the incumbent lane, and a worktree that is not mounted is a typed
    /// error so the caller can log the absence.
    pub async fn install_index_observability(
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
    pub async fn index_observability_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::observability::CodeIndexObservabilityV1> {
        let mounted = self.mounted.lock().await;
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .and_then(|(_, worktree)| worktree.index_observability.get().cloned())
    }

    /// Install the core and optional semantic query routes as one committed
    /// configuration observation. The provider CAS is repeated while the
    /// mounted-worktree lock is held, so a delayed observer cannot publish a
    /// stale authority pair after a newer committed revision.
    pub async fn begin_committed_query_activation(
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
        let mut exact_retry = false;
        if let Some(desired_epoch) = worktree.query_activation_epoch {
            let advances = epoch > desired_epoch;
            exact_retry = epoch == desired_epoch
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
        if !exact_retry {
            worktree.semantic_query_authority = None;
            tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                project_root,
                prepared_redundancy,
                false,
            );
        }
        Ok(QueryActivationAttemptV1 {
            revision: result_revision.clone(),
            token: worktree.query_activation_attempt,
            preserves_existing_authority: exact_retry,
        })
    }

    #[hotpath::measure(
        label = "daemon.code_index.registry.install_query_authorities",
        future = true
    )]
    pub async fn install_committed_query_authorities(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        commit_prepared: impl FnOnce() -> Result<(), String>,
        prepared: crate::ports::PreparedQueryActivationViewV1,
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
                if !attempt.preserves_existing_authority {
                    worktree.semantic_query_authority = None;
                    worktree.query_activation_revision =
                        Some(prepared.configuration_revision().clone());
                    tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                        project_root.clone(),
                        &prepared_redundancy,
                        false,
                    );
                }
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
        if let Err(error) = commit_prepared() {
            if !attempt.preserves_existing_authority {
                worktree.semantic_query_authority = None;
                worktree.query_activation_revision =
                    Some(prepared.configuration_revision().clone());
                tracedecay_usecases::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                    project_root.clone(),
                    &prepared_redundancy,
                    false,
                );
            }
            return Err(CodeIndexSchedulerErrorV1::Identity(error));
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
    pub async fn clear_failed_query_activation(
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
            if attempt.preserves_existing_authority {
                return Ok(false);
            }
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

    #[hotpath::measure(label = "daemon.code_index.registry.query_authority", future = true)]
    pub async fn query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<tracedecay_query::retrieval::QueryAuthorityV1>> {
        self.activate_for_scope(scope);
        let mounted = self.mounted.lock().await;
        // Same worktree isolation as `latest_matches_scope_identity`: a
        // mid-session ref switch keeps the mounted ranking authority until
        // the route remounts. Exact digest is a remount key, not a reason
        // to deny search after HEAD moved.
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .and_then(|(_, worktree)| {
                worktree
                    .query_authority
                    .as_ref()
                    .map(|(_scope_digest, authority)| Arc::clone(authority))
            })
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn has_query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        self.query_authority_for_scope(scope).await.is_some()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn query_authority_installation_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<(bool, bool, Option<ConfigurationRevisionId>)> {
        let mounted = self.mounted.lock().await;
        let (_, worktree) = unique_mounted_for_scope(&mounted, scope).unique()?;
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
    pub async fn scope_retention_mounted_roots(&self) -> Result<BTreeSet<PathBuf>, &'static str> {
        let mounted = self.mounted.lock().await;
        if mounted.len() > self.max_worktrees {
            return Err("mounted_root_inventory_exceeds_bound");
        }
        Ok(mounted.keys().cloned().collect())
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// Run the bounded Git/stat freshness ladder for an ordinary read without
    /// manufacturing an overflow. Only a proven source change posts a query
    /// admission wake; a matching stat signature refreshes the scheduler's
    /// cadence watermark and returns without traversal.
    pub async fn probe_freshness(&self, project_root: &Path) -> bool {
        self.diagnostics_change_generation(project_root)
            .await
            .is_some()
    }

    /// Return the mounted scheduler's canonical worktree-change generation.
    ///
    /// The generation is exactly as fresh as the index used by search: hook
    /// hints and Git metadata changes are observed immediately, while other
    /// out-of-band edits are observed by the 30-second stat-signature ladder.
    /// Until that ladder runs, callers intentionally receive the preceding
    /// generation and must not derive a parallel workspace fingerprint.
    pub async fn diagnostics_change_generation(&self, project_root: &Path) -> Option<u64> {
        let Ok(project_root) = project_root.canonicalize() else {
            return None;
        };
        let (scheduler, wake, pending_wake, reconcile_in_progress, epoch) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.epoch),
            )
        };
        if pending_wake.has_pending_arrival() || reconcile_in_progress.load(Ordering::Acquire) != 0
        {
            return Some(epoch.load(Ordering::Acquire));
        }
        tokio::task::spawn_blocking(move || {
            let mut scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if scheduler.request_fresh_for_query_background() {
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            epoch.load(Ordering::Acquire)
        })
        .await
        .ok()
    }

    /// Mounted scope identity plus the currently serving generation for one
    /// project. Daemon authorities that must retain this scope's code-graph
    /// runtime (semantic vectors, generation retention) resolve through this
    /// read instead of re-deriving repository/worktree identity themselves.
    pub async fn serving_code_scope(&self, project_root: &Path) -> Option<CodeIndexServingScopeV1> {
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

    #[hotpath::measure(label = "daemon.code_index.registry.install_semantic", future = true)]
    pub async fn install_semantic_vector_graph_provider(
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

    pub async fn semantic_vector_graph_provider(
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

    /// Resolve one sealed generation's replay binding without joining the
    /// scheduler mutex. A background reconcile owns that mutex for its whole
    /// pass — sealing a production-scale corpus holds it for minutes — and
    /// the binding is an immutable publication read the retained historical
    /// owner answers directly, so blocking here parked the caller (and its
    /// runtime worker thread) behind work the read never needed.
    #[hotpath::measure(label = "daemon.code_index.registry.replay_binding", future = true)]
    pub async fn code_graph_replay_binding(
        &self,
        project_root: &Path,
        generation: &CodeGenerationId,
    ) -> Option<Result<super::CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1>> {
        let project_root = project_root.canonicalize().ok()?;
        let historical = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)?
                .historical_generation_owner
                .clone()
        };
        let generation = generation.clone();
        Some(
            tokio::task::spawn_blocking(move || historical.sealed_replay_binding(&generation))
                .await
                .unwrap_or_else(|error| {
                    Err(CodeIndexSchedulerErrorV1::Identity(format!(
                        "sealed replay-binding read task failed: {error}"
                    )))
                }),
        )
    }

    /// Load one exact code generation through its durable publication store,
    /// including a source generation superseded in process-local retention.
    pub async fn published_generation(
        &self,
        project_root: &Path,
        generation_id: &CodeGenerationId,
    ) -> Option<Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexSchedulerErrorV1>>
    {
        let project_root = project_root.canonicalize().ok()?;
        let owner = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)?
                .historical_generation_owner
                .clone()
        };
        let generation_id = generation_id.clone();
        Some(
            tokio::task::spawn_blocking(move || owner.published_generation(&generation_id))
                .await
                .unwrap_or_else(|error| {
                    Err(CodeIndexSchedulerErrorV1::Identity(format!(
                        "published-generation read task failed: {error}"
                    )))
                }),
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
        let (serving, text) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
            )
        };
        // Only a seat answers here. A durable publication moves the pointer
        // long before either swap installs the generation it sealed, and a
        // slot fed from that broadcast named a generation every serving arm
        // still answered the *previous* id for: a caller that polled for a
        // changed id and then asked for the generation was handed the one it
        // had already seen. The complete serving slot is the swap's own
        // witness, so it answers first; the text slot covers a graph-off or
        // still-activating mount that deliberately leaves the complete slot
        // empty.
        let serving_id = serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| latest.generation.manifest().generation_id.clone());
        if serving_id.is_some() {
            return serving_id;
        }
        text.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| latest.metadata().manifest().generation_id.clone())
    }

    /// Re-offer the exact serving generation to its installed semantic hook.
    ///
    /// Model selection is deliberately background work and can settle after
    /// text/graph publication first offered this generation. That first offer
    /// truthfully refuses while the artifact is unavailable; selection
    /// completion calls this bounded retry so an unchanged repository does
    /// not need another source reconciliation before vector indexing starts.
    #[hotpath::measure(
        label = "daemon.code_index.semantic_generation_reschedule",
        future = true
    )]
    pub async fn reschedule_semantic_generation(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (scheduler, shutting_down, generation) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            let generation = worktree
                .serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(LatestCompleteCodeIndexV1::generation_handle);
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.shutting_down),
                generation,
            )
        };
        let Some(generation) = generation else {
            return false;
        };
        tokio::task::spawn_blocking(move || {
            let scheduler =
                Self::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down).ok()?;
            Some(scheduler.schedule_semantic_generation(generation))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    }

    /// Exact bounded dashboard projection for one mounted worktree.
    ///
    /// This is a status read, not a query-admission boundary: it reports the
    /// last scheduler execution state and never runs a freshness probe, opens
    /// Git, scans the worktree, publishes a generation, or posts a wake.
    /// Generation and scope fields are copied from the last sealed generation,
    /// never reconstructed from the dashboard's display path.
    pub async fn dashboard_freshness(
        &self,
        project_root: &Path,
    ) -> Option<tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1>
    {
        let canonical_root = project_root.canonicalize().ok()?;
        let (
            scheduler,
            reconcile_in_progress,
            serving_generation,
            last_reconciled_at_micros,
            text_generation,
            convergence_park,
            generation_recovery,
            build_progress,
            hints,
            pending_wake,
            graph_activation_enabled,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.last_reconciled_at_micros),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.convergence_park),
                Arc::clone(&worktree.generation_recovery),
                Arc::clone(&worktree.build_progress),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.pending_wake),
                worktree.graph_activation.policy().is_enabled(),
            )
        };
        tokio::task::spawn_blocking(move || {
            let progress = hotpath::measure_block!("daemon.code_index.dashboard.progress", {
                let progress = build_progress
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .snapshot()
                    .map(|snapshot| snapshot.as_ref().clone());
                #[cfg(feature = "hotpath")]
                if let Some(progress) = progress.as_ref() {
                    let age_micros = now_micros()
                        .0
                        .saturating_sub(progress.last_progress_micros)
                        .max(0);
                    hotpath::gauge!("daemon.code_index.dashboard.progress_age_micros")
                        .set(u64::try_from(age_micros).unwrap_or(u64::MAX));
                }
                progress
            });
            let refreshing = reconcile_in_progress.load(Ordering::Acquire) != 0;
            let rebuild_in_flight = refreshing
                || pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
                    != 0;
            let parked = convergence_park
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let generation_recovery = generation_recovery
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let latest = serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let text = text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let text_ready = text
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
                    let identity = if text.is_some() {
                        dashboard_text_freshness_identity(text.as_ref())
                    } else {
                        dashboard_freshness_identity(latest.as_ref())
                    };
                    let hook_hint_count = hints
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .count();
                    let code_graph_serving = dashboard_code_graph_serving(
                        latest.as_ref(),
                        text.as_ref(),
                        graph_activation_enabled,
                    );
                    let ready = dashboard_generation_is_ready(
                        latest.as_ref(),
                        text_ready,
                        graph_activation_enabled,
                        &code_graph_serving,
                    );
                    let stale = hook_hint_count != Some(0);
                    let last_reconcile_micros = match last_reconciled_at_micros
                        .load(Ordering::Acquire)
                    {
                        0 => None,
                        micros => Some(micros),
                    };
                    return tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: canonical_root.display().to_string(),
                        code_graph_serving,
                        last_reconcile_micros,
                        rebuild_in_flight,
                        staleness_state: Some(
                            if parked.is_some() && !ready {
                                "parked"
                            } else if refreshing {
                                if ready {
                                    "refreshing"
                                } else {
                                    "indexing"
                                }
                            } else if stale && ready {
                                "stale"
                            } else if ready {
                                "fresh"
                            } else {
                                "indexing"
                            }
                            .to_owned(),
                        ),
                        hook_hint_count,
                        coverage: if refreshing {
                            "partial_refresh_in_progress"
                        } else if hook_hint_count.is_some() {
                            "complete"
                        } else {
                            "partial_hook_hint_overflow"
                        }
                        .to_owned(),
                        progress,
                        parked,
                        generation_recovery,
                        ..identity
                    };
                }
            };
            let verified = scheduler.verified_against_source();
            let stale = !verified;
            let latest = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let text = text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let text_ready = text
                .as_ref()
                .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
            let hook_hint_count = scheduler.pending_hint_count();
            let code_graph_serving = dashboard_code_graph_serving(
                latest.as_ref(),
                text.as_ref(),
                graph_activation_enabled,
            );
            let ready = dashboard_generation_is_ready(
                latest.as_ref(),
                text_ready,
                graph_activation_enabled,
                &code_graph_serving,
            );
            let staleness_state = if parked.is_some() && !ready {
                "parked"
            } else if refreshing {
                if ready {
                    "refreshing"
                } else {
                    "indexing"
                }
            } else if stale || hook_hint_count != Some(0) {
                if ready {
                    "stale"
                } else {
                    "indexing"
                }
            } else if ready {
                "fresh"
            } else {
                "indexing"
            };
            let identity = if text.is_some() {
                dashboard_text_freshness_identity(text.as_ref())
            } else {
                dashboard_freshness_identity(latest.as_ref())
            };
            tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                worktree_root: canonical_root.display().to_string(),
                code_graph_serving,
                last_reconcile_micros: scheduler.last_reconciled_at_micros(),
                rebuild_in_flight,
                staleness_state: Some(staleness_state.to_owned()),
                hook_hint_count,
                coverage: if refreshing {
                    "partial_refresh_in_progress"
                } else if !verified {
                    "partial_unverified_restore"
                } else if hook_hint_count.is_some() {
                    "complete"
                } else {
                    "partial_hook_hint_overflow"
                }
                .to_owned(),
                progress,
                parked,
                generation_recovery,
                ..identity
            }
        })
        .await
        .ok()
    }

    /// The deterministic contract violation currently parking background
    /// convergence for one mounted worktree, when the worker has observed one.
    /// A status read for doctor/status projections, never an admission
    /// boundary: it takes no scheduler lock and runs no probe.
    pub async fn convergence_park(
        &self,
        project_root: &Path,
    ) -> Option<CodeIndexConvergenceParkedV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&canonical_root)?;
        worktree
            .convergence_park
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Query-admission entry point: serve only an already-decoded generation
    /// whose exact identity authority still resolves. Freshness verification and
    /// any rebuild remain retained background work.
    #[hotpath::measure(label = "daemon.code_index.query.latest_fresh", future = true)]
    pub async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Clone the per-worktree handle under a short map lock, then drop the
        // registry guard before checking the mounted route.
        let (scheduler, serving_generation, text_generation, hints, wake, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        // When the background worker already owns the scheduler, preserve the
        // last complete immutable generation instead of joining its work.
        let authority_root = project_root.clone();
        let latest = crate::ports::park_admission(tokio::task::spawn_blocking(move || {
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
            // A graph-off mount can already own authenticated text serving
            // while its graph-bearing generation deliberately remains
            // unseated. That owner is a real remedy for lexical/exact reads;
            // do not misclassify it as a cold open and inject an overflow that
            // would supersede its bounded projection.
            if text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            {
                return None;
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
                // A cold read carries no source-change evidence. Preserve any
                // snapshot the retained owner is already reconstructing and
                // keep one follow-up authoritative scan pending instead.
                hints
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .overflow();
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
    pub async fn latest_complete_ready(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_with(project_root, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// [`Self::latest_complete_ready`] under an explicit decode admission.
    #[hotpath::measure(label = "daemon.code_index.query.latest_ready", future = true)]
    async fn latest_complete_ready_with(
        &self,
        project_root: &Path,
        _admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (source_freshness, serving_generation, shutting_down) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.shutting_down),
            )
        };
        let freshness_root = project_root.clone();
        let latest = tokio::task::spawn_blocking(move || {
            source_freshness
                .ready_without_stat(&freshness_root, &shutting_down)
                .then(|| {
                    serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                })
                .flatten()
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
    pub async fn latest_complete_fresh_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
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
    pub async fn latest_complete_ready_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_for_scope_with(scope, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// Resolve one exact scope and report whether its mounted scheduler has
    /// verified the live source and that verified source publishes no code
    /// generation at all (no extractable files). This is the typed
    /// generation-empty state a caller awaiting first publication must accept
    /// instead of timing out against a generation that can never exist.
    pub async fn reconciled_without_generation_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let freshness = {
            let mounted = self.mounted.lock().await;
            let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                return false;
            };
            worktree.source_freshness.clone()
        };
        freshness.reconciled_without_generation()
    }

    /// [`Self::latest_complete_ready_for_scope`] restricted to an
    /// already-decoded generation.
    ///
    /// This is the freshness probe for a caller that *already* has a complete
    /// seated generation it can serve. Validate that immutable serving
    /// authority directly rather than consulting the publication decoder cache:
    /// unrelated activation work may own that cache while the seated generation
    /// remains fully decoded and current. When a background reconcile owns the
    /// scheduler mutex, the recorded exact-source witness answers for the
    /// seated generation instead of refusing for the whole pass (see
    /// [`MountedCodeIndexWorktreeV1::serving_source_witness`]).
    pub async fn latest_complete_ready_decoded_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.activate_for_scope(scope);
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
        };
        self.latest_complete_ready_decoded_for_root_scope(&root, scope)
            .await
    }

    fn current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // A synchronous census abstains under map contention; the verified
        // read path awaits the map instead (see
        // [`Self::latest_complete_ready_decoded_for_root_scope`]).
        let parts = {
            let mounted = self.mounted.try_lock().ok()?;
            Self::serving_parts_for_root_scope(&mounted, &project_root, scope)?
        };
        Self::ready_decoded_from_serving_parts(parts, &project_root, scope)
    }

    /// Extract the seat handles the ready probe needs from one mounted route.
    fn serving_parts_for_root_scope(
        mounted: &BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<ReadyProbeServingPartsV1> {
        let worktree = mounted.get(project_root)?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return None;
        }
        Some((
            Arc::clone(&worktree.scheduler),
            worktree.source_freshness.clone(),
            worktree.historical_generation_owner.clone(),
            Arc::clone(&worktree.serving_generation),
            Arc::clone(&worktree.serving_source_witness),
            Arc::clone(&worktree.shutting_down),
            Arc::clone(&worktree.reconcile_in_progress),
        ))
    }

    fn ready_decoded_from_serving_parts(
        (
            scheduler,
            source_freshness,
            historical_generation_owner,
            serving_generation,
            serving_source_witness,
            shutting_down,
            reconcile_in_progress,
        ): ReadyProbeServingPartsV1,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        // The census asks only whether a fully decoded generation is already
        // seated. A graph-off mount deliberately leaves this slot empty while
        // its authenticated text owner is warming. Return that known answer
        // before entering the exact freshness probe: probing an unseated slot
        // cannot produce a decoded owner, and on an initial lightweight mount
        // it would turn `freshness_unknown` into a fabricated overflow wake.
        let serving = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let witness_matches_seat = serving_source_witness
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|witness| {
                witness.generation_id == serving.generation().manifest().generation_id
            });
        if !witness_matches_seat {
            if reconcile_in_progress.load(Ordering::Acquire) != 0 {
                return None;
            }
            match scheduler.try_lock() {
                Ok(guard) => drop(guard),
                Err(std::sync::TryLockError::Poisoned(error)) => drop(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            }
        }
        if !source_freshness.exact_source_is_ready(project_root, &shutting_down)
            || !historical_generation_owner
                .active_publication_covers(serving.generation())
                .ok()?
        {
            *serving_source_witness
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            return None;
        }
        *serving_source_witness
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = source_freshness
            .source_currency_witness_for(&serving.generation().manifest().generation_id);
        // Checkout-identity gate: the ready probe (or its recorded witness)
        // proved the generation current against the live worktree, and the
        // sealed reference label is attribution, not identity (see
        // [`latest_matches_scope_identity`]).
        if !latest_matches_scope_identity(&serving, scope) {
            return None;
        }
        Some(serving)
    }

    /// Report an already-decoded current generation for one exact mounted root
    /// and scope without mounting, decoding, or reconciling.
    pub fn has_current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        self.current_ready_decoded_for_root_scope(project_root, scope)
            .is_some()
    }

    /// Return the exact ready generation without blocking the async executor
    /// on the bounded synchronous freshness probe.
    #[hotpath::measure(label = "daemon.code_index.query.latest_ready_decoded", future = true)]
    pub async fn latest_complete_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Await the map mutex rather than try-locking it: its critical
        // sections are brief map reads, while an abstention under contention
        // here falsely demotes a proven-current answer to the stale serving
        // arm for that read.
        let parts = {
            let mounted = self.mounted.lock().await;
            Self::serving_parts_for_root_scope(&mounted, &project_root, scope)?
        };
        let scope = scope.clone();
        tokio::task::spawn_blocking(move || {
            Self::ready_decoded_from_serving_parts(parts, &project_root, &scope)
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
        let (root, graph_activation_enabled, text_generation) = {
            let mounted = self.mounted.lock().await;
            let (root, worktree) = unique_mounted_for_scope(&mounted, scope).unique()?;
            (
                root.clone(),
                worktree.graph_activation.policy().is_enabled(),
                Arc::clone(&worktree.text_generation),
            )
        };
        // Graph-off mounts authenticate the sealed text source before its
        // bounded projection becomes ready. During that window the text owner
        // is the canonical warming authority; falling through to AwaitDecode
        // would reconstruct the graph-bearing generation solely to report the
        // same typed unavailability, defeating the lightweight cutover.
        if !graph_activation_enabled
            && text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        {
            return None;
        }
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
    pub async fn latest_text_serving_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCodeTextGenerationV1> {
        let text_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .text_generation,
            )
        };
        let latest = text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        (text_matches_scope_identity(&latest, scope) && latest.text_serving_is_ready())
            .then_some(latest)
    }

    /// The queryable exact/lexical owner for one mounted root, if the
    /// lightweight text slot has finished seating. Distinct from
    /// [`Self::latest_generation_id`], which prefers the graph-bearing serving
    /// slot and therefore stays on the previous generation until optional
    /// graph activation finishes.
    pub async fn latest_text_serving_for_root(
        &self,
        project_root: &Path,
    ) -> Option<LatestCodeTextGenerationV1> {
        let project_root = project_root.canonicalize().ok()?;
        let text_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.text_generation)
        };
        text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(LatestCodeTextGenerationV1::text_serving_is_ready)
    }

    /// Resolve graph-independent exact/lexical serving through the same cheap
    /// freshness ladder as complete-generation queries. The immutable text
    /// owner remains servable while a real edit is reconciled in the retained
    /// background worker; a quiet repository only refreshes its stat witness
    /// clock and posts no wake.
    pub async fn latest_text_fresh_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCodeTextGenerationV1> {
        self.latest_text_serving_freshness_for_scope(scope)
            .await
            .map(|(latest, _)| latest)
    }

    /// Resolve the graph-independent text owner together with the freshness
    /// decision made by the same scheduler observation. A ready text artifact
    /// is not inherently stale merely because native graph activation is off.
    pub async fn latest_text_serving_freshness_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<(LatestCodeTextGenerationV1, bool)> {
        let (root, scheduler, text_generation, wake, pending_wake, reconcile_in_progress) = {
            let mounted = self.mounted.lock().await;
            let (root, worktree) = unique_mounted_for_scope(&mounted, scope).unique()?;
            (
                root.clone(),
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.reconcile_in_progress),
            )
        };
        let scope = scope.clone();
        tokio::task::spawn_blocking(move || {
            if gix::open(&root).is_err() {
                return None;
            }
            let latest = text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .filter(|latest| {
                    latest.text_serving_is_ready() && text_matches_scope_identity(latest, &scope)
                })?;
            if pending_wake.has_pending_arrival()
                || reconcile_in_progress.load(Ordering::Acquire) != 0
            {
                // The existing worker owns the freshness remedy. Re-requesting
                // here would enqueue a redundant pass behind it.
                return Some((latest, false));
            }
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::BusyFollowUp,
                    );
                    return Some((latest, false));
                }
            };
            let current = !scheduler.request_fresh_for_query_background();
            if !current {
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            Some((latest, current))
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn latest_complete_serving_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .serving_generation,
            )
        };
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        // Relaxed identity gate: this arm is stale by construction, so a moved
        // reference is exactly the condition it exists to survive.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// [`Self::latest_complete_serving_for_scope`] keyed by the exact mounted
    /// root, mirroring [`Self::latest_complete_ready_decoded_for_root_scope`]:
    /// the stale-while-revalidate arm behind the daemon's exact-scope graph
    /// reads. The seat is O(1), never joins the scheduler mutex, and holds the
    /// last complete generation for the whole rebuild window; a caller that
    /// serves it must type the answer as the last complete generation rather
    /// than current.
    pub async fn latest_complete_serving_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                return None;
            }
            Arc::clone(&worktree.serving_generation)
        };
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        // Relaxed identity gate: this arm is stale by construction, so a moved
        // reference is exactly the condition it exists to survive.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Whether a rebuild remedy is actually in motion for the exact mounted
    /// root: a reconcile pass owns the worktree right now, or a wake is
    /// pending for the background worker. A caller serving the stale seat
    /// quotes this so a wedged route — days-old seat, nothing progressing —
    /// is distinguishable from a routine rebuild window.
    #[hotpath::measure(
        label = "daemon.code_index.query.rebuild_pass_in_flight",
        future = true
    )]
    pub async fn rebuild_pass_in_flight_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return false;
        }
        worktree.reconcile_in_progress.load(Ordering::Acquire) != 0
            || worktree
                .pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .micros
                != 0
    }

    /// Whether an exact mounted route has no admissible generation because its
    /// retained owner is still verifying or rebuilding it.
    pub async fn generation_is_unverified_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let mounted = self.mounted.lock().await;
        let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
            return false;
        };
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
    /// cadence arrival. Second, when a generation's immutable text owners are
    /// ready, the ladder's own suppression decides, exactly as it does on the
    /// grep/context/callers path. Authenticated metadata without those owners
    /// is still warming and always needs the worker's next bounded slice.
    pub async fn request_query_background_reconcile(
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
        let (scheduler, serving_generation, text_generation, hints, wake, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                return false;
            };
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
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
                .is_none()
                && text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none();
            let text_owners_are_warming = text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(LatestCodeTextGenerationV1::text_serving_needs_work);
            // Nothing is servable at all, so the ladder's suppression cannot
            // apply: a reconcile is the only thing that can ever make this scope
            // answerable, and no other caller on this path will ask for it.
            if nothing_servable {
                // This admission observed no source mutation, so it may not
                // supersede an in-flight authoritative snapshot. The retained
                // overflow plus pending wake guarantees a follow-up pass.
                hints
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .overflow();
            } else if !text_owners_are_warming && !scheduler.request_fresh_for_query_background() {
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

    pub async fn semantic_evaluation_snapshot_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::SemanticEvaluationCodeSnapshotV1> {
        self.latest_complete_fresh_for_scope(scope)
            .await
            .map(|latest| latest.semantic_evaluation_snapshot())
    }

    pub async fn acquire_semantic_evaluation_publication_lease(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        expected: &super::SemanticEvaluationCodeSnapshotV1,
    ) -> Option<CodeIndexSemanticEvaluationPublicationLeaseV1> {
        let gate = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .semantic_evaluation_publication_gate,
            )
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
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn scheduler_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<Mutex<CodeIndexWorktreeSchedulerV1>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.scheduler))
    }

    /// Test support for proving the explicit same-store build/publication
    /// invariant independently of the scheduler metadata mutex.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn build_publication_lock_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.build_publication_lock))
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

    pub async fn retire_project_roots(
        &self,
        project_roots: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        self.retire_project_roots_with_deadline(
            project_roots,
            super::super::DAEMON_TASK_ABORT_DEADLINE,
        )
        .await
    }

    pub async fn retire_project_roots_with_deadline(
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
    pub async fn retiring_owner_count(&self) -> usize {
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
impl tracedecay_usecases::diagnostics_publication::CodeIndexPublicationIdentityPortV1
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
    ) -> tracedecay_usecases::diagnostics_publication::CodeIndexPublicationIdentityFuture<'_> {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root.canonicalize().ok()?;
            let current = registry.latest_complete_fresh(&root).await?;
            let snapshot = current.generation.snapshot();
            Some(
                tracedecay_usecases::diagnostics_publication::CodeIndexPublicationIdentityV1::new(
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
    let relative = canonical_relative_document_path(project_root, &path)
        .ok_or_else(|| LspRuntimeFailure::new("feedback-document-outside-root"))?;
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

/// Strip the canonical `project_root` from a client-supplied document path,
/// comparing canonical to canonical.
///
/// The caller canonicalizes the mounted root; the client addresses a document
/// by whatever path it opened. Those two spellings differ whenever any prefix
/// of the root is a symlink — on macOS every `/var/folders/...` root the
/// daemon holds as `/private/var/folders/...` — and a raw prefix strip
/// refused every document under such a root as outside it.
///
/// The document need not exist yet (an unsaved buffer), so the deepest
/// existing ancestor is canonicalized and the unresolved tail re-appended.
/// The fence tightens rather than loosens: resolving before the strip refuses
/// a path that reaches outside the root through a symlink *inside* it, which
/// a raw prefix strip accepted as an ordinary logical path. The caller still
/// refuses any relative path that is empty or carries a non-normal component,
/// so an unresolved tail can neither escape upward nor name a root-external
/// path.
fn canonical_relative_document_path(project_root: &Path, path: &Path) -> Option<PathBuf> {
    let mut unresolved: Vec<&std::ffi::OsStr> = Vec::new();
    let mut candidate = path;
    loop {
        if let Ok(canonical) = candidate.canonicalize() {
            let mut relative = canonical.strip_prefix(project_root).ok()?.to_path_buf();
            for component in unresolved.iter().rev() {
                relative.push(component);
            }
            return Some(relative);
        }
        unresolved.push(candidate.file_name()?);
        candidate = candidate.parent()?;
    }
}

#[cfg(all(test, unix))]
mod feedback_document_path_tests {
    use super::feedback_document_logical_path;

    /// A symlinked root reproduces on Linux exactly what every macOS
    /// `/var/folders/...` temporary root does in production: the daemon holds
    /// the canonical root while the client addresses documents through the
    /// alias it opened.
    #[test]
    fn a_symlinked_root_alias_resolves_to_the_same_logical_path() {
        let base = tempfile::TempDir::new().expect("temporary base");
        let real = base.path().join("real");
        std::fs::create_dir_all(real.join("src")).expect("real tree");
        std::fs::write(real.join("src/lib.rs"), b"pub fn alpha() {}\n").expect("document");
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("root alias");

        let canonical_root = real.canonicalize().expect("canonical root");
        let canonical_uri = url::Url::from_file_path(canonical_root.join("src/lib.rs"))
            .expect("canonical document uri");
        let alias_uri =
            url::Url::from_file_path(alias.join("src/lib.rs")).expect("alias document uri");

        assert_eq!(
            feedback_document_logical_path(&canonical_root, canonical_uri.as_str())
                .expect("canonical spelling resolves"),
            "src/lib.rs"
        );
        assert_eq!(
            feedback_document_logical_path(&canonical_root, alias_uri.as_str())
                .expect("the client's alias spelling resolves against the canonical root"),
            "src/lib.rs"
        );
    }

    /// An unsaved buffer has no file to canonicalize; the deepest existing
    /// ancestor still binds it to the root.
    #[test]
    fn an_unsaved_document_under_a_root_alias_still_resolves() {
        let base = tempfile::TempDir::new().expect("temporary base");
        let real = base.path().join("real");
        std::fs::create_dir_all(real.join("src")).expect("real tree");
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("root alias");
        let canonical_root = real.canonicalize().expect("canonical root");

        let uri = url::Url::from_file_path(alias.join("src/unsaved.rs")).expect("document uri");
        assert_eq!(
            feedback_document_logical_path(&canonical_root, uri.as_str())
                .expect("an unsaved buffer resolves through its existing ancestor"),
            "src/unsaved.rs"
        );
    }

    /// The escape fence stays closed: an alias that leaves the root, and a
    /// traversal that climbs out of it, are both refused.
    #[test]
    fn paths_outside_the_root_stay_refused_through_an_alias() {
        let base = tempfile::TempDir::new().expect("temporary base");
        let real = base.path().join("real");
        std::fs::create_dir_all(&real).expect("real tree");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside tree");
        std::fs::write(outside.join("secret.rs"), b"pub fn secret() {}\n").expect("outside file");
        std::os::unix::fs::symlink(&outside, real.join("escape")).expect("escaping alias");
        let canonical_root = real.canonicalize().expect("canonical root");

        let escaping = url::Url::from_file_path(canonical_root.join("escape/secret.rs"))
            .expect("escaping document uri");
        assert!(
            feedback_document_logical_path(&canonical_root, escaping.as_str()).is_err(),
            "a symlink out of the root is not a document of this project"
        );

        let traversal =
            url::Url::from_file_path(outside.join("secret.rs")).expect("outside document uri");
        assert!(
            feedback_document_logical_path(&canonical_root, traversal.as_str()).is_err(),
            "a sibling directory is not a document of this project"
        );
    }
}

#[cfg(test)]
mod text_slice_fairness_tests {
    use super::{CodeIndexCadenceTriggerV1, CodeIndexSchedulerRegistryV1, PendingWakeV1};

    #[test]
    fn pending_reconcile_is_serviced_between_bounded_text_slices() {
        let pending = PendingWakeV1::default();
        let wake = tokio::sync::Notify::new();
        assert!(
            CodeIndexSchedulerRegistryV1::incomplete_text_slice_may_continue(&pending),
            "a text-only self-wake may advance the next bounded slice"
        );

        CodeIndexSchedulerRegistryV1::note_wake(
            &pending,
            &wake,
            CodeIndexCadenceTriggerV1::HookHint,
        );
        assert!(
            !CodeIndexSchedulerRegistryV1::incomplete_text_slice_may_continue(&pending),
            "a pending source reconcile must win before another text slice"
        );

        let _ = CodeIndexSchedulerRegistryV1::take_pending_arrival(
            &pending,
            CodeIndexCadenceTriggerV1::Mount,
        );
        assert!(
            CodeIndexSchedulerRegistryV1::incomplete_text_slice_may_continue(&pending),
            "text continuation resumes only after reconcile claims the pending arrival"
        );
    }
}
