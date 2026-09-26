//! Name table for the graph-backed code-health report family.
//!
//! `tracedecay_runtime` reads daemon snapshots the composition root holds, so
//! the root dispatches it itself.

use std::path::Path;

use serde_json::Value;
use tracedecay_domain::errors::Result;

use super::{
    handle_dependency_depth, handle_dsm, handle_gini, handle_health, handle_test_map,
    handle_test_risk,
};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tools::response_trailers::append_request_cost;

/// Dispatches one code-health tool (`tracedecay_health`,
/// `tracedecay_test_risk`, ...) onto its handler over the verified graph
/// opened through `open`.
pub async fn dispatch_tool(
    response_handle_root: &Path,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let operation = match tool_name {
        "tracedecay_health" => "health_delta",
        "tracedecay_test_map"
        | "tracedecay_gini"
        | "tracedecay_dependency_depth"
        | "tracedecay_dsm"
        | "tracedecay_test_risk" => "health_read",
        _ => return Err(unknown_tool_error(tool_name)),
    };
    let graph = open(read(operation)?).await?;
    let root = response_handle_root;
    let mut result = match tool_name {
        "tracedecay_test_map" => handle_test_map(root, &graph, args, scope_prefix).await,
        "tracedecay_gini" => handle_gini(root, &graph, args, scope_prefix).await,
        "tracedecay_dependency_depth" => {
            handle_dependency_depth(root, &graph, args, scope_prefix).await
        }
        "tracedecay_health" => handle_health(root, &graph, args, scope_prefix).await,
        "tracedecay_dsm" => handle_dsm(root, &graph, args, scope_prefix).await,
        "tracedecay_test_risk" => handle_test_risk(root, &graph, args, scope_prefix).await,
        _ => Err(unknown_tool_error(tool_name)),
    }?;
    append_request_cost(&mut result, &graph.read_cost());
    Ok(result)
}
