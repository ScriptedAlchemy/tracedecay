//! The advertised MCP tool catalog: `~160` JSON-Schema tool descriptors, the
//! host-capability filtering that decides which of them a process advertises,
//! and the name lists (format-capable, registered-project readers) that the
//! descriptors are projected from.
//!
//! The catalog is data that two independent consumers read: the MCP server
//! answers `tools/list` and admits dispatch from it, and the agent-host
//! installers write permission allowlists and generated plugin schema files
//! from it. Neither may reach the other — the server composition sits above
//! the installers — so the catalog lives below both. Everything here is
//! process-static: the application catalog snapshot and the `ast-grep` host
//! probe are the only runtime inputs, and both are cached once per process.
//!
//! An unavailable catalog is a typed [`McpCatalogError`], never an empty tool
//! set, so no consumer can mistake a composition failure for "this host
//! advertises no tools".

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::wildcard_imports)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod catalog_error;
mod definitions;
mod project_access;

pub use catalog_error::McpCatalogError;
pub use definitions::ast_grep::{
    AstGrepDiagnostics, ast_grep_available, ast_grep_diagnostics, ast_grep_diagnostics_json,
    ast_grep_outline_available,
};
pub use definitions::{
    SEARCH_MAX_LEXICAL_ANCHOR_BYTES, SEARCH_MAX_LEXICAL_ANCHORS, ToolRegistryMode,
    apply_context_warming_budget, context_description, context_warming_description,
    explore_call_budget, format_capable_tool_names, get_maximal_tool_definitions,
    get_maximal_tool_definitions_with_budget, get_tool_definitions,
    get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
    internal_daemon_tool_definition, mcp_input_schema, project_catalog_discovery_scope,
    retain_host_available_tool_definitions, tool_defaults_to_markdown,
};
pub use project_access::registered_project_reader_tool_names;

/// Maximum character length for a tool response before truncation.
///
/// Advertised as the ceiling of the `max_chars` schema property, so the
/// renderer that enforces it and the schema that promises it share one value.
pub const MAX_RESPONSE_CHARS: usize = 15_000;

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
