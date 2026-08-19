//! Durable workflow run journal and artifact payload store over the
//! registered Work writer.
//!
//! Falsifiable claims: journaled runs and pause transitions survive a full
//! store restart; cancellation settles terminal, refuses premature
//! reconciliation, and stays final after restart; command replay is
//! idempotent while divergent reuse and stale sequences are typed conflicts;
//! artifact payloads are digest-verified on every hydration so a corrupted
//! row can never re-enter execution.

use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, WorkflowAdmissionSnapshot, WorkflowArtifactPayload,
    WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError, WorkflowArtifactStorePort,
    WorkflowExecutionFence, WorkflowFailurePolicy, WorkflowFanOutInput, WorkflowFanOutRequest,
    WorkflowProviderAdmission, WorkflowRunAppendOutcome, WorkflowRunAppendRequest,
    WorkflowRunService, WorkflowRunServiceError, WorkflowRunStorageError, WorkflowRunStoragePort,
    durable_workflow_fan_out_plan, prepare_workflow_fan_out, work_executable_catalog_digest,
    workflow_artifact_payload_digest,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, InitiativeId,
    ManifestDigest, MilestoneId, ProjectId, ProposalId, ProviderId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkArtifactId, WorkArtifactRefV1, WorkAuthority, WorkCommandId,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1,
    WorkFilesystemPolicy, WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1,
    WorkItemV1, WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProposalV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkRouteDecisionV1, WorkSandboxPolicy, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1, WorkflowDefinition, WorkflowDefinitionId, WorkflowFanOut,
    WorkflowOperationRef, WorkflowOutputName, WorkflowRunCommand, WorkflowRunEvent,
    WorkflowRunEventContext, WorkflowRunStatus, WorkflowStep, WorkflowStepId, WorktreeId,
};
use tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority;

mod registered_workflow_store;

use registered_workflow_store::RegisteredWorkflowStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// A distinct, valid `sha256:`-tagged digest per input byte.
fn digest(byte: char) -> ManifestDigest {
    let hex_byte = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex_byte.repeat(32))).unwrap()
}

fn context(command: &str, input: char, occurred_at: i64) -> WorkflowRunEventContext {
    WorkflowRunEventContext {
        command_id: id::<WorkCommandId>(command),
        input_digest: digest(input),
        occurred_at: UtcMicros(occurred_at),
    }
}

fn fan_out_input(identity: &str, input_digest: ManifestDigest) -> WorkflowFanOutInput {
    let task_id = id::<TaskId>(&format!("task.workflow.journal.{identity}"));
    let initiative_id = id::<InitiativeId>(&format!("initiative.workflow.journal.{identity}"));
    let plan_id = id::<WorkPlanId>(&format!("plan.workflow.journal.{identity}"));
    let milestone_id = id::<MilestoneId>(&format!("milestone.workflow.journal.{identity}"));
    let created_at = UtcMicros(10);
    let initiative = WorkInitiativeV1::new(
        initiative_id.clone(),
        format!("Initiative {identity}"),
        created_at,
    )
    .unwrap();
    let plan = WorkPlanV1::new(
        plan_id.clone(),
        initiative_id.clone(),
        format!("Plan {identity}"),
        created_at,
    )
    .unwrap();
    let milestone = WorkMilestoneV1::new(
        milestone_id.clone(),
        plan_id.clone(),
        format!("Milestone {identity}"),
        created_at,
    )
    .unwrap();
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id: task_id.clone(),
        hierarchy: WorkHierarchyV1::new(initiative_id, plan_id, milestone_id),
        title: format!("Task {identity}"),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: Vec::new(),
        effort: 1,
        scheduled_at: None,
        deadline: None,
        created_at,
        updated_at: created_at,
    })
    .unwrap();
    let proposal = WorkProposalV1::new(
        id::<ProposalId>(&format!("proposal.workflow.journal.{identity}")),
        task_id,
        WorkGraphVersionV1::initial(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "complete fixture").unwrap(),
        Vec::new(),
        WorkRouteDecisionV1::abstain("fixture route").unwrap(),
        format!("Proposal {identity}"),
        input_digest.clone(),
    )
    .unwrap();
    WorkflowFanOutInput {
        instructions: identity.to_owned(),
        input_digest,
        initiative,
        plan,
        milestone,
        item,
        proposal,
    }
}

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id::<WorkflowDefinitionId>("workflow.definition.journal"),
        1,
        id::<ProjectId>("project.workflow.journal"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        work_executable_catalog_digest().unwrap(),
    )
    .unwrap()
}

fn fan_out_definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id::<WorkflowDefinitionId>("workflow.definition.journal.fan-out"),
        1,
        id::<ProjectId>("project.workflow.journal"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOut { max_width: 1 }),
        }],
        digest('a'),
        digest('b'),
        work_executable_catalog_digest().unwrap(),
    )
    .unwrap()
}

fn work_authority(worktree: &str) -> WorkAuthority {
    WorkAuthority::new(
        id("project.workflow.journal"),
        id::<RepositoryId>("repository.workflow.journal"),
        id::<WorktreeId>(worktree),
        id::<ActorId>("actor.workflow.journal"),
        digest('9'),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.workflow-journal",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.workflow-journal",
        ),
        effective_behavior_digest: digest('b'),
        resolution_provenance_digest: digest('2'),
        route: WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.codex-app-server"),
            id::<WorkProviderRouteId>("route.workflow-journal"),
        )
        .unwrap(),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: "gpt-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.workflow-journal".to_owned(),
            digest('3'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(90_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::configuration::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn admit_fan_out_run(
    service: &WorkflowRunService<WorkflowSqliteAuthority>,
    run_id: RunId,
    authority: WorkAuthority,
    command: &str,
) {
    let definition = fan_out_definition();
    let provider = WorkflowProviderAdmission {
        execution_snapshot: execution_snapshot(),
        topology_digest: digest('c'),
        provider_registry_digest: digest('d'),
        worktree_placement: tracedecay_domain::configuration::safe_work_topology_policy_v1()
            .placement,
        reference: None,
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        cancellation_generation: 1,
        effect_state: WorkEffectStateV1::Observational,
    };
    let planned = prepare_workflow_fan_out(&WorkflowFanOutRequest {
        definition: definition.clone(),
        run_id: run_id.clone(),
        step_id: id("fan-out"),
        fence: WorkflowExecutionFence {
            attempt_id: id::<AttemptId>(&format!("attempt.{command}")),
            lease: WorkLeaseFenceV1::new(
                id::<WorkLeaseId>(&format!("lease.{command}")),
                WorkFenceEpochV1::new(1).unwrap(),
            )
            .unwrap(),
        },
        admitted_at: UtcMicros(100),
        cancellation: CancellationContext::active(format!("cancel.{command}")).unwrap(),
        max_parallel: 1,
        failure_policy: WorkflowFailurePolicy::Collect,
        provider: provider.clone(),
        inputs: vec![fan_out_input(command, digest('e'))],
    })
    .unwrap();
    let durable = durable_workflow_fan_out_plan(&planned, &provider, authority).unwrap();
    service
        .admit_with_fan_out(
            run_id,
            definition,
            admission(),
            vec![durable],
            context(&format!("command.{command}"), '4', 100),
        )
        .unwrap();
}

fn admission() -> WorkflowAdmissionSnapshot {
    WorkflowAdmissionSnapshot {
        policy_digest: digest('a'),
        configuration_digest: digest('b'),
        catalog_digest: work_executable_catalog_digest().unwrap(),
        topology_digest: digest('c'),
        provider_registry_digest: digest('d'),
    }
}

fn attach(store: &RegisteredWorkflowStore) -> WorkflowSqliteAuthority {
    WorkflowSqliteAuthority::from_retained_exact_sql(store.retained_exact_sql())
        .expect("attach workflow authority")
}

fn content_artifact(name: &str, content: &[u8]) -> WorkflowArtifactPayload {
    let reference = WorkArtifactRefV1::new(
        id::<WorkArtifactId>(name),
        workflow_artifact_payload_digest(content).unwrap(),
        content.len() as u64,
    )
    .unwrap();
    WorkflowArtifactPayload::new(reference, content.to_vec()).unwrap()
}

#[test]
fn paused_run_survives_restart_and_resumes_from_durable_state() {
    let store = RegisteredWorkflowStore::start("run-journal-pause-crash-resume");
    let authority = attach(&store);
    let run_id = id::<RunId>("run.workflow.journal.pause");
    let service = WorkflowRunService::new(authority.clone());
    let admitted = service
        .admit(
            run_id.clone(),
            definition(),
            admission(),
            context("command.journal.admit", '1', 1),
        )
        .unwrap();
    assert_eq!(admitted.status(), WorkflowRunStatus::Running);
    let paused = service
        .apply(
            &run_id,
            admitted.sequence(),
            WorkflowRunCommand::Pause,
            context("command.journal.pause", '2', 2),
        )
        .unwrap();
    assert_eq!(paused.status(), WorkflowRunStatus::Paused);

    // The pause is a durable typed transition, not process suspension: a full
    // store restart rebinds the channel and the run is still exactly paused.
    let store = store.restart("run-journal-pause-crash-resume-restarted");
    let reopened = attach(&store);
    let recovered = WorkflowRunStoragePort::projection(&reopened, &run_id).unwrap();
    assert_eq!(recovered.status(), WorkflowRunStatus::Paused);
    assert_eq!(recovered.sequence(), paused.sequence());

    let resumed = WorkflowRunService::new(reopened)
        .apply(
            &run_id,
            recovered.sequence(),
            WorkflowRunCommand::Resume,
            context("command.journal.resume", '3', 3),
        )
        .unwrap();
    assert_eq!(resumed.status(), WorkflowRunStatus::Running);
}

#[test]
fn cancellation_settles_terminal_and_premature_reconcile_is_refused() {
    let store = RegisteredWorkflowStore::start("run-journal-cancellation");
    let authority = attach(&store);
    let run_id = id::<RunId>("run.workflow.journal.cancel");
    let service = WorkflowRunService::new(authority.clone());
    let admitted = service
        .admit(
            run_id.clone(),
            definition(),
            admission(),
            context("command.journal.admit", '1', 1),
        )
        .unwrap();

    // Reconciling a run that never entered Cancelling is a typed state
    // refusal, not a silent terminal write.
    assert!(matches!(
        service
            .apply(
                &run_id,
                admitted.sequence(),
                WorkflowRunCommand::ReconcileCancelled,
                context("command.journal.reconcile.early", '2', 2),
            )
            .unwrap_err(),
        WorkflowRunServiceError::State(_)
    ));

    let cancelling = service
        .apply(
            &run_id,
            admitted.sequence(),
            WorkflowRunCommand::RequestCancellation,
            context("command.journal.cancel", '3', 3),
        )
        .unwrap();
    assert_eq!(cancelling.status(), WorkflowRunStatus::Cancelling);
    let cancelled = service
        .apply(
            &run_id,
            cancelling.sequence(),
            WorkflowRunCommand::ReconcileCancelled,
            context("command.journal.reconcile", '4', 4),
        )
        .unwrap();
    assert_eq!(cancelled.status(), WorkflowRunStatus::Cancelled);

    // The terminal state is durable across a full restart and refuses resume.
    let store = store.restart("run-journal-cancellation-restarted");
    let reopened = attach(&store);
    let recovered = WorkflowRunStoragePort::projection(&reopened, &run_id).unwrap();
    assert_eq!(recovered.status(), WorkflowRunStatus::Cancelled);
    assert!(matches!(
        WorkflowRunService::new(reopened)
            .apply(
                &run_id,
                recovered.sequence(),
                WorkflowRunCommand::Resume,
                context("command.journal.resume.after-cancel", '5', 5),
            )
            .unwrap_err(),
        WorkflowRunServiceError::State(_)
    ));
}

#[test]
fn command_replay_is_idempotent_and_divergent_reuse_is_a_typed_conflict() {
    let store = RegisteredWorkflowStore::start("run-journal-idempotency");
    let authority = attach(&store);
    let run_id = id::<RunId>("run.workflow.journal.idempotency");
    let admit_event = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(),
        digest('c'),
        digest('d'),
        context("command.journal.admit", '1', 1),
    )
    .unwrap();
    let appended = authority
        .append(&WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admit_event.clone(),
        })
        .unwrap();
    assert!(matches!(appended, WorkflowRunAppendOutcome::Appended(_)));

    // Byte-identical replay of the same command is answered from the journal.
    let replayed = authority
        .append(&WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admit_event.clone(),
        })
        .unwrap();
    assert!(matches!(replayed, WorkflowRunAppendOutcome::Replayed(_)));
    assert_eq!(store.count("workflow_run_journal"), 1);

    // The same command identity with different input is a conflict, not a
    // second admission.
    let divergent = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(),
        digest('e'),
        digest('d'),
        context("command.journal.admit", '1', 1),
    )
    .unwrap();
    assert_eq!(
        authority
            .append(&WorkflowRunAppendRequest {
                expected_sequence: None,
                event: divergent,
            })
            .unwrap_err(),
        WorkflowRunStorageError::IdempotencyConflict
    );

    // A stale expected sequence is a version conflict before any write.
    let projection = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    let stale = projection.next_event(
        WorkflowRunCommand::Pause,
        context("command.journal.pause", '2', 2),
    );
    let pause_event = stale.unwrap();
    assert_eq!(
        authority
            .append(&WorkflowRunAppendRequest {
                expected_sequence: Some(projection.sequence() + 1),
                event: pause_event,
            })
            .unwrap_err(),
        WorkflowRunStorageError::VersionConflict
    );
    assert_eq!(store.count("workflow_run_journal"), 1);

    assert_eq!(
        WorkflowRunStoragePort::projection(&authority, &id::<RunId>("run.workflow.journal.absent"))
            .unwrap_err(),
        WorkflowRunStorageError::NotFound
    );
}

#[test]
fn artifact_payloads_survive_restart_and_hydration_verifies_content() {
    let store = RegisteredWorkflowStore::start("artifact-payload-durability");
    let authority = attach(&store);
    let payload = content_artifact(
        "artifact.workflow.journal.context",
        b"durable context bytes",
    );

    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Persisted
    );
    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Replayed
    );
    assert_eq!(store.count("workflow_artifact_payloads"), 1);

    let store = store.restart("artifact-payload-durability-restarted");
    let reopened = attach(&store);
    assert_eq!(reopened.load(payload.artifact()).unwrap(), payload);

    let absent = content_artifact("artifact.workflow.journal.absent", b"never persisted");
    assert_eq!(
        reopened.load(absent.artifact()).unwrap_err(),
        WorkflowArtifactStoreError::Missing
    );
}

#[test]
fn corrupted_artifact_rows_are_refused_on_hydration() {
    let store = RegisteredWorkflowStore::start("artifact-payload-corruption");
    let authority = attach(&store);
    let payload = content_artifact(
        "artifact.workflow.journal.context",
        b"durable context bytes",
    );
    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Persisted
    );

    // A foreign writer flips the stored bytes under the same digest row (the
    // same length keeps the schema CHECK satisfied, so only content
    // verification can catch it).
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE workflow_artifact_payloads SET payload = ?1",
                [b"DURABLE CONTEXT BYTES".as_slice()],
            )
            .unwrap();
    });
    assert_eq!(
        authority.load(payload.artifact()).unwrap_err(),
        WorkflowArtifactStoreError::DigestMismatch
    );
}

#[test]
fn active_recovery_pages_resume_after_restart_and_exclude_foreign_authority() {
    let store = RegisteredWorkflowStore::start("run-journal-active-recovery-pages");
    let authority = attach(&store);
    let service = WorkflowRunService::new(authority.clone());
    for ordinal in 0..32 {
        let run_id = id::<RunId>(&format!("run.workflow.recovery.{ordinal:02}"));
        service
            .admit(
                run_id,
                definition(),
                admission(),
                context(&format!("command.recovery.{ordinal:02}"), '1', ordinal + 1),
            )
            .unwrap();
    }
    let registered_authority = work_authority("worktree.workflow.journal.registered");
    admit_fan_out_run(
        &service,
        id("run.workflow.recovery.32"),
        registered_authority.clone(),
        "recovery.matching",
    );
    admit_fan_out_run(
        &service,
        id("run.workflow.recovery.33"),
        work_authority("worktree.workflow.journal.foreign"),
        "recovery.foreign",
    );

    let first = authority
        .active_projection_page(&registered_authority, None)
        .unwrap();
    assert_eq!(first.projections.len(), 32);
    let cursor = first
        .continuation
        .expect("a full page with remaining durable runs must retain a cursor");
    assert_eq!(cursor.after_run_id.as_str(), "run.workflow.recovery.31");

    let store = store.restart("run-journal-active-recovery-pages-restarted");
    let reopened = attach(&store);
    let second = reopened
        .active_projection_page(&registered_authority, Some(&cursor))
        .unwrap();
    assert_eq!(second.continuation, None);
    assert_eq!(second.projections.len(), 1);
    assert_eq!(
        second.projections[0].run_id().as_str(),
        "run.workflow.recovery.32"
    );
    assert!(
        second.projections[0]
            .fan_out_plans()
            .values()
            .all(|plan| plan.authority == registered_authority)
    );
}
