use std::path::Path;

use super::{TraceDecay, log_daemon_event};
use tracedecay_code_index_retention::code_index_generations::{
    code_generation_graph_replay_release_page, complete_code_generation_graph_replay_release,
    try_acquire_code_generation_store_lock,
};

pub(super) enum ReconcileOutcome {
    /// Every queued release event has been consumed.
    Complete,
    /// The bounded page was served and more queued release events remain; the
    /// caller should keep the short cadence instead of parking the backlog
    /// behind the full maintenance interval.
    MoreWork,
    /// At least one release stays queued because the graph still needs its
    /// replay; nothing failed.
    Retained,
    /// The attempt was skipped without touching the graph runtime because the
    /// consecutive-failure backoff window is open. Durable release evidence
    /// keeps the work safe for a later attempt.
    Deferred,
    Failed,
}

/// Removes the retired generation's sealed read bundle files from the
/// durable generations root. Idempotent; an absent bundle is a success.
fn retire_generation_read_bundle(store_root: &Path, generation_file: &str) -> Result<(), String> {
    let digest = generation_file
        .strip_prefix("generation-")
        .and_then(|value| value.strip_suffix(".json"))
        .ok_or_else(|| "sealed generation filename is invalid".to_owned())?;
    let sealed = tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest}"))
        .map_err(|error| error.to_string())?;
    tracedecay_graph_db::retire_sealed_read_bundle(&store_root.join("code-generations-v1"), &sealed)
        .map_err(|error| error.to_string())
}

pub(super) fn log_code_generation_retention_degraded(
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    project_root: &Path,
    failure: &str,
) {
    observations.emit_retention_degraded(project_root, "code_generations", failure);
}

/// Shared deferral for a held graph-replay pool: the outer probe and the
/// collection executor's typed busy result arm the same backoff and must
/// not keep the daemon writer gate.
pub(super) fn defer_graph_replay_pool_busy(
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    project_root: &Path,
) -> super::CodeGenerationRetentionOutcomeV1 {
    observations.record_graph_replay_release_unhealthy(project_root);
    hotpath::gauge!("daemon.git.maintenance.replay_pool_busy_total").inc(1_u64);
    log_code_generation_retention_degraded(observations, project_root, "graph_replay_pool_busy");
    super::CodeGenerationRetentionOutcomeV1::Failed
}

/// The degraded event with the typed error attached. A bare failure label
/// proved undiagnosable in production: `graph_replay_release_failed` recurred
/// on every retention tick with no way to tell an unregistered graph shard
/// from a pool-lock deadline from a conflict.
fn log_code_generation_retention_degraded_with_error(
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    failure: &str,
    error: &dyn std::fmt::Debug,
) {
    observations.mark_loud_retention_log();
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_generations".to_string()),
            ("failure", failure.to_string()),
            ("error", format!("{error:?}")),
        ],
    );
}

/// Whether a release failure names a graph runtime that cannot serve right
/// now — the class worth backing off from — as opposed to evidence or store
/// defects (conflict, corruption, invalid identity) that must stay loud on
/// every attempt until someone fixes them.
fn release_failure_is_runtime_unhealthy(error: &tracedecay_graph_db::GraphDbError) -> bool {
    matches!(
        error,
        tracedecay_graph_db::GraphDbError::DeadlineExceeded
            | tracedecay_graph_db::GraphDbError::Unavailable { .. }
            | tracedecay_graph_db::GraphDbError::BudgetExhausted { .. }
    )
}

/// Non-blocking replay-pool probe. The retention pass used to discover a held
/// pool lock the expensive way: the release reconcile polled it at
/// five-millisecond intervals for the full 30s graph-operation deadline and
/// then failed with `DeadlineExceeded` — every tick, for as long as the
/// holder (typically a publisher hashing a multi-GiB seal under the lock)
/// stayed wedged — and the collection executor behind it would park on the
/// same lock's *deadline-free* blocking flock. One `try_lock` answers the
/// same question for the cost of a syscall, before any full-digest planning
/// is paid. The probe lock is dropped immediately; later acquisitions re-take
/// it under a checked, budget-capped wait so a publisher that wins the
/// probe-to-execute window defers with `GraphReplayPoolBusy` instead of
/// pinning the daemon writer gate.
#[hotpath::measure(label = "daemon.git.maintenance.replay_pool_probe")]
pub(super) fn replay_pool_is_held(replay_pool_root: &Path) -> bool {
    if !replay_pool_root.is_dir() {
        return false;
    }
    match try_acquire_code_generation_store_lock(replay_pool_root) {
        Ok(Some(probe)) => {
            drop(probe);
            false
        }
        Ok(None) => true,
        // An unreadable pool is not "held": let the acquiring operations
        // report the typed storage failure instead of classifying it as busy.
        Err(_) => false,
    }
}

#[hotpath::measure(label = "daemon.git.maintenance.graph_replay_release", future = true)]
pub(super) async fn reconcile_graph_replay_releases(
    graph: &TraceDecay,
    store_root: &Path,
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
) -> ReconcileOutcome {
    let Some(project_id) = graph.hook_store_layout().identity.project_id.as_ref() else {
        log_code_generation_retention_degraded(
            observations,
            graph.project_root(),
            "graph_replay_project_identity_unavailable",
        );
        return ReconcileOutcome::Failed;
    };
    let project_id = match tracedecay_domain::ProjectId::new(project_id.clone()) {
        Ok(project_id) => project_id,
        Err(_) => {
            log_code_generation_retention_degraded(
                observations,
                graph.project_root(),
                "graph_replay_project_identity_invalid",
            );
            return ReconcileOutcome::Failed;
        }
    };
    let project_root = graph.project_root();
    // A runtime that answered its last attempts with deadline or
    // unavailability failures is skipped for the bounded backoff window
    // instead of being polled — and timed out against — on every tick. The
    // arming failure was already reported; skips stay quiet on the log and
    // visible on the gauge.
    if !observations.graph_replay_release_attempt_admitted(project_root) {
        hotpath::gauge!("daemon.git.maintenance.replay_release_deferred_total").inc(1_u64);
        return ReconcileOutcome::Deferred;
    }
    let staging_cursor = observations.graph_staging_release_cursor(project_root);
    let staging_release = graph
        .store_runtime_registry()
        .release_one_sealed_generation_staging_rows(
            project_id.clone(),
            graph.db(),
            cancellation,
            staging_cursor,
        )
        .await;
    match staging_release {
        Ok(continuation) => {
            observations.record_graph_staging_release_cursor(project_root, continuation);
        }
        Err(error) => {
            log_code_generation_retention_degraded_with_error(
                observations,
                "graph_staging_release_failed",
                &error,
            );
        }
    }
    // One bounded page per pass, resumed from the durable-queue cursor of the
    // previous attempt. Retained releases fall behind the cursor instead of
    // blocking every later page, and a backlog drains across short-cadence
    // ticks instead of monopolizing one tick (and the daemon writer gate) for
    // the whole queue.
    let after = observations.graph_replay_release_cursor(project_root);
    let page = match code_generation_graph_replay_release_page(store_root, after.as_deref()) {
        Ok(page) => page,
        Err(error) => {
            log_code_generation_retention_degraded_with_error(
                observations,
                "graph_replay_release_evidence_invalid",
                &error,
            );
            return ReconcileOutcome::Failed;
        }
    };
    if page.releases.is_empty() {
        observations.record_graph_replay_release_served(project_root, page.continuation);
        return ReconcileOutcome::Complete;
    }
    let mut retained = false;
    for release in page.releases {
        if cancellation.is_cancelled() {
            return ReconcileOutcome::Failed;
        }
        match graph
            .store_runtime_registry()
            .reconcile_deleted_code_generation_graph_replays(
                project_id.clone(),
                graph.db(),
                &release.generation.generation_id,
                &release.generation.generation_file,
                cancellation,
            )
            .await
        {
            Ok(true) => {
                // The generation's graph replay is retired; its sealed
                // read bundle (derived read artifacts) retires with it.
                // Runs before the release checkpoint so a crash here
                // retries the idempotent sweep on the next pass.
                if let Err(error) =
                    retire_generation_read_bundle(store_root, &release.generation.generation_file)
                {
                    observations.mark_loud_retention_log();
                    log_daemon_event(
                        "retention_degraded",
                        &[
                            ("pass", "code_generations".to_string()),
                            ("failure", "graph_read_bundle_retire_failed".to_string()),
                            ("error", error),
                        ],
                    );
                    return ReconcileOutcome::Failed;
                }
                if complete_code_generation_graph_replay_release(store_root, &release).is_err() {
                    log_code_generation_retention_degraded(
                        observations,
                        project_root,
                        "graph_replay_release_checkpoint_failed",
                    );
                    return ReconcileOutcome::Failed;
                }
            }
            Ok(false) => retained = true,
            Err(error) => {
                if release_failure_is_runtime_unhealthy(&error) {
                    observations.record_graph_replay_release_unhealthy(project_root);
                }
                log_code_generation_retention_degraded_with_error(
                    observations,
                    "graph_replay_release_failed",
                    &error,
                );
                return ReconcileOutcome::Failed;
            }
        }
    }
    let more_work = page.continuation.is_some();
    observations.record_graph_replay_release_served(project_root, page.continuation);
    if more_work {
        ReconcileOutcome::MoreWork
    } else if retained {
        ReconcileOutcome::Retained
    } else {
        ReconcileOutcome::Complete
    }
}
