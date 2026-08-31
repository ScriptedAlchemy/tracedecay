use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant as StdInstant;

use tokio::sync::Notify;
use tokio::time::Instant;

use super::DirtySet;
use super::health::ProjectHealth;
use crate::ports::{
    GitWatchMaintenanceWakeV1 as MaintenanceCoordinator, GitWatchSyncConfigV1 as SyncConfig,
};
use tracedecay_session_memory::context::CancellationToken;

pub enum WorktreeRegistration {
    Ready,
    Capacity,
    Retired,
}

struct WatchStateOwnership {
    worktrees: BTreeMap<PathBuf, WorktreeWatchRegistration>,
    retired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchTiming {
    pub debounce: Duration,
    pub max_delay: Duration,
    pub backstop_interval: Option<Duration>,
}

#[derive(Clone)]
struct WorktreeWatchRegistration {
    git_dir: PathBuf,
    config: SyncConfig,
}

#[derive(Clone)]
pub struct WatchCancellation {
    daemon: CancellationToken,
    repository: CancellationToken,
}

impl WatchCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.daemon.is_cancelled() || self.repository.is_cancelled()
    }

    pub async fn cancelled(&self) {
        tokio::select! {
            biased;
            () = self.daemon.cancelled() => {}
            () = self.repository.cancelled() => {}
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct OperationScanProbe {
    armed: AtomicBool,
    active: AtomicU64,
    pub entered: Notify,
    wait_lock: std::sync::Mutex<()>,
    wait: std::sync::Condvar,
}

#[cfg(test)]
impl OperationScanProbe {
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub fn block_if_armed(&self, cancellation: &WatchCancellation) {
        if !self.armed.swap(false, Ordering::AcqRel) {
            return;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        self.entered.notify_one();
        let started = StdInstant::now();
        let mut guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !cancellation.is_cancelled() && started.elapsed() < Duration::from_secs(2) {
            let (next, _) = self
                .wait
                .wait_timeout(guard, Duration::from_millis(10))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next;
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn active(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct RetirementRaceProbe {
    armed: AtomicBool,
    pub after_empty: Notify,
    pub release: Notify,
}

#[cfg(test)]
impl RetirementRaceProbe {
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub async fn pause_if_armed(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            self.after_empty.notify_one();
            self.release.notified().await;
        }
    }
}

/// Repository-scoped watcher state.
///
/// Git metadata belongs to the repository common directory, while HEAD,
/// operation markers, and scheduler ownership remain per worktree. Keeping
/// those two identities together prevents linked worktrees from multiplying
/// OS watchers without collapsing their freshness requests.
pub struct WatchState {
    pub common_dir: PathBuf,
    ownership: super::ProfiledStdMutex<WatchStateOwnership>,
    pub dirty: super::ProfiledTokioMutex<DirtySet>,
    pub reconciliation_pending: AtomicBool,
    pub wake: Notify,
    pub reconfigure: Notify,
    retry_not_before: super::ProfiledStdMutex<Option<Instant>>,
    retry_backoff_ms: AtomicU64,
    pub maintenance: MaintenanceCoordinator,
    pub health: ProjectHealth,
    task: super::ProfiledStdMutex<Option<tokio::task::JoinHandle<()>>>,
    retirement: CancellationToken,
    #[cfg(test)]
    pub entered_debounce: Notify,
    #[cfg(test)]
    pub drained_plans: AtomicU64,
    #[cfg(test)]
    pub plan_drained: Notify,
    #[cfg(test)]
    pub operation_scan_probe: OperationScanProbe,
    #[cfg(test)]
    pub retirement_probe: RetirementRaceProbe,
}

impl WatchState {
    #[cfg(test)]
    pub fn new(
        common_dir: PathBuf,
        project_root: PathBuf,
        git_dir: PathBuf,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        Self::new_with_config(
            common_dir,
            project_root,
            git_dir,
            maintenance,
            SyncConfig::default(),
        )
    }

    pub fn new_with_config(
        common_dir: PathBuf,
        project_root: PathBuf,
        git_dir: PathBuf,
        maintenance: MaintenanceCoordinator,
        config: SyncConfig,
    ) -> Self {
        Self {
            common_dir,
            ownership: hotpath::mutex!(
                std::sync::Mutex::new(WatchStateOwnership {
                    worktrees: BTreeMap::from([(
                        project_root,
                        WorktreeWatchRegistration { git_dir, config },
                    )]),
                    retired: false,
                }),
                label = "daemon.git.watch.ownership"
            ),
            dirty: hotpath::mutex!(
                tokio::sync::Mutex::new(DirtySet::default()),
                label = "daemon.git.watch.dirty"
            ),
            reconciliation_pending: AtomicBool::new(false),
            wake: Notify::new(),
            reconfigure: Notify::new(),
            retry_not_before: hotpath::mutex!(
                std::sync::Mutex::new(None),
                label = "daemon.git.watch.retry"
            ),
            retry_backoff_ms: AtomicU64::new(250),
            maintenance,
            health: ProjectHealth::default(),
            task: hotpath::mutex!(std::sync::Mutex::new(None), label = "daemon.git.watch.task"),
            retirement: CancellationToken::new(),
            #[cfg(test)]
            entered_debounce: Notify::new(),
            #[cfg(test)]
            drained_plans: AtomicU64::new(0),
            #[cfg(test)]
            plan_drained: Notify::new(),
            #[cfg(test)]
            operation_scan_probe: OperationScanProbe::default(),
            #[cfg(test)]
            retirement_probe: RetirementRaceProbe::default(),
        }
    }

    /// Adds one scheduler-owned worktree to this repository watcher.
    ///
    /// A new git directory changes the exact set of marker paths watched by
    /// the repository task, so the task is told to rebuild its small metadata
    /// watch set. Re-registering an existing root is a no-op.
    #[cfg(test)]
    pub fn register_worktree(
        &self,
        project_root: PathBuf,
        git_dir: PathBuf,
        max_worktrees: usize,
    ) -> WorktreeRegistration {
        self.register_worktree_with_config(
            project_root,
            git_dir,
            SyncConfig::default(),
            max_worktrees,
        )
    }

    pub fn register_worktree_with_config(
        &self,
        project_root: PathBuf,
        git_dir: PathBuf,
        config: SyncConfig,
        max_worktrees: usize,
    ) -> WorktreeRegistration {
        let mut ownership = self
            .ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ownership.retired {
            return WorktreeRegistration::Retired;
        }
        if ownership
            .worktrees
            .get(&project_root)
            .is_some_and(|registration| {
                registration.git_dir == git_dir && registration.config == config
            })
        {
            return WorktreeRegistration::Ready;
        }
        if !ownership.worktrees.contains_key(&project_root)
            && ownership.worktrees.len() >= max_worktrees
        {
            return WorktreeRegistration::Capacity;
        }
        ownership
            .worktrees
            .insert(project_root, WorktreeWatchRegistration { git_dir, config });
        drop(ownership);
        self.reconfigure.notify_one();
        WorktreeRegistration::Ready
    }

    pub fn worktree_roots(&self) -> Vec<PathBuf> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .keys()
            .cloned()
            .collect()
    }

    pub fn git_dirs(&self) -> Vec<PathBuf> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .values()
            .map(|registration| registration.git_dir.clone())
            .collect()
    }

    pub fn worktrees(&self) -> Vec<(PathBuf, PathBuf)> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .iter()
            .map(|(root, registration)| (root.clone(), registration.git_dir.clone()))
            .collect()
    }

    /// Resolves callback paths to exact mounted worktree roots. Shared ref
    /// registries and unknown paths return `None`, which truthfully requests
    /// repository-wide reconciliation.
    pub fn event_roots(&self, paths: &[PathBuf]) -> Option<BTreeSet<PathBuf>> {
        if paths.is_empty() {
            return None;
        }
        let worktrees = self.worktrees();
        let mut routed = BTreeSet::new();
        for path in paths {
            if path.starts_with(self.common_dir.join("refs"))
                || path == &self.common_dir.join("packed-refs")
            {
                return None;
            }
            let root = worktrees
                .iter()
                .filter(|(_, git_dir)| path.starts_with(git_dir))
                .max_by_key(|(_, git_dir)| git_dir.components().count())
                .map(|(root, _)| root.clone())?;
            routed.insert(root);
        }
        Some(routed)
    }

    pub fn cancellation(&self, daemon: &CancellationToken) -> WatchCancellation {
        WatchCancellation {
            daemon: daemon.clone(),
            repository: self.retirement.clone(),
        }
    }

    pub fn retire(&self) {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired = true;
        self.signal_retirement();
    }

    fn signal_retirement(&self) {
        self.retirement.cancel();
        self.wake.notify_waiters();
        self.reconfigure.notify_waiters();
    }

    pub fn retain_task(&self, handle: tokio::task::JoinHandle<()>) {
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    }

    pub fn take_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[cfg(test)]
    pub fn retained_task_id(&self) -> Option<tokio::task::Id> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(tokio::task::JoinHandle::id)
    }

    #[cfg(test)]
    pub fn has_retained_task(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub fn schedule_retry(&self) {
        const RETRY_MAX_MS: u64 = 60_000;
        let delay_ms = self.retry_backoff_ms.load(Ordering::Acquire);
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Instant::now() + Duration::from_millis(delay_ms));
        self.retry_backoff_ms.store(
            delay_ms.saturating_mul(2).min(RETRY_MAX_MS),
            Ordering::Release,
        );
    }

    pub fn retry_not_before(&self) -> Option<Instant> {
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn clear_retry(&self) {
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.retry_backoff_ms.store(250, Ordering::Release);
    }

    pub fn prune_missing_worktrees(&self, mut should_stop: impl FnMut() -> bool) -> bool {
        let mut ownership = self
            .ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut missing = Vec::new();
        for (root, registration) in &ownership.worktrees {
            if should_stop() {
                return false;
            }
            if !root.is_dir() || !registration.git_dir.is_dir() {
                missing.push(root.clone());
            }
        }
        for root in missing {
            ownership.worktrees.remove(&root);
        }
        true
    }

    /// Atomically closes registration when the last worktree disappears.
    ///
    /// The caller holds the repository-owner registry lock while invoking
    /// this method. A concurrent registration either wins before this lock and
    /// keeps the owner live, or observes `Retired` and retries against a newly
    /// published owner.
    pub fn retire_if_empty(&self) -> bool {
        let retired = {
            let mut ownership = self
                .ownership
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !ownership.worktrees.is_empty() {
                return false;
            }
            ownership.retired = true;
            true
        };
        if retired {
            self.signal_retirement();
        }
        retired
    }

    pub fn effective_timing(&self) -> WatchTiming {
        let ownership = self
            .ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let debounce_ms = ownership
            .worktrees
            .values()
            .map(|registration| registration.config.watch_debounce_ms)
            .min()
            .unwrap_or(0);
        let max_delay_ms = ownership
            .worktrees
            .values()
            .map(|registration| registration.config.watch_max_delay_ms)
            .min()
            .unwrap_or(0);
        let backstop_interval = ownership
            .worktrees
            .values()
            .map(|registration| registration.config.backstop_interval_mins)
            .filter(|minutes| *minutes != 0)
            .min()
            .map(|minutes| Duration::from_secs(minutes.saturating_mul(60)));
        WatchTiming {
            debounce: Duration::from_millis(debounce_ms),
            max_delay: Duration::from_millis(max_delay_ms),
            backstop_interval,
        }
    }

    pub fn backstop_intervals(&self) -> Vec<(PathBuf, Option<Duration>)> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .iter()
            .map(|(root, registration)| {
                (
                    root.clone(),
                    (registration.config.backstop_interval_mins != 0).then(|| {
                        Duration::from_secs(
                            registration
                                .config
                                .backstop_interval_mins
                                .saturating_mul(60),
                        )
                    }),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn config_for_root(&self, project_root: &Path) -> Option<SyncConfig> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .get(project_root)
            .map(|registration| registration.config.clone())
    }

    #[cfg(test)]
    pub fn contains_worktree(&self, project_root: &Path) -> bool {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .contains_key(project_root)
    }
}
