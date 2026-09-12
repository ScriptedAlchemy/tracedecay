//! Transport-owned MCP request protocol and dispatch envelope.

mod connection;
mod dispatch;
mod live_transcript_refresh;
mod project_host_admission_replay;
mod protocol;
mod read_coalescing;
mod rmcp;
mod session_refresh;
mod settlement;

pub use crate::lifecycle::{
    McpBackgroundTaskOwner, ProjectServerResponseLifecycle, StartupCatchUpMachineV1,
};
pub use connection::{
    McpConnectionContext, McpConnectionServer, McpConnectionState, McpResponseLease,
};
pub use dispatch::{
    McpDispatchParams, McpDispatchRequest, ToolCallParams, dispatch_is_independent_read,
    needs_lazy_sync_before_dispatch,
};
pub use live_transcript_refresh::{
    LiveTranscriptRefreshJoin, join_required_live_transcript_refresh,
};
pub use project_host_admission_replay::{
    ProjectHostAdmissionReplayTask, ProjectHostAdmissionReplayWorker,
};
pub use protocol::{McpMethod, classify_mcp_method, initialize_result, resources_list_result};
pub use read_coalescing::{
    IdenticalReadCoalescer, ReadCoalescingSnapshot, ReadFlight, ReadFlightClaim, ReadFlightLeader,
    tool_allows_identical_read_coalescing,
};
pub use rmcp::{
    RmcpConnectionAdapter, RmcpInitializeResponseDecorator, RmcpSelectedProjectResponseAuthority,
    RmcpWorkDeliverySettlement, await_dispatch_with_cancellation, project_server_retired_error,
    rmcp_response_result,
};
pub use session_refresh::DaemonSessionRefreshService;
pub use settlement::{
    ApplicationCancellationRegistration, DispatchControl, DispatchControlRequest, DispatchFailure,
    DispatchSettlement, DispatchToolPolicy, PreparedDispatchControl, RetainedDispatchAuthority,
    RetainedDispatchOutcome, RetainedDispatchRegistry, dispatch_cancelled_error,
};
