//! Closed JSON-object schema construction shared by application tools.

use serde_json::{Value, json};
use tracedecay_tool_catalog::{ExecutableBindingRegistryV1, OperationId};

use super::required_object_schema;

type DiscoveryResult<T> = Result<T, super::super::dispatch::McpDispatchMetadataError>;

pub(super) fn canonical_application_request_schema(
    registry: &ExecutableBindingRegistryV1,
    operation: &'static str,
) -> DiscoveryResult<Value> {
    let operation_id = OperationId::new(format!("operation.application.{operation}"))
        .map_err(|_| invalid_terminal_application_discovery())?;
    registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .map(|binding| binding.request_schema().body().clone())
        .ok_or_else(invalid_terminal_application_discovery)
}

fn invalid_terminal_application_discovery() -> super::super::dispatch::McpDispatchMetadataError {
    tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
        field: "terminal application MCP executable binding",
        reason: "must expose the canonical executable request schema",
    }
    .into()
}

pub(super) fn closed_object_schema(
    properties: serde_json::Value,
    required: &[&str],
) -> serde_json::Value {
    let mut schema = required_object_schema(properties, required);
    schema["additionalProperties"] = json!(false);
    schema
}
