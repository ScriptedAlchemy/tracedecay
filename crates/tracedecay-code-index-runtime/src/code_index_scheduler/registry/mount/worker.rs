//! The background worker of one mounted worktree. Each wake runs one pass:
//! source reconcile, text projection, the optional graph prepare and
//! activation, the serving swap, and the optional attribution tail.

use std::{
    ops::ControlFlow,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tracedecay_code_index::parallelism::collect_installed_worker_heaps_when_idle;
use tracedecay_code_index::production::{
    CodeIndexInterruptionV1, CodeIndexPublicationStoreErrorV1,
    VerifiedSealedTextGenerationMetadataV1,
};
use tracedecay_contracts::code_index_freshness::{
    CodeIndexBuildBlockedReasonV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId, WorktreeId};
use tracedecay_runtime_core::native_runtime::{self, NativeTask};
use tracedecay_runtime_core::resident_memory::ResidentOwnersV1;

use super::super::super::{
    CodeGraphReplayBindingV1, CodeIndexArrivalV1, CodeIndexBuildProgressSlotV1,
    CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1, CodeIndexGenerationPublishedV1,
    CodeIndexNoopEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexControlV1, DaemonCodeIndexPublicationStoreV1,
    LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1, ReconcilePassGuard, ReconcilePassesV1,
    RetainedTextGenerationRestoreV1, ServingSourceWitnessV1, SharedCodeIndexBytePoolV1,
    SourceFreshnessFenceV1,
    graph_activation::CodeGraphActivationAuthorityV1,
    now_micros,
    observability::CodeIndexObservabilityV1,
    reconcile_panic_guard::{
        ReconcileCapacityRetryV1, ReconcilePanicDecisionV1, ReconcilePanicGuardV1,
    },
    residency::WorktreeResidencyV1,
};
use super::super::{
    ACTIVATION_RETRY_BACKOFF_CEILING, ACTIVATION_RETRY_BACKOFF_FLOOR,
    AttributionPreparationControlV1, CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
    CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
    CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
    CONVERGENCE_PARK_REFRESH_RESIDENT_MEMORY_REMEDIATION_V1,
    CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1, CodeIndexSchedulerRegistryV1 as Registry,
    CodeIndexWorkerPhaseV1, GraphActivationGateV1, GraphSeatGateV1, MemoryRefusalRetryV1,
    PendingWakeV1, PublishedTextProjectionOutcomeV1, ServingGenerationSlot, ServingSwapOutcomeV1,
    TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1, clear_convergence_park,
    clear_graph_resident_memory_park, clear_reconcile_refusal_park,
    convergence_park_retries_on_wake, graph_head_belongs_to_another_generation,
    is_repeated_conflict_verdict, park_convergence, publication_authority_is_terminal,
    retained_noop_requires_follow_up_wake, text_projection_unfinished_withholds_seat,
};
use super::{
    GraphPrepareStopV1, PublicationAuthorityResetV1, ReconcileStepFailureV1,
    StoreReleaseWaitStopV1, exact_and_lexical_ready_for_graph, run_reconcile_step,
    worktree_waits_for_memory,
};
use crate::code_index_task_support::join_native;

/// A text projection the worker owns on its own task.
type TextProjectionTaskV1 = Result<NativeTask<PublishedTextProjectionOutcomeV1>, String>;

/// One blocking source reconcile, kept apart from a panic in its step.
type SourceReconcileResultV1 =
    Result<Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1>, ReconcileStepFailureV1>;

/// A pass's source outcome with the generation it prepared for the serving
/// swap and that generation's graph replay binding.
type PreparedSeatV1 = Result<
    (
        Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1>,
        Option<LatestCompleteCodeIndexV1>,
        Option<CodeGraphReplayBindingV1>,
    ),
    ReconcileStepFailureV1,
>;

/// What the blocking graph prepare step hands back: the decoded generation,
/// its replay binding, whether a refused ignored-source roster claimed a
/// rebuild pass, and why nothing was decoded.
type GraphPrepareStepResultV1 = Result<
    (
        Option<LatestCompleteCodeIndexV1>,
        Option<CodeGraphReplayBindingV1>,
        bool,
        Option<GraphPrepareStopV1>,
    ),
    CodeIndexSchedulerErrorV1,
>;

/// A successor sealed while a pass joined its text projection, with the
/// arrival it answers.
type SealedSuccessorV1 = (
    CodeIndexReconcileOutcomeV1,
    CodeIndexArrivalV1,
    CodeIndexCadenceTriggerV1,
);

/// Why a pass stopped before its end.
enum PassEndV1 {
    /// The pass is done; the worker waits for its next wake.
    NextPass,
    /// The worker stopped. Its retained projection is already joined.
    Exit,
}

/// The handles one worktree's background worker shares with its mount.
pub(super) struct WorktreeWorkerV1 {
    pub(super) phase: Arc<tokio::sync::watch::Sender<CodeIndexWorkerPhaseV1>>,
    pub(super) index_observability: Arc<OnceLock<CodeIndexObservabilityV1>>,
    pub(super) scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    pub(super) reconcile_in_progress: Arc<ReconcilePassesV1>,
    pub(super) build_progress: CodeIndexBuildProgressSlotV1,
    pub(super) serving_generation: Arc<ServingGenerationSlot>,
    pub(super) complete_generation_requested: Arc<AtomicBool>,
    pub(super) complete_generation_requested_changed: tokio::sync::watch::Receiver<bool>,
    pub(super) text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>>,
    pub(super) convergence_park: Arc<RwLock<Option<CodeIndexConvergenceParkedV1>>>,
    pub(super) serving_source_witness: Arc<RwLock<Option<ServingSourceWitnessV1>>>,
    pub(super) serving_generation_epoch: Arc<AtomicU64>,
    pub(super) serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    pub(super) source_freshness: SourceFreshnessFenceV1,
    pub(super) wake: Arc<tokio::sync::Notify>,
    /// The code-index control epoch. It advances exactly when new input is
    /// announced (hook hints, watch paths, overflow), so it is the signal a
    /// quarantined worker uses to decide that the bytes which panicked it
    /// are no longer the bytes it is being asked to index.
    pub(super) control_epoch: Arc<AtomicU64>,
    pub(super) pending_wake: Arc<PendingWakeV1>,
    pub(super) memory_retry: Arc<MemoryRefusalRetryV1>,
    pub(super) cadence_telemetry: Arc<tokio::sync::watch::Sender<CodeIndexCadenceTelemetryV1>>,
    pub(super) shutting_down: Arc<AtomicBool>,
    pub(super) build_publication_lock: Arc<tokio::sync::Mutex<()>>,
    pub(super) background_reconcile_admission: Arc<tokio::sync::Semaphore>,
    pub(super) generation_publications:
        tokio::sync::broadcast::Sender<CodeIndexGenerationPublishedV1>,
    pub(super) serving_seats: Arc<tokio::sync::watch::Sender<u64>>,
    pub(super) project_root: PathBuf,
    pub(super) project_id: ProjectId,
    pub(super) repository_id: RepositoryId,
    pub(super) worktree_id: WorktreeId,
    pub(super) graph_activation: CodeGraphActivationAuthorityV1,
    pub(super) scope_store_root: PathBuf,
    pub(super) residency: Arc<WorktreeResidencyV1>,
    pub(super) resident_owners: Arc<ResidentOwnersV1>,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
}

/// State the worker carries from one pass to the next.
// Each flag is an independent bound with its own lifetime; none is a mode.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct WorktreeWorkerLoopStateV1 {
    /// Headroom other owners released, and the share of it this worker
    /// already answered.
    owner_headroom: asupersync::channel::watch::Receiver<u64>,
    owner_headroom_seen: u64,
    /// Headroom the resident-memory authority gave back to admission.
    admission_headroom: asupersync::channel::watch::Receiver<u64>,
    /// Bounded retry state for activating an already-sealed complete
    /// generation. The sealed artifact is immutable and retryable, so a
    /// retryable activation failure must not fall through into a
    /// rebuild+reseal of an equivalent generation.
    seat_retry_backoff: Duration,
    next_seat_attempt_at: Option<Instant>,
    /// The generation this worker last offered to graph activation
    /// outside the replace gate. It bounds the recovery attempt for a
    /// generation that serves text without a native graph to one per
    /// generation, so a permanently unactivatable seal cannot spin.
    graph_seat_attempted: Option<CodeGenerationId>,
    /// A successor sealed while the previous pass joined its text
    /// projection, with the arrival it answers. The next pass takes it
    /// in place of its own source reconcile, so an edit never queues
    /// behind a projection.
    sealed_successor: Option<SealedSuccessorV1>,
    /// A retained revision-7 graph gets one verified-head attempt
    /// before ordinary source reconciliation owns any repair. A failed
    /// verification falls through to one canonical replay of the same
    /// sealed generation rather than repeating the head read forever.
    retained_graph_head_recovery_attempted: bool,
    /// A retained manifest whose verified head belonged to another
    /// generation. Replaying it would discard the generation that owns
    /// the head. Later retained passes of this same generation skip
    /// that replay; a different generation clears the memory.
    graph_head_conflict_generation: Option<CodeGenerationId>,
    /// The conflict verdict of the previous failed seat attempt. A
    /// Conflict can be a race (a concurrent publisher advanced the
    /// head) and is retried once like any transient failure, but the
    /// same guard site refusing with identical compared evidence for
    /// the same sealed generation on the very next attempt is a
    /// deterministic verdict that no backoff can outwait. One repeat
    /// converts the retry into the terminal typed refusal, so
    /// the backoff ceiling can never become an infinite conflict loop
    /// (issue #765).
    last_seat_conflict: Option<(
        CodeGenerationId,
        tracedecay_graph_db::GraphConflictContextV1,
    )>,
    /// Bounded retry state for a reconcile whose blocking task unwound.
    /// Arbitrary user source runs through the indexing pool, so a panic
    /// there is an input fault, not a programmer-contract break: it
    /// reproduces on every pass over the same bytes. Without this the
    /// loop re-dispatched the identical unit on every wake forever.
    panic_guard: ReconcilePanicGuardV1,
    /// Bounded retry state for a reconcile refused because a graph
    /// operation budget was momentarily held by a sibling worktree.
    /// Releasing that budget emits no wake, so this worker schedules
    /// its own. Memory refusals wait for the headroom wake instead.
    capacity_retry: ReconcileCapacityRetryV1,
    /// The previous pass was refused because another owner holds this
    /// scope's store lock. The next iteration waits for that release
    /// before it runs, so a follow-up wake cannot start another
    /// failing pass while the holder is still inside the lock.
    pass_waits_for_store_release: bool,
    /// Whether this worker already deleted and rebuilt a corrupt derived
    /// publication. One reset per mount bounds the work: a store that
    /// is corrupt again after its own rebuild parks instead of looping
    /// through full re-indexes.
    publication_authority_reset_attempted: bool,
    /// The last arrival this worker restored for a nothing-seated
    /// warming outcome. A quiet remount's seat pass restores its
    /// arrival exactly once so the next pass can restore and warm the
    /// retained text owner; a worktree with nothing restorable (for
    /// example no extractable sources and no publication) reproduces
    /// the identical warming Noop on every pass, and restoring the
    /// same arrival again would spin this worker forever.
    warming_restore_arrival: Option<i64>,
    /// Whether the optional graph prepare already yielded to a pending
    /// arrival since it last actually ran. One yield lets a landed
    /// arrival's exact/lexical pass go first; a second consecutive
    /// yield would make "no arrival pending" a seat precondition, which
    /// on a busy checkout is the quiet-tree starvation the seat gate
    /// rework removed.
    graph_prepare_yielded_to_arrival: bool,
    /// A retained owner's projection may outlive the pass that seats
    /// its verified graph. Keep the task owned by this worker while a
    /// late complete-generation request starts a successor pass; that
    /// successor must neither detach nor duplicate the text owner.
    retained_text_projection: Option<TextProjectionTaskV1>,
    /// Whether the retained projection in flight started with its
    /// query owners already serving, so its finish changes no owner the
    /// seat reads and owes the worker no successor pass.
    retained_projection_successor_only: bool,
}

impl WorktreeWorkerLoopStateV1 {
    pub(super) fn new(
        owner_headroom: asupersync::channel::watch::Receiver<u64>,
        owner_headroom_seen: u64,
        admission_headroom: asupersync::channel::watch::Receiver<u64>,
    ) -> Self {
        Self {
            owner_headroom,
            owner_headroom_seen,
            admission_headroom,
            seat_retry_backoff: ACTIVATION_RETRY_BACKOFF_FLOOR,
            next_seat_attempt_at: None,
            graph_seat_attempted: None,
            sealed_successor: None,
            retained_graph_head_recovery_attempted: false,
            graph_head_conflict_generation: None,
            last_seat_conflict: None,
            panic_guard: ReconcilePanicGuardV1::new(),
            capacity_retry: ReconcileCapacityRetryV1::new(),
            pass_waits_for_store_release: false,
            publication_authority_reset_attempted: false,
            warming_restore_arrival: None,
            graph_prepare_yielded_to_arrival: false,
            retained_text_projection: None,
            retained_projection_successor_only: false,
        }
    }
}

/// The gates a pass holds from its wake through source reconciliation.
struct PassGatesV1 {
    control_epoch: u64,
    wake_to_gates_held_micros: u64,
    admission: tokio::sync::OwnedSemaphorePermit,
    /// Retirement and daemon shutdown set the flag, then fire this watch.
    shutdown_observed: tokio::sync::watch::Receiver<()>,
    build_publication: tokio::sync::OwnedMutexGuard<()>,
}

/// What source reconciliation hands the rest of the pass.
struct SourcePassV1 {
    arrival: CodeIndexArrivalV1,
    trigger: CodeIndexCadenceTriggerV1,
    started_micros: i64,
    graph_activation_deferred: bool,
    retained_text: Option<LatestCodeTextGenerationV1>,
    source_result: SourceReconcileResultV1,
    retained_noop_follow_up: bool,
}

/// A publication's replacement text owner and the projection this pass
/// drives it with.
struct PublishedTextV1 {
    graph_text: Option<LatestCodeTextGenerationV1>,
    projection: Option<TextProjectionTaskV1>,
    opened: Option<tokio::sync::oneshot::Receiver<()>>,
}

/// How the sealed graph publication before the serving decode ended.
#[derive(Default)]
struct SealedGraphPublicationV1 {
    refusal: Option<String>,
    head_published: bool,
    budget_spent: bool,
}

impl WorktreeWorkerV1 {
    pub(super) async fn run(mut self, mut state: WorktreeWorkerLoopStateV1) {
        loop {
            // Boxed on purpose: one pass is the largest future in the daemon
            // (reconcile + text advance + decode + activation + swap inline).
            // Carried unboxed, the worker's own future holds the whole
            // machine, and materializing that on the spawning runtime
            // worker's stack before the spawn moves it to the heap was
            // measured as an instant stack overflow at project mount (the
            // same pattern as the `_inner` boxed fns from the 37MB-future fix).
            let pass = Box::pin(self.run_pass(&mut state));
            if let ControlFlow::Break(PassEndV1::Exit) = pass.await {
                return;
            }
        }
    }

    /// Stop the worker. Its retained projection is joined first, so
    /// shutdown sees the work it owns.
    async fn exit<T>(state: &mut WorktreeWorkerLoopStateV1) -> ControlFlow<PassEndV1, T> {
        Registry::join_retained_text_projection_on_worker_exit(&mut state.retained_text_projection)
            .await;
        ControlFlow::Break(PassEndV1::Exit)
    }

    /// Whether shutdown was observed at `phase`, logged when it was.
    fn shutdown_observed(&self, phase: &'static str) -> bool {
        if !self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        tracing::info!(
            event = "code_index_worker_shutdown_observed",
            phase,
            "code-index worker observed shutdown and stopped its pass"
        );
        true
    }

    async fn run_pass(&mut self, state: &mut WorktreeWorkerLoopStateV1) -> ControlFlow<PassEndV1> {
        self.await_wake(state).await?;
        let mut gates = self.hold_pass_gates(state).await?;
        let graph_activation_enabled = self.graph_activation.policy().is_enabled();
        if self.graph_off_text_is_settled(graph_activation_enabled) {
            return ControlFlow::Break(PassEndV1::NextPass);
        }
        // Cover wake claim through failed-arrival restoration so admission
        // never misreads in-flight owner work as plain unavailability.
        let mut reconcile_pass = Some(ReconcilePassGuard::enter(&self.reconcile_in_progress));
        let mut retained_work_waiting_for_source = false;
        let mut text_generation = self
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.recheck_parked_text_owner(&mut text_generation);
        let mut text_slice_incomplete = false;
        // The retained owner's bounded projection, driven to
        // completion concurrently with this pass's graph seat and
        // joined before the pass ends.
        if state.retained_text_projection.is_none()
            && let Some(latest) = text_generation.clone()
            && latest.text_projection_needs_work()
            && graph_activation_enabled
        {
            retained_work_waiting_for_source = self.start_retained_text_projection(state, latest);
        } else if let Some(latest) = text_generation
            && latest.text_projection_needs_work()
        {
            text_slice_incomplete = self.advance_text_slice(state, latest).await;
        }
        if self.shutdown_observed("text_projection") {
            return Self::exit(state).await;
        }
        if text_slice_incomplete {
            if !graph_activation_enabled {
                return ControlFlow::Break(PassEndV1::NextPass);
            }
            if Registry::incomplete_text_slice_may_continue(&self.pending_wake) {
                return ControlFlow::Break(PassEndV1::NextPass);
            }
        }
        let refused_retained_text_metadata = self.restore_retained_text(state).await?;
        let source = self
            .reconcile_source(
                state,
                graph_activation_enabled,
                text_slice_incomplete,
                gates.wake_to_gates_held_micros,
                refused_retained_text_metadata,
            )
            .await?;
        // Source reconciliation is complete: release the background
        // admission permit before HeadOpening / graph work so sibling
        // stores can start. Do not re-acquire it until this pass
        // drops `build_publication` at its optional derivation tail:
        // `run_ignored_dependency_admission` takes the
        // admission *before* that same gate, so waiting on admission
        // here would invert that order (see
        // `background_worker_waits_for_global_admission_before_publication_gate`).
        // Keep `reconcile_pass` through text seating, dropping it
        // made `reconcile_in_progress` lie while this worker still
        // owned graph try_lock, which deadlocked tests that hold the
        // scheduler mutex and wait for that flag.
        drop(gates.admission);
        // A publication reopens its own lightweight text owner and
        // swaps it in for the prior one in a single write, so status
        // always names a committed generation. A failed
        // reopen withdraws the prior owner instead: the durable
        // pointer has moved, and the next pass restores from it.
        let published_pass = matches!(
            &source.source_result,
            Ok(Ok(CodeIndexReconcileOutcomeV1::Published(_)))
        );
        let mut graph_text = source.retained_text.clone();
        // The replacement owner's bounded projection runs on its own
        // task while this pass prepares and activates the graph. Both
        // consume the same sealed generation and are corpus-sized, so
        // graph starts only after text's first advance has opened the
        // build and taken its reservation: graph replay can then no
        // longer hold the source or the RSS headroom text needs to
        // open. The serving swap still waits for the text outcome.
        let mut published_text_projection = None;
        let mut published_text_opened = None;
        if published_pass {
            let published = self
                .reopen_published_text(
                    state,
                    &mut reconcile_pass,
                    graph_activation_enabled && !source.graph_activation_deferred,
                )
                .await?;
            graph_text = published.graph_text;
            published_text_projection = published.projection;
            published_text_opened = published.opened;
        }
        let mut prepare_graph = self.decide_graph_seat(
            state,
            &mut reconcile_pass,
            graph_activation_enabled,
            &source,
            published_pass,
            published_text_projection.is_some(),
            graph_text.as_ref(),
        );
        let mut retained_head_recovered_without_complete_replay = false;
        let graph_already_serves = graph_text
            .as_ref()
            .is_some_and(|retained| retained.interactive_graph_store().is_ok());
        self.narrow_graph_prepare_to_demand(
            state,
            &mut prepare_graph,
            published_pass,
            graph_already_serves,
            graph_text.as_ref(),
        );
        // Recovery installs a fresh graph store on the owner; an owner
        // that already serves one (a publication seated it) keeps it
        // and its warm catalog.
        if prepare_graph
            && !published_pass
            && !graph_already_serves
            && !state.retained_graph_head_recovery_attempted
            && let Some(retained) = graph_text
                .as_ref()
                .filter(|retained| retained.uses_partitioned_manifest())
                .cloned()
        {
            retained_head_recovered_without_complete_replay = self
                .recover_retained_graph_head(state, retained, &mut prepare_graph)
                .await;
        }
        let prepare_graph = self
            .confirm_graph_prepare(
                prepare_graph,
                published_pass,
                graph_already_serves,
                graph_text.as_ref(),
                published_text_projection.is_some(),
                &mut published_text_opened,
            )
            .await;
        // A publication's graph build refused while its own text
        // projection still holds the build memory is sequencing, not
        // a stall: it runs again as soon as that projection joins.
        let mut graph_waits_for_text = false;
        let mut graph_publication_budget_spent = false;
        let text_projection_running = published_text_projection.is_some();
        let mut result = match source.source_result {
            Ok(outcome) if prepare_graph => {
                self.prepare_graph_seat(
                    state,
                    outcome,
                    graph_text.as_ref(),
                    graph_already_serves,
                    published_pass,
                    text_projection_running,
                    &mut graph_waits_for_text,
                    &mut graph_publication_budget_spent,
                )
                .await
            }
            Ok(outcome) => Ok((outcome, None, None)),
            Err(error) => Err(error),
        };
        let replace_serving_generation = self.replaces_serving_generation(&result);
        if Self::activates_graph(
            state,
            &result,
            graph_publication_budget_spent,
            graph_already_serves,
            replace_serving_generation,
        ) && let Ok((Ok(_), Some(latest), Some(replay_binding))) = &result
        {
            self.activate_graph(state, latest, replay_binding).await;
        }
        // Join the publication's projection, then process its outcome
        // at the existing source-proof and serving-swap boundary. Graph
        // work above overlapped it; nothing seats before text is done.
        let published_text_projection_outcome = match published_text_projection.take() {
            Some(projection) => Some(
                self.join_published_text_projection(
                    state,
                    &projection,
                    graph_activation_enabled,
                    graph_text.as_ref(),
                )
                .await,
            ),
            None => None,
        };
        if graph_waits_for_text {
            Registry::note_worker_continuation(&self.pending_wake, &self.wake);
        }
        if let Some(outcome) = published_text_projection_outcome {
            self.settle_published_text_projection(
                state,
                outcome,
                &mut result,
                graph_text.as_ref(),
                published_pass,
                source.arrival,
                source.trigger,
            )
            .await?;
        }
        #[cfg(test)]
        if matches!(&result, Ok((Ok(_), Some(_), _))) {
            Registry::wait_for_serving_swap_gate(&self.project_root).await;
        }
        if let Ok((Ok(_), Some(latest), _)) = &result
            && let Some(failure) = self
                .swap_serving_generation(latest, replace_serving_generation)
                .await
        {
            result = Ok((Err(failure), None, None));
        }
        self.settle_pass(
            state,
            &result,
            &mut reconcile_pass,
            retained_work_waiting_for_source,
            source.arrival,
            source.trigger,
            source.started_micros,
            gates.control_epoch,
        );
        if state.retained_text_projection.is_some() {
            self.join_retained_text_projection(
                state,
                &mut reconcile_pass,
                retained_head_recovered_without_complete_replay,
                source.retained_text.as_ref(),
            )
            .await?;
        }
        if self.shutdown_observed("pass_end") {
            return Self::exit(state).await;
        }
        drop(gates.build_publication);
        self.prepare_test_attribution(state, &mut gates.shutdown_observed, gates.control_epoch)
            .await
    }

    /// Wait for this pass's wake: the store release a refused pass owes,
    /// else the worker's own `Notify`. A terminal publication park consumes
    /// the wake without a pass.
    async fn await_wake(&self, state: &mut WorktreeWorkerLoopStateV1) -> ControlFlow<PassEndV1> {
        // Memory given back while the last pass was being refused
        // reached the watcher before that refusal was visible.
        let headroom_moved = self
            .residency
            .park_headroom
            .released_by_others(&mut state.owner_headroom)
            > state.owner_headroom_seen
            || state.admission_headroom.has_changed();
        if headroom_moved
            && worktree_waits_for_memory(
                &self.text_generation,
                &self.convergence_park,
                &self.residency,
            )
        {
            Registry::note_wake(
                &self.pending_wake,
                &self.wake,
                CodeIndexCadenceTriggerV1::MemoryHeadroom,
            );
        }
        // A banked follow-up permit must not skip this wait. The
        // previous pass already restored its arrival; this iteration
        // runs that pass once the holder lets go.
        let mut entered_after_store_release = false;
        if state.pass_waits_for_store_release {
            state.pass_waits_for_store_release = false;
            CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
            match Registry::wait_for_worker_store_release(
                &self.scope_store_root,
                &self.shutting_down,
                &self.serving_generation_changed,
            )
            .await
            {
                Ok(()) => entered_after_store_release = true,
                Err(StoreReleaseWaitStopV1::Shutdown) => {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "store_release_wait",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    return Self::exit(state).await;
                }
                Err(StoreReleaseWaitStopV1::Unavailable(reason)) => {
                    Registry::park_store_release_unavailable(&self.convergence_park, reason);
                }
            }
        }
        if !entered_after_store_release {
            let notified = self.wake.notified();
            tokio::pin!(notified);
            // Parked only while registered with no banked permit: a
            // banked permit resolves the wait at once, so the worker was
            // never idle.
            if !notified.as_mut().enable() {
                self.residency
                    .release_decode_when_parked(&self.resident_owners);
                CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Parked);
            }
            tracing::Instrument::instrument(
                notified,
                tracing::trace_span!("daemon.code_index.wake_wait"),
            )
            .await;
        }
        state.owner_headroom_seen = state.owner_headroom_seen.max(
            self.residency
                .park_headroom
                .released_by_others(&mut state.owner_headroom),
        );
        state.admission_headroom.mark_seen();
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
        if self.shutdown_observed("wake") {
            return Self::exit(state).await;
        }
        // The shared park slot is the authority admission already
        // consults. A task-local bool would ignore a park planted by
        // another actor and would reset if this loop state were lost
        // while the slot remained.
        if publication_authority_is_terminal(&self.convergence_park) {
            state.sealed_successor = None;
            let _ = Registry::take_pending_arrival(
                &self.pending_wake,
                &self.wake,
                CodeIndexCadenceTriggerV1::Mount,
            );
            tracing::debug!(
                event = "code_index_reconcile_terminal_suppressed",
                path = "background_worker",
                "code-index reconcile is blocked until the publication authority is reset"
            );
            return ControlFlow::Break(PassEndV1::NextPass);
        }
        ControlFlow::Continue(())
    }

    /// Take the background admission and the publication gate for this
    /// pass, unless the panic guard suppresses it.
    async fn hold_pass_gates(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
    ) -> ControlFlow<PassEndV1, PassGatesV1> {
        // This aggregate starts when the wake is observed and ends
        // when the pass holds every admission/publication gate it
        // needs. It deliberately does not attribute the span to the
        // SQL writer alone.
        let pass_wake_observed_at = Instant::now();
        let pass_control_epoch = self.control_epoch.load(Ordering::Acquire);
        // A quarantined or backing-off unit must not consume the
        // pending arrival: the wake stays outstanding so a later
        // eligible pass still measures its full queue wait.
        if state
            .panic_guard
            .suppresses_pass(Instant::now(), pass_control_epoch)
        {
            tracing::debug!(
                event = "code_index_reconcile_panic_suppressed",
                path = "background_worker",
                consecutive_panics = state.panic_guard.consecutive_panics(),
                "code-index reconcile is suppressed over unchanged input after a panic or a reproducing failure"
            );
            return ControlFlow::Break(PassEndV1::NextPass);
        }
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::AwaitingAdmission);
        let Ok(background_reconcile_admission) = tracing::Instrument::instrument(
            Arc::clone(&self.background_reconcile_admission).acquire_owned(),
            tracing::trace_span!("daemon.code_index.admission_wait"),
        )
        .await
        else {
            return Self::exit(state).await;
        };
        // Retirement and daemon shutdown set the flag, then fire this
        // watch, so the gate wait below wakes on them.
        let mut shutdown_observed = self.serving_generation_changed.subscribe();
        if self.shutdown_observed("gates_held") {
            return Self::exit(state).await;
        }
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::AwaitingPublicationGate);
        let mut publication_gate =
            std::pin::pin!(Arc::clone(&self.build_publication_lock).lock_owned());
        let build_publication = loop {
            tokio::select! {
                guard = &mut publication_gate => break guard,
                changed = shutdown_observed.changed() => {
                    if changed.is_err() || self.shutting_down.load(Ordering::Acquire) {
                        tracing::info!(
                            event = "code_index_worker_shutdown_observed",
                            phase = "build_publication_lock",
                            "code-index worker observed shutdown and stopped its pass"
                        );
                        return Self::exit(state).await;
                    }
                }
            }
        };
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
        #[cfg(test)]
        Registry::panic_worker_pass_if_armed(&self.project_root);
        let wake_to_gates_held_micros =
            u64::try_from(pass_wake_observed_at.elapsed().as_micros()).unwrap_or(u64::MAX);
        ControlFlow::Continue(PassGatesV1 {
            control_epoch: pass_control_epoch,
            wake_to_gates_held_micros,
            admission: background_reconcile_admission,
            shutdown_observed,
            build_publication,
        })
    }

    /// A coalesced text-slice wake can outlive the graph-off pass
    /// that drained its final overflow. Do not project that bare
    /// Notify permit as a second refresh: no arrival and no hint
    /// means there is no source work left, while a scheduler-owned
    /// raw overflow still reaches the worker through `None` here.
    fn graph_off_text_is_settled(&self, graph_activation_enabled: bool) -> bool {
        !graph_activation_enabled
            && !self.pending_wake.has_pending_arrival()
            && self
                .text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready)
            && match self.scheduler.try_lock() {
                Ok(scheduler) => scheduler.pending_hint_count() == Some(0),
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error.into_inner().pending_hint_count() == Some(0)
                }
                Err(std::sync::TryLockError::WouldBlock) => false,
            }
    }

    /// A query-driven advance can finish the build between worker
    /// passes; a park observed earlier must not outlive the
    /// violation it named.
    fn recheck_parked_text_owner(&self, text_generation: &mut Option<LatestCodeTextGenerationV1>) {
        if text_generation
            .as_ref()
            .is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready)
        {
            clear_convergence_park(&self.convergence_park);
        } else if convergence_park_retries_on_wake(&self.convergence_park)
            && text_generation
                .as_ref()
                .is_some_and(|latest| !latest.text_projection_needs_work())
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
                let mut current = self
                    .text_generation
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
    }

    /// Start the retained owner's projection on its own task when it may run
    /// now. Returns whether work behind serving owners waits for the source
    /// to settle, which schedules the successor pass.
    fn start_retained_text_projection(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        latest: LatestCodeTextGenerationV1,
    ) -> bool {
        // Warming a missing owner always runs here. Work left behind
        // an owner that already serves runs only once the seated
        // generation matches this text owner and the source is
        // current: starting it while the serving slot is empty,
        // mismatched, or the checkout is dirty races the
        // publish/seat path that still owns the receipt bound.
        let owners_ready = latest.query_owners_are_ready();
        let serving_matches_text = self
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|serving| {
                serving.generation().manifest().generation_id
                    == latest.metadata().manifest().generation_id
            });
        let source_current = self
            .source_freshness
            .ready_without_stat(&self.project_root, &self.shutting_down);
        // This flag schedules the successor pass; that pass
        // re-checks `serving_matches_text` before driving it.
        let retained_work_waiting_for_source = owners_ready && !source_current;
        let drive_retained = !owners_ready || (serving_matches_text && source_current);
        if !drive_retained {
            return retained_work_waiting_for_source;
        }
        // The retained owner projects on its own task, exactly as
        // a publication's replacement owner does, and this pass
        // joins it after the graph seat. Awaiting a slice here
        // instead put the whole projection ahead of the seat, and
        // one slice is not divisible below its finalization: a
        // restart that resumed an unfinished ngram index spent
        // that entire build -- 377 s measured on a 5,181-file
        // corpus -- before the pass even reached the gate that
        // would have recovered the verified head in 8 s.
        let shutting_down = Arc::clone(&self.shutting_down);
        let park = Arc::clone(&self.convergence_park);
        let installed = Arc::clone(&self.text_generation);
        #[cfg(any(test, feature = "test-helpers"))]
        let gated_root = self.project_root.clone();
        // The pass guard is what `rebuild_in_flight`, the
        // `verifying` freshness state and a read's busy fence
        // consult: it means query serving is still being
        // produced. Holding it for work behind owners that
        // already serve reported a complete current generation
        // as verifying. The worker still owns and joins
        // the task, so shutdown sees the work.
        state.retained_projection_successor_only = owners_ready;
        let projection_pass = (!state.retained_projection_successor_only)
            .then(|| ReconcilePassGuard::enter(&self.reconcile_in_progress));
        let projection_pending_wake = Arc::clone(&self.pending_wake);
        let projection_wake = Arc::clone(&self.wake);
        let projection_serving_changed = self.serving_generation_changed.clone();
        let projection_store_root = self.scope_store_root.clone();
        state.retained_text_projection = Some(NativeTask::spawn(async move {
            let _projection_pass = projection_pass;
            #[cfg(any(test, feature = "test-helpers"))]
            Registry::wait_for_retained_text_projection_gate(&gated_root).await;
            let outcome = Registry::drive_text_projection(
                latest,
                shutting_down,
                park,
                Some(installed),
                None,
                &projection_store_root,
                &projection_serving_changed,
                #[cfg(test)]
                gated_root,
            )
            .await;
            match outcome {
                // Exact and lexical serve from here on, while
                // this pass may still be recovering the graph:
                // search waiters must wake now, not at the seat.
                PublishedTextProjectionOutcomeV1::Finished => {
                    projection_serving_changed.send_replace(());
                }
                PublishedTextProjectionOutcomeV1::Unfinished => {
                    Registry::note_worker_continuation(&projection_pending_wake, &projection_wake);
                }
                PublishedTextProjectionOutcomeV1::WaitingForMemory
                | PublishedTextProjectionOutcomeV1::WaitingForStore
                | PublishedTextProjectionOutcomeV1::Shutdown => {}
            }
            outcome
        }));
        retained_work_waiting_for_source
    }

    /// Graph activation is off for this worktree: there is no
    /// seat to unblock, so the slice stays inline and the pass
    /// keeps yielding back to the loop between slices. Returns whether the
    /// slice left projection work behind.
    async fn advance_text_slice(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        latest: LatestCodeTextGenerationV1,
    ) -> bool {
        let failed_latest = latest.clone();
        let build = tracing::Instrument::instrument(
            native_runtime::spawn_blocking(move || {
                latest.advance_text_serving(TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1)
            }),
            tracing::trace_span!("daemon.code_index.text_projection"),
        )
        .await;
        match build {
            Ok(Ok(true)) => {
                clear_convergence_park(&self.convergence_park);
            }
            Ok(Ok(false)) => {
                clear_convergence_park(&self.convergence_park);
                self.wake.notify_one();
                return true;
            }
            Ok(Err(error)) => {
                if matches!(
                    &error,
                    tracedecay_query::retrieval::RetrievalPortError::Cancelled
                ) {
                    let mut current = self
                        .text_generation
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
                        &self.convergence_park,
                        error.to_string(),
                        CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
                        None,
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
                } else if error.is_generation_store_lock_contended() {
                    state.pass_waits_for_store_release = true;
                    tracing::info!(
                        event = "code_index_text_projection_waiting_for_store",
                        "code-index background text projection waits for the \
                         code-generation store lock"
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
                    &self.convergence_park,
                    format!("code text projection task failed abnormally: {error}"),
                    CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                    None,
                    false,
                );
                tracing::warn!(
                    event = "code_index_text_projection_task_failed",
                    error = %error,
                    "code-index background text projection task failed"
                );
            }
        }
        false
    }

    /// Restore the retained text owner into an empty text slot. A servable
    /// owner ends the pass with a wake for the pass that warms it; a refused
    /// one hands its metadata to source reconciliation.
    async fn restore_retained_text(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
    ) -> ControlFlow<PassEndV1, Option<VerifiedSealedTextGenerationMetadataV1>> {
        if self
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return ControlFlow::Continue(None);
        }
        let text_scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let retained_text = tracing::Instrument::instrument(
            native_runtime::spawn_blocking(move || {
                Registry::lock_scheduler_unless_shutting_down(&text_scheduler, &shutting_down)
                    .map(|mut scheduler| scheduler.restore_retained_text_generation())
            }),
            tracing::trace_span!("daemon.code_index.text_restore"),
        )
        .await;
        if self.shutdown_observed("text_restore") {
            return Self::exit(state).await;
        }
        ControlFlow::Continue(match retained_text {
            Ok(Ok(Some(RetainedTextGenerationRestoreV1::Servable(retained_text)))) => {
                *self
                    .text_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retained_text);
                // A restored text owner is a serving change with
                // no publication: waiters subscribed before the
                // restore must wake now, not after the next pass.
                self.serving_generation_changed.send_replace(());
                self.wake.notify_one();
                return ControlFlow::Break(PassEndV1::NextPass);
            }
            Ok(Ok(Some(RetainedTextGenerationRestoreV1::Refused(metadata)))) => Some(*metadata),
            Ok(Ok(Some(RetainedTextGenerationRestoreV1::Failed(error)))) => {
                tracing::error!(
                    event = "code_index_retained_text_restore_failed",
                    error = %error,
                    "retained text restore failed at decoded-cache release"
                );
                None
            }
            _ => None,
        })
    }

    /// Claim this pass's arrival and run its blocking source reconcile (or
    /// take the successor a previous pass already sealed).
    async fn reconcile_source(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        graph_activation_enabled: bool,
        text_slice_incomplete: bool,
        wake_to_gates_held_micros: u64,
        refused_retained_text_metadata: Option<VerifiedSealedTextGenerationMetadataV1>,
    ) -> ControlFlow<PassEndV1, SourcePassV1> {
        let text_serving_ready = self
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready);
        // Admission is held: queue wait ends and service time begins.
        let started_micros = now_micros().0;
        // A sealed successor answers the arrival it claimed; a later
        // arrival stays pending for its own pass.
        let (arrival, trigger) = match state.sealed_successor.as_ref() {
            Some((_, arrival, trigger)) => (*arrival, *trigger),
            None => Registry::take_pending_arrival(
                &self.pending_wake,
                &self.wake,
                CodeIndexCadenceTriggerV1::Mount,
            ),
        };
        if text_slice_incomplete {
            self.wake.notify_one();
        }
        // A retryable native-graph failure defers only graph
        // activation. Reconcile and finish the lightweight text owner
        // before opening the full generation: a large graph replay
        // must never become exact/lexical time-to-ready. During
        // backoff, the scheduled retry is the only pass that may open
        // the immutable full generation again.
        let graph_activation_deferred = state
            .next_seat_attempt_at
            .is_some_and(|at| Instant::now() < at);
        let retained_text = self
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let memory_pass = trigger == CodeIndexCadenceTriggerV1::MemoryHeadroom;
        if memory_pass {
            self.memory_retry.reset();
        }
        // Memory given back is the retry for a graph refused at the
        // watermark: its generation activates again on this pass.
        if memory_pass
            && retained_text
                .as_ref()
                .is_some_and(LatestCodeTextGenerationV1::retry_resident_memory_graph_refusal)
        {
            state.graph_seat_attempted = None;
            tracing::info!(
                event = "code_index_graph_activation_memory_retry",
                trigger = trigger.label(),
                "memory was given back; the refused native graph activates again"
            );
        }
        let serving_empty = self
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        let retained_text_uses_partitioned_manifest = retained_text
            .as_ref()
            .is_some_and(LatestCodeTextGenerationV1::uses_partitioned_manifest);
        // A revision-7 owner can restore its retained persistent graph
        // directly from the verified head. Give that recovery exactly
        // one empty-slot *mount* pass before the dirty source capture
        // creates its successor. Once the retained graph is Ready, this
        // guard falls through to the ordinary reconciliation path
        // instead of repeatedly consuming the successor's wake as a
        // Noop.
        //
        // Park also empties the decode seat. That is not a remount:
        // a later HookHint / GitWatcher / QueryAdmission must capture
        // the dirty source on the arrival that requested it. Stealing
        // that wake for reserved recovery published the edit on an
        // unattributable BusyFollowUp and wiped clone-update duration.
        let retained_partitioned_graph_recovery_pending = graph_activation_enabled
            && !graph_activation_deferred
            && serving_empty
            && !state.retained_graph_head_recovery_attempted
            && trigger == CodeIndexCadenceTriggerV1::Mount
            && retained_text.as_ref().is_some_and(|text| {
                text.uses_partitioned_manifest() && text.interactive_graph_store().is_err()
            });
        let retained_text_metadata = retained_text
            .as_ref()
            .map(|text| text.metadata().clone())
            .or(refused_retained_text_metadata);
        // Phase boundary: source reconciliation begins. Together with
        // `code_index_generation_published` / `_interrupted` and
        // `code_index_serving_generation_seated` this lets a status
        // reader attribute a `progress: null` warming window to the
        // uninstrumented capture+seal instead of to text or graph work.
        tracing::info!(
            event = "code_index_reconcile_pass_started",
            path = "background_worker",
            trigger = trigger.label(),
            arrival = arrival.label(),
            queue_delay_micros = ?arrival
                .wake_micros()
                .map(|wake_micros| started_micros.saturating_sub(wake_micros).max(0)),
            wake_to_gates_held_micros,
            retained_text = retained_text_metadata
                .as_ref()
                .map(|metadata| metadata.manifest().generation_id.as_str()),
            text_serving_ready,
            serving_empty,
            graph_activation_deferred,
            "code-index background reconcile pass started"
        );
        let step = SourceReconcileStepV1 {
            stashed_successor: state.sealed_successor.take().map(|(outcome, ..)| outcome),
            scheduler: Arc::clone(&self.scheduler),
            shutting_down: Arc::clone(&self.shutting_down),
            graph_activation_enabled,
            graph_activation_deferred,
            serving_empty,
            text_serving_ready,
            retained_text_uses_partitioned_manifest,
            retained_partitioned_graph_recovery_pending,
            retained_text_metadata,
            serving_generation: Arc::clone(&self.serving_generation),
            serving_source_witness: Arc::clone(&self.serving_source_witness),
            source_freshness: self.source_freshness.clone(),
        };
        let source_result = tracing::Instrument::instrument(
            run_reconcile_step(move || step.run()),
            // Sealing runs inside this blocking reconcile pipeline; the
            // outer span keeps the end-to-end seal path in one trace row.
            tracing::trace_span!("daemon.code_index.reconcile_or_seal"),
        )
        .await;
        if self.shutdown_observed("reconcile_or_seal") {
            return Self::exit(state).await;
        }
        if let Ok(Ok(CodeIndexReconcileOutcomeV1::Published(evidence))) = &source_result {
            // Announce only. This pass has not seated anything yet:
            // the replacement text owner reopens below and the serving
            // swap runs after graph work, so recording the id here
            // made `latest_generation_id` name a generation every
            // serving arm still answered the *previous* id for.
            Registry::broadcast_generation_publication(
                &self.generation_publications,
                self.project_root.clone(),
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
        let retained_noop_follow_up = retained_noop_requires_follow_up_wake(
            serving_empty,
            graph_activation_deferred,
            arrival.wake_micros().is_some(),
            matches!(&source_result, Ok(Ok(CodeIndexReconcileOutcomeV1::Noop(_)))),
        );
        if let Ok(Ok(outcome)) = &source_result {
            // The source no longer reproduces a refusal parked over
            // earlier input; retire it before this pass seats.
            clear_reconcile_refusal_park(&self.convergence_park);
            Registry::record_source_reconcile_observation(
                self.index_observability.get(),
                &self.pending_wake,
                outcome,
                started_micros,
            );
        }
        ControlFlow::Continue(SourcePassV1 {
            arrival,
            trigger,
            started_micros,
            graph_activation_deferred,
            retained_text,
            source_result,
            retained_noop_follow_up,
        })
    }

    /// Reopen a publication's lightweight text owner and swap it in for the
    /// prior one, then start its bounded projection when graph work follows.
    async fn reopen_published_text(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        reconcile_pass: &mut Option<ReconcilePassGuard>,
        graph_follows: bool,
    ) -> ControlFlow<PassEndV1, PublishedTextV1> {
        let text_scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let published_text = native_runtime::spawn_blocking(move || {
            Registry::lock_scheduler_unless_shutting_down(&text_scheduler, &shutting_down)
                .map(|mut scheduler| scheduler.servable_retained_text_generation())
        })
        .await;
        if self.shutdown_observed("published_text_reopen") {
            return Self::exit(state).await;
        }
        let graph_text = match published_text {
            Ok(Ok(Ok(Some(published_text)))) => {
                let mut serving_text = self
                    .text_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                published_text.hold_outgoing_graph(serving_text.as_ref());
                *serving_text = Some(published_text.clone());
                drop(serving_text);
                // The publication broadcast went out before the
                // successor's owner was installed; waiters that
                // probed in between must wake now.
                self.serving_generation_changed.send_replace(());
                Some(published_text)
            }
            failed => {
                if let Ok(Ok(Err(error))) = &failed {
                    tracing::error!(
                        event = "code_index_published_text_reopen_failed",
                        error = %error,
                        "published text restore failed at decoded-cache release"
                    );
                }
                *self
                    .text_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                self.serving_generation_changed.send_replace(());
                None
            }
        };
        let mut published = PublishedTextV1 {
            graph_text,
            projection: None,
            opened: None,
        };
        // Drive the replacement text owner in this pass. Exact and
        // lexical are the required fresh-index product; graph
        // activation is an optional projection that must not take
        // the source or resident-memory headroom text needs to
        // open. Yielding back to the loop instead would hand the
        // next pass a checkout that has already moved, and on a
        // shared repository that pass publishes again - which is
        // exactly how a sealed generation stayed unseated forever.
        if graph_follows && let Some(text) = published.graph_text.clone() {
            // `reconcile_pass` covers this projection, so the
            // pointer rename is inside the pass a reader samples.
            // The task releases it when text stops, after stamping
            // the continuation the projection still owes: graph
            // work that outlives text is not query serving. Taking
            // the admission permit back here instead would
            // deadlock against an ignored-dependency owner that
            // already holds it and is waiting for
            // `build_publication`.
            let projection_pass = if state.retained_text_projection.is_none()
                || state.retained_projection_successor_only
            {
                reconcile_pass.take()
            } else {
                None
            };
            let (opened, text_opened) = tokio::sync::oneshot::channel();
            published.opened = Some(text_opened);
            let shutting_down = Arc::clone(&self.shutting_down);
            let park = Arc::clone(&self.convergence_park);
            let projection_pending_wake = Arc::clone(&self.pending_wake);
            let projection_wake = Arc::clone(&self.wake);
            let projection_store_root = self.scope_store_root.clone();
            let projection_serving_changed = self.serving_generation_changed.clone();
            #[cfg(test)]
            let project_root = self.project_root.clone();
            published.projection = Some(NativeTask::spawn(async move {
                let _projection_pass = projection_pass;
                let outcome = Registry::drive_text_projection(
                    text.clone(),
                    shutting_down,
                    park,
                    None,
                    Some(opened),
                    &projection_store_root,
                    &projection_serving_changed,
                    #[cfg(test)]
                    project_root,
                )
                .await;
                let schedule_continuation = match outcome {
                    PublishedTextProjectionOutcomeV1::Finished => text.text_projection_needs_work(),
                    PublishedTextProjectionOutcomeV1::Unfinished => true,
                    PublishedTextProjectionOutcomeV1::WaitingForMemory
                    | PublishedTextProjectionOutcomeV1::WaitingForStore
                    | PublishedTextProjectionOutcomeV1::Shutdown => false,
                };
                if schedule_continuation {
                    Registry::note_worker_continuation(&projection_pending_wake, &projection_wake);
                }
                outcome
            }));
        } else if published
            .graph_text
            .as_ref()
            .is_none_or(LatestCodeTextGenerationV1::text_projection_needs_work)
        {
            Registry::note_worker_continuation(&self.pending_wake, &self.wake);
        }
        ControlFlow::Continue(published)
    }

    /// Decide whether this pass prepares the graph seat, and settle the
    /// arrival and pass-guard bookkeeping that decision owes.
    #[allow(clippy::too_many_arguments)]
    fn decide_graph_seat(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        reconcile_pass: &mut Option<ReconcilePassGuard>,
        graph_activation_enabled: bool,
        source: &SourcePassV1,
        published_pass: bool,
        published_text_projection_running: bool,
        graph_text: Option<&LatestCodeTextGenerationV1>,
    ) -> bool {
        let SourcePassV1 {
            arrival,
            trigger,
            graph_activation_deferred,
            source_result,
            retained_noop_follow_up,
            ..
        } = source;
        let (arrival, trigger) = (*arrival, *trigger);
        // Graph seating must not wait for the checkout to hold still.
        // Requiring a `Noop` outcome made tree quiescence the seat
        // condition, and a shared checkout with peers editing never
        // offers that window: every pass published a new generation, so
        // three full seals produced zero seat attempts and every exact
        // graph read answered "not ready" while a complete sealed
        // generation sat on disk. A publication now seats on its own
        // pass, once its lightweight text owner has reopened and its
        // projection has finished; it is stale by
        // construction and superseded by the next publication. An
        // unchanged pass still seats the retained Ready text owner's
        // generation. Source reconciliation is complete either way:
        // release its public freshness guard before the optional
        // O(store) full decode and native graph activation begin; a
        // publication's projection task holds its own share until text
        // stops. Optional graph must not hold it: each graph step that
        // must take the scheduler re-enters the pass around that
        // acquisition (see `lock_scheduler_for_graph_step`); only the
        // unlocked decode and native activation run outside it.
        let gate = GraphSeatGateV1::decide(
            graph_activation_enabled,
            *graph_activation_deferred,
            matches!(source_result, Ok(Ok(_))),
            published_pass,
            if published_pass {
                published_text_projection_running || exact_and_lexical_ready_for_graph(graph_text)
            } else {
                graph_text.is_some()
            },
        );
        // An arrival that landed during this retained pass is
        // exact/lexical work waiting for the worker. The optional
        // graph prepare parks this worker on an O(store) sealed
        // decode, tens of seconds on a cold large repository, and
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
            && !state.graph_prepare_yielded_to_arrival
            && self.pending_wake.has_pending_arrival();
        if arrival_pending_before_graph_prepare {
            state.graph_prepare_yielded_to_arrival = true;
            tracing::debug!(
                event = "code_index_graph_seat_skipped",
                reason = "arrival_pending",
                published_pass,
                "an arrival is pending; the optional graph decode yields this pass"
            );
        }
        // Stamped after the yield sample, which the worker's own
        // follow-up must not trigger, and before the guard drops, so
        // freshness never reads Fresh while the follow-up is owed.
        if *retained_noop_follow_up {
            Registry::note_worker_continuation(&self.pending_wake, &self.wake);
        }
        // A successor-only retained projection holds no pass guard of
        // its own; keeping the worker's guard through graph seat would
        // report rebuild_in_flight for work that is not query serving.
        if state.retained_text_projection.is_none() || state.retained_projection_successor_only {
            drop(reconcile_pass.take());
        }
        let prepare_graph =
            gate == GraphSeatGateV1::Prepare && !arrival_pending_before_graph_prepare;
        if prepare_graph {
            state.graph_prepare_yielded_to_arrival = false;
        }
        if gate == GraphSeatGateV1::RetainedGenerationUnavailable && !published_pass {
            // Restore the arrival so warming is not terminal, then
            // fall through to record this Noop. `continue` skipped
            // the receipt and left latest_generation_id observers
            // without event-to-ready evidence. One restore per
            // arrival: a second identical warming pass proves
            // nothing became restorable, so the arrival drains
            // instead of respinning this worker.
            self.restore_warming_arrival(state, arrival, trigger);
        }
        if let Some(reason) = gate.skip_reason() {
            tracing::debug!(
                event = "code_index_graph_seat_skipped",
                reason,
                published_pass,
                "graph seating skipped this pass; the sealed generation stays unseated"
            );
        }
        // A warming terminal source outcome with nothing seated is
        // not done: restore the arrival so the next pass can finish
        // text instead of sleeping until an unrelated hint. Failed
        // source outcomes already restore in the error arm; waking
        // those here would spin while Git is unavailable. The same
        // one-restore-per-arrival bound applies: when the follow-up
        // pass restored nothing and reconciled to the identical
        // outcome, the arrival drains rather than respinning.
        if !prepare_graph && matches!(source_result, Ok(Ok(_))) {
            self.restore_warming_arrival(state, arrival, trigger);
        }
        prepare_graph
    }

    /// Restore an arrival whose pass left nothing seated, once per arrival.
    fn restore_warming_arrival(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let text_empty = self
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        let serving_empty = self
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        if text_empty
            && serving_empty
            && arrival.wake_micros().is_some()
            && state.warming_restore_arrival != arrival.wake_micros()
        {
            state.warming_restore_arrival = arrival.wake_micros();
            Registry::restore_pending_arrival(&self.pending_wake, arrival, trigger);
            self.wake.notify_one();
        }
    }

    /// Admit a retained native graph's prepare only for explicit
    /// complete-generation demand, and never replay a retained manifest
    /// that already lost the verified head to another generation.
    fn narrow_graph_prepare_to_demand(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        prepare_graph: &mut bool,
        published_pass: bool,
        graph_already_serves: bool,
        graph_text: Option<&LatestCodeTextGenerationV1>,
    ) {
        if *prepare_graph && graph_already_serves {
            // A retained native graph serves without a full decode.
            // Explicit complete-generation demand still admits binding
            // and seating, independently of redundant activation. A
            // predecessor in the serving slot does not satisfy that
            // demand for the text owner's generation.
            let text_generation_is_unseated = graph_text.is_some_and(|text| {
                let generation_id = &text.metadata().manifest().generation_id;
                self.serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_none_or(|seat| &seat.generation().manifest().generation_id != generation_id)
            });
            *prepare_graph = text_generation_is_unseated
                && self.complete_generation_requested.load(Ordering::Acquire);
        }
        let retained_generation =
            graph_text.map(|text| text.metadata().manifest().generation_id.clone());
        if state
            .graph_head_conflict_generation
            .as_ref()
            .is_some_and(|conflicted| {
                retained_generation
                    .as_ref()
                    .is_some_and(|current| current != conflicted)
            })
        {
            state.graph_head_conflict_generation = None;
        } else if !published_pass
            && *prepare_graph
            && state.graph_head_conflict_generation.is_some()
            && retained_generation.is_some()
        {
            *prepare_graph = false;
            tracing::debug!(
                event = "code_index_graph_seat_skipped",
                reason = "graph_head_belongs_to_another_generation",
                "this retained manifest already lost the verified graph head to another \
                 generation; it is not replayed"
            );
        }
    }

    /// The reserved verified-head recovery of a retained partitioned
    /// manifest. Returns whether the recovered head serves graph reads and
    /// no complete-generation consumer asked for the replay.
    async fn recover_retained_graph_head(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        retained: LatestCodeTextGenerationV1,
        prepare_graph: &mut bool,
    ) -> bool {
        let mut retained_head_recovered_without_complete_replay = false;
        // Every outcome of this attempt schedules one successor.
        // Stamp it before the recovery await, while the pass is
        // visible, so the wait cannot be sampled as an idle slot.
        Registry::note_visible_worker_continuation(
            &self.reconcile_in_progress,
            &self.pending_wake,
            &self.wake,
        );
        state.retained_graph_head_recovery_attempted = true;
        #[cfg(any(test, feature = "test-helpers"))]
        Registry::wait_for_retained_graph_recovery_gate(
            &self.project_root,
            super::super::RetainedGraphRecoveryPauseV1::BeforeHeadRecovery,
        )
        .await;
        let generation_id = retained.metadata().manifest().generation_id.clone();
        let recovered_generation = generation_id.clone();
        let replay_scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let replay_passes = Arc::clone(&self.reconcile_in_progress);
        let replay_binding = native_runtime::spawn_blocking(move || {
            let scheduler = Registry::lock_scheduler_for_graph_step(
                &replay_scheduler,
                &shutting_down,
                &replay_passes,
            )?
            .1;
            Ok::<_, CodeIndexSchedulerErrorV1>((
                scheduler.code_graph_replay_binding(&generation_id)?,
                scheduler.names_active_publication(&generation_id)?,
            ))
        })
        .await;
        match replay_binding {
            Ok(Ok((replay_binding, retained_names_active_publication))) => {
                match self
                    .graph_activation
                    .recover_verified_head(
                        &self.project_id,
                        &self.repository_id,
                        &self.worktree_id,
                        retained,
                        replay_binding,
                        Arc::clone(&self.shutting_down),
                    )
                    .await
                {
                    Ok(true) => {
                        // Verified-head recovery is sufficient for
                        // graph-only readers, but an explicit
                        // complete-generation consumer still needs
                        // the decoded serving owner. Continue that
                        // replay in this pass: the retained text
                        // projection is joined at the end of the
                        // pass and can take minutes on a cold large
                        // store, so deferring the replay to the
                        // successor strands branch publication
                        // behind unrelated lexical work.
                        let complete_generation_requested =
                            self.complete_generation_requested.load(Ordering::Acquire);
                        *prepare_graph = complete_generation_requested;
                        retained_head_recovered_without_complete_replay =
                            !complete_generation_requested;
                        tracing::info!(
                            event = "code_index_graph_head_recovered",
                            complete_generation_requested,
                            "partitioned manifest matched the durable verified graph \
                             head; startup seated graph reads without replay"
                        );
                    }
                    Ok(false) => {}
                    Err(error)
                        if graph_head_belongs_to_another_generation(&error)
                            && retained_names_active_publication =>
                    {
                        // The durable pointer names this manifest, so the head is
                        // older and this generation's graph publication stopped
                        // before seating; resume it from its sealed segments.
                        tracing::info!(
                            event = "code_index_graph_head_recovery_resumes_publication",
                            error = %error,
                            "durable publication names the retained manifest; resume \
                             its interrupted graph publication"
                        );
                    }
                    Err(error) if graph_head_belongs_to_another_generation(&error) => {
                        // A newer generation owns the durable pointer. Falling
                        // through into a cold replay discards the in-flight successor
                        // that owns the head; do not retry this manifest.
                        *prepare_graph = false;
                        state.graph_head_conflict_generation = Some(recovered_generation);
                        state.pass_waits_for_store_release = true;
                        tracing::warn!(
                            event = "code_index_graph_head_recovery_generation_conflict",
                            error = %error,
                            "verified graph head belongs to another generation; this \
                             retained manifest is not replayed"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "code_index_graph_head_recovery_degraded",
                            error = %error,
                            "verified graph head did not match the partitioned \
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
                    "partitioned replay binding is unavailable; graph coverage stays \
                     pending while the admitted worker repairs the generation"
                );
            }
            Err(error) => {
                tracing::warn!(
                    event = "code_index_graph_head_recovery_task_failed",
                    error = %error,
                    "partitioned replay binding task failed; graph coverage stays \
                     pending while the admitted worker repairs the generation"
                );
            }
        }
        // Hold a test-installed successor gate after every reserved
        // recovery attempt, including Memory authorities that
        // abstain (`Ok(false)`) and degraded recoveries. Quiet
        // Persistent recoveries already pause here so observers can
        // see the retained text owner before the dirty successor
        // publishes; Memory remounts used to skip the gate and race
        // the rebuild past that observation window.
        #[cfg(any(test, feature = "test-helpers"))]
        if !self.complete_generation_requested.load(Ordering::Acquire) {
            Registry::wait_for_retained_graph_recovery_gate(
                &self.project_root,
                super::super::RetainedGraphRecoveryPauseV1::BeforeSuccessor,
            )
            .await;
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
        // `retained_graph_head_recovery_attempted` guard is
        // now false for every later pass, so this cannot spin
        // another retained-recovery Noop. The successor was
        // stamped before this await.
        retained_head_recovered_without_complete_replay
    }

    /// The last checks before the corpus-sized graph prepare: a recovered
    /// partitioned head is not replayed, and full replay waits for the text
    /// projection it would contend with.
    async fn confirm_graph_prepare(
        &self,
        mut prepare_graph: bool,
        published_pass: bool,
        graph_already_serves: bool,
        graph_text: Option<&LatestCodeTextGenerationV1>,
        published_text_projection_running: bool,
        published_text_opened: &mut Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> bool {
        // A recovered revision-7 verified head already serves its
        // native graph from the retained text owner, and that owner
        // is the authority every graph route reads. Preparing the
        // same generation again buys nothing but the O(store)
        // partition replay the verified-head recovery exists to
        // avoid: the decoder reloads the active generation, and the
        // seat that follows is a second copy of what already serves.
        // Restarts of a partitioned manifest therefore leave the
        // sealed slot unseated until an explicit complete-generation
        // consumer asks for it, exactly as graph-only
        // `sealed_decode_count() == 0` requires. A publication seats
        // its own product as usual, and a legacy (non-partitioned)
        // owner still takes the seat.
        if prepare_graph
            && !published_pass
            && graph_already_serves
            && graph_text.is_some_and(LatestCodeTextGenerationV1::uses_partitioned_manifest)
            && !self.complete_generation_requested.load(Ordering::Acquire)
        {
            prepare_graph = false;
            tracing::debug!(
                event = "code_index_graph_seat_skipped",
                reason = "verified_head_already_serves",
                "the recovered partitioned head already serves; the sealed generation \
                 is not replayed to seat a second copy of it"
            );
        }
        // Retained passes may race text only far enough to recover a
        // verified graph head, which reads the durable graph store
        // instead of replaying the sealed source. If recovery did not
        // satisfy this pass, wait for text before the corpus-sized
        // decode and replay below. Otherwise a failed fresh text pass
        // becomes a retained pass on its next wake and recreates the
        // same source and resident-memory contention we avoid above.
        // A publication's own projection is the exception: it already
        // holds its build reservation once `opened` fires, and the
        // swap joins it before seating.
        if prepare_graph
            && !published_text_projection_running
            && !graph_already_serves
            && !exact_and_lexical_ready_for_graph(graph_text)
        {
            prepare_graph = false;
            tracing::debug!(
                event = "code_index_graph_seat_skipped",
                reason = "text_projection_unfinished",
                published_pass,
                "full graph replay waits for exact and lexical projection"
            );
        }
        // A projection that stopped before opening its build (parked,
        // failed, or already ready) admits graph only on ready owners.
        if prepare_graph
            && let Some(opened) = published_text_opened.take()
            && opened.await.is_err()
        {
            prepare_graph = exact_and_lexical_ready_for_graph(graph_text);
            if !prepare_graph {
                tracing::debug!(
                    event = "code_index_graph_seat_skipped",
                    reason = "text_projection_unfinished",
                    published_pass,
                    "fresh graph publication waits for a text projection that did not open"
                );
            }
        }
        prepare_graph
    }

    /// Publish the sealed graph, then decode the generation the serving
    /// swap seats (unless the published head already serves graph reads
    /// and no reader demanded the whole decode).
    #[allow(clippy::too_many_arguments)]
    async fn prepare_graph_seat(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        mut outcome: Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1>,
        graph_text: Option<&LatestCodeTextGenerationV1>,
        graph_already_serves: bool,
        published_pass: bool,
        text_projection_running: bool,
        graph_waits_for_text: &mut bool,
        graph_publication_budget_spent: &mut bool,
    ) -> PreparedSeatV1 {
        // Publish the graph head from the sealed segments
        // before the serving decode below. The graph build and
        // the decoded generation are this step's two
        // corpus-sized working sets and must not be resident
        // together; the activation after the decode recovers
        // the head published here.
        //
        // An owner that already serves this generation's graph
        // needs no second build: only the decode is demanded.
        // Neither does one whose build already spent its budget.
        let sealed_graph = match graph_text
            .filter(|text| !graph_already_serves && !text.graph_publication_budget_spent())
        {
            Some(text) => self.publish_sealed_graph_before_decode(text).await,
            None => SealedGraphPublicationV1::default(),
        };
        if sealed_graph.budget_spent {
            *graph_publication_budget_spent = true;
        }
        // The head just published serves graph reads from the
        // text owner's mapped store, exactly as a restart's
        // verified-head recovery does, so the whole-generation
        // decode below runs only for a reader that demanded it.
        let graph_serves_from_text = match graph_text {
            Some(text) if sealed_graph.head_published => {
                Registry::recover_published_graph_head(
                    &self.scheduler,
                    &self.shutting_down,
                    &self.reconcile_in_progress,
                    &self.graph_activation,
                    (&self.project_id, &self.repository_id, &self.worktree_id),
                    text,
                )
                .await
            }
            _ => false,
        };
        let defer_serving_decode =
            graph_serves_from_text && !self.complete_generation_requested.load(Ordering::Acquire);
        if defer_serving_decode {
            clear_graph_resident_memory_park(&self.convergence_park);
            self.memory_retry.reset();
            Registry::release_superseded_serving_seat(
                &self.serving_generation,
                &self.serving_generation_epoch,
                &self.serving_source_witness,
                &self.serving_seats,
                &self.serving_generation_changed,
            );
            tracing::info!(
                event = "code_index_serving_decode_deferred",
                "the published graph head serves from the text owner; the \
                 whole-generation decode waits for a reader that needs it"
            );
        }
        let step = GraphPrepareStepV1 {
            graph_publish_refusal: sealed_graph.refusal,
            defer_serving_decode,
            scheduler: Arc::clone(&self.scheduler),
            graph_text: graph_text.cloned(),
            shutting_down: Arc::clone(&self.shutting_down),
            passes: Arc::clone(&self.reconcile_in_progress),
            pending_wake: Arc::clone(&self.pending_wake),
            wake: Arc::clone(&self.wake),
        };
        #[cfg(test)]
        let after_decode_gate = Registry::enter_graph_decode_gate(&self.project_root).await;
        let prepared = tracing::Instrument::instrument(
            run_reconcile_step(move || step.run()),
            tracing::trace_span!("daemon.code_index.graph_prepare"),
        )
        .await;
        #[cfg(test)]
        Registry::pass_worker_step_gate(after_decode_gate).await;
        match prepared {
            Ok(Ok((_, _, _, Some(stop)))) => {
                // Once the text owner serves the graph, the
                // generation has converged: only the reader
                // that demanded the whole decode waits for
                // memory, and freshness is not parked for it.
                let converged = graph_serves_from_text || graph_already_serves;
                // A build that waits for this pass's own text
                // projection is rescheduled at its join below.
                // That projection is also the store-lock holder
                // a publication's decode meets: its artifact
                // attachment holds the lock exclusively, and
                // the join is its release.
                *graph_waits_for_text = published_pass && text_projection_running;
                self.park_graph_prepare_stop(
                    state,
                    &stop,
                    published_pass,
                    converged,
                    *graph_waits_for_text,
                );
                Ok((outcome, None, None))
            }
            Ok(Ok((latest, replay_binding, roster_refusal_rebuild, None))) => {
                if latest.is_none() && !defer_serving_decode {
                    tracing::warn!(
                        event = "code_index_graph_prepare_no_servable_generation",
                        published_pass,
                        roster_refusal_rebuild,
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

    /// Publish the graph head from the sealed segments ahead of the
    /// serving decode.
    async fn publish_sealed_graph_before_decode(
        &self,
        text: &LatestCodeTextGenerationV1,
    ) -> SealedGraphPublicationV1 {
        let mut publication = SealedGraphPublicationV1::default();
        let generation_id = text.metadata().manifest().generation_id.clone();
        let binding_scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let binding_passes = Arc::clone(&self.reconcile_in_progress);
        // The retained seat selects a cold or changed-page
        // plan, then this authority admits that exact plan
        // before its first page is materialized.
        let admitted_binding = native_runtime::spawn_blocking(move || {
            let (_step, scheduler) = Registry::lock_scheduler_for_graph_step(
                &binding_scheduler,
                &shutting_down,
                &binding_passes,
            )?;
            let binding = scheduler.code_graph_replay_binding(&generation_id)?;
            let admission = scheduler
                .active_generation_decoder()
                .as_ref()
                .map(DaemonCodeIndexPublicationStoreV1::sealed_graph_build_admission);
            Ok::<_, CodeIndexSchedulerErrorV1>((binding, admission))
        })
        .await;
        match admitted_binding {
            Ok(Ok((replay_binding, admission))) => {
                CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::PublishingGraph);
                let published = self
                    .graph_activation
                    .publish_sealed_graph(
                        &self.project_id,
                        &self.repository_id,
                        &self.worktree_id,
                        text,
                        replay_binding,
                        admission,
                        Arc::clone(&self.shutting_down),
                    )
                    .await;
                CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
                match published {
                    Ok(published) => publication.head_published = published,
                    Err(error) if error.is_resident_memory_graph_refusal() => {
                        publication.refusal = Some(error.to_string());
                    }
                    Err(error) if error.is_graph_publication_deadline() => {
                        publication.budget_spent = true;
                        tracing::warn!(
                            event = "code_index_graph_publication_budget_spent",
                            error = %error,
                            "sealed graph publication spent its background \
                             budget; the generation serves exact and lexical \
                             with a typed graph refusal and is not replayed"
                        );
                    }
                    Err(error) => tracing::warn!(
                        event = "code_index_graph_publish_before_decode_failed",
                        error = %error,
                        "sealed graph publication failed before the serving \
                         decode; activation retries it after the decode"
                    ),
                }
            }
            Ok(Err(error)) => tracing::warn!(
                event = "code_index_graph_publish_binding_unavailable",
                error = %error,
                "sealed replay binding is unavailable; activation publishes \
                 the graph after the serving decode"
            ),
            Err(error) => tracing::warn!(
                event = "code_index_graph_publish_binding_task_failed",
                error = %error,
                "sealed replay binding task failed; activation publishes the \
                 graph after the serving decode"
            ),
        }
        publication
    }

    /// Park or retry a graph prepare that decoded nothing.
    fn park_graph_prepare_stop(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        stop: &GraphPrepareStopV1,
        published_pass: bool,
        converged: bool,
        graph_waits_for_text: bool,
    ) {
        match stop {
            GraphPrepareStopV1::ResidentMemory(detail) => {
                if !graph_waits_for_text {
                    // The outgoing graph held for reads
                    // is the headroom this decode needs.
                    if let Some(text) = self
                        .text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                    {
                        text.release_graph_predecessor();
                    }
                    if !converged {
                        park_convergence(
                            &self.convergence_park,
                            detail.clone(),
                            CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
                            Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                            true,
                        );
                    }
                    self.memory_retry.wait();
                }
                tracing::warn!(
                    event = "code_index_graph_prepare_decode_refused",
                    published_pass,
                    converged,
                    graph_waits_for_text,
                    detail = %detail,
                    "the sealed generation waits to decode until memory \
                     is given back; text serving is unaffected"
                );
            }
            GraphPrepareStopV1::StoreBusy(detail) => {
                if !graph_waits_for_text {
                    state.pass_waits_for_store_release = true;
                }
                tracing::warn!(
                    event = "code_index_graph_prepare_store_busy",
                    published_pass,
                    converged,
                    graph_waits_for_text,
                    detail = %detail,
                    "the sealed generation waits to decode until the \
                     code-generation store lock is released"
                );
            }
            GraphPrepareStopV1::DecodeFailed(detail) => {
                if !graph_waits_for_text && !converged {
                    park_convergence(
                        &self.convergence_park,
                        detail.clone(),
                        CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
                        Some(CodeIndexBuildBlockedReasonV1::ArtifactStoreUnavailable),
                        true,
                    );
                }
            }
        }
    }

    /// Whether the prepared generation replaces the one serving.
    fn replaces_serving_generation(&self, result: &PreparedSeatV1) -> bool {
        match result {
            Ok((Ok(CodeIndexReconcileOutcomeV1::Noop(_)), Some(latest), Some(_))) => {
                let serving = self
                    .serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                serving.as_ref().is_none_or(|serving| {
                    serving.generation().manifest().generation_id
                        != latest.generation().manifest().generation_id
                })
            }
            Ok((Ok(_), Some(_), Some(_))) => true,
            _ => false,
        }
    }

    /// Activation is decided independently of the seat: every arm
    /// refuses only the native graph call, and the serving
    /// swap still installs the prepared generation.
    fn activates_graph(
        state: &WorktreeWorkerLoopStateV1,
        result: &PreparedSeatV1,
        graph_publication_budget_spent: bool,
        graph_already_serves: bool,
        replace_serving_generation: bool,
    ) -> bool {
        match result {
            Ok((Ok(_), Some(latest), Some(_)))
                if graph_publication_budget_spent || latest.graph_publication_budget_spent() =>
            {
                false
            }
            Ok((Ok(_), Some(latest), Some(_))) => GraphActivationGateV1::decide(
                graph_already_serves || latest.code_graph_serving_readiness().is_activated(),
                replace_serving_generation,
                latest.graph_activation_is_pending(),
                state.graph_seat_attempted.as_ref()
                    == Some(&latest.generation().manifest().generation_id),
            )
            .activates(),
            _ => false,
        }
    }

    /// Activate the prepared generation's native graph, arming the seat
    /// backoff on a retryable failure.
    async fn activate_graph(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        latest: &LatestCompleteCodeIndexV1,
        replay_binding: &CodeGraphReplayBindingV1,
    ) {
        state.graph_seat_attempted = Some(latest.generation().manifest().generation_id.clone());
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::PublishingGraph);
        // The serving generation's graph store is the catalog a
        // layered successor carries from.
        let predecessor = self
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|serving| serving.interactive_graph_store().ok());
        let activation = self
            .graph_activation
            .activate(
                &self.project_id,
                &self.repository_id,
                &self.worktree_id,
                latest.clone(),
                predecessor,
                replay_binding.clone(),
                Arc::clone(&self.shutting_down),
            )
            .await;
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
        match activation {
            Ok(_background_catalog_warm) => {
                state.next_seat_attempt_at = None;
                state.seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                state.last_seat_conflict = None;
                clear_graph_resident_memory_park(&self.convergence_park);
                self.memory_retry.reset();
            }
            Err(error) if error.is_graph_activation_refusal() => {
                state.next_seat_attempt_at = None;
                state.seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                state.last_seat_conflict = None;
                // A configuration refusal never re-attempts, so an
                // unparked refusal read as an indefinite `indexing`.
                // A memory refusal parks typed until memory is
                // given back.
                if error.is_resident_memory_graph_refusal() {
                    park_convergence(
                        &self.convergence_park,
                        error.to_string(),
                        CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
                        Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                        true,
                    );
                    self.memory_retry.wait();
                }
                tracing::warn!(
                    event = "code_index_graph_activation_refused",
                    error = %error,
                    "code-index generation remains text-serving without native graph"
                );
            }
            Err(error) => {
                let seat_generation_id = latest.generation().manifest().generation_id.clone();
                let repeated_conflict = is_repeated_conflict_verdict(
                    &error,
                    &seat_generation_id,
                    state.last_seat_conflict.as_ref(),
                );
                // The generation just sealed is complete; a retryable
                // activation failure arms the same seat backoff so the
                // next passes retry this artifact instead of resealing.
                // A conflict verdict identical to the previous
                // attempt's for this same generation is deterministic
                // and falls through to the terminal arm instead.
                //
                // The prepared text candidate stays. Wiping it to
                // `Ok((Err, None, None))` skipped the serving swap,
                // so search kept the predecessor while graph backoff
                // ran.
                if error.is_retryable_activation() && !repeated_conflict {
                    state.last_seat_conflict = error
                        .activation_conflict_context()
                        .map(|context| (seat_generation_id, context.clone()));
                    state.next_seat_attempt_at = Some(Instant::now() + state.seat_retry_backoff);
                    let retry_delay = state.seat_retry_backoff;
                    Registry::wake_after(retry_delay, &self.wake);
                    tracing::warn!(
                        event = "code_index_graph_activation_retry_scheduled",
                        retry_delay_micros = retry_delay.as_micros() as u64,
                        error = %error,
                        "graph activation failed retryably; the sealed generation \
                         still seats and the next pass retries native graph"
                    );
                    state.seat_retry_backoff = state
                        .seat_retry_backoff
                        .saturating_mul(2)
                        .min(ACTIVATION_RETRY_BACKOFF_CEILING);
                    // The scheduled retry is the seat attempt, so it
                    // must not be turned away as already attempted.
                    state.graph_seat_attempted = None;
                    // The prepared candidate stays. Rewriting the
                    // pass result to `Ok((Err, None, None))` here
                    // failed the serving swap's own guard, so an
                    // activation that kept failing retryably never
                    // let any pass seat: search held its predecessor
                    // while a complete generation sat on disk. The
                    // terminal arm below already keeps the seat and
                    // marks graph unavailable; a retry is a weaker
                    // verdict than terminal and must not seat less.
                } else {
                    state.next_seat_attempt_at = None;
                    state.seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                    state.last_seat_conflict = None;
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

    /// Join the publication's projection. An arrival during the projection
    /// seals its successor now instead of waiting for this projection and
    /// its seat.
    async fn join_published_text_projection(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        projection: &TextProjectionTaskV1,
        graph_activation_enabled: bool,
        graph_text: Option<&LatestCodeTextGenerationV1>,
    ) -> PublishedTextProjectionOutcomeV1 {
        // The seal claims the arrival, and the next pass answers it
        // with the sealed outcome; an arrival after the seal stays
        // pending for its own pass. Admission is only tried: this pass holds
        // the publication gate, and waiting on admission under it
        // inverts the gate order.
        let mut preserve_worker_wake = false;
        let joined = loop {
            if !self.shutting_down.load(Ordering::Acquire)
                && state.sealed_successor.is_none()
                && self.pending_wake.has_pending_arrival()
                && let Some(metadata) = graph_text.map(|text| text.metadata().clone())
                && let Ok(admission) =
                    Arc::clone(&self.background_reconcile_admission).try_acquire_owned()
            {
                preserve_worker_wake = true;
                self.seal_successor_during_projection(
                    state,
                    metadata,
                    admission,
                    graph_activation_enabled,
                )
                .await;
            }
            tokio::select! {
                outcome = join_native(projection) => break outcome,
                () = self.wake.notified() => {
                    preserve_worker_wake = true;
                }
            }
        };
        if preserve_worker_wake {
            self.wake.notify_one();
        }
        match joined {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(text) = graph_text {
                    text.mark_text_serving_failed();
                }
                park_convergence(
                    &self.convergence_park,
                    format!("code text projection task failed abnormally: {error}"),
                    CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                    None,
                    false,
                );
                tracing::warn!(
                    event = "code_index_text_projection_task_failed",
                    error = %error,
                    "published text projection task failed before graph seating"
                );
                PublishedTextProjectionOutcomeV1::Unfinished
            }
        }
    }

    /// Claim the pending arrival and seal its successor under the tried
    /// admission. A seal that publishes nothing returns the arrival.
    async fn seal_successor_during_projection(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        metadata: VerifiedSealedTextGenerationMetadataV1,
        admission: tokio::sync::OwnedSemaphorePermit,
        graph_activation_enabled: bool,
    ) {
        let (arrival, trigger) = Registry::take_pending_arrival(
            &self.pending_wake,
            &self.wake,
            CodeIndexCadenceTriggerV1::Mount,
        );
        let scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let retained_text_only = !graph_activation_enabled;
        let sealed = native_runtime::spawn_blocking(move || {
            let mut scheduler =
                Registry::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down)?;
            match scheduler
                .reconcile_retained_text_generation_with(&metadata, retained_text_only)?
            {
                Some(outcome) => Ok(Some(outcome)),
                None if retained_text_only => scheduler.reconcile_now().map(Some),
                None => scheduler.activate_or_reconcile().map(Some),
            }
        })
        .await;
        drop(admission);
        match sealed {
            Ok(Ok(Some(outcome @ CodeIndexReconcileOutcomeV1::Published(_)))) => {
                tracing::info!(
                    event = "code_index_successor_sealed_during_text_projection",
                    "an arrival during text projection sealed its successor"
                );
                state.sealed_successor = Some((outcome, arrival, trigger));
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::debug!(
                event = "code_index_successor_seal_during_text_projection_deferred",
                error = %error,
                "the successor seal waits for the next pass"
            ),
            Err(error) => tracing::warn!(
                event = "code_index_successor_seal_during_text_projection_task_failed",
                error = %error,
                "the successor seal task failed; the next pass reconciles"
            ),
        }
        if state.sealed_successor.is_none() {
            Registry::restore_pending_arrival(&self.pending_wake, arrival, trigger);
        }
    }

    /// Act on the publication projection's outcome before the serving swap:
    /// a finished owner re-proves its source, and an unfinished, parked, or
    /// store-blocked one withholds the prepared seat.
    #[allow(clippy::too_many_arguments)]
    async fn settle_published_text_projection(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        outcome: PublishedTextProjectionOutcomeV1,
        result: &mut PreparedSeatV1,
        graph_text: Option<&LatestCodeTextGenerationV1>,
        published_pass: bool,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
    ) -> ControlFlow<PassEndV1> {
        // Text projection work can still be `Unfinished` after the
        // exact and lexical owners are ready. That must not clear
        // the prepared generation the way a missing owner does.
        let owners_ready = exact_and_lexical_ready_for_graph(graph_text);
        let outcome = match outcome {
            PublishedTextProjectionOutcomeV1::Unfinished
                if !text_projection_unfinished_withholds_seat(owners_ready) =>
            {
                PublishedTextProjectionOutcomeV1::Finished
            }
            other => other,
        };
        if matches!(outcome, PublishedTextProjectionOutcomeV1::Finished) {
            self.memory_retry.reset();
        }
        match outcome {
            PublishedTextProjectionOutcomeV1::Finished => {
                // The seat needs only the ready owners. Text work
                // left in the slot is retained-owner work on the
                // next pass (no pass guard once owners already
                // serve); schedule that pass here, since there is
                // no periodic cadence timer.
                if graph_text.is_some_and(LatestCodeTextGenerationV1::text_projection_needs_work) {
                    // Already stamped before optional graph. Re-enter
                    // the pass so a reader that cleared the slot
                    // during graph still cannot sample the stamp.
                    Registry::note_visible_worker_continuation(
                        &self.reconcile_in_progress,
                        &self.pending_wake,
                        &self.wake,
                    );
                }
                // Source evidence (a hint, an observed change, or a
                // Git metadata move) can land during a large text
                // projection, after the proof established before
                // publication. The serving swap must bind to source
                // truth observed after that work, otherwise an
                // exact current generation seats without a witness
                // and every readiness read schedules another
                // identical Noop.
                //
                // A plain write moves neither the source epoch nor
                // Git metadata, so an unmoved proof does not mean
                // an unchanged tree. Whatever the proof says, the
                // sealed digests are swept again here: a write that
                // landed during the projection is observed now,
                // leaves the seat stale, and wakes its successor.
                //
                // A successor sealed during the projection already
                // proved the move; re-reconciling this generation
                // would race that seal.
                if state.sealed_successor.is_none()
                    && let Some(text) = graph_text
                {
                    self.verify_source_after_projection(state, text, arrival, trigger)
                        .await;
                }
                // The ready text owner now serves this generation,
                // with or without a decoded seat after it.
                self.serving_generation_changed.send_replace(());
            }
            PublishedTextProjectionOutcomeV1::Shutdown => {
                tracing::info!(
                    event = "code_index_worker_shutdown_observed",
                    phase = "text_projection_before_seat",
                    "code-index worker observed shutdown and stopped its pass"
                );
                return Self::exit(state).await;
            }
            PublishedTextProjectionOutcomeV1::WaitingForMemory => {
                withhold_prepared_seat(result);
                self.memory_retry.wait();
            }
            PublishedTextProjectionOutcomeV1::WaitingForStore => {
                withhold_prepared_seat(result);
                state.pass_waits_for_store_release = true;
            }
            PublishedTextProjectionOutcomeV1::Unfinished => {
                withhold_prepared_seat(result);
                tracing::debug!(
                    event = "code_index_graph_seat_skipped",
                    reason = "published_text_owner_unfinished",
                    published_pass,
                    "the publication's text owner did not finish its projection; \
                     the sealed generation stays unseated until it does"
                );
                Registry::note_visible_worker_continuation(
                    &self.reconcile_in_progress,
                    &self.pending_wake,
                    &self.wake,
                );
            }
        }
        // Keep the pass lifetime around the post-projection source
        // proof so concurrent reads attribute their single wake as
        // a follow-up to this owner. The swap below takes its own
        // nested guard and publishes the witness before either
        // lifetime becomes idle.
        ControlFlow::Continue(())
    }

    /// Re-prove the source a finished projection's generation was sealed
    /// from. A republished successor becomes the next pass's sealed
    /// outcome.
    async fn verify_source_after_projection(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        text: &LatestCodeTextGenerationV1,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let proof_unmoved = self.source_freshness.serves_verified_source(
            &text.metadata().snapshot().content_identity,
            &self.project_root,
            &self.shutting_down,
        );
        let scheduler = Arc::clone(&self.scheduler);
        let shutting_down = Arc::clone(&self.shutting_down);
        let pending_wake = Arc::clone(&self.pending_wake);
        let wake = Arc::clone(&self.wake);
        let source_freshness = self.source_freshness.clone();
        let metadata = text.metadata().clone();
        let source_current = native_runtime::spawn_blocking(move || {
            let mut scheduler =
                Registry::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down)?;
            if !proof_unmoved {
                match scheduler.reconcile_retained_text_generation_with(&metadata, false)? {
                    Some(CodeIndexReconcileOutcomeV1::Noop(_)) => {
                        return Ok((true, None));
                    }
                    Some(outcome @ CodeIndexReconcileOutcomeV1::Published(_)) => {
                        return Ok((false, Some(outcome)));
                    }
                    None => {}
                }
            }
            // A pending hint or observed change already
            // carries its own wake; sweeping on top of
            // it would turn that targeted pass into an
            // overflow rescan.
            if source_freshness.source_change_pending() {
                return Ok((false, None));
            }
            let moved = scheduler.request_fresh_now_background();
            if moved {
                Registry::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Overflow);
            }
            Ok::<_, CodeIndexSchedulerErrorV1>((!moved, None))
        })
        .await;
        match source_current {
            Ok(Ok((_, Some(outcome)))) => {
                // Verification can republish a successor whose
                // first seal lost the store lock. Its serving
                // owners still need the normal publication pass;
                // reducing Published to a bool loses that work.
                state.sealed_successor = Some((outcome, arrival, trigger));
                Registry::note_worker_continuation(&self.pending_wake, &self.wake);
            }
            Ok(Ok((true, None))) => {}
            Ok(Ok((false, None))) => tracing::info!(
                event = "code_index_post_projection_source_unverified",
                "source moved while text projection ran; the completed generation may only take a stale seat"
            ),
            Ok(Err(error)) => tracing::warn!(
                event = "code_index_post_projection_source_verification_failed",
                error = %error,
                "source verification after text projection failed; the completed generation may only take a stale seat"
            ),
            Err(error) => tracing::warn!(
                event = "code_index_post_projection_source_verification_task_failed",
                error = %error,
                "source verification after text projection did not complete; the completed generation may only take a stale seat"
            ),
        }
    }

    /// Seat the prepared generation. Returns the failure that refused it.
    async fn swap_serving_generation(
        &self,
        latest: &LatestCompleteCodeIndexV1,
        replace_serving_generation: bool,
    ) -> Option<CodeIndexSchedulerErrorV1> {
        let text_latest = latest.clone();
        let step = ServingSwapStepV1 {
            scheduler: Arc::clone(&self.scheduler),
            serving_generation: Arc::clone(&self.serving_generation),
            serving_generation_epoch: Arc::clone(&self.serving_generation_epoch),
            serving_source_witness: Arc::clone(&self.serving_source_witness),
            text_generation: Arc::clone(&self.text_generation),
            serving_seats: Arc::clone(&self.serving_seats),
            serving_generation_changed: self.serving_generation_changed.clone(),
            latest: latest.clone(),
            shutting_down: Arc::clone(&self.shutting_down),
            passes: Arc::clone(&self.reconcile_in_progress),
            replace_serving_generation,
        };
        let serving_swap = tracing::Instrument::instrument(
            native_runtime::spawn_blocking(move || step.run()),
            tracing::trace_span!("daemon.code_index.serving_swap"),
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
                    // Graph seating is the last strict-readiness
                    // boundary after text publication, so it is
                    // `info` like `code_index_generation_published`:
                    // a dogfood or operator log must be able to
                    // tell "text current, graph pending" from
                    // "graph seated" without a debug filter.
                    ServingSwapOutcomeV1::Seated => tracing::info!(
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
                // Text work a seated owner still owes is this
                // worker's slice: the worker has no cadence timer,
                // so stamp what the two sibling publication sites
                // above already stamp.
                if text_latest.text_projection_needs_work() {
                    Registry::note_visible_worker_continuation(
                        &self.reconcile_in_progress,
                        &self.pending_wake,
                        &self.wake,
                    );
                }
                None
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    event = "code_index_serving_swap_failed",
                    error = %error,
                    "the serving swap refused the reconciled generation"
                );
                Some(error)
            }
            Err(error) => {
                tracing::warn!(
                    event = "code_index_serving_swap_task_failed",
                    error = %error,
                    "the serving-swap task failed; the sealed generation stays unseated"
                );
                Some(CodeIndexSchedulerErrorV1::Identity(format!(
                    "serving-swap task failed: {error}"
                )))
            }
        }
    }

    /// Close the pass's source lifecycle: release its guard, record its
    /// receipt, and account a failure against the bounded retry states.
    #[allow(clippy::too_many_arguments)]
    fn settle_pass(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        result: &PreparedSeatV1,
        reconcile_pass: &mut Option<ReconcilePassGuard>,
        retained_work_waiting_for_source: bool,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
        started_micros: i64,
        pass_control_epoch: u64,
    ) {
        // The source proof and serving witness are now published as
        // one lifecycle. Optional receipts do not keep source
        // verification in flight. A retained-work continuation this
        // pass already knows about is stamped first, so the drop is
        // not an empty slot.
        if retained_work_waiting_for_source
            && matches!(result, Ok((Ok(CodeIndexReconcileOutcomeV1::Noop(_)), _, _)))
            && self
                .text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            && self
                .source_freshness
                .ready_without_stat(&self.project_root, &self.shutting_down)
        {
            Registry::note_visible_worker_continuation(
                &self.reconcile_in_progress,
                &self.pending_wake,
                &self.wake,
            );
        }
        drop(reconcile_pass.take());
        self.byte_pool.release_dead_entries();
        collect_installed_worker_heaps_when_idle();
        let refused_for_memory = matches!(
            result,
            Ok((Err(error), _, _)) if error.is_resident_memory_refusal()
        );
        let waited_for_memory = self.residency.refresh_waits_for_memory();
        self.residency
            .set_refresh_waits_for_memory(refused_for_memory);
        if let Ok((Err(error), _, _)) = result
            && refused_for_memory
        {
            park_convergence(
                &self.convergence_park,
                error.to_string(),
                CONVERGENCE_PARK_REFRESH_RESIDENT_MEMORY_REMEDIATION_V1,
                Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                true,
            );
            self.residency
                .yield_serving_graph_to_refresh(&self.resident_owners);
        } else if waited_for_memory {
            clear_graph_resident_memory_park(&self.convergence_park);
        }
        if let Ok((Ok(outcome), _, _)) = result {
            // A pass that ran to a terminal outcome proves neither the
            // panicking input nor the capacity contention is still
            // reproducing, so both bounded retry states restart.
            state.panic_guard.record_progress();
            state.capacity_retry.record_progress();
            let _service_micros = Registry::record_reconcile_receipt(
                &self.cadence_telemetry,
                self.project_root.clone(),
                arrival,
                trigger,
                started_micros,
                outcome,
            );
            // An unchanged-source reconcile can restore admission for
            // retained text authority without publishing or replacing
            // a decoded seat. Partitioned verified-head recovery
            // intentionally leaves that seat empty, so the canonical
            // text owner and current source proof are the wake
            // authority. Readers still validate scope and freshness.
            let retained_text_serves = matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_))
                && self
                    .text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some();
            if retained_text_serves
                && self
                    .source_freshness
                    .ready_without_stat(&self.project_root, &self.shutting_down)
            {
                // The pass bound the seat's source proof under the
                // scheduler lock; announce the change now that the
                // proof is public.
                self.serving_generation_changed.send_replace(());
                // The retained-work continuation was stamped before
                // this pass dropped `reconcile_in_progress`.
            }
        } else {
            self.record_failed_pass(
                state,
                result,
                refused_for_memory,
                waited_for_memory,
                trigger,
                pass_control_epoch,
            );
            if !publication_authority_is_terminal(&self.convergence_park) {
                // Restore arrival so the next pass measures this wake's full queue wait.
                Registry::restore_pending_arrival(&self.pending_wake, arrival, trigger);
            }
        }
    }

    /// Surface bounded non-terminal failure without new project-path data.
    fn record_failed_pass(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        result: &PreparedSeatV1,
        refused_for_memory: bool,
        waited_for_memory: bool,
        trigger: CodeIndexCadenceTriggerV1,
        pass_control_epoch: u64,
    ) {
        match result {
            Ok((Err(error), _, _)) if error.reconcile_interruption().is_some() => {
                // An interrupted pass ran to a typed stop, not a
                // failure: shutdown, or a newer source observation
                // advanced the cancellation epoch. Every epoch
                // advance is paired with a wake, so the restored
                // arrival below is picked up by the pass that
                // observation already scheduled. Attribute the
                // origin instead of reporting the served
                // generation stale.
                state.panic_guard.record_progress();
                state.capacity_retry.record_progress();
                let origin = if self.shutting_down.load(Ordering::Acquire) {
                    "shutdown"
                } else {
                    match error.reconcile_interruption() {
                        Some(CodeIndexInterruptionV1::DeadlineExceeded) => "deadline",
                        _ => "superseded_by_source_observation",
                    }
                };
                tracing::info!(
                    event = "code_index_reconcile_interrupted",
                    path = "background_worker",
                    origin,
                    trigger = trigger.label(),
                    "code-index background reconcile was interrupted; the pending wake re-runs it"
                );
            }
            Ok((Err(error), _, _)) => {
                // The pass completed; whatever refused it was not an
                // unwind, so panic accounting restarts.
                state.panic_guard.record_progress();
                self.record_refused_pass(
                    state,
                    error,
                    refused_for_memory,
                    waited_for_memory,
                    trigger,
                    pass_control_epoch,
                );
            }
            Err(error) if error.is_panic() => {
                // Arbitrary user source runs through the indexing
                // pool, so an unwind here is malformed input that
                // reproduces byte-for-byte on every later pass.
                // Bound it instead of re-dispatching the identical
                // unit on every wake.
                state.capacity_retry.record_progress();
                let decision = state
                    .panic_guard
                    .record_panic(Instant::now(), self.control_epoch.load(Ordering::Acquire));
                match decision {
                    ReconcilePanicDecisionV1::RetryAfter(delay) => {
                        tracing::warn!(
                            event = "code_index_reconcile_panicked",
                            path = "background_worker",
                            consecutive_panics = state.panic_guard.consecutive_panics(),
                            error = %error,
                            "code-index background reconcile panicked; retrying the same input with backoff"
                        );
                        Registry::wake_after(delay, &self.wake);
                    }
                    ReconcilePanicDecisionV1::Quarantine => tracing::warn!(
                        event = "code_index_reconcile_quarantined",
                        path = "background_worker",
                        consecutive_panics = state.panic_guard.consecutive_panics(),
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
    }

    /// A pass that completed with a refusal: reset a corrupt publication,
    /// wait for the store lock or memory, retry a capacity refusal, or park
    /// one that reproduces on unchanged input.
    fn record_refused_pass(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        error: &CodeIndexSchedulerErrorV1,
        refused_for_memory: bool,
        waited_for_memory: bool,
        trigger: CodeIndexCadenceTriggerV1,
        pass_control_epoch: u64,
    ) {
        let publication_corruption = error.is_publication_authority_corruption();
        let mut store_lock_held = error.is_store_lock_contended();
        let transient_capacity = !store_lock_held && error.is_transient_capacity_failure();
        // A corrupt derived publication is deleted and
        // rebuilt from source, once per mount. A store
        // that is corrupt again after its own rebuild, or
        // that cannot be deleted, is the terminal park;
        // a store another owner holds is retried when it
        // is released. `None` here means the failure was
        // not corruption or the reset is being retried.
        let publication_reset = if publication_corruption {
            match Registry::reset_corrupt_publication_authority(
                &self.scheduler,
                &mut state.publication_authority_reset_attempted,
                error,
            ) {
                PublicationAuthorityResetV1::Rebuilding => {
                    clear_convergence_park(&self.convergence_park);
                    self.serving_generation_changed.send_replace(());
                    self.wake.notify_one();
                    None
                }
                PublicationAuthorityResetV1::StoreBusy => {
                    store_lock_held = true;
                    None
                }
                PublicationAuthorityResetV1::Terminal {
                    reason,
                    remediation,
                } => Some((reason, remediation)),
            }
        } else if refused_for_memory {
            if !waited_for_memory {
                tracing::warn!(
                    event = "code_index_refresh_waiting_for_memory",
                    path = "background_worker",
                    trigger = trigger.label(),
                    error = %error,
                    "code-index refresh refused for resident memory; the served generation stays stale until memory is given back"
                );
            }
            None
        } else {
            tracing::warn!(
                event = "code_index_reconcile_failed",
                path = "background_worker",
                transient_capacity,
                store_lock_held,
                trigger = trigger.label(),
                error = %error,
                "code-index background reconcile failed; the served generation stays stale"
            );
            None
        };
        if let Some((reason, remediation)) = publication_reset {
            state.capacity_retry.record_progress();
            park_convergence(
                &self.convergence_park,
                reason,
                remediation,
                Some(CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt),
                false,
            );
            self.build_progress
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .block_current(CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt);
            // Mid-wait branch publication rechecks the park on
            // serving-generation notifications. The next
            // wake reads that same slot; no local flag.
            self.serving_generation_changed.send_replace(());
        } else if store_lock_held {
            state.capacity_retry.record_progress();
            state.pass_waits_for_store_release = true;
        } else if refused_for_memory {
            // The refusal registered with the resident-memory
            // authority; the headroom wake is its retry.
            state.capacity_retry.record_progress();
        } else if transient_capacity {
            // A graph operation budget another holder
            // releases without waking this worktree gets a
            // bounded retry. Permanent refusals never reach
            // here: retrying those forever is the failure
            // this loop already had.
            if !Registry::arm_capacity_retry(&mut state.capacity_retry, &self.wake) {
                tracing::warn!(
                    event = "code_index_reconcile_capacity_retry_exhausted",
                    path = "background_worker",
                    consecutive = state.capacity_retry.consecutive(),
                    "code-index reconcile stopped retrying a capacity refusal; the next hint retries"
                );
            }
        } else if error.reproduces_on_unchanged_input() {
            // The restored arrival alone read as an
            // indefinite `indexing` while every wake
            // rebuilt the whole worktree into the same
            // refusal. Park it typed and hold passes
            // until the input changes; a daemon restart
            // remounts with an empty park and retries.
            state.capacity_retry.record_progress();
            state
                .panic_guard
                .quarantine_unchanged_input(pass_control_epoch);
            park_convergence(
                &self.convergence_park,
                error.to_string(),
                CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
                None,
                false,
            );
        } else {
            state.capacity_retry.record_progress();
        }
    }

    /// The retained owner's projection is joined after the seat, not
    /// before it: the graph this pass recovered already serves, and
    /// an unfinished projection withholds only exact and lexical
    /// serving. `reconcile_in_progress` stays truthful until here so
    /// admission does not misread in-flight text work as
    /// unavailability.
    async fn join_retained_text_projection(
        &mut self,
        state: &mut WorktreeWorkerLoopStateV1,
        reconcile_pass: &mut Option<ReconcilePassGuard>,
        retained_head_recovered_without_complete_replay: bool,
        retained_text: Option<&LatestCodeTextGenerationV1>,
    ) -> ControlFlow<PassEndV1> {
        let Some(projection) = state.retained_text_projection.as_ref() else {
            return ControlFlow::Continue(());
        };
        let projection_result = if retained_head_recovered_without_complete_replay {
            let complete_generation_requested_changed =
                &mut self.complete_generation_requested_changed;
            let wake = &self.wake;
            let mut preserve_worker_wake = false;
            let result = loop {
                tokio::select! {
                    outcome = join_native(projection) => break Some(outcome),
                    () = async {
                        match complete_generation_requested_changed
                            .wait_for(|requested| *requested)
                            .await
                        {
                            Ok(_) | Err(_) => {}
                        }
                    } => {
                        // Recovery already satisfied graph-only reads, but a complete
                        // consumer arrived after that decision while text projection
                        // was still running. Retain the projection handle across the
                        // successor pass so it remains uniquely owned and joined.
                        break None;
                    }
                    () = wake.notified() => {
                        if self.shutting_down.load(Ordering::Acquire) {
                            break Some(join_native(projection).await);
                        }
                        // The pending arrival remains authoritative while this pass
                        // owns the projection. Preserve one permit after the join;
                        // unlike complete demand, an unrelated wake cannot end it.
                        preserve_worker_wake = true;
                    }
                }
            };
            if preserve_worker_wake {
                wake.notify_one();
            }
            result
        } else {
            Some(join_native(projection).await)
        };
        let Some(projection_result) = projection_result else {
            drop(reconcile_pass.take());
            return ControlFlow::Break(PassEndV1::NextPass);
        };
        // The task completed in the branch above. Remove that
        // completed handle before processing its outcome so a
        // later pass may start a new owner only when needed.
        state.retained_text_projection.take();
        let outcome = match projection_result {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(text) = retained_text {
                    text.mark_text_serving_failed();
                }
                park_convergence(
                    &self.convergence_park,
                    format!("code text projection task failed abnormally: {error}"),
                    CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                    None,
                    false,
                );
                tracing::warn!(
                    event = "code_index_text_projection_task_failed",
                    error = %error,
                    "retained text projection task failed"
                );
                PublishedTextProjectionOutcomeV1::Unfinished
            }
        };
        if matches!(outcome, PublishedTextProjectionOutcomeV1::Finished) {
            self.memory_retry.reset();
            self.serving_generation_changed.send_replace(());
        }
        match outcome {
            PublishedTextProjectionOutcomeV1::Finished
                if !retained_head_recovered_without_complete_replay
                    && !state.retained_projection_successor_only =>
            {
                // Graph head recovery may have abstained while the
                // retained text task was still running. Give the
                // now-ready owner one bounded successor pass so a
                // full replay can proceed without overlapping it.
                // Work behind owners that already served changed
                // no owner the seat reads, so it owes no such pass.
                Registry::note_worker_continuation(&self.pending_wake, &self.wake);
            }
            PublishedTextProjectionOutcomeV1::Finished => {
                let installed_owner_still_needs_work = self
                    .text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(LatestCodeTextGenerationV1::text_projection_needs_work);
                if installed_owner_still_needs_work {
                    Registry::note_worker_continuation(&self.pending_wake, &self.wake);
                }
            }
            PublishedTextProjectionOutcomeV1::Shutdown => {
                tracing::info!(
                    event = "code_index_worker_shutdown_observed",
                    phase = "retained_text_projection",
                    "code-index worker observed shutdown and stopped its pass"
                );
                return Self::exit(state).await;
            }
            PublishedTextProjectionOutcomeV1::Unfinished => {
                // The owner carries the typed state; a later pass
                // re-checks it. Without this wake a projection that
                // stopped short would sleep until an unrelated
                // arrival, exactly as the inline slice's own
                // follow-up notify prevented.
                Registry::note_worker_continuation(&self.pending_wake, &self.wake);
            }
            PublishedTextProjectionOutcomeV1::WaitingForStore => {
                state.pass_waits_for_store_release = true;
            }
            PublishedTextProjectionOutcomeV1::WaitingForMemory => {
                self.memory_retry.wait();
            }
        }
        // The continuation is already in the slot. Dropping here
        // is the first moment this pass looks idle.
        drop(reconcile_pass.take());
        ControlFlow::Continue(())
    }

    /// Optional attribution follows every already-decoded serving
    /// seat; later reads need not manufacture a source-verification
    /// wake to prepare it. A native-only head stays decode-free.
    /// This retained worker joins preparation even on retirement.
    async fn prepare_test_attribution(
        &self,
        state: &mut WorktreeWorkerLoopStateV1,
        shutdown_observed: &mut tokio::sync::watch::Receiver<()>,
        pass_control_epoch: u64,
    ) -> ControlFlow<PassEndV1> {
        let candidate = match self.serving_generation.read() {
            Ok(seated) => seated.as_ref().map(|latest| {
                (
                    Arc::clone(&latest.generation),
                    AttributionPreparationControlV1 {
                        generation: DaemonCodeIndexControlV1::new(
                            Arc::clone(&self.serving_generation_epoch),
                            Arc::clone(&self.shutting_down),
                        ),
                        source_epoch: Arc::clone(&self.control_epoch),
                        expected_source_epoch: pass_control_epoch,
                    },
                )
            }),
            Err(error) => {
                tracing::error!(error = %error, "attribution serving seat is poisoned");
                None
            }
        };
        let Some((generation, control)) = candidate else {
            return ControlFlow::Continue(());
        };
        if !matches!(
            generation.test_attribution_read().provider_state,
            tracedecay_domain::ProviderEvaluationStateV1::Indexing
                | tracedecay_domain::ProviderEvaluationStateV1::Cancelled
        ) {
            return ControlFlow::Continue(());
        }
        // Source publication gates are released before this
        // optional work joins the shared background bound.
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::AwaitingAdmission);
        let admission = Arc::clone(&self.background_reconcile_admission).acquire_owned();
        tokio::pin!(admission);
        let mut saw_wake = false;
        let permit = loop {
            if control.source_changed() {
                break None;
            }
            if self.shutting_down.load(Ordering::Acquire) {
                break None;
            }
            tokio::select! {
                permit = &mut admission => break permit.ok(),
                () = self.wake.notified() => { saw_wake = true; }
                changed = shutdown_observed.changed() => {
                    if changed.is_err() { break None; }
                }
            }
        };
        if saw_wake {
            self.wake.notify_one();
        }
        if control.source_changed() {
            // Its real source arrival retains the next pass;
            // never consume that wake as optional derivation.
            self.wake.notify_one();
            return ControlFlow::Break(PassEndV1::NextPass);
        }
        let Some(_permit) = permit else {
            return Self::exit(state).await;
        };
        CodeIndexWorkerPhaseV1::enter(&self.phase, CodeIndexWorkerPhaseV1::Working);
        let preparing = Arc::clone(&generation);
        let completion =
            native_runtime::spawn_blocking(move || preparing.prepare_test_attribution(&control))
                .await;
        match completion {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(
                error = %error,
                generation_id = %generation.manifest().generation_id,
                "background test attribution preparation failed"
            ),
            Err(error) => tracing::error!(
                error = %error,
                "background test attribution task failed"
            ),
        }
        // A completion for an old owner cannot announce a
        // successor's readiness. The memo itself is scoped to
        // the captured immutable generation.
        match self.serving_generation.read() {
            Ok(seated) => {
                if !self.shutting_down.load(Ordering::Acquire)
                    && seated
                        .as_ref()
                        .is_some_and(|latest| Arc::ptr_eq(&latest.generation, &generation))
                {
                    self.serving_generation_changed.send_replace(());
                }
            }
            Err(error) => tracing::error!(
                error = %error, "attribution completion seat is poisoned"
            ),
        }
        ControlFlow::Continue(())
    }
}

/// A publication whose text owner did not finish keeps the sealed
/// generation unseated.
fn withhold_prepared_seat(result: &mut PreparedSeatV1) {
    if let Ok((_, latest, replay_binding)) = result {
        *latest = None;
        *replay_binding = None;
    }
}

/// One blocking source reconcile and what it decides from, captured before
/// it leaves the worker.
// The flags are the pass's independent observations, sampled before the step.
#[allow(clippy::struct_excessive_bools)]
struct SourceReconcileStepV1 {
    stashed_successor: Option<CodeIndexReconcileOutcomeV1>,
    scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    shutting_down: Arc<AtomicBool>,
    graph_activation_enabled: bool,
    graph_activation_deferred: bool,
    serving_empty: bool,
    text_serving_ready: bool,
    retained_text_uses_partitioned_manifest: bool,
    retained_partitioned_graph_recovery_pending: bool,
    retained_text_metadata: Option<VerifiedSealedTextGenerationMetadataV1>,
    serving_generation: Arc<ServingGenerationSlot>,
    serving_source_witness: Arc<RwLock<Option<ServingSourceWitnessV1>>>,
    source_freshness: SourceFreshnessFenceV1,
}

impl SourceReconcileStepV1 {
    fn run(self) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        if let Some(outcome) = self.stashed_successor {
            return Ok(outcome);
        }
        let mut scheduler =
            Registry::lock_scheduler_unless_shutting_down(&self.scheduler, &self.shutting_down)?;
        // One arrival per attempted pass, before the branch: the
        // three reconcile entry points below are alternatives, so
        // hooking them individually would under- or double-count.
        #[cfg(test)]
        scheduler.arrive_reconcile_fault_for_test()?;
        // A prior pass may have built the successor and lost
        // only the durable write. Republish before choosing a
        // reconcile branch: graph-off with no text owner goes
        // to reconcile_now and would otherwise extract again.
        if let Some(outcome) = scheduler.republish_unpublished_retained_generation()? {
            return Ok(outcome);
        }
        // Legacy graph recovery requires a decoded complete
        // generation in the serving slot before a dirty-tree
        // rebuild. Revision 7 deliberately leaves that slot
        // empty: the retained manifest owner validates and
        // seats Grafeo below without opening partition bytes.
        // Graph-off and deferred passes also fall through to
        // retained text reconciliation.
        if self.graph_activation_enabled
            && !self.graph_activation_deferred
            && self.serving_empty
            && self.text_serving_ready
            && !self.retained_text_uses_partitioned_manifest
            && let Some(outcome) = scheduler.seat_retained_generation_on_empty_serving()?
        {
            return Ok(outcome);
        }
        if self.retained_partitioned_graph_recovery_pending
            && let Some(metadata) = self.retained_text_metadata.as_ref()
        {
            // Reserve this pass for verified-head recovery.
            // `Noop` here means no successor was published;
            // it never claims source currency. The successor
            // pass below captures the source state itself so
            // a quiet remount is not fabricated as dirty.
            return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                snapshot_content_identity: metadata.snapshot().content_identity.clone(),
                overflow_reconciled: false,
            }));
        }
        let outcome = if let Some(metadata) = self.retained_text_metadata {
            match scheduler
                .reconcile_retained_text_generation_with(&metadata, !self.graph_activation_enabled)
            {
                Ok(Some(outcome)) => Ok(outcome),
                Ok(None) if self.graph_activation_enabled => scheduler.activate_or_reconcile(),
                Ok(None) => scheduler.reconcile_now(),
                Err(error) => Err(error),
            }
        } else if self.graph_activation_enabled {
            scheduler.activate_or_reconcile()
        } else {
            scheduler.reconcile_now()
        }?;
        // A seat whose publishing pass could not prove its
        // source (`code_index_post_projection_source_unverified`)
        // installs without a currency witness. The swap arm
        // re-proves such a seat as `Offered`, but a retained
        // native graph that already serves skips the graph
        // prepare and with it the swap, so no later pass ever
        // reached that arm. This unchanged pass verified
        // exactly the snapshot the seat was sealed from, so
        // bind that proof here, while this pass still holds
        // the scheduler: a reader that holds the scheduler to
        // keep a seat unproven must not see the proof land
        // after the pass has already let go.
        if let CodeIndexReconcileOutcomeV1::Noop(evidence) = &outcome {
            Registry::bind_unproven_seat_to_verified_source(
                &self.serving_generation,
                &self.serving_source_witness,
                &self.source_freshness,
                &evidence.snapshot_content_identity,
            );
        }
        Ok(outcome)
    }
}

/// The blocking graph prepare: decode the active generation for the serving
/// swap unless a memory refusal or a deferred decode already decided it.
struct GraphPrepareStepV1 {
    graph_publish_refusal: Option<String>,
    defer_serving_decode: bool,
    scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    graph_text: Option<LatestCodeTextGenerationV1>,
    shutting_down: Arc<AtomicBool>,
    passes: Arc<ReconcilePassesV1>,
    pending_wake: Arc<PendingWakeV1>,
    wake: Arc<tokio::sync::Notify>,
}

impl GraphPrepareStepV1 {
    fn run(self) -> GraphPrepareStepResultV1 {
        // A graph build the memory watermark stopped
        // parks exactly like a decode that does not fit.
        if let Some(detail) = self.graph_publish_refusal {
            return Ok((
                None,
                None,
                false,
                Some(GraphPrepareStopV1::ResidentMemory(detail)),
            ));
        }
        if self.defer_serving_decode {
            return Ok((None, None, false, None));
        }
        let decoder = Registry::lock_scheduler_for_graph_step(
            &self.scheduler,
            &self.shutting_down,
            &self.passes,
        )?
        .1
        .active_generation_decoder();
        if decoder.is_none() {
            tracing::warn!(
                event = "code_index_graph_prepare_no_decoder",
                "graph prepare found no active generation decoder; \
                 the sealed generation cannot seat"
            );
        }
        // The seal released the decoded generation for
        // the text build. Decoding it again is charged
        // against the process budget, and a decode that
        // does not fit parks until memory is given back.
        let generation = match decoder
            .as_ref()
            .map(DaemonCodeIndexPublicationStoreV1::load_active_shared)
        {
            None => None,
            Some(Ok(generation)) => generation,
            Some(Err(error)) => {
                let stop = match error {
                    CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(detail) => {
                        GraphPrepareStopV1::ResidentMemory(detail)
                    }
                    error @ CodeIndexPublicationStoreErrorV1::StoreLockContended => {
                        GraphPrepareStopV1::StoreBusy(error.to_string())
                    }
                    error => {
                        tracing::warn!(
                            event = "code_index_graph_prepare_load_failed",
                            error = %error,
                            "active generation decode failed; \
                             the sealed generation cannot seat"
                        );
                        GraphPrepareStopV1::DecodeFailed(error.to_string())
                    }
                };
                return Ok((None, None, false, Some(stop)));
            }
        };
        let latest = match generation {
            Some(generation) => Registry::lock_scheduler_for_graph_step(
                &self.scheduler,
                &self.shutting_down,
                &self.passes,
            )?
            .1
            .servable_decoded_retained_generation(generation, self.graph_text.as_ref()),
            None => None,
        };
        // A refused ignored-source roster clears
        // itself, so the very next pass can publish
        // the successor, but this pass consumed the
        // wake that would have run it.
        let roster_refusal_rebuild = latest.is_none()
            && Registry::lock_scheduler_for_graph_step(
                &self.scheduler,
                &self.shutting_down,
                &self.passes,
            )?
            .1
            .take_ignored_roster_refusal_rebuild();
        if roster_refusal_rebuild {
            // One pass, claimed from the scheduler, so
            // a refusal that keeps reproducing cannot
            // spin this worker. Stamp before this
            // closure drops the step guard: the result
            // is observed only after the slot is set.
            Registry::note_visible_worker_continuation(
                &self.passes,
                &self.pending_wake,
                &self.wake,
            );
        }
        let replay_binding = match latest.as_ref() {
            Some(latest) => Some(
                Registry::lock_scheduler_for_graph_step(
                    &self.scheduler,
                    &self.shutting_down,
                    &self.passes,
                )?
                .1
                .code_graph_replay_binding(&latest.generation().manifest().generation_id),
            ),
            None => None,
        };
        replay_binding
            .transpose()
            .map(|binding| (latest, binding, roster_refusal_rebuild, None))
    }
}

/// The blocking serving swap of one prepared generation.
struct ServingSwapStepV1 {
    scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    serving_generation: Arc<ServingGenerationSlot>,
    serving_generation_epoch: Arc<AtomicU64>,
    serving_source_witness: Arc<RwLock<Option<ServingSourceWitnessV1>>>,
    text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>>,
    serving_seats: Arc<tokio::sync::watch::Sender<u64>>,
    serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    latest: LatestCompleteCodeIndexV1,
    shutting_down: Arc<AtomicBool>,
    passes: Arc<ReconcilePassesV1>,
    replace_serving_generation: bool,
}

impl ServingSwapStepV1 {
    fn run(self) -> Result<ServingSwapOutcomeV1, CodeIndexSchedulerErrorV1> {
        let Self {
            scheduler,
            serving_generation,
            serving_generation_epoch,
            serving_source_witness,
            text_generation,
            serving_seats,
            serving_generation_changed,
            latest,
            shutting_down,
            passes,
            replace_serving_generation,
        } = self;
        let (_swap_pass, scheduler) =
            Registry::lock_scheduler_for_graph_step(&scheduler, &shutting_down, &passes)?;
        // A generation that sealed while the checkout kept
        // moving is stale the moment it completes, and the
        // durable pointer may already name its successor.
        // Refusing the swap outright then left the route
        // serving nothing at all, so an empty serving slot
        // takes the stale seat and the next publication
        // supersedes it.
        //
        // Only the *active* publication may refuse that
        // stale seat. Asking merely whether something was
        // seated let an incumbent the canonical store had
        // already superseded keep the slot forever: both
        // candidate and incumbent were then non-active, so
        // every later pass refused too and serving never
        // converged on the durable head.
        let publication_matches = scheduler.active_publication_matches(&latest)?;
        // The freshness fence is the single source-currency
        // authority; the witness only binds one of its
        // proofs to the seat. Asking the fence whether it
        // has verified *this* sealed snapshot is what makes
        // the binding truthful for a seat this pass did not
        // publish. A git-index sample this seal moved is
        // not a different snapshot.
        // Dropping the witness here cleared the newer
        // generation. The lexical full-copy is not decided
        // on this swap.
        let sealed_currency = scheduler.currency_witness_for_sealed_snapshot(
            &latest.generation().manifest().generation_id,
            &latest.generation().snapshot().content_identity,
        );
        // A store that cannot answer keeps the incumbent
        // rather than displacing it.
        let incumbent_is_active = |incumbent: &Option<LatestCompleteCodeIndexV1>| {
            incumbent.as_ref().is_some_and(|incumbent| {
                scheduler
                    .active_publication_matches(incumbent)
                    .unwrap_or(true)
            })
        };
        // The publication comparison runs outside the
        // write guard, so readers never queue behind it.
        // Every slot writer bumps the epoch, so an
        // unchanged epoch under the guard proves the
        // incumbent it judged is still seated.
        let observed_epoch = serving_generation_epoch.load(Ordering::Acquire);
        let observed_incumbent = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let observed_active = incumbent_is_active(&observed_incumbent);
        drop(observed_incumbent);
        let mut serving = serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let incumbent_is_active =
            if serving_generation_epoch.load(Ordering::Acquire) == observed_epoch {
                observed_active
            } else {
                incumbent_is_active(&serving)
            };
        let outcome = ServingSwapOutcomeV1::decide(
            publication_matches,
            incumbent_is_active,
            replace_serving_generation,
        );
        // The displaced seats can hold the last reference
        // to a whole generation; they drop after the
        // guards, not inside the swap.
        let mut displaced = (None, None);
        if outcome.installs() {
            displaced.0 = serving.replace(latest.clone());
            serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
            displaced.1 = text_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replace(latest.text_generation_handle());
        }
        // A pass is the only thing that verifies source
        // against the sealed digests, so it is also the
        // only thing that can re-prove a seat. `Offered`
        // is that case: the active publication already
        // serves and this pass re-observed the checkout it
        // was sealed from. Arming only on a publication
        // left a restored, retired, or withdrawn seat
        // permanently unproven, busy verified reads then
        // refused a generation whose source was current.
        match outcome {
            ServingSwapOutcomeV1::Seated | ServingSwapOutcomeV1::Offered => {
                *serving_source_witness
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = sealed_currency;
            }
            // The durable pointer names a successor, so no
            // proof of this seat's currency exists to bind.
            ServingSwapOutcomeV1::SeatedStale => {
                *serving_source_witness
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            // The slot kept a foreign incumbent; its
            // witness belongs to that generation.
            ServingSwapOutcomeV1::Superseded => {}
        }
        drop(serving);
        drop(displaced);
        // The serving slot is now fully published, including
        // its exact-source witness, so dependent readers
        // may wake.
        if outcome.installs() {
            Registry::record_serving_seat(&serving_seats);
            serving_generation_changed.send_replace(());
        }
        Ok::<_, CodeIndexSchedulerErrorV1>(outcome)
    }
}
