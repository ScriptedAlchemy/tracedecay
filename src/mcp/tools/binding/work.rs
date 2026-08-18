//! Canonical Work projection into MCP dispatch metadata.
//!
//! The Work executable registry owns operation identity and every lifecycle
//! contract. This module derives MCP's transport names and dispatch entries
//! from that authority, keeping the static non-Work binding table independent.

use tracedecay_tool_catalog::ExecutableBindingV1;

use super::{DispatchCatalogBinding, McpToolDispatchGroup};

/// Resolve a Work MCP name through the canonical Work operation descriptor.
///
/// Work intentionally has no row in `MCP_TOOL_BINDINGS`: the executable
/// registry already owns its complete operation set and lifecycle contracts.
/// Keeping one handwritten row per operation here would create a second source
/// of names that could drift from the mounted Work surface.
pub(crate) fn work_operation_for_tool(tool_name: &str) -> Option<tracedecay_api::WorkOperation> {
    let operation_key = tool_name.strip_prefix("tracedecay_work_")?;
    tracedecay_api::WorkOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == operation_key)
}

/// Resolve the executable Work binding that names an MCP tool.
///
/// The Work executable registry is the source of effects, cancellation,
/// idempotency, and deadlines. MCP owns only the `tracedecay_work_` transport
/// prefix; it must not recreate those lifecycle contracts in a second table.
pub(super) fn work_executable_binding_for_tool(
    tool_name: &str,
) -> Result<Option<ExecutableBindingV1>, super::super::dispatch::McpDispatchMetadataError> {
    let Some(operation) = work_operation_for_tool(tool_name) else {
        return Ok(None);
    };
    let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
        .map_err(|_| {
            super::super::dispatch::McpDispatchMetadataError::CatalogValidation(
                tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
                    field: "MCP Work operation identity",
                    reason: "must name one canonical Work operation",
                },
            )
        })?;
    tracedecay_application::work_executable_binding(&operation_id)
        .map_err(super::super::dispatch::McpDispatchMetadataError::CatalogValidation)
}

/// Project every canonical Work executable into a dispatch entry.
pub(super) fn dispatch_catalog_bindings()
-> Result<Vec<DispatchCatalogBinding>, super::super::dispatch::McpDispatchMetadataError> {
    tracedecay_api::WorkOperation::ALL
        .into_iter()
        .map(|operation| {
            let name = format!("tracedecay_work_{}", operation.operation_key());
            let binding = work_executable_binding_for_tool(&name)?.ok_or({
                super::super::dispatch::McpDispatchMetadataError::CatalogValidation(
                    tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
                        field: "MCP Work executable binding",
                        reason: "canonical Work operation is not executable",
                    },
                )
            })?;
            Ok(DispatchCatalogBinding {
                name,
                group: Some(McpToolDispatchGroup::Work),
                executable_binding: Some(binding),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{
        McpToolDispatchGroup, dispatch_catalog_bindings, tool_accepts_registered_project_selector,
    };

    #[test]
    fn bindings_are_a_projection_of_the_executable_registry() {
        let registry = tracedecay_application::work_executable_binding_registry().unwrap();
        let work_bindings = dispatch_catalog_bindings()
            .unwrap()
            .into_iter()
            .filter(|binding| binding.group == Some(McpToolDispatchGroup::Work))
            .collect::<Vec<_>>();

        assert_eq!(
            work_bindings.len(),
            tracedecay_api::WorkOperation::ALL.len()
        );
        assert_eq!(work_bindings.len(), registry.iter().count());
        for operation in tracedecay_api::WorkOperation::ALL {
            let tool_name = format!("tracedecay_work_{}", operation.operation_key());
            let binding = work_bindings
                .iter()
                .find(|binding| binding.name == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} is not bound for MCP dispatch"));
            let operation_id =
                tracedecay_tool_catalog::OperationId::new(operation.operation_id()).unwrap();
            let executable = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{} is not executable", operation.operation_id()));
            assert_eq!(executable.effect().is_read_only(), operation.is_read_only());
            assert!(!tool_accepts_registered_project_selector(&binding.name));
        }
    }
}
