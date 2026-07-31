use std::collections::BTreeSet;

use schemars::{JsonSchema, schema_for};
use serde_json::json;
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ManifestDigest, ProjectId, ProjectionGenerationId, ProposalId,
    ProviderId, RefId, RepositoryId, RunId, TaskId, UtcMicros, WorkArtifactId, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkAuthority, WorkCancellationAcknowledgementV1, WorkCancellationEscalationV1,
    WorkCancellationRequestId, WorkCancellationRequestV1, WorkCancellationStateV1,
    WorkEffectStateV1, WorkEvent, WorkEventKind, WorkExecutionBudgetV1, WorkExecutionEnvelopeV1,
    WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId, WorkProjection, WorkProjectionCoverageV1,
    WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkProviderBackendV1, WorkProviderRouteId,
    WorkProviderRouteV1, WorkRecoveryStateV1, WorkRestartReasonV1, WorkTerminalEvidenceV1,
    WorkVersion, WorkflowOperationRef, WorktreeId,
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
        id::<ProjectionGenerationId>("generation.work.runtime"),
        WorkProjectionSequenceV1::new(7),
        WorkVersion::new(3).unwrap(),
    )
    .unwrap()
}

fn requested_route() -> WorkProviderRouteV1 {
    route(
        "provider.work.codex-app-server",
        "route.work.codex-app-server.v1",
    )
}

fn execution(
    attempt_identity: WorkAttemptIdentityV1,
    projection_binding: WorkAttemptProjectionBindingV1,
) -> WorkExecutionEnvelopeV1 {
    WorkExecutionEnvelopeV1::new(
        attempt_identity,
        projection_binding,
        id::<WorkflowOperationRef>("operation.work.execute-provider"),
        requested_route(),
        WorkProviderBackendV1::CodexAppServer,
        "gpt-test".to_owned(),
        digest('c'),
        id::<ProjectId>("project.work.runtime"),
        id::<RepositoryId>("repository.work.runtime"),
        id::<WorktreeId>("worktree.work.runtime"),
        "/tmp/work-runtime".to_owned(),
        Some(id::<RefId>("refs/heads/work-runtime")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        UtcMicros(1_000_000),
        1,
        WorkExecutionBudgetV1::new(16_384, 16_384, 65_536).unwrap(),
        WorkEffectStateV1::Observational,
    )
    .unwrap()
}

fn schema_name<T: JsonSchema>() -> String {
    serde_json::to_value(schema_for!(T)).unwrap()["title"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn projection() -> WorkProjection {
    let authority = WorkAuthority::new(
        id::<ProjectId>("project.work.runtime"),
        id::<RepositoryId>("repository.work.runtime"),
        id::<WorktreeId>("worktree.work.runtime"),
        id::<ActorId>("actor.work.runtime"),
        digest('a'),
    )
    .unwrap();
    let task_id = id::<TaskId>("task.work.runtime");
    let proposal_id = id::<ProposalId>("proposal.work.runtime");
    WorkProjection::rebuild(&[
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::initial(),
            authority.clone(),
            UtcMicros(1),
            id("command.work.runtime.1"),
            digest('b'),
            WorkEventKind::Created {
                title: "Execute Work runtime".to_owned(),
                dependencies: BTreeSet::new(),
            },
        )
        .unwrap(),
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::new(2).unwrap(),
            authority.clone(),
            UtcMicros(2),
            id("command.work.runtime.2"),
            digest('c'),
            WorkEventKind::ProposalAccepted {
                proposal_id,
                proposal_digest: digest('d'),
            },
        )
        .unwrap(),
        WorkEvent::new(
            task_id,
            WorkVersion::new(3).unwrap(),
            authority,
            UtcMicros(3),
            id("command.work.runtime.3"),
            digest('e'),
            WorkEventKind::ExecutionAdmitted,
        )
        .unwrap(),
    ])
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
    attempt.validate_projection(&projection()).unwrap();

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
    assert!(wrong_task.validate_projection(&projection()).is_err());

    let snapshot = WorkProjectionSnapshotV1::new(
        id("generation.work.runtime"),
        WorkProjectionSequenceV1::new(7),
        vec![projection()],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    attempt.validate_snapshot(&snapshot).unwrap();
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
fn generated_runtime_schema_names_are_stable() {
    assert_eq!(schema_name::<WorkFenceEpochV1>(), "WorkFenceEpochV1");
    assert_eq!(
        schema_name::<WorkAttemptIdentityV1>(),
        "WorkAttemptIdentityV1"
    );
    assert_eq!(
        schema_name::<WorkAttemptProjectionBindingV1>(),
        "WorkAttemptProjectionBindingV1"
    );
    assert_eq!(schema_name::<WorkLeaseFenceV1>(), "WorkLeaseFenceV1");
    assert_eq!(schema_name::<WorkProviderRouteV1>(), "WorkProviderRouteV1");
    assert_eq!(
        schema_name::<tracedecay_domain::WorkAttemptProgressV1>(),
        "WorkAttemptProgressV1"
    );
    assert_eq!(schema_name::<WorkArtifactRefV1>(), "WorkArtifactRefV1");
    assert_eq!(
        schema_name::<WorkCancellationRequestV1>(),
        "WorkCancellationRequestV1"
    );
    assert_eq!(
        schema_name::<WorkCancellationAcknowledgementV1>(),
        "WorkCancellationAcknowledgementV1"
    );
    assert_eq!(
        schema_name::<WorkCancellationEscalationV1>(),
        "WorkCancellationEscalationV1"
    );
    assert_eq!(
        schema_name::<WorkCancellationStateV1>(),
        "WorkCancellationStateV1"
    );
    assert_eq!(schema_name::<WorkRecoveryStateV1>(), "WorkRecoveryStateV1");
    assert_eq!(schema_name::<WorkAttemptStateV1>(), "WorkAttemptStateV1");
    assert_eq!(
        schema_name::<WorkTerminalEvidenceV1>(),
        "WorkTerminalEvidenceV1"
    );
    assert_eq!(schema_name::<WorkAttemptV1>(), "WorkAttemptV1");
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
