use super::super::maintenance::retention_window_secs;
use super::*;

use notify::event::EventAttributes;
use std::process::Command;
use tokio::sync::Notify;

mod lifecycle;

fn worktree_git_dir(project_root: &Path) -> Option<PathBuf> {
    match tracedecay_runtime_core::git_discovery::discover_repository_identity_bounded(project_root)
    {
        GitRepositoryIdentityOutcome::Resolved(identity) => Some(identity.git_dir),
        GitRepositoryIdentityOutcome::NotRepository | GitRepositoryIdentityOutcome::Unknown(_) => {
            None
        }
    }
}

#[test]
fn debris_retention_enables_maintenance_without_orphan_gc() {
    let mut retention = crate::config::RetentionConfig::default();
    retention.session_lcm.enabled = false;
    retention.observation.enabled = false;
    retention.orphan_store_gc_days = None;
    retention.incident_debris_retention_days = Some(30);
    retention.compaction = None;

    assert!(retention_maintenance_enabled(&retention));
}

#[test]
fn soft_budget_alone_never_enables_destructive_maintenance() {
    let mut retention = crate::config::RetentionConfig::default();
    retention.session_lcm.enabled = false;
    retention.observation.enabled = false;
    retention.orphan_store_gc_days = None;
    retention.incident_debris_retention_days = None;
    retention.compaction = None;
    retention
        .store_soft_budgets_bytes
        .insert("sessions.db".to_string(), 1);

    assert!(
        !retention_maintenance_enabled(&retention),
        "soft budgets are Doctor findings, never a retention trigger"
    );
}

#[test]
fn retention_window_conversion_never_wraps_negative() {
    assert_eq!(retention_window_secs(u64::MAX), i64::MAX);
}

/// The ordinary retention cadence must sweep the same scoped code-index root the
/// scheduler publishes into. A cadence aimed anywhere else would find no sealed
/// generations and silently reclaim nothing.
#[test]
fn code_generation_retention_sweeps_the_scheduler_store_root() {
    let data_root = std::path::PathBuf::from("/profile/projects/alpha");
    let project_root = std::path::PathBuf::from("/work/alpha");

    let swept = store_maintenance::code_index_store_root(&data_root, &project_root);
    let published = super::super::code_index_scheduler::scoped_code_index_store_root(
        &data_root.join("code-index-v1"),
        &project_root,
    );

    assert_eq!(
        swept, published,
        "retention cadence must sweep the scheduler's scoped generation root"
    );
    assert!(
        swept.starts_with(data_root.join("code-index-v1")),
        "generation sweep must stay inside the project's code-index store"
    );
    assert_ne!(
        swept,
        data_root.join("code-index-v1"),
        "sweep root must be the per-project scoped subdirectory, not the shared parent"
    );
}

/// Scope reconciliation reaches the *siblings* generation retention cannot see,
/// so it must operate exactly one directory above the sweep root.
#[test]
fn scope_reconciliation_operates_on_the_shared_code_index_parent() {
    let data_root = PathBuf::from("/profile/projects/alpha");
    let project_root = PathBuf::from("/work/alpha");

    let parent = store_maintenance::code_index_scope_store_root(&data_root);
    let scoped = store_maintenance::code_index_store_root(&data_root, &project_root);

    assert_eq!(parent, data_root.join("code-index-v1"));
    assert_eq!(
        scoped.parent(),
        Some(parent.as_path()),
        "the scoped sweep root must be a direct child of the reconciled parent"
    );
}

fn scope_fixture_git(root: &Path, args: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

/// A linked worktree is a live canonical root with its own code-index scope.
/// Missing one would classify a scope in daily use as stranded.
#[test]
fn live_code_index_roots_cover_every_linked_worktree() {
    use crate::retention::code_index_generations::code_index_scope_hash;

    let tmp = tempfile::TempDir::new().expect("repository root");
    let primary = tmp.path().join("primary");
    let linked = tmp.path().join("linked");
    std::fs::create_dir_all(&primary).expect("create primary checkout");
    scope_fixture_git(&primary, &["init", "-q", "-b", "main"]);
    scope_fixture_git(&primary, &["config", "user.name", "TraceDecay Test"]);
    scope_fixture_git(
        &primary,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(primary.join("README.md"), b"fixture").expect("seed repository file");
    scope_fixture_git(&primary, &["add", "."]);
    scope_fixture_git(&primary, &["commit", "-qm", "fixture"]);
    scope_fixture_git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().expect("worktree path"),
        ],
    );

    let roots = store_maintenance::resolve_live_code_index_roots(&primary)
        .expect("git's own worktree registry is readable");

    let hashes = roots
        .iter()
        .map(|root| code_index_scope_hash(root))
        .collect::<std::collections::BTreeSet<_>>();
    for root in [&primary, &linked] {
        let canonical = std::fs::canonicalize(root).expect("canonical worktree root");
        assert!(
            hashes.contains(&code_index_scope_hash(&canonical)),
            "every live worktree root must be represented in the live scope set: {}",
            canonical.display()
        );
    }
}

/// Fail closed: a repository whose worktree registry cannot be resolved yields
/// no live set at all, so the caller collects nothing instead of treating an
/// empty set as "everything is stranded".
#[test]
fn live_code_index_roots_fail_closed_outside_a_repository() {
    let tmp = tempfile::TempDir::new().expect("non-repository root");

    assert!(
        store_maintenance::resolve_live_code_index_roots(tmp.path()).is_err(),
        "an unresolvable repository must never produce a smaller live set"
    );
}

#[test]
fn dirty_set_coalesces_and_takes_once() {
    let mut set = DirtySet::default();
    assert!(set.is_clean());
    set.dirty = true;
    assert!(!set.is_clean());

    assert!(set.take());
    assert!(set.is_clean());
    assert!(!set.take());
}

#[test]
fn shared_ref_event_marks_repository_reconciliation() {
    let state = Arc::new(WatchState::new(
        PathBuf::from("/repo/.git"),
        PathBuf::from("/repo"),
        PathBuf::from("/repo/.git"),
        MaintenanceCoordinator::default(),
    ));
    let create = notify::Event {
        kind: EventKind::Create(notify::event::CreateKind::File),
        paths: vec![PathBuf::from("/repo/.git/refs/heads/feat/x")],
        attrs: EventAttributes::default(),
    };
    classify_and_mark(&state, &create);

    let dirty = state.dirty.blocking_lock();
    assert!(dirty.dirty);
    assert!(
        dirty.reconcile_metadata,
        "a shared ref can affect every mounted worktree"
    );
    assert!(!state.reconciliation_pending.load(Ordering::Acquire));
}

#[tokio::test]
async fn contended_event_requests_a_bounded_reconciliation() {
    let state = Arc::new(WatchState::new(
        PathBuf::from("/repo/.git"),
        PathBuf::from("/repo"),
        PathBuf::from("/repo/.git"),
        MaintenanceCoordinator::default(),
    ));
    let event = notify::Event {
        kind: EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
        paths: vec![PathBuf::from("/repo/.git/HEAD")],
        attrs: EventAttributes::new(),
    };

    let dirty = state.dirty.lock().await;
    classify_and_mark(&state, &event);
    assert!(
        state.reconciliation_pending.load(Ordering::Acquire),
        "lock contention must preserve a reconciliation request"
    );
    drop(dirty);

    materialize_pending_reconciliation(&state).await;

    let dirty = state.dirty.lock().await;
    assert!(dirty.dirty);
    assert!(dirty.reconcile_metadata);
    assert!(dirty.first_event.is_some());
    assert!(dirty.last_event.is_some());
    assert!(!state.reconciliation_pending.load(Ordering::Acquire));
}

#[test]
fn heartbeat_staleness() {
    let now = HEARTBEAT_STALE_MILLIS + 20_000;
    let fresh = ProjectHealthSnapshot {
        last_heartbeat: now,
        status: ProjectWatchStatus::Active,
        last_freshness_request: 0,
        degraded: false,
    };
    assert!(!fresh.heartbeat_stale_at(now));
    let never = ProjectHealthSnapshot {
        last_heartbeat: 0,
        status: ProjectWatchStatus::Initializing,
        last_freshness_request: 0,
        degraded: false,
    };
    assert!(never.heartbeat_stale_at(now));
    let old = ProjectHealthSnapshot {
        last_heartbeat: now.saturating_sub(HEARTBEAT_STALE_MILLIS + 10_000),
        status: ProjectWatchStatus::Active,
        last_freshness_request: 0,
        degraded: false,
    };
    assert!(old.heartbeat_stale_at(now));
}

// ---- Real `GitWatcher` tests (drive the public API + the real debounce
// path, not a reimplemented helper). The integration suite cannot reach
// these crate-private internals because `git_watch` is not re-exported. ----

/// A test config with a tiny debounce so the real debounce path settles fast.
///
/// Production defaults (`SyncConfig::default()` in `src/config.rs`) are
/// `watch_debounce_ms = 2_000` and `watch_max_delay_ms = 30_000` — a healthy
/// production watcher may legitimately hold a sync for up to 30s to coalesce
/// a busy rebase. Tests have no reason to wait out that budget: they only
/// need the debounce/max-delay *shape* (a short quiet period bounded by a
/// hard cap), so this constructor is the injection point (shape: `#[cfg(test)]`
/// config values passed into the existing `GitWatcher::new` constructor)
/// that swaps the production windows for millisecond-scale ones while never
/// touching the production defaults themselves.
fn fast_watch_config() -> SyncConfig {
    let mut config = SyncConfig {
        auto_watch: true,
        ..SyncConfig::default()
    };
    config.watch_debounce_ms = 25;
    config.watch_max_delay_ms = 200;
    config.watch_max_projects = 32;
    config.backstop_interval_mins = 0; // no backstop noise in these tests
    config
}

/// Ceiling for [`ensure_watching_or_skip`]'s readiness race and for
/// [`debounce_loop_coalesces_and_drains_events`]'s drain wait.
///
/// This has no production counterpart — `GitWatcher` never itself waits on
/// "has a task become ready"; it is purely a test diagnostic bound: how long
/// we are willing to wait for the real watch task to signal `entered_debounce`
/// or flip `degraded` before concluding the watch task is genuinely hung (a
/// regression) rather than merely slow to schedule (real inotify install +
/// a couple of tokio task hops, normally low milliseconds). It used to be a
/// flat `Duration::from_secs(30)` inlined at each call site, which is why a
/// scheduler-starved run could burn the full 30s on every one of these tests
/// before either resolving or panicking. Kept short-but-generous rather than
/// matching the 100-500ms debounce-scale windows above: unlike the debounce
/// windows, this ceiling absorbs *real* OS/scheduler contention (sibling
/// tests racing for the same inotify watch slots), not a modeled production
/// duration, so it needs real wall-clock slack.
const TEST_READY_TIMEOUT: Duration = Duration::from_secs(8);

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new(crate::git::git_program())
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A bare temp git repo with one commit. Not indexed by tracedecay — these
/// tests exercise the watcher's registration/debounce plumbing, which runs
/// regardless of whether a store exists (a sync on a non-indexed project is
/// a cheap no-op).
fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    seed_repo(dir.path());
    dir
}

fn seed_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("create repository root");
    git(root, &["init", "-b", "main"]);
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"]);
}

fn add_linked_worktree(primary: &Path, linked: &Path) {
    git(
        primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().expect("linked worktree path is UTF-8"),
        ],
    );
}

fn linked_worktree_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let container = tempfile::tempdir().expect("linked-worktree fixture");
    let primary = container.path().join("primary");
    let linked = container.path().join("linked");
    seed_repo(&primary);
    add_linked_worktree(&primary, &linked);
    (container, primary, linked)
}

async fn ready_registered_state(watcher: &GitWatcher, repo: &Path) -> Arc<WatchState> {
    let common = crate::worktree::git_common_dir(repo).expect("repository common directory");
    let projects = watcher.inner.projects.lock().await;
    Arc::clone(projects.get(&common).expect("repository registered"))
}

/// True for the specific `notify` error that means "the OS/sandbox is out of
/// inotify watch slots" (`fs.inotify.max_user_watches` exhausted), as opposed
/// to any other watch-install failure. Kept as a plain, synchronous predicate
/// over a directly-constructed `notify::Error` so it can be unit-tested
/// without touching the filesystem or Tokio — see
/// `max_files_watch_is_recognized_as_the_watch_limit` below.
fn is_watch_limit_error(err: &notify::Error) -> bool {
    matches!(err.kind, notify::ErrorKind::MaxFilesWatch)
}

#[test]
fn max_files_watch_is_recognized_as_the_watch_limit() {
    assert!(is_watch_limit_error(&notify::Error::new(
        notify::ErrorKind::MaxFilesWatch
    )));
    assert!(!is_watch_limit_error(&notify::Error::new(
        notify::ErrorKind::PathNotFound
    )));
    assert!(!is_watch_limit_error(&notify::Error::io(
        std::io::Error::other("boom")
    )));
}

/// True right now if installing the crate's own metadata watch set on
/// `repo` would hit the OS inotify watch limit. Used only to CONFIRM (after
/// the real watch task has already failed to become ready) that the OS is
/// presently out of watches, by making the exact same `install_watches` call
/// the production task makes.
async fn currently_watch_limited(repo: &Path) -> bool {
    let Some(common) = crate::worktree::git_common_dir(repo) else {
        return false;
    };
    let Some(git_dir) = worktree_git_dir(repo) else {
        return false;
    };
    let state = Arc::new(WatchState::new(
        common,
        repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf()),
        git_dir,
        MaintenanceCoordinator::default(),
    ));
    let Ok(mut probe) = notify::recommended_watcher(|_res: notify::Result<notify::Event>| {})
    else {
        return false;
    };
    let daemon_cancellation = crate::application::context::CancellationToken::new();
    let cancellation = state.cancellation(&daemon_cancellation);
    matches!(
        install_watches(&mut probe, state, cancellation).await,
        Err(WatchInstallFailure::Notify(error)) if is_watch_limit_error(&error)
    )
}

/// Registers `repo` with `watcher` and waits for its watch task to reach
/// `debounce_loop`, unless the OS/sandbox is out of inotify watches — in
/// which case this skips (loudly, on stderr) instead of failing. Full
/// assertion strength is unchanged whenever a watch can be installed: the
/// happy path is the exact same wait-then-return the tests used directly
/// before this helper existed.
///
/// Every `GitWatcher`-driven test below spawns a *real* inotify watcher via
/// the crate's own `install_watches`. On a host at or over
/// `fs.inotify.max_user_watches` that call fails with
/// `notify::ErrorKind::MaxFilesWatch` (logged by the production task as
/// `watch_install_failed error="OS file watch limit reached"`), which is an
/// environment fact rather than a regression in this module — the watch task
/// falls back to `degraded_poll_loop` and never reaches `debounce_loop`.
///
/// We deliberately do NOT pre-probe before registering: this crate's own
/// watch limit is a shared, global, momentarily-contended resource (sibling
/// tests in this same suite register watches concurrently), so a probe taken
/// before the real registration can pass while the real install — racing
/// against those siblings a moment later — still fails. Instead, we let the
/// real watch task run and race its readiness signal against a fast poll of
/// `degraded`: as soon as EITHER fires we react, rather than always waiting
/// out the full [`TEST_READY_TIMEOUT`] budget first. That matters here
/// specifically because the contention is often a brief spike — confirming
/// "is the OS out of watches" only *after* burning the whole timeout would
/// check long after sibling tests released theirs, wrongly concluding the
/// install was healthy. Once `degraded` flips we confirm the cause
/// immediately (within one ~20ms poll tick of the real failure) with a fresh
/// `install_watches` attempt on the same repo: `MaxFilesWatch` there means
/// the OS is still (or again) out of watches, so we skip. Any other outcome —
/// the [`TEST_READY_TIMEOUT`] budget elapsing with neither signal, or
/// degraded for an unconfirmed reason — is treated as a real regression and
/// panics, exactly as the unconditional wait did before.
async fn ensure_watching_or_skip(watcher: &GitWatcher, repo: &Path) -> Option<Arc<WatchState>> {
    enum Ready {
        Debounce,
        Degraded,
    }
    assert_eq!(
        watcher.ensure_watching(repo).await,
        GitWatcherAdmission::Ready,
        "a valid repository must be admitted before watcher readiness"
    );
    let state = ready_registered_state(watcher, repo).await;

    let outcome = tokio::time::timeout(TEST_READY_TIMEOUT, async {
        tokio::select! {
            () = state.entered_debounce.notified() => Ready::Debounce,
            () = poll_until_degraded(&state) => Ready::Degraded,
        }
    })
    .await;

    match outcome {
        Ok(Ready::Debounce) => Some(state),
        Ok(Ready::Degraded) if currently_watch_limited(repo).await => {
            eprintln!(
                "SKIP: OS inotify watch limit reached (fs.inotify.max_user_watches \
                 exhausted); raise it to exercise the real git_watch debounce path"
            );
            None
        }
        Ok(Ready::Degraded) | Err(_) => panic!("watch task must reach debounce_loop"),
    }
}

/// Resolves as soon as `state` is marked degraded, polling frequently so a
/// real (but often brief) OS watch-limit failure is caught close to the
/// moment it happens, rather than after some longer fixed wait has let
/// sibling tests' transient contention clear.
async fn poll_until_degraded(state: &Arc<WatchState>) {
    loop {
        if state.health.snapshot().degraded {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn ensure_watching_registers_dedups_and_caps() {
    let repo_a = temp_repo();
    let repo_b = temp_repo();
    let repo_c = temp_repo();

    let mut config = fast_watch_config();
    config.watch_max_projects = 2;
    let watcher = GitWatcher::new(config);
    assert_eq!(
        watcher.ensure_watching(repo_a.path()).await,
        GitWatcherAdmission::Ready
    );
    assert_eq!(watcher.health_report().await.len(), 1);

    assert_eq!(
        watcher.ensure_watching(repo_a.path()).await,
        GitWatcherAdmission::Ready
    );
    assert_eq!(watcher.health_report().await.len(), 1);

    assert_eq!(
        watcher.ensure_watching(repo_b.path()).await,
        GitWatcherAdmission::Ready
    );
    assert_eq!(watcher.health_report().await.len(), 2);

    assert_eq!(
        watcher.ensure_watching(repo_c.path()).await,
        GitWatcherAdmission::Capacity
    );
    assert_eq!(watcher.health_report().await.len(), 2);
}

#[tokio::test]
async fn linked_worktrees_share_one_repository_watcher() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let unrelated = temp_repo();

    let mut config = fast_watch_config();
    config.watch_max_projects = 1;
    let watcher = GitWatcher::new(config);
    assert_eq!(
        watcher.ensure_watching(&primary).await,
        GitWatcherAdmission::Ready
    );
    assert_eq!(
        watcher.ensure_watching(&linked).await,
        GitWatcherAdmission::Ready
    );
    assert_eq!(
        watcher.ensure_watching(unrelated.path()).await,
        GitWatcherAdmission::Capacity
    );

    let projects = watcher.inner.projects.lock().await;
    assert_eq!(
        projects.len(),
        1,
        "linked roots share one slot and an unrelated repository is capped"
    );
    let common = crate::worktree::git_common_dir(&primary).expect("common directory");
    let state = projects.get(&common).expect("repository watcher");
    assert!(state.contains_worktree(&primary.canonicalize().unwrap()));
    assert!(state.contains_worktree(&linked.canonicalize().unwrap()));
    drop(projects);
    watcher.shutdown().await;
}

#[test]
fn unmounted_linked_worktree_operation_does_not_block_mounted_sibling() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let common = crate::worktree::git_common_dir(&primary).expect("git common dir");
    let primary_git_dir = worktree_git_dir(&primary).expect("primary git dir");
    let linked_git_dir = worktree_git_dir(&linked).expect("linked git dir");
    let state = WatchState::new(
        common.clone(),
        primary.canonicalize().unwrap(),
        primary_git_dir,
        MaintenanceCoordinator::default(),
    );
    let marker = linked_git_dir.join("rebase-merge");
    std::fs::create_dir(&marker).expect("create linked-worktree operation marker");

    assert_eq!(
        operation_state(&state, 8),
        OperationState::Idle,
        "an unmounted linked-worktree marker cannot starve an active sibling"
    );

    std::fs::remove_dir(&marker).expect("remove linked-worktree operation marker");
    assert_eq!(operation_state(&state, 8), OperationState::Idle);
}

#[test]
fn registered_worktree_operation_scan_fails_closed_at_its_cap() {
    let tmp = tempfile::tempdir().expect("metadata root");
    let common = tmp.path().join("git");
    let registered = common.join("registered");
    std::fs::create_dir_all(common.join("worktrees/one")).expect("first linked git directory");
    std::fs::create_dir(common.join("worktrees/two")).expect("second linked git directory");
    std::fs::create_dir(&registered).expect("registered git directory");
    let state = WatchState::new(
        common.clone(),
        tmp.path().join("worktree"),
        registered,
        MaintenanceCoordinator::default(),
    );

    let second_root = tmp.path().join("worktree-two");
    let second_git_dir = common.join("registered-two");
    std::fs::create_dir_all(&second_root).expect("second worktree root");
    std::fs::create_dir(&second_git_dir).expect("second registered git directory");
    assert!(matches!(
        state.register_worktree(second_root, second_git_dir, 2),
        WorktreeRegistration::Ready
    ));
    assert!(
        operation_state(&state, 1) == OperationState::Incomplete,
        "incomplete operation evidence must not be mistaken for an idle repository"
    );
}

#[tokio::test]
async fn callback_failure_requests_conservative_reconciliation() {
    let repo = temp_repo();
    let common = crate::worktree::git_common_dir(repo.path()).expect("git common dir");
    let git_dir = worktree_git_dir(repo.path()).expect("git dir");
    let state = WatchState::new(
        common,
        repo.path().canonicalize().expect("canonical root"),
        git_dir,
        MaintenanceCoordinator::default(),
    );

    mark_reconciliation_pending(&state);
    materialize_pending_reconciliation(&state).await;

    assert!(
        !state.dirty.lock().await.is_clean(),
        "notify backend errors must retain one bounded conservative reconcile"
    );
}

#[tokio::test(start_paused = true)]
async fn linked_worktree_operation_holds_real_debounce_until_marker_clears() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let watcher = GitWatcher::new(fast_watch_config());
    let max_delay_ms = watcher.inner.config.watch_max_delay_ms;
    let Some(state) = ensure_watching_or_skip(&watcher, &primary).await else {
        return;
    };
    assert_eq!(
        watcher.ensure_watching(&linked).await,
        GitWatcherAdmission::Ready
    );
    tokio::time::timeout(TEST_READY_TIMEOUT, state.entered_debounce.notified())
        .await
        .expect("linked-worktree registration must rebuild the repository watcher");
    let linked_git_dir = worktree_git_dir(&linked).expect("linked git dir");
    let marker = linked_git_dir.join("CHERRY_PICK_HEAD");
    std::fs::write(&marker, b"operation").expect("create operation marker");

    let event = notify::Event {
        kind: EventKind::Create(notify::event::CreateKind::File),
        paths: vec![marker.clone()],
        attrs: EventAttributes::default(),
    };
    classify_and_mark(&state, &event);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(max_delay_ms + 1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        state.drained_plans.load(Ordering::Relaxed),
        0,
        "a linked-worktree operation must hold past the ordinary hard deadline"
    );

    std::fs::remove_file(&marker).expect("clear operation marker");
    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![marker],
            attrs: EventAttributes::default(),
        },
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::timeout(TEST_READY_TIMEOUT, state.plan_drained.notified())
        .await
        .expect("clearing the linked-worktree marker must release the debounce");
    assert_eq!(state.drained_plans.load(Ordering::Relaxed), 1);
    watcher.shutdown().await;
}

#[tokio::test]
async fn metadata_frontier_routes_to_the_mounted_canonical_scheduler() {
    let repo = temp_repo();
    let store = tempfile::tempdir().expect("code-index store");
    let schedulers = super::super::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(2);
    schedulers
        .mount_worktree(
            tracedecay_domain::ProjectId::new("project.git-watcher-test")
                .expect("valid project identity"),
            repo.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount canonical scheduler");

    let watcher = GitWatcher::new_with_scheduler(
        fast_watch_config(),
        MaintenanceCoordinator::default(),
        schedulers.clone(),
    );
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        schedulers.shutdown().await;
        return;
    };
    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![state.common_dir.join("HEAD")],
            attrs: EventAttributes::default(),
        },
    );
    tokio::time::timeout(TEST_READY_TIMEOUT, async {
        while state.health.snapshot().last_freshness_request == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("debounce must route the metadata frontier");

    assert_ne!(
        state.health.snapshot().last_freshness_request,
        0,
        "the mounted scheduler must accept the exact watcher frontier"
    );
    watcher.shutdown().await;
    schedulers.shutdown().await;
}

#[tokio::test]
async fn disabled_watcher_never_registers() {
    let repo = temp_repo();
    let mut config = fast_watch_config();
    config.auto_watch = false;
    let watcher = GitWatcher::new(config);
    assert_eq!(
        watcher.ensure_watching(repo.path()).await,
        GitWatcherAdmission::Disabled
    );
    assert_eq!(watcher.spawn().await, GitWatcherStart::Disabled);
    assert!(watcher.health_report().await.is_empty());
}

#[tokio::test]
async fn missing_or_dangling_project_identity_is_rejected() {
    let tmp = tempfile::tempdir().expect("identity fixture");
    let missing = tmp.path().join("missing");
    let dangling = tmp.path().join("dangling");
    std::os::unix::fs::symlink(&missing, &dangling).expect("dangling project symlink");
    let watcher = GitWatcher::new(fast_watch_config());

    for root in [&missing, &dangling] {
        let outcome = watcher.ensure_watching(root).await;
        assert_eq!(
            outcome,
            GitWatcherAdmission::NotRepository,
            "an unresolved project root must not be admitted as a watcher identity"
        );
    }
    assert!(
        watcher.inner.projects.lock().await.is_empty(),
        "rejected identities must not create a repository owner"
    );
}

#[tokio::test]
async fn shutdown_cancels_and_joins_repository_watcher_tasks() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    watcher.shutdown().await;

    assert!(watcher.inner.projects.lock().await.is_empty());
    assert!(!state.has_retained_task());
    assert_eq!(
        watcher.ensure_watching(repo.path()).await,
        GitWatcherAdmission::ShuttingDown,
        "shutdown admission must remain distinct from a disabled watcher"
    );
    assert_eq!(watcher.spawn().await, GitWatcherStart::ShuttingDown);
    let drained_before = state.drained_plans.load(Ordering::Acquire);
    git(
        repo.path(),
        &["commit", "--allow-empty", "-m", "after shutdown"],
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        state.drained_plans.load(Ordering::Acquire),
        drained_before,
        "shutdown must join the repository task instead of detaching its notify watcher"
    );
    assert!(
        state.dirty.lock().await.is_clean(),
        "metadata events after shutdown must not reach detached watcher state"
    );
}

#[tokio::test]
async fn shutdown_cancels_and_joins_active_metadata_scan() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };
    state.operation_scan_probe.arm();
    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![state.common_dir.join("HEAD")],
            attrs: EventAttributes::default(),
        },
    );
    tokio::time::timeout(
        TEST_READY_TIMEOUT,
        state.operation_scan_probe.entered.notified(),
    )
    .await
    .expect("the bounded metadata scan must start");

    watcher.cancel();
    tokio::time::timeout(TEST_READY_TIMEOUT, watcher.shutdown())
        .await
        .expect("shutdown must join the active metadata scan after phase-one cancellation");
    assert_eq!(
        state.operation_scan_probe.active(),
        0,
        "no watcher-owned blocking scan may survive shutdown"
    );
    assert!(!state.has_retained_task());
}

/// The safety-critical property that justifies this metadata watcher over the
/// removed #80 working-tree watcher: a plain source-file edit (no git
/// operation) must NOT trigger any scheduler freshness request. We drive the
/// real watcher task and assert `last_freshness_request` never advances.
///
/// This test proves a NEGATIVE about a REAL inotify event (a working-tree
/// write that must not be delivered/acted on), so it deliberately runs on the
/// real clock — paused time cannot manufacture "an OS event that never
/// arrives". Determinism instead comes from making both the readiness and the
/// negative window OBSERVABLE rather than fixed sleeps:
///   1. We wait on the `entered_debounce` state signal, so the watch is
///      PROVABLY installed before the edit — closing the old false-pass
///      window where a 200ms sleep elapsed before inotify was armed (a real
///      regression could then slip through unseen).
///   2. After the edit we poll the accepted-request timestamp across a window
///      several times the
///      debounce+max-delay budget and fail on the FIRST advance. A scheduler
///      stall only lengthens the safe window — it can never produce a false
///      negative — so no magic epsilon is needed.
#[tokio::test]
async fn source_file_edit_triggers_no_freshness_request() {
    let repo = temp_repo();
    let config = fast_watch_config();
    let debounce_ms = config.watch_debounce_ms;
    let max_delay_ms = config.watch_max_delay_ms;
    let watcher = GitWatcher::new(config);
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    let baseline = state.health.snapshot().last_freshness_request;

    std::fs::write(repo.path().join("a.txt"), "changed by editor\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "brand new file\n").unwrap();

    // Poll across a window MUCH larger than debounce + max-delay, failing
    // fast on the FIRST sign of a spurious reaction. We assert TWO things at
    // every tick, so the test is non-vacuous even against an unindexed repo
    // (where an unmounted scheduler would reject a request):
    //   * the accepted-request timestamp never advances, AND
    //   * the dirty set never becomes marked — no working-tree event ever
    //     reached `classify_and_mark`. The dirty mark is the ROOT observable:
    //     if a regression recursively watched the working tree, the edit
    //     would set `dirty` for the ~debounce+max-delay window, which this
    //     20ms poll catches before the loop drains it. A scheduler stall only
    //     widens both safe windows; it cannot fabricate a false negative.
    let window = Duration::from_millis((debounce_ms + max_delay_ms) * 4 + 500);
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        assert_eq!(
            state.health.snapshot().last_freshness_request,
            baseline,
            "a source edit must not request metadata-driven freshness"
        );
        assert!(
            state.dirty.lock().await.is_clean(),
            "a working-tree source edit must never mark the dirty set \
             (the metadata-only watcher must not watch the working tree)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Final check after the full observation window.
    assert_eq!(
        state.health.snapshot().last_freshness_request,
        baseline,
        "a source edit must not request metadata-driven freshness"
    );
    assert!(
        state.dirty.lock().await.is_clean(),
        "a working-tree source edit must never mark the dirty set"
    );
}

/// The real debounce path (`repository_task` → `debounce_loop`) coalesces a
/// burst of metadata events into a single drained pass. This drives the
/// live task (not a reimplemented helper) and injects events through the real
/// notify-callback body (`classify_and_mark`), then asserts one debounce drain
/// and a truthful accepted-or-bounded-retry handoff.
///
/// Deterministic under `start_paused = true`: there is no wall-clock guess.
/// Readiness is a state signal (`entered_debounce`), and the coalesce sleep
/// is driven by `tokio::time::advance` PAST the hard cap, so the drain is
/// forced to fire regardless of scheduler latency. The coalescing guarantee
/// is still fully asserted: the set is dirty before time advances and exactly
/// one plan drains; the separate lifecycle test fixes the retry bound.
#[tokio::test(start_paused = true)]
async fn debounce_loop_coalesces_and_drains_events() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let max_delay_ms = watcher.inner.config.watch_max_delay_ms;
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    for i in 0..5 {
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![state.common_dir.join(format!("refs/heads/feat/{i}"))],
            attrs: EventAttributes::default(),
        };
        classify_and_mark(&state, &event);
    }
    assert!(
        !state.dirty.lock().await.is_clean(),
        "events should mark the dirty set before the debounce fires"
    );

    // Let the loop observe the burst and park on the debounce sleep.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    tokio::time::advance(Duration::from_millis(max_delay_ms + 1)).await;

    let drained = tokio::time::timeout(TEST_READY_TIMEOUT, state.plan_drained.notified())
        .await
        .is_ok();
    assert!(
        drained,
        "the real debounce loop must coalesce the event burst and drain the dirty set"
    );
    assert_eq!(
        state.drained_plans.load(Ordering::Relaxed),
        1,
        "one event burst must produce exactly one coalesced plan"
    );
    let clean = state.dirty.lock().await.is_clean();
    assert!(
        clean || state.retry_not_before().is_some(),
        "drained work is either accepted or retained behind a bounded retry"
    );
}
