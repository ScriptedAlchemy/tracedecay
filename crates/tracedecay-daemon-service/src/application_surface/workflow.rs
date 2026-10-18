//! Workflow HTTP adapter over the daemon-owned application invocation.

use std::sync::Arc;

use super::registered_http::invoke_registered;
use super::require_public_catalog_route;
use axum::response::IntoResponse;
use tracedecay_api::WorkflowOperation;
use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;
use tracedecay_daemon_protocol::DaemonInvocationExecutor;
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationRequest, WorkflowApplicationInvocation,
};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::workflow_application_router(
        WorkflowExecutorOwner { executor },
    ))
}

pub(super) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_contracts::workflow_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in WorkflowOperation::ALL {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
                .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        require_public_catalog_route(registry, &operation_id, operation.application_route_path())?;
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
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            invoke_workflow_operation(Some(executor.as_ref()), request)
                .await
                .into_response()
        })
    }
}

/// Invoke the typed Workflow owner shared by the HTTP router and MCP adapter.
///
/// A missing executor stays a canonical Workflow runtime-unavailable response
/// selected by the operation's own catalog binding; it never becomes a
/// transport-specific MCP error, which is the same contract the Work adapter
/// holds and the reason both surfaces can be graded against one descriptor.
#[tracing::instrument(
    name = "application_surface.workflow.invoke",
    level = "trace",
    skip_all
)]
pub async fn invoke_workflow_operation(
    executor: Option<&dyn DaemonInvocationExecutor>,
    request: tracedecay_api::WorkflowHttpRequest,
) -> Result<tracedecay_api::HttpJsonEnvelope<serde_json::Value>, tracedecay_api::HttpAdapterError> {
    let tracedecay_api::WorkflowHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    let decoded = match WorkflowApplicationInvocation::decode(operation.operation_key(), body) {
        Ok(request) => request,
        Err(_) => return tracedecay_api::workflow_invalid_request(request_id),
    };
    let invocation = DaemonInvocationRequest::workflow_application(
        request_id.as_str(),
        decoded,
        tracedecay_contracts::now_micros(),
        controls.deadline.clone(),
        controls.cancellation.context(),
    );
    let Some(executor) = executor else {
        return super::registered_executor_unavailable(operation, request_id);
    };
    invoke_registered(
        executor,
        operation,
        request_id,
        controls,
        invocation,
        |response| match response {
            DaemonInvocationOutcome::WorkflowApplication { scope, outcome }
                if outcome.operation_key() == operation.operation_key() =>
            {
                outcome
                    .into_application_outcome()
                    .map(|outcome| Some((scope, outcome)))
            }
            _ => Ok(None),
        },
    )
    .await
}
