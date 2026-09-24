//! Graph and port reads served by the project's graph-tool owner.
//!
//! The owner computes each operation's typed catalog result; every surface
//! renders it here, so MCP and the CLI print the same tool result.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{NodeResultV1, RenamePreviewPrimitiveOutcomeV1};
use tracedecay_domain::errors::Result;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::handlers::graph::{
    compute_impact, compute_node, compute_redundancy, compute_rename_preview, compute_similar,
    not_found_tool_result,
};
use crate::handlers::info::{compute_port_order, compute_port_status, compute_todos};
use crate::handlers::support::{generic_tool_result, unknown_tool_error};
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::{McpToolContext, ToolResult};

/// Computes one graph-tool operation's typed result on the owner's side.
pub async fn compute_graph_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    match operation {
        ApplicationSurfaceOperation::Impact => {
            compute_impact(&open(read("impact")?).await?, args).await
        }
        ApplicationSurfaceOperation::Node => compute_node(&open(read("node")?).await?, args).await,
        ApplicationSurfaceOperation::Similar => compute_similar(ctx, args).await,
        ApplicationSurfaceOperation::Redundancy => compute_redundancy(ctx, args).await,
        ApplicationSurfaceOperation::RenamePreview => {
            compute_rename_preview(ctx, &open(read("rename_preview")?).await?, args).await
        }
        ApplicationSurfaceOperation::PortStatus => {
            compute_port_status(&open(read("port_status")?).await?, args).await
        }
        ApplicationSurfaceOperation::PortOrder => {
            compute_port_order(&open(read("port_order")?).await?, args).await
        }
        ApplicationSurfaceOperation::Todos => {
            compute_todos(&open(read("todos")?).await?, args, scope_prefix).await
        }
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}

/// Renders a typed graph-tool result as its tool result.
pub fn render_graph_tool(
    project_root: Option<&Path>,
    args: &Value,
    completion: GraphToolCompletionV1,
) -> Result<ToolResult> {
    let GraphToolCompletionV1 {
        result,
        touched_files,
    } = completion;
    match &result {
        GraphToolResultV1::Node(NodeResultV1::NotFound(not_found))
        | GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(not_found)) => {
            not_found_tool_result(not_found)
        }
        _ => Ok(generic_tool_result(
            project_root,
            args,
            &result.result_value()?,
            touched_files,
        )),
    }
}
