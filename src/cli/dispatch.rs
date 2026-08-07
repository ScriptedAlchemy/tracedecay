//! CLI construction of transport-neutral catalog dispatches.
//!
//! Command parsing remains in the command tree. This module accepts already
//! typed common controls and does not call application services or stores.

use tracedecay::application_surface::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, execute_application_surface,
    observe_surface_argument_rejection, resolve_application_surface_dispatch_with_controls,
};
use tracedecay::daemon_client::{DaemonInvocationExecutor, RequestedOutputFormat};
use tracedecay_application::{CancellationSignal, Deadline, PageRequest, RequestId};
use tracedecay_tool_catalog::BindingSurface;

pub async fn resolve_cli_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    deadline: Deadline,
    cancellation: CancellationSignal,
    executor: Option<&dyn DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let page = match PageRequest::first(10) {
        Ok(page) => page,
        Err(error) => {
            let error = ApplicationSurfaceAdapterError::from(error);
            observe_surface_argument_rejection(
                executor,
                BindingSurface::Cli,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let dispatched = match resolve_application_surface_dispatch_with_controls(
        BindingSurface::Cli,
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
                BindingSurface::Cli,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    execute_application_surface(operation, dispatched, executor).await
}
