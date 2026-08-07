use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use super::ownership::retire_missing_repository_owners;
use super::{GitWatcher, WatchState, request_freshness_for_repository};

pub(super) async fn run(watcher: GitWatcher) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    let mut due_by_root = HashMap::new();
    ticker.tick().await;

    loop {
        tokio::select! {
            biased;
            () = watcher.inner.cancellation.cancelled() => return,
            _ = ticker.tick() => {}
        }
        tick(&watcher, &mut due_by_root).await;
    }
}

async fn tick(watcher: &GitWatcher, due_by_root: &mut HashMap<PathBuf, (Duration, Instant)>) {
    retire_missing_repository_owners(&watcher.inner).await;
    let entries: Vec<(PathBuf, Arc<WatchState>)> = {
        let projects = watcher.inner.projects.lock().await;
        projects
            .iter()
            .map(|(common, state)| (common.clone(), Arc::clone(state)))
            .collect()
    };
    let active: BTreeSet<_> = entries
        .iter()
        .flat_map(|(_, state)| state.worktree_roots())
        .collect();
    due_by_root.retain(|root, _| active.contains(root));

    let now = Instant::now();
    for (_, state) in &entries {
        let mut due_roots = BTreeSet::new();
        for (root, period) in state.backstop_intervals() {
            let Some(period) = period else {
                due_by_root.remove(&root);
                continue;
            };
            let (scheduled_period, due) = due_by_root
                .entry(root.clone())
                .or_insert((period, now + period));
            if *scheduled_period != period {
                *scheduled_period = period;
                *due = now + period;
            }
            if now < *due {
                continue;
            }
            *due = now + period;
            due_roots.insert(root);
        }
        if due_roots.is_empty() {
            continue;
        }
        let snapshot = state.health.snapshot();
        if snapshot.heartbeat_stale() || snapshot.status.is_degraded() {
            request_freshness_for_repository(&watcher.inner, state, Some(due_roots)).await;
        }
    }
}
