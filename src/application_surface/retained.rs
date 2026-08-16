//! HTTP owner for the canonical retained application operations.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use tracedecay_application::retained_surfaces::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreCurateRequestV1, FactStoreGetRequestV1, FactStoreListRequestV1,
    FactStoreProbeRequestV1, FactStoreReasonRequestV1, FactStoreRelatedRequestV1,
    FactStoreRemoveRequestV1, FactStoreSearchRequestV1, FactStoreUpdateRequestV1,
    LcmDescribeRequestV1, LcmDoctorRequestV1, LcmExpandQueryRequestV1, LcmExpandRequestV1,
    LcmGrepRequestV1, LcmLoadSessionRequestV1, LcmStatusRequestV1, MemoryStatusRequestV1,
    MessageSearchRequestV1, RetainedSurfaceOperation, RetainedSurfaceRequestV1,
    RetainedSurfaceResultV1, SessionRefreshActionRequestV1, SessionRefreshActionV1,
    SessionRefreshRequestV1, SessionsForRequestV1, WorkflowsRequestV1,
};
use tracedecay_tool_catalog::RouteExposureV1;

use super::{ApplicationSurfaceAdapterError, RegisteredHttpOperation, invoke_registered_http};
use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{DaemonInvocationOutcome, DaemonInvocationRequest};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::retained_application_router(
        RetainedExecutorOwner { executor },
    ))
}

fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_application::retained_surface_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::Contract)?;
    for operation in RetainedSurfaceOperation::CALLABLE {
        let operation_id = tracedecay_tool_catalog::OperationId::new(
            tracedecay_api::retained_operation_id(operation),
        )
        .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        let Some(binding) = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
        else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        if route_path != &tracedecay_api::retained_application_route_path(operation) {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(())
}

pub(super) fn active_request_conflict_response(
    request_id: tracedecay_application::RequestId,
) -> Response {
    match active_request_conflict(request_id) {
        Ok(result) => result.into_http_response(),
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn active_request_conflict(
    request_id: tracedecay_application::RequestId,
) -> Result<
    tracedecay_api::CanonicalInvocationResult<serde_json::Value>,
    ApplicationSurfaceAdapterError,
> {
    let operation = RetainedSurfaceOperation::FactStoreCurate;
    let registry = operation.registry()?;
    let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
        .map_err(ApplicationSurfaceAdapterError::Identifier)?;
    let binding = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    let RouteExposureV1::Public { binding_id, .. } = binding.exposure() else {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    };
    let contract = tracedecay_application::ResultContractRef::new(
        binding.result_schema().schema_ref().schema_id().clone(),
        binding.result_schema().schema_ref().revision(),
    )?;
    let problem = tracedecay_application::ApplicationProblemEnvelope::new(
        contract,
        request_id,
        tracedecay_application::ApplicationProblem::Conflict {
            diagnostic: tracedecay_application::SafeDiagnostic {
                code: "retained.request_already_active".to_owned(),
                message: "The retained application request is already active".to_owned(),
            },
            retry: tracedecay_application::RetryDirective::SameRequest,
            legal_actions: vec![tracedecay_application::LegalAction::Retry],
        },
    )?
    .with_owning_layer(tracedecay_application::ProblemOwningLayer::Adapter);
    Ok(tracedecay_api::CanonicalInvocationResult::new(
        binding_id.clone(),
        Err(problem),
    ))
}

#[cfg(test)]
mod conflict_tests {
    use super::active_request_conflict;

    #[test]
    fn active_replay_collision_preserves_same_request_retry_authority() {
        let request_id =
            tracedecay_application::RequestId::new("request.sdk.curate").expect("request id");
        let envelope = serde_json::to_value(
            active_request_conflict(request_id)
                .expect("conflict")
                .into_http_json(),
        )
        .expect("wire envelope");
        assert_eq!(envelope["value"]["problem"]["retry"], "same_request");
        assert_eq!(envelope["value"]["problem"]["retry_scope"], "same_request");
        assert_eq!(
            envelope["value"]["problem"]["legal_actions"],
            serde_json::json!(["retry"])
        );
    }
}

impl RegisteredHttpOperation for RetainedSurfaceOperation {
    fn operation_id(self) -> String {
        tracedecay_api::retained_operation_id(self)
    }

    fn is_read_only(self) -> bool {
        !tracedecay_application::retained_surfaces::retained_surface_operation_is_effect(self)
    }

    fn problem_family(self) -> &'static str {
        "retained"
    }

    fn display_family(self) -> &'static str {
        "retained"
    }

    fn application_problem_is_bound(
        self,
        request_id: &tracedecay_application::RequestId,
        scope: Option<&tracedecay_application::ResolvedScope>,
        problem: &tracedecay_application::ApplicationProblem,
    ) -> bool {
        tracedecay_application::retained_surface_problem_matches_terminal(
            self, request_id, scope, problem,
        )
    }

    fn registry(
        self,
    ) -> Result<tracedecay_tool_catalog::ExecutableBindingRegistryV1, ApplicationSurfaceAdapterError>
    {
        tracedecay_application::retained_surface_executable_binding_registry()
            .map_err(ApplicationSurfaceAdapterError::Contract)
    }
}

#[derive(Clone)]
struct RetainedExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::RetainedApplicationOwner for RetainedExecutorOwner {
    fn invoke_retained(
        &self,
        request: tracedecay_api::RetainedHttpRequest,
    ) -> tracedecay_api::RetainedInvocationFuture {
        Box::pin(invoke_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_operation(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: tracedecay_api::RetainedHttpRequest,
) -> Response {
    let tracedecay_api::RetainedHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    let Some(request) = decode_request(operation, body) else {
        return tracedecay_api::retained_invalid_request_response(request_id);
    };
    let invocation = DaemonInvocationRequest::retained_application(
        request_id.as_str(),
        request,
        crate::daemon_client::invocation_now_micros(),
        controls.deadline.clone(),
        controls.cancellation.context(),
    );
    let selected_request_id = request_id.clone();
    invoke_registered_http::<RetainedSurfaceResultV1, _>(
        executor.as_ref(),
        operation,
        request_id,
        controls,
        invocation,
        move |outcome| match outcome {
            DaemonInvocationOutcome::RetainedApplication { scope, outcome } => {
                tracedecay_application::retained_surface_outcome_matches_terminal(
                    operation,
                    &selected_request_id,
                    &scope,
                    &outcome,
                )
                .then_some((scope, outcome))
            }
            _ => None,
        },
    )
    .await
}

pub(crate) fn decode_request(
    operation: RetainedSurfaceOperation,
    body: serde_json::Value,
) -> Option<RetainedSurfaceRequestV1> {
    macro_rules! decode {
        ($request:ty, $variant:ident) => {
            serde_json::from_value::<$request>(body)
                .ok()
                .map(RetainedSurfaceRequestV1::$variant)
        };
    }
    match operation {
        RetainedSurfaceOperation::FactStoreCurate => {
            decode!(FactStoreCurateRequestV1, FactStoreCurate)
        }
        RetainedSurfaceOperation::FactStoreAdd => {
            decode!(FactStoreAddRequestV1, FactStoreAdd)
        }
        RetainedSurfaceOperation::FactStoreSearch => {
            decode!(FactStoreSearchRequestV1, FactStoreSearch)
        }
        RetainedSurfaceOperation::FactStoreProbe => {
            decode!(FactStoreProbeRequestV1, FactStoreProbe)
        }
        RetainedSurfaceOperation::FactStoreRelated => {
            decode!(FactStoreRelatedRequestV1, FactStoreRelated)
        }
        RetainedSurfaceOperation::FactStoreReason => {
            decode!(FactStoreReasonRequestV1, FactStoreReason)
        }
        RetainedSurfaceOperation::FactStoreContradict => {
            decode!(FactStoreContradictRequestV1, FactStoreContradict)
        }
        RetainedSurfaceOperation::FactStoreGet => {
            decode!(FactStoreGetRequestV1, FactStoreGet)
        }
        RetainedSurfaceOperation::FactStoreUpdate => {
            decode!(FactStoreUpdateRequestV1, FactStoreUpdate)
        }
        RetainedSurfaceOperation::FactStoreRemove => {
            decode!(FactStoreRemoveRequestV1, FactStoreRemove)
        }
        RetainedSurfaceOperation::FactStoreList => {
            decode!(FactStoreListRequestV1, FactStoreList)
        }
        RetainedSurfaceOperation::FactFeedback => decode!(FactFeedbackRequestV1, FactFeedback),
        RetainedSurfaceOperation::MemoryStatus => decode!(MemoryStatusRequestV1, MemoryStatus),
        RetainedSurfaceOperation::SessionRefreshStatus => {
            decode_session_refresh(body, SessionRefreshActionV1::Status)
        }
        RetainedSurfaceOperation::SessionRefreshCancel => {
            decode_session_refresh(body, SessionRefreshActionV1::Cancel)
        }
        RetainedSurfaceOperation::SessionRefreshBegin => {
            decode_session_refresh(body, SessionRefreshActionV1::Begin)
        }
        RetainedSurfaceOperation::MessageSearch => decode!(MessageSearchRequestV1, MessageSearch),
        RetainedSurfaceOperation::SessionsFor => decode!(SessionsForRequestV1, SessionsFor),
        RetainedSurfaceOperation::Workflows => decode!(WorkflowsRequestV1, Workflows),
        RetainedSurfaceOperation::LcmStatus => decode!(LcmStatusRequestV1, LcmStatus),
        RetainedSurfaceOperation::LcmDoctor => decode!(LcmDoctorRequestV1, LcmDoctor),
        RetainedSurfaceOperation::LcmLoadSession => {
            decode!(LcmLoadSessionRequestV1, LcmLoadSession)
        }
        RetainedSurfaceOperation::LcmGrep => decode!(LcmGrepRequestV1, LcmGrep),
        RetainedSurfaceOperation::LcmDescribe => decode!(LcmDescribeRequestV1, LcmDescribe),
        RetainedSurfaceOperation::LcmExpand => decode!(LcmExpandRequestV1, LcmExpand),
        RetainedSurfaceOperation::LcmExpandQuery => {
            decode!(LcmExpandQueryRequestV1, LcmExpandQuery)
        }
        RetainedSurfaceOperation::SessionRefresh => None,
    }
}

fn decode_session_refresh(
    body: serde_json::Value,
    action: SessionRefreshActionV1,
) -> Option<RetainedSurfaceRequestV1> {
    serde_json::from_value::<SessionRefreshActionRequestV1>(body)
        .ok()
        .map(|request| SessionRefreshRequestV1::with_action(action, request))
        .map(RetainedSurfaceRequestV1::SessionRefresh)
}

pub(crate) fn result_value(
    result: tracedecay_application::ApplicationResult<RetainedSurfaceResultV1>,
) -> Result<
    tracedecay_application::ApplicationResult<serde_json::Value>,
    ApplicationSurfaceAdapterError,
> {
    match result {
        Ok(envelope) => Ok(Ok(tracedecay_application::ApplicationEnvelope {
            contract: envelope.contract,
            request_id: envelope.request_id,
            scope: envelope.scope,
            outcome: outcome_value(envelope.outcome)?,
        })),
        Err(problem) => Ok(Err(problem)),
    }
}

pub(super) fn outcome_value(
    outcome: tracedecay_application::ApplicationOutcome<RetainedSurfaceResultV1>,
) -> Result<
    tracedecay_application::ApplicationOutcome<serde_json::Value>,
    ApplicationSurfaceAdapterError,
> {
    use tracedecay_application::ApplicationOutcome;

    fn payload(
        payload: Option<RetainedSurfaceResultV1>,
    ) -> Result<Option<serde_json::Value>, ApplicationSurfaceAdapterError> {
        payload
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    }

    Ok(match outcome {
        ApplicationOutcome::Evidence(packet) => {
            ApplicationOutcome::Evidence(tracedecay_application::EvidencePacket {
                temporal: packet.temporal,
                authority: packet.authority,
                evidence_authorities: packet.evidence_authorities,
                coverage: packet.coverage,
                omissions: packet.omissions,
                scores: packet.scores,
                contributions: packet.contributions,
                page: packet.page,
                execution: packet.execution,
                payload: payload(packet.payload)?,
            })
        }
        ApplicationOutcome::Preview(preview) => {
            ApplicationOutcome::Preview(tracedecay_application::PreviewResult {
                preview_id: preview.preview_id,
                preview_digest: preview.preview_digest,
                effect_class: preview.effect_class,
                authority: preview.authority,
                expected_state: preview.expected_state,
                execution: preview.execution,
                payload: payload(preview.payload)?,
            })
        }
        ApplicationOutcome::Effect(effect) => {
            ApplicationOutcome::Effect(tracedecay_application::EffectResult {
                effect_id: effect.effect_id,
                effect_class: effect.effect_class,
                idempotency_key: effect.idempotency_key,
                authority: effect.authority,
                expected_state: effect.expected_state,
                execution: effect.execution,
                reconciliation: effect.reconciliation,
                receipt: effect.receipt,
                payload: payload(effect.payload)?,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn route_selected_session_refresh_rejects_embedded_action() {
        assert!(
            decode_request(
                RetainedSurfaceOperation::SessionRefreshStatus,
                json!({ "action": "status" }),
            )
            .is_none()
        );
    }

    #[test]
    fn fact_store_curate_rejects_caller_owned_authority() {
        for forbidden in [
            "operations",
            "proposal_id",
            "approve",
            "apply",
            "run_id",
            "task",
        ] {
            let mut value = serde_json::Map::new();
            value.insert(forbidden.to_owned(), serde_json::Value::Bool(true));
            assert!(
                decode_request(
                    RetainedSurfaceOperation::FactStoreCurate,
                    serde_json::Value::Object(value),
                )
                .is_none()
            );
        }
    }
}
