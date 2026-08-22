use super::*;

use notify::event::EventAttributes;
use std::process::Command;
use tokio::sync::oneshot;

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
        wake: Notify::new(),
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
}

#[test]
fn ref_lock_sidecar_does_not_become_a_branch() {
    let state = Arc::new(WatchState {
        project_root: PathBuf::from("/repo"),
        dirty: Mutex::new(DirtySet::default()),
        wake: Notify::new(),
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

#[test]
fn timed_out_identity_discovery_retries_instead_of_degrading_forever() {
    assert!(matches!(
        identity_discovery_disposition(crate::worktree::GitRepoIdentityOutcome::Unknown),
        IdentityDiscoveryDisposition::Retry
    ));
    assert!(matches!(
        identity_discovery_disposition(crate::worktree::GitRepoIdentityOutcome::NotFound),
        IdentityDiscoveryDisposition::Degraded
    ));
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
fn fast_watch_config() -> SyncConfig {
    let mut config = SyncConfig {
        auto_watch: true,
        ..SyncConfig::default()
    };
    config.watch_debounce_ms = 50;
    config.watch_max_delay_ms = 500;
    config.watch_max_projects = 32;
    config.backstop_interval_mins = 0; // no backstop noise in these tests
    config.max_concurrent_syncs = 2;
    config
}

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
    let state = {
        let projects = watcher.inner.projects.lock().await;
        Arc::clone(projects.get(&canonical).expect("project registered"))
    };
    assert!(
        tokio::time::timeout(Duration::from_secs(30), state.entered_debounce.notified())
            .await
            .is_ok(),
        "watch task must reach debounce_loop"
    );
    state
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
async fn spawn_skips_recent_registry_rows_without_an_initialized_store() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let global_db_path = profile_root.join("global.db");
    let global_db = crate::global_db::GlobalDb::open_at(&global_db_path)
        .await
        .expect("open isolated global registry");

    let valid = temp_repo();
    crate::storage::write_enrollment_marker(
        valid.path(),
        &crate::storage::EnrollmentMarker {
            project_id: "proj_valid_watch".to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("write valid enrollment marker");
    let layout = crate::storage::resolve_layout_for_current_profile(valid.path())
        .expect("resolve valid project layout");
    std::fs::create_dir_all(layout.graph_db_path.parent().unwrap())
        .expect("create valid graph directory");
    std::fs::write(&layout.graph_db_path, b"").expect("create valid graph marker");
    global_db
        .upsert_code_project("proj_valid_watch", valid.path(), None, None, Some("main"))
        .await
        .expect("register valid project");

    let invalid = tempfile::tempdir().unwrap();
    crate::storage::write_enrollment_marker(
        invalid.path(),
        &crate::storage::EnrollmentMarker {
            project_id: "proj_invalid_watch".to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("write stale enrollment marker");
    let invalid_layout = crate::storage::resolve_layout_for_current_profile(invalid.path())
        .expect("resolve stale project layout");
    std::fs::create_dir_all(invalid_layout.graph_db_path.parent().unwrap())
        .expect("create stale graph directory");
    std::fs::write(&invalid_layout.graph_db_path, b"").expect("create stale graph marker");
    global_db
        .upsert_code_project(
            "proj_invalid_watch",
            invalid.path(),
            None,
            None,
            Some("main"),
        )
        .await
        .expect("register stale directory-only project");

    let home = dirs::home_dir().expect("test home");
    git(&home, &["init", "-q"]);
    crate::storage::write_enrollment_marker(
        &home,
        &crate::storage::EnrollmentMarker {
            project_id: "proj_protected_home_watch".to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("write protected-home enrollment marker");
    let home_layout = crate::storage::resolve_layout_for_current_profile(&home)
        .expect("resolve protected-home project layout");
    std::fs::create_dir_all(home_layout.graph_db_path.parent().unwrap())
        .expect("create protected-home graph directory");
    std::fs::write(&home_layout.graph_db_path, b"").expect("create protected-home graph marker");
    global_db
        .upsert_code_project("proj_protected_home_watch", &home, None, None, Some("main"))
        .await
        .expect("register protected-home project");

    let watcher = GitWatcher::new(fast_watch_config());
    watcher.spawn(Some(global_db_path)).await;

    let watched = watcher
        .health_report()
        .await
        .into_iter()
        .map(|(path, _)| path)
        .collect::<HashSet<_>>();
    assert!(
        watched.contains(&valid.path().canonicalize().unwrap()),
        "an initialized registered project must still be watched"
    );
    assert!(
        !watched.contains(&invalid.path().canonicalize().unwrap()),
        "a stale registry row for an existing non-project directory must not start a watcher"
    );
    assert!(
        !watched.contains(&home.canonicalize().unwrap()),
        "the user home must never start a watcher even when stale metadata looks initialized"
    );

    watcher.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_and_joins_watcher_tasks() {
    let repo = temp_repo();
    let profile = tempfile::tempdir().unwrap();
    let mut config = fast_watch_config();
    config.backstop_interval_mins = 1;
    let watcher = GitWatcher::new(config);
    watcher.spawn(Some(profile.path().join("global.db"))).await;
    watcher.ensure_watching(repo.path()).await;
    let state = ready_registered_state(&watcher, repo.path()).await;

    watcher.shutdown().await;

    assert!(watcher.inner.projects.lock().await.is_empty());
    assert!(state.task.lock().await.is_none());
    assert!(watcher.inner.backstop_task.lock().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_deadline_aborts_project_tasks_before_waiting_for_blocked_backstop() {
    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let watcher = GitWatcher::new(fast_watch_config());
    let project_path = PathBuf::from("/tmp/shutdown-deadline-project");
    let (project_started_tx, project_started_rx) = oneshot::channel();
    let (project_aborted_tx, project_aborted_rx) = oneshot::channel();
    let project_task = tokio::spawn(async move {
        let _notify_on_drop = NotifyOnDrop(Some(project_aborted_tx));
        project_started_tx.send(()).expect("announce project task");
        std::future::pending::<()>().await;
    });
    project_started_rx.await.expect("project task started");
    let state = Arc::new(WatchState {
        project_root: project_path.clone(),
        dirty: Mutex::new(DirtySet::default()),
        wake: Notify::new(),
        health: ProjectHealth::default(),
        task: Mutex::new(Some(project_task)),
        entered_debounce: Notify::new(),
        drained_plans: AtomicU64::new(0),
        plan_drained: Notify::new(),
    });
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(project_path, Arc::clone(&state));

    let (backstop_started_tx, backstop_started_rx) = oneshot::channel();
    let (release_backstop_tx, release_backstop_rx) = std::sync::mpsc::channel();
    let (backstop_finished_tx, backstop_finished_rx) = oneshot::channel();
    let backstop_task = tokio::task::spawn_blocking(move || {
        backstop_started_tx
            .send(())
            .expect("announce blocked backstop");
        release_backstop_rx
            .recv()
            .expect("release blocked backstop");
        let _ = backstop_finished_tx.send(());
    });
    backstop_started_rx.await.expect("backstop task started");
    *watcher.inner.backstop_task.lock().await = Some(backstop_task);

    tokio::time::timeout(
        Duration::from_millis(250),
        watcher.shutdown_with_deadline(Duration::from_millis(25)),
    )
    .await
    .expect("shutdown must return by its deadline");
    tokio::time::timeout(Duration::from_millis(100), project_aborted_rx)
        .await
        .expect("project task must be aborted before waiting on the blocked backstop")
        .expect("project abort notification");

    assert!(watcher.inner.projects.lock().await.is_empty());
    assert!(state.task.lock().await.is_none());
    assert!(watcher.inner.backstop_task.lock().await.is_none());

    release_backstop_tx
        .send(())
        .expect("release blocked backstop");
    backstop_finished_rx
        .await
        .expect("blocked backstop finished");
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
    watcher.ensure_watching(repo.path()).await;

    let state = ready_registered_state(&watcher, repo.path()).await;

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
    watcher.ensure_watching(repo.path()).await;

    let state = ready_registered_state(&watcher, repo.path()).await;

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

    let drained = tokio::time::timeout(Duration::from_secs(30), state.plan_drained.notified())
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
