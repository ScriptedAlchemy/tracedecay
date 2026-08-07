use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use super::ownership::retire_missing_repository_owners;
use super::{GitWatcher, WatchState, log_daemon_event, request_freshness_for_repository};

/// The backstop's per-repository coverage decision.
///
/// Returns the log label for the freshness request this tick must make, or
/// `None` when nothing has drifted a full interval. Interval drift alone drives
/// coverage: the heartbeat proves only that the watcher task is alive, and a
/// live watcher reacts to git metadata alone — never to working-tree edits or
/// missed hook deliveries — so a healthy heartbeat must never veto the request.
/// Gating coverage on a stale heartbeat left healthy-watcher projects with no
/// freshness floor at all; live profiles were observed hours stale while every
/// mechanism reported healthy.
pub(super) const fn coverage_action(
    watcher_stale: bool,
    interval_elapsed: bool,
) -> Option<&'static str> {
    match (interval_elapsed, watcher_stale) {
        (false, _) => None,
        (true, true) => Some("backstop_watcher_stale"),
        (true, false) => Some("backstop_interval_elapsed"),
    }
}

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
    super::overflow::cover_overflowed_repositories(watcher).await;
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
        let watcher_stale = snapshot.heartbeat_stale() || snapshot.status.is_degraded();
        if let Some(action) = coverage_action(watcher_stale, true) {
            log_daemon_event(
                "git_watch_backstop_coverage",
                &[
                    ("git_common_dir", state.common_dir.display().to_string()),
                    ("action", action.to_string()),
                    ("roots", due_roots.len().to_string()),
                ],
            );
            request_freshness_for_repository(&watcher.inner, state, Some(due_roots)).await;
        }
    }
}
