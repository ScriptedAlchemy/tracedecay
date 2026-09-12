//! MCP tool dispatch for the code graph.
//!
//! Portable catalog types, definitions, rendering, the binding table, and
//! catalog discovery live in `tracedecay-mcp`. This module keeps the
//! daemon-coupled handlers and the composition root's dispatch.

pub(crate) mod handlers;

use std::collections::HashSet;
use std::sync::LazyLock;

use tracedecay_mcp::get_tool_definitions;
use tracedecay_mcp::tools::dispatch::McpDispatchMetadataError;

pub(crate) use handlers::retained_catalog::{
    execute_profile_retained_mcp_tool, session_refresh_profile_scope_requested,
};
pub use handlers::{
    ToolCallRegistryOptions, handle_tool_call, handle_tool_call_with_registry_options,
};
pub(crate) use handlers::{
    handle_projectless_admin_cli, handle_projectless_hook_runtime,
    replay_projectless_hermes_host_admission,
};

/// Explicit owner for advertised tools awaiting typed application contracts.
///
/// These tools retain their existing root handlers, but they are no longer an
/// unclassified dispatch fallback: definition admission is mandatory, and any
/// application-catalog binding is resolved before this owner is entered.
pub struct LegacyToolCompatibilityOwner;

impl LegacyToolCompatibilityOwner {
    pub fn admits(tool_name: &str) -> std::result::Result<bool, McpDispatchMetadataError> {
        // Every dispatched compatibility tool call asks this, and rebuilding
        // the full schema catalog per call was the dominant per-dispatch cost.
        // The advertised name set is process-stable: the definitions are
        // static and the only host gate (`ast_grep_available`) is resolved
        // once per process, so membership is answered from a cached set.
        static ADVERTISED_TOOL_NAMES: LazyLock<std::result::Result<HashSet<String>, String>> =
            LazyLock::new(|| {
                Ok(get_tool_definitions()
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|definition| definition.name)
                    .collect())
            });
        match &*ADVERTISED_TOOL_NAMES {
            Ok(names) => Ok(names.contains(tool_name)),
            Err(error) => Err(McpDispatchMetadataError::Initialization(error.clone())),
        }
    }
}
