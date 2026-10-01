//! Retention, compaction, and garbage-collection operations run by the daemon
//! maintenance owner.
//!
//! Every operation that opens or garbage-collects a store lives here so its
//! `StoreAdministration` lifetime is kept separate from the watcher state
//! machine. The git watcher itself never opens or mutates a store: it routes
//! exact-frontier freshness requests to the code-index scheduler and wakes the
//! maintenance owner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::lease::ProjectStoreMaintenanceLeaseV1;
use crate::telemetry::StoreTelemetrySamplingRegistry;
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1, CodeGenerationRetentionPlanV1,
    CodeGenerationRetentionReportV1, CodeIndexScopeV1, code_index_scopes, code_index_store_root,
    execute_code_generation_retention_cancellable,
    prepare_next_code_generation_retention_cancellable,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_domain::CodeGenerationId;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::logging::log_daemon_event;

mod graph_replay;
mod scope_reconciliation;
use graph_replay::{defer_graph_replay_pool_busy, log_code_generation_retention_degraded};
#[cfg(test)]
pub(crate) mod registered_tests;
pub use scope_reconciliation::{run_code_index_scope_reconciliation, scheduler_owner_roots};

/// Outcome of one bounded code-generation retention pass.
///
/// `MoreWork` reports bounded progress with a remaining backlog, another
/// collectable superseded generation, superseded bytes a transient holder
/// (serving seat, in-flight text replacement) is about to release, or
/// unconsumed graph-replay release evidence, so the maintenance owner keeps
/// the short cadence until the store converges instead of parking multi-GiB
/// debris behind the full maintenance interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationRetentionOutcomeV1 {
    Complete,
    MoreWork,
    Failed,
}

impl CodeGenerationRetentionOutcomeV1 {
    /// The outcome of two independent passes over one tick: a failure keeps
    /// the retry cadence, and any remaining work keeps the short cadence.
    #[must_use]
    pub const fn with_pass(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::MoreWork, _) | (_, Self::MoreWork) => Self::MoreWork,
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }
}

fn defer_generation_store_busy(
    observations: &crate::telemetry::StoreTelemetrySamplingRegistry,
) -> CodeGenerationRetentionOutcomeV1 {
    log_code_generation_retention_degraded(observations, "generation_store_busy");
    CodeGenerationRetentionOutcomeV1::Failed
}

/// Exact liveness roots for one code-index scope beyond the active pointer,
/// which the planner marks itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeGenerationProtectionV1 {
    /// The generations a mounted scheduler serves for the root, sealed and
    /// text slots. Empty when the root is not mounted in this daemon.
    pub serving: BTreeSet<CodeGenerationId>,
    /// `serving` plus every live native-preview candidate binding of the
    /// root's repository.
    pub sources: BTreeSet<CodeGenerationId>,
}

/// Why the protection set of a root could not be resolved. Retention and the
/// storage report refuse to plan without it rather than guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationProtectionUnavailableV1 {
    /// The root yields no repository identity.
    RepositoryIdentity,
    /// The native-integration binding inventory could not be read.
    NativeBindings,
}

impl CodeGenerationProtectionUnavailableV1 {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::RepositoryIdentity => "repository_identity_unavailable",
            Self::NativeBindings => "native_bindings_unavailable",
        }
    }
}

/// Resolve the protection set daemon retention plans against for one root.
///
/// The planner marks only the active pointer head itself. This adds the
/// generation the mounted scheduler is serving and every live native-preview
/// candidate binding, because both can bind a generation the active pointer
/// alone cannot name. Native previews bind retained-only candidate generations
/// between preflight and terminal apply; their durable commitments are
/// liveness roots. The bindings are keyed by repository, a pure function of
/// the checkout's git common dir, so a root that is not mounted in this daemon
/// still resolves them.
#[hotpath::measure(
    label = "daemon.git.maintenance.code_generation_protection",
    future = true
)]
pub async fn code_generation_protection(
    schedulers: &CodeIndexSchedulerRegistryV1,
    profile_database: &RegisteredGlobalDb,
    project_root: &Path,
) -> Result<CodeGenerationProtectionV1, CodeGenerationProtectionUnavailableV1> {
    let serving = serving_generation_pins(schedulers, project_root).await;
    let repository_id = match schedulers.serving_code_scope(project_root).await {
        Some(serving_scope) => serving_scope.repository_id,
        None => tracedecay_code_index_runtime::code_index_scheduler::identity::repository_id_for(
            project_root,
        )
        .map_err(|_| CodeGenerationProtectionUnavailableV1::RepositoryIdentity)?,
    };
    let native_pins = tracedecay_global_db::GlobalDbNativeIntegrationStore::new(profile_database)
        .live_candidate_generation_bindings(
            &repository_id,
            tracedecay_contracts::clock::now_micros(),
        )
        .await
        .map_err(|_| CodeGenerationProtectionUnavailableV1::NativeBindings)?;
    let mut sources = serving.clone();
    sources.extend(native_pins);
    Ok(CodeGenerationProtectionV1 { serving, sources })
}

/// Recover any prior apply and build the next fully verified collection
/// batch. Full digest verification routinely reads several GiB, so it runs on
/// the blocking pool with the daemon shutdown token, which the planner checks
/// after every bounded read chunk; it creates no journal before verification
/// completes. `Err` carries the pass outcome, already logged.
async fn plan_collection(
    store_root: &Path,
    protected_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_root: &Path,
    backoff_root: &Path,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionOutcomeV1> {
    let plan_root = store_root.to_path_buf();
    let plan_sources = protected_sources.clone();
    let plan_cancellation = cancellation.clone();
    let plan_pool_root = graph_replay_pool_root.to_path_buf();
    let plan = tokio::task::spawn_blocking(move || {
        prepare_next_code_generation_retention_cancellable(
            &plan_root,
            &plan_sources,
            &|| plan_cancellation.is_cancelled(),
            Some(&plan_pool_root),
        )
    })
    .await;
    match plan {
        Ok(Ok(plan)) => Ok(plan),
        Ok(Err(CodeGenerationRetentionErrorV1::Cancelled)) => {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
        Ok(Err(CodeGenerationRetentionErrorV1::GraphReplayPoolBusy)) => {
            Err(defer_graph_replay_pool_busy(observations, backoff_root))
        }
        Ok(Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy)) => {
            Err(defer_generation_store_busy(observations))
        }
        Ok(Err(error)) => {
            // The bare label proved undiagnosable on a live profile: without
            // the typed error, a pointer CAS loss under rebuild churn is
            // indistinguishable from unrecognized-file or storage failures.
            observations.mark_loud_retention_log();
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "code_generations".to_string()),
                    ("failure", "retention_plan_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
        Err(_) => {
            log_code_generation_retention_degraded(observations, "retention_task_panicked");
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
    }
}

/// Apply one fully verified batch and log what it reclaimed. `Err` carries
/// the pass outcome, already logged.
async fn execute_collection(
    store_root: &Path,
    plan: CodeGenerationRetentionPlanV1,
    graph_replay_pool_root: &Path,
    backoff_root: &Path,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionOutcomeV1> {
    // `current_timestamp()` counts seconds; wrapping it in `UtcMicros` stamped
    // every deletion receipt with a seconds value in a micros-typed field
    // (live receipts read as 1970). The receipt is durable journal evidence,
    // so it takes the canonical micros clock.
    let completed_at = tracedecay_contracts::clock::now_micros();
    let execution_root = store_root.to_path_buf();
    let execution_pool_root = graph_replay_pool_root.to_path_buf();
    let execution_cancellation = cancellation.clone();
    let report = tokio::task::spawn_blocking(move || {
        execute_code_generation_retention_cancellable(
            &execution_root,
            plan,
            CodeGenerationRetentionModeV1::Apply,
            completed_at,
            Some(&execution_pool_root),
            &|| execution_cancellation.is_cancelled(),
        )
    })
    .await;
    match report {
        Ok(Ok(report)) => {
            let generation_reclaimed = report.receipt.as_ref().map_or_else(
                || {
                    report
                        .deleted_generations
                        .iter()
                        .map(|generation| generation.size_bytes)
                        .sum()
                },
                |receipt| receipt.reclaimed_bytes,
            );
            let text_artifact_reclaimed = report.text_artifact_receipt.as_ref().map_or_else(
                || {
                    report
                        .deleted_text_artifacts
                        .iter()
                        .map(|artifact| artifact.size_bytes)
                        .sum()
                },
                |receipt| receipt.reclaimed_bytes,
            );
            let reclaimed = generation_reclaimed.saturating_add(text_artifact_reclaimed);
            if reclaimed > 0 {
                log_daemon_event(
                    "retention_code_generations",
                    &[
                        ("store", "code-index-v1".to_string()),
                        ("bytes_reclaimed", reclaimed.to_string()),
                        (
                            "generations_collected",
                            report.deleted_generations.len().to_string(),
                        ),
                        (
                            "text_artifacts_collected",
                            report.deleted_text_artifacts.len().to_string(),
                        ),
                    ],
                );
            }
            Ok(report)
        }
        Ok(Err(CodeGenerationRetentionErrorV1::Cancelled)) => {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
        Ok(Err(CodeGenerationRetentionErrorV1::GraphReplayPoolBusy)) => {
            Err(defer_graph_replay_pool_busy(observations, backoff_root))
        }
        Ok(Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy)) => {
            Err(defer_generation_store_busy(observations))
        }
        Ok(Err(error)) => {
            // Same diagnosability contract as the plan failure: the apply
            // step's typed error names the exact refusal (CAS loss, unsafe
            // state, storage) instead of a bare retry label.
            observations.mark_loud_retention_log();
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "code_generations".to_string()),
                    ("failure", "retention_pass_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
        Err(_) => {
            log_code_generation_retention_degraded(observations, "retention_task_panicked");
            Err(CodeGenerationRetentionOutcomeV1::Failed)
        }
    }
}

/// Whether an applied batch left more for the next census: something was
/// collected (the rewritten index can free text artifacts the batch planned
/// before the rewrite), or the segment sweep stopped at its batch bound. A
/// census that finds nothing returns `Complete` one tick later at metadata
/// cost only.
fn collection_left_work(report: &CodeGenerationRetentionReportV1) -> bool {
    report.generation_segment_batch_exhausted
        || !report.deleted_generations.is_empty()
        || !report.deleted_text_artifacts.is_empty()
}

/// Collect superseded code-index generations for one mounted project.
///
/// Sealed generations are ordinary files, so no database retention or
/// compaction pass reclaims them; this runs on the ordinary maintenance
/// cadence against [`code_generation_protection`]. A root that yields no
/// repository identity or an unreadable binding inventory fails the pass
/// closed instead of sweeping blind.
#[hotpath::measure(
    label = "daemon.git.maintenance.code_generation_retention",
    future = true
)]
pub async fn run_code_generation_retention(
    lease: &ProjectStoreMaintenanceLeaseV1,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
) -> CodeGenerationRetentionOutcomeV1 {
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded(observations, "retention_cancelled");
        return CodeGenerationRetentionOutcomeV1::Failed;
    }
    let layout = lease.store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    // A store directory that never materialized has nothing to sweep. A store
    // *without* an active pointer is different: it is crash debris from a
    // publish that never reached its pointer write (an OOM-killed rebuild is
    // the ordinary cause), and the planner collects it as a typed unpublished
    // store, before this, such orphaned partial generations were unreachable
    // by every retention pass while their worktree root stayed live.
    if !store_root.is_dir() {
        return CodeGenerationRetentionOutcomeV1::Complete;
    }
    // Retired generations stay reachable for graph replay through the replay
    // pool; retention hard-links each one there before its release event
    // becomes durable, and the replay reconciler deletes pool entries once
    // the graph confirms it no longer needs them.
    let graph_replay_pool_root = lease
        .graph_db()
        .database_path()
        .with_extension("graph-replay");
    let protection = match code_generation_protection(
        schedulers,
        lease.profile_database().as_ref(),
        &layout.project_root,
    )
    .await
    {
        Ok(protection) => protection,
        Err(unavailable) => {
            log_code_generation_retention_degraded(observations, unavailable.reason());
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    // A held replay pool makes every later phase of this pass fail closed:
    // the release reconcile's pool acquisition would burn its whole
    // graph-operation deadline discovering the holder (the live wedge logged
    // that as `graph_replay_release_failed error=DeadlineExceeded` on every
    // tick), and the collection executor would then contend for the same
    // lock while holding the daemon writer gate. One non-blocking probe
    // defers the pass for this tick instead, before the multi-GiB
    // full-digest planning below is paid, and the executor's own checked
    // acquire returns `GraphReplayPoolBusy` if a publisher wins the
    // probe-to-execute window, so the writer gate is never pinned on a
    // blocking flock. Both paths arm the same bounded release backoff.
    if graph_replay::replay_pool_is_held(&graph_replay_pool_root) {
        return defer_graph_replay_pool_busy(observations, lease.project_root());
    }
    let plan = match plan_collection(
        &store_root,
        &protection.sources,
        &graph_replay_pool_root,
        lease.project_root(),
        observations,
        cancellation,
    )
    .await
    {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    // A failed, deferred, or retained replay reconcile keeps its durable
    // release evidence for a later graph-available pass. Deleting newly
    // planned files stays safe, retention hard-links each retired generation
    // into the replay pool before its release event becomes durable, so the
    // graph can always finish its retirement later. The pass therefore keeps
    // collecting instead of letting sealed generations and their multi-GiB
    // text artifacts accumulate without bound whenever the graph is dark,
    // wedged, or busy (a recurring `graph_replay_release_failed` used to
    // abort every pass here and grew one store by tens of GiB in a single
    // crash-rebuild night). A failure still reports degraded and fails the
    // pass so the retry cadence stays short; a deferral fails the pass
    // quietly under the bounded backoff.
    let mut replay_reconcile_failed = false;
    let mut release_backlog_remains = false;
    let replay_reconcile_attemptable = match graph_replay::reconcile_graph_replay_releases(
        lease,
        &store_root,
        observations,
        cancellation,
    )
    .await
    {
        graph_replay::ReconcileOutcome::Complete | graph_replay::ReconcileOutcome::Retained => true,
        graph_replay::ReconcileOutcome::MoreWork => {
            release_backlog_remains = true;
            true
        }
        // A deferred or failed attempt must not be repeated by the
        // post-collection reconcile below: the graph runtime already proved
        // it cannot serve this tick.
        graph_replay::ReconcileOutcome::Deferred | graph_replay::ReconcileOutcome::Failed => {
            replay_reconcile_failed = true;
            false
        }
    };
    // A superseded generation still named by the serving or text slot, or a
    // text replacement still building, is released by a seat or descriptor
    // publication that does not wake maintenance. Without the short cadence
    // its bytes wait for the next full interval (a day by default).
    let awaits_transient_release = plan.awaits_transient_release(&protection.serving);
    if !plan.has_collectable_work() {
        return if replay_reconcile_failed {
            CodeGenerationRetentionOutcomeV1::Failed
        } else if release_backlog_remains || awaits_transient_release {
            CodeGenerationRetentionOutcomeV1::MoreWork
        } else {
            CodeGenerationRetentionOutcomeV1::Complete
        };
    }
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded(observations, "retention_cancelled");
        return CodeGenerationRetentionOutcomeV1::Failed;
    }
    let report = match execute_collection(
        &store_root,
        plan,
        &graph_replay_pool_root,
        lease.project_root(),
        observations,
        cancellation,
    )
    .await
    {
        Ok(report) => report,
        Err(outcome) => return outcome,
    };
    // The just-collected batch queued fresh release evidence; offer it to the
    // graph immediately, but only when this tick's earlier reconcile was
    // actually served. A deferred or failed runtime must not be probed twice
    // in one tick.
    let mut release_reconcile_failed = replay_reconcile_failed;
    if replay_reconcile_attemptable {
        match graph_replay::reconcile_graph_replay_releases(
            lease,
            &store_root,
            observations,
            cancellation,
        )
        .await
        {
            graph_replay::ReconcileOutcome::Complete | graph_replay::ReconcileOutcome::Retained => {
            }
            graph_replay::ReconcileOutcome::MoreWork => {
                release_backlog_remains = true;
            }
            graph_replay::ReconcileOutcome::Deferred | graph_replay::ReconcileOutcome::Failed => {
                release_reconcile_failed = true;
            }
        }
    }
    if release_reconcile_failed {
        CodeGenerationRetentionOutcomeV1::Failed
    } else if release_backlog_remains || awaits_transient_release || collection_left_work(&report) {
        CodeGenerationRetentionOutcomeV1::MoreWork
    } else {
        CodeGenerationRetentionOutcomeV1::Complete
    }
}

/// The checkout an unmounted scope belongs to, or `None` when the scope is
/// mounted, no record proves its root (only the primary scope's root is known
/// without one), or the recorded checkout is gone.
fn unmounted_scope_root(
    store: &RegisteredProjectStoreV1,
    scope: &CodeIndexScopeV1,
    mounted_store_roots: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    if mounted_store_roots.contains(&scope.store_root) {
        return None;
    }
    let root = scope.recorded_root.clone().or_else(|| {
        (scope.store_root == code_index_store_root(&store.data_root, &store.canonical_root))
            .then(|| store.canonical_root.clone())
    })?;
    root.exists().then_some(root)
}

/// One registered project's profile shard, for a retention pass that runs
/// whether or not any of its worktrees is mounted in this daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredProjectStoreV1 {
    /// `projects/<project id>/` under the profile root.
    pub data_root: PathBuf,
    /// The registry's canonical root: the primary checkout.
    pub canonical_root: PathBuf,
}

/// Collect superseded generations in every scope of a registered project that
/// no mounted graph owns.
///
/// Mounted scopes are skipped: their pass runs with the mounted graph lease.
/// An unmounted scope has no graph lease, so the graph-replay release queue
/// the batch appends to waits for the project's next mount; collection is
/// still exact, because retention hard-links each retired generation into the
/// replay pool before its release event is durable, so the graph can always
/// finish that retirement later. Text artifacts, the largest superseded
/// family, are reclaimed by the batch or, once its index rewrite frees them,
/// by the continuation pass that follows.
///
/// The protection set is resolved per scope at pass time, so a scope mounted
/// after the tick selected it still protects what its scheduler serves, and
/// the executor's pointer compare-and-swap refuses a batch a concurrent
/// publication invalidated. A scope whose root no record proves, or whose
/// recorded checkout is gone, is left to stranded-scope reconciliation.
#[hotpath::measure(
    label = "daemon.git.maintenance.registered_code_generation_retention",
    future = true
)]
pub async fn run_registered_code_generation_retention(
    store: &RegisteredProjectStoreV1,
    mounted_store_roots: &BTreeSet<PathBuf>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    profile_database: &RegisteredGlobalDb,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
) -> CodeGenerationRetentionOutcomeV1 {
    let scopes = match code_index_scopes(&store.data_root) {
        Ok(scopes) => scopes,
        Err(error) => {
            observations.mark_loud_retention_log();
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "code_generations".to_string()),
                    ("failure", "scope_inventory_unavailable".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let graph_replay_pool_root = store
        .data_root
        .join(tracedecay_runtime_core::config::DB_FILENAME)
        .with_extension("graph-replay");
    let mut outcome = CodeGenerationRetentionOutcomeV1::Complete;
    for scope in scopes {
        if cancellation.is_cancelled() {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
        let Some(root) = unmounted_scope_root(store, &scope, mounted_store_roots) else {
            continue;
        };
        let protection = match code_generation_protection(schedulers, profile_database, &root).await
        {
            Ok(protection) => protection,
            Err(unavailable) => {
                log_code_generation_retention_degraded(observations, unavailable.reason());
                outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::Failed);
                continue;
            }
        };
        if graph_replay::replay_pool_is_held(&graph_replay_pool_root) {
            return outcome.with_pass(defer_graph_replay_pool_busy(observations, &root));
        }
        let plan = match plan_collection(
            &scope.store_root,
            &protection.sources,
            &graph_replay_pool_root,
            &root,
            observations,
            cancellation,
        )
        .await
        {
            Ok(plan) => plan,
            Err(failed) => {
                outcome = outcome.with_pass(failed);
                continue;
            }
        };
        if !plan.has_collectable_work() {
            continue;
        }
        match execute_collection(
            &scope.store_root,
            plan,
            &graph_replay_pool_root,
            &root,
            observations,
            cancellation,
        )
        .await
        {
            Ok(report) if collection_left_work(&report) => {
                outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::MoreWork);
            }
            Ok(_) => {}
            Err(failed) => outcome = outcome.with_pass(failed),
        }
    }
    outcome
}

/// The generation the mounted scheduler is currently serving, when one is
/// mounted at all.
#[hotpath::measure(
    label = "daemon.git.maintenance.serving_generation_pins",
    future = true
)]
async fn serving_generation_pins(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> std::collections::BTreeSet<tracedecay_domain::CodeGenerationId> {
    let mut pins = std::collections::BTreeSet::new();
    if let Some(scope) = schedulers.serving_code_scope(project_root).await {
        if let Some(serving) = scope.serving_generation {
            pins.insert(serving.manifest().generation_id.clone());
        }
        // The seated graph outlives both serving slots across a refresh: the
        // successor's graph builds over its sealed store, so collecting it
        // first turns an incremental publication into a full rebuild.
        pins.extend(scope.graph_generation);
    }
    // A clean restart whose retained revision-7 head recovered serves through
    // the text projection and never seats a second copy of its sealed
    // generation, so the sealed slot alone under-reports what is live. Pin
    // the level that actually serves or retention collects it out from under
    // the route.
    if let Some(text) = schedulers.latest_text_serving_for_root(project_root).await {
        pins.insert(text.metadata().manifest().generation_id.clone());
    }
    pins
}
