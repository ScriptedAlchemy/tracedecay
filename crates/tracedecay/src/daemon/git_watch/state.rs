use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant as StdInstant;

use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::Instant;

use super::DirtySet;
use super::health::ProjectHealth;
use crate::config::SyncConfig;
use crate::daemon::maintenance::MaintenanceCoordinator;
use tracedecay_usecases::context::CancellationToken;

pub(super) enum WorktreeRegistration {
    Ready,
    Capacity,
    Retired,
}

struct WatchStateOwnership {
    worktrees: BTreeMap<PathBuf, WorktreeWatchRegistration>,
    retired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WatchTiming {
    pub(super) debounce: Duration,
    pub(super) max_delay: Duration,
    pub(super) backstop_interval: Option<Duration>,
}

#[derive(Clone)]
struct WorktreeWatchRegistration {
    git_dir: PathBuf,
    config: SyncConfig,
}

#[derive(Clone)]
pub(super) struct WatchCancellation {
    daemon: CancellationToken,
    repository: CancellationToken,
}

impl WatchCancellation {
    pub(super) fn is_cancelled(&self) -> bool {
        self.daemon.is_cancelled() || self.repository.is_cancelled()
    }

    pub(super) async fn cancelled(&self) {
        tokio::select! {
            biased;
            () = self.daemon.cancelled() => {}
            () = self.repository.cancelled() => {}
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct OperationScanProbe {
    armed: AtomicBool,
    active: AtomicU64,
    pub(super) entered: Notify,
    wait_lock: std::sync::Mutex<()>,
    wait: std::sync::Condvar,
}

#[cfg(test)]
impl OperationScanProbe {
    pub(super) fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub(super) fn block_if_armed(&self, cancellation: &WatchCancellation) {
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

    pub(super) fn active(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct RetirementRaceProbe {
    armed: AtomicBool,
    pub(super) after_empty: Notify,
    pub(super) release: Notify,
}

#[cfg(test)]
impl RetirementRaceProbe {
    pub(super) fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub(super) async fn pause_if_armed(&self) {
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
pub(super) struct WatchState {
    pub(super) common_dir: PathBuf,
    ownership: StdMutex<WatchStateOwnership>,
    pub(super) dirty: AsyncMutex<DirtySet>,
    pub(super) reconciliation_pending: AtomicBool,
    pub(super) wake: Notify,
    pub(super) reconfigure: Notify,
    retry_not_before: std::sync::Mutex<Option<Instant>>,
    retry_backoff_ms: AtomicU64,
    pub(super) maintenance: MaintenanceCoordinator,
    pub(super) health: ProjectHealth,
    task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    retirement: CancellationToken,
    #[cfg(test)]
    pub(super) entered_debounce: Notify,
    #[cfg(test)]
    pub(super) drained_plans: AtomicU64,
    #[cfg(test)]
    pub(super) plan_drained: Notify,
    #[cfg(test)]
    pub(super) operation_scan_probe: OperationScanProbe,
    #[cfg(test)]
    pub(super) retirement_probe: RetirementRaceProbe,
}

impl WatchState {
    #[cfg(test)]
    pub(super) fn new(
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

    pub(super) fn new_with_config(
        common_dir: PathBuf,
        project_root: PathBuf,
        git_dir: PathBuf,
        maintenance: MaintenanceCoordinator,
        config: SyncConfig,
    ) -> Self {
        Self {
            common_dir,
            ownership: StdMutex::new(WatchStateOwnership {
                worktrees: BTreeMap::from([(
                    project_root,
                    WorktreeWatchRegistration { git_dir, config },
                )]),
                retired: false,
            }),
            dirty: AsyncMutex::new(DirtySet::default()),
            reconciliation_pending: AtomicBool::new(false),
            wake: Notify::new(),
            reconfigure: Notify::new(),
            retry_not_before: std::sync::Mutex::new(None),
            retry_backoff_ms: AtomicU64::new(250),
            maintenance,
            health: ProjectHealth::default(),
            task: StdMutex::new(None),
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
    pub(super) fn register_worktree(
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

    pub(super) fn register_worktree_with_config(
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

    pub(super) fn worktree_roots(&self) -> Vec<PathBuf> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .keys()
            .cloned()
            .collect()
    }

    pub(super) fn git_dirs(&self) -> Vec<PathBuf> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .values()
            .map(|registration| registration.git_dir.clone())
            .collect()
    }

    pub(super) fn worktrees(&self) -> Vec<(PathBuf, PathBuf)> {
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
    pub(super) fn event_roots(&self, paths: &[PathBuf]) -> Option<BTreeSet<PathBuf>> {
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

    pub(super) fn cancellation(&self, daemon: &CancellationToken) -> WatchCancellation {
        WatchCancellation {
            daemon: daemon.clone(),
            repository: self.retirement.clone(),
        }
    }

    pub(super) fn retire(&self) {
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

    pub(super) fn retain_task(&self, handle: tokio::task::JoinHandle<()>) {
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    }

    pub(super) fn take_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[cfg(test)]
    pub(super) fn retained_task_id(&self) -> Option<tokio::task::Id> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(tokio::task::JoinHandle::id)
    }

    #[cfg(test)]
    pub(super) fn has_retained_task(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub(super) fn schedule_retry(&self) {
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

    pub(super) fn retry_not_before(&self) -> Option<Instant> {
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn clear_retry(&self) {
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.retry_backoff_ms.store(250, Ordering::Release);
    }

    pub(super) fn prune_missing_worktrees(&self, mut should_stop: impl FnMut() -> bool) -> bool {
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
    pub(super) fn retire_if_empty(&self) -> bool {
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

    pub(super) fn effective_timing(&self) -> WatchTiming {
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

    pub(super) fn backstop_intervals(&self) -> Vec<(PathBuf, Option<Duration>)> {
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
    pub(super) fn config_for_root(&self, project_root: &Path) -> Option<SyncConfig> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .get(project_root)
            .map(|registration| registration.config.clone())
    }

    #[cfg(test)]
    pub(super) fn contains_worktree(&self, project_root: &Path) -> bool {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .worktrees
            .contains_key(project_root)
    }
}
