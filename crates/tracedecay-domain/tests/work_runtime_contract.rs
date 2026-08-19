use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use tracedecay_domain::{
    AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProposalId, ProviderId, RefId, RepositoryId, RunId, SourceStoreId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkCancellationAcknowledgementV1, WorkCancellationEscalationV1, WorkCancellationRequestId,
    WorkCancellationRequestV1, WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphChangeV1, WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1,
    WorkItemV1, WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProductEventSequenceV1, WorkProductGraphV1, WorkProductSourceWatermarkV1, WorkProposalV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkRestartReasonV1, WorkRouteDecisionV1, WorkSandboxPolicy,
    WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1, WorkTerminalEvidenceV1,
    WorkflowOperationRef, WorktreeId, safe_work_topology_policy_v1,
};

/// Platform-absolute fixture root: the work contracts require
/// `Path::is_absolute`, which a bare `/...` literal fails on Windows.
fn fixture_abs_root(posix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", posix.replace('/', "\\"))
    } else {
        posix.to_owned()
    }
}

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

fn route(provider: &str, route: &str) -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route)).unwrap()
}

fn identity() -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>("task.work.runtime"),
        id::<RunId>("run.work.runtime"),
        id::<AttemptId>("attempt.work.runtime.1"),
    )
    .unwrap()
}

fn lease(epoch: u64) -> WorkLeaseFenceV1 {
    WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.work.runtime"),
        WorkFenceEpochV1::new(epoch).unwrap(),
    )
    .unwrap()
}

fn binding() -> WorkAttemptProjectionBindingV1 {
    WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        source_watermark(),
        digest('7'),
        id::<ProposalId>("proposal.work.runtime"),
    )
    .unwrap()
}

fn source_watermark() -> WorkProductSourceWatermarkV1 {
    WorkProductSourceWatermarkV1::new(BTreeMap::from([(
        id::<SourceStoreId>("source.work.runtime"),
        7,
    )]))
    .unwrap()
}

fn requested_route() -> WorkProviderRouteV1 {
    route(
        "provider.work.codex-app-server",
        "route.work.codex-app-server.v1",
    )
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.work.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.work.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: requested_route(),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: "gpt-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.codex.app-server".to_owned(),
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
        topology: safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn execution(
    attempt_identity: WorkAttemptIdentityV1,
    projection_binding: WorkAttemptProjectionBindingV1,
) -> WorkExecutionEnvelopeV1 {
    WorkExecutionEnvelopeV1::new(
        attempt_identity,
        projection_binding,
        id::<WorkflowOperationRef>("operation.work.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>("project.work.runtime"),
        id::<RepositoryId>("repository.work.runtime"),
        id::<WorktreeId>("worktree.work.runtime"),
        fixture_abs_root("/tmp/work-runtime"),
        Some(id::<RefId>("refs/heads/work-runtime")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap()
}

fn admitted_graph(identity: WorkAttemptIdentityV1) -> WorkProductGraphV1 {
    let task_id = identity.task_id().clone();
    let graph = WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id("initiative.work.runtime"),
                "Work runtime".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id::<WorkPlanId>("plan.work.runtime"),
                id("initiative.work.runtime"),
                "Runtime plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            WorkMilestoneV1::new(
                id("milestone.work.runtime"),
                id("plan.work.runtime"),
                "Runtime milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        vec![
            WorkItemV1::new(WorkItemInputV1 {
                task_id: task_id.clone(),
                hierarchy: WorkHierarchyV1::new(
                    id("initiative.work.runtime"),
                    id("plan.work.runtime"),
                    id("milestone.work.runtime"),
                ),
                title: "Execute Work runtime".to_owned(),
                dependencies: BTreeSet::new(),
                informational_relations: BTreeSet::new(),
                causal_candidates: BTreeSet::new(),
                acceptance_criteria: Vec::new(),
                effort: 1,
                scheduled_at: None,
                deadline: None,
                created_at: UtcMicros(1),
                updated_at: UtcMicros(1),
            })
            .unwrap(),
        ],
    )
    .unwrap();
    let proposal = WorkProposalV1::new(
        id::<ProposalId>("proposal.work.runtime"),
        task_id.clone(),
        graph.version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Heuristic, 1, 1, 1, "bounded").unwrap(),
        Vec::new(),
        WorkRouteDecisionV1::abstain("execution admission pins the provider").unwrap(),
        "Admit the runtime attempt".to_owned(),
        digest('f'),
    )
    .unwrap();
    let graph = graph
        .apply(WorkGraphChangeV1::ProposalAccepted {
            proposal,
            accepted_at: UtcMicros(2),
        })
        .unwrap();
    let based_on_version = graph.version();
    let graph = graph
        .apply(WorkGraphChangeV1::ExecutionAdmitted {
            task_id: task_id.clone(),
            based_on_version,
            admitted_at: UtcMicros(3),
        })
        .unwrap();
    let based_on_version = graph.version();
    graph
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id,
            based_on_version,
            identity,
            linked_at: UtcMicros(4),
        })
        .unwrap()
}

fn running() -> WorkAttemptV1 {
    let identity = identity();
    let binding = binding();
    WorkAttemptV1::new(
        identity.clone(),
        binding.clone(),
        execution(identity, binding),
        lease(1),
        WorkAttemptStateV1::Running,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        Some(route("provider.work.actual", "route.work.actual")),
        None,
    )
    .unwrap()
}

#[test]
fn attempt_identity_fence_and_projection_binding_are_validated() {
    assert!(AttemptId::new("attempt.stable").is_ok());
    assert!(WorkFenceEpochV1::new(0).is_err());
    assert!(serde_json::from_value::<WorkFenceEpochV1>(json!(0)).is_err());

    let attempt = running();
    let admitted = admitted_graph(attempt.identity().clone());
    attempt.validate_graph_admission(&admitted).unwrap();

    let wrong_identity = WorkAttemptIdentityV1::new(
        id("task.work.other"),
        id("run.work.runtime"),
        id("attempt.work.runtime.1"),
    )
    .unwrap();
    let wrong_binding = binding();
    let wrong_task = WorkAttemptV1::new(
        wrong_identity.clone(),
        wrong_binding.clone(),
        execution(wrong_identity, wrong_binding),
        lease(1),
        WorkAttemptStateV1::Running,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        Some(route("provider.work.actual", "route.work.actual")),
        None,
    )
    .unwrap();
    assert!(wrong_task.validate_graph_admission(&admitted).is_err());
    assert_eq!(
        attempt.projection_binding().graph_version().next().unwrap(),
        admitted.version()
    );
    assert_eq!(attempt.projection_binding().event_sequence().get(), 7);
    assert_eq!(
        attempt.projection_binding().source_watermark(),
        &source_watermark()
    );
    assert_eq!(
        attempt.projection_binding().recovered_graph_digest(),
        &digest('7')
    );
    assert_eq!(
        attempt.projection_binding().accepted_proposal(),
        &id::<ProposalId>("proposal.work.runtime")
    );
}

#[test]
fn progress_artifacts_and_provider_routes_are_bounded_and_explicit() {
    assert!(tracedecay_domain::WorkAttemptProgressV1::new(4, 10).is_ok());
    assert!(tracedecay_domain::WorkAttemptProgressV1::new(11, 10).is_err());

    let artifact = WorkArtifactRefV1::new(
        id::<WorkArtifactId>("artifact.work.runtime"),
        digest('f'),
        64,
    )
    .unwrap();
    let attempt_identity = identity();
    let projection_binding = binding();
    let attempt = WorkAttemptV1::new(
        attempt_identity.clone(),
        projection_binding.clone(),
        execution(attempt_identity, projection_binding),
        lease(1),
        WorkAttemptStateV1::Running,
        Some(tracedecay_domain::WorkAttemptProgressV1::new(4, 10).unwrap()),
        vec![artifact],
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        Some(route("provider.work.actual", "route.work.actual")),
        None,
    )
    .unwrap();

    assert_ne!(attempt.requested_route(), attempt.actual_route().unwrap());
    assert_eq!(attempt.artifacts().len(), 1);
    assert!(
        attempt
            .transition(
                WorkAttemptStateV1::Running,
                Some(tracedecay_domain::WorkAttemptProgressV1::new(3, 10).unwrap()),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::None,
                WorkRecoveryStateV1::Fresh,
                attempt.actual_route().cloned(),
                None,
                lease(1),
            )
            .is_err()
    );
}

#[test]
fn cancellation_request_acknowledgement_and_escalation_are_ordered() {
    let request = WorkCancellationRequestV1::new(
        id::<WorkCancellationRequestId>("cancel.work.runtime"),
        UtcMicros(10),
    )
    .unwrap();
    let acknowledged =
        WorkCancellationAcknowledgementV1::new(request.clone(), UtcMicros(11)).unwrap();
    let escalated = WorkCancellationEscalationV1::new(acknowledged.clone(), UtcMicros(12)).unwrap();

    let requested = running()
        .transition(
            WorkAttemptStateV1::CancellationRequested,
            None,
            Vec::new(),
            WorkCancellationStateV1::Requested(request),
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(1),
        )
        .unwrap();
    let acknowledged_attempt = requested
        .transition(
            WorkAttemptStateV1::CancellationAcknowledged,
            None,
            Vec::new(),
            WorkCancellationStateV1::Acknowledged(acknowledged),
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(1),
        )
        .unwrap();
    acknowledged_attempt
        .transition(
            WorkAttemptStateV1::CancellationEscalated,
            None,
            Vec::new(),
            WorkCancellationStateV1::Escalated(escalated),
            WorkRecoveryStateV1::Fresh,
            Some(route("provider.work.actual", "route.work.actual")),
            None,
            lease(2),
        )
        .unwrap();

    assert!(
        running()
            .transition(
                WorkAttemptStateV1::CancellationEscalated,
                None,
                Vec::new(),
                WorkCancellationStateV1::None,
                WorkRecoveryStateV1::Fresh,
                Some(route("provider.work.actual", "route.work.actual")),
                None,
                lease(1),
            )
            .is_err()
    );
}

#[test]
fn recovery_and_terminal_evidence_match_attempt_state() {
    let recovery = WorkRecoveryStateV1::Restarted {
        source_attempt_id: id("attempt.work.runtime.0"),
        reason: WorkRestartReasonV1::LeaseLost,
    };
    let attempt_identity = identity();
    let projection_binding = binding();
    let restarted = WorkAttemptV1::new(
        attempt_identity.clone(),
        projection_binding.clone(),
        execution(attempt_identity, projection_binding),
        lease(2),
        WorkAttemptStateV1::Running,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        recovery,
        requested_route(),
        Some(route("provider.work.actual", "route.work.actual")),
        None,
    )
    .unwrap();
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('9'), UtcMicros(20)).unwrap();
    let succeeded = restarted
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            restarted.recovery().clone(),
            restarted.actual_route().cloned(),
            Some(terminal.clone()),
            lease(2),
        )
        .unwrap();

    assert!(succeeded.is_terminal());
    assert_eq!(
        terminal
            .runtime_evidence_ref(succeeded.identity().run_id().clone())
            .unwrap()
            .run_id(),
        succeeded.identity().run_id()
    );

    let mut forged = serde_json::to_value(succeeded).unwrap();
    forged["state"] = json!("failed");
    assert!(serde_json::from_value::<WorkAttemptV1>(forged).is_err());
}

#[test]
fn a_first_attempt_can_require_recovery_without_naming_a_predecessor() {
    let attempt = |recovery| {
        let attempt_identity = identity();
        let projection_binding = binding();
        WorkAttemptV1::new(
            attempt_identity.clone(),
            projection_binding.clone(),
            execution(attempt_identity, projection_binding),
            lease(2),
            WorkAttemptStateV1::RecoveryRequired,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            recovery,
            requested_route(),
            None,
            None,
        )
    };

    let orphan = attempt(WorkRecoveryStateV1::RecoveryRequired {
        source_attempt_id: None,
        reason: WorkRestartReasonV1::ProcessLost,
    })
    .expect("a lost first attempt has no predecessor to name");
    assert_eq!(orphan.recovery().source_attempt_id(), None);

    let successor = attempt(WorkRecoveryStateV1::RecoveryRequired {
        source_attempt_id: Some(id::<AttemptId>("attempt.work.runtime.0")),
        reason: WorkRestartReasonV1::ProcessLost,
    })
    .expect("a later attempt names the predecessor it recovers");
    assert_eq!(
        successor.recovery().source_attempt_id(),
        Some(&id::<AttemptId>("attempt.work.runtime.0"))
    );

    assert!(
        attempt(WorkRecoveryStateV1::RecoveryRequired {
            source_attempt_id: Some(identity().attempt_id().clone()),
            reason: WorkRestartReasonV1::ProcessLost,
        })
        .is_err(),
        "an attempt must never recover from itself"
    );
}

#[test]
fn persisted_recovery_required_payloads_survive_the_optional_predecessor() {
    let orphan = WorkRecoveryStateV1::RecoveryRequired {
        source_attempt_id: None,
        reason: WorkRestartReasonV1::LeaseLost,
    };
    let encoded = serde_json::to_value(&orphan).unwrap();
    assert_eq!(encoded["state"], json!("recovery_required"));
    assert_eq!(encoded["source_attempt_id"], json!(null));
    assert_eq!(
        serde_json::from_value::<WorkRecoveryStateV1>(encoded).unwrap(),
        orphan
    );

    // A payload written before the predecessor became optional still names one.
    assert_eq!(
        serde_json::from_value::<WorkRecoveryStateV1>(json!({
            "state": "recovery_required",
            "source_attempt_id": "attempt.work.runtime.0",
            "reason": "process_lost"
        }))
        .unwrap(),
        WorkRecoveryStateV1::RecoveryRequired {
            source_attempt_id: Some(id::<AttemptId>("attempt.work.runtime.0")),
            reason: WorkRestartReasonV1::ProcessLost,
        }
    );

    // A payload that omits the field entirely reads as no predecessor.
    assert_eq!(
        serde_json::from_value::<WorkRecoveryStateV1>(json!({
            "state": "recovery_required",
            "reason": "lease_lost"
        }))
        .unwrap(),
        orphan
    );
}
