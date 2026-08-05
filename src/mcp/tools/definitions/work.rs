//! MCP projections of the canonical typed Work product bindings.

use tracedecay_api::WorkOperation;
use tracedecay_tool_catalog::{CatalogValidationError, OperationId};

use crate::mcp::tools::ToolDefinition;

use super::{def, def_rw};

pub(super) fn product_definitions() -> Result<Vec<ToolDefinition>, CatalogValidationError> {
    let registry = tracedecay_application::work_executable_binding_registry()?;
    WorkOperation::PRODUCT
        .into_iter()
        .map(|operation| {
            let operation_id = OperationId::new(operation.operation_id()).map_err(|_| {
                CatalogValidationError::InvalidValue {
                    field: "Work MCP operation ID",
                    reason: "must be a canonical catalog identifier",
                }
            })?;
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .ok_or(CatalogValidationError::InvalidValue {
                    field: "Work MCP executable binding",
                    reason: "must be available before the MCP tool is advertised",
                })?;
            let name = format!("tracedecay_{}", operation.operation_key());
            let title = title(operation);
            let description = description(operation);
            let schema = binding.request_schema().body().clone();
            Ok(if operation.is_read_only() {
                def(&name, title, description, schema)
            } else {
                def_rw(&name, title, description, schema)
            })
        })
        .collect()
}

fn title(operation: WorkOperation) -> &'static str {
    match operation {
        WorkOperation::ProductSnapshot => "Read Work product snapshot",
        WorkOperation::ProductProjections => "Read Work product projections",
        WorkOperation::TaskEvidence => "Read task evidence",
        WorkOperation::ExpandTaskEvidence => "Expand task evidence",
        WorkOperation::GenerateWorkProposal => "Generate Work proposal",
        WorkOperation::ApplyWorkCommand => "Apply Work product command",
        _ => "Work product operation",
    }
}

fn description(operation: WorkOperation) -> &'static str {
    match operation {
        WorkOperation::ProductSnapshot => {
            "Read the current canonical product graph through the registered project authority."
        }
        WorkOperation::ProductProjections => {
            "Read version-aligned kanban, DAG, timeline, causal, critical-path, and workload projections."
        }
        WorkOperation::TaskEvidence => {
            "Read bounded evidence links for one task at the current graph version."
        }
        WorkOperation::ExpandTaskEvidence => {
            "Expand one task evidence link through the canonical redaction and content authority."
        }
        WorkOperation::GenerateWorkProposal => {
            "Generate an evidence-bound task shape, sizing, and provider-route proposal."
        }
        WorkOperation::ApplyWorkCommand => {
            "Apply one command-idempotent, expected-version Work graph mutation."
        }
        _ => "Execute a canonical Work product operation.",
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::OperationId;

    use super::product_definitions;

    #[test]
    fn product_tool_schemas_are_the_canonical_work_binding_bodies() {
        let definitions = product_definitions().unwrap();
        let registry = tracedecay_application::work_executable_binding_registry().unwrap();
        assert_eq!(
            definitions.len(),
            tracedecay_api::WorkOperation::PRODUCT.len()
        );
        for definition in definitions {
            let operation = definition.name.strip_prefix("tracedecay_").unwrap();
            let operation_id = OperationId::new(format!("operation.work.{operation}")).unwrap();
            let binding = registry.get(&operation_id).unwrap().binding().unwrap();
            assert_eq!(&definition.input_schema, binding.request_schema().body());
        }
    }
}
