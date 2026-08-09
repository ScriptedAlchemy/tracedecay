//! MCP discovery projected from the canonical Observatory executable binding.

use serde_json::json;
use tracedecay_tool_catalog::OperationId;

use crate::mcp::tools::ToolDefinition;

type DiscoveryResult<T> = Result<T, super::super::dispatch::McpDispatchMetadataError>;

pub(super) fn observatory_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_application::mcp_executable_binding_registry().map_err(|error| {
        super::super::dispatch::McpDispatchMetadataError::Initialization(error.to_string())
    })?;
    let operation_id = OperationId::new("operation.application.observatory_read".to_owned())
        .map_err(|_| invalid_observatory_discovery("MCP Observatory operation identity"))?;
    let binding = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .ok_or_else(|| invalid_observatory_discovery("MCP Observatory executable binding"))?;
    Ok(vec![ToolDefinition {
        name: "tracedecay_observatory_read".to_owned(),
        description: "Read this project's canonical Observatory and Costs models.".to_owned(),
        input_schema: binding.request_schema().body().clone(),
        output_schema: None,
        annotations: Some(json!({
            "readOnlyHint": binding.effect().is_read_only(),
            "title": "Read Observatory state",
        })),
        meta: None,
    }])
}

fn invalid_observatory_discovery(
    field: &'static str,
) -> super::super::dispatch::McpDispatchMetadataError {
    tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
        field,
        reason: "must expose the canonical Observatory executable binding",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::OperationId;

    use super::observatory_definitions;

    #[test]
    fn observatory_definition_projects_the_executable_request_schema() {
        let definition = observatory_definitions()
            .expect("observatory definition")
            .pop()
            .expect("one observatory definition");
        let registry = tracedecay_application::mcp_executable_binding_registry()
            .expect("observatory executable registry");
        let operation = OperationId::new("operation.application.observatory_read".to_owned())
            .expect("observatory operation id");
        let schema = registry
            .get(&operation)
            .and_then(|availability| availability.binding())
            .expect("observatory executable binding")
            .request_schema()
            .body();
        assert_eq!(definition.name, "tracedecay_observatory_read");
        assert_eq!(&definition.input_schema, schema);
    }
}
