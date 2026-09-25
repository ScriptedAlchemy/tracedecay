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
    match tool_name {
        "tracedecay_test_map" => {
            handle_test_map(
                response_handle_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_gini" => {
            handle_gini(
                response_handle_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_dependency_depth" => {
            handle_dependency_depth(
                response_handle_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_health" => {
            handle_health(
                response_handle_root,
                &open(read("health_delta")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_dsm" => {
            handle_dsm(
                response_handle_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_test_risk" => {
            handle_test_risk(
                response_handle_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
