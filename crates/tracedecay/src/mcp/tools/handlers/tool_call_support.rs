use std::path::Path;

use serde_json::{Value, json};

use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_mcp::response_handles::{
    ResponseHandleLookup, public_retrieve_error, retrieve_response_handle,
};

use super::super::binding::{
    tool_accepts_registered_project_selector, tool_is_selector_bound_effect,
};
use super::support;
use super::support::registered_project_context;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render;

pub(in crate::mcp::tools) fn text_tool_result(text: &str) -> ToolResult {
    support::text_tool_result(text, Vec::new())
}

pub(in crate::mcp::tools) fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&value.to_string())
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

pub(super) fn rejected_tool_project_selector_present(_tool_name: &str, args: &Value) -> bool {
    args.get("project_selector").is_some()
}

#[hotpath::measure(future = true, label = "mcp.project.route.resolve")]
pub(crate) async fn resolve_registered_project_route_for_tool(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectServerResolver>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if !tool_accepts_registered_project_selector(&tool_name)
        || tool_is_selector_bound_effect(&tool_name)
    {
        return Ok(None);
    }
    let semantic_top_level_fields =
        crate::mcp::project_route::semantic_route_argument_fields(&tool_name);
    let context = boxed_send(registered_project_context(
        &args,
        semantic_top_level_fields,
        global_db,
    ));
    let Some(context) = context.await? else {
        return Ok(None);
    };

    let database = global_db.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_not_authorized",
            false,
            "registered project route has no authenticated profile authority",
        )
    })?;
    let requested_path = context.project.canonical_root.clone();
    crate::mcp::project_route::resolve_registered_project_route(
        context,
        Path::new(&requested_path),
        database,
        resolver,
    )
    .await
    .map(Some)
}

#[hotpath::measure(future = true, label = "mcp.retrieve.handle.total")]
pub(super) async fn handle_retrieve(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let object = args.as_object().ok_or_else(|| TraceDecayError::Config {
        message: "tracedecay_retrieve arguments must be an object".to_string(),
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !matches!(field.as_str(), "handle" | "format" | "project_selector"))
    {
        return Err(TraceDecayError::Config {
            message: format!("unknown tracedecay_retrieve argument `{field}`"),
        });
    }
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "missing required parameter: handle (copy the exact `handle` value from a truncated MCP response envelope)"
                        .to_string(),
            })?;
    // The stored payload is by definition larger than the response cap, so
    // loading it back is real disk I/O that must not run inline on the async
    // dispatch worker.
    let lookup = {
        let project_root = cg.project_root().to_path_buf();
        let handle = handle.to_string();
        hotpath::future!(
            tokio::task::spawn_blocking(move || {
                retrieve_response_handle(&project_root, &handle, current_timestamp())
            }),
            label = "mcp.retrieve.handle.load"
        )
        .await
        .map_err(|join_error| TraceDecayError::Config {
            message: format!("response handle retrieval task failed: {join_error}"),
        })?
        .map_err(public_retrieve_error)?
    };
    let payload = match lookup {
        ResponseHandleLookup::Found(record) => {
            // Retrieval never truncates: the stored content is by definition
            // larger than the response cap, so neither output path may route
            // through the truncating envelope again. Markdown (default)
            // returns the stored text verbatim under a small header; JSON
            // serializes the payload directly.
            let text = if render::wants_json(args) {
                json!({
                    "handle": record.handle,
                    "expired": false,
                    "original_chars": record.original_chars(),
                    "created_at": record.created_at,
                    "expires_at": record.expires_at,
                    "content": record.content,
                })
                .to_string()
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
            "expired": null,
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
