use std::sync::atomic::Ordering;
use std::time::Duration;

use notify::EventKind;
use notify::event::EventAttributes;

use super::*;

fn isolated_owner_repo() -> (
    tempfile::TempDir,
    crate::config::PinnedUserDataDir,
    std::path::PathBuf,
) {
    let pin = crate::config::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().expect("isolated profile root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
            .expect("secure isolated profile root");
    }
    let repo = temp_repo();
    (repo, pin, profile_root)
}

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

    std::fs::write(repo.path().join("a.txt"), "changed by editor\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "brand new file\n").unwrap();

    let window = Duration::from_millis((debounce_ms + max_delay_ms) * 4 + 500);
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        assert!(
            state.dirty.lock().await.is_clean(),
            "a working-tree source edit must never mark the dirty set \
             (the metadata-only watcher must not watch the working tree)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(
        state.dirty.lock().await.is_clean(),
        "a working-tree source edit must never mark the dirty set"
    );
}

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

#[tokio::test]
async fn capacity_overflow_uses_one_shared_scheduler_for_every_project() {
    let repo_a = temp_repo();
    let repo_b = temp_repo();
    let repo_c = temp_repo();
    let mut config = fast_watch_config();
    config.watch_max_projects = 1;
    let watcher = GitWatcher::new(config);

    watcher.ensure_watching(repo_a.path()).await;
    watcher.ensure_watching(repo_b.path()).await;
    watcher.ensure_watching(repo_c.path()).await;

    assert_eq!(watcher.inner.projects.lock().await.len(), 1);
    let degraded = watcher.inner.degraded_projects.lock().await;
    assert_eq!(degraded.len(), 2);
    for repo in [repo_b.path(), repo_c.path()] {
        let state = degraded
            .get(&watcher_key(repo))
            .expect("every overflow project must retain shared-poll coverage");
        assert!(
            state.task.lock().await.is_none(),
            "overflow coverage must not create a task per project"
        );
    }
    drop(degraded);
    assert!(
        watcher.inner.overflow_task.lock().await.is_some(),
        "all overflow projects must share one scheduler"
    );
    tokio::time::timeout(TEST_READY_TIMEOUT, async {
        loop {
            let mut covered = true;
            for repo in [repo_b.path(), repo_c.path()] {
                let health = watcher.health_value(Some(repo)).await;
                covered &= health["coverage"] == "degraded_poll"
                    && health["last_heartbeat"]
                        .as_u64()
                        .is_some_and(|beat| beat > 0);
            }
            if covered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the shared overflow scheduler must poll every retained project");
    watcher.shutdown().await;
}

#[tokio::test]
async fn zero_watch_capacity_retains_every_project_in_the_shared_scheduler() {
    let repos = [temp_repo(), temp_repo(), temp_repo()];
    let mut config = fast_watch_config();
    config.watch_max_projects = 0;
    let watcher = GitWatcher::new(config);

    for repo in &repos {
        watcher.ensure_watching(repo.path()).await;
    }

    assert!(watcher.inner.projects.lock().await.is_empty());
    let degraded = watcher.inner.degraded_projects.lock().await;
    assert_eq!(degraded.len(), repos.len());
    for repo in &repos {
        assert!(degraded.contains_key(&watcher_key(repo.path())));
    }
    drop(degraded);
    assert!(watcher.inner.overflow_task.lock().await.is_some());
    watcher.shutdown().await;
}

#[tokio::test]
async fn startup_inventory_registers_existing_linked_worktrees() {
    let repo = temp_repo();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked-before-watcher");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/existing-inventory",
            linked.to_str().expect("linked path"),
        ],
    );
    let watcher = GitWatcher::new(fast_watch_config());
    watcher.ensure_watching(repo.path()).await;
    let state = ready_registered_state(&watcher, repo.path()).await;

    assert!(
        state.roots().await.contains(&linked),
        "startup must inventory existing linked worktree roots"
    );
    watcher.shutdown().await;
}

#[tokio::test]
async fn first_linked_worktree_is_discovered_from_the_common_parent_event() {
    let repo = temp_repo();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("first-linked-worktree");
    let watcher = GitWatcher::new(fast_watch_config());
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/first-worktree",
            linked.to_str().expect("linked path"),
        ],
    );

    tokio::time::timeout(TEST_READY_TIMEOUT, async {
        loop {
            if state.roots().await.contains(&linked) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("common parent watch must discover the first linked worktree");
    watcher.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn failed_gc_execution_requeues_gc_eligibility() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let state = test_watch_state(repo.path());
    let common = crate::worktree::git_common_dir(repo.path()).expect("git common dir");

    execute_plan(&watcher.inner, &state, &common, DirtyPlan::gc()).await;

    tokio::time::advance(SYNC_RETRY_INITIAL).await;
    let plan = state
        .take_due_retry()
        .await
        .expect("failed GC without a retained graph must remain retry eligible");
    assert!(plan.gc_eligible);
    assert!(!plan.dirty && plan.branches.is_empty());
}

#[tokio::test]
async fn backstop_inventory_recovers_a_linked_worktree_missed_by_notify() {
    let repo = temp_repo();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("backstop-linked-worktree");
    let watcher = GitWatcher::new(fast_watch_config());
    watcher.ensure_watching(repo.path()).await;
    let state = ready_registered_state(&watcher, repo.path()).await;
    let task = state.task.lock().await.take().expect("watch task");
    task.abort();
    let _ = task.await;

    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/backstop-inventory",
            linked.to_str().expect("linked path"),
        ],
    );
    assert!(!state.roots().await.contains(&linked));

    let mut last_gc = Some(Instant::now());
    backstop::tick(&watcher, &mut last_gc, Duration::from_hours(24)).await;

    assert!(
        state.roots().await.contains(&linked),
        "the production backstop must inventory worktrees even when no notify task survives"
    );
    watcher.shutdown().await;
}

#[tokio::test]
async fn deferred_worktree_tracking_retries_to_a_synchronized_store() {
    let (repo, _pin, profile_root) = isolated_owner_repo();
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "deferred worktree tracking test",
    )
    .expect("fixture lifecycle authority");
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "deferred worktree tracking test",
    )
    .expect("fixture database authority");
    let owner = TraceDecay::init_with_exclusive_maintenance(
        repo.path(),
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: None,
        },
        &lifecycle,
    )
    .await
    .expect("initialize retained owner graph");
    owner.index_all().await.expect("index owner graph");
    let linked_parent = tempfile::tempdir().unwrap();
    let worktree = linked_parent.path().join("deferred-worktree");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/deferred",
            worktree.to_str().expect("linked path"),
        ],
    );
    let data_root = owner.store_layout().data_root.clone();
    let sync_lock = crate::tracedecay::try_acquire_sync_lock_at(&data_root.join("sync.lock"))
        .expect("hold the owner store sync lock");

    let outcome = owner
        .track_worktree_branch(&worktree, "feature/deferred")
        .await
        .expect("deferred tracking outcome");
    assert_eq!(outcome, crate::branch::BranchAddOutcome::Deferred);
    assert!(
        crate::branch_meta::load_branch_meta(&data_root)
            .is_none_or(|meta| !meta.is_tracked("feature/deferred")),
        "Deferred must not publish a completed branch receipt"
    );

    drop(sync_lock);
    std::fs::write(
        worktree.join("deferred_retry.rs"),
        "pub fn deferred_retry_symbol() {}\n",
    )
    .unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "deferred retry"]);

    assert_eq!(
        owner
            .track_worktree_branch(&worktree, "feature/deferred")
            .await
            .expect("retry tracking"),
        crate::branch::BranchAddOutcome::Added
    );
    let meta =
        crate::branch_meta::load_branch_meta(&data_root).expect("retry publishes branch metadata");
    let database = data_root.join(&meta.branches["feature/deferred"].db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'deferred_retry_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn linked_head_advance_syncs_through_the_retained_owner_graph() {
    let (repo, _pin, profile_root) = isolated_owner_repo();
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "linked owner graph test",
    )
    .expect("fixture lifecycle authority");
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "linked owner graph test",
    )
    .expect("fixture database authority");
    let owner = TraceDecay::init_with_exclusive_maintenance(
        repo.path(),
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: None,
        },
        &lifecycle,
    )
    .await
    .expect("initialize retained owner graph");
    owner.index_all().await.expect("index owner graph");
    let linked_parent = tempfile::tempdir().unwrap();
    let worktree = linked_parent.path().join("owner-worktree");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/owner-refresh",
            worktree.to_str().expect("linked path"),
        ],
    );
    assert_eq!(
        owner
            .track_worktree_branch(&worktree, "feature/owner-refresh")
            .await
            .expect("initial worktree tracking"),
        crate::branch::BranchAddOutcome::Added
    );

    std::fs::write(
        worktree.join("owner_refresh.rs"),
        "pub fn owner_refresh_symbol() {}\n",
    )
    .unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "owner refresh"]);

    assert_eq!(
        owner
            .track_worktree_branch(&worktree, "feature/owner-refresh")
            .await
            .expect("owner-graph catch-up"),
        crate::branch::BranchAddOutcome::AlreadyTracked
    );
    let data_root = owner.store_layout().data_root.clone();
    let meta = crate::branch_meta::load_branch_meta(&data_root).expect("linked branch metadata");
    let database = data_root.join(&meta.branches["feature/owner-refresh"].db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'owner_refresh_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}
