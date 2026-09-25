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
    fn activation_requires_a_positive_expected_revision() {
        let definitions = workflow_definitions().expect("workflow definitions");
        let activate = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_workflow_activate_definition")
            .expect("workflow activation definition");
        let schema = &activate.input_schema;
        assert_eq!(schema["properties"]["expected_revision"]["type"], "integer");
        assert_eq!(schema["properties"]["expected_revision"]["minimum"], 1);
        assert!(
            schema["required"]
                .as_array()
                .expect("required fields")
                .contains(&serde_json::json!("expected_revision"))
        );
    }
}
