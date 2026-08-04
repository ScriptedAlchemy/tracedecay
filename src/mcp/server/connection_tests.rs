use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;

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

#[tokio::test]
async fn readiness_deadline_does_not_reacquire_the_graph_for_failure_completion() {
    let (cg, _project, _authority) = writer_test_support::init_indexed_repo().await;
    let server = McpServer::new(cg, None).await;
    let connection = server
        .new_connection_route_state()
        .expect("connection route state");
    let started = McpRequestStart::now();
    let control = server
        .request_registry
        .admit(
            "readiness-deadline",
            "tracedecay_search",
            started,
            McpToolLifecyclePolicy::new(Duration::from_millis(50), true),
        )
        .expect("request admission");
    let params = json!({
        "name": "tracedecay_search",
        "arguments": {"query": "deadline"}
    });
    let graph_write = server.cg.write().await;

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        server.handle_tools_call(
            json!(19),
            Some(&params),
            false,
            &connection.route_cache,
            None,
            connection.memory_request_scope(),
            Some(control),
            started,
        ),
    )
    .await
    .expect("readiness failure completion must not wait for the graph");
    let data = response
        .error
        .and_then(|error| error.data)
        .expect("typed readiness deadline");
    assert_eq!(data["reason_code"], "tool_dispatch_deadline_exceeded");
    assert_eq!(
        data["tracedecay/execution_receipt"]["terminal"],
        "deadline_exceeded"
    );
    drop(graph_write);
    server.shutdown().await;
}

#[tokio::test]
async fn native_project_health_retirement_writes_its_canonical_receipt() {
    let (cg, _project, _authority) = writer_test_support::init_indexed_repo().await;
    let route_live = Arc::new(AtomicBool::new(true));
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_project_server_live(Arc::clone(&route_live)),
    )
    .await;
    let graph_write = server.cg.write().await;
    let (mut transport, input, mut output) = crate::mcp::transport::ChannelTransport::new();
    input
        .send(
            json!({
                "jsonrpc": "2.0",
                "id": 27,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_search",
                    "arguments": {"query": "retirement"}
                }
            })
            .to_string(),
        )
        .expect("tool request");
    drop(input);
    let connection_server = Arc::clone(&server);
    let connection =
        tokio::spawn(async move { connection_server.run_connection(&mut transport).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if server
                .project_server_lifecycle
                .response_gate()
                .try_write()
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("project request admitted");

    route_live.store(false, Ordering::Release);
    server.revoke_project_server_responses();
    drop(graph_write);

    let line = tokio::time::timeout(Duration::from_secs(1), output.recv())
        .await
        .expect("retirement response deadline")
        .expect("retirement response");
    let response: JsonRpcResponse =
        serde_json::from_str(line.trim()).expect("retirement JSON-RPC response");
    let data = response
        .error
        .and_then(|error| error.data)
        .expect("typed retirement error");
    assert_eq!(data["reason_code"], "project_server_health_revoked");
    assert_eq!(
        data["tracedecay/execution_receipt"]["terminal"],
        "unavailable"
    );
    connection
        .await
        .expect("connection task")
        .expect("connection completion");
    server.shutdown().await;
}
