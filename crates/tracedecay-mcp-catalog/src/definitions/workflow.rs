//! MCP discovery projected from the canonical Workflow executable registry.
//!
//! The executable owns Workflow's operation set, request schemas, effects, and
//! availability. MCP contributes only its transport prefix and presentation,
//! the same division the Work family holds.

use tracedecay_api::WorkflowOperation;

use super::{FamilyOperation, project_executable_family};
use crate::ToolDefinition;

type DiscoveryResult<T> = Result<T, crate::McpCatalogError>;

/// Build every discoverable Workflow tool from the mounted executable bindings.
///
/// Discovery fails loudly if the registry is incomplete rather than silently
/// omitting a callable operation: a Workflow operation that no adapter
/// publishes is invisible to every agent.
pub(super) fn workflow_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_contracts::workflow_executable_binding_registry()
        .map_err(crate::McpCatalogError::CatalogValidation)?;
    let operations = WorkflowOperation::ALL
        .into_iter()
        .map(|operation| {
            let key = operation.operation_key();
            FamilyOperation {
                operation_id: operation.operation_id_str().to_owned(),
                name: format!("tracedecay_workflow_{key}"),
                title: format!("Workflow {key}"),
                description: format!("Invoke the Workflow {key} operation."),
            }
        })
        .collect::<Vec<_>>();
    project_executable_family(
        registry,
        &operations,
        (
            "MCP Workflow executable registry",
            "must expose exactly every canonical Workflow operation",
        ),
        (
            "MCP Workflow operation identity",
            "must name one canonical Workflow operation",
        ),
        (
            "MCP Workflow executable binding",
            "canonical Workflow operation is not executable",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::workflow_definitions;

    #[test]
    fn activation_help_names_catalog_and_revision_preconditions() {
        let definitions = workflow_definitions().expect("workflow definitions");
        let activate = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_workflow_activate_definition")
            .expect("workflow activation definition");
        let revision_help = activate.input_schema["properties"]["expected_revision"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(revision_help.contains("candidate disposition revision"));
        assert!(revision_help.contains('1'));

        let register = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_workflow_register_definition")
            .expect("workflow registration definition");
        let catalog_help = register.input_schema["$defs"]["WorkflowDefinition"]["properties"]
            ["pinned_catalog_digest"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(catalog_help.contains("live Work executable catalog digest"));
        assert!(catalog_help.to_ascii_lowercase().contains("validation"));
    }
}
