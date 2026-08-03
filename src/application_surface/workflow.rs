//! Workflow HTTP adapter over the daemon-owned application invocation.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    TaskHandoffIssueRequest, TaskHandoffRedeemRequest, WorkflowDefinitionActivateRequest,
    WorkflowDefinitionRegisterRequest, WorkflowFanOutRequest,
};

use crate::daemon_contract::{WorkflowApplicationInvocation, WorkflowApplicationOutcome};

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
        WorkflowOperation::RegisterDefinition => {
            let Ok(decoded) = serde_json::from_value::<WorkflowDefinitionRegisterRequest>(body)
            else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            invoke::<tracedecay_domain::WorkflowDefinition>(
                executor,
                operation,
                request_id,
                controls,
                WorkflowApplicationInvocation::RegisterDefinition(decoded),
                register_definition_outcome,
            )
            .await
        }
        WorkflowOperation::ActivateDefinition => {
            let Ok(decoded) = serde_json::from_value::<WorkflowDefinitionActivateRequest>(body)
            else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            invoke::<tracedecay_application::WorkflowActivation>(
                executor,
                operation,
                request_id,
                controls,
                WorkflowApplicationInvocation::ActivateDefinition(decoded),
                activate_definition_outcome,
            )
            .await
        }
        WorkflowOperation::ExecuteFanOut => {
            let Ok(decoded) = serde_json::from_value::<WorkflowFanOutRequest>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            invoke::<tracedecay_domain::WorkflowRunProjection>(
                executor,
                operation,
                request_id,
                controls,
                WorkflowApplicationInvocation::ExecuteFanOut(Box::new(decoded)),
                execute_fan_out_outcome,
            )
            .await
        }
        WorkflowOperation::HandoffIssue => {
            let Ok(decoded) = serde_json::from_value::<TaskHandoffIssueRequest>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            invoke::<tracedecay_application::TaskHandoffGrant>(
                executor,
                operation,
                request_id,
                controls,
                WorkflowApplicationInvocation::HandoffIssue(decoded),
                handoff_issue_outcome,
            )
            .await
        }
        WorkflowOperation::HandoffRedeem => {
            let Ok(decoded) = serde_json::from_value::<TaskHandoffRedeemRequest>(body) else {
                return tracedecay_api::workflow_invalid_request_response(request_id);
            };
            invoke::<tracedecay_application::TaskHandoffRedeemed>(
                executor,
                operation,
                request_id,
                controls,
                WorkflowApplicationInvocation::HandoffRedeem(decoded),
                handoff_redeem_outcome,
            )
            .await
        }
    }
}

async fn invoke<T>(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    operation: WorkflowOperation,
    request_id: tracedecay_application::RequestId,
    controls: tracedecay_api::HttpApplicationControls,
    request: WorkflowApplicationInvocation,
    select: fn(
        crate::daemon_contract::DaemonInvocationOutcome,
    ) -> Option<(
        tracedecay_application::ResolvedScope,
        tracedecay_application::ApplicationOutcome<T>,
    )>,
) -> Response
where
    T: serde::Serialize,
{
    let invocation = crate::daemon_contract::DaemonInvocationRequest::workflow_application(
        request_id.as_str(),
        request,
        crate::daemon_client::invocation_now_micros(),
        controls.deadline.clone(),
        controls.cancellation.context(),
    );
    invoke_registered_http::<T, _>(
        executor, operation, request_id, controls, invocation, select,
    )
    .await
}

macro_rules! workflow_selector {
    ($name:ident, $variant:ident, $output:ty) => {
        fn $name(
            outcome: crate::daemon_contract::DaemonInvocationOutcome,
        ) -> Option<(
            tracedecay_application::ResolvedScope,
            tracedecay_application::ApplicationOutcome<$output>,
        )> {
            match outcome {
                crate::daemon_contract::DaemonInvocationOutcome::WorkflowApplication {
                    scope,
                    outcome:
                        WorkflowApplicationOutcome::$variant(
                            tracedecay_application::ApplicationOutcome::Effect(outcome),
                        ),
                } => Some((
                    scope,
                    tracedecay_application::ApplicationOutcome::Effect(outcome),
                )),
                _ => None,
            }
        }
    };
}

workflow_selector!(
    register_definition_outcome,
    RegisterDefinition,
    tracedecay_domain::WorkflowDefinition
);
workflow_selector!(
    activate_definition_outcome,
    ActivateDefinition,
    tracedecay_application::WorkflowActivation
);
workflow_selector!(
    execute_fan_out_outcome,
    ExecuteFanOut,
    tracedecay_domain::WorkflowRunProjection
);
workflow_selector!(
    handoff_issue_outcome,
    HandoffIssue,
    tracedecay_application::TaskHandoffGrant
);
workflow_selector!(
    handoff_redeem_outcome,
    HandoffRedeem,
    tracedecay_application::TaskHandoffRedeemed
);
