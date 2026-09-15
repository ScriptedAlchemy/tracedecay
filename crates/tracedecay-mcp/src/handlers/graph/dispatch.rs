//! Name table for the code-graph navigation and lookup family.

use serde_json::Value;
use tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use tracedecay_domain::errors::Result;

use super::{
    handle_by_qualified_name, handle_callees, handle_callers, handle_callers_for, handle_context,
    handle_derives, handle_find_exact_symbol, handle_impact, handle_implementations, handle_impls,
    handle_node, handle_rename_preview, handle_search, handle_signature, handle_similar,
};
use crate::ToolResult;
use crate::handlers::ast_grep::handle_ast_grep_search;
use crate::handlers::grep::handle_grep;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tool_context::McpToolContext;

/// Dispatches one graph-family tool (`tracedecay_search`,
/// `tracedecay_callers`, ...) onto its handler, opening the verified graph
/// through `open` under the operation the catalog registers for it.
pub async fn dispatch_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<&dyn CodeIndexIgnoredDependencyAdmissionPortV1>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_search" => {
            handle_search(
                ctx,
                open(read("code_symbol_search")?),
                args,
                scope_prefix,
                ignored_dependency_admission,
            )
            .await
        }
        "tracedecay_grep" => {
            // Grep degrades to a lexical answer when the graph is unavailable,
            // so the open outcome travels to the handler instead of failing here.
            let graph = match read("source_lines") {
                Ok(operation) => open(operation).await,
                Err(error) => Err(error),
            };
            handle_grep(
                ctx.project_root(),
                graph.as_ref(),
                args,
                scope_prefix,
                ctx.deadline().cloned(),
                ctx.cancellation().cloned(),
            )
            .await
        }
        "tracedecay_ast_grep_search" => {
            handle_ast_grep_search(
                ctx.project_root(),
                args,
                scope_prefix,
                ctx.deadline().cloned(),
                ctx.cancellation().cloned(),
            )
            .await
        }
        "tracedecay_context" => {
            handle_context(ctx, open(read("context")?), args, scope_prefix).await
        }
        "tracedecay_callers" => handle_callers(&open(read("code_callers")?).await?, args).await,
        "tracedecay_callees" => handle_callees(&open(read("callees")?).await?, args).await,
        "tracedecay_impact" => handle_impact(&open(read("impact")?).await?, args).await,
        "tracedecay_node" => handle_node(&open(read("node")?).await?, args).await,
        "tracedecay_similar" => handle_similar(ctx, &open(read("similar")?).await?, args).await,
        "tracedecay_rename_preview" => {
            handle_rename_preview(ctx, &open(read("rename_preview")?).await?, args).await
        }
        "tracedecay_implementations" => {
            handle_implementations(
                &open(read("code_implementations")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_callers_for" => {
            handle_callers_for(&open(read("code_callers")?).await?, args).await
        }
        "tracedecay_find_exact_symbol" => {
            handle_find_exact_symbol(
                ctx,
                &open(read("qualified_name")?).await?,
                args,
                scope_prefix,
                ignored_dependency_admission,
            )
            .await
        }
        "tracedecay_by_qualified_name" => {
            handle_by_qualified_name(&open(read("qualified_name")?).await?, args).await
        }
        "tracedecay_signature" => {
            handle_signature(&open(read("code_signature_search")?).await?, args).await
        }
        "tracedecay_impls" => handle_impls(&open(read("code_implementations")?).await?, args).await,
        "tracedecay_derives" => {
            handle_derives(&open(read("code_type_hierarchy")?).await?, args).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
