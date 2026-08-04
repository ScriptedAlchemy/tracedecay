use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    TaskHandoffGrantV1, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    TaskHandoffRedeemedV1, WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
    WorkflowDefinitionRegisterRequestV1, WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
};
use tracedecay_domain::WorkflowDefinitionV1;
use tracedecay_tool_catalog::RouteExposureV1;

use super::{ApplicationSurfaceAdapterError, invoke_registered_http};
use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::workflow_application_router(
        WorkflowExecutorOwner { executor },
    ))
}

pub(super) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_application::workflow_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in WorkflowOperation::ALL {
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
struct WorkflowExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkflowApplicationOwner for WorkflowExecutorOwner {
    fn invoke_workflow(
        &self,
        request: tracedecay_api::WorkflowHttpRequest,
    ) -> tracedecay_api::WorkflowInvocationFuture {
        Box::pin(invoke_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_operation(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: tracedecay_api::WorkflowHttpRequest,
) -> Response {
    let tracedecay_api::WorkflowHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    match operation {
        WorkflowOperation::RegisterDefinition => {
            let Ok(decoded) = serde_json::from_value::<WorkflowDefinitionRegisterRequestV1>(body)
            else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
                request_id.as_str(),
                WorkflowApplicationInvocationV1::RegisterDefinition(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<WorkflowDefinitionV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                        scope,
                        outcome:
                            WorkflowApplicationOutcomeV1::RegisterDefinition(
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
        WorkflowOperation::ActivateDefinition => {
            let Ok(decoded) = serde_json::from_value::<WorkflowDefinitionActivateRequestV1>(body)
            else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
                request_id.as_str(),
                WorkflowApplicationInvocationV1::ActivateDefinition(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<WorkflowActivationV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                        scope,
                        outcome:
                            WorkflowApplicationOutcomeV1::ActivateDefinition(
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
        WorkflowOperation::ExecuteFanOut => {
            let Ok(decoded) = serde_json::from_value::<WorkflowFanOutRequestV1>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
                request_id.as_str(),
                WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(decoded)),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<WorkflowExecutionTruthV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                        scope,
                        outcome:
                            WorkflowApplicationOutcomeV1::ExecuteFanOut(
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
        WorkflowOperation::HandoffIssue => {
            let Ok(decoded) = serde_json::from_value::<TaskHandoffIssueRequestV1>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
                request_id.as_str(),
                WorkflowApplicationInvocationV1::HandoffIssue(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<TaskHandoffGrantV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                        scope,
                        outcome:
                            WorkflowApplicationOutcomeV1::HandoffIssue(
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
        WorkflowOperation::HandoffRedeem => {
            let Ok(decoded) = serde_json::from_value::<TaskHandoffRedeemRequestV1>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
                request_id.as_str(),
                WorkflowApplicationInvocationV1::HandoffRedeem(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<TaskHandoffRedeemedV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                        scope,
                        outcome:
                            WorkflowApplicationOutcomeV1::HandoffRedeem(
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
