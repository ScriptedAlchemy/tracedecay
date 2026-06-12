//! MCP tool call handlers.
//!
//! Each `handle_*` function implements one MCP tool: it deserializes
//! the JSON arguments, calls the appropriate `TokenSave` method, and
//! formats the result.

pub mod analysis;
pub mod dashboard;
pub mod edit;
pub mod git;
pub mod graph;
pub mod health;
pub mod info;
pub mod memory;
pub mod redundancy;
pub mod session;
pub mod workflow;

use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Value};

use crate::errors::{Result, TokenSaveError};
use crate::mcp::response_handles::{
    retrieve_response_handle, store_response_handle, RESPONSE_HANDLE_TTL_SECS,
    RESPONSE_RETRIEVE_TOOL,
};
use crate::tokensave::current_timestamp;
use crate::tokensave::TokenSave;

use super::{ToolResult, MAX_RESPONSE_CHARS};

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias. LLMs occasionally shorten `node_id` to `id`; this avoids a
/// confusing error when that happens.
pub(crate) fn require_node_id(args: &Value) -> Result<&str> {
    args.get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TokenSaveError::Config {
            message: "missing required parameter: node_id".to_string(),
        })
}

/// Returns the user-provided `path` argument, falling back to the scope
/// prefix when the argument is absent. This makes listing tools
/// automatically scoped to the subdirectory the server was launched from.
pub(crate) fn effective_path<'a>(
    args: &'a Value,
    scope_prefix: Option<&'a str>,
) -> Option<&'a str> {
    args.get("path").and_then(|v| v.as_str()).or(scope_prefix)
}

/// Filters a Vec of items by file path prefix when a scope is active.
/// Returns the vec unchanged when `scope_prefix` is `None`.
pub(crate) fn filter_by_scope<T, F>(
    items: Vec<T>,
    scope_prefix: Option<&str>,
    get_path: F,
) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    match scope_prefix {
        Some(prefix) => {
            let with_slash = if prefix.ends_with('/') {
                prefix.to_string()
            } else {
                format!("{prefix}/")
            };
            items
                .into_iter()
                .filter(|item| {
                    let p = get_path(item);
                    p.starts_with(&with_slash) || p == prefix
                })
                .collect()
        }
        None => items,
    }
}

/// Deduplicates an iterator of file path strings into a `Vec<String>`.
pub(crate) fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for p in paths {
        if seen.insert(p) {
            result.push(p.to_string());
        }
    }
    result
}

/// Truncates a string to the maximum response character limit, appending
/// a truncation notice if necessary.
pub(crate) fn truncate_response(s: &str) -> String {
    debug_assert!(!s.is_empty(), "truncate_response called with empty string");
    if s.len() <= MAX_RESPONSE_CHARS {
        s.to_string()
    } else {
        // Find a valid UTF-8 character boundary at or before MAX_RESPONSE_CHARS
        let mut end = MAX_RESPONSE_CHARS;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}\n\n[... truncated at {} chars]", &s[..end], end)
    }
}

/// Wraps a JSON string that exceeds `MAX_RESPONSE_CHARS` in a
/// `{"truncated": true, "preview": "..."}` envelope so the result is
/// always valid JSON, never a mid-structure cut.
///
/// Prefer this over [`truncate_response`] for handlers whose payload is
/// always JSON (e.g. the dashboard handler) to avoid breaking consumers
/// that parse the entire response text.
/// Wraps oversized JSON text in a valid preview envelope. When a project root
/// is available, stores the full original locally and includes a retrieval
/// handle.
pub(crate) fn truncated_json_envelope_with_handle(
    project_root: Option<&Path>,
    formatted: &str,
) -> String {
    if formatted.len() <= MAX_RESPONSE_CHARS {
        return formatted.to_string();
    }
    let stored = project_root
        .and_then(|root| store_response_handle(root, formatted, current_timestamp()).ok());
    let mut end = formatted.len().min(MAX_RESPONSE_CHARS.saturating_sub(1024));
    loop {
        while end > 0 && !formatted.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &formatted[..end];
        let mut envelope = json!({
            "truncated": true,
            "original_chars": formatted.len(),
            "preview_chars": preview.len(),
            "preview": preview,
        });
        if let (Some(record), Some(object)) = (&stored, envelope.as_object_mut()) {
            object.insert("handle".to_string(), json!(record.handle));
            object.insert("retrieve_tool".to_string(), json!(RESPONSE_RETRIEVE_TOOL));
            object.insert(
                "retrieve_ttl_seconds".to_string(),
                json!(RESPONSE_HANDLE_TTL_SECS),
            );
            object.insert("retrieve_expires_at".to_string(), json!(record.expires_at));
            object.insert(
                "retrieve_instruction".to_string(),
                json!(format!(
                    "Call {RESPONSE_RETRIEVE_TOOL} with handle '{}' to retrieve the full original response from local project storage.",
                    record.handle
                )),
            );
        }
        let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
        if text.len() <= MAX_RESPONSE_CHARS || end == 0 {
            return text;
        }
        end = end.saturating_sub(1024);
    }
}

fn handle_retrieve(cg: &TokenSave, args: &Value) -> Result<ToolResult> {
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TokenSaveError::Config {
                message: "missing required parameter: handle".to_string(),
            })?;
    let payload = match retrieve_response_handle(cg.project_root(), handle, current_timestamp())? {
        Some(record) => json!({
            "handle": record.handle,
            "expired": false,
            "original_chars": record.original_chars(),
            "created_at": record.created_at,
            "expires_at": record.expires_at,
            "content": record.content,
        }),
        None => json!({
            "handle": handle,
            "expired": true,
            "content": null,
        }),
    };
    let formatted = serde_json::to_string_pretty(&payload).unwrap_or_default();
    Ok(ToolResult {
        value: json!({ "content": [{ "type": "text", "text": formatted }] }),
        touched_files: Vec::new(),
    })
}

/// Dispatches a tool call to the appropriate handler.
///
/// Returns the tool result and touched file paths, or an error if the tool
/// name is unknown or the handler fails. The optional `server_stats` value
/// is included in `tokensave_status` responses when provided.
pub async fn handle_tool_call(
    cg: &TokenSave,
    tool_name: &str,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    debug_assert!(
        !tool_name.is_empty(),
        "handle_tool_call called with empty tool_name"
    );
    debug_assert!(
        tool_name.starts_with("tokensave_"),
        "tool_name must start with 'tokensave_' prefix"
    );
    match tool_name {
        "tokensave_search" => graph::handle_search(cg, args, scope_prefix).await,
        "tokensave_retrieve" => handle_retrieve(cg, &args),
        "tokensave_context" => graph::handle_context(cg, args, scope_prefix).await,
        "tokensave_callers" => graph::handle_callers(cg, args).await,
        "tokensave_callees" => graph::handle_callees(cg, args).await,
        "tokensave_impact" => graph::handle_impact(cg, args).await,
        "tokensave_node" => graph::handle_node(cg, args).await,
        "tokensave_status" => info::handle_status(cg, server_stats, scope_prefix).await,
        "tokensave_files" => info::handle_files(cg, args, scope_prefix).await,
        "tokensave_affected" => git::handle_affected(cg, args).await,
        "tokensave_dead_code" => analysis::handle_dead_code(cg, args, scope_prefix).await,
        "tokensave_diff_context" => git::handle_diff_context(cg, args).await,
        "tokensave_module_api" => analysis::handle_module_api(cg, args, scope_prefix).await,
        "tokensave_circular" => analysis::handle_circular(cg, args).await,
        "tokensave_hotspots" => analysis::handle_hotspots(cg, args, scope_prefix).await,
        "tokensave_similar" => graph::handle_similar(cg, args).await,
        "tokensave_rename_preview" => graph::handle_rename_preview(cg, args).await,
        "tokensave_unused_imports" => analysis::handle_unused_imports(cg, args, scope_prefix).await,
        "tokensave_rank" => analysis::handle_rank(cg, args, scope_prefix).await,
        "tokensave_largest" => analysis::handle_largest(cg, args, scope_prefix).await,
        "tokensave_coupling" => analysis::handle_coupling(cg, args, scope_prefix).await,
        "tokensave_inheritance_depth" => {
            analysis::handle_inheritance_depth(cg, args, scope_prefix).await
        }
        "tokensave_distribution" => analysis::handle_distribution(cg, args, scope_prefix).await,
        "tokensave_recursion" => analysis::handle_recursion(cg, args, scope_prefix).await,
        "tokensave_complexity" => analysis::handle_complexity(cg, args, scope_prefix).await,
        "tokensave_doc_coverage" => analysis::handle_doc_coverage(cg, args, scope_prefix).await,
        "tokensave_god_class" => analysis::handle_god_class(cg, args, scope_prefix).await,
        "tokensave_changelog" => git::handle_changelog(cg, args).await,
        "tokensave_port_status" => info::handle_port_status(cg, args).await,
        "tokensave_port_order" => info::handle_port_order(cg, args).await,
        "tokensave_commit_context" => git::handle_commit_context(cg, args).await,
        "tokensave_pr_context" => git::handle_pr_context(cg, args).await,
        "tokensave_simplify_scan" => info::handle_simplify_scan(cg, args, scope_prefix).await,
        "tokensave_test_map" => health::handle_test_map(cg, args, scope_prefix).await,
        "tokensave_type_hierarchy" => info::handle_type_hierarchy(cg, args).await,
        "tokensave_branch_search" => git::handle_branch_search(cg, args).await,
        "tokensave_branch_diff" => git::handle_branch_diff(cg, args).await,
        "tokensave_branch_list" => Ok(git::handle_branch_list(cg)),
        "tokensave_str_replace" => edit::handle_str_replace(cg, args).await,
        "tokensave_multi_str_replace" => edit::handle_multi_str_replace(cg, args).await,
        "tokensave_insert_at" => edit::handle_insert_at(cg, args).await,
        "tokensave_ast_grep_rewrite" => edit::handle_ast_grep_rewrite(cg, args).await,
        "tokensave_gini" => health::handle_gini(cg, args, scope_prefix).await,
        "tokensave_dependency_depth" => {
            health::handle_dependency_depth(cg, args, scope_prefix).await
        }
        "tokensave_health" => health::handle_health(cg, args, scope_prefix).await,
        "tokensave_redundancy" => redundancy::handle_redundancy(cg, args, scope_prefix).await,
        "tokensave_runtime" => health::handle_runtime(cg, args).await,
        "tokensave_dsm" => health::handle_dsm(cg, args, scope_prefix).await,
        "tokensave_test_risk" => health::handle_test_risk(cg, args, scope_prefix).await,
        "tokensave_session_start" => health::handle_session_start(cg, args, scope_prefix).await,
        "tokensave_session_end" => health::handle_session_end(cg, args, scope_prefix).await,
        "tokensave_body" => info::handle_body(cg, args, scope_prefix).await,
        "tokensave_todos" => info::handle_todos(cg, args, scope_prefix).await,
        "tokensave_read" => info::handle_read(cg, args).await,
        "tokensave_outline" => info::handle_outline(cg, args).await,
        "tokensave_config" => info::handle_config(cg, &args),
        "tokensave_signature_search" => info::handle_signature_search(cg, args, scope_prefix).await,
        "tokensave_implementations" => graph::handle_implementations(cg, args, scope_prefix).await,
        "tokensave_unsafe_patterns" => {
            analysis::handle_unsafe_patterns(cg, args, scope_prefix).await
        }
        "tokensave_diagnostics" => analysis::handle_diagnostics(cg, args).await,
        "tokensave_constructors" => analysis::handle_constructors(cg, args, scope_prefix).await,
        "tokensave_field_sites" => analysis::handle_field_sites(cg, args, scope_prefix).await,
        "tokensave_callers_for" => graph::handle_callers_for(cg, args).await,
        "tokensave_call_chain" => graph::handle_call_chain(cg, args).await,
        "tokensave_file_dependents" => graph::handle_file_dependents(cg, args).await,
        "tokensave_replace_symbol" => edit::handle_replace_symbol(cg, args).await,
        "tokensave_insert_at_symbol" => edit::handle_insert_at_symbol(cg, args).await,
        "tokensave_find_exact_symbol" => {
            graph::handle_find_exact_symbol(cg, args, scope_prefix).await
        }
        "tokensave_by_qualified_name" => graph::handle_by_qualified_name(cg, args).await,
        "tokensave_signature" => graph::handle_signature(cg, args).await,
        "tokensave_impls" => graph::handle_impls(cg, args).await,
        "tokensave_diagnose" => workflow::handle_diagnose(cg, args).await,
        "tokensave_run_affected_tests" => workflow::handle_run_affected_tests(cg, args).await,
        "tokensave_derives" => graph::handle_derives(cg, args).await,
        "tokensave_fact_store" => memory::handle_fact_store(cg, args).await,
        "tokensave_fact_feedback" => memory::handle_fact_feedback(cg, args).await,
        "tokensave_memory_status" => memory::handle_memory_status(cg).await,
        "tokensave_dashboard" => dashboard::handle_dashboard(cg, args).await,
        "tokensave_message_search" => session::handle_message_search(cg, args).await,
        "tokensave_lcm_status" => session::handle_lcm_status(Some(cg.project_root()), args).await,
        "tokensave_lcm_doctor" => session::handle_lcm_doctor(Some(cg.project_root()), args).await,
        "tokensave_lcm_load_session" => {
            session::handle_lcm_load_session(Some(cg.project_root()), args).await
        }
        "tokensave_lcm_grep" => session::handle_lcm_grep(Some(cg.project_root()), args).await,
        "tokensave_lcm_describe" => {
            session::handle_lcm_describe(Some(cg.project_root()), args).await
        }
        "tokensave_lcm_expand" => session::handle_lcm_expand(Some(cg.project_root()), args).await,
        "tokensave_lcm_expand_query" => {
            session::handle_lcm_expand_query(Some(cg.project_root()), args).await
        }
        "tokensave_lcm_preflight" => {
            session::handle_lcm_preflight(Some(cg.project_root()), args).await
        }
        "tokensave_lcm_compress" => {
            session::handle_lcm_compress(Some(cg.project_root()), args).await
        }
        "tokensave_lcm_session_boundary" => {
            session::handle_lcm_session_boundary(Some(cg.project_root()), args).await
        }
        _ => Err(TokenSaveError::Config {
            message: format!("unknown tool: {tool_name}"),
        }),
    }
}

/// Dispatches only the storage-scoped LCM tools that can run without an
/// initialized project (e.g. `storage_scope=hermes_profile`).
pub async fn handle_profile_scoped_lcm_tool_call(
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    match tool_name {
        "tokensave_lcm_status" => session::handle_lcm_status(None, args).await,
        "tokensave_lcm_doctor" => session::handle_lcm_doctor(None, args).await,
        "tokensave_lcm_load_session" => session::handle_lcm_load_session(None, args).await,
        "tokensave_lcm_grep" => session::handle_lcm_grep(None, args).await,
        "tokensave_lcm_describe" => session::handle_lcm_describe(None, args).await,
        "tokensave_lcm_expand" => session::handle_lcm_expand(None, args).await,
        "tokensave_lcm_expand_query" => session::handle_lcm_expand_query(None, args).await,
        "tokensave_lcm_preflight" => session::handle_lcm_preflight(None, args).await,
        "tokensave_lcm_compress" => session::handle_lcm_compress(None, args).await,
        "tokensave_lcm_session_boundary" => session::handle_lcm_session_boundary(None, args).await,
        _ => Err(TokenSaveError::Config {
            message: format!("tool `{tool_name}` does not support profile-scoped dispatch"),
        }),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod tests {
    use serde_json::json;

    use super::super::get_tool_definitions;
    use super::*;

    #[test]
    fn test_tool_definitions_complete() {
        let tools = get_tool_definitions();
        // ast_grep_rewrite is conditionally registered based on whether the
        // external `ast-grep` binary is on PATH — agents should never see a
        // tool that will instantly fail. The count and the per-tool checks
        // below adapt to the host's capability set.
        let expected_total = if super::super::definitions::ast_grep_available() {
            89
        } else {
            88
        };
        assert_eq!(tools.len(), expected_total);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"tokensave_search"));
        assert!(tool_names.contains(&"tokensave_retrieve"));
        assert!(tool_names.contains(&"tokensave_context"));
        assert!(tool_names.contains(&"tokensave_callers"));
        assert!(tool_names.contains(&"tokensave_callees"));
        assert!(tool_names.contains(&"tokensave_callers_for"));
        assert!(tool_names.contains(&"tokensave_by_qualified_name"));
        assert!(tool_names.contains(&"tokensave_signature"));
        assert!(tool_names.contains(&"tokensave_impls"));
        assert!(tool_names.contains(&"tokensave_diagnose"));
        assert!(tool_names.contains(&"tokensave_run_affected_tests"));
        assert!(tool_names.contains(&"tokensave_derives"));
        assert!(tool_names.contains(&"tokensave_fact_store"));
        assert!(tool_names.contains(&"tokensave_fact_feedback"));
        assert!(tool_names.contains(&"tokensave_memory_status"));
        assert!(tool_names.contains(&"tokensave_message_search"));
        assert!(tool_names.contains(&"tokensave_impact"));
        assert!(tool_names.contains(&"tokensave_node"));
        assert!(tool_names.contains(&"tokensave_status"));
        assert!(tool_names.contains(&"tokensave_files"));
        assert!(tool_names.contains(&"tokensave_affected"));
        assert!(tool_names.contains(&"tokensave_dead_code"));
        assert!(tool_names.contains(&"tokensave_diff_context"));
        assert!(tool_names.contains(&"tokensave_module_api"));
        assert!(tool_names.contains(&"tokensave_circular"));
        assert!(tool_names.contains(&"tokensave_hotspots"));
        assert!(tool_names.contains(&"tokensave_similar"));
        assert!(tool_names.contains(&"tokensave_rename_preview"));
        assert!(tool_names.contains(&"tokensave_unused_imports"));
        assert!(tool_names.contains(&"tokensave_changelog"));
        assert!(tool_names.contains(&"tokensave_rank"));
        assert!(tool_names.contains(&"tokensave_largest"));
        assert!(tool_names.contains(&"tokensave_coupling"));
        assert!(tool_names.contains(&"tokensave_inheritance_depth"));
        assert!(tool_names.contains(&"tokensave_distribution"));
        assert!(tool_names.contains(&"tokensave_recursion"));
        assert!(tool_names.contains(&"tokensave_complexity"));
        assert!(tool_names.contains(&"tokensave_doc_coverage"));
        assert!(tool_names.contains(&"tokensave_god_class"));
        assert!(tool_names.contains(&"tokensave_port_status"));
        assert!(tool_names.contains(&"tokensave_port_order"));
        assert!(tool_names.contains(&"tokensave_commit_context"));
        assert!(tool_names.contains(&"tokensave_pr_context"));
        assert!(tool_names.contains(&"tokensave_simplify_scan"));
        assert!(tool_names.contains(&"tokensave_test_map"));
        assert!(tool_names.contains(&"tokensave_type_hierarchy"));
        assert!(tool_names.contains(&"tokensave_branch_search"));
        assert!(tool_names.contains(&"tokensave_branch_diff"));
        assert!(tool_names.contains(&"tokensave_branch_list"));
        assert!(tool_names.contains(&"tokensave_str_replace"));
        assert!(tool_names.contains(&"tokensave_multi_str_replace"));
        assert!(tool_names.contains(&"tokensave_insert_at"));
        if super::super::definitions::ast_grep_available() {
            assert!(tool_names.contains(&"tokensave_ast_grep_rewrite"));
        } else {
            assert!(!tool_names.contains(&"tokensave_ast_grep_rewrite"));
        }
        assert!(tool_names.contains(&"tokensave_gini"));
        assert!(tool_names.contains(&"tokensave_dependency_depth"));
        assert!(tool_names.contains(&"tokensave_health"));
        assert!(tool_names.contains(&"tokensave_redundancy"));
        assert!(tool_names.contains(&"tokensave_runtime"));
        assert!(tool_names.contains(&"tokensave_dsm"));
        assert!(tool_names.contains(&"tokensave_test_risk"));
        assert!(tool_names.contains(&"tokensave_session_start"));
        assert!(tool_names.contains(&"tokensave_session_end"));
        assert!(tool_names.contains(&"tokensave_body"));
        assert!(tool_names.contains(&"tokensave_todos"));
        assert!(tool_names.contains(&"tokensave_fact_store"));
        assert!(tool_names.contains(&"tokensave_fact_feedback"));
        assert!(tool_names.contains(&"tokensave_memory_status"));
        assert!(tool_names.contains(&"tokensave_dashboard"));
        assert!(tool_names.contains(&"tokensave_message_search"));
        assert!(tool_names.contains(&"tokensave_lcm_status"));
        assert!(tool_names.contains(&"tokensave_lcm_doctor"));
        assert!(tool_names.contains(&"tokensave_lcm_load_session"));
        assert!(tool_names.contains(&"tokensave_lcm_grep"));
        assert!(tool_names.contains(&"tokensave_lcm_describe"));
        assert!(tool_names.contains(&"tokensave_lcm_expand"));
        assert!(tool_names.contains(&"tokensave_lcm_expand_query"));
        assert!(tool_names.contains(&"tokensave_lcm_preflight"));
        assert!(tool_names.contains(&"tokensave_lcm_compress"));
        assert!(tool_names.contains(&"tokensave_lcm_session_boundary"));
        assert!(tool_names.contains(&"tokensave_read"));
        assert!(tool_names.contains(&"tokensave_outline"));
        assert!(tool_names.contains(&"tokensave_implementations"));
        assert!(tool_names.contains(&"tokensave_unsafe_patterns"));
        assert!(tool_names.contains(&"tokensave_diagnostics"));
        assert!(tool_names.contains(&"tokensave_config"));
        assert!(tool_names.contains(&"tokensave_signature_search"));
        assert!(tool_names.contains(&"tokensave_constructors"));
        assert!(tool_names.contains(&"tokensave_field_sites"));
        assert!(tool_names.contains(&"tokensave_call_chain"));
        assert!(tool_names.contains(&"tokensave_file_dependents"));
        assert!(tool_names.contains(&"tokensave_replace_symbol"));
        assert!(tool_names.contains(&"tokensave_insert_at_symbol"));
        assert!(tool_names.contains(&"tokensave_find_exact_symbol"));
    }

    #[test]
    fn test_tool_definitions_have_schemas() {
        let tools = get_tool_definitions();
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(tool.input_schema.is_object());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }

    #[test]
    fn test_tool_definitions_have_annotations() {
        let tools = get_tool_definitions();
        let write_tools = [
            "tokensave_str_replace",
            "tokensave_multi_str_replace",
            "tokensave_insert_at",
            "tokensave_replace_symbol",
            "tokensave_insert_at_symbol",
            "tokensave_ast_grep_rewrite",
            "tokensave_run_affected_tests",
            "tokensave_session_start",
            "tokensave_session_end",
            "tokensave_fact_store",
            "tokensave_fact_feedback",
            "tokensave_memory_status",
            "tokensave_lcm_doctor",
            "tokensave_lcm_preflight",
            "tokensave_lcm_compress",
            "tokensave_lcm_session_boundary",
        ];
        for tool in &tools {
            let ann = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} missing annotations", tool.name));
            if write_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    ann["readOnlyHint"], false,
                    "{} should have readOnlyHint=false",
                    tool.name
                );
            } else {
                assert_eq!(
                    ann["readOnlyHint"], true,
                    "{} missing readOnlyHint",
                    tool.name
                );
            }
            assert!(
                ann["title"].is_string(),
                "{} missing title annotation",
                tool.name
            );
        }
    }

    #[test]
    fn test_always_load_tools() {
        let tools = get_tool_definitions();
        let always_load: Vec<&str> = tools
            .iter()
            .filter(|t| {
                t.meta
                    .as_ref()
                    .and_then(|m| m.get("anthropic/alwaysLoad"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            always_load.contains(&"tokensave_context"),
            "tokensave_context must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tokensave_search"),
            "tokensave_search must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tokensave_status"),
            "tokensave_status must be alwaysLoad"
        );
        assert_eq!(
            always_load.len(),
            3,
            "exactly 3 tools should be alwaysLoad, got {:?}",
            always_load
        );
    }

    #[test]
    fn test_truncate_short_response() {
        let short = "hello world";
        assert_eq!(truncate_response(short), short);
    }

    #[test]
    fn test_truncate_long_response() {
        let long = "x".repeat(20_000);
        let result = truncate_response(&long);
        assert!(result.len() < 20_000);
        assert!(result.contains("[... truncated at 15000 chars]"));
    }

    #[test]
    fn test_truncated_json_envelope_includes_handle() {
        let dir = tempfile::TempDir::new().unwrap();
        let long = format!(
            "{{\"items\":[{}]}}",
            (0..3_000)
                .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
                .collect::<Vec<_>>()
                .join(",")
        );

        let result = truncated_json_envelope_with_handle(Some(dir.path()), &long);
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["retrieve_tool"], "tokensave_retrieve");
        assert!(parsed.get("retrieve_handle").is_none());
        let handle = parsed["handle"].as_str().unwrap();
        assert!(handle.starts_with("rh_"));

        let stored = crate::mcp::response_handles::retrieve_response_handle(
            dir.path(),
            handle,
            crate::tokensave::current_timestamp(),
        )
        .unwrap()
        .expect("stored response should be retrievable");
        assert_eq!(stored.content, long);
    }

    #[test]
    fn test_tool_definitions_serializable() {
        let tools = get_tool_definitions();
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains("tokensave_search"));
        assert!(json.contains("tokensave_status"));
    }

    #[test]
    fn test_require_node_id_canonical() {
        let args = json!({"node_id": "fn:abc123"});
        assert_eq!(require_node_id(&args).unwrap(), "fn:abc123");
    }

    #[test]
    fn test_require_node_id_alias() {
        let args = json!({"id": "trait:def456"});
        assert_eq!(require_node_id(&args).unwrap(), "trait:def456");
    }

    #[test]
    fn test_require_node_id_prefers_canonical() {
        let args = json!({"node_id": "fn:canonical", "id": "fn:alias"});
        assert_eq!(require_node_id(&args).unwrap(), "fn:canonical");
    }

    #[test]
    fn test_require_node_id_missing() {
        let args = json!({"query": "something"});
        assert!(require_node_id(&args).is_err());
    }
}
