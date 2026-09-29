//! MCP tool dispatch for the code graph.
//!
//! Portable catalog types, definitions, rendering, the binding table, and
//! catalog discovery live in `tracedecay-mcp`. This module keeps the
//! daemon-coupled handlers and the composition root's dispatch.

pub(crate) mod handlers;

pub use handlers::{
    GraphToolOutcome, RetainedSurfaceExecution, ToolCallRegistryOptions,
    execute_graph_tool_surface, execute_retained_surface_tool, execute_work_tool_surface,
    execute_workflow_tool_surface, handle_application_surface, handle_tool_call,
    handle_tool_call_with_registry_options, registered_project_not_found,
    registered_project_selector_id, render_application_surface_result, render_retained_execution,
    render_settled_route_refusal, retained_tool_target, run_retained_surface_tool,
};
pub(crate) use handlers::{compute_graph_tool_for_owner, graph_tool_error_problem};
