use super::writer_test_support::init_indexed_repo;
use super::{
    BackgroundRefreshRequest, BackgroundRefreshWriter, McpServer, McpServerConstructionContext,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn admitted_tool_call_does_not_start_a_legacy_refresh() {
    let (cg, dir, authority) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    drop(cg);
    let cg = authority.reopen_project_graph(&root).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let refresh_writer: BackgroundRefreshWriter = {
        let calls = Arc::clone(&calls);
        Arc::new(move |_request: BackgroundRefreshRequest| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(Some(HashMap::new()))
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
        "startup catch-up settles before request admission"
    );
    calls.store(0, Ordering::Release);
    let snapshot = server.cg_snapshot().await;

    server
        .begin_tool_dispatch("tracedecay_search", &snapshot, false)
        .await;
    server
        .begin_tool_dispatch("tracedecay_str_replace", &snapshot, false)
        .await;

    tokio::task::yield_now().await;
    assert_eq!(
        calls.load(Ordering::Acquire),
        0,
        "read and edit admission must serve the sealed generation without opening legacy refresh"
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
