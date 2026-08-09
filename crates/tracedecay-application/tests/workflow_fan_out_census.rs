use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, WorkflowFailurePolicy, WorkflowFanOutCensusEvidenceV1,
    WorkflowFanOutRequest, WorkflowProviderAdmission, derive_workflow_fan_out_census,
    durable_workflow_fan_out_plan, prepare_workflow_fan_out,
};
use tracedecay_domain::configuration::{
    BranchTopologyKindV1, ReviewTopologyKindV1, safe_work_topology_policy_v1,
};
use tracedecay_domain::{
    ActorId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProjectId,
    ProjectionGenerationId, ProviderId, RepositoryId, RunId, UtcMicros, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptProgressV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1,
    WorkEgressPolicy, WorkEvent, WorkEventKind, WorkExecutableReference, WorkExecutionEnvelopeV1,
    WorkExecutionLimits, WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology,
    WorkFenceEpochV1, WorkFilesystemPolicy, WorkLeaseFenceV1, WorkLeaseId, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionResumeCursorV1, WorkProjectionSequenceRangeV1,
    WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkSandboxPolicy,
    WorkTerminalEvidenceV1, WorkVersion, WorkflowCensusCountV1, WorkflowCensusEvidenceReasonV1,
    WorkflowCensusGenerationV1, WorkflowDefinition, WorkflowFanOut, WorkflowOperationRef,
    WorkflowOutputName, WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext,
    WorkflowRunProjection, WorkflowStep, WorkflowStepId, WorktreeId,
};

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
        id("project.workflow.census"),
        id::<RepositoryId>("repository.workflow.census"),
        id::<WorktreeId>("worktree.workflow.census"),
        id::<ActorId>("actor.workflow.census"),
        digest('9'),
    )
    .unwrap()
}

struct Fixture {
    projection: WorkflowRunProjection,
    generation: ProjectionGenerationId,
    snapshot: WorkProjectionSnapshotV1,
    snapshot_accepted_only: WorkProjectionSnapshotV1,
    attempts: Vec<tracedecay_domain::WorkAttemptV1>,
    prior_attempts: Vec<tracedecay_domain::WorkAttemptV1>,
    non_duplicates: BTreeSet<WorkAttemptIdentityV1>,
    runnable: BTreeSet<WorkAttemptIdentityV1>,
    blocked: BTreeSet<WorkAttemptIdentityV1>,
    shared_waits: BTreeSet<WorkAttemptIdentityV1>,
}

fn fixture() -> Fixture {
    let mut topology = safe_work_topology_policy_v1();
    topology.branch_topology.allowed = BTreeSet::from([BranchTopologyKindV1::NoBranches]);
    topology.review_topology.allowed = BTreeSet::from([ReviewTopologyKindV1::NoReview]);
    let topology_digest = topology.compute_digest().unwrap().0;
    let provider_registry_digest = digest('e');
    let definition = WorkflowDefinition::new(
        id("workflow.definition.census"),
        1,
        id::<ProjectId>("project.workflow.census"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOut { max_width: 2 }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap();
    let snapshot = execution_snapshot(&topology, digest('b'));
    let provider = WorkflowProviderAdmission {
        execution_snapshot: snapshot,
        topology_digest: topology_digest.clone(),
        provider_registry_digest: provider_registry_digest.clone(),
        worktree_placement: topology.placement.clone(),
        reference: None,
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        cancellation_generation: 1,
        effect_state: WorkEffectStateV1::Observational,
    };
    let run_id = id::<RunId>("run.workflow.census");
    let request = WorkflowFanOutRequest {
        definition: definition.clone(),
        run_id: run_id.clone(),
        step_id: id("fan-out"),
        fence: tracedecay_application::WorkflowExecutionFence {
            attempt_id: id("attempt.workflow.census.fence"),
            lease: WorkLeaseFenceV1::new(
                id::<WorkLeaseId>("lease.workflow.census.fence"),
                WorkFenceEpochV1::new(1).unwrap(),
            )
            .unwrap(),
        },
        admitted_at: UtcMicros(100),
        cancellation: CancellationContext::active("cancel.workflow.census").unwrap(),
        max_parallel: 2,
        failure_policy: WorkflowFailurePolicy::Collect,
        provider,
        inputs: vec![
            tracedecay_application::WorkflowFanOutInput {
                identity: "first".to_owned(),
                input_digest: digest('1'),
            },
            tracedecay_application::WorkflowFanOutInput {
                identity: "second".to_owned(),
                input_digest: digest('2'),
            },
        ],
    };
    let planned = prepare_workflow_fan_out(&request).unwrap();
    let durable =
        durable_workflow_fan_out_plan(&planned, &request.provider, fan_out_authority()).unwrap();
    let admitted = WorkflowRunEvent::admitted_with_fan_out(
        run_id,
        definition,
        topology_digest,
        provider_registry_digest,
        vec![durable.clone()],
        WorkflowRunEventContext {
            command_id: id("command.workflow.census.admit"),
            input_digest: digest('3'),
            occurred_at: UtcMicros(100),
        },
    )
    .unwrap();
    let projection = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    let generation = ProjectionGenerationId::new("generation.workflow.census.fixture").unwrap();
    let accepted_projections = durable
        .children
        .iter()
        .map(|child| work_projection(child, false))
        .collect::<Vec<_>>();
    let admitted_projections = durable
        .children
        .iter()
        .map(|child| work_projection(child, true))
        .collect::<Vec<_>>();
    let snapshot = WorkProjectionSnapshotV1::new(
        generation.clone(),
        WorkProjectionSequenceV1::new(3),
        admitted_projections,
        WorkProjectionCoverageV1::complete(2, 2).unwrap(),
    )
    .unwrap();
    let snapshot_accepted_only = WorkProjectionSnapshotV1::new(
        generation.clone(),
        WorkProjectionSequenceV1::new(2),
        accepted_projections,
        WorkProjectionCoverageV1::complete(2, 2).unwrap(),
    )
    .unwrap();
    let attempts = durable
        .children
        .iter()
        .map(|child| work_attempt(child, &generation, 2, &durable.execution_snapshot))
        .collect::<Vec<_>>();
    let prior_attempts = durable
        .children
        .iter()
        .map(|child| work_attempt_without_progress(child, &generation, &durable.execution_snapshot))
        .collect::<Vec<_>>();
    let non_duplicates: BTreeSet<WorkAttemptIdentityV1> = durable
        .children
        .iter()
        .map(|child| child.attempt_identity.clone())
        .collect();
    Fixture {
        projection,
        generation,
        snapshot,
        snapshot_accepted_only,
        attempts,
        prior_attempts,
        non_duplicates,
        runnable: BTreeSet::new(),
        blocked: BTreeSet::new(),
        shared_waits: BTreeSet::new(),
    }
}

fn execution_snapshot(
    topology: &tracedecay_domain::WorkTopologyPolicyV1,
    configuration_digest: ManifestDigest,
) -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.census"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.census"),
        effective_behavior_digest: configuration_digest,
        resolution_provenance_digest: digest('d'),
        route: WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.codex-app-server"),
            id::<WorkProviderRouteId>("route.workflow.census"),
        )
        .unwrap(),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: "gpt-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.workflow.census".to_owned(),
            digest('f'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 2).unwrap(),
        deadline: UtcMicros(10_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: topology.clone(),
    })
    .unwrap()
}

fn work_projection(
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    execution_admitted: bool,
) -> WorkProjection {
    let task_id = child.task_id.clone();
    let authority = WorkAuthority::new(
        id::<ProjectId>("project.workflow.census"),
        id::<RepositoryId>("repository.workflow.census"),
        id::<WorktreeId>("worktree.workflow.census"),
        id("actor.workflow.census"),
        digest('9'),
    )
    .unwrap();
    let mut history = vec![
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::initial(),
            authority.clone(),
            UtcMicros(1),
            child.create_command_id.clone(),
            digest('a'),
            WorkEventKind::Created {
                title: child.instructions.clone(),
                dependencies: BTreeSet::new(),
            },
        )
        .unwrap(),
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::new(2).unwrap(),
            authority.clone(),
            UtcMicros(2),
            child.proposal_command_id.clone(),
            digest('b'),
            WorkEventKind::ProposalAccepted {
                proposal_id: child.proposal_id.clone(),
                proposal_digest: child.proposal_digest.clone(),
            },
        )
        .unwrap(),
    ];
    if execution_admitted {
        history.push(
            WorkEvent::new(
                task_id,
                WorkVersion::new(3).unwrap(),
                authority,
                UtcMicros(3),
                child.admit_command_id.clone(),
                digest('c'),
                WorkEventKind::ExecutionAdmitted,
            )
            .unwrap(),
        );
    }
    WorkProjection::rebuild(&history).unwrap()
}

fn work_attempt(
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    generation: &ProjectionGenerationId,
    completed: u64,
    snapshot: &WorkExecutionSnapshot,
) -> tracedecay_domain::WorkAttemptV1 {
    work_attempt_with_progress(
        child,
        generation,
        Some(WorkAttemptProgressV1::new(completed, 10).unwrap()),
        snapshot,
    )
}

fn work_attempt_without_progress(
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    generation: &ProjectionGenerationId,
    snapshot: &WorkExecutionSnapshot,
) -> tracedecay_domain::WorkAttemptV1 {
    work_attempt_with_progress(child, generation, None, snapshot)
}

fn work_attempt_with_progress(
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    generation: &ProjectionGenerationId,
    progress: Option<WorkAttemptProgressV1>,
    snapshot: &WorkExecutionSnapshot,
) -> tracedecay_domain::WorkAttemptV1 {
    let binding = WorkAttemptProjectionBindingV1::new(
        generation.clone(),
        WorkProjectionSequenceV1::new(3),
        WorkVersion::new(3).unwrap(),
        child.proposal_id.clone(),
    )
    .unwrap();
    let identity = child.attempt_identity.clone();
    let execution = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.work.attempt_start"),
        snapshot.clone(),
        id::<ProjectId>("project.workflow.census"),
        id::<RepositoryId>("repository.workflow.census"),
        id::<WorktreeId>("worktree.workflow.census"),
        "/tmp/workflow-census".to_owned(),
        None,
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        child.instructions.clone(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    let route = snapshot.route().clone();
    tracedecay_domain::WorkAttemptV1::new(
        identity,
        binding,
        execution,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.workflow.census.attempt"),
            WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Running,
        progress,
        Vec::new(),
        WorkCancellationStateV1::None,
        tracedecay_domain::WorkRecoveryStateV1::Fresh,
        route.clone(),
        Some(route),
        None,
    )
    .unwrap()
}

fn terminal_work_attempt(
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    generation: &ProjectionGenerationId,
    completed: u64,
    snapshot: &WorkExecutionSnapshot,
    terminal_at: i64,
) -> tracedecay_domain::WorkAttemptV1 {
    let attempt = work_attempt(child, generation, completed, snapshot);
    let route = snapshot.route().clone();
    attempt
        .transition(
            WorkAttemptStateV1::Succeeded,
            Some(WorkAttemptProgressV1::new(completed, 10).unwrap()),
            Vec::new(),
            WorkCancellationStateV1::None,
            tracedecay_domain::WorkRecoveryStateV1::Fresh,
            Some(route),
            Some(WorkTerminalEvidenceV1::succeeded(digest('7'), UtcMicros(terminal_at)).unwrap()),
            attempt.lease().clone(),
        )
        .unwrap()
}

fn census_evidence<'a>(
    fixture: &'a Fixture,
    snapshot: Option<&'a WorkProjectionSnapshotV1>,
    attempts: &'a [tracedecay_domain::WorkAttemptV1],
    previous: Option<&'a tracedecay_domain::WorkflowFanOutCensusV1>,
    non_duplicates: Option<&'a BTreeSet<WorkAttemptIdentityV1>>,
    observed_at: i64,
) -> WorkflowFanOutCensusEvidenceV1<'a> {
    WorkflowFanOutCensusEvidenceV1 {
        work_snapshot: snapshot,
        attempts,
        attempt_reads_complete: true,
        shared_authority_waits: Some(&fixture.shared_waits),
        non_duplicate_attempts: non_duplicates,
        runnable_children: Some(&fixture.runnable),
        blocked_children: Some(&fixture.blocked),
        previous,
        observed_at: UtcMicros(observed_at),
    }
}

fn count(value: &WorkflowCensusCountV1) -> Option<u16> {
    value.known()
}

#[test]
fn complete_evidence_produces_exact_counts_and_sample_after_frontier_advance() {
    let fixture = fixture();
    let first_evidence = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &fixture.prior_attempts,
        None,
        Some(&fixture.non_duplicates),
        150,
    );
    let mut previous =
        derive_workflow_fan_out_census(&fixture.projection, &first_evidence).unwrap();
    previous.useful_width = WorkflowCensusCountV1::Known { value: 0 };
    previous.validate().unwrap();
    let current_non_duplicates = BTreeSet::from([fixture.attempts[0].identity().clone()]);
    let current_evidence = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &fixture.attempts,
        Some(&previous),
        Some(&current_non_duplicates),
        200,
    );
    let census = derive_workflow_fan_out_census(&fixture.projection, &current_evidence).unwrap();

    assert_eq!(count(&census.requested_width), Some(2));
    assert_eq!(count(&census.accepted_width), Some(2));
    assert_eq!(count(&census.admitted_width), Some(2));
    assert_eq!(count(&census.active_width), Some(2));
    assert_eq!(count(&census.useful_width), Some(1));
    assert_eq!(count(&census.runnable_count), Some(0));
    assert_eq!(count(&census.blocked_count), Some(0));
    assert_eq!(count(&census.shared_authority_serialized_count), Some(0));
    assert!(matches!(
        census.work_generation,
        WorkflowCensusGenerationV1::Exact { .. }
    ));
    assert_eq!(
        census.observed_duration,
        tracedecay_domain::WorkflowCensusDurationV1::Known { micros: 100 }
    );
    assert!(census.execution_topology_sample().is_some());
}

#[test]
fn partial_or_unavailable_evidence_never_flattens_to_a_sample() {
    let fixture = fixture();
    let evidence = WorkflowFanOutCensusEvidenceV1 {
        work_snapshot: None,
        attempts: &[],
        attempt_reads_complete: false,
        shared_authority_waits: None,
        non_duplicate_attempts: None,
        runnable_children: None,
        blocked_children: None,
        previous: None,
        observed_at: UtcMicros(200),
    };
    let census = derive_workflow_fan_out_census(&fixture.projection, &evidence).unwrap();

    assert!(census.execution_topology_sample().is_none());
    assert!(matches!(
        census.work_generation,
        WorkflowCensusGenerationV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable
        }
    ));
    assert!(matches!(
        census.accepted_width,
        WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable
        }
    ));
    assert!(matches!(
        census.active_width,
        WorkflowCensusCountV1::Partial {
            observed: 0,
            reason: WorkflowCensusEvidenceReasonV1::AttemptUnavailable
        }
    ));
}

#[test]
fn partial_work_projection_keeps_generation_and_widths_typed() {
    let mut fixture = fixture();
    let second_identity = fixture.attempts[1].identity().clone();
    fixture.blocked.insert(second_identity);
    let partial_snapshot = WorkProjectionSnapshotV1::new(
        fixture.generation.clone(),
        WorkProjectionSequenceV1::new(3),
        fixture.snapshot_accepted_only.projections()[..1].to_vec(),
        WorkProjectionCoverageV1::partial(
            1,
            2,
            WorkProjectionSequenceRangeV1::new(
                WorkProjectionSequenceV1::new(0),
                WorkProjectionSequenceV1::new(3),
            )
            .unwrap(),
            WorkProjectionResumeCursorV1::new(fixture.generation.clone(), "fixture.next").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let partial_non_duplicates = BTreeSet::from([fixture.attempts[0].identity().clone()]);
    let evidence = census_evidence(
        &fixture,
        Some(&partial_snapshot),
        &fixture.attempts[..1],
        None,
        Some(&partial_non_duplicates),
        200,
    );
    let census = derive_workflow_fan_out_census(&fixture.projection, &evidence).unwrap();

    assert!(matches!(
        census.work_generation,
        WorkflowCensusGenerationV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable
        }
    ));
    assert!(matches!(
        census.accepted_width,
        WorkflowCensusCountV1::Partial {
            observed: 0,
            reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable
        }
    ));
    assert!(census.execution_topology_sample().is_none());
}

#[test]
fn work_generation_mismatch_is_typed_and_does_not_claim_exact_activity() {
    let fixture = fixture();
    let other_generation = ProjectionGenerationId::new("generation.workflow.census.other").unwrap();
    let mismatched = fixture
        .attempts
        .iter()
        .map(|attempt| {
            let child = fixture
                .projection
                .fan_out_plans()
                .values()
                .flat_map(|plan| &plan.children)
                .find(|child| child.attempt_identity == attempt.identity().clone())
                .unwrap();
            work_attempt(child, &other_generation, 2, &plan_snapshot(&fixture))
        })
        .collect::<Vec<_>>();
    let first = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &fixture.prior_attempts,
        None,
        Some(&fixture.non_duplicates),
        150,
    );
    let mut previous = derive_workflow_fan_out_census(&fixture.projection, &first).unwrap();
    previous.work_generation = WorkflowCensusGenerationV1::Exact {
        generation_id: other_generation.clone(),
    };
    previous.validate().unwrap();
    let current = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &mismatched,
        Some(&previous),
        Some(&fixture.non_duplicates),
        200,
    );
    let census = derive_workflow_fan_out_census(&fixture.projection, &current).unwrap();

    assert!(matches!(
        census.active_width,
        WorkflowCensusCountV1::Partial {
            reason: WorkflowCensusEvidenceReasonV1::WorkGenerationMismatch,
            ..
        }
    ));
    assert!(matches!(
        census.useful_width,
        WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkGenerationMismatch
        }
    ));
}

fn plan_snapshot(fixture: &Fixture) -> WorkExecutionSnapshot {
    fixture
        .projection
        .fan_out_plans()
        .values()
        .next()
        .unwrap()
        .execution_snapshot
        .clone()
}

#[test]
fn released_before_work_admission_remains_unadmitted() {
    let fixture = fixture();
    let step_id = fixture
        .projection
        .fan_out_plans()
        .keys()
        .next()
        .unwrap()
        .clone();
    let released = fixture
        .projection
        .next_event(
            WorkflowRunCommand::ReleaseFanOutChildren {
                step_id,
                attempts: vec![fixture.attempts[0].identity().clone()],
            },
            WorkflowRunEventContext {
                command_id: id("command.workflow.census.release"),
                input_digest: digest('4'),
                occurred_at: UtcMicros(110),
            },
        )
        .and_then(|event| fixture.projection.apply(&event))
        .unwrap();
    let runnable = BTreeSet::from([fixture.attempts[0].identity().clone()]);
    let blocked = BTreeSet::from([fixture.attempts[1].identity().clone()]);
    let evidence = WorkflowFanOutCensusEvidenceV1 {
        work_snapshot: Some(&fixture.snapshot_accepted_only),
        attempts: &[],
        attempt_reads_complete: true,
        shared_authority_waits: Some(&fixture.shared_waits),
        non_duplicate_attempts: None,
        runnable_children: Some(&runnable),
        blocked_children: Some(&blocked),
        previous: None,
        observed_at: UtcMicros(200),
    };
    let census = derive_workflow_fan_out_census(&released, &evidence).unwrap();

    assert_eq!(count(&census.accepted_width), Some(2));
    assert_eq!(count(&census.admitted_width), Some(0));
    assert_eq!(count(&census.active_width), Some(0));
    assert_eq!(count(&census.runnable_count), Some(1));
    assert_eq!(count(&census.blocked_count), Some(1));
}

#[test]
fn two_live_attempts_allow_one_missing_progress_frontier() {
    let fixture = fixture();
    let first_evidence = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &fixture.prior_attempts,
        None,
        Some(&fixture.non_duplicates),
        150,
    );
    let mut previous =
        derive_workflow_fan_out_census(&fixture.projection, &first_evidence).unwrap();
    previous.useful_width = WorkflowCensusCountV1::Known { value: 0 };
    previous.validate().unwrap();

    let children = fixture
        .projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .collect::<Vec<_>>();
    let first_child = children
        .iter()
        .find(|child| child.attempt_identity == fixture.attempts[0].identity().clone())
        .unwrap();
    let second_child = children
        .iter()
        .find(|child| child.attempt_identity == fixture.attempts[1].identity().clone())
        .unwrap();
    let snapshot = plan_snapshot(&fixture);
    let current_attempts = vec![
        work_attempt(first_child, &fixture.generation, 1, &snapshot),
        work_attempt_without_progress(second_child, &fixture.generation, &snapshot),
    ];
    let non_duplicates = BTreeSet::from([first_child.attempt_identity.clone()]);
    let current_evidence = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &current_attempts,
        Some(&previous),
        Some(&non_duplicates),
        200,
    );
    let census = derive_workflow_fan_out_census(&fixture.projection, &current_evidence).unwrap();

    assert_eq!(count(&census.active_width), Some(2));
    assert_eq!(count(&census.useful_width), Some(1));
    assert!(census.execution_topology_sample().is_some());
}

#[test]
fn terminal_transition_in_interval_counts_active_and_useful_once() {
    let fixture = fixture();
    let first_evidence = census_evidence(
        &fixture,
        Some(&fixture.snapshot),
        &fixture.prior_attempts,
        None,
        Some(&fixture.non_duplicates),
        150,
    );
    let mut previous =
        derive_workflow_fan_out_census(&fixture.projection, &first_evidence).unwrap();
    previous.useful_width = WorkflowCensusCountV1::Known { value: 0 };
    previous.validate().unwrap();

    let mut current_fixture = fixture;
    let second_identity = current_fixture.attempts[1].identity().clone();
    current_fixture.blocked.insert(second_identity);
    let first_child = current_fixture
        .projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .find(|child| child.attempt_identity == current_fixture.attempts[0].identity().clone())
        .unwrap();
    let terminal = terminal_work_attempt(
        first_child,
        &current_fixture.generation,
        2,
        &plan_snapshot(&current_fixture),
        180,
    );
    let current_attempts = vec![terminal];
    let current_non_duplicates = BTreeSet::from([first_child.attempt_identity.clone()]);
    let current_evidence = census_evidence(
        &current_fixture,
        Some(&current_fixture.snapshot),
        &current_attempts,
        Some(&previous),
        Some(&current_non_duplicates),
        200,
    );
    let census =
        derive_workflow_fan_out_census(&current_fixture.projection, &current_evidence).unwrap();

    assert_eq!(census.interval_started_at, UtcMicros(150));
    assert_eq!(count(&census.active_width), Some(1));
    assert_eq!(count(&census.useful_width), Some(1));
    assert!(census.execution_topology_sample().is_some());

    let zero_terminal = terminal_work_attempt(
        first_child,
        &current_fixture.generation,
        0,
        &plan_snapshot(&current_fixture),
        180,
    );
    let zero_evidence = census_evidence(
        &current_fixture,
        Some(&current_fixture.snapshot),
        std::slice::from_ref(&zero_terminal),
        Some(&previous),
        Some(&current_non_duplicates),
        200,
    );
    let zero_census =
        derive_workflow_fan_out_census(&current_fixture.projection, &zero_evidence).unwrap();
    assert_eq!(count(&zero_census.useful_width), Some(0));
}
