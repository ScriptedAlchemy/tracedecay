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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::SyncConfig;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use super::{branch_admin::StoreAdministration, log_daemon_event};

mod store_maintenance;

/// Degraded watchers fall back to polling git metadata every 5 minutes.
const DEGRADED_POLL_INTERVAL: Duration = Duration::from_mins(5);
/// A heartbeat older than this is considered stale by the backstop/doctor.
/// Two debounce+max cycles of slack over the default so a healthy but busy
/// watcher is never treated as dead.
const HEARTBEAT_STALE_SECS: u64 = 120;
/// Cap on the supervised-restart backoff.
const RESTART_BACKOFF_MAX: Duration = Duration::from_mins(1);

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
    fn snapshot(&self) -> ProjectHealthSnapshot {
        ProjectHealthSnapshot {
            last_heartbeat: self.last_heartbeat.load(Ordering::Relaxed),
            last_sync: self.last_sync.load(Ordering::Relaxed),
            degraded: self.degraded.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of a project's watch health, for the doctor section.
// The doctor watcher-health section consumes this surface (follow-up wiring);
// fields are populated by the watch loop today so the snapshot is truthful
// the moment doctor reads it.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ProjectHealthSnapshot {
    pub last_heartbeat: u64,
    pub last_sync: u64,
    pub degraded: bool,
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
    /// Dirty flag + affected-branch set. Coalesces a 50-commit rebase into a
    /// single sync — an unbounded queue would fire 50 times.
    dirty: Mutex<DirtySet>,
    /// Raised by the notify callback (or degraded poller) on every metadata
    /// event; the debounce task waits on it instead of polling.
    wake: Notify,
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
    }
    fn take(&mut self) -> DirtyPlan {
        let plan = DirtyPlan {
            dirty: self.dirty,
            branches: std::mem::take(&mut self.branches),
            new_worktrees: std::mem::take(&mut self.new_worktrees),
            gc_eligible: self.gc_eligible,
        };
        self.dirty = false;
        self.gc_eligible = false;
        self.first_event = None;
        self.last_event = None;
        plan
    }
}

/// The drained work for one debounce cycle.
struct DirtyPlan {
    dirty: bool,
    branches: HashSet<String>,
    new_worktrees: HashSet<String>,
    gc_eligible: bool,
}

impl DirtyPlan {
    fn is_empty(&self) -> bool {
        !self.dirty
            && self.branches.is_empty()
            && self.new_worktrees.is_empty()
            && !self.gc_eligible
    }
}

/// The daemon-held git-metadata watcher. Cheap to clone (all `Arc` inside), and
/// [`Default`] so `DaemonEngine` can derive `Default`.
#[derive(Clone)]
pub struct GitWatcher {
    inner: Arc<GitWatcherInner>,
}

struct GitWatcherInner {
    config: SyncConfig,
    /// Profile selected by the owning daemon. Every open and administration
    /// action must use this identity rather than whichever profile a watcher
    /// task happens to resolve from its environment.
    profile_root: PathBuf,
    /// Serializes every store-writing lifetime with daemon branch administration.
    administration: StoreAdministration,
    /// Whether watching is enabled at all (`auto_watch`). When false every
    /// method is a no-op so the daemon runs exactly as before this feature.
    enabled: bool,
    /// Daemon-wide sync concurrency governor.
    sync_semaphore: Arc<Semaphore>,
    /// Canonical project root → watch state. Also the backstop's project set.
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
            StoreAdministration::default(),
            current_profile_root(),
            false,
        )
    }

    fn from_parts(
        config: SyncConfig,
        administration: StoreAdministration,
        profile_root: PathBuf,
        enabled: bool,
    ) -> Self {
        let permits = config.max_concurrent_syncs.max(1);
        Self {
            inner: Arc::new(GitWatcherInner {
                config,
                profile_root,
                administration,
                enabled,
                sync_semaphore: Arc::new(Semaphore::new(permits)),
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
        Self::new_with_administration(
            config,
            StoreAdministration::default(),
            current_profile_root(),
        )
    }

    /// Builds a watcher bound to the daemon's profile and administration
    /// coordinator. The daemon uses this constructor so watcher syncs and
    /// destructive branch administration share one writer gate.
    pub(super) fn new_with_administration(
        config: SyncConfig,
        administration: StoreAdministration,
        profile_root: PathBuf,
    ) -> Self {
        let enabled = config.auto_watch;
        Self::from_parts(config, administration, profile_root, enabled)
    }

    // Doctor watcher-health surface (follow-up wiring).
    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.inner.enabled
    }

    /// Registers the recently-seen projects and starts the backstop timer.
    ///
    /// Called once from `run_foreground_unix` after the engine is built. Safe to
    /// call on a disabled watcher (no-op).
    pub async fn spawn(&self, global_db_path: Option<PathBuf>) {
        if !self.inner.enabled || self.inner.shutting_down.load(Ordering::Acquire) {
            return;
        }
        // Enumerate recently-seen code projects (most-recent first, capped).
        let window = 14 * 86_400;
        let cap = self.inner.config.watch_max_projects;
        let db = match global_db_path.as_deref() {
            Some(path) => crate::global_db::GlobalDb::open_at(path).await,
            None => crate::global_db::GlobalDb::open().await,
        };
        if let Some(db) = db {
            let open_options = daemon_open_options(&self.inner);
            let projects = db.code_projects_seen_within(window, cap).await;
            for record in projects {
                let root = PathBuf::from(&record.canonical_root);
                if root.is_dir()
                    && root.join(".git").exists()
                    && TraceDecay::has_initialized_store_with_options(&root, &open_options).await
                {
                    self.ensure_watching(&root).await;
                }
            }
        }

        // Start the single backstop timer.
        let watcher = self.clone();
        let db_path = global_db_path.clone();
        let handle = tokio::spawn(async move {
            backstop::run(watcher, db_path).await;
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

        let mut projects = self.inner.projects.lock().await;
        if projects.contains_key(&canonical) {
            return;
        }
        if projects.len() >= self.inner.config.watch_max_projects {
            // At capacity — the backstop still covers this project on its timer.
            return;
        }

        let state = Arc::new(WatchState {
            project_root: canonical.clone(),
            dirty: Mutex::new(DirtySet::default()),
            wake: Notify::new(),
            health: ProjectHealth::default(),
            task: Mutex::new(None),
            #[cfg(test)]
            entered_debounce: Notify::new(),
            #[cfg(test)]
            drained_plans: AtomicU64::new(0),
            #[cfg(test)]
            plan_drained: Notify::new(),
        });
        projects.insert(canonical.clone(), Arc::clone(&state));
        drop(projects);

        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(supervise_project(inner, Arc::clone(&state)));
        *state.task.lock().await = Some(handle);

        log_daemon_event(
            "git_watch_started",
            &[("project", canonical.display().to_string())],
        );
    }

    /// Stops every watcher-owned task and joins it before database shutdown.
    pub async fn shutdown(&self) {
        self.shutdown_with_deadline(super::DAEMON_TASK_ABORT_DEADLINE)
            .await;
    }

    async fn shutdown_with_deadline(&self, deadline: Duration) {
        if !self.inner.enabled || self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }

        let mut handles = Vec::new();
        if let Some(handle) = self.inner.backstop_task.lock().await.take() {
            handles.push(handle);
        }

        let states: Vec<Arc<WatchState>> = {
            let mut projects = self.inner.projects.lock().await;
            projects.drain().map(|(_, state)| state).collect()
        };
        for state in states {
            if let Some(handle) = state.task.lock().await.take() {
                handles.push(handle);
            }
        }
        for handle in &handles {
            handle.abort();
        }
        let _ = tokio::time::timeout(deadline, async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;
    }

    /// A doctor-facing snapshot of every registered project's watch health.
    #[cfg(test)]
    pub async fn health_report(&self) -> Vec<(PathBuf, ProjectHealthSnapshot)> {
        let projects = self.inner.projects.lock().await;
        let mut out: Vec<_> = projects
            .iter()
            .map(|(root, state)| (root.clone(), state.health.snapshot()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// Falls back to an empty path only when the process cannot resolve a current
/// profile. Daemon construction passes its canonical profile explicitly; the
/// fallback exists solely for standalone/test construction.
fn current_profile_root() -> PathBuf {
    crate::storage::default_profile_root().unwrap_or_default()
}

/// Builds explicit open options for the daemon-owned profile. The global
/// registry path follows that same profile rather than the ambient process
/// environment used by ordinary CLI clients.
fn daemon_open_options(inner: &GitWatcherInner) -> TraceDecayOpenOptions {
    if inner.profile_root.as_os_str().is_empty() {
        // Keep standalone construction's former behavior when no current
        // profile can be resolved: the normal open path will report failure
        // rather than treating an empty path as a writable profile directory.
        return TraceDecayOpenOptions::default();
    }
    let profile_root = inner.profile_root.clone();
    TraceDecayOpenOptions {
        global_db_path: Some(profile_root.join("global.db")),
        profile_root: Some(profile_root),
    }
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
enum IdentityDiscoveryDisposition {
    Watch(crate::worktree::GitRepoIdentity),
    Degraded,
    Retry,
}

fn identity_discovery_disposition(
    outcome: crate::worktree::GitRepoIdentityOutcome,
) -> IdentityDiscoveryDisposition {
    match outcome {
        crate::worktree::GitRepoIdentityOutcome::Resolved(identity) => {
            IdentityDiscoveryDisposition::Watch(identity)
        }
        crate::worktree::GitRepoIdentityOutcome::NotFound => IdentityDiscoveryDisposition::Degraded,
        crate::worktree::GitRepoIdentityOutcome::Unknown => IdentityDiscoveryDisposition::Retry,
    }
}

async fn project_task(inner: Arc<GitWatcherInner>, state: Arc<WatchState>) {
    let mut discovery_backoff = Duration::from_millis(500);
    let identity = loop {
        match identity_discovery_disposition(crate::worktree::git_repo_identity_outcome(
            &state.project_root,
        )) {
            IdentityDiscoveryDisposition::Watch(identity) => break identity,
            IdentityDiscoveryDisposition::Degraded => {
                // Definitively not a git repo (yet). Degrade to polling so a
                // later `git init` / clone is still eventually covered.
                state.health.set_degraded(true);
                degraded_poll_loop(&inner, &state, None).await;
                return;
            }
            IdentityDiscoveryDisposition::Retry => {
                state.health.set_degraded(true);
                state.health.beat();
                log_daemon_event(
                    "git_watch_discovery_retry",
                    &[
                        ("project", state.project_root.display().to_string()),
                        ("backoff_ms", discovery_backoff.as_millis().to_string()),
                    ],
                );
                tokio::time::sleep(discovery_backoff).await;
                discovery_backoff = (discovery_backoff * 2).min(RESTART_BACKOFF_MAX);
            }
        }
    };
    let common_dir = identity.common_dir;

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
            degraded_poll_loop(&inner, &state, Some(&common_dir)).await;
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
        degraded_poll_loop(&inner, &state, Some(&common_dir)).await;
        return;
    }

    state.health.set_degraded(false);
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
                    }
                }
            }
        }
    }
    state.wake.notify_one();
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
        // Sleep until the first event arrives.
        state.wake.notified().await;
        state.health.beat();

        // Coalesce: keep extending the quiet window until it settles or we hit
        // the hard cap. If a rebase/merge is mid-flight, HOLD (keep waiting)
        // until the markers disappear so we sync exactly once, after.
        loop {
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

/// True while a rebase/merge is mid-flight; the watcher holds during these and
/// fires exactly once after they clear.
fn operation_in_flight(common: &Path) -> bool {
    common.join("rebase-merge").exists()
        || common.join("rebase-apply").exists()
        || common.join("MERGE_HEAD").exists()
}

/// Executes one coalesced debounce cycle: proactively track new worktrees, sync
/// affected branches / the current branch, and mark GC eligibility.
async fn execute_plan(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    common: &Path,
    plan: DirtyPlan,
) {
    let root = &state.project_root;
    let opts = daemon_open_options(inner);

    // 1. Proactively track newly-created linked worktrees.
    for name in &plan.new_worktrees {
        if let Some((wt_root, branch)) = store_maintenance::resolve_worktree(common, name) {
            let _permit = inner.sync_semaphore.acquire().await;
            match store_maintenance::track_worktree_branch(
                &inner.administration,
                wt_root.clone(),
                branch.clone(),
                opts.clone(),
            )
            .await
            {
                Some(outcome) => {
                    log_daemon_event(
                        "git_watch_synced",
                        &[
                            ("project", root.display().to_string()),
                            ("action", "worktree_tracked".to_string()),
                            ("worktree", wt_root.display().to_string()),
                            ("branch", branch),
                            ("outcome", outcome),
                        ],
                    );
                }
                None => log_daemon_event(
                    "git_watch_degraded",
                    &[
                        ("project", root.display().to_string()),
                        ("reason", "worktree_track_failed".to_string()),
                    ],
                ),
            }
        }
    }

    // 2. Sync the CURRENT branch's store (HEAD move / general dirtiness).
    //
    //    `sync_project` opens the project root, which resolves to whichever
    //    branch HEAD currently points at, and diffs against the working tree.
    //    It therefore can only refresh the checked-out branch's store — a
    //    ref update to a tracked-but-not-checked-out branch (e.g. `git fetch`
    //    advancing `refs/heads/feature` while on `main`, or a branch moved in
    //    another worktree) has no working tree here to diff against, so its own
    //    store cannot be incrementally synced from this checkout. Those stores
    //    recover on the next on-read sync (when that branch is checked out /
    //    opened) or via the backstop. We only report the branch we actually
    //    refreshed, so the log never claims a sync that did not happen.
    let current_branch = if plan.dirty {
        crate::branch::current_branch(root)
    } else {
        None
    };
    // Non-current tracked branches whose refs changed: recorded but NOT synced
    // here (see above). Kept distinct from `current_branch` for honest logging.
    let other_changed_branches: Vec<String> = plan
        .branches
        .iter()
        .filter(|b| current_branch.as_deref() != Some(b.as_str()))
        .cloned()
        .collect();
    // Sync when HEAD moved (current branch) OR any ref changed at all — the
    // latter still warrants a current-branch freshness pass in case HEAD itself
    // advanced without `plan.dirty` capturing the branch name.
    if current_branch.is_some() || !plan.branches.is_empty() {
        let _permit = inner.sync_semaphore.acquire().await;
        if store_maintenance::sync_project(
            root,
            &opts,
            inner.config.full_sync_escalation_files,
            &inner.administration,
        )
        .await
        {
            state.health.mark_synced();
            let mut fields = vec![
                ("project", root.display().to_string()),
                ("action", "incremental".to_string()),
                (
                    "synced_branch",
                    current_branch
                        .clone()
                        .unwrap_or_else(|| "current".to_string()),
                ),
            ];
            // Surface the branches we saw change but could NOT sync from this
            // checkout, so the log is not misleading about coverage.
            if !other_changed_branches.is_empty() {
                fields.push(("deferred_branches", other_changed_branches.join(",")));
            }
            log_daemon_event("git_watch_synced", &fields);
        }
    }

    // 3. GC eligibility on ref/worktree deletion.
    if plan.gc_eligible {
        store_maintenance::run_gc(inner, root, &opts).await;
    }
}

/// The degraded fallback: mtime-poll HEAD + packed-refs every 5 minutes and
/// sync when they advance. Used when the inotify watcher cannot be built or
/// dies (e.g. ENOSPC). Covers ONE project — never a global failure.
async fn degraded_poll_loop(
    inner: &Arc<GitWatcherInner>,
    state: &Arc<WatchState>,
    common: Option<&Path>,
) {
    let opts = daemon_open_options(inner);
    let mut last_sig: Option<(SystemTime, SystemTime)> = None;
    loop {
        state.health.beat();
        if let Some(common) = common {
            let sig = metadata_signature(common);
            if sig != last_sig && last_sig.is_some() {
                let _permit = inner.sync_semaphore.acquire().await;
                if store_maintenance::sync_project(
                    &state.project_root,
                    &opts,
                    inner.config.full_sync_escalation_files,
                    &inner.administration,
                )
                .await
                {
                    state.health.mark_synced();
                }
            }
            last_sig = sig;
        }
        tokio::time::sleep(DEGRADED_POLL_INTERVAL).await;
    }
}

/// mtime signature of HEAD + packed-refs for the degraded poller.
fn metadata_signature(common: &Path) -> Option<(SystemTime, SystemTime)> {
    let head = std::fs::metadata(common.join("HEAD"))
        .and_then(|m| m.modified())
        .ok()?;
    let packed = std::fs::metadata(common.join("packed-refs"))
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH);
    Some((head, packed))
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

    pub(super) async fn run(watcher: GitWatcher, global_db_path: Option<PathBuf>) {
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
            tick(&watcher, global_db_path.as_deref(), &mut last_gc, gc_period).await;
        }
    }

    async fn tick(
        watcher: &GitWatcher,
        _global_db_path: Option<&Path>,
        last_gc: &mut Option<Instant>,
        gc_period: Duration,
    ) {
        let interval_secs = watcher
            .inner
            .config
            .backstop_interval_mins
            .saturating_mul(60);
        let opts = daemon_open_options(&watcher.inner);

        // Snapshot registered projects; cover those the watcher isn't keeping
        // fresh (stale/absent heartbeat) AND whose store is older than one
        // interval.
        let entries: Vec<(PathBuf, Arc<WatchState>)> = {
            let projects = watcher.inner.projects.lock().await;
            projects
                .iter()
                .map(|(root, state)| (root.clone(), Arc::clone(state)))
                .collect()
        };

        let run_gc_now = last_gc.is_none_or(|t| t.elapsed() >= gc_period);
        let mut gc_retry_needed = false;

        for (root, state) in entries {
            let snap = state.health.snapshot();
            if snap.heartbeat_stale() && store_is_stale(&root, &opts, interval_secs).await {
                let _permit = watcher.inner.sync_semaphore.acquire().await;
                if super::store_maintenance::sync_project(
                    &root,
                    &opts,
                    watcher.inner.config.full_sync_escalation_files,
                    &watcher.inner.administration,
                )
                .await
                {
                    state.health.mark_synced();
                    log_daemon_event(
                        "git_watch_synced",
                        &[
                            ("project", root.display().to_string()),
                            ("action", "backstop".to_string()),
                        ],
                    );
                }
            }

            if run_gc_now && !super::store_maintenance::run_gc(&watcher.inner, &root, &opts).await {
                gc_retry_needed = true;
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
    async fn store_is_stale(root: &Path, opts: &TraceDecayOpenOptions, interval_secs: u64) -> bool {
        let Ok(cg) = TraceDecay::open_read_only_with_options(root, opts.clone()).await else {
            return false;
        };
        let last = cg.last_sync_timestamp().await;
        let age = super::now_secs() as i64 - last;
        age > interval_secs as i64
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
