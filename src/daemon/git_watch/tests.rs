use super::super::maintenance::retention_window_secs;
use super::*;

use notify::event::EventAttributes;
use std::process::Command;
use tokio::sync::oneshot;

mod metadata_events;

fn test_watch_state(project_root: impl Into<PathBuf>) -> Arc<WatchState> {
    let project_root = project_root.into();
    Arc::new(WatchState::new(
        project_root.clone(),
        Some(project_root),
        MaintenanceCoordinator::default(),
    ))
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
fn failed_branch_compaction_keeps_maintenance_retry_eligible() {
    let report = crate::retention::branch_compaction::BranchCompactionReport {
        compacted: Vec::new(),
        skipped: vec![crate::retention::branch_compaction::BranchCompactionSkip {
            branch: "busy".to_string(),
            db_path: PathBuf::from("/tmp/busy.db"),
            reason: crate::retention::branch_compaction::BranchCompactionSkipReason::Busy,
        }],
        policy_invalid: false,
    };

    assert!(
        !store_maintenance::branch_compaction_succeeded(&report),
        "a skipped branch store must keep the maintenance cadence eligible for retry"
    );
}

#[test]
fn dirty_set_coalesces_and_takes_once() {
    let mut set = DirtySet::default();
    assert!(set.is_clean());
    set.dirty = true;
    set.branches.insert("feat/a".to_string());
    set.branches.insert("feat/a".to_string()); // dedup
    set.branches.insert("feat/b".to_string());
    assert!(!set.is_clean());

    let plan = set.take();
    assert!(plan.dirty);
    assert_eq!(plan.branches.len(), 2);
    assert!(set.is_clean());
    let empty = set.take();
    assert!(empty.is_empty());
}

#[test]
fn ref_event_marks_branch_and_delete_marks_gc() {
    let state = test_watch_state("/tmp/x");
    let create = notify::Event {
        kind: EventKind::Create(notify::event::CreateKind::File),
        paths: vec![PathBuf::from("/repo/.git/refs/heads/feat/x")],
        attrs: EventAttributes::default(),
    };
    classify_and_mark(&state, &create);
    let remove = notify::Event {
        kind: EventKind::Remove(notify::event::RemoveKind::Folder),
        paths: vec![PathBuf::from("/repo/.git/worktrees/wt1")],
        attrs: EventAttributes::default(),
    };
    classify_and_mark(&state, &remove);

    let dirty = state.dirty.blocking_lock();
    assert!(dirty.dirty);
    assert!(dirty.branches.contains("feat/x"));
    assert!(dirty.gc_eligible);
    assert!(!dirty.reconcile_metadata);
    assert!(!state.reconciliation_pending.load(Ordering::Acquire));
}

#[test]
fn ref_lock_sidecar_does_not_become_a_branch() {
    let state = test_watch_state("/repo");
    let event = notify::Event {
        kind: EventKind::Create(notify::event::CreateKind::File),
        paths: vec![PathBuf::from("/repo/.git/refs/heads/codex/topic.lock")],
        attrs: EventAttributes::new(),
    };

    classify_and_mark(&state, &event);

    let dirty = state
        .dirty
        .try_lock()
        .expect("dirty set should be unlocked");
    assert!(dirty.branches.is_empty(), "git lock sidecars are not refs");
}

#[tokio::test]
async fn contended_event_requests_a_bounded_reconciliation() {
    let state = test_watch_state("/repo");
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
fn linked_worktree_inventory_ignores_non_directories() {
    let common = tempfile::tempdir().unwrap();
    let worktrees = common.path().join("worktrees");
    std::fs::create_dir_all(worktrees.join("wt-a")).unwrap();
    std::fs::create_dir_all(worktrees.join("nested")).unwrap();
    std::fs::write(worktrees.join("not-a-worktree"), b"ignored").unwrap();

    let names = store_maintenance::linked_worktree_names(common.path());

    assert_eq!(names.len(), 2);
    assert!(names.contains("wt-a"));
    assert!(names.contains("nested"));
}

#[test]
fn heartbeat_staleness() {
    let fresh = ProjectHealth::default();
    fresh.last_heartbeat.store(now_secs(), Ordering::Relaxed);
    assert!(!fresh.heartbeat_stale());
    let never = ProjectHealth::default();
    assert!(never.heartbeat_stale());
    let old = ProjectHealth::default();
    old.last_heartbeat.store(
        now_secs().saturating_sub(HEARTBEAT_STALE_SECS + 10),
        Ordering::Relaxed,
    );
    assert!(old.heartbeat_stale());
}

/// The shared coordinator must not start a second store-writing lifetime while
/// the first one is held. Paused Tokio time plus Notify/oneshot handshakes make
/// this a scheduling-state assertion rather than a wall-clock sleep.
#[tokio::test(start_paused = true)]
async fn writer_administration_blocks_until_the_gate_is_released() {
    let administration = StoreAdministration::default();
    let holder_entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let holder = {
        let administration = administration.clone();
        let holder_entered = Arc::clone(&holder_entered);
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            administration
                .with_writer(move || async move {
                    holder_entered.notify_one();
                    release.notified().await;
                })
                .await;
        })
    };
    tokio::time::timeout(Duration::from_secs(1), holder_entered.notified())
        .await
        .expect("holder must acquire the writer gate");

    let (waiter_ready_tx, waiter_ready_rx) = oneshot::channel();
    let waiter_entered = Arc::new(Notify::new());
    let waiter = {
        let administration = administration.clone();
        let waiter_entered = Arc::clone(&waiter_entered);
        tokio::spawn(async move {
            waiter_ready_tx
                .send(())
                .expect("waiter readiness receiver must remain alive");
            administration
                .with_writer(move || async move {
                    waiter_entered.notify_one();
                })
                .await;
        })
    };
    waiter_ready_rx
        .await
        .expect("waiter task must reach the gate");
    tokio::task::yield_now().await;

    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiter_entered.notified())
            .await
            .is_err(),
        "a second writer must remain blocked while the first writer holds the gate"
    );

    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), waiter_entered.notified())
        .await
        .expect("releasing the gate must admit the waiting writer");
    tokio::time::timeout(Duration::from_secs(1), holder)
        .await
        .expect("holder task must finish")
        .expect("holder task must not panic");
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("waiter task must finish")
        .expect("waiter task must not panic");
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
    config.max_concurrent_syncs = 2;
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
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"]);
    dir
}

async fn ready_registered_state(watcher: &GitWatcher, repo: &Path) -> Arc<WatchState> {
    let canonical = watcher_key(repo);
    let projects = watcher.inner.projects.lock().await;
    Arc::clone(projects.get(&canonical).expect("project registered"))
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
fn currently_watch_limited(repo: &Path) -> bool {
    let Some(common) = crate::worktree::git_common_dir(repo) else {
        return false;
    };
    let Ok(mut probe) = notify::recommended_watcher(|_res: notify::Result<notify::Event>| {})
    else {
        return false;
    };
    matches!(install_watches(&mut probe, &common), Err(e) if is_watch_limit_error(&e))
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
/// real watch task run and wait for the actual debounce-loop readiness signal.
/// If that does not arrive in time, a fresh matching watch attempt proves an
/// environmental inotify limit before the test is skipped; any other timeout
/// remains a regression.
async fn ensure_watching_or_skip(watcher: &GitWatcher, repo: &Path) -> Option<Arc<WatchState>> {
    watcher.ensure_watching(repo).await;
    let state = ready_registered_state(watcher, repo).await;

    match tokio::time::timeout(TEST_READY_TIMEOUT, state.entered_debounce.notified()).await {
        Ok(()) => Some(state),
        Err(_) if currently_watch_limited(repo) => {
            eprintln!(
                "SKIP: OS inotify watch limit reached (fs.inotify.max_user_watches \
                 exhausted); raise it to exercise the real git_watch debounce path"
            );
            None
        }
        Err(_) => panic!("watch task must reach debounce_loop"),
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
    assert!(watcher.is_enabled());

    watcher.ensure_watching(repo_a.path()).await;
    assert_eq!(watcher.inner.projects.lock().await.len(), 1);

    watcher.ensure_watching(repo_a.path()).await;
    assert_eq!(watcher.inner.projects.lock().await.len(), 1);

    watcher.ensure_watching(repo_b.path()).await;
    assert_eq!(watcher.inner.projects.lock().await.len(), 2);

    watcher.ensure_watching(repo_c.path()).await;
    assert_eq!(watcher.inner.projects.lock().await.len(), 2);
    assert_eq!(watcher.inner.degraded_projects.lock().await.len(), 1);
    watcher.shutdown().await;
}

#[tokio::test]
async fn common_dir_collapses_aliases_but_retains_worktree_snapshots() {
    let repo = temp_repo();
    let linked_parent = tempfile::tempdir().unwrap();
    let linked_root = linked_parent.path().join("linked");
    let linked = linked_root.to_string_lossy().into_owned();
    git(
        repo.path(),
        &["worktree", "add", "-b", "feature/watcher", &linked],
    );

    let alias = repo.path().join(".");

    let watcher = GitWatcher::new(fast_watch_config());
    watcher.ensure_watching(repo.path()).await;
    watcher.ensure_watching(&alias).await;
    watcher.ensure_watching(&linked_root).await;

    let state = ready_registered_state(&watcher, repo.path()).await;
    assert_eq!(watcher.inner.projects.lock().await.len(), 1);
    assert_eq!(
        state.roots().await.len(),
        2,
        "canonical aliases collapse while linked worktree snapshots remain distinct"
    );
    watcher.shutdown().await;
}

#[tokio::test]
async fn removed_worktree_snapshot_root_is_pruned() {
    let repo = temp_repo();
    let state = test_watch_state(repo.path());
    let removed = repo.path().join("removed-linked-worktree");
    state.snapshot_roots.lock().await.insert(removed);

    state.prune_missing_roots().await;

    assert_eq!(state.roots().await, vec![repo.path().to_path_buf()]);
}

#[tokio::test]
async fn resolved_worktree_is_retained_as_a_snapshot_root() {
    let repo = temp_repo();
    let linked_parent = tempfile::tempdir().unwrap();
    let linked_root = linked_parent.path().join("linked");
    let linked = linked_root.to_string_lossy().into_owned();
    git(
        repo.path(),
        &["worktree", "add", "-b", "feature/snapshot-root", &linked],
    );
    let state = test_watch_state(repo.path());

    state.register_snapshot_root(&linked_root).await;

    let roots = state.roots().await;
    assert_eq!(roots.len(), 2);
    assert!(
        roots.contains(&repo.path().to_path_buf()) && roots.contains(&linked_root),
        "a resolved linked worktree must participate in later generation checks"
    );
}

#[tokio::test(start_paused = true)]
async fn degraded_retry_timer_merges_drained_work_without_backstop() {
    let state = test_watch_state("/repo");
    let mut first = DirtyPlan::sync();
    first.branches.insert("main".to_string());
    let mut second = DirtyPlan::sync();
    second.new_worktrees.insert("feature".to_string());

    state.schedule_retry(first, Duration::from_hours(1)).await;
    let waiting_state = Arc::clone(&state);
    let waiting = tokio::spawn(async move { wait_for_degraded_retry(&waiting_state).await });
    tokio::task::yield_now().await;

    state.schedule_retry(second, Duration::from_secs(1)).await;
    let retry_deadline = state
        .retry_deadline()
        .await
        .expect("merged retry work should retain a deadline");
    assert!(
        retry_deadline <= Instant::now() + Duration::from_mins(1),
        "retry delay must be bounded even when a caller supplies a longer wait"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let plan = waiting
        .await
        .expect("degraded retry timer task should not panic")
        .expect("the earlier retry should release the merged drained work");
    assert!(plan.dirty);
    assert!(plan.branches.contains("main"));
    assert!(plan.new_worktrees.contains("feature"));
}

#[test]
fn snapshot_generation_ignores_dirty_tree_and_index_until_head_moves() {
    let repo = temp_repo();
    let before = snapshot_generation(repo.path()).unwrap();
    assert_eq!(snapshot_generation(repo.path()).unwrap(), before);

    std::fs::write(repo.path().join("a.txt"), "next\n").unwrap();
    assert_eq!(snapshot_generation(repo.path()).unwrap(), before);

    git(repo.path(), &["add", "a.txt"]);
    assert_eq!(snapshot_generation(repo.path()).unwrap(), before);

    git(repo.path(), &["commit", "-m", "next"]);

    let after = snapshot_generation(repo.path()).unwrap();
    assert_ne!(after, before);
    assert_eq!(after.root, before.root);
    assert_eq!(after.branch.as_deref(), Some("main"));
}

#[test]
fn snapshot_generation_changes_when_head_ref_changes_at_same_commit() {
    let repo = temp_repo();
    let before = snapshot_generation(repo.path()).unwrap();

    git(repo.path(), &["checkout", "-b", "feature/generation"]);

    let after = snapshot_generation(repo.path()).unwrap();
    assert_ne!(after, before);
    assert_eq!(after.root, before.root);
    assert_eq!(after.branch.as_deref(), Some("feature/generation"));
}

#[tokio::test(start_paused = true)]
async fn generation_gate_skips_success_and_backs_off_failure() {
    let root = PathBuf::from("/repo");
    let first = SnapshotGeneration::test(root.clone(), "main", "a");
    let second = SnapshotGeneration::test(root, "main", "b");
    let mut gate = GenerationGate::default();
    let now = Instant::now();

    let first_reservation = gate
        .reserve(&first, now)
        .expect("first generation should claim the sync lane");
    assert_eq!(gate.decision(&first, now), GenerationDecision::InFlight);
    gate.record_success(first.clone());
    drop(first_reservation);
    assert_eq!(gate.decision(&first, now), GenerationDecision::Unchanged);
    let second_reservation = gate
        .reserve(&second, now)
        .expect("new generation should claim the sync lane");

    gate.record_failure(second.clone(), now);
    drop(second_reservation);
    assert!(matches!(
        gate.decision(&second, now),
        GenerationDecision::Backoff { .. }
    ));
    tokio::time::advance(SYNC_RETRY_INITIAL).await;
    assert!(
        gate.reserve(&second, Instant::now()).is_ok(),
        "a backoff-expired generation should claim the sync lane"
    );
}

#[tokio::test(start_paused = true)]
async fn generation_gate_serializes_different_generations() {
    let first = SnapshotGeneration::test("/repo", "main", "a");
    let second = SnapshotGeneration::test("/repo", "main", "b");
    let mut gate = GenerationGate::default();
    let now = Instant::now();

    let first_reservation = gate
        .reserve(&first, now)
        .expect("first generation should claim the sync lane");
    assert!(
        matches!(gate.reserve(&second, now), Err(ReservationError::InFlight)),
        "a newer generation must not replace the receipt for an active sync"
    );
    gate.record_success(first);
    drop(first_reservation);
    assert!(
        gate.reserve(&second, now).is_ok(),
        "the newer generation remains eligible after the active sync finishes"
    );
}

#[test]
fn stale_generation_never_records_success_for_a_newer_snapshot() {
    let first = SnapshotGeneration::test("/repo", "main", "a");
    let second = SnapshotGeneration::test("/repo", "main", "b");
    let mut gate = GenerationGate::default();
    let reservation = gate
        .reserve(&first, Instant::now())
        .expect("first generation should claim the sync lane");

    assert!(
        !gate.record_success_if_current(first.clone(), &second),
        "a semaphore-delayed watcher must not stamp an older generation as fresh"
    );
    drop(reservation);
    assert!(
        gate.reserve(&second, Instant::now()).is_ok(),
        "the newer generation remains eligible after stale work is discarded"
    );
}

#[test]
fn semaphore_delayed_generation_releases_its_claim_before_sync() {
    let first = SnapshotGeneration::test("/repo", "main", "a");
    let second = SnapshotGeneration::test("/repo", "main", "b");
    let mut gate = GenerationGate::default();
    let reservation = gate
        .reserve(&first, Instant::now())
        .expect("first generation should claim the sync lane");

    assert!(
        gate.release_if_stale(&first, &second),
        "a generation that moved while waiting for the semaphore must not sync"
    );
    drop(reservation);
    assert!(
        gate.reserve(&second, Instant::now()).is_ok(),
        "the newer generation must be retried after the stale claim is released"
    );
}

#[test]
fn deferred_worktree_tracking_is_retryable_not_successful() {
    assert!(!planner::worktree_tracking_succeeded(
        &crate::branch::BranchAddOutcome::Deferred
    ));
    assert!(planner::worktree_tracking_succeeded(
        &crate::branch::BranchAddOutcome::Added
    ));
}

#[test]
fn canceled_generation_reservation_reopens_the_sync_lane() {
    let generation = SnapshotGeneration::test("/repo", "main", "a");
    let mut gate = GenerationGate::default();

    let reservation = gate
        .reserve(&generation, Instant::now())
        .expect("first generation reservation should start a sync");
    assert_eq!(
        gate.decision(&generation, Instant::now()),
        GenerationDecision::InFlight
    );

    drop(reservation);

    assert!(
        gate.reserve(&generation, Instant::now()).is_ok(),
        "cancelling a watcher task must not strand the generation gate"
    );
}

#[tokio::test(start_paused = true)]
async fn generation_failure_backoff_is_bounded_and_resets_on_new_generation() {
    let first = SnapshotGeneration::test("/repo", "main", "a");
    let second = SnapshotGeneration::test("/repo", "main", "b");
    let mut gate = GenerationGate::default();
    let mut expected_delay = SYNC_RETRY_INITIAL;

    for _ in 0..10 {
        let now = Instant::now();
        let reservation = gate
            .reserve(&first, now)
            .expect("the elapsed retry should claim the sync lane");
        gate.record_failure(first.clone(), now);
        drop(reservation);
        assert_eq!(
            gate.decision(&first, now),
            GenerationDecision::Backoff {
                remaining: expected_delay
            }
        );
        tokio::time::advance(expected_delay).await;
        expected_delay = (expected_delay * 2).min(Duration::from_mins(1));
    }
    assert_eq!(expected_delay, Duration::from_mins(1));

    let now = Instant::now();
    let reservation = gate
        .reserve(&second, now)
        .expect("a new generation should claim the sync lane");
    gate.record_failure(second.clone(), now);
    drop(reservation);
    assert_eq!(
        gate.decision(&second, now),
        GenerationDecision::Backoff {
            remaining: SYNC_RETRY_INITIAL
        },
        "a new (root, branch, HEAD) generation resets retry delay"
    );
}

#[test]
fn unchanged_generation_churn_never_reopens_the_sync_lane() {
    let generation = SnapshotGeneration::test("/repo", "main", "same");
    let mut gate = GenerationGate::default();
    gate.record_success(generation.clone());

    for _ in 0..10_000 {
        assert_eq!(
            gate.decision(&generation, Instant::now()),
            GenerationDecision::Unchanged
        );
    }
}

#[tokio::test]
async fn disabled_watcher_never_registers() {
    let repo = temp_repo();
    let mut config = fast_watch_config();
    config.auto_watch = false;
    let watcher = GitWatcher::new(config);
    assert!(!watcher.is_enabled());
    watcher.ensure_watching(repo.path()).await;
    assert!(watcher.inner.projects.lock().await.is_empty());
    assert!(watcher.inner.degraded_projects.lock().await.is_empty());
}

#[tokio::test]
async fn shutdown_cancels_and_joins_project_watcher_tasks() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    watcher.shutdown().await;

    assert!(watcher.inner.projects.lock().await.is_empty());
    assert!(state.task.lock().await.is_none());
}
