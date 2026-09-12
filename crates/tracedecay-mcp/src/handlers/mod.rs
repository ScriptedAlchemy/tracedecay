//! MCP handler adapters: transport-request decoding, business-owner calls,
//! and response shaping.
//!
//! Handlers depend on application, query, protocol, and catalog crates, and
//! on [`crate::McpToolContext`] for anything the daemon admitted for the
//! call. None of them constructs a project route, opens a store, or mints an
//! authorization.

pub mod analysis;
pub mod ast_grep;
mod bounded_search;
pub mod dashboard_delivery;
pub mod dashboard_git_correlation;
pub mod dashboard_lcm;
pub mod dependency_hints;
pub mod git;
pub mod graph;
pub mod grep;
pub mod health;
pub mod info;
mod multi_root;
pub mod redundancy;
mod retained_response;
mod session_authorities;
pub mod support;
mod verified_read;
pub mod work;
pub mod workflow_family;

pub use bounded_search::run_bounded_search;
pub use multi_root::handle_multi_root;
pub use retained_response::{
    retained_problem_envelope, retained_safe_diagnostic, validated_retained_response,
};
pub use session_authorities::SessionAuthorities;
pub use support::{
    CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request, effective_path, generic_tool_result,
    rendered_tool_result, require_node_id, require_object_args, require_positive_limit,
    take_internal_context_memory_analytics, text_tool_result, tool_json, tool_json_with_md,
    unique_file_paths, unknown_tool_error,
};
pub use verified_read::{VerifiedGraphOpen, VerifiedGraphOpenFuture, verified_read_operation};
pub use work::handle_work;
pub use workflow_family::handle_workflow;
