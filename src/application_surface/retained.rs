//! HTTP owner for the canonical retained application operations.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_application::retained_surfaces::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreGetRequestV1, FactStoreListRequestV1, FactStoreProbeRequestV1,
    FactStoreReasonRequestV1, FactStoreRelatedRequestV1, FactStoreRemoveRequestV1,
    FactStoreRequestV1, FactStoreSearchRequestV1, FactStoreUpdateRequestV1, LcmDescribeRequestV1,
    LcmDoctorRequestV1, LcmExpandQueryRequestV1, LcmExpandRequestV1, LcmGrepRequestV1,
    LcmLoadSessionRequestV1, LcmStatusRequestV1, MemoryStatusRequestV1, MessageSearchRequestV1,
    RetainedSurfaceOperation, RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
    SessionRefreshActionRequestV1, SessionRefreshActionV1, SessionRefreshRequestV1,
    SessionsForRequestV1, WorkflowsRequestV1,
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
    invoke_registered_http::<RetainedSurfaceResultV1, _>(
        executor.as_ref(),
        operation,
        request_id,
        controls,
        invocation,
        |outcome| match outcome {
            DaemonInvocationOutcome::RetainedApplication { scope, outcome } => {
                Some((scope, outcome))
            }
            _ => None,
        },
    )
    .await
}

fn decode_request(
    operation: RetainedSurfaceOperation,
    body: serde_json::Value,
) -> Option<RetainedSurfaceRequestV1> {
    macro_rules! decode_fact {
        ($request:ty, $variant:ident) => {
            serde_json::from_value::<$request>(body)
                .ok()
                .map(FactStoreRequestV1::$variant)
                .map(RetainedSurfaceRequestV1::FactStore)
        };
    }
    macro_rules! decode {
        ($request:ty, $variant:ident) => {
            serde_json::from_value::<$request>(body)
                .ok()
                .map(RetainedSurfaceRequestV1::$variant)
        };
    }
    match operation {
        RetainedSurfaceOperation::FactStoreAdd => decode_fact!(FactStoreAddRequestV1, Add),
        RetainedSurfaceOperation::FactStoreSearch => {
            decode_fact!(FactStoreSearchRequestV1, Search)
        }
        RetainedSurfaceOperation::FactStoreProbe => decode_fact!(FactStoreProbeRequestV1, Probe),
        RetainedSurfaceOperation::FactStoreRelated => {
            decode_fact!(FactStoreRelatedRequestV1, Related)
        }
        RetainedSurfaceOperation::FactStoreReason => {
            decode_fact!(FactStoreReasonRequestV1, Reason)
        }
        RetainedSurfaceOperation::FactStoreContradict => {
            decode_fact!(FactStoreContradictRequestV1, Contradict)
        }
        RetainedSurfaceOperation::FactStoreGet => decode_fact!(FactStoreGetRequestV1, Get),
        RetainedSurfaceOperation::FactStoreUpdate => {
            decode_fact!(FactStoreUpdateRequestV1, Update)
        }
        RetainedSurfaceOperation::FactStoreRemove => {
            decode_fact!(FactStoreRemoveRequestV1, Remove)
        }
        RetainedSurfaceOperation::FactStoreList => decode_fact!(FactStoreListRequestV1, List),
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
        RetainedSurfaceOperation::FactStore | RetainedSurfaceOperation::SessionRefresh => None,
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
}
