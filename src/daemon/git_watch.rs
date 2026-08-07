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
//! * The [`backstop`] timer is the freshness floor for every registered
//!   repository: each due root submits a freshness request through the same
//!   scheduler ingress. A live heartbeat proves only watcher-task liveness —
//!   the watcher reacts to git metadata alone — so liveness never vetoes
//!   coverage.

#![cfg(unix)]

use std::collections::{BTreeSet, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant as StdInstant};

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use notify::EventKind;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracedecay_runtime_core::cancellation::MonotonicDeadline;
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentityOutcome, discover_repository_identity,
};

use crate::config::SyncConfig;

#[cfg(test)]
use super::maintenance::retention_maintenance_enabled;
#[cfg(test)]
use super::store_maintenance;
use super::{log_daemon_event, maintenance::MaintenanceCoordinator};

mod admission;
mod backstop;
mod health;
mod identity;
mod overflow;
mod ownership;
mod state;
mod watch_plan;
#[cfg(test)]
use health::HEARTBEAT_STALE_MILLIS;
#[cfg(test)]
use health::ProjectHealthSnapshot;
use health::ProjectWatchStatus;
use identity::resolve_watch_identity;
use ownership::{GitWatcherShutdownOutcome, join_watcher_tasks};
#[cfg(test)]
use ownership::{GitWatcherTaskFailure, GitWatcherTaskFailureKind, GitWatcherTaskOwner};
#[cfg(test)]
use state::WorktreeRegistration;
use state::{WatchCancellation, WatchState};
#[cfg(test)]
use watch_plan::{MAX_METADATA_WATCH_DIRECTORIES, observe_watch_plan};
use watch_plan::{WatchInstallFailure, WatchPlanFailure, install_watches};

/// Degraded watchers fall back to polling git metadata every 5 minutes.
const DEGRADED_POLL_INTERVAL: Duration = Duration::from_mins(5);
/// Healthy quiet repositories refresh liveness without submitting freshness.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Cap on the supervised-restart backoff.
const RESTART_BACKOFF_MAX: Duration = Duration::from_mins(1);
/// Hard bound on linked-worktree fanout and git-operation marker enumeration
/// for one repository owner.
const MAX_WORKTREES_PER_REPOSITORY: usize = 256;
/// Deadline for one watcher-owned metadata or identity observation.
pub(super) const GIT_OBSERVATION_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct DirtySet {
    /// Any metadata event happened; every registered worktree needs an exact
    /// scheduler freshness request.
    dirty: bool,
    /// Path-level event detail was lost to callback lock contention. The next
    /// cycle still requests canonical reconciliation for every registered
    /// worktree.
    reconcile_metadata: bool,
    /// Exact mounted worktrees evidenced by path-local metadata events.
    affected_roots: BTreeSet<PathBuf>,
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
        self.affected_roots.clear();
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
    ShuttingDown,
    Capacity,
    NotRepository,
    IdentityUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "watcher start state must remain explicit"]
pub(super) enum GitWatcherStart {
    Started,
    AlreadyStarted,
    Disabled,
    ShuttingDown,
}

pub(super) struct GitWatcherInner {
    #[cfg(test)]
    pub(super) config: SyncConfig,
    maintenance: MaintenanceCoordinator,
    code_index_schedulers: Option<super::code_index_scheduler::CodeIndexSchedulerRegistryV1>,
    cancellation: crate::application::context::CancellationToken,
    /// Whether watching is enabled at all (`auto_watch`). When false every
    /// method is a no-op so the daemon runs exactly as before this feature.
    enabled: bool,
    admission: std::sync::Mutex<()>,
    /// Canonical git common directory → repository-scoped watch state.
    projects: Mutex<HashMap<PathBuf, Arc<WatchState>>>,
    /// Single-flight retry owners for roots whose identity discovery timed
    /// out: a bounded git timeout is uncertainty, not absence, so admission
    /// arms a backoff retry instead of leaving the repository unwatched until
    /// the next handshake. Keyed by requested project root.
    identity_retries: std::sync::Mutex<HashMap<PathBuf, JoinHandle<()>>>,
    /// Bounded roster of capacity-refused repositories kept on the backstop's
    /// scheduler-ingress freshness floor until a watch slot frees.
    overflow: std::sync::Mutex<overflow::OverflowRoster>,
    /// Single backstop scheduler task, owned so shutdown can cancel and join it.
    backstop_task: Mutex<Option<JoinHandle<()>>>,
    shutting_down: AtomicBool,
    shutdown_completion: Mutex<Option<Shared<BoxFuture<'static, GitWatcherShutdownOutcome>>>>,
    #[cfg(test)]
    repository_publication_probe: ownership::PublicationRaceProbe,
    #[cfg(test)]
    spawn_publication_probe: ownership::PublicationRaceProbe,
    #[cfg(test)]
    shutdown_requested: tokio::sync::Notify,
    #[cfg(test)]
    lifecycle_receipts: ownership::LifecycleLinearizationReceipts,
}

impl Default for GitWatcher {
    fn default() -> Self {
        // Only satisfies `DaemonEngine: Default` before composition replaces
        // it. Project settings are admitted later from pinned server config.
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
        _config: SyncConfig,
        enabled: bool,
        maintenance: MaintenanceCoordinator,
        code_index_schedulers: Option<super::code_index_scheduler::CodeIndexSchedulerRegistryV1>,
    ) -> Self {
        Self {
            inner: Arc::new(GitWatcherInner {
                #[cfg(test)]
                config: _config,
                maintenance,
                code_index_schedulers,
                cancellation: crate::application::context::CancellationToken::new(),
                enabled,
                admission: std::sync::Mutex::new(()),
                projects: Mutex::new(HashMap::new()),
                identity_retries: std::sync::Mutex::new(HashMap::new()),
                overflow: std::sync::Mutex::new(overflow::OverflowRoster::default()),
                backstop_task: Mutex::new(None),
                shutting_down: AtomicBool::new(false),
                shutdown_completion: Mutex::new(None),
                #[cfg(test)]
                repository_publication_probe: ownership::PublicationRaceProbe::default(),
                #[cfg(test)]
                spawn_publication_probe: ownership::PublicationRaceProbe::default(),
                #[cfg(test)]
                shutdown_requested: tokio::sync::Notify::new(),
                #[cfg(test)]
                lifecycle_receipts: ownership::LifecycleLinearizationReceipts::default(),
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
        let enabled = config.auto_watch;
        Self::from_parts(
            config,
            enabled,
            MaintenanceCoordinator::default(),
            Some(super::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(32)),
        )
    }

    /// Builds a watcher bound to the daemon's canonical code-index scheduler.
    pub(super) fn new_with_canonical_scheduler(
        maintenance: MaintenanceCoordinator,
        code_index_schedulers: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    ) -> Self {
        // The stored config is a test-constructor seam only; production
        // debounce, caps, fallback cadence, and activation live on WatchState
        // and come from `ensure_watching_with_config`.
        Self::from_parts(
            SyncConfig::default(),
            true,
            maintenance,
            Some(code_index_schedulers),
        )
    }

    #[cfg(test)]
    pub(super) fn new_with_scheduler(
        config: SyncConfig,
        maintenance: MaintenanceCoordinator,
        code_index_schedulers: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    ) -> Self {
        // Project admission is gated by the exact pinned project config. The
        // daemon owner itself remains available so a project that explicitly
        // enables watching can activate even though the legacy process
        // default is off.
        Self::from_parts(config, true, maintenance, Some(code_index_schedulers))
    }

    /// Starts synchronous shutdown fencing without waiting for retained tasks.
    pub(super) fn cancel(&self) {
        #[cfg(test)]
        self.inner.shutdown_requested.notify_one();
        let _admission = self
            .inner
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.inner.shutting_down.store(true, Ordering::Release);
        #[cfg(test)]
        self.inner.lifecycle_receipts.record_shutdown();
        self.inner.cancellation.cancel();
    }

    /// Registers the recently-seen projects and starts the backstop timer.
    ///
    /// Called once from `run_foreground_unix` after the engine is built. Safe to
    /// call on a disabled watcher (no-op).
    pub(super) async fn spawn(&self) -> GitWatcherStart {
        if !self.inner.enabled {
            return GitWatcherStart::Disabled;
        }
        // Startup does not manufacture project owners from registry paths.
        // Active daemon handshakes call `ensure_watching` after publishing the
        // retained project server and graph handle.

        let mut retained = self.inner.backstop_task.lock().await;
        let _admission = self
            .inner
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return GitWatcherStart::ShuttingDown;
        }
        if retained.is_some() {
            return GitWatcherStart::AlreadyStarted;
        }
        #[cfg(test)]
        self.inner.spawn_publication_probe.block_if_armed();
        let watcher = self.clone();
        let handle = tokio::spawn(async move {
            backstop::run(watcher).await;
        });
        *retained = Some(handle);
        #[cfg(test)]
        self.inner.lifecycle_receipts.record_spawn();
        GitWatcherStart::Started
    }

    /// Stops every watcher-owned task and joins it before database shutdown.
    pub async fn shutdown(&self) -> GitWatcherShutdownOutcome {
        if !self.inner.enabled {
            return GitWatcherShutdownOutcome::default();
        }
        self.cancel();
        let completion = {
            let mut retained = self.inner.shutdown_completion.lock().await;
            if let Some(completion) = retained.as_ref() {
                completion.clone()
            } else {
                let inner = Arc::clone(&self.inner);
                let completion = async move { join_watcher_tasks(inner).await }
                    .boxed()
                    .shared();
                *retained = Some(completion.clone());
                completion
            }
        };
        completion.await
    }

    /// A doctor-facing health value for one project's watch coverage.
    /// Read-only: registered state, overflow-roster membership, and the typed
    /// watch status — no git IO and no store opens.
    pub(super) async fn health_value(&self, project_root: Option<&Path>) -> serde_json::Value {
        if !self.inner.enabled {
            return serde_json::json!({
                "status": "disabled",
                "coverage": serde_json::Value::Null,
                "reason": "auto_watch_disabled",
            });
        }
        let Some(project_root) = project_root else {
            return serde_json::json!({
                "status": "unavailable",
                "coverage": serde_json::Value::Null,
                "reason": "project_path_missing",
            });
        };
        let canonical = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let state = {
            let projects = self.inner.projects.lock().await;
            projects
                .values()
                .find(|state| state.worktree_roots().contains(&canonical))
                .cloned()
        };
        let Some(state) = state else {
            let overflowed = self
                .inner
                .overflow
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&canonical);
            if overflowed {
                return serde_json::json!({
                    "status": "degraded",
                    "coverage": "backstop_overflow",
                    "reason": "watch_capacity_reached",
                    "project_root": canonical,
                });
            }
            return serde_json::json!({
                "status": "unavailable",
                "coverage": serde_json::Value::Null,
                "reason": "project_not_registered",
                "project_root": canonical,
            });
        };
        let snapshot = state.health.snapshot();
        let heartbeat_pending = snapshot.last_heartbeat == 0;
        let heartbeat_stale = snapshot.heartbeat_stale();
        let degraded = snapshot.status.is_degraded();
        serde_json::json!({
            "status": if degraded || (heartbeat_stale && !heartbeat_pending) {
                "degraded"
            } else if heartbeat_pending {
                "starting"
            } else {
                "healthy"
            },
            "coverage": if degraded { "degraded_poll" } else { "metadata_watch" },
            "reason": watch_status_reason(snapshot.status, heartbeat_pending, heartbeat_stale),
            "git_common_dir": state.common_dir,
            "project_root": canonical,
            "watched_roots": state.worktree_roots(),
            "heartbeat_stale": heartbeat_stale,
        })
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

/// The typed reason string behind one project's doctor-facing watch health,
/// or `Null` for a healthy active watch.
fn watch_status_reason(
    status: ProjectWatchStatus,
    heartbeat_pending: bool,
    heartbeat_stale: bool,
) -> serde_json::Value {
    let reason = match status {
        ProjectWatchStatus::WatchPlanCapacity => Some("watch_plan_capacity"),
        ProjectWatchStatus::WatchPlanUnavailable => Some("watch_plan_unavailable"),
        ProjectWatchStatus::NotifyCapacity => Some("notify_capacity"),
        ProjectWatchStatus::NotifyBackend => Some("notify_backend"),
        ProjectWatchStatus::Initializing | ProjectWatchStatus::Active => {
            if heartbeat_pending {
                Some("heartbeat_pending")
            } else if heartbeat_stale {
                Some("heartbeat_stale")
            } else {
                None
            }
        }
    };
    match reason {
        Some(reason) => serde_json::Value::String(reason.to_string()),
        None => serde_json::Value::Null,
    }
}

/// Supervises one repository's watch task: on panic, restart with capped
/// exponential backoff so a transient watcher failure never permanently drops a
/// project (the backstop still covers it in the meantime).
async fn supervise_repository(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    let mut backoff = Duration::from_millis(500);
    let cancellation = state.cancellation(&inner.cancellation);
    loop {
        let result = AssertUnwindSafe(repository_task(Arc::clone(&inner), Arc::clone(&state)))
            .catch_unwind()
            .await;
        match result {
            Ok(()) => return,
            Err(_) if cancellation.is_cancelled() => return,
            Err(_) => {
                log_daemon_event(
                    "git_watch_restart",
                    &[
                        ("git_common_dir", state.common_dir.display().to_string()),
                        ("backoff_ms", backoff.as_millis().to_string()),
                    ],
                );
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
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
    let cancellation = state.cancellation(&inner.cancellation);
    loop {
        if cancellation.is_cancelled() {
            return;
        }
        let wake_state = Arc::clone(&state);
        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => classify_and_mark(&wake_state, &event),
                Err(error) => mark_notify_failure(&wake_state, &error),
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
                state.health.set_status(ProjectWatchStatus::NotifyBackend);
                degraded_poll_loop(&inner, &state, &cancellation).await;
                return;
            }
        };

        if let Err(error) =
            install_watches(&mut watcher, Arc::clone(&state), cancellation.clone()).await
        {
            let status = match error {
                WatchInstallFailure::Plan(WatchPlanFailure::Capacity) => {
                    ProjectWatchStatus::WatchPlanCapacity
                }
                WatchInstallFailure::Plan(_) => ProjectWatchStatus::WatchPlanUnavailable,
                WatchInstallFailure::Notify(ref error) if is_notify_capacity_error(error) => {
                    ProjectWatchStatus::NotifyCapacity
                }
                WatchInstallFailure::Notify(_) => ProjectWatchStatus::NotifyBackend,
            };
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("git_common_dir", state.common_dir.display().to_string()),
                    ("reason", "watch_install_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            state.health.set_status(status);
            state.reconciliation_pending.store(true, Ordering::Release);
            degraded_poll_loop(&inner, &state, &cancellation).await;
            return;
        }

        state.health.set_status(ProjectWatchStatus::Active);
        state.health.beat();

        match debounce_loop(&inner, &state, &cancellation).await {
            DebounceExit::Cancelled => return,
            DebounceExit::Reconfigure => {
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

/// Translates a raw notify event into dirty-set marks. Does NOT re-derive git
/// state — it only records *what kind of path changed* so the debounce drain
/// can resolve the actual git state once, after quiescence.
fn classify_and_mark(state: &Arc<WatchState>, event: &notify::Event) {
    let event_roots = state.event_roots(&event.paths);
    state.clear_retry();
    // Cheap synchronous classification into the dirty set. We use `try_lock` to
    // stay non-blocking in the notify thread; on contention we still wake the
    // loop, which rechecks git state anyway, so no event is lost.
    if let Ok(mut dirty) = state.dirty.try_lock() {
        let now = Instant::now();
        dirty.dirty = true;
        match event_roots {
            Some(roots) => dirty.affected_roots.extend(roots),
            None => dirty.reconcile_metadata = true,
        }
        if dirty.first_event.is_none() {
            dirty.first_event = Some(now);
        }
        dirty.last_event = Some(now);
    } else {
        state.reconciliation_pending.store(true, Ordering::Release);
    }
    if matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
    ) && event.paths.iter().any(|path| {
        path.is_dir()
            && (path.starts_with(state.common_dir.join("refs"))
                || path.starts_with(state.common_dir.join("worktrees")))
    }) {
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

fn is_notify_capacity_error(error: &notify::Error) -> bool {
    matches!(error.kind, notify::ErrorKind::MaxFilesWatch)
}

fn mark_notify_failure(state: &WatchState, error: &notify::Error) {
    let status = if is_notify_capacity_error(error) {
        ProjectWatchStatus::NotifyCapacity
    } else {
        ProjectWatchStatus::NotifyBackend
    };
    state.health.set_status(status);
    log_daemon_event(
        "git_watch_notify_failed",
        &[
            ("git_common_dir", state.common_dir.display().to_string()),
            ("status", format!("{status:?}")),
            ("error", error.to_string()),
        ],
    );
    mark_reconciliation_pending(state);
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
enum DebounceExit {
    Cancelled,
    Reconfigure,
}

async fn debounce_loop(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    cancellation: &WatchCancellation,
) -> DebounceExit {
    let timing = state.effective_timing();
    let quiet = timing.debounce;
    let max_delay = timing.max_delay;

    #[cfg(test)]
    state.entered_debounce.notify_one();

    loop {
        // Stay observable even when the repository is quiet. This heartbeat
        // prevents the backstop from turning inactivity into periodic indexing.
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return DebounceExit::Cancelled,
            () = state.reconfigure.notified() => return DebounceExit::Reconfigure,
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
            let (first, last, affected_roots) = {
                let dirty = state.dirty.lock().await;
                (
                    dirty.first_event,
                    dirty.last_event,
                    (!dirty.reconcile_metadata)
                        .then(|| dirty.affected_roots.clone())
                        .filter(|roots| !roots.is_empty()),
                )
            };
            let now = Instant::now();
            let quiet_deadline = last.map(|l| l + quiet);
            let hard_deadline = first.map(|f| f + max_delay);

            let operation_state = match observe_operation_state(
                Arc::clone(state),
                cancellation.clone(),
                affected_roots,
            )
            .await
            {
                OperationObservation::State(state) => state,
                OperationObservation::Cancelled => return DebounceExit::Cancelled,
            };
            // If an operation is in flight, do not fire yet — wait for the next
            // event (marker removal wakes us) or a short recheck tick.
            if operation_state == OperationState::InFlight {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return DebounceExit::Cancelled,
                    () = state.reconfigure.notified() => return DebounceExit::Reconfigure,
                    () = state.wake.notified() => { state.health.beat(); continue; }
                    () = tokio::time::sleep(Duration::from_secs(1)) => { continue; }
                }
            }

            // Fire when the quiet window elapsed, but never later than the cap.
            let mut fire_at = match (operation_state, quiet_deadline, hard_deadline) {
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
            if let Some(retry_not_before) = state.retry_not_before() {
                fire_at = fire_at.max(retry_not_before);
            }
            if now >= fire_at {
                break;
            }
            let sleep_for = fire_at - now;
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return DebounceExit::Cancelled,
                () = state.reconfigure.notified() => return DebounceExit::Reconfigure,
                () = state.wake.notified() => { state.health.beat(); }
                () = tokio::time::sleep(sleep_for) => {}
            }
        }

        // Drain and execute exactly one coalesced sync pass.
        let (pending, affected_roots) = {
            let mut dirty = state.dirty.lock().await;
            let affected_roots = (!dirty.reconcile_metadata)
                .then(|| dirty.affected_roots.clone())
                .filter(|roots| !roots.is_empty());
            (dirty.take(), affected_roots)
        };
        if pending {
            #[cfg(test)]
            {
                state.drained_plans.fetch_add(1, Ordering::Relaxed);
                state.plan_drained.notify_one();
            }
            request_freshness_for_repository(inner, state, affected_roots).await;
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

enum OperationObservation {
    State(OperationState),
    Cancelled,
}

fn watch_observation_stopped(cancellation: &WatchCancellation, deadline: StdInstant) -> bool {
    cancellation.is_cancelled() || StdInstant::now() >= deadline
}

fn operation_state_blocking(
    state: &WatchState,
    max_worktrees: usize,
    cancellation: &WatchCancellation,
    deadline: StdInstant,
    affected_roots: Option<&BTreeSet<PathBuf>>,
) -> OperationObservation {
    const OPERATION_MARKERS: &[&str] = &[
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
    ];
    #[cfg(test)]
    state.operation_scan_probe.block_if_armed(cancellation);
    if cancellation.is_cancelled() {
        return OperationObservation::Cancelled;
    }
    let git_dirs: Vec<_> = state
        .worktrees()
        .into_iter()
        .filter(|(root, _)| affected_roots.is_none_or(|roots| roots.contains(root)))
        .map(|(_, git_dir)| git_dir)
        .collect();
    if git_dirs.len() > max_worktrees {
        return OperationObservation::State(OperationState::Incomplete);
    }
    for git_dir in git_dirs {
        for marker in OPERATION_MARKERS {
            if watch_observation_stopped(cancellation, deadline) {
                return if cancellation.is_cancelled() {
                    OperationObservation::Cancelled
                } else {
                    OperationObservation::State(OperationState::Incomplete)
                };
            }
            if git_dir.join(marker).exists() {
                return OperationObservation::State(OperationState::InFlight);
            }
        }
    }
    OperationObservation::State(OperationState::Idle)
}

#[cfg(test)]
fn operation_state(state: &WatchState, max_worktrees: usize) -> OperationState {
    let daemon_cancellation = crate::application::context::CancellationToken::new();
    let cancellation = state.cancellation(&daemon_cancellation);
    let Some(deadline) = StdInstant::now().checked_add(GIT_OBSERVATION_BUDGET) else {
        return OperationState::Incomplete;
    };
    match operation_state_blocking(state, max_worktrees, &cancellation, deadline, None) {
        OperationObservation::State(state) => state,
        OperationObservation::Cancelled => OperationState::Incomplete,
    }
}

async fn observe_operation_state(
    state: Arc<WatchState>,
    cancellation: WatchCancellation,
    affected_roots: Option<BTreeSet<PathBuf>>,
) -> OperationObservation {
    let Some(deadline) = StdInstant::now().checked_add(GIT_OBSERVATION_BUDGET) else {
        return OperationObservation::State(OperationState::Incomplete);
    };
    let worker_cancellation = cancellation.clone();
    let mut handle = tokio::task::spawn_blocking(move || {
        operation_state_blocking(
            &state,
            MAX_WORKTREES_PER_REPOSITORY,
            &worker_cancellation,
            deadline,
            affected_roots.as_ref(),
        )
    });
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            // Aborting a spawn_blocking task cannot stop a closure that is
            // already running. Join the deadline-bounded scan instead so no
            // watcher-owned blocking scan survives phase-one cancellation.
            let _ = (&mut handle).await;
            OperationObservation::Cancelled
        }
        result = tokio::time::timeout(GIT_OBSERVATION_BUDGET, &mut handle) => match result {
            Ok(Ok(observation)) => observation,
            Ok(Err(_)) if cancellation.is_cancelled() => OperationObservation::Cancelled,
            Ok(Err(_)) | Err(_) => OperationObservation::State(OperationState::Incomplete),
        }
    }
}

/// Routes a coalesced metadata cycle through the canonical scheduler.
///
/// Bounded structural discovery happens before scheduler publication. The
/// scheduler owns exact source revision resolution, gix status,
/// changed-candidate evidence, generation assembly, and its short CAS
/// publication; this watcher owns none of those authorities.
async fn request_freshness_for_repository(
    inner: &GitWatcherInner,
    state: &Arc<WatchState>,
    affected_roots: Option<BTreeSet<PathBuf>>,
) {
    use super::code_index_scheduler::GitStateChangeRequestV1;

    let Some(code_index_schedulers) = inner.code_index_schedulers.as_ref() else {
        return;
    };
    let Some(deadline) = StdInstant::now().checked_add(GIT_OBSERVATION_BUDGET) else {
        retain_freshness_retry(state, affected_roots);
        return;
    };
    let retry_roots = affected_roots.clone();
    let worker_state = Arc::clone(state);
    let cancellation = state.cancellation(&inner.cancellation);
    let worker_cancellation = cancellation.clone();
    let mut blocking = tokio::task::spawn_blocking(move || {
        if !worker_state
            .prune_missing_worktrees(|| watch_observation_stopped(&worker_cancellation, deadline))
        {
            return None;
        }
        Some(
            worker_state
                .worktree_roots()
                .into_iter()
                .filter(|root| {
                    affected_roots
                        .as_ref()
                        .is_none_or(|roots| roots.contains(root))
                })
                .collect::<Vec<_>>(),
        )
    });
    let roots = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            blocking.abort();
            return;
        }
        result = tokio::time::timeout(GIT_OBSERVATION_BUDGET, &mut blocking) => match result {
            Ok(Ok(Some(roots))) => roots,
            Ok(Ok(None) | Err(_)) | Err(_) => {
                retain_freshness_retry(state, retry_roots);
                return;
            }
        }
    };

    let mut accepted = false;
    let mut retry = BTreeSet::new();
    for project_root in roots {
        if cancellation.is_cancelled() {
            return;
        }
        let remaining = deadline.saturating_duration_since(StdInstant::now());
        if remaining.is_zero() {
            retry.insert(project_root);
            continue;
        }
        let discovery = discover_repository_identity(
            &project_root,
            MonotonicDeadline::at(deadline),
            &inner.cancellation,
        );
        let identity = tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            outcome = tokio::time::timeout(remaining, discovery) => match outcome {
                Ok(GitRepositoryIdentityOutcome::Resolved(identity)) => identity,
                Ok(GitRepositoryIdentityOutcome::NotRepository) => {
                    log_daemon_event(
                        "git_watch_freshness_rejected",
                        &[
                            ("project", project_root.display().to_string()),
                            ("reason", "not_repository".to_string()),
                        ],
                    );
                    continue;
                }
                Ok(GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled))
                    if cancellation.is_cancelled() =>
                {
                    return;
                }
                Ok(GitRepositoryIdentityOutcome::Unknown(reason)) => {
                    log_daemon_event(
                        "git_watch_freshness_rejected",
                        &[
                            ("project", project_root.display().to_string()),
                            ("reason", "identity_unknown".to_string()),
                            ("error", format!("{reason:?}")),
                        ],
                    );
                    retry.insert(project_root);
                    continue;
                }
                Err(_) => {
                    retry.insert(project_root);
                    continue;
                }
            }
        };
        match code_index_schedulers.request_for_root(&identity) {
            GitStateChangeRequestV1::Accepted => {
                accepted = true;
                log_daemon_event(
                    "git_watch_freshness_requested",
                    &[("project", project_root.display().to_string())],
                );
            }
            GitStateChangeRequestV1::Busy | GitStateChangeRequestV1::IdentityMismatch => {
                retry.insert(project_root);
            }
            GitStateChangeRequestV1::Unmounted => {
                log_daemon_event(
                    "git_watch_freshness_deferred",
                    &[
                        ("project", project_root.display().to_string()),
                        ("reason", "scheduler_unmounted_terminal".to_string()),
                    ],
                );
            }
        }
    }

    if accepted {
        #[cfg(test)]
        state.health.mark_requested();
    }
    if !retry.is_empty() {
        retain_freshness_retry(state, Some(retry));
    } else {
        state.clear_retry();
    }
}

fn retain_freshness_retry(state: &WatchState, affected_roots: Option<BTreeSet<PathBuf>>) {
    // Preserve one bounded pending cycle. Notify stores at most one permit, so
    // repeated Busy/deadline outcomes cannot form an unbounded queue.
    if let Ok(mut dirty) = state.dirty.try_lock() {
        let now = Instant::now();
        dirty.dirty = true;
        match affected_roots {
            Some(roots) => dirty.affected_roots.extend(roots),
            None => dirty.reconcile_metadata = true,
        }
        dirty.first_event.get_or_insert(now);
        dirty.last_event = Some(now);
    } else {
        state.reconciliation_pending.store(true, Ordering::Release);
    }
    state.schedule_retry();
    state.wake.notify_one();
}

/// The degraded fallback: request one authoritative scheduler reconciliation
/// every 5 minutes. Used when the inotify watcher cannot be built or dies
/// (e.g. ENOSPC). A fixed cadence is deliberate: filesystem mtimes cannot
/// faithfully summarize loose-ref content changes, while the scheduler's gix
/// reconciliation can.
async fn degraded_poll_loop(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    cancellation: &WatchCancellation,
) {
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(DEGRADED_POLL_INTERVAL) => {}
        }
        state.health.beat();
        request_freshness_for_repository(inner, state, None).await;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
