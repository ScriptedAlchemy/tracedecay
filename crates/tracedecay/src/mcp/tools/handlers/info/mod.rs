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

/// Snapshot + serialize the generation census for the `tracedecay://status`
/// resource. Handler families read the already-computed snapshot from
/// [`tracedecay_mcp::McpToolContext`]; this wrapper keeps the resource on
/// the composition-root reader until that surface moves.
pub(crate) async fn graph_statistics_value(
    generation_census_reader: Option<
        &tracedecay_session_memory::runtime_telemetry::GenerationCensusReader,
    >,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    let census = match generation_census_reader {
        Some(reader) => reader().await,
        None => {
            tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                reason: tracedecay_session_memory::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
            }
        }
    };
    tracedecay_mcp::handlers::info::graph_statistics_value(Some(&census))
}

pub(super) use serde_json::{Value, json};

pub(super) use crate::project::TraceDecay;
pub(super) use tracedecay_domain::errors::{Result, TraceDecayError};
pub(super) use tracedecay_mcp::ToolResult;
