use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;

#[tokio::test]
async fn retryable_worker_settlement_does_not_skip_independent_shutdown() {
    let (cg, _project, _authority) = writer_test_support::init_indexed_repo().await;
    let server = McpServer::new(cg, None).await;
    let control = server
        .request_registry
        .admit(
            "shutdown-independent-cleanup",
            "tracedecay_search",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_millis(150), true),
        )
        .expect("request admission");
    let reservation = control
        .reserve_join_required_worker(McpToolDispatchStage::Handler)
        .expect("worker reservation");
    let released = Arc::new(tokio::sync::Notify::new());
    let worker_release = Arc::clone(&released);
    let worker = tokio::spawn(async move {
        worker_release.notified().await;
        Ok::<(), TraceDecayError>(())
    });
    control
        .run_owned_join_required(McpToolDispatchStage::Handler, reservation, worker)
        .await
        .expect_err("worker must transfer to settlement");

    assert!(matches!(
        server.shutdown_with_worker_status().await,
        McpWorkerReaperShutdown::Retryable { pending: 1 }
    ));
    assert!(
        server.shutdown_done.load(Ordering::Acquire),
        "independent persistence and background cleanup must still complete"
    );

    released.notify_one();
    assert!(matches!(
        server.shutdown_with_worker_status().await,
        McpWorkerReaperShutdown::Complete { .. }
    ));
}
