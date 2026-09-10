//! Code-health MCP dispatch family.

use serde_json::Value;

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use super::super::ToolCallRegistryOptions;
use super::super::health;
use super::admitted_graph_query;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::handlers::health as portable_health;
use tracedecay_mcp::handlers::redundancy as portable_redundancy;

/// Dispatch code-health and session-baseline tools (`tracedecay_health`,
/// `tracedecay_test_risk`, `tracedecay_runtime`, ...).
#[hotpath::measure(future = true, label = "mcp.dispatch.health")]
pub(in crate::mcp::tools::handlers) async fn dispatch_health_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_test_map" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_test_map(&graph, args, scope_prefix).await
        }
        "tracedecay_gini" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_gini(&graph, args, scope_prefix).await
        }
        "tracedecay_dependency_depth" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_dependency_depth(&graph, args, scope_prefix).await
        }
        "tracedecay_health" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_health(&graph, args, scope_prefix).await
        }
        "tracedecay_redundancy" => {
            let graph = admitted_graph_query(cg, &options, "redundancy").await?;
            portable_redundancy::handle_redundancy(&graph, args, scope_prefix).await
        }
        "tracedecay_runtime" => {
            health::handle_runtime(
                cg,
                args,
                options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                active_project_session_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                options.doctor_report_reader.as_ref(),
                options.generation_census_reader.as_ref(),
            )
            .await
        }
        "tracedecay_dsm" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_dsm(&graph, args, scope_prefix).await
        }
        "tracedecay_test_risk" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            portable_health::handle_test_risk(&graph, args, scope_prefix).await
        }
        _ => Err(super::super::unknown_tool_error(tool_name)),
    }
}
