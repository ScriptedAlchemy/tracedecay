use super::writer_test_support::init_indexed_repo;
use super::{
    BackgroundRefreshRequest, BackgroundRefreshWriter, McpServer, McpServerConstructionContext,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn read_refresh_uses_injected_writer_without_direct_fallback() {
    let (cg, dir, authority) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    // `init_indexed_repo` persists `session_start_sync = false`, but the handle
    // it returns still carries the init-time config snapshot (default true).
    // Re-open the project so the constructed server honors the persisted
    // setting and does not spawn a startup catch-up that would also drive the
    // injected writer, leaving this test's explicit read refresh as the only
    // observed call.
    drop(cg);
    let cg = authority.reopen_project_graph(&root).await;
    let source_path = root.join("src/a.rs");
    std::fs::write(&source_path, "pub fn a() { println!(\"changed\"); }\n").expect("modify source");
    std::fs::File::options()
        .write(true)
        .open(&source_path)
        .expect("open modified source")
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .expect("advance source mtime");
    assert!(
        cg.find_stale_files()
            .await
            .iter()
            .any(|path| path == "src/a.rs"),
        "fixture must be stale before refresh"
    );

    let observed = Arc::new(Mutex::new(Vec::<(PathBuf, usize)>::new()));
    let refresh_writer: BackgroundRefreshWriter = {
        let observed = Arc::clone(&observed);
        Arc::new(move |request: BackgroundRefreshRequest| {
            let observed = Arc::clone(&observed);
            Box::pin(async move {
                observed
                    .lock()
                    .expect("recording lock")
                    .push((request.project_root, request.full_sync_escalation_files));
                Ok(Some(HashMap::from([("injected.rs".to_string(), 41)])))
            })
        })
    };
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_background_refresh_writer(refresh_writer),
    )
    .await;
    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(5))
            .await,
        "startup catch-up settles before the explicit read refresh"
    );
    observed.lock().expect("recording lock").clear();
    let snapshot = server.cg_snapshot().await;
    server
        .background_refresh_running
        .store(true, Ordering::Release);

    server.spawn_read_refresh_task(&snapshot, 17);

    tokio::time::timeout(Duration::from_secs(5), async {
        while server.background_refresh_running.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("injected refresh settles");

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[(root, 17)]
    );
    assert_eq!(
        server.file_token_map_snapshot(),
        HashMap::from([("injected.rs".to_string(), 41)])
    );
    assert!(
        snapshot
            .find_stale_files()
            .await
            .iter()
            .any(|path| path == "src/a.rs"),
        "injected refresh must not execute the direct open/sync fallback"
    );
    assert_ne!(
        server
            .last_background_refresh_done_at
            .load(Ordering::Acquire),
        0,
        "completion timestamp must be preserved"
    );
    server.shutdown().await;
}

/// Edit-shaped tools claim the lazy-sync window but never wait on it: the tool
/// answers while the sync is still running, and the sync completes behind it.
#[tokio::test]
async fn lazy_stale_sync_is_detached_from_the_request() {
    let (cg, dir, authority) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    drop(cg);
    let cg = authority.reopen_project_graph(&root).await;

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    // Armed only after startup catch-up has settled, so the startup sync (which
    // drives the same injected writer) is not the call this test parks.
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refresh_writer: BackgroundRefreshWriter = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let completed = Arc::clone(&completed);
        let armed = Arc::clone(&armed);
        Arc::new(move |_request: BackgroundRefreshRequest| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let completed = Arc::clone(&completed);
            let armed = Arc::clone(&armed);
            Box::pin(async move {
                if !armed.load(Ordering::Acquire) {
                    return Ok(Some(HashMap::new()));
                }
                entered.notify_one();
                release.notified().await;
                completed.fetch_add(1, Ordering::AcqRel);
                Ok(Some(HashMap::from([("detached.rs".to_string(), 7)])))
            })
        })
    };
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_background_refresh_writer(refresh_writer),
    )
    .await;
    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(5))
            .await,
        "startup catch-up settles before the lazy sync claim"
    );
    armed.store(true, Ordering::Release);
    // Re-arm the cooldown so this call is the one that claims the window.
    server.last_staleness_check_at.store(0, Ordering::Release);
    server
        .background_refresh_running
        .store(false, Ordering::Release);

    // The request path. It must return while the injected sync is parked.
    tokio::time::timeout(Duration::from_secs(5), server.maybe_sync_if_stale())
        .await
        .expect("maybe_sync_if_stale must not block on the sync");
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the detached sync must have started");
    assert_eq!(
        completed.load(Ordering::Acquire),
        0,
        "the tool answered before the sync finished"
    );

    // ...and the sync still completes, refreshing the token map.
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.background_refresh_running.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the detached sync settles");
    assert_eq!(completed.load(Ordering::Acquire), 1);
    assert_eq!(
        server.file_token_map_snapshot(),
        HashMap::from([("detached.rs".to_string(), 7)])
    );
    server.shutdown().await;
}

#[tokio::test]
async fn startup_catchup_uses_configured_full_sync_escalation() {
    let (cg, _dir, _authority) = init_indexed_repo().await;
    let configured_escalation = cg.get_config().sync.full_sync_escalation_files;
    assert_ne!(
        configured_escalation, 0,
        "production startup catch-up must retain commit-diff scoping"
    );

    let observed = Arc::new(Mutex::new(Vec::<usize>::new()));
    let refresh_writer: BackgroundRefreshWriter = {
        let observed = Arc::clone(&observed);
        Arc::new(move |request: BackgroundRefreshRequest| {
            observed
                .lock()
                .expect("recording lock")
                .push(request.full_sync_escalation_files);
            Box::pin(async { Ok(Some(HashMap::new())) })
        })
    };
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_background_refresh_writer(refresh_writer),
    )
    .await;

    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(5))
            .await,
        "startup catch-up must settle"
    );
    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[configured_escalation]
    );
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_startup_catchups_use_injected_writer_authority() {
    let (first_cg, dir, authority) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let mut config = crate::config::load_config(&root).expect("load config");
    config.sync.session_start_sync = true;
    crate::config::save_config(&root, &config).expect("enable startup sync");
    let second_cg = authority.reopen_project_graph(&root).await;

    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let calls = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let refresh_writer: BackgroundRefreshWriter = {
        let gate = Arc::clone(&gate);
        let calls = Arc::clone(&calls);
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        Arc::new(move |_request: BackgroundRefreshRequest| {
            let gate = Arc::clone(&gate);
            let calls = Arc::clone(&calls);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            Box::pin(async move {
                let _authority = gate.lock().await;
                calls.fetch_add(1, Ordering::AcqRel);
                let concurrent = active.fetch_add(1, Ordering::AcqRel) + 1;
                max_active.fetch_max(concurrent, Ordering::AcqRel);
                tokio::time::sleep(Duration::from_millis(50)).await;
                active.fetch_sub(1, Ordering::AcqRel);
                Ok(Some(HashMap::new()))
            })
        })
    };

    let (first, second) = tokio::join!(
        McpServer::new_with_context(
            McpServerConstructionContext::direct(first_cg, Some("first".to_string()))
                .with_background_refresh_writer(Arc::clone(&refresh_writer)),
        ),
        McpServer::new_with_context(
            McpServerConstructionContext::direct(second_cg, Some("second".to_string()))
                .with_background_refresh_writer(refresh_writer),
        )
    );
    let (first_done, second_done) = tokio::join!(
        first.wait_for_startup_catch_up(Duration::from_secs(5)),
        second.wait_for_startup_catch_up(Duration::from_secs(5))
    );

    assert!(first_done && second_done, "startup catch-ups must settle");
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert_eq!(
        max_active.load(Ordering::Acquire),
        1,
        "the injected writer authority must serialize concurrent startup catch-ups"
    );
    first.shutdown().await;
    second.shutdown().await;
}
