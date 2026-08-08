//! MCP discovery projected from the canonical Workflow executable registry.
//!
//! The executable owns Workflow's operation set, request schemas, effects, and
//! availability. MCP contributes only its transport prefix and presentation —
//! the same division the Work family holds, so the two closed families cannot
//! drift into two different discovery stories.

use serde_json::json;
use tracedecay_api::WorkflowOperation;
use tracedecay_tool_catalog::{CatalogValidationError, OperationId};

use crate::mcp::tools::ToolDefinition;

type DiscoveryResult<T> = Result<T, super::super::dispatch::McpDispatchMetadataError>;

/// Build every discoverable Workflow tool from the mounted executable bindings.
///
/// Discovery fails loudly if the registry is incomplete rather than silently
/// omitting a callable operation: a Workflow operation that no adapter
/// publishes is invisible to every agent, which is exactly how all sixteen of
/// them stayed off MCP while CLI and HTTP carried them.
pub(super) fn workflow_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_application::workflow_executable_binding_registry()
        .map_err(super::super::dispatch::McpDispatchMetadataError::CatalogValidation)?;
    if registry.iter().count() != WorkflowOperation::ALL.len() {
        return Err(invalid_workflow_discovery(
            "MCP Workflow executable registry",
            "must expose exactly every canonical Workflow operation",
        ));
    }
    WorkflowOperation::ALL
        .into_iter()
        .map(|operation| {
            let operation_id =
                OperationId::new(operation.operation_id_str().to_owned()).map_err(|_| {
                    invalid_workflow_discovery(
                        "MCP Workflow operation identity",
                        "must name one canonical Workflow operation",
                    )
                })?;
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .ok_or_else(|| {
                    invalid_workflow_discovery(
                        "MCP Workflow executable binding",
                        "canonical Workflow operation is not executable",
                    )
                })?;
            Ok(ToolDefinition {
                name: format!("tracedecay_workflow_{}", operation.operation_key()),
                description: format!(
                    "Invoke the Workflow {} operation.",
                    operation.operation_key()
                ),
                input_schema: binding.request_schema().body().clone(),
                annotations: Some(json!({
                    "readOnlyHint": binding.effect().is_read_only(),
                    "title": format!("Workflow {}", operation.operation_key()),
                })),
                meta: None,
            })
        })
        .collect()
}

fn invalid_workflow_discovery(
    field: &'static str,
    reason: &'static str,
) -> super::super::dispatch::McpDispatchMetadataError {
    CatalogValidationError::InvalidValue { field, reason }.into()
}
