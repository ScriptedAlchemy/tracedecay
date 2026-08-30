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

// Phase 2 removes these composition-root re-exports once callers import mcp directly.
pub use tracedecay_mcp::tools::{render, renderers};
pub use tracedecay_mcp::{
    RESERVED_FLAGS_FOOTER, ToolDefinition, ToolResult, render_tool_cli_help,
    resolve_property_schema, short_tool_name,
};

pub(crate) use binding::{
    mcp_dispatch_contract, tool_dispatches_registered_project_reader,
    tool_dispatches_source_edit_effect, tool_supports_live_cancellation,
};
pub use catalog_discovery::{
    default_catalog_discovery_authority, get_catalog_filtered_tool_definitions_with_budget,
    get_catalog_filtered_tool_definitions_with_warming_budget,
};
// Phase 2 removes these composition-root re-exports once callers import mcp directly.
pub(crate) use handlers::hook_runtime::structured_hook_error_data;
pub(crate) use handlers::retained_catalog::{
    execute_profile_retained_mcp_tool, retained_mcp_operation,
};
pub(crate) use handlers::{
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryListingCommand, ProjectRegistryListingFuture,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryListingView,
    ProjectRegistryReadPort, ProjectRegistrySelector, SessionRefreshAction, SessionRefreshCommand,
    SessionRefreshCoverageView, SessionRefreshFrontierView, SessionRefreshProgressView,
    SessionRefreshReceiptView, SessionRefreshServiceOutcome, SessionRefreshServicePort,
    handle_projectless_admin_cli, handle_projectless_hook_runtime,
    replay_projectless_hermes_host_admission, utc_micros_value,
};
pub use handlers::{
    SessionAuthorities, ToolCallRegistryOptions, handle_tool_call,
    handle_tool_call_with_registry_options,
};
pub use tracedecay_mcp::{
    ToolRegistryMode, ast_grep_available, ast_grep_diagnostics_json, ast_grep_outline_available,
    context_description, explore_call_budget, format_capable_tool_names, get_tool_definitions,
    get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
    internal_daemon_tool_definition, project_catalog_discovery_scope, tool_defaults_to_markdown,
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
