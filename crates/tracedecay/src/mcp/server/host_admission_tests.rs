use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::{Barrier, Notify};

use super::writer_test_support::{
    WriterTestFixtureAuthority, counting_reconcile_sink, default_spool_broker, init_indexed_repo,
    registered_context, registered_runtime, server_with_broker, session_start,
};
use super::{CodeIndexReconcileSink, McpServer, McpServerConstructionContext};
use crate::mcp::project_route::HookProjectRouteCache;
use crate::test_support::git::git;
use tracedecay_domain::HostIntegrationIdV1;
use tracedecay_hooks::core_events::{DaemonHookEvent, HookRouteMetadata, HookTerminalReceipt};
use tracedecay_host_admission::{
    HostAdmissionBroker, HostAdmissionRuntime, SharedHostAdmissionBroker, SpoolBounds,
};
use tracedecay_private_fs::framed_log::sync_latency;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;
use tracedecay_session_memory::event_lane::{self, ActivityFamilyV1};
use tracedecay_sessions::admission::{
    HostAdmissionOutcome, HostAdmissionScope, HostAdmissionStatus,
};
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionsForQuery,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

async fn code_index_activity(server: &McpServer) -> Vec<event_lane::ActivityRecordV1> {
    let graph = server.cg_snapshot().await;
    let project_id = graph.store_layout().identity.project_id.as_deref().unwrap();
    event_lane::replay_after(
        server.project_session_db.as_deref().unwrap(),
        project_id,
        None,
    )
    .await
    .unwrap()
    .unwrap()
    .records
    .into_iter()
    .filter(|record| record.pulse.family == ActivityFamilyV1::CodeIndex)
    .collect()
}

/// Builds the terminal-receipt wire event the Hermes plugin sends. Production
/// only deserializes these, so the tests own the sender-side construction.
fn hermes_terminal_receipt_event(
    cwd: PathBuf,
    route: HookRouteMetadata,
    receipt: HookTerminalReceipt,
) -> DaemonHookEvent {
    DaemonHookEvent {
        agent: HostIntegrationIdV1::Hermes.as_wire().to_string(),
        event: "terminalReceipt".to_string(),
        rel_paths: Vec::new(),
        command: None,
        cwd: Some(cwd),
        route: Some(route),
        receipt: Some(receipt),
    }
}

fn terminal_receipt(root: PathBuf) -> Value {
    serde_json::to_value(hermes_terminal_receipt_event(
        root.clone(),
        HookRouteMetadata {
            session_id: Some("session-admission-test".to_string()),
            thread_id: None,
            cwd: Some(root),
            worktree: None,
            branch: Some("main".to_string()),
        },
        HookTerminalReceipt {
            tool_call_id: Some("call-admission-test".to_string()),
            turn_id: Some("turn-admission-test".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(1),
            transcript_watermark: Some("message-admission-test".to_string()),
        },
    ))
    .unwrap()
}

async fn server_with_owned_project_replay_worker(
    cg: tracedecay_project::project::TraceDecay,
    authority: &WriterTestFixtureAuthority,
    broker: SharedHostAdmissionBroker,
    reconcile_sink: CodeIndexReconcileSink,
) -> Arc<McpServer> {
    let context = with_broker(registered_context(cg, authority), broker, reconcile_sink)
        .with_owned_project_host_admission_replay();
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

fn with_broker(
    context: McpServerConstructionContext,
    broker: SharedHostAdmissionBroker,
    reconcile_sink: CodeIndexReconcileSink,
) -> McpServerConstructionContext {
    let mut context = context.with_code_index_reconcile_sink(reconcile_sink);
    context.host_admission_broker = Some(broker);
    context
}

fn add_branch_payload() -> Vec<u8> {
    add_branch_payload_for("main")
}

fn add_branch_payload_for(branch: &str) -> Vec<u8> {
    tracedecay_mcp::hook_events::encode_durable_hook_event_plan(
        &tracedecay_mcp::hook_events::HookEventPlan::AddBranch(branch.to_string()),
    )
    .expect("add_branch plan should encode")
}

fn sync_current_branch_payload(branch: &str) -> Vec<u8> {
    tracedecay_mcp::hook_events::encode_durable_hook_event_plan(
        &tracedecay_mcp::hook_events::HookEventPlan::SyncCurrentBranch {
            branch: branch.to_string(),
            agent: HostIntegrationIdV1::Codex,
        },
    )
    .expect("sync_current_branch plan should encode")
}

fn success_reconcile_sink() -> CodeIndexReconcileSink {
    Arc::new(|_root, _demand| Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued }))
}

#[tokio::test]
async fn file_edit_commit_failure_queues_no_sync() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let effects = Arc::new(AtomicUsize::new(0));
    let context = with_broker(
        registered_context(cg, &authority),
        Arc::clone(&broker),
        success_reconcile_sink(),
    )
    .with_code_index_hook_sink({
        let effects = Arc::clone(&effects);
        Arc::new(move |_, _| {
            effects.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued })
        })
    });
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    let metadata = spool.path().join("meta.json");
    std::fs::remove_file(&metadata).unwrap();
    std::fs::create_dir(&metadata).unwrap();
    let event = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
        HostIntegrationIdV1::Claude,
        vec!["src/lib.rs".to_owned()],
        project.path().to_path_buf(),
    ))
    .unwrap();
    let response = server
        .handle_request(&super::JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(17)),
            method: tracedecay_hooks::core_events::HOOK_EVENT_METHOD.to_owned(),
            params: Some(event.clone()),
        })
        .await
        .unwrap();
    assert!(response.result.is_none(), "failed commit must not ACK");
    let failure = response.error.unwrap().data.unwrap();
    assert_eq!(failure["reason_code"], "spool_io_failed");
    assert_eq!(failure["status"], "unavailable");
    let outcome = server
        .handle_hook_event_notification(Some(&event), &mut HookProjectRouteCache::default())
        .await;
    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("spool_recovery_required"));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    assert_eq!(
        code_index_activity(&server).await,
        Vec::<event_lane::ActivityRecordV1>::new()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn file_edit_request_acks_durable_work_while_effect_is_held() {
    let (cg, project, authority) = init_indexed_repo().await;
    let project_root = canonical_existing_identity(project.path()).unwrap();
    let mut live_activity = event_lane::subscribe().unwrap();
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let effects = Arc::new(AtomicUsize::new(0));
    let context = with_broker(
        registered_context(cg, &authority),
        Arc::clone(&broker),
        success_reconcile_sink(),
    )
    .with_owned_project_host_admission_replay()
    .with_code_index_hook_sink({
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let effects = Arc::clone(&effects);
        let expected_root = project_root.clone();
        Arc::new(move |root, paths| {
            assert_eq!(root, expected_root);
            assert_eq!(paths, ["src/lib.rs"]);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let effects = Arc::clone(&effects);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                effects.fetch_add(1, Ordering::SeqCst);
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    });
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    let event = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
        HostIntegrationIdV1::Claude,
        vec!["src/lib.rs".to_owned()],
        project.path().to_path_buf(),
    ))
    .unwrap();
    let response = server
        .handle_request(&super::JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(19)),
            method: tracedecay_hooks::core_events::HOOK_EVENT_METHOD.to_owned(),
            params: Some(event),
        })
        .await
        .unwrap();
    assert!(response.error.is_none(), "{response:?}");
    assert_eq!(response.result, Some(json!({})));
    entered.notified().await;
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        code_index_activity(&server).await,
        Vec::<event_lane::ActivityRecordV1>::new()
    );
    release.notify_one();
    server.wait_project_host_admission_replay_idle().await;
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let activity = code_index_activity(&server).await;
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].pulse.units, 1);
    assert_eq!(activity[0].pulse.detail, None);
    let mut queued_activity = Vec::new();
    while let Ok(record) = live_activity.try_recv() {
        if record.pulse.family == ActivityFamilyV1::CodeIndex
            && record.pulse.project_root == project_root
        {
            queued_activity.push(record);
        }
    }
    assert_eq!(queued_activity.len(), 1);
    assert_eq!(
        queued_activity[0].pulse.project_id,
        activity[0].pulse.project_id
    );
    server.replay_host_admission(None).await;
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(code_index_activity(&server).await, activity);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_path_hints_emit_activity_only_for_queued_paths() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let effects = Arc::new(AtomicUsize::new(0));
    let mut context = registered_context(cg, &authority);
    context.host_admission_broker = Some(broker);
    context = context
        .with_code_index_reconcile_sink(success_reconcile_sink())
        .with_code_index_hook_sink({
            let effects = Arc::clone(&effects);
            let root = canonical_existing_identity(project.path()).unwrap();
            Arc::new(move |effect_root, paths| {
                assert_eq!(effect_root, root);
                assert_eq!(paths, ["src/lib.rs"]);
                effects.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued })
            })
        });
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    for paths in [json!(["src/lib.rs"]), json!([])] {
        let event = json!({"agent":"claude", "event":"postToolUse", "rel_paths":paths, "cwd":project.path()});
        let outcome = server
            .handle_hook_event_notification(Some(&event), &mut HookProjectRouteCache::default())
            .await;
        assert_eq!(outcome.status, HostAdmissionStatus::Committed);
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let activity = code_index_activity(&server).await;
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].pulse.detail, None);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_file_edits_reconcile_branch_drift_without_blocking_the_source() {
    let (cg, project, authority) = init_indexed_repo().await;
    let broker_root = TempDir::new().unwrap();
    let broker = default_spool_broker(broker_root.path());
    let owner = registered_runtime(&authority)
        .project_registry_context_by_alias(project.path())
        .await
        .unwrap()
        .unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let available = Arc::new(AtomicBool::new(false));
    let root = canonical_existing_identity(project.path()).unwrap();
    let mut context = registered_context(cg, &authority);
    context.host_admission_broker = Some(Arc::clone(&broker));
    context = context.with_code_index_hook_sink({
        let effects = Arc::clone(&effects);
        let available = Arc::clone(&available);
        let root = root.clone();
        Arc::new(move |effect_root, paths| {
            assert_eq!(effect_root, root);
            assert_eq!(paths, ["src/lib.rs"]);
            if !available.load(Ordering::SeqCst) {
                return Box::pin(async { super::CodeIndexDemandAdmissionV1::RefusedByPolicy });
            }
            effects.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued })
        })
    });
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    let mut seqs = Vec::new();
    for branch in ["main", "next"] {
        if branch == "next" {
            git(&root, &["checkout", "-q", "-b", branch]);
        }
        let (_, scope) = tracedecay_mcp::scope::resolve_query_scope(&owner, &root).unwrap();
        let plan = tracedecay_mcp::hook_events::HookEventPlan::SyncFilesAt {
            root: root.clone(),
            scope,
            agent: HostIntegrationIdV1::Claude,
            event_kind: tracedecay_mcp::hook_events::HookEventKind::FileEdit,
            had_command: false,
            route: Some(HookRouteMetadata {
                session_id: Some("AKIASYNTHETICCANARY3".to_owned()),
                thread_id: Some("branch-session".to_owned()),
                cwd: Some(root.clone()),
                worktree: Some(root.clone()),
                branch: Some(branch.to_owned()),
            }),
            rel_paths: vec!["src/lib.rs".to_owned()],
        };
        let payload = tracedecay_mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap();
        seqs.push(broker.admit("same-source", &payload).await.unwrap().seq);
    }
    assert_eq!(broker.pending_replay_count().await.unwrap(), 2);
    assert_eq!(
        server.replay_host_admission(None).await.status,
        HostAdmissionStatus::Degraded
    );
    assert_eq!(broker.pending_replay_count().await.unwrap(), 2);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        code_index_activity(&server).await,
        Vec::<event_lane::ActivityRecordV1>::new()
    );
    available.store(true, Ordering::SeqCst);
    assert_eq!(
        server.replay_host_admission(None).await,
        HostAdmissionOutcome::accepted_for_replay()
    );
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(code_index_activity(&server).await.len(), 2);
    server.replay_host_admission(None).await;
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    server.ledger_writes_settled().await;
    let runtime = server.host_admission_test_runtime_for_test().unwrap();
    let rows = runtime
        .query_profile_analytics_events_for_test(&tracedecay_global_db::AnalyticsEventQuery {
            provider: Some("daemon_hook".to_owned()),
            project_id: None,
            session_id: None,
            event_kind: Some("hook_route".to_owned()),
            since: None,
            until: None,
            before_id: None,
            limit: 16,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let mut observed_seqs = rows
        .iter()
        .map(|row| {
            let metadata = row.metadata_json.as_deref().unwrap();
            assert!(!metadata.contains("AKIASYNTHETICCANARY3"));
            assert_ne!(row.session_id.as_deref(), Some("AKIASYNTHETICCANARY3"));
            serde_json::from_str::<Value>(metadata).unwrap()["admission_seq"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    observed_seqs.sort_unstable();
    assert_eq!(observed_seqs, seqs);
    let spans = runtime
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("next".to_owned()),
                since: None,
                until: None,
                limit: 16,
            },
            CommitRelationFilter::Produced,
        )
        .await
        .unwrap();
    assert_eq!(spans.len(), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_file_edits_replay_once_on_each_exact_linked_worktree() {
    let (cg, project, authority) = init_indexed_repo().await;
    let mut live_activity = event_lane::subscribe().unwrap();
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let effects = Arc::new(Mutex::new(Vec::new()));
    let sink: super::CodeIndexHookSink = {
        let effects = Arc::clone(&effects);
        Arc::new(move |root, paths| {
            let effects = Arc::clone(&effects);
            Box::pin(async move {
                effects.lock().unwrap().push((root, paths));
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    };
    let runtime = registered_runtime(&authority);
    let owner = runtime
        .project_registry_context_by_alias(project.path())
        .await
        .unwrap()
        .unwrap();
    let mut servers = Vec::new();
    let mut plans = Vec::new();
    let mut roots = Vec::new();
    for (name, directory) in [
        ("linked-a", "linked-a-api_key=locator-required"),
        ("linked-b", "linked-b"),
    ] {
        let root = project.path().join(directory);
        git(
            project.path(),
            &["worktree", "add", "-q", "-b", name, root.to_str().unwrap()],
        );
        std::fs::write(root.join("src/lib.rs"), "pub fn edited() {}\n").unwrap();
        let graph = authority.reopen_project_graph(&root).await;
        let (_, scope) = tracedecay_mcp::scope::resolve_query_scope(&owner, &root).unwrap();
        let mut context =
            registered_context(graph, &authority).with_code_index_hook_sink(Arc::clone(&sink));
        context.host_admission_broker = Some(Arc::clone(&broker));
        servers.push(
            McpServer::new_with_registered_test_context(context, Vec::new())
                .await
                .unwrap(),
        );
        let plan = tracedecay_mcp::hook_events::HookEventPlan::SyncFilesAt {
            root: root.clone(),
            scope,
            agent: HostIntegrationIdV1::Claude,
            event_kind: tracedecay_mcp::hook_events::HookEventKind::FileEdit,
            had_command: false,
            route: Some(HookRouteMetadata {
                session_id: Some("AKIASYNTHETICCANARY3".to_owned()),
                thread_id: Some(name.to_owned()),
                cwd: Some(root.clone()),
                worktree: Some(root.clone()),
                branch: None,
            }),
            rel_paths: vec!["src/lib.rs".to_owned()],
        };
        let encoded = tracedecay_mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("AKIASYNTHETICCANARY3"));
        let decoded =
            tracedecay_mcp::hook_events::decode_durable_hook_event_plan(&encoded).unwrap();
        let tracedecay_mcp::hook_events::HookEventPlan::SyncFilesAt {
            root: decoded_root,
            scope: decoded_scope,
            ..
        } = &decoded
        else {
            panic!("routed edit")
        };
        assert_eq!(decoded_root, &root);
        let tracedecay_mcp::hook_events::HookEventPlan::SyncFilesAt { scope, .. } = &plan else {
            unreachable!()
        };
        assert_eq!(decoded_scope, scope);
        broker.admit(name, &encoded).await.unwrap();
        plans.push(decoded);
        roots.push(canonical_existing_identity(&root).unwrap());
    }
    let mut context = registered_context(cg, &authority);
    context.host_admission_broker = Some(Arc::clone(&broker));
    context = context.with_code_index_hook_sink(Arc::clone(&sink));
    let server = McpServer::new_with_registered_test_context(context, servers.clone())
        .await
        .unwrap();
    assert_eq!(broker.pending_replay_count().await.unwrap(), 2);
    let drained = server.replay_host_admission(None).await;
    assert_eq!(drained, HostAdmissionOutcome::accepted_for_replay());
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    server.replay_host_admission(None).await;
    let mut activity_roots = Vec::new();
    while let Ok(record) = live_activity.try_recv() {
        if record.pulse.family == ActivityFamilyV1::CodeIndex
            && roots.contains(&record.pulse.project_root)
        {
            assert_eq!(
                record.pulse.project_id.as_deref(),
                Some(owner.project.project_id.as_str())
            );
            activity_roots.push(record.pulse.project_root);
        }
    }
    activity_roots.sort();
    let mut expected_roots = roots.clone();
    expected_roots.sort();
    assert_eq!(activity_roots, expected_roots);
    assert_eq!(
        *effects.lock().unwrap(),
        roots
            .iter()
            .map(|root| (root.clone(), vec!["src/lib.rs".to_owned()]))
            .collect::<Vec<_>>()
    );
    let warm_edit = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
        HostIntegrationIdV1::Claude,
        vec!["src/lib.rs".to_owned()],
        roots[0].clone(),
    ))
    .unwrap();
    let outcome = server
        .handle_hook_event_notification(Some(&warm_edit), &mut HookProjectRouteCache::default())
        .await;
    assert_eq!(outcome.status, HostAdmissionStatus::Committed);
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    assert_eq!(
        effects.lock().unwrap().len(),
        3,
        "one warm admission queues one sync"
    );
    assert_eq!(
        effects.lock().unwrap()[2],
        (roots[0].clone(), vec!["src/lib.rs".to_owned()])
    );
    let tracedecay_mcp::hook_events::HookEventPlan::SyncFilesAt { root, .. } = &mut plans[0] else {
        unreachable!()
    };
    *root = roots[1].clone();
    let outcome = server
        .run_hook_event_plan(server.cg().await, project.path(), plans.remove(0), None)
        .await;
    assert_eq!(outcome.reason_code, Some("project_route_stale"));
    assert_eq!(effects.lock().unwrap().len(), 3);
    server.shutdown().await;
    for server in servers {
        server.shutdown().await;
    }
}

#[tokio::test]
async fn hook_watch_policy_refusal_is_not_scheduler_unavailable() {
    let (cg, project, authority) = init_indexed_repo().await;
    let context = registered_context(cg, &authority)
        .with_code_index_reconcile_sink(Arc::new(|_, _| {
            Box::pin(async { super::CodeIndexDemandAdmissionV1::RefusedByPolicy })
        }))
        .with_code_index_hook_sink(Arc::new(|_, _| {
            Box::pin(async { super::CodeIndexDemandAdmissionV1::RefusedByPolicy })
        }));
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    let cg = server.cg_snapshot().await;
    for plan in [
        tracedecay_mcp::hook_events::HookEventPlan::SyncFiles(vec!["src/a.rs".to_owned()]),
        tracedecay_mcp::hook_events::HookEventPlan::SyncCurrentBranch {
            branch: cg.active_branch().unwrap().to_owned(),
            agent: HostIntegrationIdV1::Codex,
        },
    ] {
        let outcome = server
            .run_hook_event_plan(Arc::clone(&cg), project.path(), plan, None)
            .await;
        assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
        assert_eq!(outcome.reason_code, Some("linked_worktree_disabled"));
        assert!(!outcome.retryable);
    }
    server.run_startup_catch_up_sync().await;
    assert!(server.startup_catch_up_done());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_saves_share_spool_durability_barriers_on_a_slow_fsync_disk() {
    const SAVES: usize = 8;
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let context = with_broker(
        registered_context(cg, &authority),
        Arc::clone(&broker),
        success_reconcile_sink(),
    )
    .with_code_index_hook_sink(Arc::new(|_, _| {
        Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued })
    }));
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .unwrap();
    // A loaded disk: every spool durability barrier waits as long as the
    // ~24 ms per fsync measured under load in #2659.
    let slow_disk = sync_latency::inject(spool.path(), Duration::from_millis(20));
    let start = Arc::new(Barrier::new(SAVES + 1));

    let saves = (0..SAVES)
        .map(|index| {
            let server = Arc::clone(&server);
            let start = Arc::clone(&start);
            let event = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
                HostIntegrationIdV1::Codex,
                vec![format!("src/saved_{index}.rs")],
                project.path().to_path_buf(),
            ))
            .unwrap();
            tokio::spawn(async move {
                start.wait().await;
                let mut routes = HookProjectRouteCache::default();
                let outcome =
                    Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes))
                        .await;
                outcome.status
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;
    for save in saves {
        let status = save.await.unwrap();
        assert!(
            matches!(
                status,
                HostAdmissionStatus::Committed | HostAdmissionStatus::AcceptedForReplay
            ),
            "save outcome {status:?}"
        );
    }
    let concurrent_syncs = slow_disk.syncs();
    for index in SAVES..(2 * SAVES) {
        let event = serde_json::to_value(DaemonHookEvent::post_tool_use_edit(
            HostIntegrationIdV1::Codex,
            vec![format!("src/saved_{index}.rs")],
            project.path().to_path_buf(),
        ))
        .unwrap();
        let mut routes = HookProjectRouteCache::default();
        let outcome =
            Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
        assert!(
            matches!(
                outcome.status,
                HostAdmissionStatus::Committed | HostAdmissionStatus::AcceptedForReplay
            ),
            "sequential save outcome {:?}",
            outcome.status
        );
    }
    let sequential_syncs = slow_disk.syncs().saturating_sub(concurrent_syncs);
    assert!(
        concurrent_syncs < sequential_syncs,
        "concurrent saves should amortize durability barriers: concurrent syncs={concurrent_syncs}, sequential syncs={sequential_syncs}"
    );
    assert_eq!(broker.pending_count().await, 0);
    drop(slow_disk);
    server.shutdown().await;
}

#[tokio::test]
async fn hook_event_is_durable_before_attempt_and_retained_on_failure() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted_after_append = Arc::new(Mutex::new(false));
    let reconcile_sink: CodeIndexReconcileSink = {
        let broker = Arc::clone(&broker);
        let attempted_after_append = Arc::clone(&attempted_after_append);
        Arc::new(move |_request: PathBuf, _demand| {
            let broker = Arc::clone(&broker);
            let attempted_after_append = Arc::clone(&attempted_after_append);
            Box::pin(async move {
                assert_eq!(broker.pending_count().await, 1);
                *attempted_after_append.lock().unwrap() = true;
                super::CodeIndexDemandAdmissionV1::Unavailable(
                    tracedecay_code_index_runtime::code_index_scheduler::CodeIndexDemandUnavailableV1::SchedulerUnmounted,
                )
            })
        })
    };
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        outcome.reason_code,
        Some("code_index_scheduler_unavailable")
    );
    assert!(*attempted_after_append.lock().unwrap());
    assert_eq!(broker.pending_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_event_is_rejected_before_canonical_attempt() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::new(8, 128, 256, 4))
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("spool_record_too_large"));
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_semantic_payload_is_explicit_and_quarantined_across_reopen() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(
        cg,
        &authority,
        Arc::clone(&broker),
        Arc::clone(&reconcile_sink),
    )
    .await;
    let admitted = broker
        .admit(
            "codex:invalid-plan-fixture",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
        )
        .await
        .unwrap();

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
    assert!(!outcome.retryable);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    let rendered = serde_json::to_string(&outcome).unwrap();
    assert!(!rendered.contains("invalid-plan-fixture"));
    assert!(!rendered.contains("\"branch\":\"\""));
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    server.shutdown().await;
    drop(server);
    drop(broker);

    let broker = default_spool_broker(spool.path());
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server =
        server_with_broker(reopened, &authority, Arc::clone(&broker), reconcile_sink).await;

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;

    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn unsupported_payload_version_is_retryable_and_retained_across_reopen() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let payload = br#"{"version":2,"plan":{"kind":"future_host_event","opaque":"private"}}"#;
    let admitted = broker
        .admit("codex:future-plan-fixture", payload)
        .await
        .unwrap();

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        outcome.reason_code,
        Some("host_event_payload_unsupported_version")
    );
    assert!(outcome.retryable);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(broker.quarantine_count().await, 0);
    server.shutdown().await;
    drop(server);
    drop(broker);

    let recovered = default_spool_broker(spool.path());
    assert_eq!(recovered.pending_count().await, 1);
    assert_eq!(recovered.quarantine_count().await, 0);
    let replay = recovered.begin_replay().await.unwrap();
    let retained = replay.lease_next().await.unwrap().unwrap();
    assert_eq!(retained.seq, admitted.seq);
    assert_eq!(retained.payload.as_slice(), payload);
    replay.defer(retained.seq).await.unwrap();
}

#[tokio::test]
async fn quarantine_releases_active_capacity_then_full_fails_closed() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let bounds = SpoolBounds::new(256, 128, 1024, 1).with_quarantine_limits(1024, 1);
    let runtime = HostAdmissionRuntime::open(spool.path(), bounds).unwrap().0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;

    let first = broker
        .admit(
            "codex:first-terminal",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
        )
        .await
        .unwrap();
    let first_outcome = Box::pin(server.replay_host_admission(Some(first.seq))).await;
    assert_eq!(
        first_outcome.reason_code,
        Some("host_event_payload_malformed")
    );
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);

    let second = broker
        .admit(
            "codex:second-terminal",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":"","secret":"do-not-report"}}"#,
        )
        .await
        .expect("first terminal must release the one-record active capacity");
    let second_outcome = Box::pin(server.replay_host_admission(Some(second.seq))).await;
    assert_eq!(second_outcome, HostAdmissionOutcome::quarantine_full());
    assert!(!matches!(
        second_outcome.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
    ));
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(broker.quarantine_count().await, 1);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert!(
        !serde_json::to_string(&second_outcome)
            .unwrap()
            .contains("do-not-report")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_source_does_not_starve_valid_sibling_source() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let valid_payload = tracedecay_mcp::hook_events::encode_durable_hook_event_plan(
        &tracedecay_mcp::hook_events::HookEventPlan::AddBranch("main".to_string()),
    )
    .unwrap();
    let attempts = Arc::new(Mutex::new(0usize));
    let reconcile_sink: CodeIndexReconcileSink = {
        let attempts = Arc::clone(&attempts);
        Arc::new(move |_request, _demand| {
            let attempts = Arc::clone(&attempts);
            Box::pin(async move {
                *attempts.lock().unwrap() += 1;
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    };
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let malformed = broker
        .admit(
            "codex:malformed-source",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
        )
        .await
        .unwrap();
    broker
        .admit("claude:valid-source", &valid_payload)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        Box::pin(server.replay_host_admission(Some(malformed.seq))),
    )
    .await
    .expect("bounded replay must not spin on the malformed record");

    assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
    assert_eq!(*attempts.lock().unwrap(), 1);
    assert_eq!(
        broker.pending_count().await,
        0,
        "terminal evidence is quarantined and the committed sibling releases active capacity"
    );
    assert_eq!(broker.quarantine_count().await, 1);

    Box::pin(server.replay_host_admission(Some(malformed.seq))).await;
    assert_eq!(
        *attempts.lock().unwrap(),
        1,
        "the completed sibling is not retried before restart"
    );
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn cancelled_canonical_attempt_is_recovered_and_replayed() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempts = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let reconcile_sink: CodeIndexReconcileSink = {
        let attempts = Arc::clone(&attempts);
        let started = Arc::clone(&started);
        Arc::new(move |_request, _demand| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let started = Arc::clone(&started);
            Box::pin(async move {
                if attempt == 0 {
                    started.notify_one();
                    return std::future::pending::<super::CodeIndexDemandAdmissionV1>().await;
                }
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    };
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let event = session_start(project.path().to_path_buf());
    let attempt = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let mut routes = HookProjectRouteCache::default();
            Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .expect("canonical attempt should start");
    assert_eq!(broker.pending_count().await, 1);
    attempt.abort();
    assert!(attempt.await.unwrap_err().is_cancelled());

    let outcome = Box::pin(server.replay_host_admission(None)).await;
    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

fn add_branch_at_payload(root: PathBuf, branch: &str) -> Vec<u8> {
    tracedecay_mcp::hook_events::encode_durable_hook_event_plan(
        &tracedecay_mcp::hook_events::HookEventPlan::AddBranchAt {
            root,
            branch: branch.to_string(),
            agent: HostIntegrationIdV1::Codex,
        },
    )
    .expect("add_branch_at plan should encode")
}

fn linked_worktree_on(project: &std::path::Path) -> PathBuf {
    // Sibling of the unique TempDir root, never a shared /tmp fixed name.
    let worktree = project.with_file_name(format!(
        "{}-admission-wt",
        project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo")
    ));
    let worktree_arg = worktree.to_string_lossy();
    git(
        project,
        &[
            "worktree",
            "add",
            worktree_arg.as_ref(),
            "-b",
            "feature/admission",
        ],
    );
    worktree
}

fn unique_sibling(project: &std::path::Path, suffix: &str) -> PathBuf {
    project.with_file_name(format!(
        "{}-{suffix}",
        project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo")
    ))
}

#[tokio::test]
async fn add_branch_at_replay_rejects_stale_root_after_adversarial_replace() {
    let (cg, project, authority) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let payload = add_branch_at_payload(worktree.clone(), "feature/admission");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let admitted = broker
        .admit("codex:add-branch-at-stale", &payload)
        .await
        .unwrap();

    // Adversarial remove/replace after admit, before effect/replay.
    std::fs::remove_dir_all(&worktree).expect("remove worktree");
    std::fs::create_dir_all(worktree.join("src")).expect("replacement dirs");
    std::fs::write(worktree.join("src/a.rs"), "pub fn replaced() {}\n").expect("write");
    git(&worktree, &["init", "-q", "-b", "main"]);
    git(&worktree, &["config", "user.email", "t@t.com"]);
    git(&worktree, &["config", "user.name", "T"]);
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "replacement"]);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    assert_eq!(
        attempted.load(Ordering::SeqCst),
        0,
        "stale root must not write"
    );
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);

    // A second pass cannot reauthorize or write the terminal admission.
    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_at_replay_rejects_stale_branch_after_switch() {
    let (cg, project, authority) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let payload = add_branch_at_payload(worktree.clone(), "feature/admission");
    git(&worktree, &["switch", "-c", "feature/other"]);

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let admitted = broker
        .admit("codex:add-branch-at-stale-branch", &payload)
        .await
        .unwrap();

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_replay_rejects_stale_branch_after_delayed_switch() {
    let (cg, project, authority) = init_indexed_repo().await;
    let payload = add_branch_payload_for("main");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let admitted = broker
        .admit("codex:add-branch-stale-delayed", &payload)
        .await
        .unwrap();

    // Delayed switch after admit, before effect/replay.
    git(project.path(), &["switch", "-c", "feature/other"]);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    assert!(!outcome.retryable);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_restart_replay_rejects_stale_branch_after_switch() {
    let (_cg, project, authority) = init_indexed_repo().await;
    let payload = add_branch_payload_for("main");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:add-branch-stale-restart", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    git(project.path(), &["switch", "-c", "feature/restart"]);

    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server =
        server_with_broker(reopened, &authority, Arc::clone(&broker), reconcile_sink).await;

    let outcome = Box::pin(server.replay_host_admission(None)).await;
    assert!(matches!(
        outcome.status,
        HostAdmissionStatus::Degraded | HostAdmissionStatus::AcceptedForReplay
    ));
    if outcome.status == HostAdmissionStatus::Degraded {
        assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
        assert!(!outcome.retryable);
    }
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn sync_current_branch_replay_rejects_stale_branch_after_delayed_switch() {
    let (cg, project, authority) = init_indexed_repo().await;
    let payload = sync_current_branch_payload("main");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let server = server_with_broker(cg, &authority, Arc::clone(&broker), reconcile_sink).await;
    let admitted = broker
        .admit("codex:sync-current-branch-stale-delayed", &payload)
        .await
        .unwrap();

    git(project.path(), &["switch", "-c", "feature/sync-other"]);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    assert!(!outcome.retryable);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn sync_current_branch_restart_replay_rejects_stale_branch_after_switch() {
    let (_cg, project, authority) = init_indexed_repo().await;
    let payload = sync_current_branch_payload("main");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:sync-current-branch-stale-restart", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    git(project.path(), &["switch", "-c", "feature/sync-restart"]);

    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server =
        server_with_broker(reopened, &authority, Arc::clone(&broker), reconcile_sink).await;

    let outcome = Box::pin(server.replay_host_admission(None)).await;
    assert!(matches!(
        outcome.status,
        HostAdmissionStatus::Degraded | HostAdmissionStatus::AcceptedForReplay
    ));
    if outcome.status == HostAdmissionStatus::Degraded {
        assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
        assert!(!outcome.retryable);
    }
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_at_restart_replay_rejects_common_dir_drift() {
    let (_cg, project, authority) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let payload = add_branch_at_payload(worktree.clone(), "feature/admission");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:add-branch-at-common-dir", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    let stranger = unique_sibling(project.path(), "stranger-repo");
    std::fs::create_dir_all(stranger.join("src")).expect("stranger");
    std::fs::write(stranger.join("src/a.rs"), "pub fn stranger() {}\n").expect("write");
    git(&stranger, &["init", "-q", "-b", "main"]);
    git(&stranger, &["config", "user.email", "t@t.com"]);
    git(&stranger, &["config", "user.name", "T"]);
    git(&stranger, &["add", "."]);
    git(&stranger, &["commit", "-q", "-m", "stranger"]);
    let stranger_git = stranger.join(".git").canonicalize().expect("gitdir");
    let git_pointer = worktree.join(".git");
    assert!(git_pointer.is_file());
    std::fs::write(
        &git_pointer,
        format!("gitdir: {}\n", stranger_git.display()),
    )
    .expect("rewrite common-dir pointer");

    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server =
        server_with_broker(reopened, &authority, Arc::clone(&broker), reconcile_sink).await;

    let outcome = Box::pin(server.replay_host_admission(None)).await;
    assert!(matches!(
        outcome.status,
        HostAdmissionStatus::Degraded | HostAdmissionStatus::AcceptedForReplay
    ));
    if outcome.status == HostAdmissionStatus::Degraded {
        assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    }
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn add_branch_at_restart_replay_rejects_symlink_swap() {
    let (_cg, project, authority) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let alias = unique_sibling(project.path(), "worktree-alias");
    std::os::unix::fs::symlink(&worktree, &alias).expect("alias");
    let payload = add_branch_at_payload(alias.clone(), "feature/admission");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:add-branch-at-symlink", &payload)
        .await
        .unwrap();
    drop(broker);

    let stranger = unique_sibling(project.path(), "symlink-stranger");
    std::fs::create_dir_all(stranger.join("src")).expect("stranger");
    std::fs::write(stranger.join("src/a.rs"), "pub fn stranger() {}\n").expect("write");
    git(&stranger, &["init", "-q", "-b", "main"]);
    git(&stranger, &["config", "user.email", "t@t.com"]);
    git(&stranger, &["config", "user.name", "T"]);
    git(&stranger, &["add", "."]);
    git(&stranger, &["commit", "-q", "-m", "stranger"]);
    std::fs::remove_file(&alias).expect("remove alias");
    std::os::unix::fs::symlink(&stranger, &alias).expect("swap");

    let broker = default_spool_broker(spool.path());
    let attempted = Arc::new(AtomicUsize::new(0));
    let reconcile_sink = counting_reconcile_sink(Arc::clone(&attempted));
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server =
        server_with_broker(reopened, &authority, Arc::clone(&broker), reconcile_sink).await;

    let outcome = Box::pin(server.replay_host_admission(None)).await;
    assert!(matches!(
        outcome.status,
        HostAdmissionStatus::Degraded | HostAdmissionStatus::AcceptedForReplay
    ));
    if outcome.status == HostAdmissionStatus::Degraded {
        assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    }
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    server.shutdown().await;
}

async fn server_with_broker_and_runtime(
    cg: tracedecay_project::project::TraceDecay,
    broker: SharedHostAdmissionBroker,
    reconcile_sink: CodeIndexReconcileSink,
    runtime: Arc<HostAdmissionTestRuntimeV1>,
) -> Arc<McpServer> {
    let mut context = with_broker(
        crate::test_support::host_admission::mcp_server_context_for_test(runtime, cg, None)
            .expect("registered MCP server context"),
        broker,
        reconcile_sink,
    );
    // Same isolation as `registered_context`: startup catch-up admits one
    // reconciliation through the counted sink and must not race this
    // fixture's accounting.
    context.startup_catch_up_enabled = false;
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

#[tokio::test]
async fn durable_route_survives_unavailable_effect_for_same_connection_retry() {
    let (cg, project, authority) = init_indexed_repo().await;
    let test_runtime = registered_runtime(&authority);
    let git_dir = project.path().join(".git");
    let registered = test_runtime
        .upsert_code_project(
            "proj_route_admission",
            project.path(),
            Some(&git_dir),
            None,
            Some("main"),
        )
        .await
        .expect("register route project");
    test_runtime
        .upsert_project_alias(project.path(), &registered.project_id)
        .await
        .expect("register route alias");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let first_attempt = Arc::new(AtomicBool::new(true));
    let reconcile_sink: CodeIndexReconcileSink = {
        let first_attempt = Arc::clone(&first_attempt);
        Arc::new(move |_request, _demand| {
            let first_attempt = Arc::clone(&first_attempt);
            Box::pin(async move {
                if first_attempt.swap(false, Ordering::SeqCst) {
                    return super::CodeIndexDemandAdmissionV1::Unavailable(
                        tracedecay_code_index_runtime::code_index_scheduler::CodeIndexDemandUnavailableV1::SchedulerUnmounted,
                    );
                }
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    };
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), reconcile_sink, test_runtime).await;
    let raw_session = ["AKIA", "SYNTHETIC", "CANARY", "3"].concat();
    let event = serde_json::to_value(
        DaemonHookEvent::session_start(HostIntegrationIdV1::Codex, project.path().to_path_buf())
            .with_route(Some(HookRouteMetadata {
                session_id: Some(raw_session.clone()),
                thread_id: None,
                cwd: Some(project.path().to_path_buf()),
                worktree: Some(project.path().to_path_buf()),
                branch: Some("main".to_string()),
            })),
    )
    .unwrap();
    let mut routes = HookProjectRouteCache::default();

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert_eq!(first.status, HostAdmissionStatus::Unavailable);
    assert_eq!(broker.pending_count().await, 1);

    let mut tool_arguments = json!({"query": "needle", "session_id": raw_session});
    crate::mcp::project_route::protect_tool_structural_ids(&mut tool_arguments)
        .expect("protect routed tool identities");
    let expected_arguments = tool_arguments.clone();
    let selected = match routes.workspace_route_for_arguments(&tool_arguments) {
        Some(crate::mcp::project_route::WorkspaceProjectRoute::Resolved(selected)) => selected,
        Some(crate::mcp::project_route::WorkspaceProjectRoute::Failed(failure)) => {
            panic!("the durable hook route failed: {}", failure.detail)
        }
        None => panic!("the durable hook route must remain available"),
    };
    let routed = tool_arguments;
    assert_eq!(
        routed, expected_arguments,
        "the same connection must not mutate tool arguments after durable append"
    );
    assert_eq!(
        selected.owner.project.canonical_root, registered.canonical_root,
        "the same connection must carry the exact project route out of band"
    );
    assert_eq!(
        routed["session_id"],
        tracedecay_privacy::protect_sensitive_structural_id(
            event["route"]["session_id"].as_str().expect("raw session")
        )
        .unwrap()
    );
    assert!(
        !routed
            .to_string()
            .contains(event["route"]["session_id"].as_str().expect("raw session"))
    );
    server.ledger_writes_settled().await;
    let test_runtime = server
        .host_admission_test_runtime_for_test()
        .expect("host-admission test runtime");
    let rows = test_runtime
        .query_profile_analytics_events_for_test(&tracedecay_global_db::AnalyticsEventQuery {
            provider: Some("daemon_hook".to_string()),
            project_id: None,
            session_id: None,
            event_kind: Some("hook_route".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 16,
        })
        .await
        .expect("query pre-commit analytics");
    assert!(rows.is_empty(), "retained effects must not leak analytics");
    let spans = test_runtime
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 16,
            },
            CommitRelationFilter::Produced,
        )
        .await
        .expect("query pre-commit hook spans");
    assert!(spans.is_empty(), "retained effects must not leak git spans");

    let replayed = Box::pin(server.replay_host_admission(None)).await;
    assert!(matches!(
        replayed.status,
        HostAdmissionStatus::AcceptedForReplay
            | HostAdmissionStatus::Committed
            | HostAdmissionStatus::ExactDuplicate
    ));
    server.wait_project_host_admission_replay_idle().await;
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn committed_admissions_emit_post_commit_private_route_analytics() {
    let (cg, project, authority) = init_indexed_repo().await;
    let test_runtime = registered_runtime(&authority);
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let reconcile_sink: CodeIndexReconcileSink =
        Arc::new(|_request, _demand| Box::pin(async { super::CodeIndexDemandAdmissionV1::Queued }));
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), reconcile_sink, test_runtime).await;
    let mut routes = HookProjectRouteCache::default();
    let event = terminal_receipt(project.path().to_path_buf());

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert!(matches!(
        first.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
    ));
    server.ledger_writes_settled().await;

    let second = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert!(matches!(
        second.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
    ));
    server.ledger_writes_settled().await;

    let rows = server
        .host_admission_test_runtime_for_test()
        .expect("host-admission test runtime")
        .query_profile_analytics_events_for_test(&tracedecay_global_db::AnalyticsEventQuery {
            provider: Some("daemon_hook".to_string()),
            project_id: None,
            session_id: None,
            event_kind: Some("hook_route".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 16,
        })
        .await
        .expect("query analytics");
    assert_eq!(
        rows.len(),
        2,
        "distinct durable admissions must remain distinct analytics rows: {rows:?}"
    );
    assert!(rows.iter().all(|row| {
        row.session_id.as_deref() == Some("session-admission-test")
            && row.metadata_json.as_deref().is_some_and(|metadata| {
                !metadata.contains("session-admission-test") && metadata.contains("idempotency_key")
            })
            && row
                .hint_id
                .as_deref()
                .is_some_and(|id| id.starts_with("h:"))
    }));
    assert_ne!(rows[0].hint_id, rows[1].hint_id);
    server.shutdown().await;
}

#[tokio::test]
async fn credential_canary_receipt_analytics_and_git_span_survive_database_reopen() {
    let (cg, project, authority) = init_indexed_repo().await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let profile_root = authority.profile_root().to_path_buf();
    let project_id = tracedecay_domain::ProjectId::new(
        cg.store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("project identity"),
    )
    .expect("typed project identity");
    let test_runtime = registered_runtime(&authority);
    let git_dir = project.path().join(".git");
    let registered = test_runtime
        .upsert_code_project(
            "proj_hook_identity",
            project.path(),
            Some(&git_dir),
            None,
            Some("main"),
        )
        .await
        .expect("register identity project");
    test_runtime
        .upsert_project_alias(project.path(), &registered.project_id)
        .await
        .expect("register identity alias");

    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let server = server_with_broker_and_runtime(
        cg,
        Arc::clone(&broker),
        success_reconcile_sink(),
        test_runtime,
    )
    .await;
    let raw = ["AKIA", "SYNTHETIC", "CANARY", "4"].concat();
    let protected = tracedecay_privacy::protect_sensitive_structural_id(&raw).unwrap();
    let session = SessionRecord {
        provider: "hermes".to_string(),
        session_id: protected.clone(),
        project_key: registered.canonical_root.clone(),
        project_path: registered.canonical_root.clone(),
        title: None,
        started_at: Some(1),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let test_runtime = server
        .host_admission_test_runtime_for_test()
        .expect("host-admission test runtime");
    assert!(
        test_runtime
            .upsert_session_for_test(HostAdmissionScope::Project, &session)
            .await
            .expect("seed protected session")
    );
    test_runtime
        .seed_session_messages_for_test(
            HostAdmissionScope::Project,
            &session,
            std::slice::from_ref(&SessionMessageRecord {
                provider: "hermes".to_string(),
                message_id: protected.clone(),
                session_id: protected.clone(),
                role: "assistant".to_string(),
                timestamp: Some(1),
                ordinal: 1,
                text: "credential canary join fixture".to_string(),
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            }),
        )
        .await
        .expect("seed protected transcript");
    let route = HookRouteMetadata {
        session_id: Some(raw.clone()),
        thread_id: Some(raw.clone()),
        cwd: Some(project.path().to_path_buf()),
        worktree: Some(project.path().to_path_buf()),
        branch: Some("main".to_string()),
    };
    let receipt = HookTerminalReceipt {
        tool_call_id: Some(raw.clone()),
        turn_id: Some(raw.clone()),
        status: Some("success".to_string()),
        duration_ms: Some(1),
        transcript_watermark: Some(raw.clone()),
    };
    let terminal = serde_json::to_value(hermes_terminal_receipt_event(
        project.path().to_path_buf(),
        route.clone(),
        receipt.clone(),
    ))
    .unwrap();
    let mut turn_ingested =
        hermes_terminal_receipt_event(project.path().to_path_buf(), route, receipt);
    turn_ingested.event = "turnIngested".to_string();
    let turn_ingested = serde_json::to_value(turn_ingested).unwrap();
    let mut routes = HookProjectRouteCache::default();

    for event in [&terminal, &turn_ingested] {
        let outcome =
            Box::pin(server.handle_hook_event_notification(Some(event), &mut routes)).await;
        assert!(matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ));
    }
    server.ledger_writes_settled().await;
    let ready =
        tracedecay_automation_runtime::automation::host_receipts::oldest_ready(&dashboard_root)
            .await
            .unwrap()
            .expect("credential receipt should join its ingested watermark");
    assert_eq!(ready.pending.session_key, protected);
    assert_eq!(ready.transcript_watermark, protected);
    assert!(
        test_runtime
            .project_lcm_raw_message_exists_for_test("hermes", &ready.transcript_watermark)
            .await
            .expect("query protected LCM message"),
        "receipt watermark must join the protected LCM message"
    );

    server.shutdown().await;
    // The profile session-relation graph has exactly one reconcile_sink, so every
    // retained handle must drop before a fresh-process reopen can mount it.
    // `test_runtime` only borrows the server, so its borrow ends here on its
    // own; the server is the retained handle that must drop.
    drop(server);
    let _profile_pin = authority.release_runtime_for_reopen();
    let reopened = HostAdmissionTestRuntimeV1::project(&profile_root, project.path(), project_id)
        .await
        .expect("reopen registered host-admission runtime");
    let analytics = reopened
        .query_profile_analytics_events_for_test(&tracedecay_global_db::AnalyticsEventQuery {
            provider: Some("daemon_hook".to_string()),
            project_id: None,
            session_id: Some(protected.clone()),
            event_kind: Some("hook_route".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 16,
        })
        .await
        .expect("query protected route analytics");
    assert_eq!(analytics.len(), 2);
    assert!(analytics.iter().all(|row| {
        row.session_id.as_deref() == Some(protected.as_str())
            && !row
                .metadata_json
                .as_deref()
                .unwrap_or_default()
                .contains(&raw)
    }));
    assert!(
        reopened
            .project_lcm_raw_message_exists_for_test("hermes", &protected)
            .await
            .expect("query reopened protected LCM message"),
        "protected LCM join must survive database reopen"
    );

    let spans = reopened
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 16,
            },
            CommitRelationFilter::Produced,
        )
        .await
        .expect("query protected hook spans after reopen");
    assert!(
        spans.iter().any(|span| span.session_id == protected),
        "protected hook span must remain joinable after database reopen"
    );
}

#[tokio::test]
async fn owned_project_replay_worker_continues_past_one_bounded_batch() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let payload = add_branch_payload();
    for index in 0..65 {
        broker
            .admit(&format!("codex:startup-{index}"), &payload)
            .await
            .unwrap();
    }
    assert_eq!(broker.pending_count().await, 65);

    let server = server_with_owned_project_replay_worker(
        cg,
        &authority,
        Arc::clone(&broker),
        success_reconcile_sink(),
    )
    .await;

    server.wait_project_host_admission_replay_idle().await;
    assert_eq!(broker.pending_count().await, 0);
    assert!(
        server.project_host_admission_replay_pass_count().await >= 2,
        "65 records require more than one 64-record pass"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn owned_project_replay_worker_backoffs_on_retryable_failure() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:retryable", &add_branch_payload())
        .await
        .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let reconcile_sink: CodeIndexReconcileSink = {
        let attempts = Arc::clone(&attempts);
        Arc::new(move |_request, _demand| {
            let attempts = Arc::clone(&attempts);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                super::CodeIndexDemandAdmissionV1::Unavailable(
                    tracedecay_code_index_runtime::code_index_scheduler::CodeIndexDemandUnavailableV1::SchedulerUnmounted,
                )
            })
        })
    };

    let server = server_with_owned_project_replay_worker(
        cg,
        &authority,
        Arc::clone(&broker),
        reconcile_sink,
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while server.project_host_admission_replay_backoff_count().await < 2
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        server.project_host_admission_replay_backoff_count().await >= 2,
        "retryable retained failures must sleep between passes"
    );
    assert!(attempts.load(Ordering::SeqCst) >= 2);
    assert_eq!(broker.pending_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn owned_project_replay_worker_is_cancelled_and_joined_on_shutdown() {
    let (cg, _project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker
        .admit("codex:hang", &add_branch_payload())
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let reconcile_sink: CodeIndexReconcileSink = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Arc::new(move |_request, _demand| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Box::pin(async move {
                // `notify_one` stores a permit when no waiter is registered
                // yet; `notify_waiters` would drop the signal whenever the
                // worker reaches the sink before the test starts awaiting.
                entered.notify_one();
                release.notified().await;
                super::CodeIndexDemandAdmissionV1::Queued
            })
        })
    };

    let server = server_with_owned_project_replay_worker(
        cg,
        &authority,
        Arc::clone(&broker),
        reconcile_sink,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("worker must enter an in-flight canonical attempt");

    tokio::time::timeout(Duration::from_secs(2), server.shutdown())
        .await
        .expect("shutdown must cancel and join the owned project replay worker");
    // Keep release alive so the aborted task drop path stays well-formed.
    drop(release);
}
