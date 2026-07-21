//! CLI construction of transport-neutral catalog dispatches.
//!
//! Command parsing remains in the command tree. This module accepts already
//! typed common controls and does not call application services or stores.

use tracedecay::daemon_client::{
    BindingResolver, DispatchError, DispatchInput, DispatchedInvocation, resolve_dispatch,
};
use tracedecay_tool_catalog::BindingSurface;

pub type CliDispatchInput<T> = DispatchInput<T>;
pub type CliDispatchError = DispatchError;

/// Resolves the CLI binding and constructs the canonical invocation.
///
/// Presentation format remains in the canonical invocation only until a later
/// caller converts it at the application boundary.
pub fn resolve_cli_dispatch<T>(
    resolver: &impl BindingResolver,
    input: CliDispatchInput<T>,
) -> Result<DispatchedInvocation<T>, CliDispatchError> {
    resolve_dispatch(resolver, BindingSurface::Cli, input)
}
