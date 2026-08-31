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

pub(crate) use status::graph_statistics_value;
pub(super) use status::{handle_active_project, handle_admin_sync, handle_status};

pub(super) use serde_json::{Value, json};

pub(super) use crate::tracedecay::{BranchDiagnostics, TraceDecay};
pub(super) use tracedecay_domain::errors::{Result, TraceDecayError};
pub(super) use tracedecay_global_db::{RegisteredGlobalDb, SessionIngestHealth};
pub(super) use tracedecay_runtime_core::storage::{StorageMode, StoreKind};

pub(super) use super::support::{generic_tool_result, rendered_tool_result};
pub(super) use tracedecay_mcp::ToolResult;
pub(super) use tracedecay_mcp::tools::render::Md;

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}
