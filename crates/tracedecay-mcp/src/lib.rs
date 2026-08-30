//! Portable MCP catalog, rendering, and JSON-RPC transport.
//!
//! This crate owns daemon-free MCP surface: JSON-RPC contracts, concrete
//! stdio/channel/replay transports, tool definitions, response truncation,
//! and canonical application-result presentation. Server construction,
//! connection lifecycle, and handlers that reach daemon internals stay in
//! the composition root.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
#![allow(clippy::large_futures)]
#![allow(unreachable_pub)]

pub mod application_output;
mod catalog_error;
pub mod context_headings;
pub mod host_cli;
pub mod jsonrpc;
pub mod lifecycle;
pub mod output_format;
pub mod path_tree;
pub mod project_access;
pub mod response_handles;
pub mod tools;
pub mod transport;

pub use catalog_error::McpCatalogError;
pub use context_headings::{
    CODE_CONTEXT_HEADING, CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING,
    CONTEXT_EXTENSION_POINTS_HEADING, CONTEXT_INDEX_COVERAGE_HINT_HEADING,
    CONTEXT_MEMORY_FEEDBACK_HINT, CONTEXT_MEMORY_MATCHES_HEADING, CONTEXT_PRIORITY_HEADINGS,
    CONTEXT_RELATED_SYMBOLS_HEADING, CONTEXT_SEEN_NODE_IDS_LABEL, CONTEXT_TEST_COVERAGE_HEADING,
};
pub use jsonrpc::{ErrorCode, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTransport};
pub use lifecycle::{
    McpConnectionLifecyclePort, McpLifecycleDrainFuture, McpRequestActivity, McpShutdownStatus,
};
pub use output_format::{RequestedOutputFormat, requested_output_format};
pub use project_access::registered_project_reader_tool_names;
pub use tools::render::format_relative_time;
pub use tools::{
    MAX_RESPONSE_CHARS, RESERVED_FLAGS_FOOTER, ToolDefinition, ToolRegistryMode, ToolResult,
    apply_context_warming_budget, ast_grep_available, ast_grep_diagnostics_json,
    ast_grep_outline_available, context_description, explore_call_budget,
    format_capable_tool_names, get_maximal_tool_definitions,
    get_maximal_tool_definitions_with_budget, get_tool_definitions,
    get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
    internal_daemon_tool_definition, project_catalog_discovery_scope, render_tool_cli_help,
    retain_host_available_tool_definitions, short_tool_name, tool_defaults_to_markdown,
};
