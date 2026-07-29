//! MCP construction of transport-neutral catalog dispatches.
//!
//! MCP transport converts a protocol request ID and cancellation notification
//! into the typed fields below before this module runs. No handler, query,
//! store, or renderer is selected here.

use tracedecay_application::{
    CancellationSignal, Deadline, InvocationTarget, PageRequest, RequestId,
};
use tracedecay_tool_catalog::BindingSurface;

use crate::application_surface::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, execute_application_surface,
    observe_surface_argument_rejection, resolve_application_surface_dispatch,
    resolve_application_surface_dispatch_with_controls,
};
use crate::daemon_client::{DaemonInvocationExecutor, DispatchedInvocation, RequestedOutputFormat};

pub async fn resolve_mcp_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    executor: Option<&dyn DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    resolve_mcp_application_surface_for_target(
        operation,
        request_id,
        request,
        requested_format,
        InvocationTarget::CurrentProject,
        executor,
    )
    .await
}

pub async fn resolve_mcp_application_surface_for_target(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    target: InvocationTarget,
    executor: Option<&dyn DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let mut dispatched = match resolve_mcp_application_surface_dispatch(
        operation,
        request_id.clone(),
        request,
        requested_format,
    ) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            observe_surface_argument_rejection(
                executor,
                BindingSurface::Mcp,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    dispatched.invocation.invocation.scope = target;
    execute_application_surface(operation, dispatched, executor).await
}

#[allow(clippy::too_many_arguments)]
pub async fn resolve_mcp_application_surface_with_controls(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    deadline: Deadline,
    cancellation: CancellationSignal,
    executor: Option<&dyn DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    resolve_mcp_application_surface_with_controls_for_target(
        operation,
        request_id,
        request,
        requested_format,
        deadline,
        cancellation,
        InvocationTarget::CurrentProject,
        executor,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn resolve_mcp_application_surface_with_controls_for_target(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    deadline: Deadline,
    cancellation: CancellationSignal,
    target: InvocationTarget,
    executor: Option<&dyn DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let page = match PageRequest::first(10) {
        Ok(page) => page,
        Err(error) => {
            let error = ApplicationSurfaceAdapterError::from(error);
            observe_surface_argument_rejection(
                executor,
                BindingSurface::Mcp,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let mut dispatched = match resolve_application_surface_dispatch_with_controls(
        BindingSurface::Mcp,
        operation,
        request_id.clone(),
        request,
        page,
        Some(deadline),
        cancellation,
        requested_format,
    ) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            observe_surface_argument_rejection(
                executor,
                BindingSurface::Mcp,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    dispatched.invocation.invocation.scope = target;
    execute_application_surface(operation, dispatched, executor).await
}

pub fn resolve_mcp_application_surface_dispatch(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    resolve_application_surface_dispatch(
        BindingSurface::Mcp,
        operation,
        request_id,
        request,
        requested_format,
    )
}
