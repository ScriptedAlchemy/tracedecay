//! MCP (Model Context Protocol) server for the code graph.
//!
//! Provides a JSON-RPC 2.0 interface over stdio so that AI assistants can
//! query the code graph interactively. Exposes tools for searching, context
//! building, call graph traversal, impact analysis, and more.

pub(crate) mod hook_events;
pub(crate) mod project_route;
pub mod response_handles;
pub(crate) mod scope;
/// MCP server implementation.
pub mod server;
mod tool_analytics;
pub(crate) mod tool_call_deadline;

/// Tool definitions and dispatch.
pub mod tools;

/// JSON-RPC 2.0 transport types.
pub mod transport;

pub(crate) use server::DatabaseOwnerReconciler;
pub use server::McpServer;
pub use tools::{ToolDefinition, ToolResult, get_tool_definitions, handle_tool_call};
pub use transport::{
    ErrorCode, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTransport, ReplayTransport,
    StdioTransport,
};
