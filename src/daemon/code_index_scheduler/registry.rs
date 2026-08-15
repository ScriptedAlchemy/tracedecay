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
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, WorktreeId};
use tracedecay_lsp::LspRuntimeFailure;

use super::graph_activation::CodeGraphActivationAuthorityV1;
use super::{
    CodeIndexArrivalV1, CodeIndexBytePoolStatsV1, CodeIndexCadenceOutcomeV1,
    CodeIndexCadenceReadModelV1, CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1,
    CodeIndexEventToReadyReceiptV1, CodeIndexNoopEvidenceV1, CodeIndexPublishEvidenceV1,
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexControlV1, GenerationDecodeAdmissionV1, LatestCompleteCodeIndexV1,
    PendingHintsV1, SharedCodeIndexBytePoolV1, newly_eligible_percentile, now_micros,
};

mod ignored_dependencies;
mod lsp_projection;
#[cfg(test)]
mod runtime_generation_census_tests;
mod scope_identity;

use self::ignored_dependencies::exact_activated_serving_generation;
pub(super) use scope_identity::{latest_matches_scope, latest_matches_scope_identity};

const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;

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
fn query_admission_barriers() -> &'static Mutex<BTreeMap<WorktreeId, Arc<tokio::sync::Barrier>>> {
    static BARRIERS: std::sync::OnceLock<Mutex<BTreeMap<WorktreeId, Arc<tokio::sync::Barrier>>>> =
        std::sync::OnceLock::new();
    BARRIERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// One mounted worktree's code scope identity and serving generation, read
/// without touching the scheduler mutex.
pub(in crate::daemon) struct CodeIndexServingScopeV1 {
    pub(in crate::daemon) repository_id: RepositoryId,
    pub(in crate::daemon) worktree_id: WorktreeId,
    pub(in crate::daemon) shutting_down: Arc<AtomicBool>,
    pub(in crate::daemon) serving_generation: Option<Arc<CodeIndexPublishedGenerationV1>>,
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
    /// Unix micros of the earliest pending wake not yet consumed by a receipt.
    pending_wake_micros: Arc<AtomicU64>,
    /// Packed [`CodeIndexCadenceTriggerV1`] for the pending wake.
    pending_wake_trigger: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    /// Count of in-flight owner passes; nonzero means activation or reconcile
    /// work is running for this worktree.
    reconcile_in_progress: Arc<AtomicUsize>,
    active_generation_encoded_bytes: Arc<AtomicU64>,
    pub(super) semantic_evaluation_publication_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

pub(in crate::daemon) struct CodeIndexSemanticEvaluationPublicationLeaseV1 {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

#[derive(Clone)]
pub(crate) struct CodeIndexSchedulerRegistryV1 {
    pub(super) max_worktrees: usize,
    pub(super) resident_memory: Arc<resident_memory::ProcessResidentMemoryV1>,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub(super) mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    /// Owners whose project was retired (remote deletion, replacement) but whose
    /// reconcile task has not finished draining. A root parked here must never
    /// re-mount: a fresh owner would race the dying one over the same store.
    pub(super) retiring: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    background_reconcile_admission: Arc<tokio::sync::Semaphore>,
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
    pub(super) fn install_query_admission_barrier(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        callers: usize,
    ) {
        let barrier = Arc::new(tokio::sync::Barrier::new(callers));
        let replaced = query_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.worktree_id.clone(), barrier);
        assert!(
            replaced.is_none(),
            "query-admission barrier already installed"
        );
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
    async fn pause_query_admission_for_test(scope: &tracedecay_application::ResolvedScope) {
        let barrier = query_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope.worktree_id)
            .cloned();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
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
            .map(|worktree| worktree.pending_wake_micros.load(Ordering::Acquire))
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
                worktree.pending_wake_micros.store(0, Ordering::Release);
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
                *worktree
                    .serving_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
        }
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
        pending_wake_micros: &AtomicU64,
        pending_wake_trigger: &AtomicU64,
        wake: &tokio::sync::Notify,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let wake_micros = u64::try_from(now_micros().0).unwrap_or(u64::MAX);
        let _ = pending_wake_micros.compare_exchange(
            0,
            wake_micros,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        pending_wake_trigger.store(Self::pack_trigger(trigger), Ordering::Release);
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
        pending_wake_micros: &AtomicU64,
        pending_wake_trigger: &AtomicU64,
        default_trigger: CodeIndexCadenceTriggerV1,
    ) -> (CodeIndexArrivalV1, CodeIndexCadenceTriggerV1) {
        let wake_micros = pending_wake_micros.swap(0, Ordering::AcqRel);
        if wake_micros == 0 {
            return (CodeIndexArrivalV1::Unavailable, default_trigger);
        }
        let trigger = Self::unpack_trigger(pending_wake_trigger.load(Ordering::Acquire));
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
        pending_wake_micros: &AtomicU64,
        pending_wake_trigger: &AtomicU64,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let Some(wake_micros) = arrival.wake_micros() else {
            return;
        };
        let Ok(wake_micros) = u64::try_from(wake_micros) else {
            return;
        };
        let mut observed = pending_wake_micros.load(Ordering::Acquire);
        loop {
            // A wake that arrived while this pass ran is newer, so the restored
            // arrival remains the earliest and stays authoritative.
            if observed != 0 && observed <= wake_micros {
                return;
            }
            match pending_wake_micros.compare_exchange_weak(
                observed,
                wake_micros,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        pending_wake_trigger.store(Self::pack_trigger(trigger), Ordering::Release);
    }

    fn record_reconcile_receipt(
        telemetry: &Mutex<CodeIndexCadenceTelemetryV1>,
        project_root: PathBuf,
        arrival: CodeIndexArrivalV1,
        trigger: CodeIndexCadenceTriggerV1,
        started_micros: i64,
        outcome: &CodeIndexReconcileOutcomeV1,
    ) {
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
    }

    /// Latest completed event-to-ready receipt for this registry, if any.
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

    pub(in crate::daemon) fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

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
                        .active_generation_encoded_bytes
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
        // A retiring owner still holds the store: admitting a fresh mount here
        // would race the dying reconcile task over the same physical shard.
        // Upstream took this check after mount admission; that permit no longer
        // exists at this tip, so the guard sits at the head of the mount path.
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
            // Reconcile holds this mutex; wait in the blocking pool so remount
            // never parks a runtime worker or admission for other lanes.
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
            return Ok(false);
        }
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        drop(mounted);
        drop(retiring);
        #[cfg(test)]
        Self::pause_cold_mount_admission_for_test(&project_root).await;
        // Keep CPU-bound cold-open identity setup off runtime workers.
        let scoped_store_root = super::scoped_code_index_store_root(&store_root, &project_root);
        let open_project_id = project_id.clone();
        let open_project_root = project_root.clone();
        let open_byte_pool = Arc::clone(&self.byte_pool);
        let opened = tokio::task::spawn_blocking(move || {
            let mut opened = CodeIndexWorktreeSchedulerV1::open(
                open_project_id,
                &open_project_root,
                scoped_store_root,
                open_byte_pool,
            )?;
            opened.replace_semantic_schedule_hook(semantic_schedule);
            Ok::<_, CodeIndexSchedulerErrorV1>(opened)
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
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let scheduler = Arc::new(Mutex::new(opened));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let ignored_dependency_admissions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_wake_micros = Arc::new(AtomicU64::new(0));
        let pending_wake_trigger = Arc::new(AtomicU64::new(0));
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_reconcile_in_progress = Arc::clone(&reconcile_in_progress);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_wake = Arc::clone(&wake);
        let worker_pending_wake_micros = Arc::clone(&pending_wake_micros);
        let worker_pending_wake_trigger = Arc::clone(&pending_wake_trigger);
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
        let task = tokio::spawn(async move {
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
                // Cover wake claim through failed-arrival restoration so admission
                // never misreads in-flight owner work as plain unavailability.
                let _reconcile_pass =
                    super::ReconcilePassGuard::enter(&worker_reconcile_in_progress);
                // Admission is held: queue wait ends and service time begins.
                let started_micros = now_micros().0;
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake_micros,
                    &worker_pending_wake_trigger,
                    CodeIndexCadenceTriggerV1::Mount,
                );
                // Serve-during-refresh: seat the last complete compatible
                // generation before rebuild. A cancelled refresh or branch
                // split must not hide a sealed generation for the duration
                // of reconcile. Stale is truthful; do not mark_reconciled.
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
                        if let Err(error) = activation {
                            tracing::warn!(
                                event = "code_index_retained_seat_failed",
                                path = "background_worker",
                                error = %error,
                                "code-index retained generation did not activate; refresh continues without stale serving"
                            );
                        } else {
                            let swap_scheduler = Arc::clone(&scheduler);
                            let swap_serving = Arc::clone(&serving_generation);
                            let _ = tokio::task::spawn_blocking(move || {
                                let scheduler = swap_scheduler
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if scheduler
                                    .active_publication_matches(&retained)
                                    .unwrap_or(false)
                                {
                                    *swap_serving
                                        .write()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(retained.clone());
                                    let _ = scheduler
                                        .schedule_semantic_generation(retained.generation());
                                }
                            })
                            .await;
                        }
                    }
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
                        result = Ok((Err(error), None, None));
                    }
                }
                if let Ok((Ok(_), Some(latest), _)) = &result {
                    let scheduler = Arc::clone(&worker_scheduler);
                    let serving_generation = Arc::clone(&worker_serving_generation);
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
                        *serving_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(latest.clone());
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
                    Self::record_reconcile_receipt(
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
                    Self::restore_pending_arrival(
                        &worker_pending_wake_micros,
                        &worker_pending_wake_trigger,
                        arrival,
                        trigger,
                    );
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                // The next coalesced hint wakes this worker after a contained panic.
                let _ = result;
            }
        });
        let mut mounted = self.mounted.lock().await;
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        mounted.insert(
            project_root,
            MountedCodeIndexWorktreeV1 {
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
                graph_activation,
                ignored_dependency_admissions,
                hints,
                wake: Arc::clone(&wake),
                epoch,
                pending_wake_micros: Arc::clone(&pending_wake_micros),
                pending_wake_trigger: Arc::clone(&pending_wake_trigger),
                shutting_down,
                reconcile_in_progress,
                active_generation_encoded_bytes,
                semantic_evaluation_publication_gate,
                task,
            },
        );
        // Until retained decode/truth verification completes, reads see warming
        // instead of serving unproven bytes.
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::Mount,
        );
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

    /// Revoke the live query authority for one exact admitted scope before a
    /// committed profile refresh. A failed replacement therefore leaves
    /// search unavailable instead of serving the prior profile.
    pub(in crate::daemon) async fn clear_query_authority(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let mut mounted = self.mounted.lock().await;
        let roots = mounted
            .iter()
            .filter(|(_, worktree)| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|(root, _)| root.clone())
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "cannot clear query authority before its worktree".to_owned(),
            ));
        }
        let mut scope_mismatch = false;
        for root in &roots {
            let worktree = mounted.get_mut(root).ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity("worktree disappeared".to_owned())
            })?;
            scope_mismatch |= worktree
                .query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest != &scope.scope_digest);
            if worktree.query_activation_revision.is_some() {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "standalone query clear cannot reset a committed authority pair".to_owned(),
                ));
            }
            worktree.query_authority = None;
        }
        if roots.len() != 1 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope is ambiguous".to_owned(),
            ));
        }
        if scope_mismatch {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope does not match the mounted authority".to_owned(),
            ));
        }
        Ok(())
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

    pub async fn notify_path(&self, project_root: &Path, path: PathBuf) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, epoch, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path(path);
        DaemonCodeIndexControlV1::advance(&epoch);
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::HookHint,
        );
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
        let (hints, wake, epoch, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
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
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::HookHint,
        );
        true
    }

    /// Preserve correctness when the pre-mount activation queue exceeds its
    /// bounded exact-path capacity. Overflow requests one authoritative scan for
    /// this exact mounted worktree; it never aliases a sibling worktree.
    pub async fn notify_hook_overflow(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (hints, wake, epoch, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        DaemonCodeIndexControlV1::advance(&epoch);
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::Overflow,
        );
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
        let (hints, wake, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::Overflow,
        );
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
        let (scheduler, serving_generation, wake, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
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
                        &pending_wake_micros,
                        &pending_wake_trigger,
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
                        &pending_wake_micros,
                        &pending_wake_trigger,
                        &wake,
                        CodeIndexCadenceTriggerV1::QueryAdmission,
                    );
                }
                return Some(latest);
            }
            // Cold open has no servable generation. Verification and any
            // rebuild stay with the retained owner; reads only request the
            // wake and return typed unavailable/unverified.
            if pending_wake_micros.load(Ordering::Acquire) == 0 {
                scheduler.request_background_reconcile();
                Self::note_wake(
                    &pending_wake_micros,
                    &pending_wake_trigger,
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
        if !latest_matches_scope(&latest, scope) {
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
        latest_matches_scope(&latest, scope).then_some(latest)
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
                || worktree.pending_wake_micros.load(Ordering::Acquire) != 0)
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
        let (scheduler, serving_generation, wake, pending_wake_micros, pending_wake_trigger) = {
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
                    Arc::clone(&worktree.pending_wake_micros),
                    Arc::clone(&worktree.pending_wake_trigger),
                ));
            }
            let Some(matched) = matched else {
                return false;
            };
            matched
        };
        // Debounce on the existing pending-wake slot: a wake already posted and
        // not yet claimed by the worker covers this admission too.
        if pending_wake_micros.load(Ordering::Acquire) != 0 {
            return false;
        }
        #[cfg(test)]
        Self::pause_query_admission_for_test(scope).await;
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
                        &pending_wake_micros,
                        &pending_wake_trigger,
                        &wake,
                        CodeIndexCadenceTriggerV1::BusyFollowUp,
                    );
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
                &pending_wake_micros,
                &pending_wake_trigger,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
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
        let retired = {
            let mut mounted = self.mounted.lock().await;
            project_roots
                .iter()
                .filter_map(|root| {
                    mounted
                        .remove(root)
                        .map(|worktree| (root.clone(), worktree))
                })
                .collect::<Vec<_>>()
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
        drained
    }

    #[cfg(test)]
    pub(super) async fn retiring_owner_count(&self) -> usize {
        self.retiring.lock().await.len()
    }

    pub fn cancel(&self) {
        self.background_reconcile_admission.close();
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
