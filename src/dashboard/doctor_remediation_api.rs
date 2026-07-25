//! Admitted owner-operation routing for Doctor remediation references.
//!
//! Doctor remains diagnostic-only. These endpoints delegate preview, apply,
//! and durable status reads to an optional owner-supplied dispatcher. No
//! dispatcher means unsupported; the dashboard never derives or executes a
//! repair from a finding.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorRemediationRegistryV1,
};
use tracedecay_application::{
    EffectReceipt, IdempotencyKey, OperationReceipt, PreviewId, RequestId,
};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, scope_from_state,
};
use crate::application::operation_stream::OperationId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DoctorRemediationDispatchCommandV1 {
    Preview {
        operation: DoctorOwningOperationRefV1,
    },
    Apply {
        operation: DoctorOwningOperationRefV1,
        preview_id: Option<PreviewId>,
        idempotency_key: IdempotencyKey,
    },
    Status {
        operation_id: OperationId,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DoctorRemediationOperationPhaseV1 {
    Previewed,
    Running,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DoctorRemediationOperationV1 {
    pub operation_id: OperationId,
    pub owning_operation: DoctorOwningOperationRefV1,
    pub phase: DoctorRemediationOperationPhaseV1,
    pub preview_id: Option<PreviewId>,
    pub execution: Option<OperationReceipt>,
    pub effect_receipt: Option<EffectReceipt>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DoctorRemediationDispatchErrorV1 {
    Unsupported,
    Denied,
    InvalidReference,
    ConfirmationRequired,
    OwnerUnavailable,
}

pub(crate) type DoctorRemediationDispatchFuture = Pin<
    Box<
        dyn Future<Output = Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>>
            + Send
            + 'static,
    >,
>;

pub(crate) type DoctorRemediationLegalActionsFuture =
    Pin<Box<dyn Future<Output = Vec<DashboardLegalActionKindV1>> + Send + 'static>>;
pub(crate) type LegalActions = Arc<
    dyn Fn(DoctorRemediationRefV1) -> DoctorRemediationLegalActionsFuture + Send + Sync + 'static,
>;
pub(crate) type Dispatch = Arc<
    dyn Fn(DoctorRemediationDispatchCommandV1) -> DoctorRemediationDispatchFuture
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone)]
pub(crate) struct DoctorRemediationDispatcherV1 {
    // Both callbacks are owner supplied. They must re-check current authority;
    // construction-time admission alone never authorizes a later request.
    legal_actions: LegalActions,
    dispatch: Dispatch,
    durable_receipt_root: Option<std::path::PathBuf>,
}

impl DoctorRemediationDispatcherV1 {
    pub(crate) fn new(legal_actions: LegalActions, dispatch: Dispatch) -> Self {
        Self {
            legal_actions,
            dispatch,
            durable_receipt_root: None,
        }
    }

    pub(crate) fn new_durable(
        durable_receipt_root: std::path::PathBuf,
        legal_actions: LegalActions,
        dispatch: Dispatch,
    ) -> Self {
        Self {
            legal_actions,
            dispatch,
            durable_receipt_root: Some(durable_receipt_root),
        }
    }

    pub(crate) async fn legal_actions(
        &self,
        reference: &DoctorRemediationRefV1,
    ) -> Vec<DashboardLegalActionKindV1> {
        let preview_available = DoctorRemediationRegistryV1::default_registry()
            .resolve(reference)
            .is_ok_and(|descriptor| descriptor.preview_available());
        (self.legal_actions)(reference.clone())
            .await
            .into_iter()
            .filter(|kind| {
                matches!(
                    (reference.kind(), *kind, preview_available),
                    (
                        DoctorRemediationKindV1::Preview,
                        DashboardLegalActionKindV1::RequestDryRun,
                        true
                    ) | (
                        DoctorRemediationKindV1::Action,
                        DashboardLegalActionKindV1::RequestApply,
                        _
                    ) | (
                        DoctorRemediationKindV1::Action,
                        DashboardLegalActionKindV1::RequestDryRun,
                        true
                    )
                )
            })
            .collect()
    }

    async fn dispatch(
        &self,
        command: DoctorRemediationDispatchCommandV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        if let (Some(root), DoctorRemediationDispatchCommandV1::Status { operation_id }) =
            (&self.durable_receipt_root, &command)
            && let Some(operation) = read_durable_operation(root, operation_id)?
        {
            let kind = if operation.phase == DoctorRemediationOperationPhaseV1::Previewed {
                DoctorRemediationKindV1::Preview
            } else {
                DoctorRemediationKindV1::Action
            };
            let reference = DoctorRemediationRefV1::new(operation.owning_operation.clone(), kind);
            if self.legal_actions(&reference).await.is_empty() {
                return Err(DoctorRemediationDispatchErrorV1::Denied);
            }
            return Ok(operation);
        }
        let operation = (self.dispatch)(command).await?;
        if let Some(root) = &self.durable_receipt_root {
            validate_outcome(operation.clone())?;
            write_durable_operation(root, &operation)?;
        }
        Ok(operation)
    }
}

fn durable_operation_path(
    root: &std::path::Path,
    operation_id: &OperationId,
) -> Result<std::path::PathBuf, DoctorRemediationDispatchErrorV1> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.doctor-remediation-operation.v1",
        operation_id,
    ))
    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    Ok(root.join(format!("{}.json", digest.as_str())))
}

fn read_durable_operation(
    root: &std::path::Path,
    operation_id: &OperationId,
) -> Result<Option<DoctorRemediationOperationV1>, DoctorRemediationDispatchErrorV1> {
    crate::storage::PrivateStoreIo::create_dir_all(root)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let path = durable_operation_path(root, operation_id)?;
    let lock_path = crate::storage::append_lock_path(&path);
    let _lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(DoctorRemediationDispatchErrorV1::InvalidReference)
        }
        Ok(_) => {
            let bytes = std::fs::read(&path)
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let operation: DoctorRemediationOperationV1 = serde_json::from_slice(&bytes)
                .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
            (operation.operation_id == *operation_id)
                .then_some(Some(operation))
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable),
    }
}

fn write_durable_operation(
    root: &std::path::Path,
    operation: &DoctorRemediationOperationV1,
) -> Result<(), DoctorRemediationDispatchErrorV1> {
    crate::storage::PrivateStoreIo::create_dir_all(root)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let path = durable_operation_path(root, &operation.operation_id)?;
    let lock_path = crate::storage::append_lock_path(&path);
    let _lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let bytes = serde_json::to_vec(operation)
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    crate::storage::PrivateStoreIo::write_file_atomically(&path, &temp_path, &bytes)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationPreviewRequestV1 {
    operation: DoctorOwningOperationRefV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationApplyRequestV1 {
    operation: DoctorOwningOperationRefV1,
    preview_id: Option<PreviewId>,
    idempotency_key: IdempotencyKey,
    confirmed: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DoctorRemediationPayloadV1 {
    Operation {
        operation: DoctorRemediationOperationV1,
    },
    Unavailable {
        reason: DoctorRemediationDispatchErrorV1,
    },
}

pub(crate) async fn preview(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationPreviewRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    if !reference_is_registered(&request.operation, DoctorRemediationKindV1::Preview) {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    }
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let reference =
        DoctorRemediationRefV1::new(request.operation.clone(), DoctorRemediationKindV1::Preview);
    if !dispatcher
        .legal_actions(&reference)
        .await
        .contains(&DashboardLegalActionKindV1::RequestDryRun)
    {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Denied));
    }
    let expected_operation = request.operation.clone();
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Preview {
            operation: request.operation,
        })
        .await
        .and_then(|outcome| {
            (outcome.owning_operation == expected_operation)
                .then_some(outcome)
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

pub(crate) async fn apply(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationApplyRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    if !request.confirmed {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::ConfirmationRequired),
        );
    }
    if !reference_is_registered(&request.operation, DoctorRemediationKindV1::Action) {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    }
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let reference =
        DoctorRemediationRefV1::new(request.operation.clone(), DoctorRemediationKindV1::Action);
    if !dispatcher
        .legal_actions(&reference)
        .await
        .contains(&DashboardLegalActionKindV1::RequestApply)
    {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Denied));
    }
    let expected_operation = request.operation.clone();
    let expected_preview_id = request.preview_id.clone();
    let expected_idempotency_key = request.idempotency_key.clone();
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Apply {
            operation: request.operation,
            preview_id: request.preview_id,
            idempotency_key: request.idempotency_key,
        })
        .await
        .and_then(|outcome| {
            (outcome.owning_operation == expected_operation
                && outcome.preview_id == expected_preview_id
                && outcome
                    .effect_receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.idempotency_key == expected_idempotency_key))
            .then_some(outcome)
            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

pub(crate) async fn status(
    State(state): State<DashboardState>,
    Path(operation_id): Path<String>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let Ok(request_id) = RequestId::new(operation_id) else {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    };
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let operation_id = OperationId::from_request(request_id);
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Status {
            operation_id: operation_id.clone(),
        })
        .await
        .and_then(|outcome| {
            (outcome.operation_id == operation_id)
                .then_some(outcome)
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

fn reference_is_registered(
    operation: &DoctorOwningOperationRefV1,
    kind: DoctorRemediationKindV1,
) -> bool {
    DoctorRemediationRegistryV1::default_registry()
        .resolve(&DoctorRemediationRefV1::new(operation.clone(), kind))
        .is_ok()
}

fn validate_outcome(
    outcome: DoctorRemediationOperationV1,
) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
    let invalid_effect_binding = outcome.effect_receipt.as_ref().is_some_and(|receipt| {
        receipt.operation.as_str() != outcome.owning_operation.as_str()
            || &receipt.request_id != outcome.operation_id.request_id()
            || outcome
                .execution
                .as_ref()
                .is_none_or(|execution| execution.termination != receipt.outcome.into())
    });
    if !reference_is_registered(&outcome.owning_operation, DoctorRemediationKindV1::Action)
        || outcome
            .execution
            .as_ref()
            .is_some_and(|receipt| receipt.validate().is_err())
        || outcome
            .effect_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.validate().is_err())
        || invalid_effect_binding
    {
        return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
    }
    let expected_termination = match outcome.phase {
        DoctorRemediationOperationPhaseV1::Previewed
        | DoctorRemediationOperationPhaseV1::Completed => {
            Some(tracedecay_application::OperationTermination::Completed)
        }
        DoctorRemediationOperationPhaseV1::Cancelled => {
            Some(tracedecay_application::OperationTermination::Cancelled)
        }
        DoctorRemediationOperationPhaseV1::TimedOut => {
            Some(tracedecay_application::OperationTermination::TimedOut)
        }
        DoctorRemediationOperationPhaseV1::Failed => {
            Some(tracedecay_application::OperationTermination::Failed)
        }
        DoctorRemediationOperationPhaseV1::Partial => {
            Some(tracedecay_application::OperationTermination::Partial)
        }
        DoctorRemediationOperationPhaseV1::EffectUnknown => {
            Some(tracedecay_application::OperationTermination::EffectUnknown)
        }
        DoctorRemediationOperationPhaseV1::Running => None,
    };
    if expected_termination.is_some_and(|expected| {
        outcome
            .execution
            .as_ref()
            .is_none_or(|execution| execution.termination != expected)
    }) || (outcome.phase == DoctorRemediationOperationPhaseV1::Previewed
        && outcome.preview_id.is_none())
        || (outcome.phase == DoctorRemediationOperationPhaseV1::Previewed
            && outcome.effect_receipt.is_some())
        || matches!(
            outcome.phase,
            DoctorRemediationOperationPhaseV1::Completed
                | DoctorRemediationOperationPhaseV1::EffectUnknown
        ) && outcome.effect_receipt.is_none()
    {
        return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
    }
    Ok(outcome)
}

fn response(
    state: &DashboardState,
    result: Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let scope = scope_from_state(state);
    match result {
        Ok(operation) => {
            let domain_state = match operation.phase {
                DoctorRemediationOperationPhaseV1::Previewed
                | DoctorRemediationOperationPhaseV1::Completed => DashboardDomainStateV1::Ready,
                DoctorRemediationOperationPhaseV1::Running
                | DoctorRemediationOperationPhaseV1::Partial => DashboardDomainStateV1::Partial,
                DoctorRemediationOperationPhaseV1::Cancelled => DashboardDomainStateV1::Cancelled,
                DoctorRemediationOperationPhaseV1::TimedOut => DashboardDomainStateV1::TimedOut,
                DoctorRemediationOperationPhaseV1::Failed
                | DoctorRemediationOperationPhaseV1::EffectUnknown => DashboardDomainStateV1::Error,
            };
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                DashboardCoverageV1::complete(1, "doctor_remediation_operation"),
                DashboardFreshnessV1::fresh_now(),
                DoctorRemediationPayloadV1::Operation { operation },
            ))
        }
        Err(error) => {
            let domain_state = match error {
                DoctorRemediationDispatchErrorV1::Unsupported => {
                    DashboardDomainStateV1::Unsupported
                }
                DoctorRemediationDispatchErrorV1::Denied
                | DoctorRemediationDispatchErrorV1::ConfirmationRequired => {
                    DashboardDomainStateV1::Denied
                }
                DoctorRemediationDispatchErrorV1::InvalidReference => DashboardDomainStateV1::Error,
                DoctorRemediationDispatchErrorV1::OwnerUnavailable => {
                    DashboardDomainStateV1::Offline
                }
            };
            let coverage = if error == DoctorRemediationDispatchErrorV1::Unsupported {
                DashboardCoverageV1::unsupported()
            } else {
                DashboardCoverageV1::unknown()
            };
            let freshness = if error == DoctorRemediationDispatchErrorV1::Unsupported {
                DashboardFreshnessV1::unsupported()
            } else {
                DashboardFreshnessV1::unknown()
            };
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                coverage,
                freshness,
                DoctorRemediationPayloadV1::Unavailable { reason: error },
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{Deadline, OperationBudgetUsage, OperationTermination};
    use tracedecay_domain::UtcMicros;

    use crate::tracedecay::TraceDecay;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, state)
    }

    fn configuration_operation() -> DoctorOwningOperationRefV1 {
        DoctorOwningOperationRefV1::new(
            tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY,
        )
        .unwrap()
    }

    fn runtime_recovery_operation() -> DoctorOwningOperationRefV1 {
        DoctorOwningOperationRefV1::new(
            tracedecay_application::doctor::operations::RUNTIME_RECOVER_DAEMON,
        )
        .unwrap()
    }

    fn failed_operation() -> DoctorRemediationOperationV1 {
        DoctorRemediationOperationV1 {
            operation_id: OperationId::from_request(
                RequestId::new("request.doctor-remediation-failed").unwrap(),
            ),
            owning_operation: configuration_operation(),
            phase: DoctorRemediationOperationPhaseV1::Failed,
            preview_id: Some(PreviewId::new("preview.doctor-remediation-failed").unwrap()),
            execution: Some(OperationReceipt {
                started_at: UtcMicros(1),
                ended_at: UtcMicros(2),
                effective_deadline: Deadline::new(UtcMicros(10)).unwrap(),
                cancellation: None,
                budget: OperationBudgetUsage::default(),
                termination: OperationTermination::Failed,
            }),
            effect_receipt: None,
        }
    }

    #[tokio::test]
    async fn preview_is_typed_unsupported_without_an_admitted_dispatcher() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;

        let Json(envelope) = preview(
            State(state),
            Json(DoctorRemediationPreviewRequestV1 {
                operation: configuration_operation(),
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::Unsupported
            }
        );
    }

    #[tokio::test]
    async fn denied_owner_apply_stays_denied() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new(|_| Box::pin(async { Err(DoctorRemediationDispatchErrorV1::Denied) })),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                preview_id: Some(PreviewId::new("preview.denied").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.denied").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Denied);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::Denied
            }
        );
    }

    #[tokio::test]
    async fn apply_rejects_an_owner_outcome_for_a_different_preview() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new(|_| Box::pin(async { Ok(failed_operation()) })),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                preview_id: Some(PreviewId::new("preview.different").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.different").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Error);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::InvalidReference
            }
        );
    }

    #[tokio::test]
    async fn legal_actions_reject_dry_run_when_registry_has_no_preview() {
        let dispatcher = DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new(|_| Box::pin(async { Err(DoctorRemediationDispatchErrorV1::Denied) })),
        );
        let reference = DoctorRemediationRefV1::new(
            DoctorOwningOperationRefV1::new(
                tracedecay_application::doctor::operations::RUNTIME_RECOVER_DAEMON,
            )
            .unwrap(),
            DoctorRemediationKindV1::Action,
        );

        assert!(dispatcher.legal_actions(&reference).await.is_empty());
    }

    #[tokio::test]
    async fn apply_delegates_an_operation_that_has_no_preview() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        let mut failed = failed_operation();
        failed.owning_operation = runtime_recovery_operation();
        failed.preview_id = None;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: runtime_recovery_operation(),
                preview_id: None,
                idempotency_key: IdempotencyKey::new("idempotency.runtime-recovery").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Operation { operation: failed }
        );
    }

    #[tokio::test]
    async fn apply_requires_explicit_confirmation_before_owner_dispatch() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                preview_id: Some(PreviewId::new("preview.unconfirmed").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.unconfirmed").unwrap(),
                confirmed: false,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Denied);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::ConfirmationRequired
            }
        );
    }

    #[tokio::test]
    async fn failed_receipt_is_resumed_through_same_owner_after_dashboard_rebuild() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let failed = failed_operation();
        let dispatcher = DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        );
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let mut first_state = crate::dashboard::build_state(&cg)
            .await
            .expect("first dashboard state");
        first_state.doctor_remediation_dispatcher = Some(dispatcher.clone());

        let Json(first) = preview(
            State(first_state),
            Json(DoctorRemediationPreviewRequestV1 {
                operation: configuration_operation(),
            }),
        )
        .await;
        assert_eq!(first.domain_state, DashboardDomainStateV1::Error);

        let operation_id = failed.operation_id.to_string();
        let mut rebuilt_state = crate::dashboard::build_state(&cg)
            .await
            .expect("rebuilt dashboard state");
        rebuilt_state.doctor_remediation_dispatcher = Some(dispatcher);
        let Json(resumed) = status(State(rebuilt_state), Path(operation_id)).await;

        assert_eq!(
            resumed.payload,
            DoctorRemediationPayloadV1::Operation { operation: failed }
        );
        assert_eq!(resumed.domain_state, DashboardDomainStateV1::Error);
    }

    #[tokio::test]
    async fn durable_dispatcher_resumes_terminal_receipt_after_rebuild() {
        let root = tempfile::tempdir().expect("receipt root");
        let failed = failed_operation();
        let first = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        );
        first
            .dispatch(DoctorRemediationDispatchCommandV1::Preview {
                operation: configuration_operation(),
            })
            .await
            .expect("persist terminal operation");

        let rebuilt = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new(|_| {
                Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })
            }),
        );
        let resumed = rebuilt
            .dispatch(DoctorRemediationDispatchCommandV1::Status {
                operation_id: failed.operation_id.clone(),
            })
            .await
            .expect("resume durable terminal operation");

        assert_eq!(resumed, failed);
    }
}
