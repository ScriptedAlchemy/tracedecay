//! PR6 hook-boundary failure matrix at the daemon host-admission spool.
//!
//! Each row proves typed failure dispositions do not corrupt the durable
//! writer frontier (pending watermark / replay backlog) and do not invent a
//! default-success commit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use super::writer_test_support::{init_indexed_repo, registered_context};
use super::{HookBranchWriteRequest, HookBranchWriteResult, HookBranchWriter, McpServer};
use crate::application::host_admission::{
    HostAdmissionBroker, HostAdmissionRuntime, HostAdmissionStatus, SharedHostAdmissionBroker,
    SpoolBounds,
};
use crate::daemon::{DaemonHookEvent, HookAgent};
use crate::errors::TraceDecayError;
use crate::mcp::project_route::HookProjectRouteCache;

fn session_start(root: PathBuf) -> Value {
    serde_json::to_value(DaemonHookEvent::session_start(HookAgent::Codex, root)).unwrap()
}

async fn server_with_broker(
    cg: crate::tracedecay::TraceDecay,
    broker: SharedHostAdmissionBroker,
    writer: HookBranchWriter,
) -> Arc<McpServer> {
    let mut context = registered_context(cg).await.with_hook_branch_writer(writer);
    context.host_admission_broker = Some(broker);
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

async fn server_without_broker(
    cg: crate::tracedecay::TraceDecay,
    writer: HookBranchWriter,
) -> Arc<McpServer> {
    let context = registered_context(cg).await.with_hook_branch_writer(writer);
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

fn failing_writer(message: &'static str) -> HookBranchWriter {
    Arc::new(move |_request: HookBranchWriteRequest| {
        Box::pin(async move {
            Err(TraceDecayError::Config {
                message: message.to_string(),
            })
        })
    })
}

fn counting_success_writer(writes: Arc<Mutex<usize>>) -> HookBranchWriter {
    Arc::new(move |_request: HookBranchWriteRequest| {
        let writes = Arc::clone(&writes);
        Box::pin(async move {
            *writes.lock().unwrap() += 1;
            Ok(HookBranchWriteResult {
                branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                refresh_file_token_map: false,
            })
        })
    })
}

#[tokio::test]
async fn matrix_duplicate_is_exact_duplicate_without_frontier_corruption() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writes = Arc::new(Mutex::new(0usize));
    let server = server_with_broker(
        cg,
        Arc::clone(&broker),
        counting_success_writer(Arc::clone(&writes)),
    )
    .await;
    let mut routes = HookProjectRouteCache::default();
    let event = session_start(project.path().to_path_buf());

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    let second = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;

    assert!(matches!(
        first.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
    ));
    assert_eq!(second.status, HostAdmissionStatus::ExactDuplicate);
    assert_eq!(*writes.lock().unwrap(), 2);
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_reordered_completion_waits_for_contiguous_frontier() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let server = server_with_broker(
        cg,
        Arc::clone(&broker),
        failing_writer("injected retain for reorder"),
    )
    .await;
    let mut routes = HookProjectRouteCache::default();

    Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;
    Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    let replay = broker.begin_replay().await.unwrap();
    let first = replay.lease_next().await.unwrap().unwrap();
    let second = replay.lease_next().await.unwrap().unwrap();
    assert_ne!(first.seq, second.seq);

    assert_eq!(replay.commit(second.seq).await.unwrap(), 0);
    assert_eq!(broker.pending_count().await, 2);
    assert_eq!(replay.commit(first.seq).await.unwrap(), 2);
    assert_eq!(broker.pending_count().await, 0);
    drop(replay);
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_timeout_cancels_in_flight_replay_and_preserves_durable_frontier() {
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let admitted = broker.admit("cancel-test", b"durable-event").await.unwrap();
    let lease_observed = Arc::new(AtomicBool::new(false));
    let replay_broker = Arc::clone(&broker);
    let replay_lease_observed = Arc::clone(&lease_observed);
    let cancelled = tokio::time::timeout(Duration::from_millis(100), async move {
        let replay = replay_broker.begin_replay().await.unwrap();
        let leased = replay
            .lease_next()
            .await
            .expect("lease operation must succeed")
            .expect("admitted frame must be leased");
        assert_eq!(leased.seq, admitted.seq);
        replay_lease_observed.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
        drop(replay);
    })
    .await;

    assert!(cancelled.is_err(), "in-flight replay must time out");
    assert!(
        lease_observed.load(Ordering::SeqCst),
        "timeout must occur after replay lease"
    );
    assert_eq!(broker.pending_count().await, 1);

    // Dropping the timed-out future also drops the replay guard. The next
    // replay recovers the abandoned lease and can lease the same durable frame.
    let replay = broker.begin_replay().await.unwrap();
    let recovered = replay
        .lease_next()
        .await
        .expect("lease operation must succeed")
        .expect("cancelled lease must remain replayable");
    assert_eq!(broker.pending_count().await, 1);
    replay.defer(recovered.seq).await.unwrap();
    drop(replay);

    drop(broker);

    // Persistence, not only in-memory queue state: reopen retains the frontier.
    let reopened = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let reopened = HostAdmissionBroker::new(reopened);
    assert_eq!(reopened.pending_count().await, 1);
}

#[tokio::test]
async fn matrix_daemon_unavailable_without_broker_skips_writer_and_frontier() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let attempted = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request: HookBranchWriteRequest| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                *attempted.lock().unwrap() = true;
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_without_broker(cg, writer).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("spool_unavailable"));
    assert!(
        !*attempted.lock().unwrap(),
        "unavailable daemon path must not open a local canonical writer"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_backpressure_overflow_rejects_before_writer_without_pending_growth() {
    let (cg, project, _pin) = init_indexed_repo().await;
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
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request: HookBranchWriteRequest| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                *attempted.lock().unwrap() += 1;
                Err(TraceDecayError::Config {
                    message: "retain first frame for overflow".to_string(),
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
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
        "overflow must reject before the canonical writer"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn matrix_unavailable_then_success_keeps_sticky_retained_failure_frontier() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let server = server_with_broker(
        cg,
        Arc::clone(&broker),
        failing_writer("injected unavailable"),
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

    // Later canonical success against a different in-memory writer must still
    // see the retained frame; failure is sticky in the durable frontier.
    server.shutdown().await;
    drop(server);
    drop(broker);

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writes = Arc::new(Mutex::new(0usize));
    let reopened_cg = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(
        reopened_cg,
        Arc::clone(&broker),
        counting_success_writer(Arc::clone(&writes)),
    )
    .await;
    Box::pin(server.replay_host_admission(None)).await;
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(*writes.lock().unwrap(), 1);
    server.shutdown().await;
}

/// Item-2 wiring: an after-edit host hook must deliver its touched paths into
/// the injected code-index scheduler bridge (the queue entry point), so the
/// incremental indexer reconciles the edit without any standing filesystem
/// watcher. Proves the `requests.rs` hook boundary invokes the sink built in
/// `daemon.rs` from the daemon-owned scheduler registry.
#[tokio::test]
async fn after_edit_hook_delivers_touched_paths_to_code_index_sink() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let expected_root = cg.project_root().to_path_buf();
    type RecordedDeliveries = Arc<Mutex<Vec<(PathBuf, Vec<String>)>>>;
    let recorded: RecordedDeliveries = Arc::new(Mutex::new(Vec::new()));
    let sink_recorded = Arc::clone(&recorded);
    let sink: super::CodeIndexHookSink = Arc::new(move |root: PathBuf, rel_paths: Vec<String>| {
        let sink_recorded = Arc::clone(&sink_recorded);
        Box::pin(async move {
            sink_recorded.lock().unwrap().push((root, rel_paths));
            // Report "delivered": a mounted worktree accepted the paths.
            true
        })
    });
    let context = registered_context(cg).await.with_code_index_hook_sink(sink);
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");
    let mut routes = HookProjectRouteCache::default();
    let event = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
        HookAgent::Codex,
        vec!["src/lib.rs".to_owned()],
        project.path().to_path_buf(),
    ))
    .unwrap();

    Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;

    {
        let recorded = recorded.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "after-edit hook must reach the code-index scheduler bridge exactly once"
        );
        let (root, rel_paths) = &recorded[0];
        assert_eq!(
            root, &expected_root,
            "sink must receive the served project root"
        );
        assert_eq!(
            rel_paths,
            &vec!["src/lib.rs".to_owned()],
            "sink must receive the exact touched rel paths carried by the hook"
        );
    }
    server.shutdown().await;
}
