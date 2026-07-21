//! CLI construction of transport-neutral catalog dispatches.
//!
//! Command parsing remains in the command tree. This module accepts already
//! typed common controls and does not call application services or stores.

use tracedecay::daemon_client::{BindingResolver, DispatchedInvocation, resolve_dispatch};
pub use tracedecay::daemon_client::{
    CancellationRef, CanonicalInvocation, DispatchError as CliDispatchError,
    DispatchInput as CliDispatchInput, InvocationControls as CliInvocationControls,
    RequestedOutputFormat, ScopeSelector,
};
use tracedecay_tool_catalog::BindingSurface;

/// Resolves the CLI binding and constructs the canonical invocation.
///
/// Presentation format remains in `CanonicalInvocation` only until a later
/// caller converts it with `into_application_invocation`.
pub fn resolve_cli_dispatch<T>(
    resolver: &impl BindingResolver,
    input: CliDispatchInput<T>,
) -> Result<DispatchedInvocation<T>, CliDispatchError> {
    resolve_dispatch(resolver, BindingSurface::Cli, input)
}
