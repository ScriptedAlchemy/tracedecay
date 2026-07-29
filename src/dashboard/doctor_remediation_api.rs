//! Thin HTTP binding for the canonical Doctor remediation application authority.

use axum::Json;
use axum::extract::{Path, State};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::DoctorOwningOperationRefV1;
use tracedecay_application::{IdempotencyKey, PreviewId, RequestId};

pub(crate) use crate::application::doctor_remediation::{
    Dispatch, DoctorRemediationAuthorityV1, DoctorRemediationDispatchCommandV1,
    DoctorRemediationDispatchErrorV1, DoctorRemediationLegalActionV1,
    DoctorRemediationOperationPhaseV1, DoctorRemediationOperationV1, DoctorRemediationTargetV1,
    DoctorRemediationVerificationV1, LegalActions, Observation, operation_id_for_command,
};
use crate::application::operation_stream::OperationId;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    scope_from_state,
};

pub(crate) type DoctorRemediationDispatcherV1 = DoctorRemediationAuthorityV1;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationPreviewRequestV1 {
    operation: DoctorOwningOperationRefV1,
    target: DoctorRemediationTargetV1,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationApplyRequestV1 {
    operation: DoctorOwningOperationRefV1,
    target: DoctorRemediationTargetV1,
    preview_id: Option<PreviewId>,
    idempotency_key: IdempotencyKey,
    confirmed: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DoctorRemediationPayloadV1 {
    Operation {
        operation: Box<DoctorRemediationOperationV1>,
    },
    Unavailable {
        reason: DoctorRemediationDispatchErrorV1,
    },
}

pub(crate) async fn preview(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationPreviewRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let Some(authority) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    response(
        &state,
        authority
            .preview(DoctorRemediationDispatchCommandV1::Preview {
                operation: request.operation,
                target: request.target,
            })
            .await,
    )
}

pub(crate) async fn apply(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationApplyRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let Some(authority) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    response(
        &state,
        authority
            .apply(
                DoctorRemediationDispatchCommandV1::Apply {
                    operation: request.operation,
                    target: request.target,
                    preview_id: request.preview_id,
                    idempotency_key: request.idempotency_key,
                },
                request.confirmed,
            )
            .await,
    )
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
    let operation_id = OperationId::from_request(request_id);
    let Some(authority) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    response(&state, authority.status(operation_id).await)
}

fn response(
    state: &DashboardState,
    result: Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let scope = scope_from_state(state);
    match result {
        Ok(operation) => {
            let domain_state = operation_domain_state(&operation);
            let complete = matches!(
                operation.phase,
                DoctorRemediationOperationPhaseV1::Previewed
            ) || matches!(
                operation.verification,
                DoctorRemediationVerificationV1::Verified { .. }
                    | DoctorRemediationVerificationV1::NotRequired
            );
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                if complete {
                    DashboardCoverageV1::complete(1, "doctor_remediation_operation")
                } else {
                    DashboardCoverageV1::unknown()
                },
                if complete {
                    DashboardFreshnessV1::fresh_now()
                } else {
                    DashboardFreshnessV1::unknown()
                },
                DoctorRemediationPayloadV1::Operation {
                    operation: Box::new(operation),
                },
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
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                if error == DoctorRemediationDispatchErrorV1::Unsupported {
                    DashboardCoverageV1::unsupported()
                } else {
                    DashboardCoverageV1::unknown()
                },
                if error == DoctorRemediationDispatchErrorV1::Unsupported {
                    DashboardFreshnessV1::unsupported()
                } else {
                    DashboardFreshnessV1::unknown()
                },
                DoctorRemediationPayloadV1::Unavailable { reason: error },
            ))
        }
    }
}

fn operation_domain_state(operation: &DoctorRemediationOperationV1) -> DashboardDomainStateV1 {
    match operation.phase {
        DoctorRemediationOperationPhaseV1::Previewed => DashboardDomainStateV1::Ready,
        DoctorRemediationOperationPhaseV1::Running => DashboardDomainStateV1::Partial,
        DoctorRemediationOperationPhaseV1::Cancelled => DashboardDomainStateV1::Cancelled,
        DoctorRemediationOperationPhaseV1::TimedOut => DashboardDomainStateV1::TimedOut,
        DoctorRemediationOperationPhaseV1::Completed
        | DoctorRemediationOperationPhaseV1::Failed
        | DoctorRemediationOperationPhaseV1::Partial
        | DoctorRemediationOperationPhaseV1::EffectUnknown => match operation.verification {
            DoctorRemediationVerificationV1::Verified { .. } => DashboardDomainStateV1::Ready,
            DoctorRemediationVerificationV1::Partial { .. }
            | DoctorRemediationVerificationV1::Pending => DashboardDomainStateV1::Partial,
            DoctorRemediationVerificationV1::Denied => DashboardDomainStateV1::Denied,
            DoctorRemediationVerificationV1::Unavailable => DashboardDomainStateV1::Offline,
            DoctorRemediationVerificationV1::Failed { .. }
            | DoctorRemediationVerificationV1::NotRequired => DashboardDomainStateV1::Error,
        },
    }
}
