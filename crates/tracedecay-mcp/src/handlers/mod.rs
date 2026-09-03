//! Portable MCP handler adapters that depend only on application, protocol,
//! and catalog crates.

pub mod analysis;
pub mod ast_grep;
mod bounded_search;
pub mod graph;
pub mod grep;
pub mod health;
pub mod info;
mod multi_root;
mod retained_response;
pub mod support;

pub use bounded_search::run_bounded_search;
pub use multi_root::handle_multi_root;
pub use retained_response::{
    retained_problem_envelope, retained_safe_diagnostic, validated_retained_response,
};
pub use support::{
    CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request, effective_path, generic_tool_result,
    rendered_tool_result, require_node_id, require_object_args, require_positive_limit,
    take_internal_context_memory_analytics, text_tool_result, tool_json, tool_json_with_md,
    unique_file_paths,
};
