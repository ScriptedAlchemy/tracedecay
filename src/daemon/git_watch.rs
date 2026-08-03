//! Daemon git-metadata watcher (design D3), backstop scheduler (D5), and the
//! concurrency governor that both share.
//!
//! # Why this is safe (unlike the removed #80 working-tree watcher)
//!
//! The v6.x `notify-debouncer-full` watcher recursively watched the **working
//! tree** and drowned on monorepo `node_modules`/`target` churn. This watcher
//! watches **only git metadata** under `<git_common_dir>` — `HEAD`,
//! `packed-refs`, `refs/` and `worktrees/` — which is ~5-20 inotify watches per
//! project and never fires on a source-file edit. That distinction is the
//! entire safety argument: we react to *git operations* (commit, checkout,
//! branch create, worktree add, rebase), not to editor saves.
//!
//! # Shape
//!
//! * One [`GitWatcher`] is held by the [`super::DaemonEngine`]; both the accept
//!   loop and `project_server` reach it to lazily [`GitWatcher::ensure_watching`]
//!   freshly-handshaken projects.
//! * Each watched project gets one supervised debounce task ([`project_task`])
//!   that owns a raw `notify` watcher over the metadata paths. Raw events wake
//!   the task via a [`Notify`]; the task sleeps until the quiet deadline
//!   (`watch_debounce_ms`) or the hard cap (`watch_max_delay_ms`), whichever is
//!   first — no busy polling.
//! * A single daemon-wide [`Semaphore`] (`max_concurrent_syncs`) gates every
//!   sync. Per-store single-flight is already provided by the existing sync
//!   lock; `SyncLock` errors are treated as success (a peer synced).
//! * The [`backstop`] timer covers projects whose watcher heartbeat is
//!   stale/absent, and runs branch-store GC on a daily cadence.

#![cfg(unix)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::SyncConfig;
use crate::tracedecay::TraceDecay;

#[cfg(test)]
use super::maintenance::retention_maintenance_enabled;
use super::{
    branch_admin::StoreAdministration, log_daemon_event, maintenance::MaintenanceCoordinator,
    store_maintenance,
};

mod generation;
mod planner;
#[cfg(test)]
use generation::{
    GenerationDecision, ReservationError, SYNC_RETRY_INITIAL, SnapshotGeneration,
    snapshot_generation,
};
use generation::{GenerationGate, SYNC_RETRY_MAX};
use planner::execute_plan;

/// Degraded watchers fall back to polling git metadata every 5 minutes.
const DEGRADED_POLL_INTERVAL: Duration = Duration::from_mins(5);
/// A heartbeat older than this is considered stale by the backstop/doctor.
/// Two debounce+max cycles of slack over the default so a healthy but busy
/// watcher is never treated as dead.
const HEARTBEAT_STALE_SECS: u64 = 120;
/// Cap on the supervised-restart backoff.
const RESTART_BACKOFF_MAX: Duration = Duration::from_mins(1);
/// A claimed sync lane is already progressing elsewhere; retry soon without
/// spinning while preserving the coalesced plan.
const IN_FLIGHT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// The truthful delivery mode for a registered project.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum ProjectWatchCoverage {
    #[default]
    Active = 0,
    DegradedPoll = 1,
}

impl ProjectWatchCoverage {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::DegradedPoll,
            _ => Self::Active,
        }
    }
}

/// Per-project health, readable by the backstop and `tracedecay doctor`.
///
/// Timestamps are UNIX seconds (0 = never). `degraded` flips true when the
/// inotify watcher could not be built / died (e.g. ENOSPC) and the task fell
/// back to mtime polling.
#[derive(Debug, Default)]
struct ProjectHealth {
    /// Last time the watch task completed a poll cycle (event drain or degraded
    /// stat). Advances even when nothing needed syncing — it is a liveness
    /// signal, not a sync signal.
    last_heartbeat: AtomicU64,
    /// Last time a watcher-triggered sync of this project succeeded.
    last_sync: AtomicU64,
    /// True while the project is on the degraded mtime-poll fallback.
    degraded: std::sync::atomic::AtomicBool,
    events_received: AtomicU64,
    plans_drained: AtomicU64,
    deduplicated_activations: AtomicU64,
    unchanged_generation_skips: AtomicU64,
    backoff_skips: AtomicU64,
    worktree_generation_skips: AtomicU64,
    coverage: AtomicU8,
}

impl ProjectHealth {
    fn beat(&self) {
        self.last_heartbeat.store(now_secs(), Ordering::Relaxed);
    }
    fn mark_synced(&self) {
        self.last_sync.store(now_secs(), Ordering::Relaxed);
    }
    fn set_degraded(&self, degraded: bool) {
        self.degraded.store(degraded, Ordering::Relaxed);
    }
    fn set_coverage(&self, coverage: ProjectWatchCoverage) {
        self.coverage.store(coverage as u8, Ordering::Relaxed);
    }
    fn snapshot(&self) -> ProjectHealthSnapshot {
        ProjectHealthSnapshot {
            last_heartbeat: self.last_heartbeat.load(Ordering::Relaxed),
            last_sync: self.last_sync.load(Ordering::Relaxed),
            degraded: self.degraded.load(Ordering::Relaxed),
            events_received: self.events_received.load(Ordering::Relaxed),
            plans_drained: self.plans_drained.load(Ordering::Relaxed),
            deduplicated_activations: self.deduplicated_activations.load(Ordering::Relaxed),
            unchanged_generation_skips: self.unchanged_generation_skips.load(Ordering::Relaxed),
            backoff_skips: self.backoff_skips.load(Ordering::Relaxed),
            worktree_generation_skips: self.worktree_generation_skips.load(Ordering::Relaxed),
            coverage: ProjectWatchCoverage::from_raw(self.coverage.load(Ordering::Relaxed)),
            active_snapshot_roots: 0,
        }
    }
}

/// A point-in-time copy of a project's watch health, for the doctor section.
// The doctor watcher-health section consumes this surface (follow-up wiring);
// fields are populated by the watch loop today so the snapshot is truthful
// the moment doctor reads it.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ProjectHealthSnapshot {
    pub last_heartbeat: u64,
    pub last_sync: u64,
    pub degraded: bool,
    pub events_received: u64,
    pub plans_drained: u64,
    pub deduplicated_activations: u64,
    pub unchanged_generation_skips: u64,
    pub backoff_skips: u64,
    pub worktree_generation_skips: u64,
    pub coverage: ProjectWatchCoverage,
    pub active_snapshot_roots: usize,
}

impl ProjectHealthSnapshot {
    /// True when the watcher has not reported a heartbeat within the staleness
    /// window (or never has). The backstop uses this to decide coverage.
    fn heartbeat_stale(&self) -> bool {
        let hb = self.last_heartbeat;
        hb == 0 || now_secs().saturating_sub(hb) > HEARTBEAT_STALE_SECS
    }
}

/// Per-project watch state shared between the debounce task and the coordinator.
struct WatchState {
    project_root: PathBuf,
    common_dir: Option<PathBuf>,
    snapshot_roots: Mutex<HashSet<PathBuf>>,
    sync_gates: Mutex<HashMap<PathBuf, GenerationGate>>,
    worktree_gates: Mutex<HashMap<PathBuf, GenerationGate>>,
    /// One bounded retry timer owns all skipped drained work for this project.
    retry: Mutex<Option<ScheduledPlan>>,
    retry_wake: Notify,
    /// Dirty flag + affected-branch set. Coalesces a 50-commit rebase into a
    /// single sync — an unbounded queue would fire 50 times.
    dirty: Mutex<DirtySet>,
    /// Set before every notify callback attempts the non-blocking dirty lock.
    /// If that lock is contended, the debounce task turns this latch into one
    /// bounded full reconciliation instead of dropping the event.
    reconciliation_pending: AtomicBool,
    /// Raised by the notify callback (or degraded poller) on every metadata
    /// event; the debounce task waits on it instead of polling.
    wake: Notify,
    maintenance: MaintenanceCoordinator,
    health: ProjectHealth,
    /// Handle to the supervised task so drop cancels it on shutdown.
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Test-only: `debounce_loop` signals once before its first `wake` wait.
    #[cfg(test)]
    entered_debounce: Notify,
    /// Test-only: count and signal completed dirty-set drains before plan I/O.
    #[cfg(test)]
    drained_plans: AtomicU64,
    #[cfg(test)]
    plan_drained: Notify,
}

impl WatchState {
    fn new(
        project_root: PathBuf,
        common_dir: Option<PathBuf>,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        Self {
            snapshot_roots: Mutex::new(HashSet::from([project_root.clone()])),
            project_root,
            common_dir,
            sync_gates: Mutex::new(HashMap::new()),
            worktree_gates: Mutex::new(HashMap::new()),
            retry: Mutex::new(None),
            retry_wake: Notify::new(),
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

    async fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<_> = self.snapshot_roots.lock().await.iter().cloned().collect();
        roots.sort();
        roots
    }

    async fn register_snapshot_root(&self, root: &Path) {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.snapshot_roots.lock().await.insert(canonical);
    }

    async fn prune_missing_roots(&self) {
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

    async fn schedule_retry(&self, plan: DirtyPlan, delay: Duration) {
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
    }

    async fn retry_deadline(&self) -> Option<Instant> {
        self.retry
            .lock()
            .await
            .as_ref()
            .map(|retry| retry.not_before)
    }

    async fn take_due_retry(&self) -> Option<DirtyPlan> {
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

#[derive(Debug, Default)]
struct DirtySet {
    /// Any metadata event happened; the project needs at least a current-branch
    /// freshness pass.
    dirty: bool,
    /// Branches whose `refs/heads/<b>` changed, for diff-scoped incremental
    /// syncs. Empty + `dirty` => sync current branch only.
    branches: HashSet<String>,
    /// Worktree directories newly created under `worktrees/`, to proactively
    /// track. Values are the `worktrees/<name>` leaf names.
    new_worktrees: HashSet<String>,
    /// A ref or worktree was deleted → GC is eligible on the next cycle.
    gc_eligible: bool,
    /// A linked worktree disappeared; stale generation receipts must be
    /// discarded so recreating the same path is tracked again.
    worktree_removed: bool,
    /// Path-level event detail was lost to callback lock contention. The next
    /// cycle must inventory linked worktrees and consider GC, not merely sync
    /// the current branch.
    reconcile_metadata: bool,
    /// Instant of the first event since the last drain (for the hard cap).
    first_event: Option<Instant>,
    /// Instant of the most recent event (for the quiet-window deadline).
    last_event: Option<Instant>,
}

impl DirtySet {
    /// Test-only invariant probe; `cfg_attr` keeps the non-test lib build
    /// from flagging it dead.
    #[cfg_attr(not(test), allow(dead_code))]
    fn is_clean(&self) -> bool {
        !self.dirty
            && self.branches.is_empty()
            && self.new_worktrees.is_empty()
            && !self.gc_eligible
            && !self.worktree_removed
            && !self.reconcile_metadata
    }
    fn take(&mut self) -> DirtyPlan {
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
struct DirtyPlan {
    dirty: bool,
    branches: HashSet<String>,
    new_worktrees: HashSet<String>,
    gc_eligible: bool,
    worktree_removed: bool,
    reconcile_metadata: bool,
}

impl DirtyPlan {
    fn sync() -> Self {
        Self {
            dirty: true,
            branches: HashSet::new(),
            new_worktrees: HashSet::new(),
            gc_eligible: false,
            worktree_removed: false,
            reconcile_metadata: false,
        }
    }

    fn is_empty(&self) -> bool {
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

/// The daemon-held git-metadata watcher. Cheap to clone (all `Arc` inside), and
/// [`Default`] so `DaemonEngine` can derive `Default`.
#[derive(Clone)]
pub struct GitWatcher {
    inner: Arc<GitWatcherInner>,
}

pub(super) struct GitWatcherInner {
    pub(super) config: SyncConfig,
    /// Serializes every store-writing lifetime with daemon branch administration.
    pub(super) administration: StoreAdministration,
    maintenance: MaintenanceCoordinator,
    /// Whether watching is enabled at all (`auto_watch`). When false every
    /// method is a no-op so the daemon runs exactly as before this feature.
    enabled: bool,
    /// Daemon-wide sync concurrency governor.
    pub(super) sync_semaphore: Arc<Semaphore>,
    /// Canonical git common dir → one OS watcher and all active worktree
    /// snapshot roots belonging to it.
    projects: Mutex<HashMap<PathBuf, Arc<WatchState>>>,
    /// Bounded overflow coverage for projects that cannot receive another OS
    /// metadata watcher. These run only the degraded poll/retry loop.
    degraded_projects: Mutex<HashMap<PathBuf, Arc<WatchState>>>,
    /// Single backstop scheduler task, owned so shutdown can cancel and join it.
    backstop_task: Mutex<Option<JoinHandle<()>>>,
    shutting_down: AtomicBool,
}

impl Default for GitWatcher {
    fn default() -> Self {
        // The daemon loads the real (global/default) sync config at spawn via
        // `GitWatcher::spawn`; the Default impl is only used to satisfy
        // `DaemonEngine: Default` before spawn wires the config in. It is
        // disabled so a never-spawned watcher does nothing.
        Self::disabled()
    }
}

impl GitWatcher {
    fn disabled() -> Self {
        Self::from_parts(
            SyncConfig::default(),
            StoreAdministration::default(),
            false,
            MaintenanceCoordinator::default(),
        )
    }

    fn from_parts(
        config: SyncConfig,
        administration: StoreAdministration,
        enabled: bool,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        let permits = config.max_concurrent_syncs.max(1);
        Self {
            inner: Arc::new(GitWatcherInner {
                config,
                administration,
                maintenance,
                enabled,
                sync_semaphore: Arc::new(Semaphore::new(permits)),
                projects: Mutex::new(HashMap::new()),
                degraded_projects: Mutex::new(HashMap::new()),
                backstop_task: Mutex::new(None),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// Builds a watcher from the given sync config. Watching is gated on
    /// `auto_watch`; when disabled the returned watcher is inert.
    ///
    /// The test constructor deliberately uses the process's current profile
    /// and a standalone coordinator so unit tests retain their existing behavior.
    #[cfg(test)]
    pub fn new(config: SyncConfig) -> Self {
        Self::new_with_administration(
            config,
            StoreAdministration::default(),
            MaintenanceCoordinator::default(),
        )
    }

    /// Builds a watcher bound to the daemon's profile and administration
    /// coordinator. The daemon uses this constructor so watcher syncs and
    /// destructive branch administration share one writer gate.
    pub(super) fn new_with_administration(
        config: SyncConfig,
        administration: StoreAdministration,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        let enabled = config.auto_watch;
        Self::from_parts(config, administration, enabled, maintenance)
    }

    // Doctor watcher-health surface (follow-up wiring).
    pub fn is_enabled(&self) -> bool {
        self.inner.enabled
    }

    /// Registers the recently-seen projects and starts the backstop timer.
    ///
    /// Called once from `run_foreground_unix` after the engine is built. Safe to
    /// call on a disabled watcher (no-op).
    pub(super) async fn spawn(&self, profile_database: Arc<crate::global_db::RegisteredGlobalDb>) {
        if !self.inner.enabled || self.inner.shutting_down.load(Ordering::Acquire) {
            return;
        }
        // Startup does not manufacture project owners from registry paths.
        // Active daemon handshakes call `ensure_watching` after publishing the
        // retained project server and graph handle.

        let watcher = self.clone();
        let handle = tokio::spawn(async move {
            backstop::run(watcher, profile_database).await;
        });
        *self.inner.backstop_task.lock().await = Some(handle);
    }

    /// Lazily starts watching `project_root` if not already watched and under
    /// the project cap. Idempotent and cheap on the hot path (a map lookup).
    pub async fn ensure_watching(&self, project_root: &Path) {
        if !self.inner.enabled || self.inner.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let canonical = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let common_dir = crate::worktree::git_common_dir(&canonical);
        let key = common_dir.clone().unwrap_or_else(|| canonical.clone());

        let mut projects = self.inner.projects.lock().await;
        if let Some(state) = projects.get(&key).cloned() {
            drop(projects);
            state.register_snapshot_root(&canonical).await;
            state
                .health
                .deduplicated_activations
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        if projects.len() >= self.inner.config.watch_max_projects {
            drop(projects);
            self.ensure_degraded_coverage(key, canonical).await;
            return;
        }

        let state = Arc::new(WatchState::new(
            canonical.clone(),
            common_dir,
            self.inner.maintenance.clone(),
        ));
        projects.insert(key.clone(), Arc::clone(&state));
        drop(projects);

        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(supervise_project(inner, Arc::clone(&state)));
        *state.task.lock().await = Some(handle);

        log_daemon_event(
            "git_watch_started",
            &[
                ("project", canonical.display().to_string()),
                ("watch_identity", key.display().to_string()),
            ],
        );
    }

    async fn ensure_degraded_coverage(&self, key: PathBuf, canonical: PathBuf) {
        let overflow_limit = self.inner.config.watch_max_projects;
        if overflow_limit == 0 {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", canonical.display().to_string()),
                    ("reason", "watch_capacity_uncovered".to_string()),
                ],
            );
            return;
        }
        let mut degraded = self.inner.degraded_projects.lock().await;
        if let Some(state) = degraded.get(&key).cloned() {
            drop(degraded);
            state.register_snapshot_root(&canonical).await;
            state
                .health
                .deduplicated_activations
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        if degraded.len() >= overflow_limit {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", canonical.display().to_string()),
                    ("reason", "watch_capacity_uncovered".to_string()),
                ],
            );
            return;
        }
        let state = Arc::new(WatchState::new(
            canonical.clone(),
            crate::worktree::git_common_dir(&canonical),
            self.inner.maintenance.clone(),
        ));
        state.health.set_degraded(true);
        state
            .health
            .set_coverage(ProjectWatchCoverage::DegradedPoll);
        degraded.insert(key.clone(), Arc::clone(&state));
        drop(degraded);

        let inner = Arc::clone(&self.inner);
        let task_state = Arc::clone(&state);
        let handle = tokio::spawn(async move { degraded_poll_loop(&inner, &task_state).await });
        *state.task.lock().await = Some(handle);
        log_daemon_event(
            "git_watch_degraded",
            &[
                ("project", canonical.display().to_string()),
                ("reason", "watch_capacity_reached".to_string()),
                ("coverage", "degraded_poll".to_string()),
            ],
        );
    }

    /// Stops every watcher-owned task and joins it before database shutdown.
    pub async fn shutdown(&self) {
        if !self.inner.enabled || self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }

        if let Some(handle) = self.inner.backstop_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }

        let states: Vec<Arc<WatchState>> = {
            let mut projects = self.inner.projects.lock().await;
            let mut states: Vec<_> = projects.drain().map(|(_, state)| state).collect();
            drop(projects);
            states.extend(
                self.inner
                    .degraded_projects
                    .lock()
                    .await
                    .drain()
                    .map(|(_, state)| state),
            );
            states
        };
        for state in states {
            if let Some(handle) = state.task.lock().await.take() {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    /// A doctor-facing snapshot of every registered project's watch health.
    #[cfg(test)]
    pub async fn health_report(&self) -> Vec<(PathBuf, ProjectHealthSnapshot)> {
        let mut out: Vec<_> = self
            .inner
            .projects
            .lock()
            .await
            .iter()
            .map(|(root, state)| (root.clone(), Arc::clone(state)))
            .collect();
        out.extend(
            self.inner
                .degraded_projects
                .lock()
                .await
                .iter()
                .map(|(root, state)| (root.clone(), Arc::clone(state))),
        );
        let mut snapshots = Vec::with_capacity(out.len());
        for (root, state) in out.drain(..) {
            let mut snapshot = state.health.snapshot();
            snapshot.active_snapshot_roots = state.snapshot_roots.lock().await.len();
            snapshots.push((root, snapshot));
        }
        snapshots.sort_by(|a, b| a.0.cmp(&b.0));
        snapshots
    }
}

#[cfg(test)]
fn watcher_key(project_root: &Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    crate::worktree::git_common_dir(&canonical).unwrap_or(canonical)
}

async fn retained_project_graph(
    inner: &GitWatcherInner,
    project_root: &Path,
) -> Option<Arc<TraceDecay>> {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let active_branch = crate::branch::current_branch(&canonical);
    inner
        .administration
        .mounted_project_graphs()
        .await
        .into_iter()
        .find(|graph| {
            graph.project_root() == canonical && graph.active_branch() == active_branch.as_deref()
        })
}

/// Supervises one project's watch task: on panic, restart with capped
/// exponential backoff so a transient watcher failure never permanently drops a
/// project (the backstop still covers it in the meantime).
async fn supervise_project(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let inner_c = Arc::clone(&inner);
        let state_c = Arc::clone(&state);
        let result =
            tokio::spawn(async move { Box::pin(project_task(inner_c, state_c)).await }).await;
        match result {
            Ok(()) => return, // clean exit (watcher gave up gracefully)
            Err(join_err) if join_err.is_cancelled() => return,
            Err(_panic) => {
                log_daemon_event(
                    "git_watch_restart",
                    &[
                        ("project", state.project_root.display().to_string()),
                        ("backoff_ms", backoff.as_millis().to_string()),
                    ],
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
            }
        }
    }
}

/// One project's event loop: build the notify watcher over git metadata, then
/// debounce raw events into coalesced syncs. On watcher construction/death,
/// fall back to a 5-minute mtime poll for THIS project only.
async fn project_task(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    let Some(common_dir) = state.common_dir.clone() else {
        // Not a resolvable git repo (yet). Degrade to polling so a later `git
        // init` / clone is still eventually covered.
        state.health.set_degraded(true);
        state
            .health
            .set_coverage(ProjectWatchCoverage::DegradedPoll);
        degraded_poll_loop(&inner, &state).await;
        return;
    };

    // Build the raw watcher. Its callback pushes into the dirty set and wakes
    // the debounce loop — it never blocks and never syncs inline.
    let wake_state = Arc::clone(&state);
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            classify_and_mark(&wake_state, &event);
        }
    });

    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", state.project_root.display().to_string()),
                    ("reason", "watcher_build_failed".to_string()),
                    ("error", e.to_string()),
                ],
            );
            state.health.set_degraded(true);
            state
                .health
                .set_coverage(ProjectWatchCoverage::DegradedPoll);
            degraded_poll_loop(&inner, &state).await;
            return;
        }
    };

    if let Err(e) = install_watches(&mut watcher, &common_dir) {
        log_daemon_event(
            "git_watch_degraded",
            &[
                ("project", state.project_root.display().to_string()),
                ("reason", "watch_install_failed".to_string()),
                ("error", e.to_string()),
            ],
        );
        state.health.set_degraded(true);
        state
            .health
            .set_coverage(ProjectWatchCoverage::DegradedPoll);
        degraded_poll_loop(&inner, &state).await;
        return;
    }

    state.health.set_degraded(false);
    state.health.set_coverage(ProjectWatchCoverage::Active);
    state.health.beat();

    Box::pin(debounce_loop(&inner, &state, &common_dir)).await;
    // Keep the watcher alive for the whole loop.
    drop(watcher);
}

/// Installs the minimal metadata watch set: `HEAD`, `packed-refs`, in-flight
/// operation markers (non-recursive per-file), and `refs/` + `worktrees/`
/// (recursive). Never the working tree.
fn install_watches(watcher: &mut notify::RecommendedWatcher, common: &Path) -> notify::Result<()> {
    // Per-file, non-recursive. Missing files are fine (packed-refs / markers may
    // not exist yet); ignore their NotFound so a repo without packed-refs still
    // watches HEAD.
    for file in ["HEAD", "packed-refs", "MERGE_HEAD"] {
        let path = common.join(file);
        let _ = watcher.watch(&path, RecursiveMode::NonRecursive);
    }
    // Rebase markers are directories that appear/disappear; watch the common
    // dir non-recursively so their creation/removal is observed even before
    // they exist. (Watching a not-yet-existing dir fails, so we lean on the
    // recursive refs/ + the common-dir file watches plus the debounce recheck.)
    // Recursive watches for the ref namespaces.
    for dir in ["refs", "worktrees"] {
        let path = common.join(dir);
        if path.is_dir() {
            watcher.watch(&path, RecursiveMode::Recursive)?;
        }
    }
    Ok(())
}

/// Translates a raw notify event into dirty-set marks. Does NOT re-derive git
/// state — it only records *what kind of path changed* so the debounce drain
/// can resolve the actual git state once, after quiescence.
fn classify_and_mark(state: &Arc<WatchState>, event: &notify::Event) {
    let is_remove = matches!(event.kind, EventKind::Remove(_));
    let is_create = matches!(event.kind, EventKind::Create(_));
    state.health.events_received.fetch_add(1, Ordering::Relaxed);

    // Cheap synchronous classification into the dirty set. We use `try_lock` to
    // stay non-blocking in the notify thread; on contention we still wake the
    // loop, which rechecks git state anyway, so no event is lost.
    if let Ok(mut dirty) = state.dirty.try_lock() {
        let now = Instant::now();
        dirty.dirty = true;
        if dirty.first_event.is_none() {
            dirty.first_event = Some(now);
        }
        dirty.last_event = Some(now);

        for path in &event.paths {
            let s = path.to_string_lossy();
            if let Some(idx) = s.find("/refs/heads/") {
                let branch = &s[idx + "/refs/heads/".len()..];
                // Git creates `<ref>.lock` beside a branch ref while updating
                // it. The sidecar is not a branch and may disappear before
                // the debounce drain, so never enqueue it for catch-up sync.
                let is_lock_sidecar = std::path::Path::new(branch)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"));
                if !branch.is_empty() && !is_lock_sidecar {
                    dirty.branches.insert(branch.to_string());
                }
                if is_remove {
                    dirty.gc_eligible = true;
                }
            } else if let Some(idx) = s.find("/worktrees/") {
                let rest = &s[idx + "/worktrees/".len()..];
                let name = rest.split('/').next().unwrap_or("");
                if !name.is_empty() {
                    if is_create {
                        dirty.new_worktrees.insert(name.to_string());
                    }
                    if is_remove {
                        dirty.gc_eligible = true;
                        dirty.worktree_removed = true;
                    }
                }
            }
        }
    } else {
        state.reconciliation_pending.store(true, Ordering::Release);
    }
    state.wake.notify_one();
    state.maintenance.wake();
}

/// Converts any callback event that could not record detailed path evidence
/// into one conservative reconciliation plan.
async fn materialize_pending_reconciliation(state: &WatchState) {
    if !state.reconciliation_pending.load(Ordering::Acquire) {
        return;
    }
    let mut dirty = state.dirty.lock().await;
    if state.reconciliation_pending.swap(false, Ordering::AcqRel) {
        let now = Instant::now();
        dirty.dirty = true;
        dirty.reconcile_metadata = true;
        dirty.first_event.get_or_insert(now);
        dirty.last_event = Some(now);
    }
}

/// The debounce state machine for a healthy watcher. Wakes on events, sleeps
/// until the quiet deadline or the hard cap (whichever comes first), then
/// drains and syncs. No busy polling.
async fn debounce_loop(inner: &Arc<GitWatcherInner>, state: &Arc<WatchState>, common: &Path) {
    let quiet = Duration::from_millis(inner.config.watch_debounce_ms);
    let max_delay = Duration::from_millis(inner.config.watch_max_delay_ms);

    #[cfg(test)]
    state.entered_debounce.notify_one();

    loop {
        if let Some(plan) = wait_for_retry_or_metadata(state).await {
            execute_plan(inner, state, common, plan).await;
            state.health.beat();
            continue;
        }
        state.health.beat();

        // Coalesce: keep extending the quiet window until it settles or we hit
        // the hard cap. If a rebase/merge is mid-flight, HOLD (keep waiting)
        // until the markers disappear so we sync exactly once, after.
        loop {
            materialize_pending_reconciliation(state).await;
            let (first, last) = {
                let dirty = state.dirty.lock().await;
                (dirty.first_event, dirty.last_event)
            };
            let now = Instant::now();
            let quiet_deadline = last.map(|l| l + quiet);
            let hard_deadline = first.map(|f| f + max_delay);

            // If an operation is in flight, do not fire yet — wait for the next
            // event (marker removal wakes us) or a short recheck tick.
            if operation_in_flight(common) {
                tokio::select! {
                    () = state.wake.notified() => { state.health.beat(); continue; }
                    () = tokio::time::sleep(Duration::from_secs(1)) => { continue; }
                }
            }

            // Fire when the quiet window elapsed, but never later than the cap.
            let fire_at = match (quiet_deadline, hard_deadline) {
                (Some(q), Some(h)) => q.min(h),
                (Some(q), None) => q,
                (None, Some(h)) => h,
                (None, None) => break, // nothing pending; back to outer wait
            };
            if now >= fire_at {
                break;
            }
            let sleep_for = fire_at - now;
            tokio::select! {
                () = state.wake.notified() => { state.health.beat(); }
                () = tokio::time::sleep(sleep_for) => {}
            }
        }

        // Drain and execute exactly one coalesced sync pass.
        let plan = {
            let mut dirty = state.dirty.lock().await;
            dirty.take()
        };
        if !plan.is_empty() {
            state.health.plans_drained.fetch_add(1, Ordering::Relaxed);
            #[cfg(test)]
            {
                state.drained_plans.fetch_add(1, Ordering::Relaxed);
                state.plan_drained.notify_one();
            }
            execute_plan(inner, state, common, plan).await;
        }
        state.health.beat();
    }
}

/// Waits for either a metadata event or the single bounded retry timer. A
/// retry wake without a due plan only refreshes the deadline after plan merge.
async fn wait_for_retry_or_metadata(state: &WatchState) -> Option<DirtyPlan> {
    loop {
        if let Some(deadline) = state.retry_deadline().await {
            tokio::select! {
                () = state.wake.notified() => return None,
                () = state.retry_wake.notified() => {}
                () = tokio::time::sleep_until(deadline) => {
                    if let Some(plan) = state.take_due_retry().await {
                        return Some(plan);
                    }
                }
            }
        } else {
            tokio::select! {
                () = state.wake.notified() => return None,
                () = state.retry_wake.notified() => {}
            }
        }
    }
}

/// True while a rebase/merge is mid-flight; the watcher holds during these and
/// fires exactly once after they clear.
fn operation_in_flight(common: &Path) -> bool {
    common.join("rebase-merge").exists()
        || common.join("rebase-apply").exists()
        || common.join("MERGE_HEAD").exists()
}

/// The degraded fallback: mtime-poll HEAD + packed-refs every 5 minutes and
/// sync when they advance. Used when the inotify watcher cannot be built or
/// dies (e.g. ENOSPC). Covers ONE project — never a global failure.
async fn degraded_poll_loop(inner: &Arc<GitWatcherInner>, state: &Arc<WatchState>) {
    loop {
        state.health.beat();
        for root in state.roots().await {
            planner::sync_snapshot(inner, state, &root, "degraded_poll").await;
        }
        if let Some(plan) = wait_for_degraded_retry(state).await {
            let common = state
                .common_dir
                .as_deref()
                .unwrap_or(state.project_root.as_path());
            execute_plan(inner, state, common, plan).await;
        }
    }
}

/// The degraded path still owns the retry timer when backstop is disabled.
async fn wait_for_degraded_retry(state: &WatchState) -> Option<DirtyPlan> {
    loop {
        if let Some(deadline) = state.retry_deadline().await {
            tokio::select! {
                () = tokio::time::sleep(DEGRADED_POLL_INTERVAL) => return None,
                () = state.retry_wake.notified() => {}
                () = tokio::time::sleep_until(deadline) => {
                    if let Some(plan) = state.take_due_retry().await {
                        return Some(plan);
                    }
                }
            }
        } else {
            tokio::select! {
                () = tokio::time::sleep(DEGRADED_POLL_INTERVAL) => return None,
                () = state.retry_wake.notified() => {}
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Backstop scheduler (design D5): a single daemon timer covering projects whose
/// watcher heartbeat is stale/absent, plus daily branch-store GC.
mod backstop {
    use super::*;

    pub(super) async fn run(
        watcher: GitWatcher,
        _profile_database: Arc<crate::global_db::RegisteredGlobalDb>,
    ) {
        let interval_mins = watcher.inner.config.backstop_interval_mins;
        if interval_mins == 0 {
            return; // disabled
        }
        let period = Duration::from_secs(interval_mins.saturating_mul(60).max(1));
        let mut ticker = tokio::time::interval(period);
        // Skip the immediate first tick so startup registration settles first.
        ticker.tick().await;

        let mut last_gc: Option<Instant> = None;
        let gc_period = Duration::from_hours(24);

        loop {
            ticker.tick().await;
            tick(&watcher, &mut last_gc, gc_period).await;
        }
    }

    async fn tick(watcher: &GitWatcher, last_gc: &mut Option<Instant>, gc_period: Duration) {
        let interval_secs = watcher
            .inner
            .config
            .backstop_interval_mins
            .saturating_mul(60);
        // Snapshot registered projects; cover those the watcher isn't keeping
        // fresh (stale/absent heartbeat) AND whose store is older than one
        // interval.
        let entries: Vec<(PathBuf, Arc<WatchState>)> = {
            let projects = watcher.inner.projects.lock().await;
            let mut entries: Vec<_> = projects
                .iter()
                .map(|(root, state)| (root.clone(), Arc::clone(state)))
                .collect();
            drop(projects);
            entries.extend(
                watcher
                    .inner
                    .degraded_projects
                    .lock()
                    .await
                    .iter()
                    .map(|(root, state)| (root.clone(), Arc::clone(state))),
            );
            entries
        };

        let run_gc_now = last_gc.is_none_or(|t| t.elapsed() >= gc_period);
        let mut gc_retry_needed = false;

        for (_watch_identity, state) in &entries {
            let snap = state.health.snapshot();
            for root in state.roots().await {
                let retained_graph = retained_project_graph(&watcher.inner, &root).await;
                let store_stale = match retained_graph.as_deref() {
                    Some(graph) => store_is_stale(graph, interval_secs).await,
                    None => false,
                };
                if snap.heartbeat_stale() && store_stale {
                    planner::sync_snapshot(&watcher.inner, state, &root, "backstop").await;
                }

                if run_gc_now
                    && let Some(cg) = retained_graph.as_ref()
                    && !super::store_maintenance::run_gc(&watcher.inner, cg).await
                {
                    gc_retry_needed = true;
                }
            }
        }

        if run_gc_now && !gc_retry_needed {
            *last_gc = Some(Instant::now());
        }
    }

    /// True when the project's store `last_sync_at` is older than one backstop
    /// interval. Returns `false` when the project is not indexed (nothing to
    /// backstop). The read-only open/read futures are `Send`, so they are
    /// awaited directly (see [`super::sync_project`]).
    async fn store_is_stale(cg: &TraceDecay, interval_secs: u64) -> bool {
        let last = cg.last_sync_timestamp().await;
        let age = super::now_secs() as i64 - last;
        age > interval_secs as i64
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
