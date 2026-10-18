//! HTTP and MCP ownership for the canonical Work surface.
//!
//! Both transports enter this module after normalizing their request controls.
//! Work request decoding, daemon invocation construction, catalog binding
//! lookup, and typed response envelopes therefore have one owner.

use std::sync::Arc;

use super::registered_http::invoke_registered;
use super::require_public_catalog_route;
use axum::response::IntoResponse;
use tracedecay_api::{WorkHttpRequest, WorkOperation};
use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;
use tracedecay_daemon_protocol::DaemonInvocationExecutor;
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationRequest, WorkApplicationInvocationV1,
};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::work_application_router(WorkExecutorOwner {
        executor,
    }))
}

pub(super) fn dashboard_router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::work_dashboard_router(WorkExecutorOwner {
        executor,
    }))
}

/// Refuse to mount Work unless the executable catalog advertises every
/// canonical descriptor operation at the application path this build serves.
pub(crate) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_contracts::work_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in WorkOperation::ALL {
        let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
            .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        require_public_catalog_route(registry, &operation_id, operation.application_route_path())?;
    }
    Ok(())
}

#[derive(Clone)]
struct WorkExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkApplicationOwner for WorkExecutorOwner {
    fn invoke_work(&self, request: WorkHttpRequest) -> tracedecay_api::WorkInvocationFuture {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            invoke_work_operation(Some(executor.as_ref()), request)
                .await
                .into_response()
        })
    }
}

/// Invoke the typed Work owner shared by the HTTP router and MCP adapter.
///
/// A missing executor remains a canonical Work runtime-unavailable response;
/// it never becomes a transport-specific MCP error.
#[tracing::instrument(name = "application_surface.work.invoke", level = "trace", skip_all)]
pub(crate) async fn invoke_work_operation(
    executor: Option<&dyn DaemonInvocationExecutor>,
    request: WorkHttpRequest,
) -> Result<tracedecay_api::HttpJsonEnvelope<serde_json::Value>, tracedecay_api::HttpAdapterError> {
    let WorkHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    let decoded = match WorkApplicationInvocationV1::decode(operation.operation_key(), body) {
        Ok(request) => request,
        Err(_) => return tracedecay_api::work_invalid_request(request_id),
    };
    let invocation = DaemonInvocationRequest::work_application(
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
            DaemonInvocationOutcome::WorkApplication { scope, outcome }
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
