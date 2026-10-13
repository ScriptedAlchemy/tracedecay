//! MCP discovery definitions for the daemon-owned multi-root operations.

use serde_json::{Value, json};

use super::{mcp_input_schema, required_object_schema};
use crate::McpCatalogError;

fn selector_schema() -> serde_json::Value {
    required_object_schema(
        json!({
            "project_id": {
                "type": "string",
                "description": "Exact registered project identity."
            },
            "root": {
                "type": "string",
                "description": "Exact canonical registered root for that project."
            }
        }),
        &["project_id", "root"],
    )
}

pub(super) fn multi_root_scope_set_read_schema() -> Value {
    required_object_schema(
        json!({
            "scope_set_id": {
                "type": "string",
                "description": "Exact opaque scope-set identity."
            }
        }),
        &["scope_set_id"],
    )
}

pub(super) fn multi_root_scope_set_compare_and_swap_schema() -> Value {
    required_object_schema(
        json!({
            "scope_set_id": {
                "type": "string",
                "description": "Exact opaque scope-set identity."
            },
            "expected_revision": {
                "type": ["integer", "null"],
                "minimum": 1,
                "description": "Expected saved revision, or null when creating the set."
            },
            "roots": {
                "type": "array",
                "minItems": 1,
                "items": selector_schema(),
                "description": "Every exact registered root that belongs in this frozen scope set."
            }
        }),
        &["scope_set_id", "roots"],
    )
}

pub(super) fn multi_root_execute_schema() -> Result<Value, McpCatalogError> {
    let canonical = serde_json::to_value(schemars::schema_for!(
        tracedecay_contracts::MultiRootExecuteRequestV1
    ))
    .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    Ok(mcp_input_schema(&canonical))
}
