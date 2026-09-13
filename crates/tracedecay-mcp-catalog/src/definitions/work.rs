//! MCP discovery projected from the canonical Work executable registry.
//!
//! The executable owns Work's operation set, request schemas, effects, and
//! availability. MCP contributes only its transport prefix and presentation.

use serde_json::json;
use tracedecay_api::WorkOperation;
use tracedecay_tool_catalog::{CatalogValidationError, OperationId};

use crate::ToolDefinition;

type DiscoveryResult<T> = Result<T, crate::McpCatalogError>;

/// Build every discoverable Work tool from the mounted executable bindings.
///
/// Every mounted Work operation must have an executable binding; discovery
/// fails loudly if the registry is incomplete rather than silently omitting a
/// callable operation. Schema bodies come from the same registry the HTTP
/// owner validates, so MCP cannot omit a required request field or admit one
/// that typed Work decoding rejects.
pub(super) fn work_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_contracts::work_executable_binding_registry()
        .map_err(crate::McpCatalogError::CatalogValidation)?;
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
                description: operation.description().to_owned(),
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

fn invalid_work_discovery(field: &'static str, reason: &'static str) -> crate::McpCatalogError {
    CatalogValidationError::InvalidValue { field, reason }.into()
}

#[cfg(test)]
mod tests {
    use super::work_definitions;
    use tracedecay_api::WorkOperation;

    #[test]
    fn discovery_uses_discriminating_work_descriptions() {
        let definitions = work_definitions().expect("Work definitions");
        assert_eq!(definitions.len(), WorkOperation::ALL.len());
        assert!(definitions.iter().all(|definition| {
            !definition.description.starts_with("Invoke the Work ")
                && !definition.description.trim().is_empty()
        }));

        for (name, description) in [
            (
                "tracedecay_work_generate_proposal",
                "Generate an evidence-calibrated proposal for one task against an exact current Work graph.",
            ),
            (
                "tracedecay_work_resume_attempts",
                "Recover open attempts after daemon restart and fence those that require an explicit retry.",
            ),
            (
                "tracedecay_work_mutate_graph",
                "Apply an exact prepared Work graph mutation using its preserved identity and revision pins.",
            ),
            (
                "tracedecay_work_release_placement",
                "Release or quarantine one run's placement at the expected authority version without deleting bytes.",
            ),
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(definition.description, description);
        }
    }
}
