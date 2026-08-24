use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::HandoffOperation;
use tracedecay_application::{
    IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1, ListTaskHandoffsRequestV1,
    ListTaskHandoffsResultV1, OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1,
    OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1,
};
use tracedecay_tool_catalog::RouteExposureV1;

use super::{ApplicationSurfaceAdapterError, invoke_registered_http};
use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::handoff_application_router(
        HandoffExecutorOwner { executor },
    ))
}

pub(super) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_application::handoff_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in HandoffOperation::ALL {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
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
        if route_path != operation.application_route_path() {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(())
}

#[derive(Clone)]
struct HandoffExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::HandoffApplicationOwner for HandoffExecutorOwner {
    fn invoke_handoff(
        &self,
        request: tracedecay_api::HandoffHttpRequest,
    ) -> tracedecay_api::HandoffInvocationFuture {
        Box::pin(invoke_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_operation(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: tracedecay_api::HandoffHttpRequest,
) -> Response {
    let tracedecay_api::HandoffHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    match operation {
        HandoffOperation::IssueTaskHandoff => {
            let Ok(decoded) = serde_json::from_value::<IssueTaskHandoffRequestV1>(body) else {
                return tracedecay_api::handoff_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::handoff_application(
                request_id.as_str(),
                HandoffApplicationInvocationV1::IssueTaskHandoff(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<IssueTaskHandoffResultV1, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::IssueTaskHandoff(
                                tracedecay_application::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_application::ApplicationOutcome::Effect(outcome),
                    )),
                    _ => None,
                },
            )
            .await
        }
        HandoffOperation::ListTaskHandoffs => {
            let Ok(decoded) = serde_json::from_value::<ListTaskHandoffsRequestV1>(body) else {
                return tracedecay_api::handoff_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::handoff_application(
                request_id.as_str(),
                HandoffApplicationInvocationV1::ListTaskHandoffs(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<ListTaskHandoffsResultV1, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
                // `Evidence`, not `Effect`: the enumeration commits nothing, so
                // there is no effect receipt to project. Matching `Effect` here
                // would drop every successful read on the floor.
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::ListTaskHandoffs(
                                tracedecay_application::ApplicationOutcome::Evidence(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_application::ApplicationOutcome::Evidence(outcome),
                    )),
                    _ => None,
                },
            )
            .await
        }
        HandoffOperation::OpenInvestigationHandoff => {
            let Ok(decoded) = serde_json::from_value::<OpenInvestigationHandoffRequestV1>(body)
            else {
                return tracedecay_api::handoff_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::handoff_application(
                request_id.as_str(),
                HandoffApplicationInvocationV1::OpenInvestigationHandoff(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<OpenInvestigationHandoffResultV1, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::OpenInvestigationHandoff(
                                tracedecay_application::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_application::ApplicationOutcome::Effect(outcome),
                    )),
                    _ => None,
                },
            )
            .await
        }
        HandoffOperation::OpenTaskHandoff => {
            let Ok(decoded) = serde_json::from_value::<OpenTaskHandoffRequestV1>(body) else {
                return tracedecay_api::handoff_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::handoff_application(
                request_id.as_str(),
                HandoffApplicationInvocationV1::OpenTaskHandoff(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<OpenTaskHandoffResultV1, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::OpenTaskHandoff(
                                tracedecay_application::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_application::ApplicationOutcome::Effect(outcome),
                    )),
                    _ => None,
                },
            )
            .await
        }
    }
}
