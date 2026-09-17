//! CLI construction of transport-neutral catalog dispatches.
//!
//! Command parsing remains in the command tree. This module accepts already
//! typed common controls and does not call application services or stores.

use std::time::Duration;

use tracedecay_contracts::{CancellationSignal, Deadline, PageRequest, RequestId};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult, ApplicationSurfaceRequest,
};
use tracedecay_daemon_protocol::{DaemonInvocationExecutor, RequestedOutputFormat};
use tracedecay_daemon_service::application_surface::{
    execute_application_surface, observe_surface_argument_rejection,
    resolve_application_surface_dispatch_with_controls,
};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

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

/// Delay before re-sending the same CLI application request when its typed
/// pre-admission problem explicitly directs an after-delay retry.
pub(crate) fn surface_retry_delay(result: &ApplicationSurfaceInvocationResult) -> Option<Duration> {
    result
        .result
        .as_ref()
        .err()?
        .problem
        .pre_admission_retry_delay()
}
