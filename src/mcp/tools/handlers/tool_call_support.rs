use std::path::Path;

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;

use super::super::ToolResult;
use super::super::binding::tool_dispatches_registered_project_reader;
use super::super::render;
use super::support;
use super::support::{project_registry_context, project_selector_present};

pub(in crate::mcp::tools) fn text_tool_result(text: &str) -> ToolResult {
    support::text_tool_result(text, Vec::new())
}

pub(in crate::mcp::tools) fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&serde_json::to_string(value).unwrap_or_default())
}

pub(super) fn boxed_send<'a, T, F>(
    future: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(future)
}

pub(crate) const INTERNAL_DAEMON_TOOL_NAMES: &[&str] = &[
    "tracedecay_admin_branch_add",
    "tracedecay_admin_cli",
    "tracedecay_admin_project",
    "tracedecay_admin_sync",
    "tracedecay_hook_runtime",
];

pub(super) fn rejected_tool_project_selector_present(tool_name: &str, args: &Value) -> bool {
    let top_level_path_keys = if tool_name.starts_with("tracedecay_lcm_") {
        &["project_path"][..]
    } else {
        &["project_path", "project_root"][..]
    };
    project_selector_present(args, top_level_path_keys)
}

pub(crate) async fn selected_registered_project_reader(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
    let context = boxed_send(project_registry_context(
        &args,
        &["project_path", "project_root"],
        global_db,
    ));
    let Some(context) = context.await.map_err(|error| {
        crate::mcp::project_route::ProjectRouteFailure::from_selection_error(&error).into_error()
    })?
    else {
        return Ok(None);
    };

    let Some(resolver) = resolver else {
        return Err(TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "registered project graph resolver is unavailable",
        ));
    };
    let requested_path = args
        .get("project_selector")
        .and_then(Value::as_object)
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| args.get("project_path"))
        .or_else(|| args.get("project_root"))
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(|path| {
            crate::worktree::git_worktree_root(path).or_else(|| path.canonicalize().ok())
        })
        .unwrap_or_else(|| Path::new(&context.project.canonical_root).to_path_buf());
    let request = crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
        context.clone(),
        requested_path.clone(),
    );
    let graph = resolver(request.clone()).await?.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            format!(
                "registered project '{}' is not mounted for workspace {}",
                context.project.project_id,
                requested_path.display()
            ),
        )
    })?;
    let scope = crate::mcp::scope::resolve_query_scope(&context, &requested_path)
        .map_err(|error| error.into_route_failure().into_error())?;
    Ok(Some(crate::mcp::project_route::ResolvedProjectRoute {
        graph,
        owner: context,
        requested_root: requested_path,
        requested_git_common_dir: request.requested_git_common_dir,
        requested_branch: request.requested_branch,
        scope,
    }))
}

pub(super) fn handle_retrieve(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "missing required parameter: handle (copy the exact `handle` value from a truncated MCP response envelope)"
                        .to_string(),
            })?;
    let payload = match retrieve_response_handle(cg.project_root(), handle, current_timestamp())? {
        ResponseHandleLookup::Found(record) => {
            // Retrieval never truncates: the stored content is by definition
            // larger than the response cap, so neither output path may route
            // through the truncating envelope again. Markdown (default)
            // returns the stored text verbatim under a small header; JSON
            // serializes the payload directly.
            let text = if render::wants_json(args) {
                serde_json::to_string(&json!({
                    "handle": record.handle,
                    "expired": false,
                    "original_chars": record.original_chars(),
                    "created_at": record.created_at,
                    "expires_at": record.expires_at,
                    "content": record.content,
                }))
                .unwrap_or_default()
            } else {
                format!(
                    "## Retrieved Response\n**handle:** `{}` ({} chars, expires at {})\n\n{}",
                    record.handle,
                    record.original_chars(),
                    record.expires_at,
                    record.content,
                )
            };
            return Ok(text_tool_result(&text));
        }
        ResponseHandleLookup::Missing => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_not_found",
            "message": "Response handle was not found in this project's local cache.",
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
        }),
        ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        } => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_expired",
            "message": format!(
                "Response handle expired at {expires_at} and was removed from this project's local cache."
            ),
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
            "created_at": created_at,
            "expires_at": expires_at,
        }),
    };
    Ok(support::tool_json(Some(cg.project_root()), args, &payload))
}
