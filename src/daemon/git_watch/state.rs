//! Per-project watcher state and bounded retry ownership.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

use super::super::maintenance::MaintenanceCoordinator;
use super::generation::{GenerationGate, SYNC_RETRY_MAX};

/// Liveness needed by the production backstop.
#[derive(Default)]
pub(super) struct ProjectHealth {
    pub(super) last_heartbeat: AtomicU64,
    last_sync: AtomicU64,
    degraded: AtomicBool,
}

impl ProjectHealth {
    pub(super) fn beat(&self) {
        self.last_heartbeat
            .store(super::now_secs(), Ordering::Relaxed);
    }

    pub(super) fn heartbeat_stale(&self) -> bool {
        let heartbeat = self.last_heartbeat.load(Ordering::Relaxed);
        heartbeat == 0 || super::now_secs().saturating_sub(heartbeat) > super::HEARTBEAT_STALE_SECS
    }

    pub(super) fn mark_synced(&self) {
        self.last_sync.store(super::now_secs(), Ordering::Relaxed);
    }

    pub(super) fn set_degraded(&self, degraded: bool) {
        self.degraded.store(degraded, Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> ProjectHealthSnapshot {
        ProjectHealthSnapshot {
            last_heartbeat: self.last_heartbeat.load(Ordering::Relaxed),
            last_sync: self.last_sync.load(Ordering::Relaxed),
            degraded: self.degraded.load(Ordering::Relaxed),
        }
    }
}

pub(super) struct ProjectHealthSnapshot {
    pub(super) last_heartbeat: u64,
    pub(super) last_sync: u64,
    pub(super) degraded: bool,
}

/// Per-project watch state shared between the debounce task and the coordinator.
pub(super) struct WatchState {
    pub(super) project_root: PathBuf,
    pub(super) common_dir: Option<PathBuf>,
    snapshot_roots: Mutex<HashSet<PathBuf>>,
    pub(super) sync_gates: Mutex<HashMap<PathBuf, GenerationGate>>,
    pub(super) worktree_gates: Mutex<HashMap<PathBuf, GenerationGate>>,
    /// One bounded retry timer owns all skipped drained work for this project.
    retry: Mutex<Option<ScheduledPlan>>,
    pub(super) retry_wake: Notify,
    shared_retry_wake: Option<Arc<Notify>>,
    /// Dirty flag + affected-branch set. Coalesces a 50-commit rebase into a
    /// single sync — an unbounded queue would fire 50 times.
    pub(super) dirty: Mutex<DirtySet>,
    /// Set before every notify callback attempts the non-blocking dirty lock.
    /// If that lock is contended, the debounce task turns this latch into one
    /// bounded full reconciliation instead of dropping the event.
    pub(super) reconciliation_pending: AtomicBool,
    /// Raised by the notify callback (or degraded poller) on every metadata
    /// event; the debounce task waits on it instead of polling.
    pub(super) wake: Notify,
    pub(super) maintenance: MaintenanceCoordinator,
    pub(super) health: ProjectHealth,
    /// Handle to the supervised task so drop cancels it on shutdown.
    pub(super) task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Test-only: `debounce_loop` signals once before its first `wake` wait.
    #[cfg(test)]
    pub(super) entered_debounce: Notify,
    /// Test-only: count and signal completed dirty-set drains before plan I/O.
    #[cfg(test)]
    pub(super) drained_plans: AtomicU64,
    #[cfg(test)]
    pub(super) plan_drained: Notify,
}

impl WatchState {
    pub(super) fn new(
        project_root: PathBuf,
        common_dir: Option<PathBuf>,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        Self::new_with_retry_wake(project_root, common_dir, maintenance, None)
    }

    pub(super) fn new_with_retry_wake(
        project_root: PathBuf,
        common_dir: Option<PathBuf>,
        maintenance: MaintenanceCoordinator,
        shared_retry_wake: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            snapshot_roots: Mutex::new(HashSet::from([project_root.clone()])),
            project_root,
            common_dir,
            sync_gates: Mutex::new(HashMap::new()),
            worktree_gates: Mutex::new(HashMap::new()),
            retry: Mutex::new(None),
            retry_wake: Notify::new(),
            shared_retry_wake,
            dirty: Mutex::new(DirtySet::default()),
            reconciliation_pending: AtomicBool::new(false),
            wake: Notify::new(),
            maintenance,
            health: ProjectHealth::default(),
            task: Mutex::new(None),
            #[cfg(test)]
            entered_debounce: Notify::new(),
            #[cfg(test)]
            drained_plans: AtomicU64::new(0),
            #[cfg(test)]
            plan_drained: Notify::new(),
        }
    }

    pub(super) async fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<_> = self.snapshot_roots.lock().await.iter().cloned().collect();
        roots.sort();
        roots
    }

    pub(super) async fn register_snapshot_root(&self, root: &Path) -> bool {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.snapshot_roots.lock().await.insert(canonical)
    }

    pub(super) async fn prune_missing_roots(&self) {
        let roots = {
            let mut roots = self.snapshot_roots.lock().await;
            roots.retain(|root| root.is_dir());
            roots.clone()
        };
        self.sync_gates
            .lock()
            .await
            .retain(|root, _| roots.contains(root));
        self.worktree_gates
            .lock()
            .await
            .retain(|root, _| roots.contains(root));
    }

    pub(super) async fn schedule_retry(&self, plan: DirtyPlan, delay: Duration) {
        if plan.is_empty() {
            return;
        }
        let not_before = Instant::now() + delay.min(SYNC_RETRY_MAX);
        let mut retry = self.retry.lock().await;
        match retry.as_mut() {
            Some(pending) => {
                pending.plan.merge(plan);
                pending.not_before = pending.not_before.min(not_before);
            }
            None => {
                *retry = Some(ScheduledPlan { plan, not_before });
            }
        }
        drop(retry);
        self.retry_wake.notify_one();
        if let Some(wake) = &self.shared_retry_wake {
            wake.notify_one();
        }
    }

    pub(super) async fn retry_deadline(&self) -> Option<Instant> {
        self.retry
            .lock()
            .await
            .as_ref()
            .map(|retry| retry.not_before)
    }

    pub(super) async fn take_due_retry(&self) -> Option<DirtyPlan> {
        let mut retry = self.retry.lock().await;
        if retry
            .as_ref()
            .is_some_and(|pending| pending.not_before <= Instant::now())
        {
            return retry.take().map(|pending| pending.plan);
        }
        None
    }
}

#[derive(Default)]
pub(super) struct DirtySet {
    /// Any metadata event happened; the project needs at least a current-branch
    /// freshness pass.
    pub(super) dirty: bool,
    /// Branches whose `refs/heads/<b>` changed, for diff-scoped incremental
    /// syncs. Empty + `dirty` => sync current branch only.
    pub(super) branches: HashSet<String>,
    /// Worktree directories newly created under `worktrees/`, to proactively
    /// track. Values are the `worktrees/<name>` leaf names.
    pub(super) new_worktrees: HashSet<String>,
    /// A ref or worktree was deleted → GC is eligible on the next cycle.
    pub(super) gc_eligible: bool,
    /// A linked worktree disappeared; stale generation receipts must be
    /// discarded so recreating the same path is tracked again.
    pub(super) worktree_removed: bool,
    /// Path-level event detail was lost to callback lock contention. The next
    /// cycle must inventory linked worktrees and consider GC, not merely sync
    /// the current branch.
    pub(super) reconcile_metadata: bool,
    /// Instant of the first event since the last drain (for the hard cap).
    pub(super) first_event: Option<Instant>,
    /// Instant of the most recent event (for the quiet-window deadline).
    pub(super) last_event: Option<Instant>,
}

impl DirtySet {
    #[cfg(test)]
    pub(super) fn is_clean(&self) -> bool {
        !self.dirty
            && self.branches.is_empty()
            && self.new_worktrees.is_empty()
            && !self.gc_eligible
            && !self.worktree_removed
            && !self.reconcile_metadata
    }

    pub(super) fn take(&mut self) -> DirtyPlan {
        let plan = DirtyPlan {
            dirty: self.dirty,
            branches: std::mem::take(&mut self.branches),
            new_worktrees: std::mem::take(&mut self.new_worktrees),
            gc_eligible: self.gc_eligible,
            worktree_removed: self.worktree_removed,
            reconcile_metadata: self.reconcile_metadata,
        };
        self.dirty = false;
        self.gc_eligible = false;
        self.worktree_removed = false;
        self.reconcile_metadata = false;
        self.first_event = None;
        self.last_event = None;
        plan
    }
}

/// The drained work for one debounce cycle.
#[derive(Clone, Debug)]
pub(super) struct DirtyPlan {
    pub(super) dirty: bool,
    pub(super) branches: HashSet<String>,
    pub(super) new_worktrees: HashSet<String>,
    pub(super) gc_eligible: bool,
    pub(super) worktree_removed: bool,
    pub(super) reconcile_metadata: bool,
}

impl DirtyPlan {
    pub(super) fn sync() -> Self {
        Self {
            dirty: true,
            branches: HashSet::new(),
            new_worktrees: HashSet::new(),
            gc_eligible: false,
            worktree_removed: false,
            reconcile_metadata: false,
        }
    }

    pub(super) fn gc() -> Self {
        Self {
            dirty: false,
            branches: HashSet::new(),
            new_worktrees: HashSet::new(),
            gc_eligible: true,
            worktree_removed: false,
            reconcile_metadata: false,
        }
    }

    pub(super) fn worktrees(new_worktrees: HashSet<String>) -> Self {
        Self {
            dirty: false,
            branches: HashSet::new(),
            new_worktrees,
            gc_eligible: false,
            worktree_removed: false,
            reconcile_metadata: false,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        !self.dirty
            && self.branches.is_empty()
            && self.new_worktrees.is_empty()
            && !self.gc_eligible
            && !self.worktree_removed
            && !self.reconcile_metadata
    }

    fn merge(&mut self, other: Self) {
        self.dirty |= other.dirty;
        self.branches.extend(other.branches);
        self.new_worktrees.extend(other.new_worktrees);
        self.gc_eligible |= other.gc_eligible;
        self.worktree_removed |= other.worktree_removed;
        self.reconcile_metadata |= other.reconcile_metadata;
    }
}

#[derive(Debug)]
struct ScheduledPlan {
    plan: DirtyPlan,
    not_before: Instant,
}
