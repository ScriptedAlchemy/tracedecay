//! Transport-owned MCP request protocol and dispatch envelope.

mod dispatch;
mod protocol;
mod settlement;

pub use crate::lifecycle::{
    McpBackgroundTaskOwner, ProjectServerResponseLifecycle, StartupCatchUpMachineV1,
};
pub use dispatch::{
    McpDispatchParams, McpDispatchRequest, ToolCallParams, dispatch_is_independent_read,
};
pub use protocol::{McpMethod, classify_mcp_method, initialize_result, resources_list_result};
pub use settlement::{
    ApplicationCancellationRegistration, DispatchControl, DispatchControlRequest, DispatchFailure,
    DispatchSettlement, DispatchToolPolicy, PreparedDispatchControl, RetainedDispatchAuthority,
    RetainedDispatchOutcome, RetainedDispatchRegistry, dispatch_cancelled_error,
};
