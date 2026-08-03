//! MCP construction of transport-neutral catalog dispatches.
//!
//! MCP transport converts a protocol request ID and cancellation notification
//! into the typed fields below before this module runs. No handler, query,
//! store, or renderer is selected here.

use serde_json::{Map, Value};
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
use crate::mcp::tools::ToolDefinition;

pub(crate) const DISPATCH_METADATA_KEY: &str = "tracedecay/dispatch";

#[derive(Debug, thiserror::Error)]
pub enum McpDispatchMetadataError {
    #[error("application catalog discovery failed: {0}")]
    Application(#[from] ApplicationSurfaceAdapterError),
    #[error("MCP dispatch catalog is invalid: {0}")]
    Catalog(#[from] tracedecay_tool_catalog::McpDispatchCatalogError),
    #[error("MCP dispatch metadata is invalid: {0}")]
    CatalogValidation(#[from] tracedecay_tool_catalog::CatalogValidationError),
    #[error("MCP dispatch catalog initialization failed: {0}")]
    Initialization(String),
    #[error("advertised MCP tool '{0}' has no dispatch contract")]
    MissingContract(String),
}

pub(crate) fn attach_dispatch_metadata(
    definitions: &mut [ToolDefinition],
) -> Result<(), McpDispatchMetadataError> {
    let catalog = super::binding::mcp_dispatch_catalog()?;
    let version = catalog.version();
    let fingerprint = catalog.fingerprint().to_string();
    for definition in definitions {
        let contract = catalog
            .contract(&definition.name)
            .ok_or_else(|| McpDispatchMetadataError::MissingContract(definition.name.clone()))?;
        let mut metadata = serde_json::to_value(contract)
            .map_err(|error| {
                tracedecay_tool_catalog::McpDispatchCatalogError::Serialization(error.to_string())
            })?
            .as_object()
            .cloned()
            .ok_or_else(|| {
                tracedecay_tool_catalog::McpDispatchCatalogError::Serialization(
                    "MCP dispatch contract did not serialize as an object".to_owned(),
                )
            })?;
        metadata.remove("tool_name");
        metadata.insert("version".to_owned(), Value::from(version));
        metadata.insert("fingerprint".to_owned(), Value::from(fingerprint.clone()));

        let annotations = definition
            .annotations
            .get_or_insert_with(|| Value::Object(Map::new()));
        let annotation_map = annotations.as_object_mut().ok_or_else(|| {
            tracedecay_tool_catalog::McpDispatchCatalogError::Serialization(format!(
                "MCP tool '{}' annotations are not an object",
                definition.name
            ))
        })?;
        annotation_map.insert("readOnlyHint".to_owned(), Value::Bool(contract.read_only()));

        let tool_meta = definition
            .meta
            .get_or_insert_with(|| Value::Object(Map::new()));
        let tool_meta = tool_meta.as_object_mut().ok_or_else(|| {
            tracedecay_tool_catalog::McpDispatchCatalogError::Serialization(format!(
                "MCP tool '{}' metadata is not an object",
                definition.name
            ))
        })?;
        tool_meta.insert(DISPATCH_METADATA_KEY.to_owned(), Value::Object(metadata));
    }
    Ok(())
}

#[cfg(test)]
mod metadata_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn missing_dispatch_contract_fails_closed() {
        let mut definitions = [ToolDefinition {
            name: "tracedecay_not_cataloged".to_owned(),
            description: "test".to_owned(),
            input_schema: json!({"type": "object"}),
            annotations: None,
            meta: None,
        }];
        assert!(matches!(
            attach_dispatch_metadata(&mut definitions),
            Err(McpDispatchMetadataError::MissingContract(name))
                if name == "tracedecay_not_cataloged"
        ));
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
