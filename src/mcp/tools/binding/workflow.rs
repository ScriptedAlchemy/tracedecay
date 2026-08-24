//! Canonical Workflow projection into MCP dispatch metadata.
//!
//! This is the Work projection's sibling, and deliberately its mirror image.
//! The Workflow executable registry already owns the family's operation set,
//! request schemas, effects, deadlines, idempotency and cancellation; MCP owns
//! only the `tracedecay_workflow_` transport prefix. Writing one handwritten
//! row per operation into `MCP_TOOL_BINDINGS` would create a second source of
//! names, and a second source is exactly how this family came to be mounted on
//! CLI and HTTP but not on MCP at all.

use tracedecay_tool_catalog::ExecutableBindingV1;

use super::{DispatchCatalogBinding, McpToolDispatchGroup};

/// Resolve a Workflow MCP name through the canonical Workflow descriptor.
pub(crate) fn workflow_operation_for_tool(
    tool_name: &str,
) -> Option<tracedecay_api::WorkflowOperation> {
    let operation_key = tool_name.strip_prefix("tracedecay_workflow_")?;
    tracedecay_api::WorkflowOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == operation_key)
}

/// Resolve the executable Workflow binding that names an MCP tool.
pub(super) fn workflow_executable_binding_for_tool(
    tool_name: &str,
) -> Result<Option<ExecutableBindingV1>, super::super::dispatch::McpDispatchMetadataError> {
    let Some(operation) = workflow_operation_for_tool(tool_name) else {
        return Ok(None);
    };
    let operation_id =
        tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
            .map_err(|_| invalid_workflow_binding("must name one canonical Workflow operation"))?;
    let registry = tracedecay_application::workflow_executable_binding_registry()
        .map_err(super::super::dispatch::McpDispatchMetadataError::CatalogValidation)?;
    Ok(registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .cloned())
}

/// Project every canonical Workflow executable into a dispatch entry.
pub(super) fn dispatch_catalog_bindings()
-> Result<Vec<DispatchCatalogBinding>, super::super::dispatch::McpDispatchMetadataError> {
    tracedecay_api::WorkflowOperation::ALL
        .into_iter()
        .map(|operation| {
            let name = format!("tracedecay_workflow_{}", operation.operation_key());
            let binding = workflow_executable_binding_for_tool(&name)?.ok_or_else(|| {
                invalid_workflow_binding("canonical Workflow operation is not executable")
            })?;
            Ok(DispatchCatalogBinding {
                name,
                group: Some(McpToolDispatchGroup::Workflow),
                executable_binding: Some(binding),
            })
        })
        .collect()
}

fn invalid_workflow_binding(
    reason: &'static str,
) -> super::super::dispatch::McpDispatchMetadataError {
    super::super::dispatch::McpDispatchMetadataError::CatalogValidation(
        tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
            field: "MCP Workflow executable binding",
            reason,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::{
        McpToolDispatchGroup, dispatch_catalog_bindings, tool_accepts_registered_project_selector,
    };

    #[test]
    fn bindings_are_a_projection_of_the_executable_registry() {
        let registry = tracedecay_application::workflow_executable_binding_registry().unwrap();
        let workflow_bindings = dispatch_catalog_bindings()
            .unwrap()
            .into_iter()
            .filter(|binding| binding.group == Some(McpToolDispatchGroup::Workflow))
            .collect::<Vec<_>>();

        assert_eq!(
            workflow_bindings.len(),
            tracedecay_api::WorkflowOperation::ALL.len()
        );
        assert_eq!(workflow_bindings.len(), registry.iter().count());
        for operation in tracedecay_api::WorkflowOperation::ALL {
            let tool_name = format!("tracedecay_workflow_{}", operation.operation_key());
            let binding = workflow_bindings
                .iter()
                .find(|binding| binding.name == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} is not bound for MCP dispatch"));
            let operation_id =
                tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
                    .unwrap();
            registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{} is not executable", operation.operation_id_str()));
            assert!(!tool_accepts_registered_project_selector(&binding.name));
        }
    }

    #[test]
    fn the_transport_prefix_is_the_only_name_authority() {
        for operation in tracedecay_api::WorkflowOperation::ALL {
            let name = format!("tracedecay_workflow_{}", operation.operation_key());
            assert_eq!(super::workflow_operation_for_tool(&name), Some(operation));
        }
        assert_eq!(
            super::workflow_operation_for_tool("tracedecay_workflow_missing"),
            None
        );
        // The legacy read-only `operation.application.workflows` query is a
        // different operation entirely and must not be captured by the prefix.
        assert_eq!(
            super::workflow_operation_for_tool("tracedecay_workflows"),
            None
        );
    }
}
