//! MCP tool dispatch for the code graph.
//!
//! Portable catalog types, definitions, and rendering live in `tracedecay-mcp`.
//! This module keeps daemon-coupled dispatch, bindings, and handlers.

pub(crate) mod binding;
pub(crate) mod catalog_discovery;
pub mod dispatch;
pub(crate) mod handlers;
#[cfg(test)]
mod plugin_conformance_tests;

use std::collections::HashSet;
use std::sync::LazyLock;

use tracedecay_mcp::get_tool_definitions;

pub(crate) use binding::{
    mcp_dispatch_contract, tool_dispatches_registered_project_reader,
    tool_dispatches_source_edit_effect, tool_supports_live_cancellation,
};
pub use catalog_discovery::{
    catalog_discovery_tools_list_payload, default_catalog_discovery_authority,
    get_catalog_filtered_tool_definitions_with_budget,
    get_catalog_filtered_tool_definitions_with_warming_budget,
};
pub(crate) use handlers::retained_catalog::{
    execute_profile_retained_mcp_tool, retained_mcp_operation,
};
pub use handlers::{
    SessionAuthorities, ToolCallRegistryOptions, handle_tool_call,
    handle_tool_call_with_registry_options,
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
    pub const OWNER: &'static str = "root MCP tool-dispatch migration";
    pub const REASON: &'static str =
        "typed ApplicationSurfaceRequest contract has not yet landed for this tool family";

    pub fn admits(
        tool_name: &str,
    ) -> std::result::Result<bool, dispatch::McpDispatchMetadataError> {
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
            Err(error) => Err(dispatch::McpDispatchMetadataError::Initialization(
                error.clone(),
            )),
        }
    }
}
