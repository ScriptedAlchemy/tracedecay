//! Daemon git-metadata watcher (design D3) and scheduler backstop (D5).
//!
//! # Why this is safe (unlike the removed #80 working-tree watcher)
//!
//! The v6.x `notify-debouncer-full` watcher recursively watched the **working
//! tree** and drowned on monorepo `node_modules`/`target` churn. This watcher
//! watches **only git metadata** under `<git_common_dir>` — `HEAD`,
//! `packed-refs`, `refs/` and `worktrees/` — which is ~5-20 inotify watches per
//! repository and never fires on a source-file edit. That distinction is the
//! entire safety argument: we react to *git operations* (commit, checkout,
//! branch create, worktree add, rebase), not to editor saves.
//!
//! # Shape
//!
//! * One [`GitWatcher`] is held by the [`super::DaemonEngine`]; both the accept
//!   loop and `project_server` reach it to lazily [`GitWatcher::ensure_watching`]
//!   freshly-handshaken projects.
//! * Each repository common directory gets one supervised debounce task
//!   ([`repository_task`]) and carries the exact roots and git directories of
//!   every active linked worktree. Raw events wake the task via a
//!   [`tokio::sync::Notify`];
//!   the task sleeps until the quiet deadline
//!   (`watch_debounce_ms`) or the hard cap (`watch_max_delay_ms`), whichever is
//!   first — no busy polling.
//! * Debounce drains submit exact-frontier freshness requests to the canonical
//!   code-index scheduler. The watcher never opens or mutates a legacy graph.
//! * The [`backstop`] timer retries repositories whose watcher heartbeat is
//!   stale/absent through the same scheduler ingress.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::SyncConfig;

#[cfg(test)]
use super::maintenance::retention_maintenance_enabled;
#[cfg(test)]
use super::store_maintenance;
use super::{log_daemon_event, maintenance::MaintenanceCoordinator};

mod state;
use state::{WatchState, WorktreeRegistration};

/// Degraded watchers fall back to polling git metadata every 5 minutes.
const DEGRADED_POLL_INTERVAL: Duration = Duration::from_mins(5);
/// A heartbeat older than this is considered stale by the backstop/doctor.
/// Two debounce+max cycles of slack over the default so a healthy but busy
/// watcher is never treated as dead.
const HEARTBEAT_STALE_SECS: u64 = 120;
/// Healthy quiet repositories refresh liveness without submitting freshness.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Cap on the supervised-restart backoff.
const RESTART_BACKOFF_MAX: Duration = Duration::from_mins(1);
/// Hard bound on linked-worktree fanout and git-operation marker enumeration
/// for one repository owner.
const MAX_WORKTREES_PER_REPOSITORY: usize = 256;

/// Per-repository watcher health, readable by the backstop.
///
/// Timestamps are UNIX seconds (0 = never). Tests also retain acceptance and
/// degraded-fallback receipts without expanding the production health state.
#[derive(Debug, Default)]
struct ProjectHealth {
    /// Last time the watch task completed a poll cycle (event drain or degraded
    /// stat). Advances even when nothing needed syncing — it is a liveness
    /// signal, not a sync signal.
    last_heartbeat: AtomicU64,
    /// Test receipt for the last accepted watcher freshness request.
    #[cfg(test)]
    last_freshness_request: AtomicU64,
    /// Test receipt for the degraded cadence fallback.
    #[cfg(test)]
    degraded: std::sync::atomic::AtomicBool,
}

impl ProjectHealth {
    fn beat(&self) {
        self.last_heartbeat.store(now_secs(), Ordering::Relaxed);
    }
    #[cfg(test)]
    fn mark_requested(&self) {
        self.last_freshness_request
            .store(now_secs(), Ordering::Relaxed);
    }
    #[cfg(test)]
    fn set_degraded(&self, degraded: bool) {
        self.degraded.store(degraded, Ordering::Relaxed);
    }
    fn snapshot(&self) -> ProjectHealthSnapshot {
        ProjectHealthSnapshot {
            last_heartbeat: self.last_heartbeat.load(Ordering::Relaxed),
            #[cfg(test)]
            last_freshness_request: self.last_freshness_request.load(Ordering::Relaxed),
            #[cfg(test)]
            degraded: self.degraded.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of repository watcher health.
#[derive(Debug, Clone)]
struct ProjectHealthSnapshot {
    last_heartbeat: u64,
    #[cfg(test)]
    last_freshness_request: u64,
    #[cfg(test)]
    degraded: bool,
}

impl ProjectHealthSnapshot {
    /// True when the watcher has not reported a heartbeat within the staleness
    /// window (or never has). The backstop uses this to decide coverage.
    fn heartbeat_stale(&self) -> bool {
        let hb = self.last_heartbeat;
        hb == 0 || now_secs().saturating_sub(hb) > HEARTBEAT_STALE_SECS
    }
}

#[derive(Debug, Default)]
struct DirtySet {
    /// Any metadata event happened; every registered worktree needs an exact
    /// scheduler freshness request.
    dirty: bool,
    /// Path-level event detail was lost to callback lock contention. The next
    /// cycle still requests canonical reconciliation for every registered
    /// worktree.
    reconcile_metadata: bool,
    /// Instant of the first event since the last drain (for the hard cap).
    first_event: Option<Instant>,
    /// Instant of the most recent event (for the quiet-window deadline).
    last_event: Option<Instant>,
}

impl DirtySet {
    fn is_clean(&self) -> bool {
        !self.dirty && !self.reconcile_metadata
    }
    fn take(&mut self) -> bool {
        let pending = !self.is_clean();
        self.dirty = false;
        self.reconcile_metadata = false;
        self.first_event = None;
        self.last_event = None;
        pending
    }
}

/// The daemon-held git-metadata watcher. Cheap to clone (all `Arc` inside), and
/// [`Default`] so `DaemonEngine` can derive `Default`.
#[derive(Clone)]
pub struct GitWatcher {
    inner: Arc<GitWatcherInner>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "watcher admission rejection must remain a truthful fallback state"]
pub(super) enum GitWatcherAdmission {
    Ready,
    Disabled,
    Capacity,
    IdentityUnavailable,
}

struct WatchIdentity {
    canonical_root: PathBuf,
    common_dir: PathBuf,
    git_dir: PathBuf,
}

pub(super) struct GitWatcherInner {
    pub(super) config: SyncConfig,
    maintenance: MaintenanceCoordinator,
    code_index_schedulers: Option<super::code_index_scheduler::CodeIndexSchedulerRegistryV1>,
    cancellation: crate::application::context::CancellationToken,
    /// Whether watching is enabled at all (`auto_watch`). When false every
    /// method is a no-op so the daemon runs exactly as before this feature.
    enabled: bool,
    /// Canonical git common directory → repository-scoped watch state.
    projects: Mutex<HashMap<PathBuf, Arc<WatchState>>>,
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
            false,
            MaintenanceCoordinator::default(),
            None,
        )
    }

    fn from_parts(
        config: SyncConfig,
        enabled: bool,
        maintenance: MaintenanceCoordinator,
        code_index_schedulers: Option<super::code_index_scheduler::CodeIndexSchedulerRegistryV1>,
    ) -> Self {
        Self {
            inner: Arc::new(GitWatcherInner {
                config,
                maintenance,
                code_index_schedulers,
                cancellation: crate::application::context::CancellationToken::new(),
                enabled,
                projects: Mutex::new(HashMap::new()),
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
        Self::new_with_scheduler(
            config,
            MaintenanceCoordinator::default(),
            super::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(32),
        )
    }

    /// Builds a watcher bound to the daemon's canonical code-index scheduler.
    pub(super) fn new_with_scheduler(
        config: SyncConfig,
        maintenance: MaintenanceCoordinator,
        code_index_schedulers: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    ) -> Self {
        let enabled = config.auto_watch;
        Self::from_parts(config, enabled, maintenance, Some(code_index_schedulers))
    }

    // Doctor watcher-health surface (follow-up wiring).
    pub fn is_enabled(&self) -> bool {
        self.inner.enabled
    }

    /// Registers the recently-seen projects and starts the backstop timer.
    ///
    /// Called once from `run_foreground_unix` after the engine is built. Safe to
    /// call on a disabled watcher (no-op).
    pub(super) async fn spawn(&self) {
        if !self.inner.enabled || self.inner.shutting_down.load(Ordering::Acquire) {
            return;
        }
        // Startup does not manufacture project owners from registry paths.
        // Active daemon handshakes call `ensure_watching` after publishing the
        // retained project server and graph handle.

        let watcher = self.clone();
        let handle = tokio::spawn(async move {
            backstop::run(watcher).await;
        });
        *self.inner.backstop_task.lock().await = Some(handle);
    }

    /// Lazily starts watching `project_root` if not already watched and under
    /// the repository cap. Linked worktrees register distinct scheduler roots
    /// on one common-directory watcher.
    pub async fn ensure_watching(&self, project_root: &Path) -> GitWatcherAdmission {
        if !self.inner.enabled || self.inner.shutting_down.load(Ordering::Acquire) {
            return GitWatcherAdmission::Disabled;
        }
        let Some(identity) = watch_identity(project_root) else {
            return GitWatcherAdmission::IdentityUnavailable;
        };
        let WatchIdentity {
            canonical_root,
            common_dir,
            git_dir,
        } = identity;

        let mut projects = self.inner.projects.lock().await;
        if let Some(state) = projects.get(&common_dir) {
            return match state.register_worktree(
                canonical_root,
                git_dir,
                MAX_WORKTREES_PER_REPOSITORY,
            ) {
                WorktreeRegistration::Ready => GitWatcherAdmission::Ready,
                WorktreeRegistration::Capacity => GitWatcherAdmission::Capacity,
            };
        }
        if projects.len() >= self.inner.config.watch_max_projects {
            // Capacity is repository-scoped so linked worktrees never consume
            // additional OS-watcher slots.
            return GitWatcherAdmission::Capacity;
        }

        let state = Arc::new(WatchState::new(
            common_dir.clone(),
            canonical_root,
            git_dir,
            self.inner.maintenance.clone(),
        ));
        projects.insert(common_dir.clone(), Arc::clone(&state));
        drop(projects);

        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(supervise_repository(inner, Arc::clone(&state)));
        *state.task.lock().await = Some(handle);

        log_daemon_event(
            "git_watch_started",
            &[("git_common_dir", common_dir.display().to_string())],
        );
        GitWatcherAdmission::Ready
    }

    /// Stops every watcher-owned task and joins it before database shutdown.
    pub async fn shutdown(&self) {
        if !self.inner.enabled || self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }

        self.inner.cancellation.cancel();
        if let Some(handle) = self.inner.backstop_task.lock().await.take() {
            let _ = handle.await;
        }

        let states: Vec<Arc<WatchState>> = {
            let mut projects = self.inner.projects.lock().await;
            projects.drain().map(|(_, state)| state).collect()
        };
        for state in states {
            if let Some(handle) = state.task.lock().await.take() {
                let _ = handle.await;
            }
        }
    }

    /// A doctor-facing snapshot of every registered project's watch health.
    #[cfg(test)]
    async fn health_report(&self) -> Vec<(PathBuf, ProjectHealthSnapshot)> {
        let projects = self.inner.projects.lock().await;
        let mut out: Vec<_> = projects
            .values()
            .flat_map(|state| {
                state
                    .worktree_roots()
                    .into_iter()
                    .map(|root| (root, state.health.snapshot()))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn canonical_git_metadata_path(project_root: &Path, path: &Path) -> Option<PathBuf> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    resolved.canonicalize().ok()
}

fn watch_identity(project_root: &Path) -> Option<WatchIdentity> {
    let canonical_root = project_root.canonicalize().ok()?;
    let repository = gix::discover(&canonical_root).ok()?;
    let common_dir = canonical_git_metadata_path(&canonical_root, repository.common_dir())?;
    let git_dir = canonical_git_metadata_path(&canonical_root, repository.git_dir())?;
    Some(WatchIdentity {
        canonical_root,
        common_dir,
        git_dir,
    })
}

fn worktree_git_dir(project_root: &Path) -> Option<PathBuf> {
    watch_identity(project_root).map(|identity| identity.git_dir)
}

/// Supervises one repository's watch task: on panic, restart with capped
/// exponential backoff so a transient watcher failure never permanently drops a
/// project (the backstop still covers it in the meantime).
async fn supervise_repository(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let inner_c = Arc::clone(&inner);
        let state_c = Arc::clone(&state);
        let mut task =
            tokio::spawn(async move { Box::pin(repository_task(inner_c, state_c)).await });
        let result = tokio::select! {
            biased;
            () = inner.cancellation.cancelled() => {
                task.abort();
                let _ = task.await;
                return;
            }
            result = &mut task => result,
        };
        match result {
            Ok(()) => return, // clean exit (watcher gave up gracefully)
            Err(join_err) if join_err.is_cancelled() => return,
            Err(_panic) => {
                log_daemon_event(
                    "git_watch_restart",
                    &[
                        ("git_common_dir", state.common_dir.display().to_string()),
                        ("backoff_ms", backoff.as_millis().to_string()),
                    ],
                );
                tokio::select! {
                    biased;
                    () = inner.cancellation.cancelled() => return,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
            }
        }
    }
}

/// One repository event loop. The watcher is rebuilt when another linked
/// worktree registers so its per-worktree operation-marker directory joins the
/// same small metadata watch set.
async fn repository_task(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    loop {
        let wake_state = Arc::clone(&state);
        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => classify_and_mark(&wake_state, &event),
                Err(_) => mark_reconciliation_pending(&wake_state),
            });

        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                log_daemon_event(
                    "git_watch_degraded",
                    &[
                        ("git_common_dir", state.common_dir.display().to_string()),
                        ("reason", "watcher_build_failed".to_string()),
                        ("error", error.to_string()),
                    ],
                );
                #[cfg(test)]
                state.health.set_degraded(true);
                degraded_poll_loop(&inner, &state).await;
                return;
            }
        };

        if let Err(error) = install_watches(&mut watcher, &state) {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("git_common_dir", state.common_dir.display().to_string()),
                    ("reason", "watch_install_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            #[cfg(test)]
            state.health.set_degraded(true);
            degraded_poll_loop(&inner, &state).await;
            return;
        }

        #[cfg(test)]
        state.health.set_degraded(false);
        state.health.beat();

        tokio::select! {
            () = debounce_loop(&inner, &state) => return,
            () = state.reconfigure.notified() => {
                // A canceled debounce future may already have consumed the
                // event wake. Preserve its dirty evidence across reconfigure.
                if !state.dirty.lock().await.is_clean() {
                    state.wake.notify_one();
                }
            }
        }
        drop(watcher);
    }
}

/// Installs one repository's minimal metadata watch set. Directories that can
/// create operation markers are watched non-recursively; ref registries are
/// recursive. The working trees and object database are never watched.
fn install_watches(
    watcher: &mut notify::RecommendedWatcher,
    state: &WatchState,
) -> notify::Result<()> {
    let common = &state.common_dir;
    watcher.watch(common, RecursiveMode::NonRecursive)?;
    for dir in ["refs", "worktrees"] {
        let path = common.join(dir);
        if path.is_dir() {
            watcher.watch(&path, RecursiveMode::Recursive)?;
        }
    }
    for git_dir in state.git_dirs() {
        if git_dir != *common {
            watcher.watch(&git_dir, RecursiveMode::NonRecursive)?;
        }
    }
    Ok(())
}

/// Translates a raw notify event into dirty-set marks. Does NOT re-derive git
/// state — it only records *what kind of path changed* so the debounce drain
/// can resolve the actual git state once, after quiescence.
fn classify_and_mark(state: &Arc<WatchState>, event: &notify::Event) {
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
    } else {
        state.reconciliation_pending.store(true, Ordering::Release);
    }
    if matches!(event.kind, EventKind::Create(_))
        && event.paths.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name == "refs" || name == "worktrees")
        })
    {
        state.reconfigure.notify_one();
    }
    state.wake.notify_one();
    state.maintenance.wake();
}

fn mark_reconciliation_pending(state: &WatchState) {
    state.reconciliation_pending.store(true, Ordering::Release);
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
async fn debounce_loop(inner: &Arc<GitWatcherInner>, state: &Arc<WatchState>) {
    let quiet = Duration::from_millis(inner.config.watch_debounce_ms);
    let max_delay = Duration::from_millis(inner.config.watch_max_delay_ms);

    #[cfg(test)]
    state.entered_debounce.notify_one();

    loop {
        // Stay observable even when the repository is quiet. This heartbeat
        // prevents the backstop from turning inactivity into periodic indexing.
        tokio::select! {
            () = state.wake.notified() => {}
            () = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                state.health.beat();
                continue;
            }
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

            let operation_state = operation_state(state, MAX_WORKTREES_PER_REPOSITORY);
            // If an operation is in flight, do not fire yet — wait for the next
            // event (marker removal wakes us) or a short recheck tick.
            if operation_state == OperationState::InFlight {
                tokio::select! {
                    () = state.wake.notified() => { state.health.beat(); continue; }
                    () = tokio::time::sleep(Duration::from_secs(1)) => { continue; }
                }
            }

            // Fire when the quiet window elapsed, but never later than the cap.
            let fire_at = match (operation_state, quiet_deadline, hard_deadline) {
                // Incomplete registry evidence cannot safely use the quiet
                // deadline, but also cannot stall the repository forever.
                (OperationState::Incomplete, _, Some(h)) => h,
                (OperationState::Incomplete, Some(q), None) => q,
                (OperationState::Incomplete, None, None) => break,
                (_, Some(q), Some(h)) => q.min(h),
                (_, Some(q), None) => q,
                (_, None, Some(h)) => h,
                (_, None, None) => break, // nothing pending; back to outer wait
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
        let pending = {
            let mut dirty = state.dirty.lock().await;
            dirty.take()
        };
        if pending {
            #[cfg(test)]
            {
                state.drained_plans.fetch_add(1, Ordering::Relaxed);
                state.plan_drained.notify_one();
            }
            request_freshness_for_repository(inner, state);
        }
        state.health.beat();
    }
}

/// Bounded observation of worktree operation markers. The common directory
/// alone is insufficient: linked worktrees keep their markers under
/// `<common>/worktrees/<name>`. Incomplete enumeration remains distinct from
/// idle so debounce waits for its hard deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationState {
    Idle,
    InFlight,
    Incomplete,
}

fn operation_state(state: &WatchState, max_worktrees: usize) -> OperationState {
    const OPERATION_MARKERS: &[&str] = &[
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
    ];
    let Some(git_dirs) = state.operation_git_dirs(max_worktrees) else {
        return OperationState::Incomplete;
    };
    if git_dirs.iter().any(|git_dir| {
        OPERATION_MARKERS
            .iter()
            .any(|marker| git_dir.join(marker).exists())
    }) {
        OperationState::InFlight
    } else {
        OperationState::Idle
    }
}

/// Routes a coalesced metadata cycle through the canonical scheduler.
///
/// Exact identity resolution happens before scheduler publication. The
/// scheduler owns gix status, changed-candidate evidence, generation assembly,
/// and its short CAS publication; this watcher owns none of those authorities.
fn request_freshness_for_repository(inner: &GitWatcherInner, state: &WatchState) {
    use super::code_index_scheduler::{GitStateChangeRequestV1, identity::IndexingIdentityV1};

    let Some(code_index_schedulers) = inner.code_index_schedulers.as_ref() else {
        return;
    };
    state.prune_missing_worktrees();
    let mut accepted = false;
    let mut retry = false;
    for project_root in state.worktree_roots() {
        if inner.cancellation.is_cancelled() {
            return;
        }
        let identity = match IndexingIdentityV1::resolve(&project_root) {
            Ok(identity) => identity,
            Err(error) => {
                log_daemon_event(
                    "git_watch_freshness_rejected",
                    &[
                        ("project", project_root.display().to_string()),
                        ("reason", "identity_unavailable".to_string()),
                        ("error", error.to_string()),
                    ],
                );
                continue;
            }
        };
        match code_index_schedulers.request_for_root(&project_root, identity) {
            GitStateChangeRequestV1::Accepted => {
                accepted = true;
                log_daemon_event(
                    "git_watch_freshness_requested",
                    &[("project", project_root.display().to_string())],
                );
            }
            GitStateChangeRequestV1::Busy | GitStateChangeRequestV1::IdentityMismatch => {
                retry = true;
            }
            GitStateChangeRequestV1::Unmounted => {}
        }
    }

    if accepted {
        #[cfg(test)]
        state.health.mark_requested();
    }
    if retry {
        // Preserve one bounded pending cycle. Notify stores at most one permit,
        // so repeated Busy outcomes cannot form an unbounded queue.
        if let Ok(mut dirty) = state.dirty.try_lock() {
            let now = Instant::now();
            dirty.dirty = true;
            dirty.first_event.get_or_insert(now);
            dirty.last_event = Some(now);
        } else {
            state.reconciliation_pending.store(true, Ordering::Release);
        }
        state.wake.notify_one();
    }
}

/// The degraded fallback: request one authoritative scheduler reconciliation
/// every 5 minutes. Used when the inotify watcher cannot be built or dies
/// (e.g. ENOSPC). A fixed cadence is deliberate: filesystem mtimes cannot
/// faithfully summarize loose-ref content changes, while the scheduler's gix
/// reconciliation can.
async fn degraded_poll_loop(inner: &Arc<GitWatcherInner>, state: &Arc<WatchState>) {
    loop {
        tokio::select! {
            biased;
            () = inner.cancellation.cancelled() => return,
            () = tokio::time::sleep(DEGRADED_POLL_INTERVAL) => {}
        }
        state.health.beat();
        request_freshness_for_repository(inner, state);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Backstop scheduler (design D5): a single daemon timer covering repositories
/// whose watcher heartbeat is stale or absent.
mod backstop {
    use super::*;

    pub(super) async fn run(watcher: GitWatcher) {
        let interval_mins = watcher.inner.config.backstop_interval_mins;
        if interval_mins == 0 {
            return; // disabled
        }
        let period = Duration::from_secs(interval_mins.saturating_mul(60).max(1));
        let mut ticker = tokio::time::interval(period);
        // Skip the immediate first tick so startup registration settles first.
        ticker.tick().await;

        loop {
            tokio::select! {
                biased;
                () = watcher.inner.cancellation.cancelled() => return,
                _ = ticker.tick() => {}
            }
            tick(&watcher).await;
        }
    }

    async fn tick(watcher: &GitWatcher) {
        let entries: Vec<Arc<WatchState>> = {
            let projects = watcher.inner.projects.lock().await;
            projects.values().cloned().collect()
        };

        for state in &entries {
            let snap = state.health.snapshot();
            if snap.heartbeat_stale() {
                request_freshness_for_repository(&watcher.inner, state);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
