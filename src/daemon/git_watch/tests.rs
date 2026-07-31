use super::*;

use notify::event::EventAttributes;
use std::process::Command;
use tokio::sync::oneshot;

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
    assert_eq!(store_maintenance::retention_window_secs(u64::MAX), i64::MAX);
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
    let state = Arc::new(WatchState {
        project_root: PathBuf::from("/tmp/x"),
        dirty: Mutex::new(DirtySet::default()),
        reconciliation_pending: AtomicBool::new(false),
        wake: Notify::new(),
        maintenance: MaintenanceCoordinator::default(),
        health: ProjectHealth::default(),
        task: Mutex::new(None),
        entered_debounce: Notify::new(),
        drained_plans: AtomicU64::new(0),
        plan_drained: Notify::new(),
    });
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
    let state = Arc::new(WatchState {
        project_root: PathBuf::from("/repo"),
        dirty: Mutex::new(DirtySet::default()),
        reconciliation_pending: AtomicBool::new(false),
        wake: Notify::new(),
        maintenance: MaintenanceCoordinator::default(),
        health: ProjectHealth::default(),
        task: Mutex::new(None),
        entered_debounce: Notify::new(),
        drained_plans: AtomicU64::new(0),
        plan_drained: Notify::new(),
    });
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
    let state = Arc::new(WatchState {
        project_root: PathBuf::from("/repo"),
        dirty: Mutex::new(DirtySet::default()),
        reconciliation_pending: AtomicBool::new(false),
        wake: Notify::new(),
        maintenance: MaintenanceCoordinator::default(),
        health: ProjectHealth::default(),
        task: Mutex::new(None),
        entered_debounce: Notify::new(),
        drained_plans: AtomicU64::new(0),
        plan_drained: Notify::new(),
    });
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
    let fresh = ProjectHealthSnapshot {
        last_heartbeat: now_secs(),
        last_sync: 0,
        degraded: false,
    };
    assert!(!fresh.heartbeat_stale());
    let never = ProjectHealthSnapshot {
        last_heartbeat: 0,
        last_sync: 0,
        degraded: false,
    };
    assert!(never.heartbeat_stale());
    let old = ProjectHealthSnapshot {
        last_heartbeat: now_secs().saturating_sub(HEARTBEAT_STALE_SECS + 10),
        last_sync: 0,
        degraded: false,
    };
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
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
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
    watcher.ensure_watching(repo).await;
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
        Ok(Ready::Degraded) if currently_watch_limited(repo) => {
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
    assert!(watcher.is_enabled());

    watcher.ensure_watching(repo_a.path()).await;
    assert_eq!(watcher.health_report().await.len(), 1);

    watcher.ensure_watching(repo_a.path()).await;
    assert_eq!(watcher.health_report().await.len(), 1);

    watcher.ensure_watching(repo_b.path()).await;
    assert_eq!(watcher.health_report().await.len(), 2);

    watcher.ensure_watching(repo_c.path()).await;
    assert_eq!(watcher.health_report().await.len(), 2);
}

#[tokio::test]
async fn disabled_watcher_never_registers() {
    let repo = temp_repo();
    let mut config = fast_watch_config();
    config.auto_watch = false;
    let watcher = GitWatcher::new(config);
    assert!(!watcher.is_enabled());
    watcher.ensure_watching(repo.path()).await;
    assert!(watcher.health_report().await.is_empty());
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

/// The safety-critical property that justifies this metadata watcher over the
/// removed #80 working-tree watcher: a plain source-file edit (no git
/// operation) must NOT trigger any watcher sync. We drive the REAL watcher
/// task and assert `last_sync` never advances.
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
///   2. After the edit we poll `last_sync` across a window several times the
///      debounce+max-delay budget and fail on the FIRST advance. A scheduler
///      stall only lengthens the safe window — it can never produce a false
///      negative — so no magic epsilon is needed.
#[tokio::test]
async fn source_file_edit_triggers_no_sync() {
    let repo = temp_repo();
    let config = fast_watch_config();
    let debounce_ms = config.watch_debounce_ms;
    let max_delay_ms = config.watch_max_delay_ms;
    let watcher = GitWatcher::new(config);
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    let baseline = state.health.snapshot().last_sync;

    std::fs::write(repo.path().join("a.txt"), "changed by editor\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "brand new file\n").unwrap();

    // Poll across a window MUCH larger than debounce + max-delay, failing
    // fast on the FIRST sign of a spurious reaction. We assert TWO things at
    // every tick, so the test is non-vacuous even against an unindexed repo
    // (where a sync would no-op and never move `last_sync`):
    //   * `last_sync` never advances — no sync ran, AND
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
            state.health.snapshot().last_sync,
            baseline,
            "a working-tree source edit must not advance last_sync (metadata-only watcher)"
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
        state.health.snapshot().last_sync,
        baseline,
        "a working-tree source edit must not advance last_sync (metadata-only watcher)"
    );
    assert!(
        state.dirty.lock().await.is_clean(),
        "a working-tree source edit must never mark the dirty set"
    );
}

/// The REAL debounce path (`project_task` → `debounce_loop`) coalesces a
/// burst of metadata events into a single drained pass: after events stop,
/// the dirty set is taken exactly once and returns to clean. This drives the
/// live task (not a reimplemented helper) and injects events through the real
/// notify-callback body (`classify_and_mark`), then asserts the debounce loop
/// drains them.
///
/// Deterministic under `start_paused = true`: there is no wall-clock guess.
/// Readiness is a state signal (`entered_debounce`), and the coalesce sleep
/// is driven by `tokio::time::advance` PAST the hard cap, so the drain is
/// forced to fire regardless of scheduler latency. The coalescing guarantee
/// is still fully asserted: the set is dirty before time advances and clean
/// after exactly one drain (a per-event re-fire would either not reach clean
/// or would leave residue across the burst).
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
            paths: vec![state.project_root.join(format!(".git/refs/heads/feat/{i}"))],
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
    assert!(
        state.dirty.lock().await.is_clean(),
        "draining the coalesced plan must clear the dirty set"
    );
}
