use std::collections::BTreeSet;

use tracedecay_application::work::RegisteredWorkApplicationServicesV1;
use tracedecay_contracts::{
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, PauseWorkRunCommand,
    RequestContext, RequestId, ResolvedScope, ResumeWorkRunCommand, WorkAttemptStoragePort,
    WorkRunControlReadingV1, WorkRunControlRequestV1,
};
use tracedecay_domain::test_fixtures::{digest, fixture_abs_root, id};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ProjectId,
    ProposalId, ProviderId, RepositoryId, RunId, TaskId, UtcMicros, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId, WorkProductEventSequenceV1,
    WorkProductSourceWatermarkV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkRecoveryStateV1, WorkRunControlReasonV1, WorkRunControlStateV1,
    WorkSandboxPolicy, WorkflowOperationRef, WorktreeId,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits;
use tracedecay_rusqlite_runtime::native_runtime::{
    NativeTask, NativeTaskOutcome, native_thread_writer_waits,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const PROJECT: &str = "project.run-control-jobs";
const REPOSITORY: &str = "repository.run-control-jobs";
const WORKTREE: &str = "worktree.run-control-jobs";
const ACTOR: &str = "actor.run-control-jobs";

pub(crate) fn task() -> TaskId {
    id::<TaskId>("task.run-control-jobs")
}

fn run() -> RunId {
    id::<RunId>("run.run-control-jobs")
}

pub(crate) fn context() -> RequestContext {
    context_for("pause_run")
}

/// A request context granted the `capability.work.{operation}` capability
/// and use case, under the same authority as [`context`].
pub(crate) fn context_for(operation: &str) -> RequestContext {
    context_for_actor(operation, ACTOR)
}

/// [`context_for`] issued to `actor` instead of the default one.
pub(crate) fn context_for_actor(operation: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(PROJECT),
        id::<RepositoryId>(REPOSITORY),
        id::<WorktreeId>(WORKTREE),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.run-control-jobs"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(100_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new(format!("capability.work.{operation}")).unwrap()]),
        BTreeSet::from([UseCaseId::new(format!("use-case.work.{operation}")).unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new("request.run-control-jobs").unwrap(),
        Deadline::new(UtcMicros(90_000)).unwrap(),
        CancellationContext::active("cancel.run-control-jobs").unwrap(),
    )
    .unwrap()
}

pub(crate) fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>(PROJECT),
        id::<RepositoryId>(REPOSITORY),
        id::<WorktreeId>(WORKTREE),
        id::<ActorId>(ACTOR),
        digest('a'),
    )
    .unwrap()
}

pub(crate) fn route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.run-control-jobs.claude-code.v1"),
    )
    .unwrap()
}

fn leased_attempt() -> WorkAttemptV1 {
    leased_attempt_of(run(), "run-control-jobs")
}

/// A leased attempt of `task()` in `run_id` whose attempt and lease ids end
/// in `suffix`.
pub(crate) fn leased_attempt_of(run_id: RunId, suffix: &str) -> WorkAttemptV1 {
    let identity = WorkAttemptIdentityV1::new(
        task(),
        run_id,
        id::<AttemptId>(&format!("attempt.{suffix}")),
    )
    .unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        WorkProductSourceWatermarkV1::new(Default::default()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.run-control-jobs"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.run-control-jobs.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>(PROJECT),
        id::<RepositoryId>(REPOSITORY),
        id::<WorktreeId>(WORKTREE),
        fixture_abs_root("/tmp/run-control-jobs"),
        None,
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    WorkAttemptV1::new(
        identity,
        binding,
        envelope,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>(&format!("lease.{suffix}")),
            WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        route(),
        None,
        None,
    )
    .unwrap()
}

/// The pinned execution snapshot every fixture attempt runs under.
pub(crate) fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.rcj.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.rcj.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.claude.code-cli".to_owned(),
            digest('e'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

#[tokio::test]
async fn pause_and_resume_awaited_on_a_native_worker_park_no_native_thread() {
    tracedecay_global_db::register_registered_schema_installer();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = RegisteredGlobalDbTestRuntime::project(
        profile.path(),
        project.path(),
        id::<ProjectId>(PROJECT),
    )
    .await
    .expect("registered project store opens");
    let database = runtime.project_database().expect("project database");
    database
        .work_storage()
        .expect("Work storage")
        .insert(&authority(), &leased_attempt())
        .expect("the run holds one leased attempt");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let writer_waits = native_thread_writer_waits();
    let reader_waits = native_thread_reader_waits();
    let native = NativeTask::spawn(async move {
        let paused = work
            .pause_run(
                context(),
                PauseWorkRunCommand {
                    task_id: task(),
                    run_id: run(),
                    reason: WorkRunControlReasonV1::OperatorRequest,
                    expected_authority_version: None,
                    occurred_at: UtcMicros(50_000),
                },
            )
            .await
            .map_err(|problem| format!("pause: {problem:?}"))?
            .control;
        let resumed = work
            .resume_run(
                context(),
                ResumeWorkRunCommand {
                    task_id: task(),
                    run_id: run(),
                    reason: WorkRunControlReasonV1::OperatorRequest,
                    expected_authority_version: paused.authority().get(),
                    occurred_at: UtcMicros(60_000),
                },
            )
            .await
            .map_err(|problem| format!("resume: {problem:?}"))?
            .control;
        Ok::<_, String>((paused.state(), resumed.state()))
    })
    .expect("native task spawns");
    let NativeTaskOutcome::Completed(outcome) = native.wait().await else {
        panic!("the pause and resume task completes");
    };
    assert_eq!(
        native_thread_writer_waits() - writer_waits,
        0,
        "pause and resume parked no native worker on the writer"
    );
    assert_eq!(
        native_thread_reader_waits() - reader_waits,
        0,
        "pause and resume parked no native worker on a reader"
    );
    let (paused_state, resumed_state) = outcome.expect("pause and resume commit");
    assert_eq!(paused_state, WorkRunControlStateV1::Paused);
    assert_eq!(resumed_state, WorkRunControlStateV1::Running);
    let final_state = RegisteredWorkApplicationServicesV1::attach(database)
        .expect("Work services")
        .run_control()
        .read(
            &context(),
            &WorkRunControlRequestV1 {
                task_id: task(),
                run_id: run(),
            },
        )
        .expect("the run control reads");
    assert!(
        matches!(&final_state, WorkRunControlReadingV1::Controlled { control, .. }
            if control.state() == WorkRunControlStateV1::Running),
        "the resume committed: {final_state:?}"
    );
}
