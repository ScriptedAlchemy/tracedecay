use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay::application::doctor_remediation::{
    Dispatch, DoctorRemediationAuthorityV1, DoctorRemediationDispatchCommandV1,
    DoctorRemediationDispatchErrorV1, DoctorRemediationLegalActionV1,
    DoctorRemediationObservationFuture, DoctorRemediationOperationPhaseV1,
    DoctorRemediationOperationV1, DoctorRemediationTargetV1, DoctorRemediationVerificationV1,
    LegalActions, Observation, operation_id_for_command,
};
use tracedecay::application::operation_stream::OperationId;
use tracedecay::application_surface::{
    ConfigurationProtectedApplySurfaceRequest, ConfigurationProtectedPreviewSurfaceRequest,
};
use tracedecay_application::doctor::{
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1, operations,
};
use tracedecay_application::{
    CancellationObservation, CancellationStage, Deadline, IdempotencyKey, OperationBudgetUsage,
    OperationReceipt, OperationTermination, PreviewId, RequestId,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::configuration::{
    ChangePlanId, ConfigurationIdempotencyKey, ConfigurationRevisionId, ProtectedChange,
    safe_work_topology_policy_v1,
};

fn operation() -> DoctorOwningOperationRefV1 {
    DoctorOwningOperationRefV1::new(operations::CONFIGURATION_PROTECTED_APPLY).unwrap()
}

fn preview_target() -> DoctorRemediationTargetV1 {
    DoctorRemediationTargetV1::ConfigurationProtectedPreview(
        ConfigurationProtectedPreviewSurfaceRequest {
            change: ProtectedChange::ReplaceWorkTopologyPolicy(safe_work_topology_policy_v1()),
            expected_revision: ConfigurationRevisionId::new(
                "configuration-revision.remediation",
            )
            .unwrap(),
        },
    )
}

fn apply_target() -> DoctorRemediationTargetV1 {
    DoctorRemediationTargetV1::ConfigurationProtectedApply(
        ConfigurationProtectedApplySurfaceRequest {
            plan_id: ChangePlanId::new("change-plan.remediation").unwrap(),
            expected_base_revision_id: ConfigurationRevisionId::new(
                "configuration-revision.remediation",
            )
            .unwrap(),
            operation_digest: ManifestDigest::new(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            idempotency_key: ConfigurationIdempotencyKey::new(
                "configuration-idempotency.remediation",
            )
            .unwrap(),
        },
    )
}

fn legal_actions() -> LegalActions {
    Arc::new(|reference: DoctorRemediationRefV1| {
        Box::pin(async move {
            match reference.kind() {
                DoctorRemediationKindV1::Preview => {
                    vec![DoctorRemediationLegalActionV1::RequestPreview]
                }
                DoctorRemediationKindV1::Action => {
                    vec![DoctorRemediationLegalActionV1::RequestApply]
                }
            }
        })
    })
}

fn completed(command: &DoctorRemediationDispatchCommandV1) -> DoctorRemediationOperationV1 {
    let operation_id = operation_id_for_command(command).unwrap();
    DoctorRemediationOperationV1 {
        operation_id,
        owning_operation: operation(),
        phase: DoctorRemediationOperationPhaseV1::Partial,
        preview_id: match command {
            DoctorRemediationDispatchCommandV1::Apply { preview_id, .. }
            | DoctorRemediationDispatchCommandV1::Resume { preview_id, .. } => preview_id.clone(),
            _ => None,
        },
        execution: Some(OperationReceipt {
            started_at: tracedecay_domain::UtcMicros(1),
            ended_at: tracedecay_domain::UtcMicros(2),
            effective_deadline: Deadline::new(tracedecay_domain::UtcMicros(10)).unwrap(),
            cancellation: None,
            budget: OperationBudgetUsage::default(),
            termination: OperationTermination::Partial,
        }),
        effect_receipt: None,
        owner_effect_receipt: None,
        owner_result_digest: Some(
            ManifestDigest::new(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
        ),
        verification: DoctorRemediationVerificationV1::Pending,
    }
}

fn owner_dispatch() -> Dispatch {
    Arc::new(|command| {
        Box::pin(async move {
            if matches!(command, DoctorRemediationDispatchCommandV1::Preview { .. }) {
                let mut preview = completed(&command);
                preview.phase = DoctorRemediationOperationPhaseV1::Previewed;
                preview.preview_id =
                    Some(PreviewId::new(format!("preview.{}", preview.operation_id)).unwrap());
                preview.execution.as_mut().unwrap().termination = OperationTermination::Completed;
                preview.verification = DoctorRemediationVerificationV1::NotRequired;
                return Ok(preview);
            }
            Ok(completed(&command))
        })
    })
}

fn verified_observation(calls: Arc<AtomicUsize>) -> Observation {
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        let digest = ManifestDigest::new(
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        )
        .unwrap();
        Box::pin(async move {
            Ok(DoctorRemediationVerificationV1::Verified {
                observation_digest: digest,
            })
        }) as DoctorRemediationObservationFuture
    })
}

#[tokio::test]
async fn application_authority_independently_verifies_and_persists_owner_result() {
    let root = tempfile::tempdir().unwrap();
    let observations = Arc::new(AtomicUsize::new(0));
    let authority = DoctorRemediationAuthorityV1::new_durable(
        root.path().to_path_buf(),
        legal_actions(),
        owner_dispatch(),
        verified_observation(Arc::clone(&observations)),
    );
    let preview = authority
        .preview(DoctorRemediationDispatchCommandV1::Preview {
            operation: operation(),
            target: preview_target(),
        })
        .await
        .unwrap();
    assert_eq!(preview.phase, DoctorRemediationOperationPhaseV1::Previewed);
    assert_eq!(
        preview.verification,
        DoctorRemediationVerificationV1::NotRequired
    );
    assert_eq!(observations.load(Ordering::Relaxed), 0);
    let command = DoctorRemediationDispatchCommandV1::Apply {
        operation: operation(),
        target: apply_target(),
        preview_id: preview.preview_id,
        idempotency_key: IdempotencyKey::new("idempotency.remediation").unwrap(),
    };

    let applied = authority.apply(command.clone(), true).await.unwrap();
    assert!(matches!(
        applied.verification,
        DoctorRemediationVerificationV1::Verified { .. }
    ));
    assert_eq!(observations.load(Ordering::Relaxed), 1);

    let rebuilt = DoctorRemediationAuthorityV1::new_durable(
        root.path().to_path_buf(),
        legal_actions(),
        Arc::new(|_| Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })),
        verified_observation(Arc::clone(&observations)),
    );
    let resumed = rebuilt.status(applied.operation_id.clone()).await.unwrap();
    assert_eq!(resumed, applied);
    assert_eq!(observations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn unavailable_reobservation_is_retried_after_restart_without_redispatch() {
    let root = tempfile::tempdir().unwrap();
    let unavailable: Observation =
        Arc::new(|_| Box::pin(async { Ok(DoctorRemediationVerificationV1::Unavailable) }));
    let authority = DoctorRemediationAuthorityV1::new_durable(
        root.path().to_path_buf(),
        legal_actions(),
        owner_dispatch(),
        unavailable,
    );
    let command = DoctorRemediationDispatchCommandV1::Apply {
        operation: operation(),
        target: apply_target(),
        preview_id: Some(PreviewId::new("preview.restart").unwrap()),
        idempotency_key: IdempotencyKey::new("idempotency.restart").unwrap(),
    };
    let first = authority.apply(command, true).await.unwrap();
    assert_eq!(
        first.verification,
        DoctorRemediationVerificationV1::Unavailable
    );

    let observations = Arc::new(AtomicUsize::new(0));
    let rebuilt = DoctorRemediationAuthorityV1::new_durable(
        root.path().to_path_buf(),
        legal_actions(),
        Arc::new(|_| panic!("status must not redispatch a terminal owner effect")),
        verified_observation(Arc::clone(&observations)),
    );
    let verified = rebuilt.status(first.operation_id).await.unwrap();
    assert!(matches!(
        verified.verification,
        DoctorRemediationVerificationV1::Verified { .. }
    ));
    assert_eq!(observations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn denial_confirmation_and_cancelled_truth_remain_distinct() {
    let authority = DoctorRemediationAuthorityV1::new(
        legal_actions(),
        Arc::new(|command| {
            Box::pin(async move {
                let mut operation = completed(&command);
                operation.phase = DoctorRemediationOperationPhaseV1::Cancelled;
                let execution = operation.execution.as_mut().unwrap();
                execution.termination = OperationTermination::Cancelled;
                execution.cancellation = Some(CancellationObservation {
                    stage: CancellationStage::BeforeEffect,
                    observed_at: tracedecay_domain::UtcMicros(2),
                });
                operation.verification = DoctorRemediationVerificationV1::NotRequired;
                Ok(operation)
            })
        }),
        Arc::new(|_| panic!("cancelled-before-effect must not be re-observed")),
    );
    let command = DoctorRemediationDispatchCommandV1::Apply {
        operation: operation(),
        target: apply_target(),
        preview_id: Some(PreviewId::new("preview.cancelled").unwrap()),
        idempotency_key: IdempotencyKey::new("idempotency.cancelled").unwrap(),
    };
    assert_eq!(
        authority.apply(command.clone(), false).await,
        Err(DoctorRemediationDispatchErrorV1::ConfirmationRequired)
    );
    let cancelled = authority.apply(command, true).await.unwrap();
    assert_eq!(
        cancelled.phase,
        DoctorRemediationOperationPhaseV1::Cancelled
    );
    assert_eq!(
        cancelled.verification,
        DoctorRemediationVerificationV1::NotRequired
    );

    let denied = DoctorRemediationAuthorityV1::new(
        Arc::new(|_| Box::pin(async { Vec::new() })),
        owner_dispatch(),
        Arc::new(|_| panic!("denied action must not be re-observed")),
    );
    assert_eq!(
        denied
            .preview(DoctorRemediationDispatchCommandV1::Preview {
                operation: operation(),
                target: preview_target(),
            })
            .await,
        Err(DoctorRemediationDispatchErrorV1::Denied)
    );
}

#[test]
fn operation_id_is_application_owned_request_identity() {
    let id = OperationId::from_request(RequestId::new("request.remediation-authority").unwrap());
    assert_eq!(id.to_string(), "request.remediation-authority");
}
