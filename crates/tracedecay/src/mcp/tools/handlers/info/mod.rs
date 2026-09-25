//! Project-info handlers that still depend on composition-root authorities.
//!
//! Portable file-inspection and registry/config/remote-status/simplify tools
//! live in `tracedecay_mcp::handlers::info`.

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod remote_status_dispatch_tests;
mod status;

pub(super) use status::handle_admin_sync;

pub(super) use serde_json::json;

pub(super) use tracedecay_domain::errors::{Result, TraceDecayError};
pub(super) use tracedecay_mcp::ToolResult;
pub(super) use tracedecay_project::project::TraceDecay;
