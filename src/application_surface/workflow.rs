//! Workflow HTTP adapter over the daemon-owned application invocation.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::WorkflowFanOutRequestV1;

use crate::daemon_contract::{WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1};

use super::invoke_registered_http;

#[derive(Clone)]
pub(crate) struct WorkflowExecutorOwner {
    pub(crate) executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkflowApplicationOwner for WorkflowExecutorOwner {
    fn invoke_workflow(
        &self,
        request: tracedecay_api::WorkflowHttpRequest,
    ) -> tracedecay_api::WorkflowInvocationFuture {
        Box::pin(invoke_workflow_operation(
            Arc::clone(&self.executor),
            request,
        ))
    }
}

async fn invoke_workflow_operation(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    request: tracedecay_api::WorkflowHttpRequest,
) -> Response {
    let tracedecay_api::WorkflowHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    match operation {
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
            invoke_registered_http::<tracedecay_domain::WorkflowRunProjectionV1, _>(
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
    }
}
