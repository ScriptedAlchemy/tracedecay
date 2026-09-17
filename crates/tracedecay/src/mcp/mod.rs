//! MCP (Model Context Protocol) server for the code graph.
//!
//! Provides a JSON-RPC 2.0 interface over stdio so that AI assistants can
//! query the code graph interactively. Exposes tools for searching, context
//! building, call graph traversal, impact analysis, and more.

pub(crate) mod project_route;
/// MCP server implementation.
pub mod server;

/// Tool dispatch and daemon-coupled handlers.
pub mod tools;

pub(crate) use server::DatabaseOwnerReconciler;
pub use server::McpServer;
pub use tools::handle_tool_call;
