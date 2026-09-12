//! Name table for the git-aware family, bounded by the caller's deadline.

use serde_json::Value;
use tracedecay_domain::errors::Result;

use super::{
    git_dispatch_deadline_result, handle_affected, handle_branch_diff, handle_branch_list,
    handle_branch_search, handle_changelog, handle_commit_context, handle_diff_context,
    handle_pr_context,
};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tool_context::McpToolContext;

/// Dispatches one git tool (`tracedecay_affected`, `tracedecay_changelog`,
/// branch and PR context) onto its handler.
///
/// Tree walks and revwalks need a uniform dispatch deadline. Branch generation
/// reads additionally carry the caller's deadline into their bounded
/// blocking/ref and daemon-generation executors, so timing out this future
/// also tells the underlying operation to stop at its next checkpoint.
pub async fn dispatch_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    let carried_deadline = ctx.deadline();
    let remaining = carried_deadline.and_then(tracedecay_daemon_protocol::deadline_remaining);

    let handler = async {
        match tool_name {
            "tracedecay_affected" => {
                handle_affected(ctx, &open(read("file_dependents")?).await?, args).await
            }
            "tracedecay_diff_context" => {
                handle_diff_context(ctx, &open(read("file_dependents")?).await?, args).await
            }
            "tracedecay_changelog" => handle_changelog(ctx, args).await,
            "tracedecay_commit_context" => {
                handle_commit_context(ctx, &open(read("file_dependents")?).await?, args).await
            }
            "tracedecay_pr_context" => {
                handle_pr_context(ctx, open(read("file_dependents")?), args).await
            }
            "tracedecay_branch_search" => handle_branch_search(ctx, args).await,
            "tracedecay_branch_diff" => handle_branch_diff(ctx, args).await,
            "tracedecay_branch_list" => handle_branch_list(ctx, args).await,
            _ => Err(unknown_tool_error(tool_name)),
        }
    };

    match (carried_deadline.is_some(), remaining) {
        (_, Some(remaining)) => match tokio::time::timeout(remaining, handler).await {
            Ok(result) => result,
            Err(_elapsed) => Ok(git_dispatch_deadline_result(ctx, tool_name)),
        },
        // `deadline_remaining` yields `None` for a non-positive budget, so a
        // carried deadline that already elapsed must be rejected rather than
        // dispatched unbounded.
        (true, None) => Ok(git_dispatch_deadline_result(ctx, tool_name)),
        // Standalone / non-admission callers carry no deadline and stay
        // unbounded.
        (false, None) => handler.await,
    }
}
