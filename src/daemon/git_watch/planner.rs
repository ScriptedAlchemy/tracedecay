use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::branch::BranchAddOutcome;

use super::generation::{
    GenerationError, GenerationGate, GenerationReservation, ReservationError, SYNC_RETRY_INITIAL,
    SnapshotGeneration, snapshot_generation,
};
use super::{
    DirtyPlan, GitWatcherInner, IN_FLIGHT_RETRY_DELAY, TraceDecay, WatchState, log_daemon_event,
    retained_project_graph, store_maintenance,
};

pub(super) async fn execute_plan(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    common: &Path,
    plan: DirtyPlan,
) {
    if plan.worktree_removed || plan.reconcile_metadata {
        state.prune_missing_roots().await;
    }
    let retry_plan = plan.clone();
    let roots = state.roots().await;
    let owner_graph = first_retained_project_graph(inner, &roots).await;

    let mut worktrees = plan.new_worktrees;
    if plan.reconcile_metadata {
        worktrees.extend(store_maintenance::linked_worktree_names(common));
    }
    for name in &worktrees {
        let Some((wt_root, branch)) = store_maintenance::resolve_worktree(common, name) else {
            continue;
        };
        state.register_snapshot_root(&wt_root).await;
        let generation = match snapshot_generation(&wt_root) {
            Ok(generation) => generation,
            Err(error) => {
                defer_generation_error(
                    state,
                    &state.worktree_gates,
                    &wt_root,
                    error,
                    retry_plan.clone(),
                )
                .await;
                continue;
            }
        };
        let reservation = match reserve_generation(&state.worktree_gates, &generation).await {
            Ok(reservation) => reservation,
            Err(ReservationError::Unchanged) => {
                continue;
            }
            Err(error) => {
                defer_plan(state, retry_plan.clone(), error).await;
                continue;
            }
        };

        let _permit = inner.sync_semaphore.acquire().await;
        match generation_still_current(&state.worktree_gates, &generation, &wt_root).await {
            Ok(true) => {}
            Ok(false) => {
                state
                    .schedule_retry(retry_plan.clone(), IN_FLIGHT_RETRY_DELAY)
                    .await;
                drop(reservation);
                continue;
            }
            Err(error) => {
                let retry_after =
                    record_generation_error(&state.worktree_gates, &wt_root, &error).await;
                state.schedule_retry(retry_plan.clone(), retry_after).await;
                log_generation_failure(&wt_root, &error);
                drop(reservation);
                continue;
            }
        }
        let outcome = match owner_graph.as_deref() {
            Some(graph) => {
                store_maintenance::track_worktree_branch(
                    &inner.administration,
                    graph,
                    wt_root.clone(),
                    branch.clone(),
                )
                .await
            }
            None => None,
        };
        match outcome {
            Some(outcome) if worktree_tracking_succeeded(&outcome) => {
                match record_success_if_current(&state.worktree_gates, generation.clone(), &wt_root)
                    .await
                {
                    Ok(true) => {
                        log_daemon_event(
                            "git_watch_synced",
                            &[
                                ("project", state.project_root.display().to_string()),
                                ("action", "worktree_tracked".to_string()),
                                ("worktree", wt_root.display().to_string()),
                                ("branch", branch),
                                ("outcome", format!("{outcome:?}")),
                            ],
                        );
                    }
                    Ok(false) => {
                        state
                            .schedule_retry(retry_plan.clone(), IN_FLIGHT_RETRY_DELAY)
                            .await;
                    }
                    Err(error) => {
                        let retry_after =
                            record_generation_error(&state.worktree_gates, &wt_root, &error).await;
                        state.schedule_retry(retry_plan.clone(), retry_after).await;
                        log_generation_failure(&wt_root, &error);
                    }
                }
            }
            Some(BranchAddOutcome::Deferred) => {
                let retry_after =
                    record_generation_failure(&state.worktree_gates, generation).await;
                state.schedule_retry(retry_plan.clone(), retry_after).await;
                log_daemon_event(
                    "git_watch_degraded",
                    &[
                        ("project", state.project_root.display().to_string()),
                        ("reason", "worktree_track_deferred".to_string()),
                    ],
                );
            }
            Some(_) | None => {
                let retry_after =
                    record_generation_failure(&state.worktree_gates, generation).await;
                state.schedule_retry(retry_plan.clone(), retry_after).await;
                log_daemon_event(
                    "git_watch_degraded",
                    &[
                        ("project", state.project_root.display().to_string()),
                        ("reason", "worktree_track_failed".to_string()),
                    ],
                );
            }
        }
        drop(reservation);
    }

    if plan.dirty || !plan.branches.is_empty() {
        for root in &roots {
            sync_snapshot(inner, state, root, "incremental").await;
        }
    }

    if plan.gc_eligible || plan.reconcile_metadata {
        let mut gc_retry = false;
        for root in &roots {
            if let Some(graph) = retained_project_graph(inner, root).await {
                gc_retry |= !store_maintenance::run_gc(inner, &graph).await;
            } else {
                gc_retry = true;
            }
        }
        if gc_retry {
            state
                .schedule_retry(DirtyPlan::gc(), SYNC_RETRY_INITIAL)
                .await;
        }
    }
}

async fn first_retained_project_graph(
    inner: &GitWatcherInner,
    roots: &[PathBuf],
) -> Option<Arc<TraceDecay>> {
    for root in roots {
        if let Some(graph) = retained_project_graph(inner, root).await {
            return Some(graph);
        }
    }
    None
}

async fn reserve_generation(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: &SnapshotGeneration,
) -> Result<GenerationReservation, ReservationError> {
    gates
        .lock()
        .await
        .entry(generation.root.clone())
        .or_default()
        .reserve(generation, Instant::now())
}

async fn record_generation_failure(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: SnapshotGeneration,
) -> Duration {
    let root = generation.root.clone();
    gates
        .lock()
        .await
        .entry(root)
        .or_default()
        .record_failure(generation, Instant::now())
}

async fn record_success_if_current(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: SnapshotGeneration,
    root: &Path,
) -> Result<bool, GenerationError> {
    let observed = snapshot_generation(root)?;
    Ok(gates
        .lock()
        .await
        .entry(generation.root.clone())
        .or_default()
        .record_success_if_current(generation, &observed))
}

async fn generation_still_current(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: &SnapshotGeneration,
    root: &Path,
) -> Result<bool, GenerationError> {
    let observed = snapshot_generation(root)?;
    Ok(!gates
        .lock()
        .await
        .entry(generation.root.clone())
        .or_default()
        .release_if_stale(generation, &observed))
}

async fn record_generation_error(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    root: &Path,
    error: &GenerationError,
) -> Duration {
    record_generation_failure(gates, SnapshotGeneration::unavailable(root, error.kind)).await
}

fn retry_delay(error: ReservationError) -> Duration {
    match error {
        ReservationError::InFlight => IN_FLIGHT_RETRY_DELAY,
        ReservationError::Backoff { remaining } => remaining,
        ReservationError::Unchanged => IN_FLIGHT_RETRY_DELAY,
    }
}

async fn defer_plan(state: &WatchState, plan: DirtyPlan, error: ReservationError) {
    if error == ReservationError::Unchanged {
        return;
    }
    state.schedule_retry(plan, retry_delay(error)).await;
}

pub(super) fn worktree_tracking_succeeded(outcome: &BranchAddOutcome) -> bool {
    !matches!(outcome, BranchAddOutcome::Deferred)
}

fn log_generation_failure(root: &Path, error: &GenerationError) {
    log_daemon_event(
        "git_watch_degraded",
        &[
            ("project", root.display().to_string()),
            ("reason", error.kind.as_str().to_string()),
            ("error", error.to_string()),
        ],
    );
}

async fn defer_generation_error(
    state: &WatchState,
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    root: &Path,
    error: GenerationError,
    plan: DirtyPlan,
) {
    let unavailable = SnapshotGeneration::unavailable(root, error.kind);
    match reserve_generation(gates, &unavailable).await {
        Ok(reservation) => {
            let retry_after = record_generation_failure(gates, unavailable).await;
            state.schedule_retry(plan, retry_after).await;
            log_generation_failure(root, &error);
            drop(reservation);
        }
        Err(ReservationError::Unchanged) => {}
        Err(error) => {
            defer_plan(state, plan, error).await;
        }
    }
}

pub(super) async fn sync_snapshot(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    root: &Path,
    action: &'static str,
) {
    let generation = match snapshot_generation(root) {
        Ok(generation) => generation,
        Err(error) => {
            defer_generation_error(state, &state.sync_gates, root, error, DirtyPlan::sync()).await;
            return;
        }
    };

    let reservation = match reserve_generation(&state.sync_gates, &generation).await {
        Ok(reservation) => reservation,
        Err(error) => {
            defer_plan(state, DirtyPlan::sync(), error).await;
            return;
        }
    };

    let Some(graph) = retained_project_graph(inner, root).await else {
        let retry_after = record_generation_failure(&state.sync_gates, generation).await;
        log_daemon_event(
            "git_watch_degraded",
            &[
                ("project", root.display().to_string()),
                ("reason", "project_graph_unavailable".to_string()),
            ],
        );
        state.schedule_retry(DirtyPlan::sync(), retry_after).await;
        drop(reservation);
        return;
    };

    let _permit = inner.sync_semaphore.acquire().await;
    match generation_still_current(&state.sync_gates, &generation, root).await {
        Ok(true) => {}
        Ok(false) => {
            state
                .schedule_retry(DirtyPlan::sync(), IN_FLIGHT_RETRY_DELAY)
                .await;
            drop(reservation);
            return;
        }
        Err(error) => {
            let retry_after = record_generation_error(&state.sync_gates, root, &error).await;
            state.schedule_retry(DirtyPlan::sync(), retry_after).await;
            log_generation_failure(root, &error);
            drop(reservation);
            return;
        }
    }
    let synced = if graph.project_root() == root {
        store_maintenance::sync_project(
            &graph,
            inner.config.full_sync_escalation_files,
            &inner.administration,
        )
        .await
    } else if let Some(branch) = crate::branch::current_branch(root) {
        matches!(
            store_maintenance::track_worktree_branch(
                &inner.administration,
                &graph,
                root.to_path_buf(),
                branch,
            )
            .await,
            Some(outcome) if worktree_tracking_succeeded(&outcome)
        )
    } else {
        false
    };
    if synced {
        match record_success_if_current(&state.sync_gates, generation.clone(), root).await {
            Ok(true) => {
                let branch = generation
                    .branch
                    .clone()
                    .unwrap_or_else(|| "detached".to_string());
                log_daemon_event(
                    "git_watch_synced",
                    &[
                        ("project", root.display().to_string()),
                        ("action", action.to_string()),
                        ("synced_branch", branch),
                    ],
                );
            }
            Ok(false) => {
                state
                    .schedule_retry(DirtyPlan::sync(), IN_FLIGHT_RETRY_DELAY)
                    .await;
            }
            Err(error) => {
                let retry_after = record_generation_error(&state.sync_gates, root, &error).await;
                state.schedule_retry(DirtyPlan::sync(), retry_after).await;
                log_generation_failure(root, &error);
            }
        }
    } else {
        let retry_after = record_generation_failure(&state.sync_gates, generation).await;
        state.schedule_retry(DirtyPlan::sync(), retry_after).await;
        log_daemon_event(
            "git_watch_degraded",
            &[
                ("project", root.display().to_string()),
                ("reason", "sync_failed".to_string()),
            ],
        );
    }
    drop(reservation);
}
