use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, CancellationContext, CapabilityGrantSnapshot,
    CreateWorkCommand, Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
    ReviewProposalCommand, WorkService,
};
use tracedecay_domain::{
    ActorId, AttemptId, ManifestDigest, ProjectId, ProjectionGenerationId, ProposalId,
    RepositoryId, RunId, TaskId, WorkCancellationRequestId, WorkFenceEpochV1, WorkLeaseId,
    WorkProjectionCoverageV1, WorkProjectionSequenceV1, WorkVersion, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::codex_provider::CODEX_PROVIDER_ID;
use super::*;
use crate::application::event_lane;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(project_id: ProjectId) -> RequestContext {
    let scope = ResolvedScope::new(
        project_id,
        id::<RepositoryId>("repository.work.daemon"),
        id::<WorktreeId>("worktree.work.daemon"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.daemon"),
        1,
        digest('a'),
        id::<ActorId>("actor.work.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.daemon").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.daemon").unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id("actor.work.daemon"),
        scope,
        grant,
        RequestId::new("request.work.daemon").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.work.daemon").unwrap(),
    )
    .unwrap()
}

fn authority(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap()
}

fn lease(epoch: u64) -> WorkLeaseFenceV1 {
    WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.work.daemon"),
        WorkFenceEpochV1::new(epoch).unwrap(),
    )
    .unwrap()
}

fn identity(task_id: &TaskId, suffix: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>(&format!("run.work.daemon.{suffix}")),
        id::<AttemptId>(&format!("attempt.work.daemon.{suffix}")),
    )
    .unwrap()
}

fn cancellation_request(suffix: &str, at: i64) -> WorkCancellationRequestV1 {
    WorkCancellationRequestV1::new(
        id::<WorkCancellationRequestId>(&format!("cancel.work.daemon.{suffix}")),
        UtcMicros(at),
    )
    .unwrap()
}

fn install_codex_fixture(path: &Path) {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    method = message.get("method")
    if request_id == 0:
        print(json.dumps({"jsonrpc": "2.0", "id": 0, "result": {}}), flush=True)
    elif request_id == 1:
        print(json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"thread": {"id": "thread.work.fixture", "model": "codex-work-fixture"}}}), flush=True)
    elif request_id == 2 and method == "turn/start":
        print(json.dumps({"method": "item/completed", "params": {"model": "codex-work-fixture", "item": {"content": [{"type": "output_text", "text": "fixture work completed"}]}}}), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
"#,
    )
    .unwrap();
    make_executable(path);
}

fn install_stubborn_codex_fixture(path: &Path, descendant_pid_path: &Path) {
    fs::write(
        path,
        format!(
            r#"#!/usr/bin/env python3
import json
import subprocess
import sys
import time

descendant = subprocess.Popen([
    sys.executable,
    "-c",
    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)",
])
with open({pid_path:?}, "w", encoding="utf-8") as handle:
    handle.write(str(descendant.pid))

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    if request_id == 0:
        print(json.dumps({{"jsonrpc": "2.0", "id": 0, "result": {{}}}}), flush=True)
    elif request_id == 1:
        print(json.dumps({{"jsonrpc": "2.0", "id": 1, "result": {{"thread": {{"id": "thread.work.stubborn"}}}}}}), flush=True)
    elif request_id == 2:
        while True:
            time.sleep(1)
"#,
            pid_path = descendant_pid_path.to_string_lossy(),
        ),
    )
    .unwrap();
    make_executable(path);
}

/// A fixture that never answers the initialize handshake, so its execution
/// stays in flight until the queue cancels or reaps it.
fn install_idle_codex_fixture(path: &Path) {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
import time

while True:
    time.sleep(1)
"#,
    )
    .unwrap();
    make_executable(path);
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn prepare_work(
    storage: &WorkSqliteStorage,
    context: &RequestContext,
) -> (TaskId, WorkProjectionSnapshotV1) {
    let service = WorkService::new(storage.clone());
    let task_id = id::<TaskId>("task.work.daemon");
    service
        .create(
            context,
            CreateWorkCommand {
                task_id: task_id.clone(),
                title: "Run the daemon Codex fixture".to_owned(),
                dependencies: BTreeSet::new(),
                command_id: id("command.work.daemon.create"),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
    service
        .accept_proposal(
            context,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: id::<ProposalId>("proposal.work.daemon"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.work.daemon.proposal"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();
    service
        .admit_execution(
            context,
            AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.work.daemon.admit"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    let projection = service.load(context, &task_id).unwrap();
    let snapshot = WorkProjectionSnapshotV1::new(
        id::<ProjectionGenerationId>("generation.work.daemon"),
        WorkProjectionSequenceV1::new(3),
        vec![projection],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    (task_id, snapshot)
}

struct Harness {
    _project: tempfile::TempDir,
    _host: crate::application::host_admission::HostAdmissionTestRuntimeV1,
    project_root: std::path::PathBuf,
    storage: WorkSqliteStorage,
    observation_db: Arc<RegisteredGlobalDb>,
    authority: WorkAuthority,
    task_id: TaskId,
    snapshot: WorkProjectionSnapshotV1,
}

impl Harness {
    async fn open(project_id: &str) -> Self {
        let project = tempfile::tempdir().unwrap();
        let project_id = id::<ProjectId>(project_id);
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            project.path(),
            project_id.clone(),
        )
        .await
        .unwrap();
        let observation_db = host.project_observation_database_arc_for_test().unwrap();
        let storage = observation_db.work_storage().unwrap();
        let context = context(project_id);
        let authority = authority(&context);
        let (task_id, snapshot) = prepare_work(&storage, &context);
        Self {
            project_root: project.path().to_path_buf(),
            _project: project,
            _host: host,
            storage,
            observation_db,
            authority,
            task_id,
            snapshot,
        }
    }

    fn runtime(
        &self,
        codex_bin: &Path,
        timeout: Duration,
        capacity: usize,
    ) -> DaemonWorkRuntimeV1<WorkSqliteStorage> {
        DaemonWorkRuntimeV1::with_capacity(
            self.authority.clone(),
            self.storage.clone(),
            CodexAppServerSummaryConfig {
                codex_bin: codex_bin.to_string_lossy().into_owned(),
                model: None,
                timeout,
            },
            Arc::clone(&self.observation_db),
            self.project_root.clone(),
            NonZeroUsize::new(capacity).unwrap(),
        )
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.project_root.join(name)
    }
}

#[tokio::test]
async fn codex_runtime_covers_fence_terminal_cancel_resume_recovery_and_sse() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon").await;
    let fixture = harness.path("codex-work-fixture");
    install_codex_fixture(&fixture);
    let runtime = harness.runtime(&fixture, Duration::from_secs(5), 4);
    let task_id = harness.task_id.clone();
    let snapshot = harness.snapshot.clone();
    assert!(runtime.is_ready());
    assert_eq!(
        runtime.provider_route().unwrap().provider_id().as_str(),
        CODEX_PROVIDER_ID
    );
    let mut activity = event_lane::subscribe().unwrap();

    let successful_identity = identity(&task_id, "success");
    runtime
        .acquire_lease(&snapshot, successful_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(&successful_identity, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    let running = runtime
        .publish_progress(
            &successful_identity,
            &lease(1),
            WorkAttemptProgressV1::new(0, 1).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(running.state(), WorkAttemptStateV1::Running);
    assert_eq!(running.progress().unwrap().completed(), 0);
    let renewed = runtime
        .renew_lease(&successful_identity, &lease(1), lease(2))
        .unwrap();
    assert_eq!(renewed.lease(), &lease(2));
    let stale_terminal = WorkTerminalEvidenceV1::failed(digest('c'), UtcMicros(35)).unwrap();
    assert_eq!(
        runtime
            .terminalize(&successful_identity, &lease(1), stale_terminal)
            .await
            .unwrap_err(),
        WorkExecutionError::StaleLease
    );
    let completed = runtime
        .finish(&successful_identity, &lease(2), UtcMicros(40))
        .await
        .unwrap();
    assert_eq!(completed.state(), WorkAttemptStateV1::Succeeded);
    assert_eq!(completed.progress().unwrap().completed(), 1);
    assert_eq!(completed.artifacts().len(), 1);
    let replayed = runtime
        .terminalize(
            &successful_identity,
            &lease(2),
            completed.terminal().unwrap().clone(),
        )
        .await
        .unwrap();
    assert_eq!(replayed, completed);
    let pulse = tokio::time::timeout(Duration::from_secs(1), activity.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pulse.pulse.family, ActivityFamilyV1::Task);
    assert!(
        event_lane::replay_after(
            harness.observation_db.as_ref(),
            harness.authority.project_id().as_str(),
            None,
        )
        .await
        .is_some_and(|replay| !replay.records.is_empty()),
        "Work activity must be durably retained, not only broadcast"
    );

    let cancelled_identity = identity(&task_id, "cancelled");
    runtime
        .acquire_lease(&snapshot, cancelled_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(&cancelled_identity, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    let cancellation = cancellation_request("request", 50);
    let cancelled = runtime
        .cancel(&cancelled_identity, &lease(1), cancellation.clone())
        .await
        .unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(matches!(
        cancelled.cancellation(),
        WorkCancellationStateV1::Acknowledged(_)
    ));
    assert_eq!(
        runtime
            .cancel(&cancelled_identity, &lease(1), cancellation)
            .await
            .unwrap(),
        cancelled
    );

    let resumed_identity = identity(&task_id, "resumed");
    runtime
        .acquire_lease(&snapshot, resumed_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(
            &resumed_identity,
            &lease(1),
            WorkRecoveryStateV1::Resumed {
                source_attempt_id: cancelled_identity.attempt_id().clone(),
                checkpoint: None,
            },
        )
        .await
        .unwrap();
    let resumed = runtime
        .finish(&resumed_identity, &lease(1), UtcMicros(70))
        .await
        .unwrap();
    assert!(matches!(
        resumed.recovery(),
        WorkRecoveryStateV1::Resumed { .. }
    ));

    let restarted_identity = identity(&task_id, "restarted");
    runtime
        .acquire_lease(&snapshot, restarted_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(
            &restarted_identity,
            &lease(1),
            WorkRecoveryStateV1::Restarted {
                source_attempt_id: resumed_identity.attempt_id().clone(),
                reason: WorkRestartReasonV1::ProcessLost,
            },
        )
        .await
        .unwrap();
    let restarted = runtime
        .finish(&restarted_identity, &lease(1), UtcMicros(80))
        .await
        .unwrap();
    assert!(matches!(
        restarted.recovery(),
        WorkRecoveryStateV1::Restarted { .. }
    ));

    let recovery_identity = identity(&task_id, "recovery");
    runtime
        .acquire_lease(&snapshot, recovery_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(
            &recovery_identity,
            &lease(1),
            WorkRecoveryStateV1::Restarted {
                source_attempt_id: restarted_identity.attempt_id().clone(),
                reason: WorkRestartReasonV1::ProcessLost,
            },
        )
        .await
        .unwrap();
    let recovery = runtime
        .recover(
            &recovery_identity,
            &lease(1),
            WorkRestartReasonV1::ProviderUnavailable,
        )
        .await
        .unwrap();
    assert_eq!(recovery.state(), WorkAttemptStateV1::RecoveryRequired);
    assert_eq!(
        recovery.recovery().source_attempt_id(),
        Some(restarted_identity.attempt_id()),
        "recovery must name the predecessor it resumes from"
    );
    assert_eq!(
        runtime.attempt(&recovery_identity).unwrap().unwrap(),
        recovery
    );
    assert_eq!(
        runtime.in_flight(),
        0,
        "every settled attempt must release its execution slot"
    );
    assert!(
        harness
            .storage
            .execution_attempt_history(&harness.authority, &successful_identity)
            .unwrap()
            .len()
            >= 6
    );
}

#[tokio::test]
async fn codex_cancel_terminates_and_reaps_stubborn_process_tree() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.cancel").await;
    let fixture = harness.path("codex-work-stubborn-fixture");
    let descendant_pid_path = harness.path("codex-work-descendant.pid");
    install_stubborn_codex_fixture(&fixture, &descendant_pid_path);
    let runtime = harness.runtime(&fixture, Duration::from_secs(2), 4);
    let attempt_identity = identity(&harness.task_id, "stubborn-cancel");
    runtime
        .acquire_lease(&harness.snapshot, attempt_identity.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(&attempt_identity, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    let descendant_pid = await_descendant_pid(&descendant_pid_path).await;

    let cancelled = runtime
        .cancel(
            &attempt_identity,
            &lease(1),
            cancellation_request("stubborn", 50),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(matches!(
        cancelled.cancellation(),
        WorkCancellationStateV1::Acknowledged(_)
    ));
    assert_eq!(runtime.in_flight(), 0);
    assert!(
        !process_is_alive(descendant_pid).await,
        "Codex Work cancellation must leave no provider descendant alive"
    );
}

/// The queue bound, not the host, decides how many providers run at once.
#[tokio::test]
async fn saturated_work_queue_refuses_new_executions_and_keeps_the_durable_intent() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.saturated").await;
    let fixture = harness.path("codex-work-idle-fixture");
    install_idle_codex_fixture(&fixture);
    let runtime = harness.runtime(&fixture, Duration::from_secs(30), 1);

    let occupying = identity(&harness.task_id, "saturating");
    runtime
        .acquire_lease(&harness.snapshot, occupying.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(&occupying, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    assert_eq!(runtime.in_flight(), 1);

    let refused = identity(&harness.task_id, "refused");
    runtime
        .acquire_lease(&harness.snapshot, refused.clone(), lease(1))
        .await
        .unwrap();
    let error = runtime
        .start(&refused, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            WorkExecutionError::Provider(WorkProviderExecutionError::Unavailable(message))
                if message.contains("saturated at 1")
        ),
        "backpressure must be reported, not silently queued: {error}"
    );
    assert_eq!(
        runtime.attempt(&refused).unwrap().unwrap().state(),
        WorkAttemptStateV1::Running,
        "the durable running intent must survive a refused admission"
    );
    assert_eq!(runtime.in_flight(), 1);

    runtime
        .cancel(
            &occupying,
            &lease(1),
            cancellation_request("saturating", 60),
        )
        .await
        .unwrap();
    assert_eq!(runtime.in_flight(), 0);

    runtime
        .start(&refused, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    assert_eq!(runtime.in_flight(), 1);
    assert_eq!(runtime.shutdown(), 1);
    assert_eq!(runtime.in_flight(), 0);
}

/// A rejected durable transition must never leave a provider running.
#[tokio::test]
async fn a_rejected_transition_starts_no_provider_execution() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.rejected").await;
    let fixture = harness.path("codex-work-idle-fixture");
    install_idle_codex_fixture(&fixture);
    let runtime = harness.runtime(&fixture, Duration::from_secs(30), 2);

    let fenced = identity(&harness.task_id, "fenced");
    runtime
        .acquire_lease(&harness.snapshot, fenced.clone(), lease(2))
        .await
        .unwrap();

    assert_eq!(
        runtime
            .start(&fenced, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap_err(),
        WorkExecutionError::StaleLease
    );
    assert_eq!(runtime.in_flight(), 0);
    assert_eq!(
        runtime.attempt(&fenced).unwrap().unwrap().state(),
        WorkAttemptStateV1::Leased
    );

    let missing = identity(&harness.task_id, "missing");
    assert_eq!(
        runtime
            .start(&missing, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap_err(),
        WorkExecutionError::NotFound
    );
    assert_eq!(runtime.in_flight(), 0);
}

/// After a restart the durable record is the only execution authority.
#[tokio::test]
async fn a_restarted_runtime_replays_terminals_and_never_invents_success() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.restart").await;
    let fixture = harness.path("codex-work-idle-fixture");
    install_idle_codex_fixture(&fixture);
    let before = harness.runtime(&fixture, Duration::from_secs(30), 2);
    let restarted = harness.runtime(&fixture, Duration::from_secs(30), 2);

    let orphaned = identity(&harness.task_id, "orphaned");
    before
        .acquire_lease(&harness.snapshot, orphaned.clone(), lease(1))
        .await
        .unwrap();
    before
        .start(&orphaned, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();

    let error = restarted
        .finish(&orphaned, &lease(1), UtcMicros(90))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            WorkExecutionError::Provider(WorkProviderExecutionError::Unavailable(message))
                if message.contains("not owned by this process")
        ),
        "a runtime that owns no execution must not report an outcome: {error}"
    );
    assert_eq!(
        restarted.attempt(&orphaned).unwrap().unwrap().state(),
        WorkAttemptStateV1::Running
    );

    let cancelled = before
        .cancel(&orphaned, &lease(1), cancellation_request("restart", 95))
        .await
        .unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert_eq!(
        restarted
            .finish(&orphaned, &lease(1), UtcMicros(120))
            .await
            .unwrap(),
        cancelled,
        "the durable terminal must be replayed exactly, not re-derived"
    );
    assert_eq!(before.in_flight(), 0);
    assert_eq!(restarted.shutdown(), 0);
}

/// A provider that finished before it observed the stop request must still
/// terminate as cancelled: the recorded intent admits no other terminal, and an
/// attempt must never be stranded in `CancellationRequested`.
#[tokio::test]
async fn cancelling_a_provider_that_already_completed_still_terminates_as_cancelled() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.raced-cancel").await;
    let fixture = harness.path("codex-work-fixture");
    install_codex_fixture(&fixture);
    let runtime = harness.runtime(&fixture, Duration::from_secs(5), 2);

    let raced = identity(&harness.task_id, "raced-cancel");
    runtime
        .acquire_lease(&harness.snapshot, raced.clone(), lease(1))
        .await
        .unwrap();
    runtime
        .start(&raced, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();
    // Let the fixture run to completion so the settlement is `Completed` while
    // the durable state will say cancellation was requested.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cancelled = runtime
        .cancel(&raced, &lease(1), cancellation_request("raced", 50))
        .await
        .unwrap();

    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(matches!(
        cancelled.cancellation(),
        WorkCancellationStateV1::Acknowledged(_)
    ));
    assert_eq!(runtime.in_flight(), 0);
}

/// A recorded cancellation is answered from durable state, so an attempt whose
/// execution this process does not own must still reach its cancelled terminal.
///
/// Requiring a claimable settlement first stranded the attempt permanently: the
/// transition table lets `CancellationRequested` reach only an acknowledged or
/// escalated cancellation, and no exposed operation could supply either.
#[tokio::test]
async fn a_cancellation_resolves_without_an_execution_this_process_owns() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.foreign-cancel").await;
    let fixture = harness.path("codex-work-idle-fixture");
    install_idle_codex_fixture(&fixture);
    let owner = harness.runtime(&fixture, Duration::from_secs(30), 2);
    let restarted = harness.runtime(&fixture, Duration::from_secs(30), 2);

    let stranded = identity(&harness.task_id, "foreign-cancel");
    owner
        .acquire_lease(&harness.snapshot, stranded.clone(), lease(1))
        .await
        .unwrap();
    owner
        .start(&stranded, &lease(1), WorkRecoveryStateV1::Fresh)
        .await
        .unwrap();

    // `restarted` never admitted this execution, so it has no settlement to
    // claim — exactly the state a daemon restart or an expiry reap leaves.
    assert_eq!(restarted.in_flight(), 0);
    let cancelled = restarted
        .cancel(&stranded, &lease(1), cancellation_request("foreign", 60))
        .await
        .unwrap();

    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(matches!(
        cancelled.cancellation(),
        WorkCancellationStateV1::Acknowledged(_)
    ));
    assert_eq!(
        owner.shutdown(),
        1,
        "the owning runtime still reaps the execution it admitted"
    );
}

/// The exposed terminal path must release the execution slot, or the bound
/// becomes a standing refusal after a few completed attempts.
#[tokio::test]
async fn terminalizing_an_attempt_releases_its_execution_slot() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let harness = Harness::open("project.work.daemon.terminalize").await;
    let fixture = harness.path("codex-work-idle-fixture");
    install_idle_codex_fixture(&fixture);
    let runtime = harness.runtime(&fixture, Duration::from_secs(30), 1);

    for suffix in ["one", "two", "three"] {
        let attempt = identity(&harness.task_id, suffix);
        runtime
            .acquire_lease(&harness.snapshot, attempt.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(&attempt, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap();
        assert_eq!(runtime.in_flight(), 1);
        let completed = runtime
            .terminalize(
                &attempt,
                &lease(1),
                WorkTerminalEvidenceV1::failed(digest('d'), UtcMicros(60)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(completed.state(), WorkAttemptStateV1::Failed);
        assert_eq!(
            runtime.in_flight(),
            0,
            "a terminal attempt must not keep holding a slot in the bound"
        );
    }
}

async fn await_descendant_pid(path: &Path) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !path.is_file() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap()
}

async fn process_is_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unsafe { kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    unsafe { kill(pid, 0) == 0 }
}
