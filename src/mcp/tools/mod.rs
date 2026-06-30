//! MCP tool definitions and dispatch for the code graph.
//!
//! Split into two sub-modules:
//! - `definitions`: JSON Schema tool descriptors (`def_*` functions)
//! - `handlers`: tool call implementations (`handle_*` functions)

mod definitions;
mod dispatch_policy;
mod handlers;
pub(crate) mod render;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use definitions::{
    ast_grep_available, ast_grep_diagnostics_json, ast_grep_outline_available, context_description,
    explore_call_budget, format_capable_tool_names, get_tool_definitions,
    get_tool_definitions_with_budget,
};
pub use handlers::{
    handle_profile_scoped_lcm_tool_call, handle_tool_call, handle_tool_call_with_registry,
};

/// Maximum character length for a tool response before truncation.
const MAX_RESPONSE_CHARS: usize = 15_000;

/// A tool definition exposed by the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    /// MCP tool annotations (readOnlyHint, title, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// MCP tool metadata (e.g. anthropic/alwaysLoad).
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// The result of a tool call, including the JSON response and the file
/// paths that were touched (used to track saved tokens).
#[derive(Debug)]
pub struct ToolResult {
    /// The JSON-RPC result payload.
    pub value: Value,
    /// Unique file paths referenced in the result.
    pub touched_files: Vec<String>,
    /// Internal analytics metadata for the server runtime. This must never be
    /// serialized into the tool response payload.
    internal_analytics: Option<Value>,
}

impl ToolResult {
    pub fn new(value: Value, touched_files: Vec<String>) -> Self {
        Self {
            value,
            touched_files,
            internal_analytics: None,
        }
    }

    #[must_use]
    pub fn with_internal_analytics(mut self, internal_analytics: Value) -> Self {
        self.internal_analytics = Some(internal_analytics);
        self
    }

    pub fn internal_analytics(&self) -> Option<&Value> {
        self.internal_analytics.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_result_constructors_keep_internal_analytics_explicit() {
        let result = ToolResult::new(json!({"content": []}), vec!["src/lib.rs".to_string()]);
        assert_eq!(result.value, json!({"content": []}));
        assert_eq!(result.touched_files, vec!["src/lib.rs"]);
        assert!(result.internal_analytics().is_none());

        let result = result.with_internal_analytics(json!({"context_memory": {"match_count": 1}}));
        assert_eq!(
            result.internal_analytics(),
            Some(&json!({"context_memory": {"match_count": 1}}))
        );
    }
}
