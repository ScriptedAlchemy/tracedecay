use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::Mutex;
use tokio::time::Instant;

use super::generation::{
    GenerationError, GenerationGate, GenerationReservation, ReservationError, SnapshotGeneration,
    snapshot_generation,
};
use super::{
    DirtyPlan, GitWatcherInner, TraceDecay, WatchState, log_daemon_event, retained_project_graph,
    store_maintenance,
};

pub(super) async fn execute_plan(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    common: &Path,
    plan: DirtyPlan,
) {
    if plan.worktree_removed || plan.reconcile_metadata {
        state.prune_missing_roots().await;
        state
            .worktree_gates
            .lock()
            .await
            .retain(|root, _| root.is_dir());
    }
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
        let generation = match snapshot_generation(&wt_root) {
            Ok(generation) => generation,
            Err(error) => {
                backoff_generation_error(state, &state.worktree_gates, &wt_root, error).await;
                continue;
            }
        };
        let reservation = match reserve_generation(&state.worktree_gates, &generation).await {
            Ok(reservation) => reservation,
            Err(ReservationError::Unchanged) => {
                state
                    .health
                    .worktree_generation_skips
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            Err(ReservationError::InFlight | ReservationError::Backoff) => {
                state.health.backoff_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        let _permit = inner.sync_semaphore.acquire().await;
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
        if let Some(outcome) = outcome {
            record_generation_success(&state.worktree_gates, generation).await;
            log_daemon_event(
                "git_watch_synced",
                &[
                    ("project", state.project_root.display().to_string()),
                    ("action", "worktree_tracked".to_string()),
                    ("worktree", wt_root.display().to_string()),
                    ("branch", branch),
                    ("outcome", outcome),
                ],
            );
        } else {
            record_generation_failure(&state.worktree_gates, generation).await;
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", state.project_root.display().to_string()),
                    ("reason", "worktree_track_failed".to_string()),
                ],
            );
        }
        drop(reservation);
    }

    if plan.dirty || !plan.branches.is_empty() {
        for root in &roots {
            sync_snapshot(inner, state, root, "incremental").await;
        }
    }

    if plan.gc_eligible || plan.reconcile_metadata {
        for root in &roots {
            if let Some(graph) = retained_project_graph(inner, root).await {
                store_maintenance::run_gc(inner, &graph).await;
            }
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

async fn record_generation_success(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: SnapshotGeneration,
) {
    gates
        .lock()
        .await
        .entry(generation.root.clone())
        .or_default()
        .record_success(generation);
}

async fn record_generation_failure(
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    generation: SnapshotGeneration,
) {
    let root = generation.root.clone();
    gates
        .lock()
        .await
        .entry(root)
        .or_default()
        .record_failure(generation, Instant::now());
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

async fn backoff_generation_error(
    state: &WatchState,
    gates: &Mutex<HashMap<PathBuf, GenerationGate>>,
    root: &Path,
    error: GenerationError,
) {
    let unavailable = SnapshotGeneration::unavailable(root, error.kind);
    match reserve_generation(gates, &unavailable).await {
        Ok(reservation) => {
            record_generation_failure(gates, unavailable).await;
            log_generation_failure(root, &error);
            drop(reservation);
        }
        Err(ReservationError::Unchanged) => {
            state
                .health
                .unchanged_generation_skips
                .fetch_add(1, Ordering::Relaxed);
        }
        Err(ReservationError::InFlight | ReservationError::Backoff) => {
            state.health.backoff_skips.fetch_add(1, Ordering::Relaxed);
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
            backoff_generation_error(state, &state.sync_gates, root, error).await;
            return;
        }
    };

    let reservation = match reserve_generation(&state.sync_gates, &generation).await {
        Ok(reservation) => reservation,
        Err(ReservationError::Unchanged) => {
            state
                .health
                .unchanged_generation_skips
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        Err(ReservationError::InFlight | ReservationError::Backoff) => {
            state.health.backoff_skips.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let Some(graph) = retained_project_graph(inner, root).await else {
        record_generation_failure(&state.sync_gates, generation).await;
        log_daemon_event(
            "git_watch_degraded",
            &[
                ("project", root.display().to_string()),
                ("reason", "project_graph_unavailable".to_string()),
            ],
        );
        drop(reservation);
        return;
    };

    let _permit = inner.sync_semaphore.acquire().await;
    if store_maintenance::sync_project(
        &graph,
        inner.config.full_sync_escalation_files,
        &inner.administration,
    )
    .await
    {
        let branch = generation
            .branch
            .clone()
            .unwrap_or_else(|| "detached".to_string());
        record_generation_success(&state.sync_gates, generation).await;
        state.health.mark_synced();
        log_daemon_event(
            "git_watch_synced",
            &[
                ("project", root.display().to_string()),
                ("action", action.to_string()),
                ("synced_branch", branch),
            ],
        );
    } else {
        record_generation_failure(&state.sync_gates, generation).await;
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
