use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, WorkflowFailurePolicy, WorkflowFanOutInput, WorkflowFanOutRequest,
    WorkflowFanOutRuntimeError, WorkflowProviderAdmission, durable_workflow_fan_out_plan,
    prepare_workflow_fan_out,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, InitiativeId,
    ManifestDigest, MilestoneId, ProjectId, ProposalId, ProviderId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkAuthority, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProposalV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRouteDecisionV1, WorkSandboxPolicy, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkflowDefinition, WorkflowFanOut, WorkflowOperationRef, WorkflowOutputName, WorkflowStep,
    WorkflowStepId, WorktreeId,
};

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

fn fan_out_input(identity: &str, input_digest: ManifestDigest) -> WorkflowFanOutInput {
    let task_id = id::<TaskId>(&format!("task.workflow.runtime.{identity}"));
    let initiative_id = id::<InitiativeId>(&format!("initiative.workflow.runtime.{identity}"));
    let plan_id = id::<WorkPlanId>(&format!("plan.workflow.runtime.{identity}"));
    let milestone_id = id::<MilestoneId>(&format!("milestone.workflow.runtime.{identity}"));
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
        id::<ProposalId>(&format!("proposal.workflow.runtime.{identity}")),
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

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id("project.workflow.runtime"),
        id::<RepositoryId>("repository.workflow.runtime"),
        id::<WorktreeId>("worktree.workflow.runtime"),
        id::<ActorId>("actor.workflow.runtime"),
        digest('9'),
    )
    .unwrap()
}

fn execution_snapshot(model: &str) -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.workflow.runtime",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.workflow.runtime",
        ),
        effective_behavior_digest: digest('b'),
        resolution_provenance_digest: digest('c'),
        route: WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.codex-app-server"),
            id::<WorkProviderRouteId>("route.work.codex-app-server.v1"),
        )
        .unwrap(),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: model.to_owned(),
        executable: WorkExecutableReference::new(
            "executable.codex.app-server".to_owned(),
            digest('f'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn request(inputs: &[&str], max_width: u32, max_parallel: u32) -> WorkflowFanOutRequest {
    let definition = WorkflowDefinition::new(
        id("workflow.definition.runtime"),
        1,
        id::<ProjectId>("project.workflow.runtime"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOut { max_width }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap();
    WorkflowFanOutRequest {
        definition,
        run_id: id::<RunId>("run.workflow.runtime"),
        step_id: id::<WorkflowStepId>("fan-out"),
        fence: tracedecay_application::WorkflowExecutionFence {
            attempt_id: id::<AttemptId>("attempt.workflow.runtime"),
            lease: WorkLeaseFenceV1::new(
                id::<WorkLeaseId>("lease.workflow.runtime"),
                WorkFenceEpochV1::new(1).unwrap(),
            )
            .unwrap(),
        },
        admitted_at: UtcMicros(100),
        cancellation: CancellationContext::active("cancel.workflow.runtime").unwrap(),
        max_parallel,
        failure_policy: WorkflowFailurePolicy::Collect,
        provider: WorkflowProviderAdmission {
            execution_snapshot: execution_snapshot("gpt-test"),
            topology_digest: digest('d'),
            provider_registry_digest: digest('e'),
            worktree_placement: safe_work_topology_policy_v1().placement,
            reference: None,
            commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
            cancellation_generation: 1,
            effect_state: WorkEffectStateV1::Observational,
        },
        inputs: inputs
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                fan_out_input(
                    identity,
                    digest(char::from(b'1' + u8::try_from(index).unwrap())),
                )
            })
            .collect(),
    }
}

#[test]
fn planner_separates_fan_out_width_from_parallelism() {
    let plan = prepare_workflow_fan_out(&request(&["c", "a", "b"], 4, 2)).unwrap();

    assert_eq!(plan.max_parallel, 2);
    assert_eq!(plan.children.len(), 3);
    assert_eq!(
        plan.children
            .iter()
            .map(|child| child.input.instructions.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert!(
        plan.children
            .iter()
            .all(|child| child.task_id.as_str().starts_with("task.workflow.runtime."))
    );
}

#[test]
fn durable_plan_releases_only_the_committed_parallel_frontier_after_rebuild() {
    let request = request(&["a", "b", "c"], 3, 2);
    let planned = prepare_workflow_fan_out(&request).unwrap();
    let durable = durable_workflow_fan_out_plan(&planned, &request.provider, authority()).unwrap();
    let admitted = tracedecay_domain::WorkflowRunEvent::admitted_with_fan_out(
        request.run_id.clone(),
        request.definition,
        request.provider.topology_digest,
        request.provider.provider_registry_digest,
        vec![durable.clone()],
        tracedecay_domain::WorkflowRunEventContext {
            command_id: id("workflow.fan-out.admit"),
            input_digest: digest('f'),
            occurred_at: request.admitted_at,
        },
    )
    .unwrap();
    let projection =
        tracedecay_domain::WorkflowRunProjection::rebuild(std::slice::from_ref(&admitted)).unwrap();
    let released = durable
        .children
        .iter()
        .take(2)
        .map(|child| child.attempt_identity.clone())
        .collect::<Vec<_>>();
    let event = projection
        .next_event(
            tracedecay_domain::WorkflowRunCommand::ReleaseFanOutChildren {
                step_id: durable.step_id.clone(),
                attempts: released.clone(),
            },
            tracedecay_domain::WorkflowRunEventContext {
                command_id: id("workflow.fan-out.release"),
                input_digest: digest('0'),
                occurred_at: UtcMicros(101),
            },
        )
        .unwrap();
    let rebuilt = tracedecay_domain::WorkflowRunProjection::rebuild(&[admitted, event]).unwrap();
    assert_eq!(
        rebuilt
            .released_fan_out_attempts()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        released
    );

    let third = durable.children[2].attempt_identity.clone();
    assert_eq!(
        rebuilt
            .next_event(
                tracedecay_domain::WorkflowRunCommand::ReleaseFanOutChildren {
                    step_id: durable.step_id.clone(),
                    attempts: vec![third.clone()],
                },
                tracedecay_domain::WorkflowRunEventContext {
                    command_id: id("workflow.fan-out.over-capacity"),
                    input_digest: digest('1'),
                    occurred_at: UtcMicros(102),
                },
            )
            .unwrap_err(),
        tracedecay_domain::WorkflowRunStateError::InvalidTransition
    );
    let settled = rebuilt
        .next_event(
            tracedecay_domain::WorkflowRunCommand::SettleFanOutChildren {
                step_id: durable.step_id.clone(),
                attempts: vec![released[0].clone()],
            },
            tracedecay_domain::WorkflowRunEventContext {
                command_id: id("workflow.fan-out.settle"),
                input_digest: digest('2'),
                occurred_at: UtcMicros(103),
            },
        )
        .unwrap();
    let after_settlement = rebuilt.apply(&settled).unwrap();
    let next_release = after_settlement
        .next_event(
            tracedecay_domain::WorkflowRunCommand::ReleaseFanOutChildren {
                step_id: durable.step_id.clone(),
                attempts: vec![third.clone()],
            },
            tracedecay_domain::WorkflowRunEventContext {
                command_id: id("workflow.fan-out.next-release"),
                input_digest: digest('3'),
                occurred_at: UtcMicros(104),
            },
        )
        .unwrap();
    assert!(matches!(
        next_release.event(),
        tracedecay_domain::WorkflowRunEventKind::FanOutChildrenReleased { attempts, .. }
            if attempts == std::slice::from_ref(&third)
    ));

    let cancelling = after_settlement
        .next_event(
            tracedecay_domain::WorkflowRunCommand::RequestCancellation,
            tracedecay_domain::WorkflowRunEventContext {
                command_id: id("workflow.fan-out.cancel"),
                input_digest: digest('4'),
                occurred_at: UtcMicros(105),
            },
        )
        .and_then(|event| after_settlement.apply(&event))
        .unwrap();
    assert_eq!(
        cancelling.status(),
        tracedecay_domain::WorkflowRunStatus::Cancelling
    );
    assert_eq!(
        cancelling
            .next_event(
                tracedecay_domain::WorkflowRunCommand::ReleaseFanOutChildren {
                    step_id: durable.step_id.clone(),
                    attempts: vec![third],
                },
                tracedecay_domain::WorkflowRunEventContext {
                    command_id: id("workflow.fan-out.release-after-cancel"),
                    input_digest: digest('5'),
                    occurred_at: UtcMicros(106),
                },
            )
            .unwrap_err(),
        tracedecay_domain::WorkflowRunStateError::InvalidTransition
    );
}

#[test]
fn planner_rejects_width_parallelism_and_duplicate_violations() {
    assert_eq!(
        prepare_workflow_fan_out(&request(&["a", "b"], 1, 1)).unwrap_err(),
        WorkflowFanOutRuntimeError::FanOutLimitExceeded {
            limit: 1,
            actual: 2,
        }
    );
    assert_eq!(
        prepare_workflow_fan_out(&request(&["a", "b"], 2, 3)).unwrap_err(),
        WorkflowFanOutRuntimeError::InvalidParallelism
    );
    assert_eq!(
        prepare_workflow_fan_out(&request(&["same", "same"], 2, 1)).unwrap_err(),
        WorkflowFanOutRuntimeError::DuplicateChildIdentity("task.workflow.runtime.same".to_owned())
    );
}

#[test]
fn provider_admission_is_part_of_the_immutable_plan() {
    let first = prepare_workflow_fan_out(&request(&["a"], 1, 1)).unwrap();
    let mut changed = request(&["a"], 1, 1);
    changed.provider.execution_snapshot = execution_snapshot("different-model");
    let changed = prepare_workflow_fan_out(&changed).unwrap();

    assert_ne!(first.plan_digest, changed.plan_digest);
    assert_ne!(
        first.children[0].proposal_command_id,
        changed.children[0].proposal_command_id
    );
}

#[test]
fn child_attempt_identity_survives_workflow_fence_renewal() {
    let first = prepare_workflow_fan_out(&request(&["a", "b"], 2, 1)).unwrap();
    let mut retried = request(&["a", "b"], 2, 1);
    retried.fence.attempt_id = id::<AttemptId>("attempt.workflow.runtime.retry");
    retried.fence.lease = WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.workflow.runtime.retry"),
        WorkFenceEpochV1::new(2).unwrap(),
    )
    .unwrap();
    let retried = prepare_workflow_fan_out(&retried).unwrap();

    assert_eq!(first.plan_digest, retried.plan_digest);
    assert_eq!(
        first
            .children
            .iter()
            .map(|child| (&child.task_id, &child.attempt_identity))
            .collect::<Vec<_>>(),
        retried
            .children
            .iter()
            .map(|child| (&child.task_id, &child.attempt_identity))
            .collect::<Vec<_>>()
    );
}
