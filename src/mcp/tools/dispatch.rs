//! MCP construction of transport-neutral catalog dispatches.
//!
//! MCP transport converts a protocol request ID and cancellation notification
//! into the typed fields below before this module runs. No handler, query,
//! store, or renderer is selected here.

use crate::daemon_client::{BindingResolver, DispatchedInvocation, resolve_dispatch};
pub use crate::daemon_client::{
    DispatchError as McpDispatchError, DispatchInput as McpDispatchInput,
    InvocationControls as McpInvocationControls,
};
use tracedecay_tool_catalog::BindingSurface;

/// Resolves the MCP binding and constructs the same canonical dispatch used by
/// the CLI adapter.
pub fn resolve_mcp_dispatch<T>(
    resolver: &impl BindingResolver,
    input: McpDispatchInput<T>,
) -> Result<DispatchedInvocation<T>, McpDispatchError> {
    resolve_dispatch(resolver, BindingSurface::Mcp, input)
}
