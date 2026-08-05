use std::path::PathBuf;
use std::sync::Arc;

use super::{GIT_OBSERVATION_BUDGET, GitWatcherInner, WatchState, log_daemon_event};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::daemon) enum GitWatcherTaskOwner {
    Backstop,
    Repository(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::daemon) enum GitWatcherTaskFailureKind {
    Cancelled,
    Panicked,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::daemon) struct GitWatcherTaskFailure {
    pub(in crate::daemon) owner: GitWatcherTaskOwner,
    pub(in crate::daemon) kind: GitWatcherTaskFailureKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::daemon) struct GitWatcherShutdownOutcome {
    failures: Vec<GitWatcherTaskFailure>,
}

impl GitWatcherShutdownOutcome {
    pub(in crate::daemon) fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    pub(in crate::daemon) fn failures(&self) -> &[GitWatcherTaskFailure] {
        &self.failures
    }

    fn record_join(
        &mut self,
        owner: GitWatcherTaskOwner,
        result: Result<(), tokio::task::JoinError>,
    ) {
        let Err(error) = result else {
            return;
        };
        let kind = if error.is_cancelled() {
            GitWatcherTaskFailureKind::Cancelled
        } else {
            GitWatcherTaskFailureKind::Panicked
        };
        log_daemon_event(
            "git_watch_task_join_failed",
            &[
                ("owner", format!("{owner:?}")),
                ("kind", format!("{kind:?}")),
            ],
        );
        self.failures.push(GitWatcherTaskFailure { owner, kind });
    }

    fn record_timeout(&mut self, owner: GitWatcherTaskOwner) {
        log_daemon_event(
            "git_watch_task_join_failed",
            &[
                ("owner", format!("{owner:?}")),
                ("kind", format!("{:?}", GitWatcherTaskFailureKind::TimedOut)),
            ],
        );
        self.failures.push(GitWatcherTaskFailure {
            owner,
            kind: GitWatcherTaskFailureKind::TimedOut,
        });
    }
}

async fn join_before(
    outcome: &mut GitWatcherShutdownOutcome,
    owner: GitWatcherTaskOwner,
    mut handle: tokio::task::JoinHandle<()>,
    deadline: tokio::time::Instant,
) {
    match tokio::time::timeout_at(deadline, &mut handle).await {
        Ok(result) => outcome.record_join(owner, result),
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            outcome.record_timeout(owner);
        }
    }
}

pub(super) async fn join_watcher_tasks(inner: Arc<GitWatcherInner>) -> GitWatcherShutdownOutcome {
    let mut outcome = GitWatcherShutdownOutcome::default();
    let deadline = tokio::time::Instant::now() + GIT_OBSERVATION_BUDGET;
    if let Some(handle) = inner.backstop_task.lock().await.take() {
        join_before(
            &mut outcome,
            GitWatcherTaskOwner::Backstop,
            handle,
            deadline,
        )
        .await;
    }

    let states: Vec<Arc<WatchState>> = {
        let mut projects = inner.projects.lock().await;
        projects.drain().map(|(_, state)| state).collect()
    };
    for state in states {
        state.retire();
        if let Some(handle) = state.task.lock().await.take() {
            join_before(
                &mut outcome,
                GitWatcherTaskOwner::Repository(state.common_dir.clone()),
                handle,
                deadline,
            )
            .await;
        }
    }
    outcome
}

pub(super) async fn retire_missing_repository_owners(inner: &Arc<GitWatcherInner>) {
    let candidates: Vec<(PathBuf, Arc<WatchState>)> = {
        let projects = inner.projects.lock().await;
        projects
            .iter()
            .map(|(common_dir, state)| (common_dir.clone(), Arc::clone(state)))
            .collect()
    };
    let mut retired = Vec::new();
    for (common_dir, state) in candidates {
        if inner.cancellation.is_cancelled() {
            return;
        }
        if !state.prune_missing_worktrees(|| inner.cancellation.is_cancelled()) || !state.is_empty()
        {
            continue;
        }
        #[cfg(test)]
        state.retirement_probe.pause_if_armed().await;
        let removed = {
            let mut projects = inner.projects.lock().await;
            if projects
                .get(&common_dir)
                .is_some_and(|current| Arc::ptr_eq(current, &state))
                && state.is_empty()
            {
                projects.remove(&common_dir)
            } else {
                None
            }
        };
        if let Some(state) = removed {
            state.retire();
            retired.push(state);
        }
    }
    for state in retired {
        if let Some(handle) = state.task.lock().await.take() {
            let mut outcome = GitWatcherShutdownOutcome::default();
            join_before(
                &mut outcome,
                GitWatcherTaskOwner::Repository(state.common_dir.clone()),
                handle,
                tokio::time::Instant::now() + GIT_OBSERVATION_BUDGET,
            )
            .await;
            log_daemon_event(
                "git_watch_retired",
                &[("git_common_dir", state.common_dir.display().to_string())],
            );
        }
    }
}
