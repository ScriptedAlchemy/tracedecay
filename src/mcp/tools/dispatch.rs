//! MCP construction of transport-neutral catalog dispatches.
//!
//! MCP transport converts a protocol request ID and cancellation notification
//! into the typed fields below before this module runs. No handler, query,
//! store, or renderer is selected here.

use serde_json::json;
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

use super::ToolDefinition;

pub(crate) const DISPATCH_METADATA_KEY: &str = "tracedecay/dispatch";

/// Resolve the exact lifecycle policy advertised by the MCP catalog.
///
/// Catalog surface bindings keep their declared maximum; root handlers must
/// have a canonical binding row. Missing or invalid catalog authority is
/// unavailable, never replaced by fabricated metadata or an implicit timeout.
pub(crate) fn lifecycle_policy_for_tool(
    tool_name: &str,
) -> Result<Option<crate::mcp::server::McpToolLifecyclePolicy>, ApplicationSurfaceAdapterError> {
    if let Some(policy) = crate::application_surface::resolve_catalog_tool_lifecycle_policy(
        BindingSurface::Mcp,
        tool_name,
    )? {
        return Ok(Some(crate::mcp::server::McpToolLifecyclePolicy::new(
            std::time::Duration::from_millis(policy.maximum_millis),
            policy.externally_cancellable,
        )));
    }
    Ok(super::binding::lifecycle_policy_for_bound_tool(tool_name))
}

pub(crate) fn attach_dispatch_metadata(definitions: &mut [ToolDefinition]) {
    for definition in definitions {
        let policy = match lifecycle_policy_for_tool(&definition.name) {
            Ok(Some(policy)) => json!({
                "version": 1,
                "availability": { "state": "available" },
                "policy_source": "catalog",
                "deadline_ms": u64::try_from(policy.maximum_duration().as_millis())
                    .unwrap_or(u64::MAX),
                "externally_cancellable": policy.externally_cancellable(),
            }),
            Ok(None) => json!({
                "version": 1,
                "availability": {
                    "state": "unavailable",
                    "reason_code": "catalog_binding_missing",
                    "retryable": false,
                },
                "policy_source": "catalog",
            }),
            Err(error) => json!({
                "version": 1,
                "availability": {
                    "state": "unavailable",
                    "reason_code": "catalog_binding_unavailable",
                    "retryable": true,
                    "detail": error.to_string(),
                },
                "policy_source": "catalog",
            }),
        };
        let meta = definition.meta.get_or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(DISPATCH_METADATA_KEY.to_owned(), policy);
        } else {
            *meta = json!({ DISPATCH_METADATA_KEY: policy });
        }
    }
}

/// Reports an argument rejection to the executor and hands the error back so the
/// caller can return it unchanged.
async fn reject_surface_argument(
    executor: Option<&dyn DaemonInvocationExecutor>,
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
    error: ApplicationSurfaceAdapterError,
) -> ApplicationSurfaceAdapterError {
    observe_surface_argument_rejection(
        executor,
        BindingSurface::Mcp,
        operation,
        request_id,
        &error,
    )
    .await;
    error
}

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
            return Err(reject_surface_argument(executor, operation, &request_id, error).await);
        }
    };
    dispatched.invocation.invocation.scope = target;
    execute_application_surface(operation, dispatched, executor).await
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
            return Err(reject_surface_argument(executor, operation, &request_id, error).await);
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
            return Err(reject_surface_argument(executor, operation, &request_id, error).await);
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
