//! Daemon-owned registry of mounted per-worktree code-index schedulers.
//!
//! Owns the map of live worktree schedulers, their reconciliation worker tasks,
//! and the shared content-addressed byte pool. The registry is the async-facing
//! surface: hook-hint delivery, query-admission freshness, and lifecycle
//! (mount/shutdown). The synchronous per-worktree indexing logic lives on
//! [`CodeIndexWorktreeSchedulerV1`]; this module never runs it while holding the
//! registry map lock.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use tracedecay_code_index::graph_projection::CodeGraphProjectionStore;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, WorktreeId};
use tracedecay_graph_db::{GraphProjectionTelemetryRequest, NeverCancelled};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::{
    CodeIndexArrivalV1, CodeIndexBytePoolStatsV1, CodeIndexCadenceOutcomeV1,
    CodeIndexCadenceReadModelV1, CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1,
    CodeIndexEventToReadyReceipt, CodeIndexNoopEvidenceV1, CodeIndexProductionErrorV1,
    CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexControlV1, GenerationDecodeAdmissionV1,
    LatestCompleteCodeIndex, PendingHintsV1, SharedCodeIndexBytePoolV1, newly_eligible_percentile,
    now_micros,
};

const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;
const RECONCILE_STALLED_AFTER_MICROS: u64 = 5 * 60 * 1_000_000;
const MIN_THROUGHPUT_SAMPLE_MICROS: u64 = 1_000_000;
const GIT_GRAPH_EVIDENCE_DRAIN_LIMIT: usize = 1_024;

#[derive(Clone)]
struct CodeIndexGitEvidenceReceiptSink {
    publication: super::DaemonCodeIndexPublicationStoreV1,
}

impl crate::graph::git::GitEvidenceReceiptSink for CodeIndexGitEvidenceReceiptSink {
    fn acknowledge<'a>(
        &'a self,
        intent: &'a tracedecay_domain::GitGraphEvidenceIntent,
        receipt: tracedecay_domain::GitGraphEvidencePublicationReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::graph::git::GitTopologyError>> + Send + 'a>>
    {
        let publication = self.publication.clone();
        let intent_digest = intent.intent_digest().clone();
        Box::pin(async move {
            let acknowledged = tokio::task::spawn_blocking(move || {
                publication.acknowledge_git_graph_evidence(&intent_digest, receipt)
            })
            .await
            .map_err(|error| {
                crate::graph::git::GitTopologyError::Repository(format!(
                    "code-generation Git evidence acknowledgement task failed: {error}"
                ))
            })?
            .map_err(|error| crate::graph::git::GitTopologyError::Repository(error.to_string()))?;
            if !acknowledged {
                return Err(crate::graph::git::GitTopologyError::Contract(
                    "code-generation Git evidence intent is no longer journaled".to_owned(),
                ));
            }
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
struct CodeIndexProgressProjection {
    lifecycle: crate::dashboard::code_index_freshness_api::CodeIndexLifecycleStateV1,
    processed_files: Option<u64>,
    total_files: Option<u64>,
    remaining_files: Option<u64>,
    throughput_files_per_second: Option<f64>,
    eta_lower_micros: Option<u64>,
    eta_upper_micros: Option<u64>,
}

fn project_code_index_progress(
    now_micros: u64,
    refreshing: bool,
    pending_wake_micros: u64,
    reconcile_failed: bool,
    reconcile_started_micros: u64,
    total_files: u64,
    processed_files: u64,
    complete_generation_files: Option<u64>,
) -> CodeIndexProgressProjection {
    use crate::dashboard::code_index_freshness_api::CodeIndexLifecycleStateV1;

    let elapsed_micros = now_micros.saturating_sub(reconcile_started_micros);
    let lifecycle = if refreshing
        && reconcile_started_micros > 0
        && elapsed_micros >= RECONCILE_STALLED_AFTER_MICROS
    {
        CodeIndexLifecycleStateV1::Stalled
    } else if refreshing {
        CodeIndexLifecycleStateV1::Indexing
    } else if reconcile_failed {
        CodeIndexLifecycleStateV1::Failed
    } else if pending_wake_micros > 0 {
        CodeIndexLifecycleStateV1::Queued
    } else if complete_generation_files.is_some() {
        CodeIndexLifecycleStateV1::Ready
    } else {
        CodeIndexLifecycleStateV1::Queued
    };

    let (total_files, processed_files) = if refreshing && total_files > 0 {
        let processed = processed_files.min(total_files);
        (Some(total_files), Some(processed))
    } else if matches!(lifecycle, CodeIndexLifecycleStateV1::Ready) {
        complete_generation_files.map_or((None, None), |complete| (Some(complete), Some(complete)))
    } else {
        (None, None)
    };
    let remaining_files = total_files
        .zip(processed_files)
        .map(|(total, processed)| total.saturating_sub(processed));

    let throughput_files_per_second = if refreshing
        && reconcile_started_micros > 0
        && elapsed_micros >= MIN_THROUGHPUT_SAMPLE_MICROS
        && processed_files.is_some_and(|processed| processed >= 2)
    {
        processed_files.map(|processed| (processed as f64) * 1_000_000.0 / (elapsed_micros as f64))
    } else {
        None
    };
    let (eta_lower_micros, eta_upper_micros) = match (
        throughput_files_per_second,
        remaining_files,
        processed_files,
    ) {
        (Some(rate), Some(remaining), Some(processed)) if rate > 0.0 && remaining > 0 => {
            // The only timing evidence available here is aggregate observed
            // throughput. Widen that observed rate by its finite-sample
            // relative error (1/sqrt(n)), bounded so the interval remains
            // useful without pretending to precision the scheduler did not
            // measure.
            let relative_error = (1.0 / (processed as f64).sqrt()).clamp(0.1, 0.5);
            let slow_rate = rate * (1.0 - relative_error);
            let fast_rate = rate * (1.0 + relative_error);
            let lower = ((remaining as f64) / fast_rate * 1_000_000.0).ceil();
            let upper = ((remaining as f64) / slow_rate * 1_000_000.0).ceil();
            (
                Some(lower.min(u64::MAX as f64) as u64),
                Some(upper.min(u64::MAX as f64) as u64),
            )
        }
        _ => (None, None),
    };

    CodeIndexProgressProjection {
        lifecycle,
        processed_files,
        total_files,
        remaining_files,
        throughput_files_per_second,
        eta_lower_micros,
        eta_upper_micros,
    }
}

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

/// How long a mount may wait for an admission permit before failing retryably.
///
/// Admission is only 2 permits wide, and the work it guards is an O(store)
/// generation decode. Several large worktrees opening at once therefore queue
/// N sequential decodes behind one unbounded `acquire()`, and the caller has no
/// way to learn it is queued rather than working. The wait is bounded instead:
/// a mount that cannot be admitted in time reports a typed warming error the
/// caller can retry, which is strictly better than holding the caller's
/// deadline hostage to a queue whose depth it cannot see.
const MOUNT_ADMISSION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Deadline-bounded mount admission.
///
/// Timing out is not a failure of the mount: the store is intact, the decode is
/// simply queued behind other worktrees. The typed
/// [`CodeIndexSchedulerErrorV1::MountAdmissionWarming`] says exactly that, so a
/// caller retries rather than treating a busy daemon as a broken store.
async fn acquire_mount_admission(
    admission: &Arc<tokio::sync::Semaphore>,
    deadline: std::time::Duration,
) -> Result<tokio::sync::SemaphorePermit<'_>, CodeIndexSchedulerErrorV1> {
    match tokio::time::timeout(deadline, admission.acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(CodeIndexSchedulerErrorV1::Identity(
            "code-index mount admission semaphore is closed".to_owned(),
        )),
        Err(_) => Err(CodeIndexSchedulerErrorV1::MountAdmissionWarming {
            waited_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexGenerationPublished {
    pub project_root: PathBuf,
    pub repository_id: RepositoryId,
    pub generation_id: CodeGenerationId,
    pub snapshot_content_identity: tracedecay_domain::ContentDigest,
    pub observation_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodeIndexSchedulerMemoryStatsV1 {
    pub mounted_worktrees: u64,
    pub reconciling_worktrees: u64,
    pub retained_generation_encoded_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexReconcileFailureV1 {
    Reconcile(String),
    Worker(String),
}

impl std::fmt::Display for CodeIndexReconcileFailureV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reconcile(message) => write!(formatter, "reconcile failed: {message}"),
            Self::Worker(message) => write!(formatter, "reconcile worker failed: {message}"),
        }
    }
}

pub(super) struct MountedCodeIndexWorktreeV1 {
    pub(super) project_id: ProjectId,
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
    pub(super) scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    publication: super::DaemonCodeIndexPublicationStoreV1,
    pub(super) graph_database: Arc<tracedecay_graph_db::GraphDb>,
    pub(super) serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndex>>>,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    /// Unix micros of the earliest pending wake not yet consumed by a receipt.
    pending_wake_micros: Arc<AtomicU64>,
    /// Packed [`CodeIndexCadenceTriggerV1`] for the pending wake.
    pending_wake_trigger: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    reconcile_in_progress: Arc<AtomicBool>,
    reconcile_started_micros: Arc<AtomicU64>,
    reconcile_failed: Arc<AtomicBool>,
    last_reconcile_failure: Arc<RwLock<Option<CodeIndexReconcileFailureV1>>>,
    reconcile_total_files: Arc<AtomicU64>,
    reconcile_processed_files: Arc<AtomicU64>,
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
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub(super) mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    graph_runtime: crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
    mount_admission: Arc<tokio::sync::Semaphore>,
    background_reconcile_admission: Arc<tokio::sync::Semaphore>,
    generation_publications: tokio::sync::broadcast::Sender<CodeIndexGenerationPublished>,
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
    pub fn new(max_worktrees: usize) -> Self {
        Self::with_graph_runtime(
            max_worktrees,
            crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry::default(),
        )
    }

    pub(in crate::daemon) fn with_graph_runtime(
        max_worktrees: usize,
        graph_runtime: crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
    ) -> Self {
        let (generation_publications, _) =
            tokio::sync::broadcast::channel(GENERATION_PUBLICATION_CHANNEL_CAPACITY);
        Self {
            max_worktrees,
            byte_pool: Arc::new(SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            graph_runtime,
            mount_admission: Arc::new(tokio::sync::Semaphore::new(
                bounded_daemon_admission_permits(),
            )),
            background_reconcile_admission: Arc::new(tokio::sync::Semaphore::new(
                bounded_daemon_admission_permits(),
            )),
            generation_publications,
            cadence_telemetry: Arc::new(Mutex::new(CodeIndexCadenceTelemetryV1::default())),
            activations: Arc::new(Mutex::new(BTreeMap::new())),
            test_attribution_authorities: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(in crate::daemon) async fn enqueue_git_convergence(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
        worktree_root: &Path,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let runtime = self.graph_runtime.clone();
        let graph_project_id = project_id.clone();
        let graph_store_root = project_store_root.to_path_buf();
        let database = tokio::task::spawn_blocking(move || {
            runtime.resolve_project(&graph_project_id, &graph_store_root)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::GraphUnavailable(format!(
                "Git convergence graph mount task failed: {error}"
            ))
        })??;
        self.graph_runtime
            .enqueue_git_convergence(project_id, project_store_root, worktree_root, database)
            .await?;
        Ok(())
    }

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
        }
    }

    fn unpack_trigger(packed: u64) -> CodeIndexCadenceTriggerV1 {
        match packed {
            2 => CodeIndexCadenceTriggerV1::HookHint,
            3 => CodeIndexCadenceTriggerV1::Overflow,
            4 => CodeIndexCadenceTriggerV1::QueryAdmission,
            5 => CodeIndexCadenceTriggerV1::BusyFollowUp,
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
        let receipt = CodeIndexEventToReadyReceipt::new(
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
    ) -> Option<CodeIndexEventToReadyReceipt> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest()
            .cloned()
    }

    /// Every retained event-to-ready receipt, oldest first.
    pub(in crate::daemon) fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceipt> {
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
    ) -> tokio::sync::broadcast::Receiver<CodeIndexGenerationPublished> {
        self.generation_publications.subscribe()
    }

    fn publish_generation(
        sender: &tokio::sync::broadcast::Sender<CodeIndexGenerationPublished>,
        project_root: PathBuf,
        evidence: &CodeIndexPublishEvidenceV1,
    ) {
        let _ = sender.send(CodeIndexGenerationPublished {
            project_root,
            repository_id: evidence.repository_id.clone(),
            generation_id: evidence.generation_id.clone(),
            snapshot_content_identity: evidence.snapshot_content_identity.clone(),
            observation_time_micros: now_micros().0,
        });
    }

    async fn enqueue_pending_git_graph_evidence(
        graph_runtime: &crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
        project_id: &ProjectId,
        project_store_root: &Path,
        project_root: &Path,
        graph_database: Arc<tracedecay_graph_db::GraphDb>,
        publication: super::DaemonCodeIndexPublicationStoreV1,
    ) -> Result<usize, CodeIndexSchedulerErrorV1> {
        let pending_publication = publication.clone();
        let pending = tokio::task::spawn_blocking(move || {
            pending_publication
                .pending_git_graph_evidence(GIT_GRAPH_EVIDENCE_DRAIN_LIMIT)
                .map_err(CodeIndexProductionErrorV1::Publication)
                .map_err(CodeIndexSchedulerErrorV1::from)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!(
                "code-index Git graph evidence read task failed: {error}"
            ))
        })??;
        let count = pending.len();
        let sink: Arc<dyn crate::graph::git::GitEvidenceReceiptSink> =
            Arc::new(CodeIndexGitEvidenceReceiptSink { publication });
        for entry in pending {
            graph_runtime
                .enqueue_git_evidence(
                    project_id,
                    project_store_root,
                    project_root,
                    Arc::clone(&graph_database),
                    entry.intent,
                    Arc::clone(&sink),
                )
                .await?;
        }
        Ok(count)
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
                    .filter(|worktree| worktree.reconcile_in_progress.load(Ordering::Acquire))
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

    pub(in crate::daemon) async fn mount_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_project_worktree(
            project_id,
            project_root,
            store_root.clone(),
            store_root,
            semantic_schedule,
        )
        .await
    }

    pub(in crate::daemon) async fn mount_project_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        project_store_root: PathBuf,
        code_index_store_root: PathBuf,
        semantic_schedule: Option<
            crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let graph_runtime = self.graph_runtime.clone();
        let graph_project_id = project_id.clone();
        let graph_store_root = project_store_root.clone();
        let graph_database = tokio::task::spawn_blocking(move || {
            graph_runtime.resolve_project(&graph_project_id, &graph_store_root)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!(
                "embedded graph mount task failed: {error}"
            ))
        })?
        .map_err(CodeIndexSchedulerErrorV1::from)?;
        let graph_store = Arc::new(CodeGraphProjectionStore::from_project_database(
            &project_id,
            graph_database.as_ref().clone(),
        )?);
        // Bound (not fully serialize) expensive mounts without pinning the
        // registry map. Restoring a sealed generation for a distinct worktree is
        // independent work, so a small bound lets concurrent opens proceed while
        // still capping simultaneous store-open pressure. Holding `mounted` here
        // would instead block every foreground query across every project.
        let _mount_admission =
            acquire_mount_admission(&self.mount_admission, MOUNT_ADMISSION_DEADLINE).await?;
        let mounted = self.mounted.lock().await;
        if let Some(existing) = mounted.get(&project_root) {
            let scheduler = Arc::clone(&existing.scheduler);
            let serving_generation = Arc::clone(&existing.serving_generation);
            drop(mounted);
            // The scheduler mutex is held for the full duration of any
            // in-flight reconcile. Waiting for it on a runtime worker — while
            // also holding the mount-admission permit — parked that worker and
            // starved mount admission for every other lane whenever a rebuild
            // was running. Pay the wait on the blocking pool instead.
            let remount_project_id = project_id.clone();
            let remount_hook = semantic_schedule.clone();
            tokio::task::spawn_blocking(move || {
                let mut scheduler = scheduler
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if scheduler.project_id() != &remount_project_id {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "mounted worktree belongs to a different project identity".to_owned(),
                    ));
                }
                scheduler.replace_semantic_schedule_hook(remount_hook);
                Ok(())
            })
            .await
            .map_err(|error| {
                CodeIndexSchedulerErrorV1::Identity(format!(
                    "code-index remount task failed: {error}"
                ))
            })??;
            let latest = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(|latest| latest.generation.clone());
            if let (Some(hook), Some(generation)) = (semantic_schedule, latest) {
                let _ = hook(&generation);
            }
            return Ok(false);
        }
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        drop(mounted);
        // Mount resolves structural identity and opens lightweight store owners
        // only. Sealed-generation decoding, validation, graph publication, and
        // source convergence all belong to the background worktree owner.
        let scoped_store_root =
            super::scoped_code_index_store_root(&code_index_store_root, &project_root);
        let open_project_id = project_id.clone();
        let open_project_root = project_root.clone();
        let open_byte_pool = Arc::clone(&self.byte_pool);
        let open_graph_store = Arc::clone(&graph_store);
        let open_semantic_schedule = semantic_schedule.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let mut opened = CodeIndexWorktreeSchedulerV1::open_with_graph_store(
                open_project_id,
                &open_project_root,
                scoped_store_root,
                open_byte_pool,
                open_graph_store,
            )?;
            if let Some(hook) = open_semantic_schedule {
                opened.replace_semantic_schedule_hook(Some(hook));
            }
            Ok::<_, CodeIndexSchedulerErrorV1>(opened)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!("code-index mount task failed: {error}"))
        })??;
        let repository_id = opened.identity().repository_id().clone();
        let worktree_id = opened.identity().worktree_id().clone();
        let reconcile_in_progress = opened.reconcile_in_progress();
        let (reconcile_total_files, reconcile_processed_files) = opened.reconcile_file_progress();
        let reconcile_started_micros = Arc::new(AtomicU64::new(0));
        let reconcile_failed = Arc::new(AtomicBool::new(false));
        let last_reconcile_failure = Arc::new(RwLock::new(None));
        let active_generation_encoded_bytes = opened.active_generation_encoded_bytes();
        let publication = opened.publication.clone();
        let serving_generation = Arc::new(RwLock::new(None));
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let scheduler = Arc::new(Mutex::new(opened));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let pending_wake_micros = Arc::new(AtomicU64::new(0));
        let pending_wake_trigger = Arc::new(AtomicU64::new(0));
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_wake = Arc::clone(&wake);
        let worker_pending_wake_micros = Arc::clone(&pending_wake_micros);
        let worker_pending_wake_trigger = Arc::clone(&pending_wake_trigger);
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_semantic_evaluation_publication_gate =
            Arc::clone(&semantic_evaluation_publication_gate);
        let worker_reconcile_started_micros = Arc::clone(&reconcile_started_micros);
        let worker_reconcile_failed = Arc::clone(&reconcile_failed);
        let worker_last_reconcile_failure = Arc::clone(&last_reconcile_failure);
        let worker_background_reconcile_admission =
            Arc::clone(&self.background_reconcile_admission);
        let worker_generation_publications = self.generation_publications.clone();
        let worker_graph_runtime = self.graph_runtime.clone();
        let worker_graph_project_id = project_id.clone();
        let worker_graph_store_root = project_store_root;
        let worker_graph_database = Arc::clone(&graph_database);
        let worker_publication = publication.clone();
        let worker_project_root = project_root.clone();
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
                // Dequeue instant: admission is held and the reconcile is about
                // to start, so queue wait ends here and service time begins.
                let started_micros = now_micros().0;
                worker_reconcile_started_micros.store(
                    u64::try_from(started_micros).unwrap_or(0),
                    Ordering::Release,
                );
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake_micros,
                    &worker_pending_wake_trigger,
                    CodeIndexCadenceTriggerV1::Mount,
                );
                let result = tokio::task::spawn_blocking(move || {
                    let mut scheduler = scheduler
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let result = scheduler.reconcile_now();
                    let latest = scheduler.latest_complete();
                    // Reconcile completion is an activation point: build this
                    // generation's serving derivations here, on the blocking
                    // pool, so the first query against it stays O(result).
                    if let Some(latest) = latest.as_ref() {
                        latest.warm_serving_caches();
                    }
                    (result, latest)
                })
                .await;
                if let Ok((_, Some(latest))) = &result {
                    *worker_serving_generation
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latest.clone());
                }
                if let Ok((Ok(outcome), _)) = &result {
                    worker_reconcile_failed.store(false, Ordering::Release);
                    if let Ok(mut failure) = worker_last_reconcile_failure.write() {
                        *failure = None;
                    }
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
                    if let Err(error) = Self::enqueue_pending_git_graph_evidence(
                        &worker_graph_runtime,
                        &worker_graph_project_id,
                        &worker_graph_store_root,
                        &worker_project_root,
                        Arc::clone(&worker_graph_database),
                        worker_publication.clone(),
                    )
                    .await
                    {
                        tracing::warn!(
                            event = "code_index_git_graph_evidence",
                            outcome = "pending",
                            error = %error,
                            "code-generation Git evidence remains durably pending"
                        );
                    }
                } else {
                    worker_reconcile_failed.store(true, Ordering::Release);
                    let failure = match &result {
                        Ok((Err(error), _)) => {
                            Some(CodeIndexReconcileFailureV1::Reconcile(error.to_string()))
                        }
                        Err(error) => Some(CodeIndexReconcileFailureV1::Worker(error.to_string())),
                        Ok((Ok(_), _)) => None,
                    };
                    if let Ok(mut retained_failure) = worker_last_reconcile_failure.write() {
                        *retained_failure = failure;
                    }
                    // A reconcile that never reaches a terminal outcome is the
                    // failure mode that leaves search stale indefinitely, and it
                    // used to be entirely silent. Surface it: bounded, redacted,
                    // no project path beyond what cadence events already carry.
                    match &result {
                        Ok((Err(error), _)) => tracing::warn!(
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
                        Ok((Ok(_), _)) => {}
                    }
                    // No terminal outcome, so no receipt is owed. Give the
                    // arrival back or the next pass would measure from its own
                    // dequeue and under-report the wait this wake really took.
                    Self::restore_pending_arrival(
                        &worker_pending_wake_micros,
                        &worker_pending_wake_trigger,
                        arrival,
                        trigger,
                    );
                }
                worker_reconcile_started_micros.store(0, Ordering::Release);
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                // A task panic must not permanently retire the mounted
                // worktree. The next coalesced hint wakes this worker again.
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
                project_id,
                repository_id,
                worktree_id,
                query_authority: None,
                semantic_query_authority: None,
                scheduler,
                publication,
                graph_database,
                serving_generation,
                hints,
                wake: Arc::clone(&wake),
                epoch,
                pending_wake_micros: Arc::clone(&pending_wake_micros),
                pending_wake_trigger: Arc::clone(&pending_wake_trigger),
                shutting_down,
                reconcile_in_progress,
                reconcile_started_micros,
                reconcile_failed,
                last_reconcile_failure,
                reconcile_total_files,
                reconcile_processed_files,
                active_generation_encoded_bytes,
                semantic_evaluation_publication_gate,
                task,
            },
        );
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
        worktree.query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(())
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

    pub(super) async fn query_authority_for_scope(
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
            let Some((scope_digest, authority)) = &worktree.query_authority else {
                return None;
            };
            if scope_digest != &scope.scope_digest || matched.is_some() {
                return None;
            }
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

    /// Retire one exact worktree scheduler without deleting its immutable
    /// generation projection from the project graph.
    ///
    /// Managed worktrees must call this before removing the checkout: the
    /// worker may otherwise still be reconciling source files while Git tears
    /// the directory down.  The project-wide Grafeo projection intentionally
    /// survives so historical generation/ref evidence remains queryable.
    pub(in crate::daemon) async fn unmount_worktree(&self, project_root: &Path) -> bool {
        let project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let worktree = self.mounted.lock().await.remove(&project_root);
        let Some(worktree) = worktree else {
            return false;
        };
        worktree.shutting_down.store(true, Ordering::Release);
        worktree.wake.notify_one();
        let _ = worktree.task.await;
        true
    }

    /// Retire live schedulers whose managed checkout has disappeared. Immutable
    /// generations remain in the project graph as ref/snapshot provenance.
    pub(in crate::daemon) async fn unmount_missing_worktrees(
        &self,
        project_id: &ProjectId,
    ) -> usize {
        let missing = {
            let mounted = self.mounted.lock().await;
            mounted
                .iter()
                .filter(|(root, worktree)| worktree.project_id == *project_id && !root.exists())
                .map(|(root, _)| root.clone())
                .collect::<Vec<_>>()
        };
        let mut retired = 0;
        for root in missing {
            retired += usize::from(self.unmount_worktree(&root).await);
        }
        retired
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

    pub(crate) async fn last_reconcile_failure(
        &self,
        project_root: &Path,
    ) -> Option<CodeIndexReconcileFailureV1> {
        let project_root = project_root.canonicalize().ok()?;
        let failure = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            Arc::clone(&worktree.last_reconcile_failure)
        };
        match failure.read() {
            Ok(current) => current.clone(),
            Err(_) => Some(CodeIndexReconcileFailureV1::Worker(
                "reconcile failure state lock is poisoned".to_owned(),
            )),
        }
    }

    pub(crate) async fn pending_git_graph_evidence(
        &self,
        project_root: &Path,
        limit: usize,
    ) -> Result<Vec<tracedecay_domain::GitGraphEvidenceIntent>, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let publication = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)
                .map(|worktree| worktree.publication.clone())
                .ok_or_else(|| {
                    CodeIndexSchedulerErrorV1::Identity(
                        "code-index worktree is not mounted".to_owned(),
                    )
                })?
        };
        tokio::task::spawn_blocking(move || {
            publication
                .pending_git_graph_evidence(limit)
                .map(|entries| entries.into_iter().map(|entry| entry.intent).collect())
                .map_err(CodeIndexProductionErrorV1::Publication)
                .map_err(CodeIndexSchedulerErrorV1::from)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!(
                "code-index Git graph evidence read task failed: {error}"
            ))
        })?
    }

    pub(crate) async fn acknowledge_git_graph_evidence(
        &self,
        project_root: &Path,
        intent_digest: tracedecay_domain::ContentDigest,
        receipt: tracedecay_domain::GitGraphEvidencePublicationReceipt,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let publication = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)
                .map(|worktree| worktree.publication.clone())
                .ok_or_else(|| {
                    CodeIndexSchedulerErrorV1::Identity(
                        "code-index worktree is not mounted".to_owned(),
                    )
                })?
        };
        tokio::task::spawn_blocking(move || {
            publication
                .acknowledge_git_graph_evidence(&intent_digest, receipt)
                .map_err(CodeIndexProductionErrorV1::Publication)
                .map_err(CodeIndexSchedulerErrorV1::from)
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!(
                "code-index Git graph evidence acknowledgement task failed: {error}"
            ))
        })?
    }

    /// Exact live dashboard projection for one mounted worktree.
    ///
    /// The freshness ladder runs before projection. Generation and scope fields
    /// are copied from the durable sealed generation, never reconstructed from
    /// the dashboard's display path.
    pub(in crate::daemon) async fn dashboard_freshness(
        &self,
        project_root: &Path,
    ) -> Option<crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let (
            project_id,
            scheduler,
            graph_database,
            reconcile_in_progress,
            reconcile_started_micros,
            reconcile_failed,
            reconcile_total_files,
            reconcile_processed_files,
            pending_wake_micros,
            serving_generation,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical_root)?;
            (
                worktree.project_id.clone(),
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.graph_database),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.reconcile_started_micros),
                Arc::clone(&worktree.reconcile_failed),
                Arc::clone(&worktree.reconcile_total_files),
                Arc::clone(&worktree.reconcile_processed_files),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.serving_generation),
            )
        };
        tokio::task::spawn_blocking(move || {
            let projection_for = |latest: &LatestCompleteCodeIndex| {
                let generation = latest.generation();
                let snapshot = generation.snapshot();
                let graph_watermark = tracedecay_code_index::graph_projection::
                    project_graph_namespace(&project_id)
                    .and_then(|namespace| {
                        tracedecay_code_index::graph_projection::
                            code_generation_projection_id(&generation.manifest().generation_id)
                            .and_then(|projection| {
                                tracedecay_code_index::graph_projection::
                                    code_generation_watermark(
                                        &generation.manifest().generation_id,
                                    )
                                    .map(|watermark| (namespace, projection, watermark))
                            })
                    })
                    .ok()
                    .and_then(|(namespace, projection, expected_watermark)| {
                        graph_database
                            .projection_telemetry(GraphProjectionTelemetryRequest {
                                namespace,
                                projection,
                                cancellation: Arc::new(NeverCancelled),
                            })
                            .ok()
                            .flatten()
                            .filter(|telemetry| telemetry.watermark == expected_watermark)
                    })
                    .map(|telemetry| telemetry.watermark.as_str().to_owned());
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
                    Some(generation.manifest().generation_id.as_str().to_owned()),
                    graph_watermark,
                    Some(snapshot.content_identity.as_str().to_owned()),
                    Some(generation.manifest().seal.sealed_at.0),
                    Some(u64::try_from(snapshot.files.len()).unwrap_or(u64::MAX)),
                )
            };
            let refreshing = reconcile_in_progress.load(Ordering::Acquire);
            let mut scheduler = match scheduler.try_lock() {
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
                        generation_id,
                        graph_watermark,
                        content_identity,
                        sealed,
                        complete_generation_files,
                    ) = latest.as_ref().map_or(
                        (None, None, None, None, None, None, None, None),
                        &projection_for,
                    );
                    let progress = project_code_index_progress(
                        u64::try_from(now_micros().0).unwrap_or(0),
                        refreshing,
                        pending_wake_micros.load(Ordering::Acquire),
                        reconcile_failed.load(Ordering::Acquire),
                        reconcile_started_micros.load(Ordering::Acquire),
                        reconcile_total_files.load(Ordering::Acquire),
                        reconcile_processed_files.load(Ordering::Acquire),
                        complete_generation_files,
                    );
                    let staleness_state = match progress.lifecycle {
                        crate::dashboard::code_index_freshness_api::
                            CodeIndexLifecycleStateV1::Queued => "queued",
                        crate::dashboard::code_index_freshness_api::
                            CodeIndexLifecycleStateV1::Indexing => "indexing",
                        crate::dashboard::code_index_freshness_api::
                            CodeIndexLifecycleStateV1::Stalled => "stalled",
                        crate::dashboard::code_index_freshness_api::
                            CodeIndexLifecycleStateV1::Failed => "failed",
                        crate::dashboard::code_index_freshness_api::
                            CodeIndexLifecycleStateV1::Ready => "fresh",
                    };
                    return crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: canonical_root.display().to_string(),
                        repository_id,
                        worktree_id,
                        source_reference,
                        latest_generation_id: generation_id,
                        graph_watermark,
                        snapshot_content_identity: content_identity,
                        sealed_at_micros: sealed,
                        last_reconcile_micros: None,
                        lifecycle_state: progress.lifecycle,
                        processed_files: progress.processed_files,
                        total_files: progress.total_files,
                        remaining_files: progress.remaining_files,
                        throughput_files_per_second: progress.throughput_files_per_second,
                        eta_lower_micros: progress.eta_lower_micros,
                        eta_upper_micros: progress.eta_upper_micros,
                        staleness_state: Some(staleness_state.to_owned()),
                        hook_hint_count: None,
                        coverage: "partial_refresh_in_progress".to_owned(),
                    };
                }
            };
            let reconciled = !reconcile_failed.load(Ordering::Acquire);
            let verified = scheduler.verified_against_source();
            let latest = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .or_else(|| scheduler.latest_complete_already_decoded());
            let hook_hint_count = scheduler.pending_hint_count();
            let (
                repository_id,
                worktree_id,
                source_reference,
                generation_id,
                graph_watermark,
                content_identity,
                sealed,
                complete_generation_files,
            ) = latest
                .as_ref()
                .map_or((None, None, None, None, None, None, None, None), projection_for);
            let progress = project_code_index_progress(
                u64::try_from(now_micros().0).unwrap_or(0),
                reconcile_in_progress.load(Ordering::Acquire),
                pending_wake_micros.load(Ordering::Acquire),
                !reconciled,
                reconcile_started_micros.load(Ordering::Acquire),
                reconcile_total_files.load(Ordering::Acquire),
                reconcile_processed_files.load(Ordering::Acquire),
                complete_generation_files,
            );
            let staleness_state = match progress.lifecycle {
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Failed => "failed",
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Stalled => "stalled",
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Queued => "queued",
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Indexing => "indexing",
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Ready if !verified => "stale",
                crate::dashboard::code_index_freshness_api::
                    CodeIndexLifecycleStateV1::Ready => "fresh",
            };
            let graph_projection_ready = graph_watermark.is_some();
            crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                worktree_root: canonical_root.display().to_string(),
                repository_id,
                worktree_id,
                source_reference,
                latest_generation_id: generation_id,
                graph_watermark,
                snapshot_content_identity: content_identity,
                sealed_at_micros: sealed,
                last_reconcile_micros: scheduler.last_reconciled_at_micros(),
                lifecycle_state: progress.lifecycle,
                processed_files: progress.processed_files,
                total_files: progress.total_files,
                remaining_files: progress.remaining_files,
                throughput_files_per_second: progress.throughput_files_per_second,
                eta_lower_micros: progress.eta_lower_micros,
                eta_upper_micros: progress.eta_upper_micros,
                staleness_state: Some(staleness_state.to_owned()),
                hook_hint_count,
                coverage: if !reconciled {
                    "unknown_reconcile_failed"
                } else if !verified {
                    "partial_unverified_restore"
                } else if !graph_projection_ready {
                    "partial_graph_projection_warming"
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

    /// Query admission serves only an already activated immutable generation.
    ///
    /// It never opens Git, scans status, captures files, decodes a generation,
    /// or waits for the per-scheduler mutex. A coalesced background wake owns
    /// convergence while status reports queued/indexing/stale truth.
    pub(in crate::daemon) async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndex> {
        let project_root = project_root.canonicalize().ok()?;
        let (serving_generation, wake, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        let authority_root = project_root.clone();
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(latest) = latest else {
            Self::note_wake(
                &pending_wake_micros,
                &pending_wake_trigger,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
            return None;
        };
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
    ) -> Option<LatestCompleteCodeIndex> {
        self.latest_complete_ready_with(project_root, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// [`Self::latest_complete_ready`] under an explicit decode admission.
    async fn latest_complete_ready_with(
        &self,
        project_root: &Path,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndex> {
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
            *serving_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latest.clone());
            Some(latest)
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
    ) -> Option<LatestCompleteCodeIndex> {
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
        // [`Self::latest_matches_scope_identity`]), and the ladder has already
        // scheduled the rebuild that will replace this generation.
        Self::latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Resolve one exact scope and admit only an already-current generation.
    pub(in crate::daemon) async fn latest_complete_ready_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndex> {
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
    ) -> Option<LatestCompleteCodeIndex> {
        self.latest_complete_ready_for_scope_with(
            scope,
            GenerationDecodeAdmissionV1::AlreadyDecoded,
        )
        .await
    }

    async fn latest_complete_ready_for_scope_with(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndex> {
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
        Self::latest_matches_scope(&latest, scope).then_some(latest)
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
    ) -> Option<LatestCompleteCodeIndex> {
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
        Self::latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Coalesce one background wake for a query that found nothing servable.
    ///
    /// This path deliberately performs no Git/status/file work and never waits
    /// for the scheduler mutex; the single worktree owner consumes the wake.
    pub(in crate::daemon) async fn request_query_background_reconcile(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let (serving_generation, wake, pending_wake_micros, pending_wake_trigger) = {
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
        if serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return false;
        }
        if pending_wake_micros.load(Ordering::Acquire) != 0 {
            return false;
        }
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::QueryAdmission,
        );
        true
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

    /// The exact scope gate: repository, worktree, **and** reference must all
    /// equal the admitted scope.
    ///
    /// This is the gate for anything reported as *current*. A generation sealed
    /// under a different reference is not current for this scope and must never
    /// be presented as fresh.
    pub(super) fn latest_matches_scope(
        latest: &LatestCompleteCodeIndex,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        Self::latest_matches_scope_identity(latest, scope)
            && latest.generation.snapshot().reference == scope.reference
    }

    /// The relaxed scope gate for the **stale** serving arms: the structural
    /// identity (repository + worktree) must still match exactly, but a moved
    /// `reference` is tolerated.
    ///
    /// Why this exists: `serving_generation` is in-memory and reseeded at mount
    /// from the restored sealed generation. That generation was sealed under
    /// whatever HEAD was current when it was published, so the ordinary
    /// develop-then-restart cycle (commit, then restart the daemon) leaves every
    /// restored generation with a reference the admitted scope has already moved
    /// past. Under the exact gate that made serve-stale die with the process and
    /// collapsed search — the one lane with no other fallback — for the entire
    /// rebuild window, which is precisely the invariant
    /// `docs/SERVING-PATH-PERFORMANCE.md` forbids ("await-new never preempts
    /// serve-old").
    ///
    /// Attribution stays sound because it is never derived from the admitted
    /// scope. Every hydration path builds its `RetrievalScope` from
    /// `latest.generation.snapshot()` — the generation's own sealed identity —
    /// so a relaxed admission answers *as the generation it actually is*, under
    /// its own repository/worktree/reference and its own snapshot digest. The
    /// caller is required to mark the answer stale; it is a different, older
    /// revision of the same worktree, not a current one.
    pub(super) fn latest_matches_scope_identity(
        latest: &LatestCompleteCodeIndex,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let snapshot = latest.generation.snapshot();
        snapshot.repository == scope.repository_id
            && snapshot.worktree.as_ref() == Some(&scope.worktree_id)
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
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
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
    }
}

impl crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
        document_uri: Option<String>,
    ) -> crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityFuture
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
                crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityV1 {
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

impl crate::application::lsp_runtime::LspCodeIndexProjectionIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn current_identity(
        &self,
        project_root: PathBuf,
        document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<
        Result<crate::application::lsp_runtime::LspCodeIndexProjectionIdentity, LspRuntimeFailure>,
    > {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("lsp-code-index-root-unavailable"))?;
            let current = registry
                .latest_complete_ready(&root)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-generation-unavailable"))?;
            let generation = &current.generation;
            let document_content_digest = document_relative_path
                .map(|path| path.replace('\\', "/"))
                .map(|logical_path| {
                    generation
                        .snapshot()
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .map(|file| file.content_digest.clone())
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("lsp-code-index-document-unavailable")
                        })
                })
                .transpose()?;
            Ok(
                crate::application::lsp_runtime::LspCodeIndexProjectionIdentity {
                    code_generation_id: generation.manifest().generation_id.clone(),
                    snapshot_digest: generation.manifest().snapshot_digest.clone(),
                    invalidation_digest: generation.manifest().invalidation_digest.clone(),
                    snapshot_content_digest: generation.snapshot().content_identity.clone(),
                    document_content_digest,
                },
            )
        })
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

#[cfg(test)]
mod mount_admission_tests {
    use super::*;
    use crate::dashboard::code_index_freshness_api::CodeIndexLifecycleStateV1;

    #[test]
    fn progress_projection_exposes_typed_queue_failure_stall_and_ready_states() {
        let now = 1_000_000_000;
        let queued = project_code_index_progress(now, false, now, false, 0, 0, 0, None);
        assert_eq!(queued.lifecycle, CodeIndexLifecycleStateV1::Queued);
        assert_eq!(queued.processed_files, None);

        let failed = project_code_index_progress(now, false, now, true, 0, 0, 0, None);
        assert_eq!(failed.lifecycle, CodeIndexLifecycleStateV1::Failed);

        let stalled = project_code_index_progress(
            now,
            true,
            0,
            false,
            now - RECONCILE_STALLED_AFTER_MICROS,
            10,
            3,
            None,
        );
        assert_eq!(stalled.lifecycle, CodeIndexLifecycleStateV1::Stalled);
        assert_eq!(stalled.remaining_files, Some(7));

        let ready = project_code_index_progress(now, false, 0, false, 0, 0, 0, Some(12));
        assert_eq!(ready.lifecycle, CodeIndexLifecycleStateV1::Ready);
        assert_eq!(ready.processed_files, Some(12));
        assert_eq!(ready.remaining_files, Some(0));
    }

    #[test]
    fn progress_projection_withholds_rate_until_evidenced_and_bounds_eta() {
        let now = 10_000_000;
        let too_early =
            project_code_index_progress(now, true, 0, false, now - 500_000, 10, 3, None);
        assert_eq!(too_early.throughput_files_per_second, None);
        assert_eq!(too_early.eta_lower_micros, None);

        let measured =
            project_code_index_progress(now, true, 0, false, now - 2_000_000, 10, 4, None);
        assert_eq!(measured.throughput_files_per_second, Some(2.0));
        assert!(measured.eta_lower_micros.is_some());
        assert!(
            measured.eta_lower_micros.expect("lower ETA")
                < measured.eta_upper_micros.expect("upper ETA")
        );
    }

    #[tokio::test]
    async fn admission_within_the_deadline_returns_a_permit() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = acquire_mount_admission(&admission, std::time::Duration::from_secs(5))
            .await
            .expect("a free permit is admitted immediately");
        drop(permit);
    }

    #[tokio::test]
    async fn an_exhausted_admission_fails_retryably_at_the_deadline() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&admission)
            .acquire_owned()
            .await
            .expect("semaphore is open");
        let deadline = std::time::Duration::from_millis(50);
        let started = std::time::Instant::now();
        let error = acquire_mount_admission(&admission, deadline)
            .await
            .expect_err("an exhausted admission must not wait unbounded");
        assert!(
            started.elapsed() >= deadline,
            "the deadline must be observed before failing"
        );
        assert!(
            matches!(
                error,
                CodeIndexSchedulerErrorV1::MountAdmissionWarming { waited_ms } if waited_ms == 50
            ),
            "expected a typed warming error, got {error:?}"
        );
        assert!(error.is_retryable(), "warming is retryable");
        drop(held);
    }

    #[tokio::test]
    async fn a_closed_admission_is_not_retryable() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        admission.close();
        let error = acquire_mount_admission(&admission, std::time::Duration::from_secs(5))
            .await
            .expect_err("a closed semaphore cannot admit");
        assert!(
            !error.is_retryable(),
            "a closed admission never reopens, so retrying cannot succeed"
        );
    }
}
