//! Durable Work run-control storage contract: run admission derived from
//! attempt rows, compare-and-swap publication of the monotonic control
//! authority, authority isolation, and restart durability over the registered
//! exact-SQL channel.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "One runtime, run control, and effect budget") requires one durable control
//! aggregate per run with "monotonically versioned authority" and a deadline
//! checkpoint whose remaining time "never increases". Both are storage
//! behaviours here: the version is the compare-and-swap key, and the deadline
//! the aggregate is first admitted under is read out of the attempt's own
//! pinned execution snapshot rather than supplied by a caller.

mod work_registered_store;

use std::collections::BTreeSet;

use tracedecay_application::{
    WorkAttemptStorageError, WorkAttemptStoragePort, WorkRunControlStorageError,
    WorkRunControlStoragePort,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProjectionGenerationId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1,
    WorkFilesystemPolicy, WorkLeaseFenceV1, WorkLeaseId, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkRunControlAuthorityV1, WorkRunControlReasonV1, WorkRunControlStateV1, WorkRunControlV1,
    WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkVersion, WorkflowOperationRef, WorktreeId,
};

use work_registered_store::RegisteredWorkStore;

const ADMITTED_DEADLINE: UtcMicros = UtcMicros(1_000_000);

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

fn authority(actor: &str) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.run-control.storage"),
        id::<RepositoryId>("repository.run-control.storage"),
        id::<WorktreeId>("worktree.run-control.storage"),
        id::<ActorId>(actor),
        digest('a'),
    )
    .unwrap()
}

fn task() -> TaskId {
    id::<TaskId>("task.run-control.storage")
}

fn run() -> RunId {
    id::<RunId>("run.run-control.storage")
}

fn route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.run-control.claude-code.v1"),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.rc.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.rc.1"),
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
        deadline: ADMITTED_DEADLINE,
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn attempt_for(task_id: TaskId, run_id: RunId, attempt_id: &str) -> WorkAttemptV1 {
    let identity =
        WorkAttemptIdentityV1::new(task_id, run_id, id::<AttemptId>(attempt_id)).unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.run-control.storage"),
        tracedecay_domain::WorkProjectionSequenceV1::new(7),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.run-control.storage"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.run-control.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>("project.run-control.storage"),
        id::<RepositoryId>("repository.run-control.storage"),
        id::<WorktreeId>("worktree.run-control.storage"),
        "/tmp/run-control-storage".to_owned(),
        Some(id::<RefId>("refs/heads/run-control-storage")),
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
            id::<WorkLeaseId>("lease.run-control.storage"),
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

fn attempt(attempt_id: &str) -> WorkAttemptV1 {
    attempt_for(task(), run(), attempt_id)
}

fn attempt_with_admission(
    attempt_id: &str,
    deadline: UtcMicros,
    topology: tracedecay_domain::WorkTopologyPolicyV1,
) -> WorkAttemptV1 {
    let identity = WorkAttemptIdentityV1::new(task(), run(), id::<AttemptId>(attempt_id)).unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.run-control.storage"),
        tracedecay_domain::WorkProjectionSequenceV1::new(7),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.run-control.storage"),
    )
    .unwrap();
    let execution = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.rc.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.rc.1"),
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
        deadline,
        fallback: WorkFallbackTopology::Disabled,
        topology,
    })
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.run-control.execute-provider"),
        execution,
        id::<ProjectId>("project.run-control.storage"),
        id::<RepositoryId>("repository.run-control.storage"),
        id::<WorktreeId>("worktree.run-control.storage"),
        "/tmp/run-control-storage".to_owned(),
        Some(id::<RefId>("refs/heads/run-control-storage")),
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
            id::<WorkLeaseId>("lease.run-control.storage"),
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

fn succeeded(attempt: &WorkAttemptV1) -> WorkAttemptV1 {
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('9'), UtcMicros(500)).unwrap();
    attempt
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route()),
            Some(terminal),
            attempt.lease().clone(),
        )
        .unwrap()
}

fn paused(
    reason: WorkRunControlReasonV1,
    at: UtcMicros,
    fenced: Vec<AttemptId>,
) -> WorkRunControlV1 {
    WorkRunControlV1::admitted(task(), run(), ADMITTED_DEADLINE, UtcMicros(0))
        .unwrap()
        .pause(reason, at, fenced)
        .unwrap()
}

#[test]
fn a_run_with_no_durable_attempt_has_no_admission_to_control() {
    let store = RegisteredWorkStore::start("run-control-absent");
    let authority = authority("actor.run-control.absent");
    // Absence is the answer, not an empty admission: a run nobody ever leased
    // an attempt for cannot be paused, and a fabricated deadline here would be
    // a way to buy budget.
    assert_eq!(
        store
            .storage()
            .run_admission(&authority, &task(), &run())
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .storage()
            .load_run_control(&authority, &task(), &run())
            .unwrap(),
        None
    );
}

#[test]
fn run_admission_reads_the_deadline_and_live_frontier_off_the_attempt_rows() {
    let store = RegisteredWorkStore::start("run-control-admission");
    let authority = authority("actor.run-control.admission");
    let live = attempt("attempt.rc.1");
    let done = attempt("attempt.rc.2");
    store.storage().insert(&authority, &live).unwrap();
    store.storage().insert(&authority, &done).unwrap();
    let finished = succeeded(&done);
    store
        .storage()
        .update(&authority, done.lease(), done.state(), &finished, None)
        .unwrap();

    let admission = store
        .storage()
        .run_admission(&authority, &task(), &run())
        .unwrap()
        .expect("the run holds durable attempts");
    // The deadline is the one the attempt was admitted under, verbatim.
    assert_eq!(admission.deadline, ADMITTED_DEADLINE);
    assert_eq!(admission.total_attempts, 2);
    // A terminal attempt is not part of the live frontier a pause fences.
    assert_eq!(
        admission.live_attempts,
        vec![id::<AttemptId>("attempt.rc.1")]
    );

    // A different run under the same authority is a separate admission.
    let other = attempt_for(task(), id::<RunId>("run.run-control.other"), "attempt.rc.9");
    store.storage().insert(&authority, &other).unwrap();
    assert_eq!(
        store
            .storage()
            .run_admission(&authority, &task(), &run())
            .unwrap()
            .expect("this run")
            .total_attempts,
        2
    );
}

#[test]
fn a_later_lexical_attempt_cannot_replace_the_run_admission() {
    let store = RegisteredWorkStore::start("run-control-first-admission");
    let authority = authority("actor.run-control.first-admission");
    let d1 = UtcMicros(1_000_000);
    let d2 = UtcMicros(2_000_000);
    let first_topology = tracedecay_domain::safe_work_topology_policy_v1();
    let mut conflicting_topology = first_topology.clone();
    conflicting_topology.notifications = tracedecay_domain::TopologyNotificationLevelV1::Verbose;

    let first = attempt_with_admission("attempt-2", d1, first_topology);
    store.storage().insert(&authority, &first).unwrap();
    assert_eq!(
        store
            .storage()
            .run_admission(&authority, &task(), &run())
            .unwrap()
            .expect("the first attempt admits the run")
            .deadline,
        d1
    );

    let conflicting = attempt_with_admission("attempt-10", d2, conflicting_topology);
    assert_eq!(
        store
            .storage()
            .insert(&authority, &conflicting)
            .expect_err("a later attempt with a different admission must conflict"),
        WorkAttemptStorageError::RunAdmissionConflict
    );
    // The first durable attempt remains unchanged after the rejected later
    // admission, so the caller cannot buy additional deadline or topology.
    assert_eq!(
        store
            .storage()
            .run_admission(&authority, &task(), &run())
            .unwrap()
            .expect("the first admission remains durable")
            .deadline,
        d1
    );
}

#[test]
fn the_first_publication_inserts_and_a_racing_first_publication_conflicts() {
    let store = RegisteredWorkStore::start("run-control-first");
    let authority = authority("actor.run-control.first");
    let control = paused(
        WorkRunControlReasonV1::OperatorRequest,
        UtcMicros(400),
        Vec::new(),
    );
    store
        .storage()
        .publish_run_control(&authority, None, &control)
        .unwrap();
    assert_eq!(
        store
            .storage()
            .load_run_control(&authority, &task(), &run())
            .unwrap(),
        Some(control.clone())
    );

    // A second writer that also believed nothing was published is refused
    // rather than allowed to overwrite the row it never read.
    assert_eq!(
        store
            .storage()
            .publish_run_control(&authority, None, &control)
            .expect_err("a racing first publication conflicts"),
        WorkRunControlStorageError::AuthorityConflict
    );
    assert_eq!(store.count("work_run_controls_v1"), 1);
}

#[test]
fn publication_is_a_compare_and_swap_on_the_monotonic_authority_version() {
    let store = RegisteredWorkStore::start("run-control-cas");
    let authority = authority("actor.run-control.cas");
    let paused_control = paused(
        WorkRunControlReasonV1::HumanWait,
        UtcMicros(400),
        Vec::new(),
    );
    store
        .storage()
        .publish_run_control(&authority, None, &paused_control)
        .unwrap();
    assert_eq!(paused_control.authority().get(), 2);

    let resumed = paused_control
        .resume(WorkRunControlReasonV1::OperatorRequest, UtcMicros(9_000))
        .unwrap();
    // A caller holding a stale version cannot publish over a newer one.
    assert_eq!(
        store
            .storage()
            .publish_run_control(
                &authority,
                Some(WorkRunControlAuthorityV1::new(1).unwrap()),
                &resumed,
            )
            .expect_err("stale authority version"),
        WorkRunControlStorageError::AuthorityConflict
    );
    // The exact version that is published swaps successfully.
    store
        .storage()
        .publish_run_control(&authority, Some(paused_control.authority()), &resumed)
        .unwrap();
    let stored = store
        .storage()
        .load_run_control(&authority, &task(), &run())
        .unwrap()
        .expect("published control");
    assert_eq!(stored.state(), WorkRunControlStateV1::Running);
    assert_eq!(stored.authority().get(), 3);
    // Resuming preserved the remaining budget rather than extending it.
    assert_eq!(
        stored.deadline().remaining_micros,
        ADMITTED_DEADLINE.0 - 400
    );
}

#[test]
fn control_rows_are_isolated_per_authority_and_survive_a_restart() {
    let store = RegisteredWorkStore::start("run-control-isolation");
    let mine = authority("actor.run-control.mine");
    let peer = authority("actor.run-control.peer");
    let control = paused(
        WorkRunControlReasonV1::Recovery,
        UtcMicros(400),
        vec![id::<AttemptId>("attempt.rc.1")],
    );
    store
        .storage()
        .publish_run_control(&mine, None, &control)
        .unwrap();

    // Another actor sees no control row at all — not a running one.
    assert_eq!(
        store
            .storage()
            .load_run_control(&peer, &task(), &run())
            .unwrap(),
        None
    );

    let restarted = store.restart("run-control-isolation");
    let recovered = restarted
        .storage()
        .load_run_control(&mine, &task(), &run())
        .unwrap()
        .expect("control survives a restart");
    assert_eq!(recovered, control);
    assert_eq!(recovered.state(), WorkRunControlStateV1::Paused);
    assert_eq!(
        recovered.fenced_attempts(),
        [id::<AttemptId>("attempt.rc.1")]
    );
}
