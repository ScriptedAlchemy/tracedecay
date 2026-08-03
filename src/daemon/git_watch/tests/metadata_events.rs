use std::sync::atomic::Ordering;
use std::time::Duration;

use notify::EventKind;
use notify::event::EventAttributes;

use super::*;

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
    let Some(state) = ensure_watching_or_skip(&watcher, repo.path()).await else {
        return;
    };

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
async fn gc_retry_timer_preserves_gc_only_work_without_backstop() {
    let state = test_watch_state("/repo");
    state
        .schedule_retry(DirtyPlan::gc(), Duration::from_secs(1))
        .await;

    tokio::time::advance(Duration::from_secs(1)).await;
    let plan = state
        .take_due_retry()
        .await
        .expect("GC retry must survive until the timer fires");
    assert!(plan.gc_eligible);
    assert!(!plan.dirty && plan.branches.is_empty());
}
