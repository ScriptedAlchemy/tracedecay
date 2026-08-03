use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::mcp::transport::McpTransport;

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

#[tokio::test(start_paused = true)]
async fn response_write_reserve_puts_typed_deadline_on_the_wire() {
    let registry = McpRequestRegistry::new();
    let started = McpRequestStart::now();
    let control = registry
        .admit(
            "scope:response-wire",
            "tracedecay_search",
            started,
            McpToolLifecyclePolicy::new(Duration::from_millis(100), true),
        )
        .expect("request admission");
    let handler_control = control.clone();
    let handler = tokio::spawn(async move {
        handler_control
            .run_value(McpToolDispatchStage::Handler, std::future::pending::<()>())
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(90)).await;
    let error = handler
        .await
        .expect("handler task")
        .expect_err("execution reserve must end the handler");
    let response = crate::mcp::server::request_receipts::finish_tool_error_response(
        serde_json::json!(11),
        "tracedecay_search",
        &error,
        started,
        Some(&control),
    );
    let line = format!(
        "{}\n",
        serde_json::to_string(&response).expect("serialize response")
    );
    let (mut transport, _input, mut output) = crate::mcp::transport::ChannelTransport::new();
    control
        .run(McpToolDispatchStage::ResponseWrite, async {
            transport.write_line(&line).await?;
            transport.flush().await?;
            Ok::<_, crate::errors::TraceDecayError>(())
        })
        .await
        .expect("response reserve must remain available");

    let wire = output.recv().await.expect("wire response");
    let response: crate::mcp::transport::JsonRpcResponse =
        serde_json::from_str(wire.trim()).expect("valid wire response");
    let data = response
        .error
        .and_then(|error| error.data)
        .expect("typed error data");
    assert_eq!(data["reason_code"], "tool_dispatch_deadline_exceeded");
    assert_eq!(
        data["tracedecay/execution_receipt"]["terminal"],
        "deadline_exceeded"
    );
    assert!(started.elapsed() < Duration::from_millis(100));
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
fn retained_cancellation_does_not_apply_to_non_cancellable_policy() {
    let registry = McpRequestRegistry::new();
    assert!(registry.cancel_or_retain("scope:non-cancellable"));

    let control = registry
        .admit(
            "scope:non-cancellable",
            "tracedecay_outline",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_secs(1), false),
        )
        .expect("request admission");

    assert!(!control.is_cancelled());
    assert!(!registry.cancel_or_retain("scope:non-cancellable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_admission_race_obeys_each_policy() {
    for externally_cancellable in [false, true] {
        for iteration in 0..64 {
            let registry = McpRequestRegistry::new();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let request_key = format!("scope:race:{externally_cancellable}:{iteration}");
            let cancel_registry = registry.clone();
            let cancel_key = request_key.clone();
            let cancel_barrier = Arc::clone(&barrier);
            let cancellation = tokio::spawn(async move {
                cancel_barrier.wait().await;
                cancel_registry.cancel_or_retain(&cancel_key)
            });
            barrier.wait().await;
            let control = registry
                .admit(
                    &request_key,
                    "tracedecay_search",
                    McpRequestStart::now(),
                    McpToolLifecyclePolicy::new(Duration::from_secs(1), externally_cancellable),
                )
                .expect("request admission");
            let _ = cancellation.await.expect("cancellation task");
            assert_eq!(
                control.is_cancelled(),
                externally_cancellable,
                "iteration {iteration}"
            );
        }
    }
}

#[test]
fn duplicate_active_request_id_is_rejected_without_replacing_the_owner() {
    let registry = McpRequestRegistry::new();
    let policy = McpToolLifecyclePolicy::new(Duration::from_secs(1), true);
    let first = registry
        .admit(
            "scope:duplicate",
            "tracedecay_search",
            McpRequestStart::now(),
            policy,
        )
        .expect("first request admission");
    let error = match registry.admit(
        "scope:duplicate",
        "tracedecay_context",
        McpRequestStart::now(),
        policy,
    ) {
        Ok(_) => panic!("duplicate active identity must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.mcp_tool_dispatch_context(),
        Some((
            "tool_dispatch_duplicate_request_id",
            "queue_admission",
            false,
            "tool 'tracedecay_context' reused request id 'scope:duplicate' while its prior request is active",
        ))
    );
    assert!(registry.cancel_or_retain("scope:duplicate"));
    assert!(first.is_cancelled());
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
    let next_control = registry
        .admit(
            "scope:worker-after-drain",
            "tracedecay_search",
            McpRequestStart::now(),
            McpToolLifecyclePolicy::new(Duration::from_secs(5), true),
        )
        .expect("second request admission");
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
    let error = match next_control.reserve_join_required_worker(McpToolDispatchStage::Handler) {
        Ok(_) => panic!("draining reaper must reject new reservations"),
        Err(error) => error,
    };
    assert_eq!(
        error.mcp_tool_dispatch_context().map(|context| context.0),
        Some("tool_dispatch_reaper_unavailable")
    );

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
