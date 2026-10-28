//! hook-boundary failure matrix at the daemon host-admission spool.
//!
//! Each row proves typed failure dispositions do not corrupt the durable
//! `reconcile_sink` frontier (pending watermark / replay backlog) and do not invent a
//! default-success commit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::writer_test_support::{
    WriterTestFixtureAuthority, counting_reconcile_sink, default_spool_broker, init_indexed_repo,
    registered_context, server_with_broker, session_start,
};
use super::{CodeIndexReconcileSink, McpServer};
use crate::mcp::project_route::HookProjectRouteCache;
use tracedecay_host_admission::{HostAdmissionBroker, HostAdmissionRuntime, SpoolBounds};
use tracedecay_sessions::admission::HostAdmissionStatus;

async fn server_without_broker(
    cg: tracedecay_project::project::TraceDecay,
    authority: &WriterTestFixtureAuthority,
    reconcile_sink: CodeIndexReconcileSink,
) -> Arc<McpServer> {
    let context = registered_context(cg, authority).with_code_index_reconcile_sink(reconcile_sink);
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

fn failing_reconcile_sink() -> CodeIndexReconcileSink {
    Arc::new(|_request: PathBuf, _demand| {
        Box::pin(async {
            super::CodeIndexDemandAdmissionV1::Unavailable(
                tracedecay_code_index_runtime::code_index_scheduler::CodeIndexDemandUnavailableV1::SchedulerUnmounted,
            )
        })
    })
}

#[tokio::test]
async fn matrix_identical_notifications_are_distinct_without_frontier_corruption() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = server_with_broker(
        cg,
        &authority,
        Arc::clone(&broker),
        counting_reconcile_sink(Arc::clone(&attempts)),
    )
    .await;
    let mut routes = HookProjectRouteCache::default();
    let event = session_start(project.path().to_path_buf());

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    let second = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;

    // Host delivery attempts remain distinct durable admissions even when the
    // encoded payload bytes match. ExactDuplicate is reserved for an
    // idempotent production effect, not for spool-level payload coalescing.
    assert_eq!(first.status, HostAdmissionStatus::Committed);
    assert_eq!(second.status, HostAdmissionStatus::Committed);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_daemon_unavailable_without_broker_skips_reconcile_and_frontier() {
    let (cg, project, authority) = init_indexed_repo().await;
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_without_broker(cg, &authority, reconcile_sink).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("spool_unavailable"));
    assert_eq!(
        attempted.load(Ordering::SeqCst),
        0,
        "unavailable daemon path must not open a local canonical reconcile_sink"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_backpressure_overflow_rejects_before_reconcile_without_pending_growth() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    // One durable slot: first failing admit retains pending=1; second overflows.
    let runtime = HostAdmissionRuntime::open(
        spool.path(),
        SpoolBounds::new(64 * 1024, 1024, 64 * 1024, 1),
    )
    .unwrap()
    .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(Mutex::new(0usize));
    let reconcile_sink: CodeIndexReconcileSink = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request: PathBuf, _demand| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                *attempted.lock().unwrap() += 1;
                super::CodeIndexDemandAdmissionV1::Unavailable(
                    tracedecay_code_index_runtime::code_index_scheduler::CodeIndexDemandUnavailableV1::SchedulerUnmounted,
                )
            })
        })
    };
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let mut routes = HookProjectRouteCache::default();
    let event = session_start(project.path().to_path_buf());

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert_eq!(first.status, HostAdmissionStatus::Unavailable);
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(*attempted.lock().unwrap(), 1);

    let second = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert_eq!(second.status, HostAdmissionStatus::Backpressured);
    assert_eq!(second.reason_code, Some("spool_overflow"));
    assert_eq!(
        broker.pending_count().await,
        1,
        "backpressure must not grow or shrink the retained frontier"
    );
    assert_eq!(
        *attempted.lock().unwrap(),
        1,
        "overflow must reject before the canonical reconcile_sink"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_unavailable_then_success_keeps_sticky_retained_failure_frontier() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let server = server_with_broker(
        cg,
        &authority,
        Arc::clone(&broker),
        failing_reconcile_sink(),
    )
    .await;
    let mut routes = HookProjectRouteCache::default();

    let failed = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;
    assert_eq!(failed.status, HostAdmissionStatus::Unavailable);
    assert_eq!(broker.pending_count().await, 1);

    // Later canonical success against a different in-memory reconcile_sink must still
    // see the retained frame; failure is sticky in the durable frontier.
    server.shutdown().await;
    drop(server);
    drop(broker);

    let broker = default_spool_broker(spool.path());
    let attempts = Arc::new(AtomicUsize::new(0));
    let reopened_cg = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(
        reopened_cg,
        &authority,
        Arc::clone(&broker),
        counting_reconcile_sink(Arc::clone(&attempts)),
    )
    .await;
    Box::pin(server.replay_host_admission(None)).await;
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}
