//! Portable handler result and argument helpers used by graph-backed tools.

use std::collections::HashSet;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::ToolResult;
use crate::tools::render;

/// Key under which context handlers stash analytics that must reach the server
/// but never the client. [`rendered_tool_result`] is the one place it is lifted
/// back out, so no handler has to remember to strip it.
pub const CONTEXT_MEMORY_ANALYTICS_KEY: &str = "context_memory_analytics";

/// The single wrapper every MCP tool handler returns through.
///
/// Lifts internal analytics out of `value` so they travel beside the result
/// instead of inside the client payload, renders the default-format (markdown)
/// body with `md`, and records `touched_files`. The `format:"json"` path is
/// unaffected — [`render::finalize`] serializes `value` compactly there.
pub fn rendered_tool_result<F: FnOnce() -> String>(
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

pub fn take_internal_context_memory_analytics(value: &mut Value) -> Option<Value> {
    value.as_object_mut()?.remove(CONTEXT_MEMORY_ANALYTICS_KEY)
}

pub fn text_tool_result(text: &str, touched_files: Vec<String>) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
}

/// [`rendered_tool_result`] for handlers that touch no files.
pub fn tool_json_with_md<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    md: F,
) -> ToolResult {
    rendered_tool_result(project_root, args, value, Vec::new(), md)
}

/// [`rendered_tool_result`] for handlers that don't need a custom markdown
/// renderer — the default body is [`render::generic_md`] over the same value.
pub fn generic_tool_result(
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
pub fn tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    generic_tool_result(project_root, args, value, Vec::new())
}

/// Rejects tool arguments that are not a JSON object.
pub fn require_object_args(args: &Value, tool_name: &str) -> Result<()> {
    if args.is_object() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!("invalid arguments: {tool_name} expects a JSON object"),
    })
}

/// Decode one catalog-owned primitive request after removing keys owned by
/// the MCP transport rather than the application operation.
pub fn decode_primitive_request<T: DeserializeOwned>(args: &Value, tool_name: &str) -> Result<T> {
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

/// Rejects a zero result limit with a typed error.
pub fn require_positive_limit(limit: usize, tool_name: &str) -> Result<()> {
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: format!("invalid parameter: {tool_name} requires limit to be at least 1"),
        });
    }
    Ok(())
}

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias.
pub fn require_node_id(args: &Value) -> Result<&str> {
    let node_id = args
        .get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: node_id".to_string(),
        })?;
    if node_id.trim().is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: node_id must not be empty".to_string(),
        });
    }
    Ok(node_id)
}

/// Returns the user-provided `path` argument, falling back to the scope
/// prefix when the argument is absent.
pub fn effective_path<'a>(args: &'a Value, scope_prefix: Option<&'a str>) -> Option<&'a str> {
    args.get("path").and_then(|v| v.as_str()).or(scope_prefix)
}

pub fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        if seen.insert(path) {
            result.push(path.to_string());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_application::retrieval::NodeSurfaceRequestV1;

    use super::{
        CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request, generic_tool_result,
        rendered_tool_result, require_node_id, require_positive_limit, unique_file_paths,
    };
    use crate::tools::render;

    #[test]
    fn generic_result_preserves_rendering_analytics_and_touched_files() {
        let mut value = json!({
            "count": 2,
            "items": [
                {"name": "alpha", "file": "src/a.rs"},
                {"name": "beta", "file": "src/b.rs"}
            ],
        });
        value[CONTEXT_MEMORY_ANALYTICS_KEY] = json!({"matches": 1});
        let touched = vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()];

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
                "analytics differ for {args}"
            );
        }
    }

    #[test]
    fn rendered_result_matches_the_canonical_text_envelope() {
        let value = json!({"passed": 0, "failed": 1, "results": []});
        let touched = vec!["src/a.rs".to_owned()];

        for args in [
            json!({}),
            json!({"format": "markdown"}),
            json!({"format": "json"}),
        ] {
            let text = render::finalize(None, &args, &value, || render::generic_md(&value));
            let expected = super::text_tool_result(&text, touched.clone());
            let actual = generic_tool_result(None, &args, &value, touched.clone());

            assert_eq!(actual.value, expected.value, "payload differs for {args}");
            assert_eq!(actual.touched_files, expected.touched_files);
            assert!(actual.internal_analytics().is_none());
        }
    }

    #[test]
    fn node_id_validation_accepts_alias_and_rejects_missing_or_blank_values() {
        assert!(matches!(
            require_node_id(&json!({"node_id": "fn:canonical", "id": "fn:alias"})),
            Ok("fn:canonical")
        ));
        assert!(matches!(
            require_node_id(&json!({"id": "trait:alias"})),
            Ok("trait:alias")
        ));
        assert!(require_node_id(&json!({"query": "missing"})).is_err());

        for args in [
            json!({"node_id": ""}),
            json!({"node_id": "   "}),
            json!({"node_id": "\t\n"}),
            json!({"id": ""}),
        ] {
            let error = require_node_id(&args).expect_err("blank node id must fail");
            assert!(
                error.to_string().contains("node_id must not be empty"),
                "unexpected error for {args}: {error}"
            );
        }
    }

    #[test]
    fn primitive_decode_strips_transport_keys_and_rejects_legacy_aliases() {
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

        for invalid in [
            json!({"node_id": "function:canonical", "project_id": "project.legacy"}),
            json!({"id": "function:legacy"}),
        ] {
            assert!(
                decode_primitive_request::<NodeSurfaceRequestV1>(&invalid, "tracedecay_node")
                    .is_err(),
                "legacy request must fail: {invalid}"
            );
        }
    }

    #[test]
    fn positive_limit_and_unique_paths_keep_validation_parity() {
        assert!(require_positive_limit(1, "tracedecay_fixture").is_ok());
        let error =
            require_positive_limit(0, "tracedecay_fixture").expect_err("zero limit must fail");
        assert!(error.to_string().contains("limit to be at least 1"));

        assert_eq!(
            unique_file_paths(["src/a.rs", "src/b.rs", "src/a.rs"].into_iter()),
            vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()]
        );
    }
}
