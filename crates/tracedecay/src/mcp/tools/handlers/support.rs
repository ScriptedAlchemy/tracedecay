//! Shared helpers for MCP tool handlers.
//!
//! Keep this module free of tool dispatch logic. Handler modules use it for
//! argument normalization, scope filtering, and registered-project selection.

use std::collections::HashSet;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{ProjectRegistryContext, RegisteredGlobalDb};
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render;

/// Builds a `Config` error from a message, for argument-validation failures.
pub(super) fn argument_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

pub(super) fn retrieval_cursor(args: &Value) -> Result<Option<tracedecay_domain::RetrievalCursor>> {
    let Some(encoded) = args.get("cursor").and_then(Value::as_str) else {
        return Ok(None);
    };
    if encoded.len() > 4_096 {
        return Err(argument_error(
            "cursor exceeds its bounded authenticated envelope",
        ));
    }
    let cursor: tracedecay_domain::RetrievalCursor = serde_json::from_str(encoded)?;
    cursor.validate().map_err(|_| {
        argument_error("cursor is not a valid authenticated retrieval continuation")
    })?;
    Ok(Some(cursor))
}

/// Key under which context handlers stash analytics that must reach the server
/// but never the client. [`rendered_tool_result`] is the one place it is lifted
/// back out, so no handler has to remember to strip it.
pub(super) const CONTEXT_MEMORY_ANALYTICS_KEY: &str = "context_memory_analytics";

/// The single wrapper every MCP tool handler returns through.
///
/// Lifts internal analytics out of `value` so they travel beside the result
/// instead of inside the client payload, renders the default-format (markdown)
/// body with `md`, and records `touched_files`. The `format:"json"` path is
/// unaffected — [`render::finalize`] serializes `value` compactly there.
pub(super) fn rendered_tool_result<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
    md: F,
) -> ToolResult {
    let internal_analytics = value.get(CONTEXT_MEMORY_ANALYTICS_KEY).cloned();
    let public_value = internal_analytics
        .as_ref()
        .and_then(|_| public_value_without_internal_context_memory_analytics(value));
    let value = public_value.as_ref().unwrap_or(value);
    let text = render::finalize(project_root, args, value, md);
    let result = text_tool_result(&text, touched_files);
    if let Some(internal_analytics) = internal_analytics {
        result.with_internal_analytics(internal_analytics)
    } else {
        result
    }
}

fn public_value_without_internal_context_memory_analytics(value: &Value) -> Option<Value> {
    let mut value = value.clone();
    take_internal_context_memory_analytics(&mut value).map(|_| value)
}

pub(super) fn take_internal_context_memory_analytics(value: &mut Value) -> Option<Value> {
    value.as_object_mut()?.remove(CONTEXT_MEMORY_ANALYTICS_KEY)
}

pub(super) fn text_tool_result(text: &str, touched_files: Vec<String>) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
}

/// [`rendered_tool_result`] for handlers that touch no files.
pub(super) fn tool_json_with_md<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    md: F,
) -> ToolResult {
    rendered_tool_result(project_root, args, value, Vec::new(), md)
}

/// [`rendered_tool_result`] for handlers that don't need a custom markdown
/// renderer — the default body is [`render::generic_md`] over the same value.
pub(super) fn generic_tool_result(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
) -> ToolResult {
    rendered_tool_result(project_root, args, value, touched_files, || {
        render::generic_md(value)
    })
}

/// [`generic_tool_result`] for handlers that touch no files.
pub(super) fn tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    generic_tool_result(project_root, args, value, Vec::new())
}

/// Rejects tool arguments that are not a JSON object.
///
/// The argument value comes straight off the wire (an MCP client, the
/// `tracedecay tool --args` CLI, or an internal dispatch probe), so a scalar
/// or array is caller error, not a broken invariant — asserting it would
/// panic the daemon's client task and the caller would see only a dropped
/// connection.
pub(crate) fn require_object_args(args: &Value, tool_name: &str) -> Result<()> {
    if args.is_object() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!("invalid arguments: {tool_name} expects a JSON object"),
    })
}

/// Decode one catalog-owned primitive request after removing keys owned by
/// the MCP transport rather than the application operation.
pub(crate) fn decode_primitive_request<T: DeserializeOwned>(
    args: &Value,
    tool_name: &str,
) -> Result<T> {
    require_object_args(args, tool_name)?;
    let mut request = args.clone();
    if let Some(object) = request.as_object_mut() {
        for key in ["format", "__mcp_request_id", "project_selector"] {
            object.remove(key);
        }
    }
    serde_json::from_value(request).map_err(|error| TraceDecayError::Config {
        message: format!("invalid arguments for {tool_name}: {error}"),
    })
}

/// Returns the user-provided `path` argument, falling back to the scope
/// prefix when the argument is absent. This makes listing tools
/// automatically scoped to the subdirectory the server was launched from.
pub(super) fn effective_path<'a>(
    args: &'a Value,
    scope_prefix: Option<&'a str>,
) -> Option<&'a str> {
    args.get("path").and_then(|v| v.as_str()).or(scope_prefix)
}

pub(super) fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for p in paths {
        if seen.insert(p) {
            result.push(p.to_string());
        }
    }
    result
}

fn invalid_registered_project_selector(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("project_route_invalid_selector", false, detail.into())
}

pub(super) async fn registered_project_context(
    args: &Value,
    semantic_top_level_fields: &[&str],
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<Option<ProjectRegistryContext>> {
    if let Some(alias) = ["project_id", "project_path", "project_root", "root"]
        .into_iter()
        .find(|key| !semantic_top_level_fields.contains(key) && args.get(*key).is_some())
    {
        return Err(invalid_registered_project_selector(format!(
            "top-level `{alias}` is not a registered-project selector; use project_selector.project_id"
        )));
    }
    let Some(selector_value) = args.get("project_selector") else {
        return Ok(None);
    };
    let selector = selector_value
        .as_object()
        .ok_or_else(|| invalid_registered_project_selector("project_selector must be an object"))?;
    if selector.len() != 1 || !selector.contains_key("project_id") {
        return Err(invalid_registered_project_selector(
            "project_selector accepts only project_id",
        ));
    }
    let project_id = selector
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            invalid_registered_project_selector(
                "project_selector.project_id must be a non-empty string",
            )
        })?;
    let db = global_db.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_not_authorized",
            false,
            "client project registry is unavailable for selector resolution",
        )
    })?;
    db.project_registry_context_by_id(project_id)
        .await?
        .map(Some)
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "project_route_not_found",
                false,
                format!(
                    "registered project not found for project_selector.project_id={project_id}; run tracedecay_project_search"
                ),
            )
        })
}

/// Whether a selector names a path rather than a bare project name. This is
/// pure syntax: it decides whether a selector may fall back to Git identity,
/// and never consults the registry. Delegates to the canonical
/// [`RegisteredGlobalDb::is_explicit_project_path_selector`].
pub(super) fn is_explicit_project_path_selector(selector: &str) -> bool {
    RegisteredGlobalDb::is_explicit_project_path_selector(selector)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use tracedecay_mcp::tools::render;

    use super::{
        CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request, generic_tool_result,
        is_explicit_project_path_selector, rendered_tool_result,
    };
    use tracedecay_application::retrieval::NodeSurfaceRequestV1;

    /// `generic_tool_result` must stay a pure spelling of the closure form it
    /// replaced at every call site — same bytes on both output formats, and the
    /// same internal-analytics lifting.
    #[test]
    fn generic_tool_result_matches_the_explicit_generic_md_closure() {
        let mut value = json!({
            "count": 2,
            "items": [{"name": "alpha", "file": "src/a.rs"}, {"name": "beta", "file": "src/b.rs"}],
        });
        // Exercise the internal-analytics lifting branch too.
        value[CONTEXT_MEMORY_ANALYTICS_KEY] = json!({"matches": 1});
        let touched = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];

        for args in [
            json!({}),
            json!({"format": "markdown"}),
            json!({"format": "json"}),
        ] {
            let expected = rendered_tool_result(None, &args, &value, touched.clone(), || {
                render::generic_md(&value)
            });
            let actual = generic_tool_result(None, &args, &value, touched.clone());

            assert_eq!(actual.value, expected.value, "payload differs for {args}");
            assert_eq!(
                actual.touched_files, expected.touched_files,
                "touched files differ for {args}"
            );
            assert_eq!(
                actual.internal_analytics(),
                expected.internal_analytics(),
                "internal analytics differ for {args}"
            );
        }
    }

    /// Handlers that used to build their own text envelope — `render::finalize`
    /// then a hand-written `{"content":[{"type":"text",...}]}` — now go through
    /// `rendered_tool_result`. That is the same envelope for any payload without
    /// the internal-analytics key, which is every payload those handlers build.
    #[test]
    fn rendered_tool_result_matches_a_hand_built_text_envelope() {
        let value = json!({"passed": 0, "failed": 1, "results": [], "note": "nothing ran"});
        let touched = vec!["src/a.rs".to_string()];

        for args in [
            json!({}),
            json!({"format": "markdown"}),
            json!({"format": "json"}),
        ] {
            let text = render::finalize(None, &args, &value, || render::generic_md(&value));
            let expected = super::text_tool_result(&text, touched.clone());
            let actual = generic_tool_result(None, &args, &value, touched.clone());

            assert_eq!(actual.value, expected.value, "payload differs for {args}");
            assert_eq!(
                actual.touched_files, expected.touched_files,
                "touched files differ for {args}"
            );
            assert!(actual.internal_analytics().is_none(), "for {args}");
        }
    }

    #[test]
    fn primitive_request_decode_strips_transport_keys_and_rejects_legacy_aliases() {
        let decoded = decode_primitive_request::<NodeSurfaceRequestV1>(
            &json!({
                "node_id": "function:canonical",
                "format": "json",
                "project_selector": {"project_id": "project.fixture"},
                "__mcp_request_id": "request.fixture",
            }),
            "tracedecay_node",
        )
        .expect("transport keys must not enter the canonical request body");
        assert_eq!(decoded.node_id, "function:canonical");

        let error = decode_primitive_request::<NodeSurfaceRequestV1>(
            &json!({"node_id": "function:canonical", "project_id": "project.legacy"}),
            "tracedecay_node",
        )
        .expect_err("the top-level project id alias is not a transport key");
        assert!(error.to_string().contains("unknown field `project_id`"));

        let error = decode_primitive_request::<NodeSurfaceRequestV1>(
            &json!({"id": "function:legacy"}),
            "tracedecay_node",
        )
        .expect_err("the unreleased id alias is not part of the canonical request");
        assert!(error.to_string().contains("unknown field `id`"));
    }

    #[test]
    fn explicit_project_path_detection_is_syntax_only() {
        assert!(is_explicit_project_path_selector("/workspace/project"));
        assert!(is_explicit_project_path_selector("team/project"));
        assert!(is_explicit_project_path_selector("."));
        assert!(!is_explicit_project_path_selector("project"));
    }
}
