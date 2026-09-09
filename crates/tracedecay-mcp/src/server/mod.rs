//! Transport-owned MCP request protocol and dispatch envelope.

mod connection;
mod dispatch;
mod protocol;
mod rmcp;
mod settlement;

pub use crate::lifecycle::{
    McpBackgroundTaskOwner, ProjectServerResponseLifecycle, StartupCatchUpMachineV1,
};
pub use connection::{
    McpConnectionContext, McpConnectionServer, McpConnectionState, McpResponseLease,
};
pub use dispatch::{
    McpDispatchParams, McpDispatchRequest, ToolCallParams, dispatch_is_independent_read,
};
pub use protocol::{McpMethod, classify_mcp_method, initialize_result, resources_list_result};
pub use rmcp::{
    RmcpConnectionAdapter, RmcpInitializeResponseDecorator, RmcpSelectedProjectResponseAuthority,
    RmcpWorkDeliverySettlement, await_dispatch_with_cancellation, project_server_retired_error,
    rmcp_response_result,
};
pub use settlement::{
    ApplicationCancellationRegistration, DispatchControl, DispatchControlRequest, DispatchFailure,
    DispatchSettlement, DispatchToolPolicy, PreparedDispatchControl, RetainedDispatchAuthority,
    RetainedDispatchOutcome, RetainedDispatchRegistry, dispatch_cancelled_error,
};
