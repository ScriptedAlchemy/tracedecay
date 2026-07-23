//! CLI construction of transport-neutral catalog dispatches.
//!
//! Command parsing remains in the command tree. This module accepts already
//! typed common controls and does not call application services or stores.

use tracedecay::application_surface::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, execute_application_surface,
    observe_surface_argument_rejection, resolve_application_surface_dispatch,
    resolve_application_surface_dispatch_with_controls,
};
use tracedecay::daemon_client::{
    BindingResolver, DaemonInvocationClient, DispatchError, DispatchInput, DispatchedInvocation,
    RequestedOutputFormat, resolve_dispatch,
};
use tracedecay_application::{CancellationSignal, Deadline, PageRequest, RequestId};
use tracedecay_tool_catalog::BindingSurface;

#[allow(dead_code)] // Plan 21 CLI dispatch surface — staged
pub type CliDispatchInput<T> = DispatchInput<T>;
#[allow(dead_code)] // Plan 21 CLI dispatch surface — staged
pub type CliDispatchError = DispatchError;

/// Resolves the CLI binding and constructs the canonical invocation.
///
/// Presentation format remains in the canonical invocation only until a later
/// caller converts it at the application boundary.
#[allow(dead_code)] // Plan 21 CLI dispatch surface — staged
pub fn resolve_cli_dispatch<T>(
    resolver: &impl BindingResolver,
    input: CliDispatchInput<T>,
) -> Result<DispatchedInvocation<T>, CliDispatchError> {
    resolve_dispatch(resolver, BindingSurface::Cli, input)
}

pub async fn resolve_cli_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    deadline: Deadline,
    cancellation: CancellationSignal,
    client: Option<&DaemonInvocationClient>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let page = match PageRequest::first(10) {
        Ok(page) => page,
        Err(error) => {
            let error = ApplicationSurfaceAdapterError::from(error);
            observe_surface_argument_rejection(
                client,
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
                client,
                BindingSurface::Cli,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    execute_application_surface(operation, dispatched, client).await
}

#[allow(dead_code)] // Plan 21 CLI dispatch surface — staged
pub fn resolve_cli_application_surface_dispatch(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    resolve_application_surface_dispatch(
        BindingSurface::Cli,
        operation,
        request_id,
        request,
        requested_format,
    )
}
