use std::sync::Arc;

use super::registered_http::invoke_registered_http;
use super::require_public_catalog_route;
use axum::response::Response;
use tracedecay_api::HandoffOperation;
use tracedecay_contracts::{
    IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1, ListTaskHandoffsRequestV1,
    ListTaskHandoffsResultV1, OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1,
    OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1,
};
use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;
use tracedecay_daemon_protocol::DaemonInvocationExecutor;
use tracedecay_daemon_protocol::{HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::handoff_application_router(
        HandoffExecutorOwner { executor },
    ))
}

pub(super) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_contracts::handoff_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in HandoffOperation::ALL {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
                .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        require_public_catalog_route(&registry, &operation_id, operation.application_route_path())?;
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

#[hotpath::measure(label = "application_surface.handoff.invoke", future = true)]
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
            let invocation =
                tracedecay_daemon_protocol::DaemonInvocationRequest::handoff_application(
                    request_id.as_str(),
                    HandoffApplicationInvocationV1::IssueTaskHandoff(decoded),
                    tracedecay_contracts::now_micros(),
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
                    tracedecay_daemon_protocol::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::IssueTaskHandoff(
                                tracedecay_contracts::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_contracts::ApplicationOutcome::Effect(outcome),
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
            let invocation =
                tracedecay_daemon_protocol::DaemonInvocationRequest::handoff_application(
                    request_id.as_str(),
                    HandoffApplicationInvocationV1::ListTaskHandoffs(decoded),
                    tracedecay_contracts::now_micros(),
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
                    tracedecay_daemon_protocol::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::ListTaskHandoffs(
                                tracedecay_contracts::ApplicationOutcome::Evidence(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_contracts::ApplicationOutcome::Evidence(outcome),
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
            let invocation =
                tracedecay_daemon_protocol::DaemonInvocationRequest::handoff_application(
                    request_id.as_str(),
                    HandoffApplicationInvocationV1::OpenInvestigationHandoff(decoded),
                    tracedecay_contracts::now_micros(),
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
                    tracedecay_daemon_protocol::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::OpenInvestigationHandoff(
                                tracedecay_contracts::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_contracts::ApplicationOutcome::Effect(outcome),
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
            let invocation =
                tracedecay_daemon_protocol::DaemonInvocationRequest::handoff_application(
                    request_id.as_str(),
                    HandoffApplicationInvocationV1::OpenTaskHandoff(decoded),
                    tracedecay_contracts::now_micros(),
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
                    tracedecay_daemon_protocol::DaemonInvocationOutcome::HandoffApplication {
                        scope,
                        outcome:
                            HandoffApplicationOutcomeV1::OpenTaskHandoff(
                                tracedecay_contracts::ApplicationOutcome::Effect(outcome),
                            ),
                    } => Some((
                        scope,
                        tracedecay_contracts::ApplicationOutcome::Effect(outcome),
                    )),
                    _ => None,
                },
            )
            .await
        }
    }
}
