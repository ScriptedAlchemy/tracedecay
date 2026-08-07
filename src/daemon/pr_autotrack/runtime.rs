use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use tracedecay_runtime_core::cancellation::CancellationToken;

use crate::daemon::branch_admin::StoreAdministration;
use crate::daemon::log_daemon_event;

use super::{
    MAX_NEW_TRACKS_PER_CYCLE, PrCommandControl, PrDiscovery, PrStoreAdministration,
    discover_open_prs_with_control, load_state, reconcile_project_with_administration,
};

/// Base cadence of the poll loop; per-project intervals are honored on top of
/// this floor via a last-run map.
const BASE_TICK: Duration = Duration::from_mins(1);

/// Retained owner for the PR-autotrack loop and every bounded child process it
/// starts. Shutdown signals the same token carried into Git/GitHub commands
/// before joining the task.
pub struct PrAutotrackTask {
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl PrAutotrackTask {
    pub async fn shutdown(self) {
        self.cancellation.cancel();
        if let Err(error) = self.task.await {
            log_daemon_event(
                "pr_autotrack",
                &[
                    ("action", "shutdown".to_string()),
                    ("outcome", "task_join_failed".to_string()),
                    ("reason", error.to_string()),
                ],
            );
        }
    }
}

/// Spawns the PR-autotrack poll loop. Cheap and inert when no registered project
/// has the feature enabled — each tick consults only daemon-published snapshots.
pub fn spawn(global_db_path: Option<PathBuf>) -> PrAutotrackTask {
    spawn_with_administration(global_db_path, StoreAdministration::default())
}

/// Spawns the PR-autotrack poll loop with the daemon's shared store coordinator.
/// The coordinator serializes PR additions and destructive branch administration
/// with every other daemon connection that owns the same store family.
pub(crate) fn spawn_with_administration(
    _global_db_path: Option<PathBuf>,
    administration: StoreAdministration,
) -> PrAutotrackTask {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        run(administration, task_cancellation).await;
    });
    PrAutotrackTask { cancellation, task }
}

async fn run(administration: StoreAdministration, cancellation: CancellationToken) {
    let Ok(database) = administration.registered_profile_database().await else {
        return;
    };
    let mut last_poll: HashMap<PathBuf, Instant> = HashMap::new();
    loop {
        if cancellation.is_cancelled() {
            return;
        }
        tick(
            database.as_ref(),
            &mut last_poll,
            &administration,
            &cancellation,
        )
        .await;
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(BASE_TICK) => {}
        }
    }
}

async fn tick(
    database: &crate::global_db::RegisteredGlobalDb,
    last_poll: &mut HashMap<PathBuf, Instant>,
    administration: &StoreAdministration,
    cancellation: &CancellationToken,
) {
    let window = 14 * 86_400;
    let cap = 64;
    let cutoff = crate::tracedecay::current_timestamp().saturating_sub(window);
    let Ok(records) = database.list_code_projects(cap).await else {
        return;
    };
    for record in records
        .into_iter()
        .filter(|record| record.last_seen_at >= cutoff)
    {
        if cancellation.is_cancelled() {
            return;
        }
        let root = PathBuf::from(&record.canonical_root);
        if !root.is_dir() {
            continue;
        }
        // A poll loop has no right to turn an arbitrary project path into
        // configuration authority. Missing/pending daemon snapshot means no
        // poll and, critically, no destructive disabled-state teardown.
        let Ok(cfg) =
            crate::config::cached_runtime_configuration_for_project_id(&root, &record.project_id)
                .map(|configuration| configuration.config.sync)
        else {
            continue;
        };
        let interval = Duration::from_secs(cfg.effective_auto_track_pr_poll_secs());
        let due = last_poll
            .get(&root)
            .is_none_or(|time| time.elapsed() >= interval);
        if !due {
            continue;
        }
        last_poll.insert(root.clone(), Instant::now());
        if cfg.auto_track_pr_branches {
            poll_project(root, administration, cancellation).await;
        } else {
            // Feature disabled: if it left managed PR state behind (it was on,
            // then turned off), tear that state down once instead of stranding
            // worktrees/refs/branches/stores forever.
            teardown_disabled_project_with_administration(&root, administration, cancellation)
                .await;
        }
    }
}

async fn retained_project_graph(
    administration: &StoreAdministration,
    project_root: &Path,
) -> Option<Arc<crate::tracedecay::TraceDecay>> {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    administration
        .mounted_project_graphs()
        .await
        .into_iter()
        .find(|graph| graph.project_root() == canonical)
}

async fn poll_project(
    repo_root: PathBuf,
    administration: &StoreAdministration,
    cancellation: &CancellationToken,
) {
    let Some(graph) = retained_project_graph(administration, &repo_root).await else {
        return;
    };
    let data_root = graph.store_layout().data_root.clone();
    let command_control = PrCommandControl {
        cancellation: Some(cancellation.clone()),
        ..PrCommandControl::default()
    };
    let repo_for_discovery = repo_root.clone();
    let discovery_control = command_control.clone();
    let discovery = match tokio::task::spawn_blocking(move || {
        discover_open_prs_with_control(&repo_for_discovery, &discovery_control)
    })
    .await
    {
        Ok(Ok(discovery)) => discovery,
        Ok(Err(reason)) => {
            log_daemon_event(
                "pr_autotrack",
                &[
                    ("project", repo_root.display().to_string()),
                    ("action", "poll".to_string()),
                    ("outcome", "error".to_string()),
                    ("reason", reason),
                ],
            );
            return;
        }
        Err(_) => return,
    };

    let report = reconcile_project_with_administration(
        &repo_root,
        &data_root,
        &discovery,
        MAX_NEW_TRACKS_PER_CYCLE,
        PrStoreAdministration::with_control(administration, &graph, &command_control),
    )
    .await;
    let managed = load_state(&data_root).managed.len();
    log_daemon_event(
        "pr_autotrack",
        &[
            ("project", repo_root.display().to_string()),
            ("action", "poll".to_string()),
            ("tracked_now", managed.to_string()),
            ("new_tracked", report.tracked.len().to_string()),
            ("untracked", report.untracked.len().to_string()),
            ("skipped_forks", report.skipped_forks.len().to_string()),
        ],
    );
}

/// Tears down all managed PR state for a project whose `auto_track_pr_branches`
/// is now disabled.
pub async fn teardown_disabled_project(
    graph: Arc<crate::tracedecay::TraceDecay>,
    repo_root: &Path,
) {
    let Ok(administration) = StoreAdministration::for_retained_project_graph(&graph).await else {
        return;
    };
    teardown_disabled_project_with_graph(repo_root, graph, &administration, None).await;
}

async fn teardown_disabled_project_with_administration(
    repo_root: &Path,
    administration: &StoreAdministration,
    cancellation: &CancellationToken,
) {
    let Some(graph) = retained_project_graph(administration, repo_root).await else {
        return;
    };
    teardown_disabled_project_with_graph(repo_root, graph, administration, Some(cancellation))
        .await;
}

async fn teardown_disabled_project_with_graph(
    repo_root: &Path,
    graph: Arc<crate::tracedecay::TraceDecay>,
    administration: &StoreAdministration,
    cancellation: Option<&CancellationToken>,
) {
    let data_root = graph.store_layout().data_root.clone();
    if load_state(&data_root).managed.is_empty() {
        return;
    }
    let command_control = PrCommandControl {
        cancellation: cancellation.cloned(),
        ..PrCommandControl::default()
    };
    let report = reconcile_project_with_administration(
        repo_root,
        &data_root,
        &PrDiscovery::default(),
        MAX_NEW_TRACKS_PER_CYCLE,
        PrStoreAdministration::with_control(administration, &graph, &command_control),
    )
    .await;
    log_daemon_event(
        "pr_autotrack",
        &[
            ("project", repo_root.display().to_string()),
            ("action", "teardown".to_string()),
            ("untracked", report.untracked.len().to_string()),
        ],
    );
}
