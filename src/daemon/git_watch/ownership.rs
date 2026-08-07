use std::path::PathBuf;
use std::sync::Arc;

use super::{GIT_OBSERVATION_BUDGET, GitWatcherInner, WatchState, log_daemon_event};

#[cfg(test)]
#[derive(Default)]
pub(super) struct PublicationRaceProbe {
    armed: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    pub(super) entered: tokio::sync::Notify,
    wait_lock: std::sync::Mutex<()>,
    wait: std::sync::Condvar,
}

#[cfg(test)]
impl PublicationRaceProbe {
    pub(super) fn arm(&self) {
        self.released
            .store(false, std::sync::atomic::Ordering::Release);
        self.armed.store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn block_if_armed(&self) {
        if !self.armed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        self.entered.notify_one();
        let mut guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !self.released.load(std::sync::atomic::Ordering::Acquire) {
            guard = self
                .wait
                .wait(guard)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(super) fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::Release);
        self.wait.notify_all();
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct LifecycleLinearizationReceipts {
    next: std::sync::atomic::AtomicU64,
    shutdown: std::sync::atomic::AtomicU64,
    repository: std::sync::atomic::AtomicU64,
    registration: std::sync::atomic::AtomicU64,
    spawn: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl LifecycleLinearizationReceipts {
    fn record(&self, receipt: &std::sync::atomic::AtomicU64) {
        let order = self.next.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        receipt.store(order, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn record_shutdown(&self) {
        self.record(&self.shutdown);
    }

    pub(super) fn record_repository(&self) {
        self.record(&self.repository);
    }

    pub(super) fn record_registration(&self) {
        self.record(&self.registration);
    }

    pub(super) fn record_spawn(&self) {
        self.record(&self.spawn);
    }

    pub(super) fn shutdown(&self) -> u64 {
        self.shutdown.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn repository(&self) -> u64 {
        self.repository.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn registration(&self) -> u64 {
        self.registration.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn spawn(&self) -> u64 {
        self.spawn.load(std::sync::atomic::Ordering::Acquire)
    }
}

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
        if let Some(handle) = state.take_task() {
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
    let mut projects = inner.projects.lock().await;
    let candidates = projects.keys().cloned().collect::<Vec<_>>();
    let mut retired = Vec::new();
    for common_dir in candidates {
        if inner.cancellation.is_cancelled() {
            return;
        }
        let Some(state) = projects.get(&common_dir).cloned() else {
            continue;
        };
        if !state.prune_missing_worktrees(|| inner.cancellation.is_cancelled()) {
            continue;
        }
        #[cfg(test)]
        state.retirement_probe.pause_if_armed().await;
        let removed = if projects
            .get(&common_dir)
            .is_some_and(|current| Arc::ptr_eq(current, &state))
            && state.retire_if_empty()
        {
            projects.remove(&common_dir)
        } else {
            None
        };
        if let Some(state) = removed {
            retired.push(state);
        }
    }
    drop(projects);
    for state in retired {
        join_retired_repository_state(&state).await;
    }
}

pub(super) async fn join_retired_repository_state(state: &WatchState) {
    state.retire();
    if let Some(handle) = state.take_task() {
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
