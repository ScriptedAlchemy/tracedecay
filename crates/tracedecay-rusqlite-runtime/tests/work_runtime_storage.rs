use std::collections::BTreeSet;

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, CancellationContext, CapabilityGrantSnapshot,
    CreateWorkCommand, Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
    ReviewProposalCommand, WorkAttemptAcquireLeaseRequestV1, WorkAttemptPersistencePort,
    WorkExecutionPersistenceError, WorkExecutionService, WorkService,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ManifestDigest, ProjectId, ProjectionGenerationId, ProposalId,
    ProviderId, RepositoryId, RunId, TaskId, UtcMicros, WorkArtifactId, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAttemptProgressV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationAcknowledgementV1,
    WorkCancellationRequestId, WorkCancellationRequestV1, WorkCancellationStateV1,
    WorkEffectStateV1, WorkExecutionBudgetV1, WorkExecutionEnvelopeV1, WorkFenceEpochV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkProjectionCoverageV1, WorkProjectionSequenceV1,
    WorkProjectionSnapshotV1, WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkRestartReasonV1, WorkTerminalEvidenceV1, WorkVersion,
    WorkflowOperationRef, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

mod work_registered_store;

use work_registered_store::RegisteredWorkStore;

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

fn context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.work.runtime-store"),
        id::<RepositoryId>("repository.work.runtime-store"),
        id::<WorktreeId>("worktree.work.runtime-store"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.runtime-store"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.runtime-store").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.runtime-store").unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id("actor.work.runtime-store"),
        scope,
        grant,
        RequestId::new("request.work.runtime-store").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.work.runtime-store").unwrap(),
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

fn route(provider: &str, route: &str) -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route)).unwrap()
}

fn requested_route() -> WorkProviderRouteV1 {
    route(
        "provider.work.codex-app-server",
        "route.work.codex-app-server.v1",
    )
}

fn execution_envelope(
    authority: &WorkAuthority,
    identity: WorkAttemptIdentityV1,
    binding: WorkAttemptProjectionBindingV1,
) -> WorkExecutionEnvelopeV1 {
    WorkExecutionEnvelopeV1::new(
        identity,
        binding,
        id::<WorkflowOperationRef>("operation.work.execute-provider"),
        requested_route(),
        WorkProviderBackendV1::CodexAppServer,
        "gpt-test".to_owned(),
        digest('c'),
        authority.project_id().clone(),
        authority.repository_id().clone(),
        authority.worktree_id().clone(),
        "/tmp/work-runtime-store".to_owned(),
        None,
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        UtcMicros(9_000),
        1,
        WorkExecutionBudgetV1::new(16_384, 16_384, 65_536).unwrap(),
        WorkEffectStateV1::Observational,
    )
    .unwrap()
}

fn lease(epoch: u64) -> WorkLeaseFenceV1 {
    WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.work.runtime-store"),
        WorkFenceEpochV1::new(epoch).unwrap(),
    )
    .unwrap()
}

fn prepare_admitted_work(storage: &WorkSqliteStorage) -> (RequestContext, TaskId) {
    let service = WorkService::new(storage.clone());
    let context = context();
    let task_id = id::<TaskId>("task.work.runtime-store");
    service
        .create(
            &context,
            CreateWorkCommand {
                task_id: task_id.clone(),
                title: "Persist runtime attempt".to_owned(),
                dependencies: BTreeSet::new(),
                command_id: id("command.work.runtime-store.create"),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
    service
        .accept_proposal(
            &context,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: id::<ProposalId>("proposal.work.runtime-store"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.work.runtime-store.proposal"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();
    service
        .admit_execution(
            &context,
            AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.work.runtime-store.admit"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    (context, task_id)
}

fn leased(authority: &WorkAuthority, task_id: TaskId) -> WorkAttemptV1 {
    let identity = WorkAttemptIdentityV1::new(
        task_id,
        id::<RunId>("run.work.runtime-store"),
        id::<AttemptId>("attempt.work.runtime-store.1"),
    )
    .unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        authority.projection_generation_id().unwrap(),
        WorkProjectionSequenceV1::new(3),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.work.runtime-store"),
    )
    .unwrap();
    WorkAttemptV1::new(
        identity.clone(),
        binding.clone(),
        execution_envelope(authority, identity, binding),
        lease(1),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        None,
        None,
    )
    .unwrap()
}

fn insert_attempt(
    storage: &WorkSqliteStorage,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkExecutionPersistenceError> {
    WorkAttemptPersistencePort::insert(storage, authority, attempt)
}

fn replace_attempt(
    storage: &WorkSqliteStorage,
    authority: &WorkAuthority,
    expected: &WorkAttemptV1,
    replacement: &WorkAttemptV1,
) -> Result<(), WorkExecutionPersistenceError> {
    storage.compare_and_swap(authority, expected, replacement)
}

#[test]
fn application_execution_service_composes_with_sqlite_adapter() {
    let store = RegisteredWorkStore::start("application");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let projection = WorkService::new(storage.clone())
        .load(&context, &task_id)
        .unwrap();
    let generation = owner.projection_generation_id().unwrap();
    let snapshot = WorkProjectionSnapshotV1::new(
        generation.clone(),
        WorkProjectionSequenceV1::new(3),
        vec![projection],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    let identity = WorkAttemptIdentityV1::new(
        task_id,
        id("run.work.runtime-store.application"),
        id("attempt.work.runtime-store.application"),
    )
    .unwrap();
    let projection_binding = WorkAttemptProjectionBindingV1::new(
        generation,
        WorkProjectionSequenceV1::new(3),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.work.runtime-store"),
    )
    .unwrap();
    let execution = execution_envelope(&owner, identity.clone(), projection_binding.clone());
    let service = WorkExecutionService::new(storage.clone());
    let leased = service
        .acquire_lease(
            &owner,
            WorkAttemptAcquireLeaseRequestV1 {
                snapshot: snapshot.clone(),
                identity: identity.clone(),
                projection_binding,
                execution,
                lease: lease(1),
                requested_route: requested_route(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .acquire_lease(
                &owner,
                WorkAttemptAcquireLeaseRequestV1 {
                    snapshot: snapshot.clone(),
                    identity: identity.clone(),
                    projection_binding: leased.projection_binding().clone(),
                    execution: leased.execution().clone(),
                    lease: lease(1),
                    requested_route: leased.requested_route().clone(),
                },
            )
            .unwrap(),
        leased
    );
    service
        .start(
            &owner,
            &identity,
            &lease(1),
            WorkRecoveryStateV1::Fresh,
            route("provider.work.actual", "route.work.actual"),
        )
        .unwrap();
    service
        .publish_progress(
            &owner,
            &identity,
            &lease(1),
            WorkAttemptProgressV1::new(1, 2).unwrap(),
        )
        .unwrap();
    service
        .publish_artifact(
            &owner,
            &identity,
            &lease(1),
            WorkArtifactRefV1::new(
                id("artifact.work.runtime-store.application"),
                digest('7'),
                32,
            )
            .unwrap(),
        )
        .unwrap();
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('8'), UtcMicros(50)).unwrap();
    let completed = service
        .terminalize(&owner, &identity, &lease(1), terminal.clone())
        .unwrap();
    assert_eq!(
        service
            .terminalize(&owner, &identity, &lease(1), terminal)
            .unwrap(),
        completed
    );
}

#[test]
fn attempt_transitions_replay_and_rebuild_after_restart() {
    let store = RegisteredWorkStore::start("attempt-restart");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let leased = leased(&owner, task_id);
    insert_attempt(&storage, &owner, &leased).unwrap();
    insert_attempt(&storage, &owner, &leased).unwrap();

    let artifact = WorkArtifactRefV1::new(
        id::<WorkArtifactId>("artifact.work.runtime-store"),
        digest('d'),
        64,
    )
    .unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            Some(WorkAttemptProgressV1::new(1, 2).unwrap()),
            vec![artifact],
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(2),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &leased, &running).unwrap();
    drop(storage);

    let store = store.restart("attempt-restart");
    let reopened = store.storage().clone();
    assert_eq!(
        reopened
            .execution_attempt(&owner, running.identity())
            .unwrap()
            .unwrap(),
        running
    );
    assert_eq!(
        reopened
            .execution_attempt_history(&owner, running.identity())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn lease_loss_rejects_stale_writer_without_partial_progress_or_artifacts() {
    let store = RegisteredWorkStore::start("lease-loss");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let leased = leased(&owner, task_id);
    insert_attempt(&storage, &owner, &leased).unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(2),
        )
        .unwrap();

    let error = replace_attempt(&storage, &owner, &running, &running).unwrap_err();
    assert_eq!(error, WorkExecutionPersistenceError::Conflict);
    assert_eq!(
        storage
            .execution_attempt_history(&owner, leased.identity())
            .unwrap(),
        vec![leased]
    );
}

#[test]
fn terminal_evidence_is_published_exactly_once() {
    let store = RegisteredWorkStore::start("terminal-once");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let leased = leased(&owner, task_id);
    insert_attempt(&storage, &owner, &leased).unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &leased, &running).unwrap();
    let terminal = running
        .transition(
            WorkAttemptStateV1::Succeeded,
            Some(WorkAttemptProgressV1::new(1, 1).unwrap()),
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            running.actual_route().cloned(),
            Some(WorkTerminalEvidenceV1::succeeded(digest('e'), UtcMicros(40)).unwrap()),
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &running, &terminal).unwrap();
    replace_attempt(&storage, &owner, &running, &terminal).unwrap();
    let other_terminal = running
        .transition(
            WorkAttemptStateV1::Failed,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            running.actual_route().cloned(),
            Some(WorkTerminalEvidenceV1::failed(digest('9'), UtcMicros(41)).unwrap()),
            lease(1),
        )
        .unwrap();
    assert_eq!(
        replace_attempt(&storage, &owner, &running, &other_terminal).unwrap_err(),
        WorkExecutionPersistenceError::Conflict
    );
}

#[test]
fn failed_attempt_event_rolls_back_snapshot_idempotency_and_terminal_rows() {
    let store = RegisteredWorkStore::start("attempt-atomic");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    store.inspect(|connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER reject_work_attempt_event
                 BEFORE INSERT ON work_attempt_events_v1
                 BEGIN
                   SELECT RAISE(ABORT, 'injected attempt append failure');
                 END;",
            )
            .unwrap();
    });
    assert!(matches!(
        insert_attempt(
            &storage,
            &authority(&context),
            &leased(&authority(&context), task_id),
        ),
        Err(WorkExecutionPersistenceError::Unavailable(_))
    ));

    for table in [
        "work_attempt_events_v1",
        "work_attempt_snapshots_v1",
        "work_attempt_idempotency_v1",
        "work_attempt_terminal_evidence_v1",
    ] {
        assert_eq!(
            store.count(table),
            0,
            "{table} must share the append transaction"
        );
    }
}

#[test]
fn forged_projection_generation_rejects_insert_with_no_attempt_row() {
    let store = RegisteredWorkStore::start("forged-generation");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let mut forged = leased(&owner, task_id);
    // Rebuild with a caller-forged generation while keeping admitted projection data.
    let forged_identity = forged.identity().clone();
    let forged_binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.work.forged-by-caller"),
        forged.projection_binding().sequence(),
        forged.projection_binding().work_version(),
        forged.projection_binding().accepted_proposal().clone(),
    )
    .unwrap();
    forged = WorkAttemptV1::new(
        forged_identity.clone(),
        forged_binding.clone(),
        execution_envelope(&owner, forged_identity, forged_binding),
        forged.lease().clone(),
        forged.state(),
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        forged.requested_route().clone(),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        insert_attempt(&storage, &owner, &forged),
        Err(WorkExecutionPersistenceError::InvalidRequest)
    );
    assert_eq!(store.count("work_attempt_events_v1"), 0);
    assert_eq!(store.count("work_attempt_snapshots_v1"), 0);
    assert_eq!(store.count("work_attempt_idempotency_v1"), 0);
}

#[test]
fn wrong_authority_rejects_insert_with_no_attempt_row() {
    let store = RegisteredWorkStore::start("wrong-authority");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let foreign = WorkAuthority::new(
        id::<ProjectId>("project.work.runtime-store.foreign"),
        owner.repository_id().clone(),
        owner.worktree_id().clone(),
        owner.actor_id().clone(),
        owner.policy_digest().clone(),
    )
    .unwrap();
    let attempt = leased(&owner, task_id);

    assert_eq!(
        insert_attempt(&storage, &foreign, &attempt),
        Err(WorkExecutionPersistenceError::InvalidRequest)
    );
    // Wrong authority must not create an attempt under either identity.
    assert_eq!(store.count("work_attempt_events_v1"), 0);
    assert_eq!(store.count("work_attempt_snapshots_v1"), 0);
}

#[test]
fn recovery_candidate_resumes_and_clears_restart_work() {
    let store = RegisteredWorkStore::start("recovery");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let leased = leased(&owner, task_id);
    insert_attempt(&storage, &owner, &leased).unwrap();
    let recovery = leased
        .transition(
            WorkAttemptStateV1::RecoveryRequired,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::RecoveryRequired {
                source_attempt_id: Some(id("attempt.work.runtime-store.0")),
                reason: WorkRestartReasonV1::ProcessLost,
            },
            None,
            None,
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &leased, &recovery).unwrap();
    assert_eq!(
        storage.recovery_candidates(&owner).unwrap(),
        vec![recovery.clone()]
    );

    let resumed = recovery
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Resumed {
                source_attempt_id: id("attempt.work.runtime-store.0"),
                checkpoint: None,
            },
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(2),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &recovery, &resumed).unwrap();
    assert!(storage.recovery_candidates(&owner).unwrap().is_empty());
    assert_eq!(
        storage
            .execution_attempt(&owner, resumed.identity())
            .unwrap()
            .unwrap(),
        resumed
    );
}

#[test]
fn cancellation_acknowledgement_and_terminal_evidence_persist_together() {
    let store = RegisteredWorkStore::start("cancellation");
    let storage = store.storage().clone();
    let (context, task_id) = prepare_admitted_work(&storage);
    let owner = authority(&context);
    let leased = leased(&owner, task_id);
    insert_attempt(&storage, &owner, &leased).unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &leased, &running).unwrap();
    let request = WorkCancellationRequestV1::new(
        id::<WorkCancellationRequestId>("cancel.work.runtime-store"),
        UtcMicros(40),
    )
    .unwrap();
    let requested = running
        .transition(
            WorkAttemptStateV1::CancellationRequested,
            None,
            Vec::new(),
            WorkCancellationStateV1::Requested(request.clone()),
            WorkRecoveryStateV1::Fresh,
            running.actual_route().cloned(),
            None,
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &running, &requested).unwrap();
    let acknowledgement = WorkCancellationAcknowledgementV1::new(request, UtcMicros(41)).unwrap();
    let acknowledged = requested
        .transition(
            WorkAttemptStateV1::CancellationAcknowledged,
            None,
            Vec::new(),
            WorkCancellationStateV1::Acknowledged(acknowledgement.clone()),
            WorkRecoveryStateV1::Fresh,
            requested.actual_route().cloned(),
            None,
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &requested, &acknowledged).unwrap();
    let cancelled = acknowledged
        .transition(
            WorkAttemptStateV1::Cancelled,
            None,
            Vec::new(),
            WorkCancellationStateV1::Acknowledged(acknowledgement),
            WorkRecoveryStateV1::Fresh,
            acknowledged.actual_route().cloned(),
            Some(WorkTerminalEvidenceV1::cancelled(digest('9'), UtcMicros(42)).unwrap()),
            lease(1),
        )
        .unwrap();
    replace_attempt(&storage, &owner, &acknowledged, &cancelled).unwrap();
    assert_eq!(
        storage
            .execution_attempt(&owner, cancelled.identity())
            .unwrap()
            .unwrap(),
        cancelled
    );
}
