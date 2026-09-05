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
    tool_accepts_registered_project_selector, tool_dispatches_registered_project_reader,
};
use super::support;
use super::support::registered_project_context;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render;

const RETRIEVE_PAGE_HEADER_ALLOWANCE: usize = 2_048;
const RETRIEVE_FRAME_RESERVED_BYTES: usize = 256;

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

/// The registered project a selector-bound retained route names, if any.
///
/// Selector-bound retained routes (`RegisteredProjectAccess::SelectorOnly`)
/// are never re-dispatched onto the selected project's server: the retained
/// owner opens that project's memory store read-only from the calling
/// session's own admitted runtime. The selector is therefore the only place
/// the served project appears at this boundary, and it is read before the
/// request body is normalized (normalization strips `project_selector`).
pub(super) fn selected_project_id_argument(args: &Value) -> Option<String> {
    args.get("project_selector")?
        .get("project_id")?
        .as_str()
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty())
        .map(str::to_owned)
}

/// What the result envelope's scope must say once a selector-bound retained
/// route has been served.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SelectedProjectScopeV1 {
    /// The selector named the calling session's own admitted project; the
    /// scope the daemon already resolved is the exact served scope.
    Unchanged,
    /// The selector named another registered project, whose exact scope this
    /// is. The envelope must report it instead of the admitted scope.
    Restated(Box<tracedecay_application::ResolvedScope>),
    /// The selected project could not be resolved to an exact registered
    /// scope. The route fails closed rather than reporting a result under a
    /// project the caller did not select.
    Refused,
}

/// Resolve the exact application scope a selector-bound retained read was
/// served from.
///
/// Every unresolved state — no project registry at this boundary, an
/// unregistered project id, a registry row whose identity does not match the
/// selector, or a root that no longer resolves to a valid scope — fails
/// closed. The surface never substitutes the calling session's scope for the
/// selected one.
pub(super) async fn selected_project_scope(
    selected_project_id: &str,
    served: &tracedecay_application::ResolvedScope,
    global_db: Option<&RegisteredGlobalDb>,
) -> SelectedProjectScopeV1 {
    if served.project_id.as_str() == selected_project_id {
        return SelectedProjectScopeV1::Unchanged;
    }
    let Some(database) = global_db else {
        return SelectedProjectScopeV1::Refused;
    };
    let Ok(Some(context)) = database
        .project_registry_context_by_id(selected_project_id)
        .await
    else {
        return SelectedProjectScopeV1::Refused;
    };
    if context.project.project_id != selected_project_id {
        return SelectedProjectScopeV1::Refused;
    }
    let requested_root = context.project.canonical_root.clone();
    match crate::mcp::scope::resolve_query_scope(&context, Path::new(&requested_root)) {
        Ok((_, scope)) if scope.project_id.as_str() == selected_project_id => {
            SelectedProjectScopeV1::Restated(Box::new(scope))
        }
        Ok(_) | Err(_) => SelectedProjectScopeV1::Refused,
    }
}

#[hotpath::measure(future = true, label = "mcp.project.route.resolve")]
pub(crate) async fn resolve_registered_project_route_for_tool(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectServerResolver>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    let semantic_top_level_fields =
        crate::mcp::project_route::semantic_route_argument_fields(&tool_name);
    if tool_accepts_registered_project_selector(&tool_name) {
        support::validate_registered_project_selector_aliases(&args, semantic_top_level_fields)?;
    }
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
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
    if let Some(field) = object.keys().find(|field| {
        !matches!(
            field.as_str(),
            "handle" | "format" | "project_selector" | "offset" | "max_chars"
        )
    }) {
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
    let offset = optional_usize_argument(args, "offset")?.unwrap_or(0);
    let requested_max_chars = optional_usize_argument(args, "max_chars")?
        .unwrap_or(tracedecay_mcp::MAX_RESPONSE_CHARS - RETRIEVE_PAGE_HEADER_ALLOWANCE);
    if requested_max_chars == 0 {
        return Err(TraceDecayError::project_route(
            "response_handle_invalid_page_size",
            false,
            "tracedecay_retrieve max_chars must be at least 1",
        ));
    }
    let max_chars = requested_max_chars
        .min(tracedecay_mcp::MAX_RESPONSE_CHARS - RETRIEVE_PAGE_HEADER_ALLOWANCE);
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
            let total_chars = record.original_chars();
            if offset > total_chars {
                return Err(TraceDecayError::project_route(
                    "response_handle_offset_out_of_range",
                    false,
                    format!(
                        "tracedecay_retrieve offset {offset} exceeds stored response length {total_chars}"
                    ),
                ));
            }
            let mut page_limit = max_chars;
            loop {
                let content = response_handle_page(&record.content, offset, page_limit);
                let page_chars = content.chars().count();
                let next = offset.saturating_add(page_chars);
                let has_more = next < total_chars;
                let next_offset = has_more.then_some(next);
                let text = if render::wants_json(args) {
                    json!({
                        "handle": record.handle,
                        "expired": false,
                        "original_chars": total_chars,
                        "total_chars": total_chars,
                        "offset": offset,
                        "next_offset": next_offset,
                        "has_more": has_more,
                        "created_at": record.created_at,
                        "expires_at": record.expires_at,
                        "content": content,
                    })
                    .to_string()
                } else {
                    format!(
                        "## Retrieved Response\n**handle:** `{}` ({} chars, expires at {})\n**offset:** {}\n**next_offset:** {}\n**has_more:** {}\n\n{}",
                        record.handle,
                        total_chars,
                        record.expires_at,
                        offset,
                        next_offset.map_or_else(|| "none".to_owned(), |value| value.to_string()),
                        has_more,
                        content,
                    )
                };
                let result = text_tool_result(&text);
                let frame = tracedecay_mcp::serialize_response_line(
                    &tracedecay_mcp::transport::JsonRpcResponse::success(
                        Value::Null,
                        result.value.clone(),
                    ),
                );
                let frame_budget =
                    tracedecay_mcp::MAX_RESPONSE_CHARS - RETRIEVE_FRAME_RESERVED_BYTES;
                if frame.len() <= frame_budget || page_limit == 1 || page_chars == 0 {
                    return Ok(result);
                }
                let scaled = page_limit.saturating_mul(frame_budget) / frame.len();
                page_limit = scaled.clamp(1, page_limit - 1);
            }
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

fn optional_usize_argument(args: &Value, field: &str) -> Result<Option<usize>> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    let value = value.as_u64().ok_or_else(|| TraceDecayError::Config {
        message: format!("{field} must be a non-negative integer"),
    })?;
    usize::try_from(value)
        .map(Some)
        .map_err(|_| TraceDecayError::Config {
            message: format!("{field} exceeds this platform's supported range"),
        })
}

fn response_handle_page(content: &str, offset: usize, max_chars: usize) -> String {
    if content.is_ascii() {
        let end = offset.saturating_add(max_chars).min(content.len());
        return content[offset..end].to_owned();
    }
    let start_byte = char_offset_to_byte(content, offset);
    let end_byte = char_offset_to_byte(&content[start_byte..], max_chars) + start_byte;
    content[start_byte..end_byte].to_owned()
}

fn char_offset_to_byte(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .nth(offset)
        .map_or(content.len(), |(index, _)| index)
}

#[cfg(test)]
mod selected_project_scope_tests {
    use serde_json::json;
    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

    use super::{SelectedProjectScopeV1, selected_project_id_argument, selected_project_scope};

    fn scope(project_id: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(project_id).expect("fixture project id"),
            RepositoryId::new("repository.selected-scope.fixture").expect("fixture repository id"),
            WorktreeId::new("worktree.selected-scope.fixture").expect("fixture worktree id"),
            None,
        )
        .expect("fixture scope is valid")
    }

    #[test]
    fn reads_only_a_non_empty_selector_project_id() {
        assert_eq!(
            selected_project_id_argument(&json!({
                "fact_id": "fact.alpha",
                "project_selector": {"project_id": " project.beta "},
            }))
            .as_deref(),
            Some("project.beta")
        );
        assert_eq!(selected_project_id_argument(&json!({})), None);
        assert_eq!(
            selected_project_id_argument(&json!({"project_selector": {"project_id": "   "}})),
            None
        );
        assert_eq!(
            selected_project_id_argument(&json!({"project_selector": {"project_id": 7}})),
            None
        );
    }

    #[tokio::test]
    async fn a_selector_naming_the_admitted_project_keeps_the_resolved_scope() {
        assert_eq!(
            selected_project_scope(
                "project.selected-scope.alpha",
                &scope("project.selected-scope.alpha"),
                None,
            )
            .await,
            SelectedProjectScopeV1::Unchanged
        );
    }

    /// Without a project registry at this boundary the served project cannot
    /// be named, so the route must fail closed instead of reporting the
    /// calling session's own scope for another project's fact.
    #[tokio::test]
    async fn a_foreign_selector_without_a_registry_fails_closed() {
        assert_eq!(
            selected_project_scope(
                "project.selected-scope.beta",
                &scope("project.selected-scope.alpha"),
                None,
            )
            .await,
            SelectedProjectScopeV1::Refused
        );
    }
}
