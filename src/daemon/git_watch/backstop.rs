use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use super::{
    GitWatcher, WatchState, request_freshness_for_repository, retire_missing_repository_owners,
};

pub(super) async fn run(watcher: GitWatcher) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    let mut due_by_repository = HashMap::new();
    ticker.tick().await;

    loop {
        tokio::select! {
            biased;
            () = watcher.inner.cancellation.cancelled() => return,
            _ = ticker.tick() => {}
        }
        tick(&watcher, &mut due_by_repository).await;
    }
}

async fn tick(watcher: &GitWatcher, due_by_repository: &mut HashMap<PathBuf, Instant>) {
    retire_missing_repository_owners(&watcher.inner).await;
    let entries: Vec<(PathBuf, Arc<WatchState>)> = {
        let projects = watcher.inner.projects.lock().await;
        projects
            .iter()
            .map(|(common, state)| (common.clone(), Arc::clone(state)))
            .collect()
    };
    let active: BTreeSet<_> = entries.iter().map(|(common, _)| common.clone()).collect();
    due_by_repository.retain(|common, _| active.contains(common));

    let now = Instant::now();
    for (common, state) in &entries {
        let interval_mins = state.config.backstop_interval_mins;
        if interval_mins == 0 {
            due_by_repository.remove(common);
            continue;
        }
        let period = Duration::from_secs(interval_mins.saturating_mul(60).max(1));
        let due = due_by_repository
            .entry(common.clone())
            .or_insert(now + period);
        if now < *due {
            continue;
        }
        *due = now + period;
        let snapshot = state.health.snapshot();
        if snapshot.heartbeat_stale() || snapshot.status.is_degraded() {
            request_freshness_for_repository(&watcher.inner, state, None).await;
        }
    }
}
