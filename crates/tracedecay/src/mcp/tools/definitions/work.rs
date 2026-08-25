//! MCP discovery projected from the canonical Work executable registry.
//!
//! The executable owns Work's operation set, request schemas, effects, and
//! availability. MCP contributes only its transport prefix and presentation.

use serde_json::json;
use tracedecay_api::WorkOperation;
use tracedecay_tool_catalog::{CatalogValidationError, OperationId};

use crate::mcp::tools::ToolDefinition;

type DiscoveryResult<T> = Result<T, super::super::dispatch::McpDispatchMetadataError>;

/// Build every discoverable Work tool from the mounted executable bindings.
///
/// Every mounted Work operation must have an executable binding; discovery
/// fails loudly if the registry is incomplete rather than silently omitting a
/// callable operation. Schema bodies come from the same registry the HTTP
/// owner validates, so MCP cannot omit a required request field or admit one
/// that typed Work decoding rejects.
pub(super) fn work_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_application::work_executable_binding_registry()
        .map_err(super::super::dispatch::McpDispatchMetadataError::CatalogValidation)?;
    if registry.iter().count() != WorkOperation::ALL.len() {
        return Err(invalid_work_discovery(
            "MCP Work executable registry",
            "must expose exactly every canonical Work operation",
        ));
    }
    WorkOperation::ALL
        .into_iter()
        .map(|operation| {
            let operation_id = OperationId::new(operation.operation_id()).map_err(|_| {
                invalid_work_discovery(
                    "MCP Work operation identity",
                    "must name one canonical Work operation",
                )
            })?;
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .ok_or_else(|| {
                    invalid_work_discovery(
                        "MCP Work executable binding",
                        "canonical Work operation is not executable",
                    )
                })?;
            Ok(ToolDefinition {
                name: format!("tracedecay_work_{}", operation.operation_key()),
                description: format!("Invoke the Work {} operation.", operation.operation_key()),
                input_schema: binding.request_schema().body().clone(),
                annotations: Some(json!({
                    "readOnlyHint": binding.effect().is_read_only(),
                    "title": format!("Work {}", operation.operation_key()),
                })),
                meta: None,
            })
        })
        .collect()
}

fn invalid_work_discovery(
    field: &'static str,
    reason: &'static str,
) -> super::super::dispatch::McpDispatchMetadataError {
    CatalogValidationError::InvalidValue { field, reason }.into()
}
