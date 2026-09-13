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
    time::Duration,
};

#[cfg(test)]
use std::sync::Condvar;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, RepositoryId, WorktreeId, host_cpu_target,
};
use tracedecay_lsp::LspRuntimeFailure;

use tracedecay_application::semantic_runtime::SavedGenerationScheduleOutcomeV1;

#[cfg(test)]
use super::CodeIndexBytePoolStatsV1;
use super::graph_activation::CodeGraphActivationAuthorityV1;
use super::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, CodeIndexNoopEvidenceV1,
    CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexControlV1, LatestCodeTextGenerationV1,
    LatestCompleteCodeIndexV1, PendingHintsV1, SharedCodeIndexBytePoolV1,
    newly_eligible_percentile, now_micros,
};

#[cfg(test)]
mod cold_read_wake_tests;
#[cfg(all(test, unix))]
mod convergence_park_tests;
mod ignored_dependencies;
mod lsp_projection;
mod mount;
mod query_authority;
#[cfg(test)]
mod reconcile_failure_isolation_tests;
mod scope_identity;
#[cfg(test)]
mod serving_readiness_tests;
mod serving_reads;

pub use scope_identity::latest_matches_scope_identity;

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
/// gate is now the text owner, not the tree: a publication prepares on its own
/// pass once its lightweight text owner has reopened, and an unchanged pass
/// prepares as soon as a retained owner exists to recover a head from. Both
/// project text on their own task, concurrently with the graph decode and
/// activation, so neither seat waits for the lexical artifact. Every skip
/// names itself.
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
    /// A publication whose replacement text owner did not reopen.
    PublishedTextOwnerUnavailable,
    /// An unchanged pass with no retained owner to recover a head from.
    RetainedGenerationUnavailable,
}

/// How a publication's replacement text owner finished the bounded
/// projection this pass drove for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishedTextProjectionOutcomeV1 {
    /// Exact and lexical serving are ready for the publication.
    Finished,
    /// The projection stopped short of ready: the advance bound, a typed
    /// contract violation (parked on the owner), a failure, or an abnormal
    /// task exit. The owner carries the typed state; the pass seats nothing.
    Unfinished,
    /// Shutdown retired the text control mid-slice.
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEvaluationGenerationRefusalV1 {
    ProjectRootCanonicalizationFailed,
    ProjectRootNotMounted,
    ScopeIdentityMismatch,
    SourceUnverified,
    SourceChanged,
    SchedulerUnavailable,
    GitAuthorityUnavailable,
    GenerationUnavailable,
    GenerationScopeMismatch,
    WorkerJoinFailed,
}

impl SemanticEvaluationGenerationRefusalV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectRootCanonicalizationFailed => "project_root_canonicalization_failed",
            Self::ProjectRootNotMounted => "project_root_not_mounted",
            Self::ScopeIdentityMismatch => "scope_identity_mismatch",
            Self::SourceUnverified => "source_unverified",
            Self::SourceChanged => "source_changed",
            Self::SchedulerUnavailable => "scheduler_unavailable",
            Self::GitAuthorityUnavailable => "git_authority_unavailable",
            Self::GenerationUnavailable => "generation_unavailable",
            Self::GenerationScopeMismatch => "generation_scope_mismatch",
            Self::WorkerJoinFailed => "worker_join_failed",
        }
    }
}

fn record_semantic_candidate_refusal(
    project_root: &Path,
    reason: SemanticEvaluationGenerationRefusalV1,
) {
    tracing::info!(
        event = "code_index_semantic_candidate_unavailable",
        project = %project_root.display(),
        reason = reason.as_str(),
        "semantic evaluation generation is unavailable"
    );
}

impl GraphSeatGateV1 {
    /// `text_owner_present` names the owner of the generation this pass would
    /// seat: a publication's reopened replacement owner, or the restored
    /// retained owner. Both carry the sealed manifest the seat reads; neither
    /// needs to have finished its lexical projection.
    #[hotpath::skip]
    pub const fn decide(
        activation_enabled: bool,
        activation_deferred: bool,
        reconcile_is_terminal: bool,
        published_pass: bool,
        text_owner_present: bool,
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
        // The seat reads the owner's sealed manifest, not its lexical
        // artifact: a publication's own product is stale by construction, and
        // a retained owner's verified-head recovery compares the manifest
        // against the durable head. Every check either performs stays
        // fail-closed; only the wait for a finished projection is gone.
        //
        // The retained pass used to demand a *ready* text owner. A restart
        // that resumed an unfinished ngram index therefore held the recovered
        // graph unseated for the whole build -- minutes on a large store --
        // while `code_symbol_search` refused with
        // `lsp-code-index-generation-unavailable` and code-generation
        // retention degraded on an incomplete vector census (issue #1244).
        if text_owner_present {
            return Self::Prepare;
        }
        if published_pass {
            return Self::PublishedTextOwnerUnavailable;
        }
        Self::RetainedGenerationUnavailable
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
            Self::RetainedGenerationUnavailable => Some("retained_generation_unavailable"),
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
    /// The durable pointer already names a successor, and the slot holds
    /// nothing the store still calls active: a stale seat beats an empty or
    /// equally superseded route, and the next publication supersedes it.
    SeatedStale,
    /// The durable pointer already names a successor and the incumbent *is*
    /// that active publication, so the slot keeps what it has.
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
    pub const fn decide(
        publication_matches: bool,
        incumbent_is_active: bool,
        replace: bool,
    ) -> Self {
        if !publication_matches {
            if incumbent_is_active {
                // The active durable publication already serves; a superseded
                // generation must not move the slot backwards.
                return Self::Superseded;
            }
            // Nothing active holds the slot — it is empty, or its incumbent
            // was superseded too. Either way this generation is no worse than
            // what is there, and refusing left the route wedged on a
            // generation the store no longer publishes.
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

pub(crate) fn semantic_handoff_has_exact_witness(
    publication_matches: bool,
    witness: Option<&super::ServingSourceWitnessV1>,
    generation: &CodeGenerationId,
) -> bool {
    publication_matches && witness.is_some_and(|witness| &witness.generation_id == generation)
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
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// Armed gates, keyed by the exact worktree they fence. The slot is process
/// wide while the tests that arm it run concurrently in one binary, so a
/// single slot made two unrelated restart fixtures collide by scheduling
/// accident; the key is the isolation the fixtures already have.
#[cfg(any(test, feature = "test-helpers"))]
fn retained_graph_recovery_successor_gate()
-> &'static Mutex<BTreeMap<PathBuf, RetainedGraphRecoverySuccessorGateV1>> {
    static GATE: std::sync::OnceLock<
        Mutex<BTreeMap<PathBuf, RetainedGraphRecoverySuccessorGateV1>>,
    > = std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(any(test, feature = "test-helpers"))]
struct RetainedTextProjectionGateV1 {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// Armed gates, keyed by the exact worktree they fence, on the same
/// per-worktree isolation as the recovery gate above.
#[cfg(any(test, feature = "test-helpers"))]
fn retained_text_projection_gate() -> &'static Mutex<BTreeMap<PathBuf, RetainedTextProjectionGateV1>>
{
    static GATE: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, RetainedTextProjectionGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
struct PublishedTextProjectionGateV1 {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
fn published_text_projection_gate()
-> &'static Mutex<BTreeMap<PathBuf, PublishedTextProjectionGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, PublishedTextProjectionGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeMap::new()))
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
#[cfg(test)]
mod test_gates;
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

/// Register for `notify` before reading `flag`. `Notify::notified()` is inert
/// until it is polled or `enable()`d; a notification between the flag load and
/// the first poll is otherwise dropped forever.
#[cfg(test)]
async fn wait_notified_if_unset(flag: &AtomicBool, notify: &tokio::sync::Notify) {
    let notified = notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if !flag.load(Ordering::Acquire) {
        notified.await;
    }
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

/// Mounted scope identity without consulting either serving-generation seat.
#[derive(Clone)]
pub struct CodeIndexMountedScopeV1 {
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub shutting_down: Arc<AtomicBool>,
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
    pub semantic_lifecycle_owner: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    pub query_activation_revision: Option<ConfigurationRevisionId>,
    pub query_activation_epoch: Option<i64>,
    pub query_activation_transition_digest: Option<ManifestDigest>,
    pub query_activation_attempt: u64,
    pub query_activation_redundancy:
        Option<tracedecay_application::semantic_runtime::PreparedSemanticRedundancyAuthorityV1>,
    pub semantic_vector_graph_provider:
        Option<Arc<dyn tracedecay_application::semantic_runtime::SemanticVectorGraphProviderV1>>,
    pub scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    /// Explicit same-store build/publication invariant shared by source
    /// reconcile, ignored-dependency publication, and historical generation
    /// minting. Async owners acquire this before entering blocking scheduler
    /// work so competing builds wait without occupying a blocking-pool thread.
    pub(super) build_publication_lock: Arc<tokio::sync::Mutex<()>>,
    pub historical_generation_owner: super::HistoricalCodeIndexGenerationOwnerV1,
    pub serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    /// Complete-generation callers need the decoded serving owner; restored
    /// text and persistent graph reads do not. Only their explicit demand
    /// admits this optional decode after verified-head recovery.
    complete_generation_requested: Arc<AtomicBool>,
    complete_generation_requested_changed: tokio::sync::watch::Sender<bool>,
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
        RwLock<Option<tracedecay_contracts::code_index_freshness::CodeIndexGenerationRecoveryV1>>,
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
    /// A wake signal only: readers resolve identity and availability from the serving slot.
    serving_generation_changed: tokio::sync::watch::Sender<()>,
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
    scope: &tracedecay_contracts::ResolvedScope,
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
) -> tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
    let mut identity =
        tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1::default();
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
) -> tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
    let mut identity =
        tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1::default();
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

    fn still_owns(&self) -> bool {
        let state = self
            .pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.owner == self.owner && state.micros == self.claimed_micros
    }
}

impl Drop for PendingWakeClaimV1 {
    fn drop(&mut self) {
        if !self.settled {
            // The test drop gate parks on a Condvar. Do that before taking
            // `pending_wake.state` so a runtime worker never blocks under
            // the production lock.
            #[cfg(test)]
            self.pending_wake.pause_claim_drop_for_test();
            let mut state = self
                .pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    super::SourceFreshnessFenceV1,
    super::HistoricalCodeIndexGenerationOwnerV1,
    Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    Arc<RwLock<Option<super::ServingSourceWitnessV1>>>,
    Arc<AtomicBool>,
    Arc<tokio::sync::Notify>,
    Arc<PendingWakeV1>,
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
    /// Seating counter. Publication is broadcast when reconcile seals, which
    /// is before the sealed generation takes the serving slot, so a waiter
    /// that needs the seated slot has no publication event to wake on. This
    /// advances once per install, after the slot is written, so those waiters
    /// block on a transition instead of polling the slot.
    serving_seats: Arc<tokio::sync::watch::Sender<u64>>,
    cadence_telemetry: Arc<Mutex<CodeIndexCadenceTelemetryV1>>,
    pub(super) relation_symbol_hydrations: Arc<AtomicU64>,
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
        scope: &tracedecay_contracts::ResolvedScope,
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
        scope: &tracedecay_contracts::ResolvedScope,
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
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<super::CodeIndexAutomaticAdmissionV1> {
        self.activation_for_scope(scope)
            .map(|activation| activation.automatic_admission())
    }

    fn activate_for_scope(&self, scope: &tracedecay_contracts::ResolvedScope) -> bool {
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
        let mut gates = retained_graph_recovery_successor_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates
                .insert(
                    project_root.clone(),
                    RetainedGraphRecoverySuccessorGateV1 { entered, release },
                )
                .is_none(),
            "one retained graph recovery successor gate per worktree: {}",
            project_root.display()
        );
        (entered_observed, released)
    }

    /// Hold a restart's retained text projection at its first advance, so a
    /// fixture can observe what the graph seat does while exact and lexical
    /// serving are still warming. A real store holds this open by itself: the
    /// projection's finalization index build takes minutes on a large corpus.
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn pause_next_retained_text_projection(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (released, release) = tokio::sync::oneshot::channel();
        let mut gates = retained_text_projection_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates
                .insert(
                    project_root.clone(),
                    RetainedTextProjectionGateV1 { entered, release },
                )
                .is_none(),
            "one retained text projection gate per worktree: {}",
            project_root.display()
        );
        (entered_observed, released)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    async fn wait_for_retained_text_projection_gate(project_root: &Path) {
        let gate = retained_text_projection_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    async fn wait_for_retained_graph_recovery_successor_gate(project_root: &Path) {
        let gate = retained_graph_recovery_successor_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
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

    /// Clear the pending-wake slot so a test starts from a known due window.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn clear_pending_wake_for_scope(&self, scope: &tracedecay_contracts::ResolvedScope) {
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
            serving_generation_changed,
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
                worktree.serving_generation_changed.clone(),
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
            serving_generation_changed.send_replace(());
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

    /// Post one arrival through the existing coalesced per-worktree owner.
    /// A queued arrival already supplies the same background remedy.
    fn note_wake_if_idle(
        pending_wake: &PendingWakeV1,
        wake: &tokio::sync::Notify,
        trigger: CodeIndexCadenceTriggerV1,
    ) -> bool {
        let wake_micros = u64::try_from(now_micros().0).unwrap_or(u64::MAX);
        let mut state = pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.micros != 0 {
            return false;
        }
        state.owner = state.next_owner();
        state.micros = wake_micros;
        state.trigger = Self::pack_trigger(trigger);
        drop(state);
        wake.notify_one();
        true
    }

    /// Queue worker-owned continuation work through the same pending-arrival
    /// authority as external wakes. This keeps readiness truthful while the
    /// continuation waits for shared admission; a bare `Notify` permit is not
    /// observable by freshness readers.
    fn note_worker_continuation(pending_wake: &PendingWakeV1, wake: &tokio::sync::Notify) {
        if !Self::note_wake_if_idle(pending_wake, wake, CodeIndexCadenceTriggerV1::BusyFollowUp) {
            // This pass may have consumed the permit for an arrival it has not
            // claimed yet. Keep that observable arrival and replenish its
            // coalesced permit so the continuation cannot sleep behind it.
            wake.notify_one();
        }
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

    /// Take the scheduler for one optional-graph step with the worker's pass
    /// visible for as long as the acquisition blocks.
    ///
    /// The graph section runs after the source pass releases
    /// `reconcile_in_progress`, because an O(store) sealed decode must not
    /// read as a rebuild in flight. Blocking on the scheduler mutex is the
    /// opposite case: the worker is still inside its pass and cannot move
    /// until whoever holds that mutex lets go, so a caller that holds it and
    /// waits for the flag to rise waits on itself. Count the wait and the
    /// locked step; the decode between two of these stays uncounted.
    ///
    /// The returned pass guard rides in the tuple so it lives exactly as long
    /// as the statement that took the lock.
    fn lock_scheduler_for_graph_step<'a>(
        scheduler: &'a Mutex<CodeIndexWorktreeSchedulerV1>,
        shutting_down: &AtomicBool,
        passes: &Arc<AtomicUsize>,
    ) -> Result<
        (
            super::ReconcilePassGuard,
            std::sync::MutexGuard<'a, CodeIndexWorktreeSchedulerV1>,
        ),
        CodeIndexSchedulerErrorV1,
    > {
        let pass = super::ReconcilePassGuard::enter(passes);
        Self::lock_scheduler_unless_shutting_down(scheduler, shutting_down)
            .map(|scheduler| (pass, scheduler))
    }

    /// Drive one pass's text owner — a publication's replacement owner or the
    /// restored retained owner — through its bounded projection, one blocking
    /// advance at a time, until exact and lexical serving are ready or the
    /// projection stops typed.
    ///
    /// Runs on its own task, concurrently with the same pass's graph prepare
    /// and native activation: text and graph both consume the sealed
    /// generation and neither depends on the other until the seat, which
    /// joins this task. The advance itself is single-flight on the owner's
    /// projection slot, so scheduler wakes that race it wait, never double
    /// drive.
    ///
    /// `installed` is the worktree's text slot for an owner that is already
    /// installed there. A cancelled advance latches that handle, so the slot
    /// withdraws it and the next pass restores a fresh one from the durable
    /// pointer. A publication's owner is not installed until it reopens, so
    /// that caller passes `None`.
    async fn join_retained_text_projection_on_worker_exit(
        projection: &mut Option<tokio::task::JoinHandle<PublishedTextProjectionOutcomeV1>>,
    ) {
        if let Some(projection) = projection.take() {
            let _ = projection.await;
        }
    }

    async fn drive_text_projection(
        text: LatestCodeTextGenerationV1,
        shutting_down: Arc<AtomicBool>,
        convergence_park: Arc<RwLock<Option<CodeIndexConvergenceParkedV1>>>,
        installed: Option<Arc<RwLock<Option<LatestCodeTextGenerationV1>>>>,
        #[cfg(test)] project_root: PathBuf,
    ) -> PublishedTextProjectionOutcomeV1 {
        #[cfg(test)]
        if installed.is_none() {
            Self::wait_for_published_text_projection_gate(&project_root).await;
        }
        let mut advances = 0_usize;
        while text.text_serving_needs_work() {
            if shutting_down.load(Ordering::Acquire) {
                return PublishedTextProjectionOutcomeV1::Shutdown;
            }
            advances += 1;
            if advances > TEXT_PROJECTION_MAXIMUM_ACTIVATION_ADVANCES_V1 {
                tracing::warn!(
                    event = "code_index_text_projection_advance_bound_reached",
                    advances = TEXT_PROJECTION_MAXIMUM_ACTIVATION_ADVANCES_V1,
                    "published text projection did not complete within its bounded advance \
                     budget; graph seating waits for a later pass"
                );
                break;
            }
            let advancing = text.clone();
            match hotpath::future!(
                tokio::task::spawn_blocking(
                    move || advancing.advance_text_serving(TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1)
                ),
                label = "daemon.code_index.text_projection"
            )
            .await
            {
                Ok(Ok(true)) => {
                    clear_convergence_park(&convergence_park);
                    break;
                }
                Ok(Ok(false)) => {
                    clear_convergence_park(&convergence_park);
                }
                Ok(Err(error)) => {
                    if matches!(
                        &error,
                        tracedecay_query::retrieval::RetrievalPortError::Cancelled
                    ) && let Some(installed) = installed.as_ref()
                    {
                        let mut current = installed
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if current
                            .as_ref()
                            .is_some_and(|current| current.same_text_owner(&text))
                        {
                            *current = None;
                        }
                    }
                    if error.is_deterministic_contract() {
                        park_convergence(
                            &convergence_park,
                            error.to_string(),
                            CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
                            true,
                        );
                        tracing::warn!(
                            event = "code_index_convergence_parked",
                            path = "background_worker",
                            error = %error,
                            "text projection parked on a deterministic contract violation \
                             before graph seating; status reports it typed and every wake \
                             re-checks"
                        );
                    } else if matches!(
                        &error,
                        tracedecay_query::retrieval::RetrievalPortError::Cancelled
                    ) && shutting_down.load(Ordering::Acquire)
                    {
                        // Shutdown retired the text control mid-slice. The
                        // slice stops typed; nothing failed.
                        tracing::info!(
                            event = "code_index_text_projection_interrupted",
                            origin = "shutdown",
                            "published text projection stopped before graph seating"
                        );
                        return PublishedTextProjectionOutcomeV1::Shutdown;
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
                        &convergence_park,
                        format!("code text projection task failed abnormally: {error}"),
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
        // Readiness, not "nothing left to advance": a latched failed owner
        // also has no work left, and it must not seat.
        if text.text_serving_is_ready() {
            PublishedTextProjectionOutcomeV1::Finished
        } else {
            PublishedTextProjectionOutcomeV1::Unfinished
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

    pub fn subscribe_generation_publications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1> {
        self.generation_publications.subscribe()
    }

    /// Subscribe before probing the serving slot. Sealed publication precedes
    /// serving, including on a restored mount where no new publication event is
    /// emitted. Successful source revalidation also signals this watch when an
    /// unchanged complete generation stays seated.
    ///
    /// This is a watch only. Callers that need complete-generation demand must
    /// also call [`Self::request_complete_generation`].
    pub async fn subscribe_serving_generation_changes(
        &self,
        project_root: &Path,
    ) -> Option<tokio::sync::watch::Receiver<()>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root)?;
        Some(worktree.serving_generation_changed.subscribe())
    }

    /// Stamp complete-generation demand for a mounted worktree. The first flip
    /// notes a [`CodeIndexCadenceTriggerV1::QueryAdmission`] wake so the worker
    /// yields text-only work and seats a complete generation.
    pub async fn request_complete_generation(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        if !worktree
            .complete_generation_requested
            .swap(true, Ordering::AcqRel)
        {
            worktree
                .complete_generation_requested_changed
                .send_replace(true);
            Self::note_wake(
                &worktree.pending_wake,
                &worktree.wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
        }
        true
    }

    /// Observe serving-slot seating. Each advance means the serving slot was
    /// written; the receiver reads the slot to learn what it now holds.
    pub fn subscribe_serving_seats(&self) -> tokio::sync::watch::Receiver<u64> {
        self.serving_seats.subscribe()
    }

    /// Record that the serving slot was written. Call this only after the slot
    /// holds the new generation, so a woken waiter observes the seated value.
    fn record_serving_seat(seats: &tokio::sync::watch::Sender<u64>) {
        seats.send_modify(|seats| *seats = seats.wrapping_add(1));
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

    /// Run the bounded Git/stat/content freshness ladder for an ordinary read
    /// without manufacturing an overflow. Only a proven source change posts a
    /// query admission wake; a source witness that still matches (stat
    /// signature, then sealed file digests) refreshes the scheduler's cadence
    /// watermark and returns without extraction.
    pub async fn probe_freshness(&self, project_root: &Path) -> bool {
        self.diagnostics_change_generation(project_root)
            .await
            .is_some()
    }

    #[hotpath::measure(label = "daemon.code_index.registry.install_semantic", future = true)]
    pub async fn install_semantic_vector_graph_provider(
        &self,
        project_root: &Path,
        provider: Arc<dyn tracedecay_application::semantic_runtime::SemanticVectorGraphProviderV1>,
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
    ) -> Option<Arc<dyn tracedecay_application::semantic_runtime::SemanticVectorGraphProviderV1>>
    {
        let project_root = project_root.canonicalize().ok()?;
        self.mounted
            .lock()
            .await
            .get(&project_root)?
            .semantic_vector_graph_provider
            .clone()
    }

    pub async fn reschedule_semantic_generation(
        &self,
        project_root: &Path,
    ) -> SavedGenerationScheduleOutcomeV1 {
        let Ok(project_root) = project_root.canonicalize() else {
            return Self::record_reschedule_decline(
                project_root,
                SavedGenerationScheduleOutcomeV1::SchedulerUnavailable,
            );
        };
        let (scheduler, shutting_down, generation) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return Self::record_reschedule_decline(
                    &project_root,
                    SavedGenerationScheduleOutcomeV1::SchedulerUnavailable,
                );
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
            return Self::record_reschedule_decline(
                &project_root,
                SavedGenerationScheduleOutcomeV1::NoServingGeneration,
            );
        };
        let outcome = tokio::task::spawn_blocking(move || {
            let scheduler =
                Self::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down).ok()?;
            Some(scheduler.schedule_semantic_generation(generation))
        })
        .await
        .ok()
        .flatten();
        match outcome {
            // `schedule_semantic_generation` already recorded this outcome.
            Some(outcome) => outcome,
            None => Self::record_reschedule_decline(
                &project_root,
                SavedGenerationScheduleOutcomeV1::SchedulerUnavailable,
            ),
        }
    }

    /// Name a re-offer that never reached the scheduler. Without this the
    /// activation reconciler's retry produced no evidence at all, so a runtime
    /// that stopped scheduling looked identical to one with nothing to do.
    fn record_reschedule_decline(
        project_root: &Path,
        outcome: SavedGenerationScheduleOutcomeV1,
    ) -> SavedGenerationScheduleOutcomeV1 {
        tracing::warn!(
            event = "code_index_semantic_schedule_declined",
            outcome = outcome.as_str(),
            project = %project_root.display(),
            "code-index could not re-offer a serving generation to semantic projection"
        );
        outcome
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

    #[hotpath::measure(label = "daemon.code_index.shutdown", future = true)]
    pub async fn shutdown(&self) {
        self.cancel();
        let cold_mount_completions = self.cold_mount_reservation_completions();
        let mut retiring = self.retiring.lock().await;
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for worktree in mounted.values() {
            worktree.shutting_down.store(true, Ordering::Release);
            worktree.serving_generation_changed.send_replace(());
            worktree.wake.notify_one();
        }
        // Mount admission refuses roots already retiring. Keep each owner in
        // that same registry while awaiting its worker: cancellation of this
        // waiter must not detach an in-flight blocking reconcile or lose the
        // handle a later shutdown needs to join.
        retiring.extend(mounted);
        let mut owner_releases = Vec::new();
        while let Some(root) = retiring.keys().next().cloned() {
            if let Some(worktree) = retiring.get_mut(&root) {
                let started = std::time::Instant::now();
                tracedecay_runtime_core::logging::log_daemon_event(
                    "daemon_shutdown",
                    &[
                        ("outcome", "code_index_worker_join_start".to_string()),
                        ("root", root.display().to_string()),
                    ],
                );
                let _ = hotpath::future!(
                    &mut worktree.task,
                    label = "daemon.code_index.shutdown.worker_join"
                )
                .await;
                tracedecay_runtime_core::logging::log_daemon_event(
                    "daemon_shutdown",
                    &[
                        ("outcome", "code_index_worker_joined".to_string()),
                        ("root", root.display().to_string()),
                        ("elapsed_ms", started.elapsed().as_millis().to_string()),
                    ],
                );
            }
            if let Some(worktree) = retiring.remove(&root) {
                // A joined owner still holds whatever its last pass built: a
                // cancelled seal retains its unpublished candidate, and a
                // generation-sized candidate is seconds of deallocation.
                // Freeing it inline would block this runtime worker for that
                // long and hide it inside the owner's join budget, so the
                // release runs on the blocking pool. Shutdown still joins it
                // below: ownership must be gone when this returns.
                owner_releases.push(tokio::task::spawn_blocking(move || {
                    hotpath::measure_block!(
                        "daemon.code_index.shutdown.owner_release",
                        drop(worktree)
                    );
                }));
            }
        }
        // Cold mounts take `retiring` at their final admission fence.
        drop(retiring);
        for mut completion in cold_mount_completions {
            let _ = completion.changed().await;
        }
        let release_started = std::time::Instant::now();
        let release_count = owner_releases.len();
        for release in owner_releases {
            let _ = hotpath::future!(
                release,
                label = "daemon.code_index.shutdown.owner_release_join"
            )
            .await;
        }
        tracedecay_runtime_core::logging::log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "code_index_owner_releases_joined".to_string()),
                ("releases", release_count.to_string()),
                (
                    "elapsed_ms",
                    release_started.elapsed().as_millis().to_string(),
                ),
            ],
        );
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
            worktree.serving_generation_changed.send_replace(());
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

    pub fn cancel(&self) {
        self.background_reconcile_admission.close();
        self.cancel_cold_mount_reservations();
        if let Ok(mounted) = self.mounted.try_lock() {
            for worktree in mounted.values() {
                worktree.shutting_down.store(true, Ordering::Release);
                worktree.serving_generation_changed.send_replace(());
                worktree.wake.notify_one();
            }
        }
    }
}

#[derive(Clone)]
pub struct ScopedFeedbackDocumentIdentityV1 {
    registry: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: tracedecay_contracts::ResolvedScope,
}

impl CodeIndexSchedulerRegistryV1 {
    pub async fn latest_feedback_generation_for_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCodeTextGenerationV1> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted_root = {
            let mounted = self.mounted.lock().await;
            let (mounted_root, _) = unique_mounted_for_scope(&mounted, scope).unique()?;
            mounted_root.clone()
        };
        if mounted_root != project_root {
            return None;
        }
        if let Some(generation) = self
            .latest_complete_ready_decoded_for_root_scope(&project_root, scope)
            .await
        {
            return Some(generation.text_generation_handle());
        }
        if let Some(generation) = self.latest_complete_ready_for_scope(scope).await {
            return Some(generation.text_generation_handle());
        }
        self.latest_text_serving_freshness_for_scope(scope)
            .await
            .and_then(|(generation, current)| current.then_some(generation))
    }
}

impl ScopedFeedbackDocumentIdentityV1 {
    pub fn new(
        registry: CodeIndexSchedulerRegistryV1,
        project_root: &Path,
        scope: tracedecay_contracts::ResolvedScope,
    ) -> Option<Self> {
        Some(Self {
            registry,
            project_root: project_root.canonicalize().ok()?,
            scope,
        })
    }
}

impl tracedecay_application::feedback::cycle_production::ProductionFeedbackDocumentIdentityPort
    for ScopedFeedbackDocumentIdentityV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
        document_uri: Option<String>,
    ) -> tracedecay_application::feedback::cycle_production::ProductionFeedbackDocumentIdentityFuture
    {
        let owner = self.clone();
        Box::pin(async move {
            let requested_root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("feedback-code-index-root-unavailable"))?;
            if requested_root != owner.project_root {
                return Err(LspRuntimeFailure::new("feedback-code-index-root-mismatch"));
            }
            let selection = owner
                .registry
                .latest_feedback_generation_for_scope(&owner.project_root, &owner.scope)
                .await
                .ok_or_else(|| {
                    LspRuntimeFailure::new("feedback-code-index-generation-unavailable")
                })?;
            feedback_document_identity_from_generation(
                selection,
                &owner.project_root,
                document_uri.as_deref(),
            )
        })
    }
}

pub fn feedback_document_identity_from_generation(
    generation: LatestCodeTextGenerationV1,
    project_root: &Path,
    document_uri: Option<&str>,
) -> Result<
    tracedecay_application::feedback::cycle_production::ProductionFeedbackDocumentIdentityV1,
    LspRuntimeFailure,
> {
    let snapshot = generation.metadata().snapshot();
    let file = match document_uri {
        Some(uri) => {
            let logical_path = feedback_document_logical_path(project_root, uri)?;
            snapshot
                .files
                .iter()
                .find(|file| file.logical_path == logical_path)
                .ok_or_else(|| LspRuntimeFailure::new("feedback-code-index-document-unavailable"))?
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
    let manifest = generation.metadata().manifest();
    let generation_digest = ManifestDigest::new(manifest.snapshot_digest.as_str().to_owned())
        .map_err(|_| LspRuntimeFailure::new("feedback-code-index-generation-invalid"))?;
    let language = file
        .language
        .clone()
        .ok_or_else(|| LspRuntimeFailure::new("feedback-code-index-language-unavailable"))?;
    Ok(
        tracedecay_application::feedback::cycle_production::ProductionFeedbackDocumentIdentityV1 {
            generation_id: manifest.generation_id.clone(),
            generation_digest,
            file: file.file_occurrence_id.clone(),
            language,
            content_digest: file.content_digest.clone(),
        },
    )
}

/// The registry is the single mint for file and generation identity, so every
/// diagnostic producer resolves through here instead of inventing its own.
///
/// Without this, a producer had no way to reach the authority and fell back to
/// a repository-relative path; the LSP feedback projection then refused each
/// published record with `ImpactTargetFileMismatch` / `GenerationMismatch`,
/// because the saved-edit cycle's impact target is minted here as
/// `file.daemon.<digest>` under this generation.
impl CodeIndexSchedulerRegistryV1 {
    async fn resolve_current_publication_identity(
        &self,
        project_root: PathBuf,
        scope: Option<tracedecay_contracts::ResolvedScope>,
    ) -> Option<tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityV1>
    {
        let root = project_root.canonicalize().ok()?;
        let root_generation = self.latest_text_serving_for_root(&root).await?;
        let scope = match scope {
            Some(scope) => scope,
            None => {
                let metadata = root_generation.metadata();
                let snapshot = metadata.snapshot();
                tracedecay_contracts::ResolvedScope::new(
                    metadata.manifest().project_id.clone(),
                    snapshot.repository.clone(),
                    snapshot.worktree.clone()?,
                    snapshot.reference.clone(),
                )
                .ok()?
            }
        };
        let (current, fresh) = self.latest_text_serving_freshness_for_scope(&scope).await?;
        if !fresh
            || root_generation.metadata().manifest().generation_id
                != current.metadata().manifest().generation_id
        {
            return None;
        }
        let metadata = current.metadata();
        let snapshot = metadata.snapshot();
        Some(
            tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityV1::new(
                metadata.manifest().generation_id.clone(),
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
    }
}

impl tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
    ) -> tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityFuture<'_>
    {
        let registry = self.clone();
        Box::pin(async move {
            registry
                .resolve_current_publication_identity(project_root, None)
                .await
        })
    }

    fn resolve_current_for_scope(
        &self,
        project_root: PathBuf,
        scope: tracedecay_contracts::ResolvedScope,
    ) -> tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityFuture<'_>
    {
        let registry = self.clone();
        Box::pin(async move {
            registry
                .resolve_current_publication_identity(project_root, Some(scope))
                .await
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

#[cfg(test)]
mod notify_rendezvous_tests {
    use super::wait_notified_if_unset;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn wait_notified_if_unset_observes_a_notify_completed_before_first_poll() {
        let flag = AtomicBool::new(false);
        let notify = Notify::new();
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        flag.store(true, Ordering::Release);
        notify.notify_waiters();
        // The flag check would skip the wait and hide a missed notify. Poll
        // the enabled Notified after the notifier has already finished.
        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .expect("enable() must retain a notify that completed before the first poll");

        let already = AtomicBool::new(true);
        let quiet = Notify::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_notified_if_unset(&already, &quiet),
        )
        .await
        .expect("an already-set flag must not wait");
    }
}
