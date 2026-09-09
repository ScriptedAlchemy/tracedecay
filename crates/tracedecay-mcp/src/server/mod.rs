//! Transport-owned MCP request protocol and dispatch envelope.

mod dispatch;
mod protocol;

pub use dispatch::{
    McpDispatchParams, McpDispatchRequest, ToolCallParams, dispatch_is_independent_read,
};
pub use protocol::{McpMethod, classify_mcp_method, initialize_result, resources_list_result};
