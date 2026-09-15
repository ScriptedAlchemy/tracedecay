//! Retention, compaction, and garbage-collection operations run by the daemon
//! maintenance owner.
//!
//! Every operation that opens or garbage-collects a store lives here so its
//! [`StoreAdministration`] lifetime is kept separate from the watcher state
//! machine. The git watcher itself never opens or mutates a store: it routes
//! exact-frontier freshness requests to the code-index scheduler and wakes the
//! maintenance owner.

use std::path::Path;

use crate::lease::ProjectStoreMaintenanceLeaseV1;
use crate::telemetry::StoreTelemetrySamplingRegistry;
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;
use tracedecay_runtime_core::logging::log_daemon_event;

mod graph_replay;
use graph_replay::{defer_graph_replay_pool_busy, log_code_generation_retention_degraded};

/// Outcome of one bounded code-generation retention pass.
///
/// `MoreWork` reports bounded progress with a remaining backlog — another
/// collectable superseded generation, or unconsumed graph-replay release
/// evidence — so the maintenance owner keeps the short cadence until the
/// store converges instead of parking multi-GiB debris behind the full
/// maintenance interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationRetentionOutcomeV1 {
    Complete,
    MoreWork,
    Failed,
}

/// Collect superseded code-index generations for one mounted project.
///
/// Sealed generations are ordinary files, so no database retention or
/// compaction pass reclaims them; this runs on the ordinary maintenance
/// cadence. The planner marks only the active pointer head itself. The pass
/// adds the generation the mounted scheduler is serving and every live
/// native-preview candidate binding, because both can bind a generation the
/// active pointer alone cannot name. A root that yields no repository
/// identity or an unreadable binding inventory fails the pass closed instead
/// of sweeping blind.
#[hotpath::measure(
    label = "daemon.git.maintenance.code_generation_retention",
    future = true
)]
pub async fn run_code_generation_retention(
    lease: &ProjectStoreMaintenanceLeaseV1,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
) -> CodeGenerationRetentionOutcomeV1 {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1,
        execute_code_generation_retention_cancellable,
        prepare_next_code_generation_retention_cancellable,
    };
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded(observations, "retention_cancelled");
        return CodeGenerationRetentionOutcomeV1::Failed;
    }
    let layout = lease.store_layout();
    let store_root = tracedecay_code_index_retention::code_index_generations::code_index_store_root(
        &layout.data_root,
        &layout.project_root,
    );
    // A store directory that never materialized has nothing to sweep. A store
    // *without* an active pointer is different: it is crash debris from a
    // publish that never reached its pointer write (an OOM-killed rebuild is
    // the ordinary cause), and the planner collects it as a typed unpublished
    // store — before this, such orphaned partial generations were unreachable
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
    let mut protected_sources = serving_generation_pins(schedulers, &layout.project_root).await;
    // Native previews bind retained-only candidate generations between
    // preflight and terminal apply. Their durable commitments are liveness
    // roots; omitting them lets an ordinary maintenance tick collect the exact
    // evidence apply must reopen. The bindings are keyed by repository, which
    // is a pure function of the checkout's git common dir: a project whose
    // worktree is not mounted in this daemon — the inactive project with the
    // largest backlog — still resolves it, so retention neither collects blind
    // nor fails every tick. Only a root that cannot yield an identity at all
    // stays fail-closed.
    let repository_id = match schedulers.serving_code_scope(&layout.project_root).await {
        Some(serving_scope) => serving_scope.repository_id,
        None => {
            match tracedecay_code_index_runtime::code_index_scheduler::identity::repository_id_for(
                &layout.project_root,
            ) {
                Ok(repository_id) => repository_id,
                Err(_) => return CodeGenerationRetentionOutcomeV1::Failed,
            }
        }
    };
    let native_store = tracedecay_global_db::GlobalDbNativeIntegrationStore::new(
        lease.profile_database().as_ref(),
    );
    let native_pins = match native_store
        .live_candidate_generation_bindings(
            &repository_id,
            tracedecay_contracts::clock::now_micros(),
        )
        .await
    {
        Ok(pins) => pins,
        Err(_) => return CodeGenerationRetentionOutcomeV1::Failed,
    };
    protected_sources.extend(native_pins);
    // A held replay pool makes every later phase of this pass fail closed:
    // the release reconcile's pool acquisition would burn its whole
    // graph-operation deadline discovering the holder (the live wedge logged
    // that as `graph_replay_release_failed error=DeadlineExceeded` on every
    // tick), and the collection executor would then contend for the same
    // lock while holding the daemon writer gate. One non-blocking probe
    // defers the pass for this tick instead — before the multi-GiB
    // full-digest planning below is paid — and the executor's own checked
    // acquire returns `GraphReplayPoolBusy` if a publisher wins the
    // probe-to-execute window, so the writer gate is never pinned on a
    // blocking flock. Both paths arm the same bounded release backoff.
    if graph_replay::replay_pool_is_held(&graph_replay_pool_root) {
        return defer_graph_replay_pool_busy(observations, lease.project_root());
    }
    // Full digest verification routinely reads several GiB. Run it before
    // entering the graph transaction and preserve the daemon shutdown token
    // through the blocking boundary; the planner checks it after every bounded
    // read chunk and creates no journal before verification completes.
    let plan_root = store_root.clone();
    let plan_sources = protected_sources.clone();
    let plan_cancellation = cancellation.clone();
    let plan_pool_root = graph_replay_pool_root.clone();
    let plan = tokio::task::spawn_blocking(move || {
        prepare_next_code_generation_retention_cancellable(
            &plan_root,
            &plan_sources,
            &|| plan_cancellation.is_cancelled(),
            Some(&plan_pool_root),
        )
    })
    .await;
    let plan = match plan {
        Ok(Ok(plan)) => plan,
        Ok(Err(
            tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionErrorV1::Cancelled,
        )) => {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
        Ok(Err(
            tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionErrorV1::GraphReplayPoolBusy,
        )) => {
            return defer_graph_replay_pool_busy(observations, lease.project_root());
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
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
        Err(_) => {
            log_code_generation_retention_degraded(observations, "retention_task_panicked");
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    // A failed, deferred, or retained replay reconcile keeps its durable
    // release evidence for a later graph-available pass. Deleting newly
    // planned files stays safe — retention hard-links each retired generation
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
    if !plan.has_collectable_work() {
        return if replay_reconcile_failed {
            CodeGenerationRetentionOutcomeV1::Failed
        } else if release_backlog_remains {
            CodeGenerationRetentionOutcomeV1::MoreWork
        } else {
            CodeGenerationRetentionOutcomeV1::Complete
        };
    }
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded(observations, "retention_cancelled");
        return CodeGenerationRetentionOutcomeV1::Failed;
    }
    // `current_timestamp()` counts seconds; wrapping it in `UtcMicros` stamped
    // every deletion receipt with a seconds value in a micros-typed field
    // (live receipts read as 1970). The receipt is durable journal evidence,
    // so it takes the canonical micros clock.
    let completed_at = tracedecay_contracts::clock::now_micros();
    let execution_root = store_root.clone();
    let execution_pool_root = graph_replay_pool_root.clone();
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
            // The just-collected generation queued fresh release evidence;
            // offer it to the graph immediately — but only when this tick's
            // earlier reconcile was actually served. A deferred or failed
            // runtime must not be probed twice in one tick.
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
                    graph_replay::ReconcileOutcome::Complete
                    | graph_replay::ReconcileOutcome::Retained => {}
                    graph_replay::ReconcileOutcome::MoreWork => {
                        release_backlog_remains = true;
                    }
                    graph_replay::ReconcileOutcome::Deferred
                    | graph_replay::ReconcileOutcome::Failed => {
                        release_reconcile_failed = true;
                    }
                }
            }
            if release_reconcile_failed {
                CodeGenerationRetentionOutcomeV1::Failed
            } else if release_backlog_remains
                || report.generation_segment_batch_exhausted
                || !report.deleted_generations.is_empty()
                || !report.deleted_text_artifacts.is_empty()
            {
                // Something was collected, so the next bounded census may find
                // another collectable unit; stay on the short cadence until a
                // pass proves the store converged. A census that finds nothing
                // returns Complete one tick later at metadata cost only.
                CodeGenerationRetentionOutcomeV1::MoreWork
            } else {
                CodeGenerationRetentionOutcomeV1::Complete
            }
        }
        Ok(Err(CodeGenerationRetentionErrorV1::Cancelled)) => {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            CodeGenerationRetentionOutcomeV1::Failed
        }
        Ok(Err(CodeGenerationRetentionErrorV1::GraphReplayPoolBusy)) => {
            defer_graph_replay_pool_busy(observations, lease.project_root())
        }
        Ok(Err(error)) => {
            // Same diagnosability contract as the plan failure above: the
            // apply step's typed error names the exact refusal (CAS loss,
            // unsafe state, storage) instead of a bare retry label.
            observations.mark_loud_retention_log();
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "code_generations".to_string()),
                    ("failure", "retention_pass_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            CodeGenerationRetentionOutcomeV1::Failed
        }
        Err(_) => {
            log_code_generation_retention_degraded(observations, "retention_task_panicked");
            CodeGenerationRetentionOutcomeV1::Failed
        }
    }
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
    if let Some(scope) = schedulers.serving_code_scope(project_root).await
        && let Some(serving) = scope.serving_generation
    {
        pins.insert(serving.manifest().generation_id.clone());
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

/// Runs bounded incremental-vacuum compaction over every tracked branch
/// database other than the one `cg` currently has mounted (the maintenance
/// owner compacts that store through its live-runtime authority). Best-effort
/// and independent per file: a busy or failing branch database never blocks
/// the rest, but keeps the maintenance cadence retry-eligible — see
/// `src/retention/branch_compaction.rs` for the compaction policy itself.
#[hotpath::measure(label = "daemon.git.maintenance.branch_compaction")]
pub fn run_branch_compaction(
    lease: &ProjectStoreMaintenanceLeaseV1,
    config: &CompactionThresholdConfig,
) -> bool {
    let layout = lease.store_layout();
    let Some(meta) = tracedecay_runtime_core::branch_meta::load_branch_meta(&layout.data_root)
    else {
        return true;
    };
    let active_db_path = layout.graph_db_path.clone();
    let candidates = crate::retention::branch_compaction::select_branch_db_candidates(
        &layout.data_root,
        &meta,
        &active_db_path,
    );
    if candidates.is_empty() {
        return true;
    }
    let report = crate::retention::branch_compaction::compact_branch_databases(&candidates, config);
    if report.policy_invalid {
        // Never silent: an out-of-range threshold disables the pass entirely
        // and would otherwise be indistinguishable from "nothing to compact".
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "branch_compaction".to_string()),
                ("failure", "invalid_compaction_policy".to_string()),
                (
                    "free_page_ratio_threshold",
                    config.free_page_ratio_threshold.to_string(),
                ),
            ],
        );
        return false;
    }
    if report.compacted.is_empty() && report.skipped.is_empty() {
        return true;
    }
    let freed_pages: u64 = report
        .compacted
        .iter()
        .map(|outcome| outcome.freed_pages)
        .sum();
    let unreclaimable = report
        .skipped
        .iter()
        .filter(|skip| {
            skip.reason
                == crate::retention::branch_compaction::BranchCompactionSkipReason::IncrementalVacuumUnavailable
        })
        .count();
    log_daemon_event(
        "retention_branch_compaction",
        &[
            ("project", lease.project_root().display().to_string()),
            ("compacted", report.compacted.len().to_string()),
            ("freed_pages", freed_pages.to_string()),
            ("skipped", report.skipped.len().to_string()),
            // Branch databases predating `auto_vacuum = INCREMENTAL`: their
            // free pages need a full VACUUM this pass deliberately avoids.
            ("unreclaimable", unreclaimable.to_string()),
        ],
    );
    branch_compaction_succeeded(&report)
}

pub fn branch_compaction_succeeded(
    report: &crate::retention::branch_compaction::BranchCompactionReport,
) -> bool {
    !report.policy_invalid && report.skipped.is_empty()
}
