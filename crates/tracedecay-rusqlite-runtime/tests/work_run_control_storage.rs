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
    ProjectId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkAuthority, WorkBlockedIntervalCauseV1, WorkBlockedIntervalClosureV1,
    WorkBlockedIntervalIdentityV1, WorkBlockedIntervalReceiptV1, WorkCancellationStateV1,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionEnvelopeV1,
    WorkExecutionLimits, WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology,
    WorkFenceEpochV1, WorkFilesystemPolicy, WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkProductEventSequenceV1, WorkProductSourceWatermarkV1, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkRunControlAuthorityV1, WorkRunControlReasonV1, WorkRunControlStateV1, WorkRunControlV1,
    WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkflowOperationRef, WorkflowStepId, WorktreeId,
};
use tracedecay_rusqlite_runtime::workflow::install_workflow_schema;

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

fn projection_binding() -> WorkAttemptProjectionBindingV1 {
    WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        WorkProductSourceWatermarkV1::new(Default::default()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.run-control.storage"),
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
    let binding = projection_binding();
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
    let binding = projection_binding();
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
    // An attempt is admitted `Leased` and the domain transition graph only
    // reaches a terminal state through `Running`, so the durable terminal row
    // is produced exactly the way the runtime produces it. Going straight from
    // `Leased` to `Succeeded` is an `InvalidAttemptTransition`.
    attempt
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route()),
            None,
            attempt.lease().clone(),
        )
        .unwrap()
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

fn blocked_interval(attempt_id: &str, started_at: UtcMicros) -> WorkBlockedIntervalReceiptV1 {
    WorkBlockedIntervalReceiptV1::opened(
        WorkBlockedIntervalIdentityV1::new(
            task(),
            run(),
            id::<AttemptId>(attempt_id),
            id::<WorkflowStepId>("step.run-control.storage"),
        ),
        WorkBlockedIntervalCauseV1::new(
            WorkRunControlReasonV1::HumanWait,
            WorkRunControlAuthorityV1::new(2).unwrap(),
        ),
        started_at,
    )
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
    let store = RegisteredWorkStore::start_with_setup("run-control-admission", |connection| {
        install_workflow_schema(connection).unwrap();
    });
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
    // Journal binding is pause-only evidence. It must not promote a terminal
    // attempt into a new blocked interval during a later run-control pause.
    let bindings = store
        .storage()
        .workflow_bound_live_attempts(&authority, &task(), &run())
        .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].attempt_id, id::<AttemptId>("attempt.rc.1"));
    assert_eq!(bindings[0].step_id, None);

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
fn publication_refuses_an_attempt_frontier_changed_after_snapshot() {
    let store = RegisteredWorkStore::start("run-control-frontier-race");
    let authority = authority("actor.run-control.frontier-race");
    let first = attempt("attempt.frontier-race.1");
    store.storage().insert(&authority, &first).unwrap();
    let frontier = store
        .storage()
        .run_control_frontier(&authority, &task(), &run())
        .unwrap()
        .expect("frontier");

    // This admission lands after pause prepared its frontier. The storage CAS
    // must observe it in the same write transaction as publication.
    let racing = attempt("attempt.frontier-race.2");
    store.storage().insert(&authority, &racing).unwrap();
    let control = paused(
        WorkRunControlReasonV1::OperatorRequest,
        UtcMicros(400),
        frontier.admission.live_attempts.clone(),
    );
    assert_eq!(
        store
            .storage()
            .publish_run_control_at_frontier(&authority, &frontier, &control, &[])
            .expect_err("changed attempt frontier"),
        WorkRunControlStorageError::AuthorityConflict
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
        .publish_run_control(&authority, None, &control, &[])
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
            .publish_run_control(&authority, None, &control, &[])
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
        .publish_run_control(&authority, None, &paused_control, &[])
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
                &[],
            )
            .expect_err("stale authority version"),
        WorkRunControlStorageError::AuthorityConflict
    );
    // The exact version that is published swaps successfully.
    store
        .storage()
        .publish_run_control(&authority, Some(paused_control.authority()), &resumed, &[])
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
        .publish_run_control(&mine, None, &control, &[])
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

#[test]
fn settled_blocked_intervals_are_revisioned_isolated_and_replayed_after_restart() {
    let store = RegisteredWorkStore::start("run-control-blocked-interval-replay");
    let mine = authority("actor.run-control.blocked.mine");
    let peer = authority("actor.run-control.blocked.peer");
    let paused_control = paused(
        WorkRunControlReasonV1::HumanWait,
        UtcMicros(400),
        vec![id::<AttemptId>("attempt.blocked")],
    );
    let opened = blocked_interval("attempt.blocked", UtcMicros(400));
    store
        .storage()
        .publish_run_control(&mine, None, &paused_control, std::slice::from_ref(&opened))
        .unwrap();
    assert_eq!(
        store
            .storage()
            .open_blocked_intervals(&mine, &task(), &run())
            .unwrap(),
        vec![opened.clone()]
    );

    let resumed = paused_control
        .resume(WorkRunControlReasonV1::OperatorRequest, UtcMicros(800))
        .unwrap();
    let settled = opened
        .close(
            UtcMicros(800),
            WorkBlockedIntervalClosureV1::Resumed {
                reason: WorkRunControlReasonV1::OperatorRequest,
                authority: resumed.authority(),
            },
        )
        .unwrap();
    store
        .storage()
        .publish_run_control(
            &mine,
            Some(paused_control.authority()),
            &resumed,
            std::slice::from_ref(&settled),
        )
        .unwrap();
    assert_eq!(settled.interval_revision(), 2);
    assert!(
        store
            .storage()
            .open_blocked_intervals(&mine, &task(), &run())
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .storage()
            .next_settled_blocked_intervals_for_observation(&peer, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .storage()
            .next_settled_blocked_intervals_for_observation(&mine, 1)
            .unwrap(),
        vec![settled.clone()]
    );

    // Queue admission is not delivery. The durable cursor schedules bounded
    // cyclic replay, so a producer crash after `try_emit` remains recoverable
    // after reopening the registered store.
    let restarted = store.restart("run-control-blocked-interval-replay");
    assert_eq!(
        restarted
            .storage()
            .next_settled_blocked_intervals_for_observation(&mine, 1)
            .unwrap(),
        vec![settled.clone()]
    );
    // Once the retained path has a durable producer claim, its exact receipt
    // CAS removes only that revision from future scans.
    restarted
        .storage()
        .mark_settled_blocked_interval_durable(&mine, &settled)
        .unwrap();
    assert!(
        restarted
            .storage()
            .next_settled_blocked_intervals_for_observation(&mine, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn terminal_attempt_closes_its_open_blocked_interval_in_the_same_fenced_cas() {
    let store = RegisteredWorkStore::start("run-control-blocked-interval-terminal");
    let authority = authority("actor.run-control.blocked.terminal");
    let attempt = attempt("attempt.blocked.terminal");
    store.storage().insert(&authority, &attempt).unwrap();

    let paused_control = paused(
        WorkRunControlReasonV1::HumanWait,
        UtcMicros(400),
        vec![attempt.identity().attempt_id().clone()],
    );
    let opened = blocked_interval("attempt.blocked.terminal", UtcMicros(400));
    store
        .storage()
        .publish_run_control(
            &authority,
            None,
            &paused_control,
            std::slice::from_ref(&opened),
        )
        .unwrap();

    let terminal = succeeded(&attempt);
    store
        .storage()
        .update(
            &authority,
            attempt.lease(),
            attempt.state(),
            &terminal,
            None,
        )
        .unwrap();

    let settled = opened
        .close(
            UtcMicros(500),
            WorkBlockedIntervalClosureV1::AttemptTerminal,
        )
        .unwrap();
    assert!(
        store
            .storage()
            .open_blocked_intervals(&authority, &task(), &run())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .storage()
            .next_settled_blocked_intervals_for_observation(&authority, 1)
            .unwrap(),
        vec![settled.clone()]
    );

    let restarted = store.restart("run-control-blocked-interval-terminal");
    assert_eq!(
        restarted
            .storage()
            .next_settled_blocked_intervals_for_observation(&authority, 1)
            .unwrap(),
        vec![settled]
    );
}
