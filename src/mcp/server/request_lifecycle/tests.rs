use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    MAX_ACTIVE_REQUESTS, MAX_PENDING_CANCELLATIONS, McpRequestRegistry, McpRequestStart,
    McpToolDispatchStage, McpToolLifecyclePolicy, McpToolWorkerSettlement, McpWorkerReaperShutdown,
};

#[tokio::test(start_paused = true)]
async fn queue_time_consumes_the_absolute_request_deadline() {
    let registry = McpRequestRegistry::new();
    let enqueued = McpRequestStart::now();
    tokio::time::advance(Duration::from_millis(80)).await;
    let control = registry
        .admit(
            "scope:1",
            "tracedecay_search",
            enqueued,
            McpToolLifecyclePolicy::new(Duration::from_millis(100), true),
        )
        .expect("request admission");

    let dispatch = tokio::spawn(async move {
        control
            .run_value(McpToolDispatchStage::ProjectSelection, async {
                std::future::pending::<()>().await
            })
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(20)).await;

    let error = dispatch
        .await
        .expect("dispatch task")
        .expect_err("queue time must leave only twenty milliseconds");
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_deadline_exceeded")
    );
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.1),
        Some("project_selection")
    );
}

#[test]
fn cancellation_before_registration_is_retained_for_native_dispatch() {
    let registry = McpRequestRegistry::new();
    assert!(registry.cancel_or_retain("scope:7"));

    let control = registry
        .admit(
            "scope:7",
            "tracedecay_search",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_secs(1), true),
        )
        .expect("request admission");

    assert!(control.is_cancelled());
}

#[test]
fn orphaned_cancellation_retention_is_bounded() {
    let registry = McpRequestRegistry::new();
    for id in 0..MAX_PENDING_CANCELLATIONS.saturating_mul(2) {
        assert!(registry.cancel_or_retain(&format!("scope:{id}")));
    }
    assert_eq!(
        registry.retained_cancellation_count(),
        MAX_PENDING_CANCELLATIONS
    );
}

#[test]
fn saturated_request_queue_is_a_typed_retryable_admission_failure() {
    let registry = McpRequestRegistry::new();
    let policy = McpToolLifecyclePolicy::new(Duration::from_secs(1), true);
    let admitted: Vec<_> = (0..MAX_ACTIVE_REQUESTS)
        .map(|id| {
            registry
                .admit(
                    &format!("scope:queue:{id}"),
                    "tracedecay_search",
                    McpRequestStart::now(),
                    policy,
                )
                .expect("request within queue capacity")
        })
        .collect();

    let error = match registry.admit(
        "scope:queue:overflow",
        "tracedecay_search",
        McpRequestStart::now(),
        policy,
    ) {
        Ok(_) => panic!("request beyond queue capacity must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_queue_saturated")
    );
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.2),
        Some(true)
    );
    drop(admitted);
}

#[tokio::test]
async fn server_shutdown_is_a_distinct_retryable_terminal_outcome() {
    let registry = McpRequestRegistry::new();
    let control = registry
        .admit(
            "scope:shutdown",
            "tracedecay_search",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_secs(1), true),
        )
        .expect("request admission");
    assert_eq!(
        control.worker_receipt_snapshot().0,
        McpToolWorkerSettlement::NotStarted
    );
    assert_eq!(registry.cancel_all_live(), 1);

    let error = control
        .run_value(McpToolDispatchStage::Handler, std::future::pending::<()>())
        .await
        .expect_err("shutdown must terminate the request");
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_shutdown")
    );
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.2),
        Some(true)
    );
}

#[tokio::test]
async fn uncooperative_worker_cleanup_is_bounded_and_shutdown_is_retryable() {
    let registry = McpRequestRegistry::new();
    let control = registry
        .admit(
            "scope:worker",
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
        Ok::<(), crate::errors::TraceDecayError>(())
    });

    let error = control
        .run_owned_join_required(McpToolDispatchStage::Handler, reservation, worker)
        .await
        .expect_err("worker must miss bounded cleanup");
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_deadline_exceeded")
    );
    assert_eq!(
        control.worker_receipt_snapshot().0,
        McpToolWorkerSettlement::Indeterminate
    );
    assert!(matches!(
        registry.shutdown_workers(Duration::from_millis(10)).await,
        McpWorkerReaperShutdown::Retryable { pending: 1 }
    ));

    released.notify_one();
    tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            if matches!(
                registry.shutdown_workers(Duration::from_millis(10)).await,
                McpWorkerReaperShutdown::Complete { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retryable shutdown must eventually complete");
}

#[tokio::test]
async fn cooperative_worker_joins_before_cancelled_response() {
    let registry = McpRequestRegistry::new();
    let control = registry
        .admit(
            "scope:cooperative",
            "tracedecay_search",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_secs(1), true),
        )
        .expect("request admission");
    let reservation = control
        .reserve_join_required_worker(McpToolDispatchStage::Handler)
        .expect("worker reservation");
    let cleaned = Arc::new(AtomicBool::new(false));
    let worker_cleaned = Arc::clone(&cleaned);
    let cancellation = control.cancellation();
    let worker = tokio::spawn(async move {
        crate::daemon_client::wait_for_cancellation(cancellation).await;
        worker_cleaned.store(true, Ordering::Release);
        Ok::<(), crate::errors::TraceDecayError>(())
    });
    assert!(registry.cancel_or_retain("scope:cooperative"));

    let error = control
        .run_owned_join_required(McpToolDispatchStage::Handler, reservation, worker)
        .await
        .expect_err("cancelled request");
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_cancelled")
    );
    assert!(cleaned.load(Ordering::Acquire));
    assert_eq!(
        control.worker_receipt_snapshot().0,
        McpToolWorkerSettlement::Joined
    );
}
