//! Runtime-telemetry MCP dispatch family.

use serde_json::Value;

use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_project::project::TraceDecay;

use super::super::ToolCallRegistryOptions;
use super::{admitted_project_authorities, admitted_runtime_snapshots, admitted_tool_context};
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::handlers::health as portable_health;
use tracedecay_mcp::handlers::unknown_tool_error;

/// Dispatch `tracedecay_runtime`: it snapshots the daemon census and doctor
/// report the root holds, and stamps the build the root was compiled as.
#[hotpath::measure(future = true, label = "mcp.dispatch.health")]
pub(in crate::mcp::tools::handlers) async fn dispatch_health_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    if tool_name != "tracedecay_runtime" {
        return Err(unknown_tool_error(tool_name));
    }
    let project = admitted_project_authorities(cg, &options)?;
    let include_doctor = args
        .get("doctor_report")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snapshots = admitted_runtime_snapshots(&options, include_doctor).await;
    let ctx = admitted_tool_context(&options, &project, &snapshots, None)?;
    portable_health::handle_runtime(
        &ctx,
        args,
        options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
        tracedecay_project::version::build_version()?,
    )
    .await
}
