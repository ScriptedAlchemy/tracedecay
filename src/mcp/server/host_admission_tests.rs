use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;

use super::writer_test_support::{init_indexed_repo, registered_context, registered_runtime};
use super::{
    HookBranchWriteRequest, HookBranchWriteResult, HookBranchWriter, McpServer,
    McpServerConstructionContext,
};
use crate::application::host_admission::{
    HostAdmissionBroker, HostAdmissionOutcome, HostAdmissionRuntime, HostAdmissionScope,
    HostAdmissionStatus, HostAdmissionTestRuntimeV1, SharedHostAdmissionBroker, SpoolBounds,
};
use crate::daemon::{DaemonHookEvent, HookAgent, HookRouteMetadata, HookTerminalReceipt};
use crate::errors::TraceDecayError;
use crate::mcp::project_route::HookProjectRouteCache;
use crate::sessions::git_correlation::{CommitRelationFilter, GitRefFilter, SessionsForQuery};
use crate::sessions::{SessionMessageRecord, SessionRecord};

fn session_start(root: PathBuf) -> Value {
    serde_json::to_value(DaemonHookEvent::session_start(HookAgent::Codex, root)).unwrap()
}

fn terminal_receipt(root: PathBuf) -> Value {
    serde_json::to_value(DaemonHookEvent::hermes_terminal_receipt(
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

async fn server_with_broker(
    cg: crate::tracedecay::TraceDecay,
    broker: SharedHostAdmissionBroker,
    writer: HookBranchWriter,
) -> Arc<McpServer> {
    let context = with_broker(registered_context(cg).await, broker, writer);
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

async fn server_with_owned_project_replay_worker(
    cg: crate::tracedecay::TraceDecay,
    broker: SharedHostAdmissionBroker,
    writer: HookBranchWriter,
) -> Arc<McpServer> {
    let context = with_broker(registered_context(cg).await, broker, writer)
        .with_owned_project_host_admission_replay();
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

fn with_broker(
    context: McpServerConstructionContext,
    broker: SharedHostAdmissionBroker,
    writer: HookBranchWriter,
) -> McpServerConstructionContext {
    let mut context = context.with_hook_branch_writer(writer);
    context.host_admission_broker = Some(broker);
    context
}

fn add_branch_payload() -> Vec<u8> {
    add_branch_payload_for("main")
}

fn add_branch_payload_for(branch: &str) -> Vec<u8> {
    crate::mcp::hook_events::encode_durable_hook_event_plan(
        &crate::mcp::hook_events::HookEventPlan::AddBranch(branch.to_string()),
    )
    .expect("add_branch plan should encode")
}

fn sync_current_branch_payload(branch: &str) -> Vec<u8> {
    crate::mcp::hook_events::encode_durable_hook_event_plan(
        &crate::mcp::hook_events::HookEventPlan::SyncCurrentBranch {
            branch: branch.to_string(),
            agent: HookAgent::Codex,
        },
    )
    .expect("sync_current_branch plan should encode")
}

fn success_writer() -> HookBranchWriter {
    Arc::new(|_request| {
        Box::pin(async {
            Ok(HookBranchWriteResult {
                branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                refresh_file_token_map: false,
            })
        })
    })
}

#[tokio::test]
async fn hook_event_is_durable_before_attempt_and_retained_on_failure() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted_after_append = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let broker = Arc::clone(&broker);
        let attempted_after_append = Arc::clone(&attempted_after_append);
        Arc::new(move |_request: HookBranchWriteRequest| {
            let broker = Arc::clone(&broker);
            let attempted_after_append = Arc::clone(&attempted_after_append);
            Box::pin(async move {
                assert_eq!(broker.pending_count().await, 1);
                *attempted_after_append.lock().unwrap() = true;
                Err(TraceDecayError::Config {
                    message: "injected canonical admission failure".to_string(),
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("canonical_admission_failed"));
    assert!(*attempted_after_append.lock().unwrap());
    assert_eq!(broker.pending_count().await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn commit_before_ack_replays_once_and_acknowledges_exact_duplicate() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let authoritative_commit = Arc::new(Mutex::new(false));
    let failing_writer: HookBranchWriter = {
        let authoritative_commit = Arc::clone(&authoritative_commit);
        Arc::new(move |_request| {
            let authoritative_commit = Arc::clone(&authoritative_commit);
            Box::pin(async move {
                *authoritative_commit.lock().unwrap() = true;
                Err(TraceDecayError::Config {
                    message: "injected failure after authoritative commit".to_string(),
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), failing_writer).await;
    let mut routes = HookProjectRouteCache::default();
    Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;
    assert!(*authoritative_commit.lock().unwrap());
    assert_eq!(broker.pending_count().await, 1);
    server.shutdown().await;
    drop(server);
    drop(broker);

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writes = Arc::new(Mutex::new(0usize));
    let duplicate_writer: HookBranchWriter = {
        let writes = Arc::clone(&writes);
        let authoritative_commit = Arc::clone(&authoritative_commit);
        Arc::new(move |_request| {
            let writes = Arc::clone(&writes);
            let authoritative_commit = Arc::clone(&authoritative_commit);
            Box::pin(async move {
                assert!(*authoritative_commit.lock().unwrap());
                *writes.lock().unwrap() += 1;
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), duplicate_writer).await;

    // The constructor schedules startup replay. This explicit pass joins the
    // same single-flight, so either ordering leaves one authoritative attempt
    // and an empty durable backlog.
    Box::pin(server.replay_host_admission(None)).await;
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(*writes.lock().unwrap(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn authoritative_commit_deletes_the_durable_hook_event() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writer: HookBranchWriter = Arc::new(|_request| {
        Box::pin(async {
            Ok(HookBranchWriteResult {
                branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                refresh_file_token_map: false,
            })
        })
    });
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&terminal_receipt(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Committed);
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_event_is_rejected_before_canonical_attempt() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::new(8, 128, 256, 4))
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
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
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("spool_record_too_large"));
    assert!(!*attempted.lock().unwrap());
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_semantic_payload_is_explicit_and_quarantined_across_reopen() {
    let (cg, project, authority) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
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
    let server = server_with_broker(cg, Arc::clone(&broker), Arc::clone(&writer)).await;
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
    assert!(!*attempted.lock().unwrap());
    server.shutdown().await;
    drop(server);
    drop(broker);

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), writer).await;

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;

    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    assert!(!*attempted.lock().unwrap());
    server.shutdown().await;
}

#[tokio::test]
async fn unsupported_payload_version_is_retryable_and_retained_across_reopen() {
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
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
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
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
    assert!(!*attempted.lock().unwrap());
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(broker.quarantine_count().await, 0);
    server.shutdown().await;
    drop(server);
    drop(broker);

    let recovered = Arc::new(HostAdmissionBroker::new(
        HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
            .unwrap()
            .0,
    ));
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
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let bounds = SpoolBounds::new(256, 128, 1024, 1).with_quarantine_limits(1024, 1);
    let runtime = HostAdmissionRuntime::open(spool.path(), bounds).unwrap().0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;

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
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let valid_payload = crate::mcp::hook_events::encode_durable_hook_event_plan(
        &crate::mcp::hook_events::HookEventPlan::AddBranch("main".to_string()),
    )
    .unwrap();
    let attempts = Arc::new(Mutex::new(0usize));
    let writer: HookBranchWriter = {
        let attempts = Arc::clone(&attempts);
        Arc::new(move |_request| {
            let attempts = Arc::clone(&attempts);
            Box::pin(async move {
                *attempts.lock().unwrap() += 1;
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
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
    let (cg, project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempts = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let writer: HookBranchWriter = {
        let attempts = Arc::clone(&attempts);
        let started = Arc::clone(&started);
        Arc::new(move |_request| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let started = Arc::clone(&started);
            Box::pin(async move {
                if attempt == 0 {
                    started.notify_one();
                    return std::future::pending::<
                        std::result::Result<HookBranchWriteResult, TraceDecayError>,
                    >()
                    .await;
                }
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
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
    crate::mcp::hook_events::encode_durable_hook_event_plan(
        &crate::mcp::hook_events::HookEventPlan::AddBranchAt {
            root,
            branch: branch.to_string(),
            agent: HookAgent::Codex,
        },
    )
    .expect("add_branch_at plan should encode")
}

fn linked_worktree_on(project: &std::path::Path) -> PathBuf {
    use super::writer_test_support::git;

    // Sibling of the unique TempDir root — never a shared /tmp fixed name.
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
    let (cg, project, _pin) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let payload = add_branch_at_payload(worktree.clone(), "feature/admission");

    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(Mutex::new(false));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
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
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let admitted = broker
        .admit("codex:add-branch-at-stale", &payload)
        .await
        .unwrap();

    // Adversarial remove/replace after admit, before effect/replay.
    std::fs::remove_dir_all(&worktree).expect("remove worktree");
    std::fs::create_dir_all(worktree.join("src")).expect("replacement dirs");
    std::fs::write(worktree.join("src/a.rs"), "pub fn replaced() {}\n").expect("write");
    super::writer_test_support::git(&worktree, &["init", "-q", "-b", "main"]);
    super::writer_test_support::git(&worktree, &["config", "user.email", "t@t.com"]);
    super::writer_test_support::git(&worktree, &["config", "user.name", "T"]);
    super::writer_test_support::git(&worktree, &["add", "."]);
    super::writer_test_support::git(&worktree, &["commit", "-q", "-m", "replacement"]);

    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
    assert_eq!(outcome.reason_code, Some("stale_branch_authorization"));
    assert!(!*attempted.lock().unwrap(), "stale root must not write");
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);

    // A second pass cannot reauthorize or write the terminal admission.
    let outcome = Box::pin(server.replay_host_admission(Some(admitted.seq))).await;
    assert_eq!(outcome.status, HostAdmissionStatus::AcceptedForReplay);
    assert!(!*attempted.lock().unwrap());
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_at_replay_rejects_stale_branch_after_switch() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let worktree = linked_worktree_on(project.path());
    let payload = add_branch_at_payload(worktree.clone(), "feature/admission");
    super::writer_test_support::git(&worktree, &["switch", "-c", "feature/other"]);

    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
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
    let (cg, project, _pin) = init_indexed_repo().await;
    let payload = add_branch_payload_for("main");

    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let admitted = broker
        .admit("codex:add-branch-stale-delayed", &payload)
        .await
        .unwrap();

    // Delayed switch after admit, before effect/replay.
    super::writer_test_support::git(project.path(), &["switch", "-c", "feature/other"]);

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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:add-branch-stale-restart", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    super::writer_test_support::git(project.path(), &["switch", "-c", "feature/restart"]);

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), writer).await;

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
    let (cg, project, _pin) = init_indexed_repo().await;
    let payload = sync_current_branch_payload("main");

    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: true,
                })
            })
        })
    };
    let server = server_with_broker(cg, Arc::clone(&broker), writer).await;
    let admitted = broker
        .admit("codex:sync-current-branch-stale-delayed", &payload)
        .await
        .unwrap();

    super::writer_test_support::git(project.path(), &["switch", "-c", "feature/sync-other"]);

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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:sync-current-branch-stale-restart", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    super::writer_test_support::git(project.path(), &["switch", "-c", "feature/sync-restart"]);

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: true,
                })
            })
        })
    };
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), writer).await;

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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:add-branch-at-common-dir", &payload)
        .await
        .unwrap();
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    let stranger = unique_sibling(project.path(), "stranger-repo");
    std::fs::create_dir_all(stranger.join("src")).expect("stranger");
    std::fs::write(stranger.join("src/a.rs"), "pub fn stranger() {}\n").expect("write");
    super::writer_test_support::git(&stranger, &["init", "-q", "-b", "main"]);
    super::writer_test_support::git(&stranger, &["config", "user.email", "t@t.com"]);
    super::writer_test_support::git(&stranger, &["config", "user.name", "T"]);
    super::writer_test_support::git(&stranger, &["add", "."]);
    super::writer_test_support::git(&stranger, &["commit", "-q", "-m", "stranger"]);
    let stranger_git = stranger.join(".git").canonicalize().expect("gitdir");
    let git_pointer = worktree.join(".git");
    assert!(git_pointer.is_file());
    std::fs::write(
        &git_pointer,
        format!("gitdir: {}\n", stranger_git.display()),
    )
    .expect("rewrite common-dir pointer");

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), writer).await;

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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:add-branch-at-symlink", &payload)
        .await
        .unwrap();
    drop(broker);

    let stranger = unique_sibling(project.path(), "symlink-stranger");
    std::fs::create_dir_all(stranger.join("src")).expect("stranger");
    std::fs::write(stranger.join("src/a.rs"), "pub fn stranger() {}\n").expect("write");
    super::writer_test_support::git(&stranger, &["init", "-q", "-b", "main"]);
    super::writer_test_support::git(&stranger, &["config", "user.email", "t@t.com"]);
    super::writer_test_support::git(&stranger, &["config", "user.name", "T"]);
    super::writer_test_support::git(&stranger, &["add", "."]);
    super::writer_test_support::git(&stranger, &["commit", "-q", "-m", "stranger"]);
    std::fs::remove_file(&alias).expect("remove alias");
    std::os::unix::fs::symlink(&stranger, &alias).expect("swap");

    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let attempted = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempted = Arc::clone(&attempted);
        Arc::new(move |_request| {
            let attempted = Arc::clone(&attempted);
            Box::pin(async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::Added,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let reopened = authority.reopen_project_graph(project.path()).await;
    let server = server_with_broker(reopened, Arc::clone(&broker), writer).await;

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

fn session_start_with_route(root: PathBuf) -> Value {
    serde_json::to_value(
        DaemonHookEvent::session_start(HookAgent::Codex, root.clone()).with_route(Some(
            HookRouteMetadata {
                session_id: Some("session-admission-test".to_string()),
                thread_id: Some("thread-admission-test".to_string()),
                cwd: Some(root.clone()),
                worktree: Some(root),
                branch: Some("main".to_string()),
            },
        )),
    )
    .unwrap()
}

async fn server_with_broker_and_runtime(
    cg: crate::tracedecay::TraceDecay,
    broker: SharedHostAdmissionBroker,
    writer: HookBranchWriter,
    runtime: HostAdmissionTestRuntimeV1,
) -> Arc<McpServer> {
    let context = with_broker(
        runtime
            .into_mcp_server_context_for_test(cg, None)
            .expect("registered MCP server context"),
        broker,
        writer,
    );
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

#[tokio::test]
async fn failed_admission_does_not_emit_hook_route_analytics() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let test_runtime = registered_runtime(&cg).await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writer: HookBranchWriter = Arc::new(|_request| {
        Box::pin(async {
            Err(TraceDecayError::Config {
                message: "injected canonical admission failure".to_string(),
            })
        })
    });
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), writer, test_runtime).await;
    let mut routes = HookProjectRouteCache::default();

    let outcome = Box::pin(server.handle_hook_event_notification(
        Some(&session_start_with_route(project.path().to_path_buf())),
        &mut routes,
    ))
    .await;

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    server.ledger_writes_settled().await;
    let rows = server
        .host_admission_test_runtime_for_test()
        .expect("host-admission test runtime")
        .query_profile_analytics_events_for_test(&crate::global_db::AnalyticsEventQuery {
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
    assert!(
        rows.is_empty(),
        "pre-commit/failed admission must not emit route analytics: {rows:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn durable_route_survives_unavailable_effect_for_same_connection_retry() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let test_runtime = registered_runtime(&cg).await;
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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let first_attempt = Arc::new(AtomicBool::new(true));
    let writer: HookBranchWriter = {
        let first_attempt = Arc::clone(&first_attempt);
        Arc::new(move |_request| {
            let first_attempt = Arc::clone(&first_attempt);
            Box::pin(async move {
                if first_attempt.swap(false, Ordering::SeqCst) {
                    return Err(TraceDecayError::Config {
                        message: "injected delayed effect".to_string(),
                    });
                }
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: false,
                })
            })
        })
    };
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), writer, test_runtime).await;
    let raw_session = ["AKIA", "SYNTHETIC", "CANARY", "3"].concat();
    let event = serde_json::to_value(
        DaemonHookEvent::session_start(HookAgent::Codex, project.path().to_path_buf()).with_route(
            Some(HookRouteMetadata {
                session_id: Some(raw_session.clone()),
                thread_id: None,
                cwd: Some(project.path().to_path_buf()),
                worktree: Some(project.path().to_path_buf()),
                branch: Some("main".to_string()),
            }),
        ),
    )
    .unwrap();
    let mut routes = HookProjectRouteCache::default();

    let first = Box::pin(server.handle_hook_event_notification(Some(&event), &mut routes)).await;
    assert_eq!(first.status, HostAdmissionStatus::Unavailable);
    assert_eq!(broker.pending_count().await, 1);

    let mut tool_arguments = json!({"query": "needle", "session_id": raw_session});
    crate::mcp::project_route::protect_tool_structural_ids(&mut tool_arguments)
        .expect("protect routed tool identities");
    let routed = routes.apply_to_tool_arguments("tracedecay_grep", tool_arguments);
    assert_eq!(
        routed["project_selector"]["path"], registered.canonical_root,
        "the same connection must route tools after durable append"
    );
    assert_eq!(
        routed["session_id"],
        crate::privacy::protect_sensitive_structural_id(
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
        .query_profile_analytics_events_for_test(&crate::global_db::AnalyticsEventQuery {
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
    assert!(
        server
            .wait_project_host_admission_replay_idle(Duration::from_secs(5))
            .await,
        "owned project replay worker should settle the retained admission"
    );
    assert_eq!(broker.pending_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn committed_admissions_emit_post_commit_private_route_analytics() {
    let (cg, project, _pin) = init_indexed_repo().await;
    let test_runtime = registered_runtime(&cg).await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let writer: HookBranchWriter = Arc::new(|_request| {
        Box::pin(async {
            Ok(HookBranchWriteResult {
                branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                refresh_file_token_map: false,
            })
        })
    });
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), writer, test_runtime).await;
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
        .query_profile_analytics_events_for_test(&crate::global_db::AnalyticsEventQuery {
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
    let (cg, project, _pin) = init_indexed_repo().await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let profile_root = crate::config::user_data_dir().expect("isolated profile root");
    let project_id = tracedecay_domain::ProjectId::new(
        cg.store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("project identity"),
    )
    .expect("typed project identity");
    let test_runtime =
        HostAdmissionTestRuntimeV1::project(&profile_root, project.path(), project_id.clone())
            .await
            .expect("registered host-admission runtime");
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
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let server =
        server_with_broker_and_runtime(cg, Arc::clone(&broker), success_writer(), test_runtime)
            .await;
    let raw = ["AKIA", "SYNTHETIC", "CANARY", "4"].concat();
    let protected = crate::privacy::protect_sensitive_structural_id(&raw).unwrap();
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
        .upsert_transcript_batch_for_test(
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
            &format!("host-admission-test-message:hermes:{protected}"),
            crate::global_db::ParseOffset::default(),
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
    let terminal = serde_json::to_value(DaemonHookEvent::hermes_terminal_receipt(
        project.path().to_path_buf(),
        route.clone(),
        receipt.clone(),
    ))
    .unwrap();
    let mut turn_ingested =
        DaemonHookEvent::hermes_terminal_receipt(project.path().to_path_buf(), route, receipt);
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
    let ready = crate::automation::host_receipts::oldest_ready(&dashboard_root)
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
    drop(server);
    let reopened = HostAdmissionTestRuntimeV1::project(&profile_root, project.path(), project_id)
        .await
        .expect("reopen registered host-admission runtime");
    let analytics = reopened
        .query_profile_analytics_events_for_test(&crate::global_db::AnalyticsEventQuery {
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
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    let payload = add_branch_payload();
    for index in 0..65 {
        broker
            .admit(&format!("codex:startup-{index}"), &payload)
            .await
            .unwrap();
    }
    assert_eq!(broker.pending_count().await, 65);

    let server =
        server_with_owned_project_replay_worker(cg, Arc::clone(&broker), success_writer()).await;

    assert!(
        server
            .wait_project_host_admission_replay_idle(Duration::from_secs(5))
            .await,
        "owned worker must drain a 65-record startup backlog across bounded passes"
    );
    assert_eq!(broker.pending_count().await, 0);
    assert!(
        server.project_host_admission_replay_pass_count().await >= 2,
        "65 records require more than one 64-record pass"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn owned_project_replay_worker_backoffs_on_retryable_failure() {
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:retryable", &add_branch_payload())
        .await
        .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let writer: HookBranchWriter = {
        let attempts = Arc::clone(&attempts);
        Arc::new(move |_request| {
            let attempts = Arc::clone(&attempts);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(TraceDecayError::Config {
                    message: "injected retryable canonical failure".to_string(),
                })
            })
        })
    };

    let server = server_with_owned_project_replay_worker(cg, Arc::clone(&broker), writer).await;

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
    let (cg, _project, _pin) = init_indexed_repo().await;
    let spool = TempDir::new().unwrap();
    let runtime = HostAdmissionRuntime::open(spool.path(), SpoolBounds::default())
        .unwrap()
        .0;
    let broker = Arc::new(HostAdmissionBroker::new(runtime));
    broker
        .admit("codex:hang", &add_branch_payload())
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let writer: HookBranchWriter = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Arc::new(move |_request| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Box::pin(async move {
                entered.notify_waiters();
                release.notified().await;
                Ok(HookBranchWriteResult {
                    branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
                    refresh_file_token_map: false,
                })
            })
        })
    };

    let server = server_with_owned_project_replay_worker(cg, Arc::clone(&broker), writer).await;
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("worker must enter an in-flight canonical attempt");

    tokio::time::timeout(Duration::from_secs(2), server.shutdown())
        .await
        .expect("shutdown must cancel and join the owned project replay worker");
    // Keep release alive so the aborted task drop path stays well-formed.
    drop(release);
}
