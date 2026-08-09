use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, WorkflowFailurePolicy, WorkflowFanOutCensusPersistOutcomeV1,
    WorkflowFanOutCensusStoragePort, WorkflowFanOutInput, WorkflowFanOutRequest,
    WorkflowProviderAdmission, WorkflowRunAppendRequest, WorkflowRunStoragePort,
    durable_workflow_fan_out_plan, prepare_workflow_fan_out,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId,
    ExecutionPlacementV1, ExecutionTopologyKindV1, InitiativeId, IntegrationStrategyV1,
    ManifestDigest, MilestoneId, ProjectId, ProposalId, ProviderId, RepositoryId, ReviewTopologyV1,
    RunId, TaskId, UtcMicros, WorkApprovalPolicy, WorkAuthority, WorkEffectStateV1,
    WorkExecutableReference, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProposalV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRouteDecisionV1, WorkSandboxPolicy, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkTopologyBranchV1, WorkflowCensusCountV1, WorkflowCensusDurationV1,
    WorkflowCensusEvidenceReasonV1, WorkflowCensusGenerationV1, WorkflowDefinition,
    WorkflowExecutionTopologyClassificationV1, WorkflowExecutionTopologyEvidenceV1, WorkflowFanOut,
    WorkflowFanOutCensusV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowProviderCapacityEvidenceV1, WorkflowRunCommand, WorkflowRunEvent,
    WorkflowRunEventContext, WorkflowStep, WorkflowStepId, WorktreeId,
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

fn digest(byte: char) -> ManifestDigest {
    let hex = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex.repeat(32))).unwrap()
}

fn fan_out_authority() -> WorkAuthority {
    WorkAuthority::new(
        id("project.workflow.census-storage"),
        id::<RepositoryId>("repository.workflow.census-storage"),
        id::<WorktreeId>("worktree.workflow.census-storage"),
        id::<ActorId>("actor.workflow.census-storage"),
        digest('9'),
    )
    .unwrap()
}

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.census-storage"),
        1,
        id::<ProjectId>("project.workflow.census-storage"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>("operation.workflow.prepare"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: Some(WorkflowFanOut { max_width: 1 }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap()
}

fn context(command: &str, input: char, at: i64) -> WorkflowRunEventContext {
    WorkflowRunEventContext {
        command_id: id(command),
        input_digest: digest(input),
        occurred_at: UtcMicros(at),
    }
}

fn fan_out_input(identity: &str, input_digest: ManifestDigest) -> WorkflowFanOutInput {
    let task_id = id::<TaskId>(&format!("task.workflow.census-storage.{identity}"));
    let initiative_id =
        id::<InitiativeId>(&format!("initiative.workflow.census-storage.{identity}"));
    let plan_id = id::<WorkPlanId>(&format!("plan.workflow.census-storage.{identity}"));
    let milestone_id = id::<MilestoneId>(&format!("milestone.workflow.census-storage.{identity}"));
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
        id::<ProposalId>(&format!("proposal.workflow.census-storage.{identity}")),
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

fn census(
    run_id: RunId,
    sequence: u64,
    interval_started_at: i64,
    observed_at: i64,
    sample: bool,
) -> WorkflowFanOutCensusV1 {
    WorkflowFanOutCensusV1 {
        run_id,
        workflow_sequence: sequence,
        topology_digest: digest('c'),
        provider_registry_digest: digest('d'),
        work_generation: if sample {
            WorkflowCensusGenerationV1::Exact {
                generation_id: id("generation.workflow.census-storage"),
            }
        } else {
            WorkflowCensusGenerationV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            }
        },
        execution_topology: if sample {
            WorkflowExecutionTopologyEvidenceV1::Known {
                value: WorkflowExecutionTopologyClassificationV1 {
                    topology: ExecutionTopologyKindV1::Parallel,
                    placement: ExecutionPlacementV1::InPlace,
                    branch_topology: WorkTopologyBranchV1::NoBranches,
                    review_topology: ReviewTopologyV1::NoReview,
                    integration_strategy: IntegrationStrategyV1::NoIntegration,
                },
            }
        } else {
            WorkflowExecutionTopologyEvidenceV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            }
        },
        interval_started_at: UtcMicros(interval_started_at),
        observed_at: UtcMicros(observed_at),
        requested_width: WorkflowCensusCountV1::Known { value: 1 },
        accepted_width: if sample {
            WorkflowCensusCountV1::Known { value: 1 }
        } else {
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            }
        },
        admitted_width: if sample {
            WorkflowCensusCountV1::Known { value: 1 }
        } else {
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            }
        },
        active_width: if sample {
            WorkflowCensusCountV1::Known { value: 1 }
        } else {
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::AttemptUnavailable,
            }
        },
        useful_width: if sample {
            WorkflowCensusCountV1::Known { value: 0 }
        } else {
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::FirstObservation,
            }
        },
        shared_authority_serialized_count: WorkflowCensusCountV1::Known { value: 0 },
        runnable_count: WorkflowCensusCountV1::Known { value: 0 },
        blocked_count: WorkflowCensusCountV1::Known { value: 0 },
        provider_capacities: WorkflowProviderCapacityEvidenceV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
        },
        observed_duration: WorkflowCensusDurationV1::Known { micros: 0 },
        critical_path_duration: WorkflowCensusDurationV1::Known { micros: 0 },
        attempt_frontiers: Vec::<tracedecay_domain::WorkflowAttemptFrontierV1>::new(),
    }
}

fn provider() -> WorkflowProviderAdmission {
    let topology = safe_work_topology_policy_v1();
    let execution_snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.census-storage",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.census-storage",
        ),
        effective_behavior_digest: digest('b'),
        resolution_provenance_digest: digest('e'),
        route: WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.codex-app-server"),
            id::<WorkProviderRouteId>("route.workflow.census-storage"),
        )
        .unwrap(),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: "gpt-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.workflow.census-storage".to_owned(),
            digest('f'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: tracedecay_domain::WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(10_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: topology.clone(),
    })
    .unwrap();
    WorkflowProviderAdmission {
        execution_snapshot,
        topology_digest: digest('c'),
        provider_registry_digest: digest('d'),
        worktree_placement: topology.placement,
        reference: None,
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        cancellation_generation: 1,
        effect_state: WorkEffectStateV1::Observational,
    }
}

fn durable_plan(
    run_id: &RunId,
    definition: &WorkflowDefinition,
) -> tracedecay_domain::WorkflowFanOutPlanV1 {
    let provider = provider();
    let planned = prepare_workflow_fan_out(&WorkflowFanOutRequest {
        definition: definition.clone(),
        run_id: run_id.clone(),
        step_id: id("prepare"),
        fence: tracedecay_application::WorkflowExecutionFence {
            attempt_id: id::<AttemptId>("attempt.workflow.census-storage.fence"),
            lease: WorkLeaseFenceV1::new(
                id::<WorkLeaseId>("lease.workflow.census-storage.fence"),
                WorkFenceEpochV1::new(1).unwrap(),
            )
            .unwrap(),
        },
        admitted_at: UtcMicros(100),
        cancellation: CancellationContext::active("cancel.workflow.census-storage").unwrap(),
        max_parallel: 1,
        failure_policy: WorkflowFailurePolicy::Collect,
        provider: provider.clone(),
        inputs: vec![fan_out_input("child", digest('1'))],
    })
    .unwrap();
    durable_workflow_fan_out_plan(&planned, &provider, fan_out_authority()).unwrap()
}

fn open_authority(store: &RegisteredWorkflowStore) -> WorkflowSqliteAuthority {
    WorkflowSqliteAuthority::from_registered(store.storage().clone()).unwrap()
}

#[test]
fn census_replay_conflict_and_restart_recover_the_durable_transition() {
    let store = RegisteredWorkflowStore::start("workflow-fan-out-census-storage");
    let authority = open_authority(&store);
    let run_id = id::<RunId>("run.workflow.census-storage");
    let definition = definition();
    let plan = durable_plan(&run_id, &definition);
    let admitted = WorkflowRunEvent::admitted_with_fan_out(
        run_id.clone(),
        definition.clone(),
        digest('c'),
        digest('d'),
        vec![plan.clone()],
        context("command.workflow.census-storage.admit", '1', 100),
    )
    .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admitted,
        },
    )
    .unwrap();
    let binding =
        WorkflowRunStoragePort::fan_out_binding(&authority, &plan.children[0].attempt_identity)
            .unwrap()
            .unwrap();
    assert_eq!(binding.run_id, run_id);
    assert_eq!(binding.step_id, plan.step_id);
    assert_eq!(binding.plan_digest, plan.plan_digest);
    let unrelated = tracedecay_domain::WorkAttemptIdentityV1::new(
        id::<TaskId>("task.workflow.census-storage.unrelated"),
        id::<RunId>("run.workflow.census-storage.unrelated"),
        id::<AttemptId>("attempt.workflow.census-storage.unrelated"),
    )
    .unwrap();
    assert!(
        WorkflowRunStoragePort::fan_out_binding(&authority, &unrelated)
            .unwrap()
            .is_none()
    );

    let first = census(run_id.clone(), 1, 200, 200, false);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &first).unwrap(),
        WorkflowFanOutCensusPersistOutcomeV1::Persisted
    );
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &first).unwrap(),
        WorkflowFanOutCensusPersistOutcomeV1::Replayed
    );

    let mut conflict = first.clone();
    conflict.observed_at = UtcMicros(201);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &conflict).unwrap_err(),
        tracedecay_application::WorkflowFanOutCensusError::Conflict
    );

    let projection = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    let pause = projection
        .next_event(
            WorkflowRunCommand::Pause,
            context("command.workflow.census-storage.pause", '2', 250),
        )
        .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequest {
            expected_sequence: Some(projection.sequence()),
            event: pause,
        },
    )
    .unwrap();
    let second = census(run_id.clone(), 2, 200, 300, true);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &second).unwrap(),
        WorkflowFanOutCensusPersistOutcomeV1::Persisted
    );
    assert_eq!(
        WorkflowFanOutCensusStoragePort::census_before(&authority, &run_id, 2)
            .unwrap()
            .unwrap(),
        first
    );
    let pending =
        WorkflowFanOutCensusStoragePort::pending_census_observations(&authority, 16).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].census, second);
    assert_eq!(pending[0].previous_observed_at, UtcMicros(200));
    assert_eq!(pending[0].terminal, None);

    let store = store.restart("workflow-fan-out-census-storage-restarted");
    let reopened = open_authority(&store);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::latest_census(&reopened, &run_id)
            .unwrap()
            .unwrap(),
        second
    );
    let pending_after_restart =
        WorkflowFanOutCensusStoragePort::pending_census_observations(&reopened, 16).unwrap();
    assert_eq!(pending_after_restart.len(), 1);
    assert_eq!(pending_after_restart[0].census, second);
    assert_eq!(
        pending_after_restart[0].previous_observed_at,
        UtcMicros(200)
    );
    assert_eq!(pending_after_restart[0].terminal, None);
    WorkflowFanOutCensusStoragePort::mark_census_observability_durable(&reopened, &second).unwrap();
    WorkflowFanOutCensusStoragePort::mark_census_observability_durable(&reopened, &second).unwrap();
    assert!(
        WorkflowFanOutCensusStoragePort::pending_census_observations(&reopened, 16)
            .unwrap()
            .is_empty()
    );
    let mut divergent = second.clone();
    divergent.observed_at = UtcMicros(301);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::mark_census_observability_durable(&reopened, &divergent)
            .unwrap_err(),
        tracedecay_application::WorkflowFanOutCensusError::Conflict
    );
    assert_eq!(
        WorkflowFanOutCensusStoragePort::census_before(&reopened, &run_id, 3)
            .unwrap()
            .unwrap(),
        second
    );
    assert_eq!(store.count("workflow_fan_out_census_journal"), 2);
}

#[test]
fn census_persist_failure_restarts_and_backfills_the_current_projection() {
    let store = RegisteredWorkflowStore::start("workflow-fan-out-census-backfill");
    let authority = open_authority(&store);
    let run_id = id::<RunId>("run.workflow.census-backfill");
    let definition = definition();
    let plan = durable_plan(&run_id, &definition);
    let admitted = WorkflowRunEvent::admitted_with_fan_out(
        run_id.clone(),
        definition.clone(),
        digest('c'),
        digest('d'),
        vec![plan],
        context("command.workflow.census-backfill.admit", '1', 100),
    )
    .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admitted,
        },
    )
    .unwrap();
    let first = census(run_id.clone(), 1, 200, 200, false);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &first).unwrap(),
        WorkflowFanOutCensusPersistOutcomeV1::Persisted
    );

    let projection = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    let cancellation = projection
        .next_event(
            WorkflowRunCommand::RequestCancellation,
            context("command.workflow.census-backfill.cancel", '2', 250),
        )
        .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequest {
            expected_sequence: Some(projection.sequence()),
            event: cancellation,
        },
    )
    .unwrap();
    let cancelling = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    let cancelled = cancelling
        .next_event(
            WorkflowRunCommand::ReconcileCancelled,
            context("command.workflow.census-backfill.cancelled", '3', 260),
        )
        .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequest {
            expected_sequence: Some(cancelling.sequence()),
            event: cancelled,
        },
    )
    .unwrap();
    let expected_projection = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    assert!(expected_projection.status().is_terminal());

    store.inspect(|connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER workflow_census_test_fail_insert
                 BEFORE INSERT ON workflow_fan_out_census_journal
                 WHEN NEW.workflow_sequence = 3
                 BEGIN
                     SELECT RAISE(ABORT, 'injected census insert failure');
                 END;",
            )
            .unwrap();
    });
    let failed = census(run_id.clone(), 3, 200, 300, true);
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&authority, &failed).unwrap_err(),
        tracedecay_application::WorkflowFanOutCensusError::Unavailable
    );
    assert_eq!(store.count("workflow_fan_out_census_journal"), 1);

    let store = store.restart("workflow-fan-out-census-backfill-restarted");
    let reopened = open_authority(&store);
    let page = WorkflowFanOutCensusStoragePort::census_backfill_projection_page(
        &reopened,
        &fan_out_authority(),
        None,
    )
    .unwrap();
    assert_eq!(page.continuation, None);
    assert_eq!(page.projections, vec![expected_projection.clone()]);

    store.inspect(|connection| {
        connection
            .execute_batch("DROP TRIGGER workflow_census_test_fail_insert;")
            .unwrap();
    });
    let rederived = census(
        run_id.clone(),
        expected_projection.sequence(),
        200,
        300,
        true,
    );
    assert_eq!(
        WorkflowFanOutCensusStoragePort::persist_census(&reopened, &rederived).unwrap(),
        WorkflowFanOutCensusPersistOutcomeV1::Persisted
    );
    assert_eq!(store.count("workflow_fan_out_census_journal"), 2);
    let empty = WorkflowFanOutCensusStoragePort::census_backfill_projection_page(
        &reopened,
        &fan_out_authority(),
        page.continuation.as_ref(),
    )
    .unwrap();
    assert!(empty.projections.is_empty());
    assert_eq!(empty.continuation, None);
}
