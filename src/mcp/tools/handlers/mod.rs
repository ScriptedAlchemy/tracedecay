//! MCP tool call handlers.
//!
//! Each `handle_*` function implements one MCP tool: it deserializes
//! the JSON arguments, calls the appropriate `TraceDecay` method, and
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
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::{global_db_path, GlobalDb, ProjectRegistryContext};
use crate::mcp::response_handles::{
    note_response_handle_store_skipped_no_project_root, observe_response_truncation,
    retrieve_response_handle, store_response_handle, ResponseHandleLookup,
    RESPONSE_HANDLE_TTL_SECS, RESPONSE_RETRIEVE_TOOL,
};
use crate::tracedecay::current_timestamp;
use crate::tracedecay::TraceDecay;

use super::{ToolResult, MAX_RESPONSE_CHARS};

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias. LLMs occasionally shorten `node_id` to `id`; this avoids a
/// confusing error when that happens.
pub(crate) fn require_node_id(args: &Value) -> Result<&str> {
    args.get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
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

pub(crate) fn safe_profile_relpath(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(TraceDecayError::Config {
            message: format!("registry artifact path is not a safe profile-relative path: {value}"),
        });
    }
    Ok(path)
}

pub(crate) fn global_db_profile_root() -> Result<PathBuf> {
    global_db_path()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not resolve tracedecay profile root".to_string(),
        })
}

pub(crate) fn project_selector_present(args: &Value, top_level_path_keys: &[&str]) -> bool {
    args.get("project_selector").is_some()
        || args.get("project_id").is_some()
        || top_level_path_keys
            .iter()
            .any(|key| args.get(*key).is_some())
}

fn rejected_tool_project_selector_present(tool_name: &str, args: &Value) -> bool {
    let top_level_path_keys = if tool_name.starts_with("tracedecay_lcm_") {
        &["project_path"][..]
    } else {
        &["project_path", "project_root"][..]
    };
    project_selector_present(args, top_level_path_keys)
}

pub(crate) async fn project_registry_context(
    args: &Value,
    top_level_path_keys: &[&str],
) -> Result<Option<ProjectRegistryContext>> {
    let selector_present = project_selector_present(args, top_level_path_keys);
    let selector = args
        .get("project_selector")
        .map(|value| {
            value.as_object().ok_or_else(|| TraceDecayError::Config {
                message: "project_selector must be an object".to_string(),
            })
        })
        .transpose()?;
    let project_id = selector
        .and_then(|selector| selector.get("project_id"))
        .or_else(|| args.get("project_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let project_path = selector
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| top_level_path_keys.iter().find_map(|key| args.get(*key)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if project_id.is_none() && project_path.is_none() {
        if selector_present {
            return Err(TraceDecayError::Config {
                message: "project selector must include project_id or project_path".to_string(),
            });
        }
        return Ok(None);
    }

    let db = GlobalDb::open()
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not open tracedecay project registry; run tracedecay init first"
                .to_string(),
        })?;
    let context = if let Some(project_id) = project_id {
        db.project_registry_context_by_id(project_id).await
    } else if let Some(project_path) = project_path {
        db.project_registry_context_by_alias(Path::new(project_path))
            .await
    } else {
        return Ok(None);
    };

    context
        .ok_or_else(|| TraceDecayError::Config {
            message: "registered project not found for selector".to_string(),
        })
        .map(Some)
}

fn tool_accepts_registered_project_selector(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_search"
            | "tracedecay_context"
            | "tracedecay_retrieve"
            | "tracedecay_callers"
            | "tracedecay_callees"
            | "tracedecay_impact"
            | "tracedecay_node"
            | "tracedecay_files"
            | "tracedecay_body"
            | "tracedecay_read"
            | "tracedecay_outline"
            | "tracedecay_signature_search"
            | "tracedecay_implementations"
            | "tracedecay_callers_for"
            | "tracedecay_call_chain"
            | "tracedecay_file_dependents"
            | "tracedecay_find_exact_symbol"
            | "tracedecay_by_qualified_name"
            | "tracedecay_signature"
            | "tracedecay_impls"
            | "tracedecay_derives"
            | "tracedecay_project_context"
            | "tracedecay_fact_store"
            | "tracedecay_memory_status"
            | "tracedecay_message_search"
    )
}

fn tool_dispatches_registered_project_reader(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_search"
            | "tracedecay_context"
            | "tracedecay_retrieve"
            | "tracedecay_callers"
            | "tracedecay_callees"
            | "tracedecay_impact"
            | "tracedecay_node"
            | "tracedecay_files"
            | "tracedecay_body"
            | "tracedecay_read"
            | "tracedecay_outline"
            | "tracedecay_signature_search"
            | "tracedecay_implementations"
            | "tracedecay_callers_for"
            | "tracedecay_call_chain"
            | "tracedecay_file_dependents"
            | "tracedecay_find_exact_symbol"
            | "tracedecay_by_qualified_name"
            | "tracedecay_signature"
            | "tracedecay_impls"
            | "tracedecay_derives"
    )
}

async fn selected_registered_project_reader(
    tool_name: &str,
    args: &Value,
) -> Result<Option<TraceDecay>> {
    if !tool_dispatches_registered_project_reader(tool_name) {
        return Ok(None);
    }
    let Some(context) = project_registry_context(args, &["project_path", "project_root"]).await?
    else {
        return Ok(None);
    };

    TraceDecay::open_read_only(Path::new(&context.project.canonical_root))
        .await
        .map(Some)
}

/// Truncates a string to the maximum response character limit, appending
/// a truncation notice if necessary.
pub(crate) fn truncate_response(s: &str) -> String {
    debug_assert!(!s.is_empty(), "truncate_response called with empty string");
    if s.len() <= MAX_RESPONSE_CHARS {
        s.to_string()
    } else {
        let started = std::time::Instant::now();
        let now = current_timestamp();
        // Find a valid UTF-8 character boundary at or before MAX_RESPONSE_CHARS
        let mut end = MAX_RESPONSE_CHARS;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        let truncated = format!("{}\n\n[... truncated at {} chars]", &s[..end], end);
        observe_response_truncation(
            s.len(),
            truncated.len(),
            false,
            now,
            "not_available",
            started.elapsed(),
        );
        truncated
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
///
/// If local handle storage is unavailable or fails, the envelope still carries
/// a preview but also includes explicit recovery metadata so clients can tell
/// why no handle was emitted and what to retry.
pub(crate) fn truncated_json_envelope_with_handle(
    project_root: Option<&Path>,
    formatted: &str,
) -> String {
    if formatted.len() <= MAX_RESPONSE_CHARS {
        return formatted.to_string();
    }
    let started = std::time::Instant::now();
    let now = current_timestamp();
    let mut handle_unavailable = None;
    let stored = if let Some(root) = project_root {
        match store_response_handle(root, formatted, current_timestamp()) {
            Ok(record) => Some(record),
            Err(err) => {
                handle_unavailable = Some(json!({
                    "reason_code": "handle_store_failed",
                    "message": format!(
                        "The full response could not be cached locally, so no retrieval handle is available: {err}"
                    ),
                    "retryable": true,
                    "retry_instruction": "Fix the local project cache path or filesystem error, then re-run the original MCP tool to regenerate the full response and a fresh handle."
                }));
                None
            }
        }
    } else {
        note_response_handle_store_skipped_no_project_root();
        handle_unavailable = Some(json!({
            "reason_code": "handle_storage_unavailable",
            "message": "This response was truncated in a context without a project-local cache path, so no retrieval handle could be created.",
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool from a project-scoped tracedecay session if you need a retrievable full response."
        }));
        None
    };
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
        if let Some(object) = envelope.as_object_mut() {
            if let Some(record) = &stored {
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
                        "This response was truncated: `preview` contains only the first {} of {} characters. The full original response is stored locally in this project and expires at {} (TTL {} seconds). To recover it, call `{RESPONSE_RETRIEVE_TOOL}` with required argument `handle` set to `{}`. If the original tool call used a project selector (`project_id`, `project_path`, or `project_selector`), pass the same selector to `{RESPONSE_RETRIEVE_TOOL}` so the handle is looked up in the same project cache. Only call it if the missing details are needed to answer the user's request.",
                        preview.len(),
                        formatted.len(),
                        record.expires_at,
                        RESPONSE_HANDLE_TTL_SECS,
                        record.handle
                    )),
                );
            } else if let Some(status) = &handle_unavailable {
                object.insert("handle_available".to_string(), json!(false));
                object.insert("handle_status".to_string(), status.clone());
            }
        }
        let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
        if text.len() <= MAX_RESPONSE_CHARS || end == 0 {
            let handle_status = if stored.is_some() {
                "stored"
            } else if project_root.is_none() {
                "no_project_root"
            } else {
                "store_failed"
            };
            observe_response_truncation(
                formatted.len(),
                text.len(),
                true,
                now,
                handle_status,
                started.elapsed(),
            );
            return text;
        }
        end = end.saturating_sub(1024);
    }
}

fn handle_retrieve(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "missing required parameter: handle (copy the exact `handle` value from a truncated MCP response envelope)"
                        .to_string(),
            })?;
    let payload = match retrieve_response_handle(cg.project_root(), handle, current_timestamp())? {
        ResponseHandleLookup::Found(record) => json!({
            "handle": record.handle,
            "expired": false,
            "original_chars": record.original_chars(),
            "created_at": record.created_at,
            "expires_at": record.expires_at,
            "content": record.content,
        }),
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
/// is included in `tracedecay_status` responses when provided.
pub async fn handle_tool_call(
    cg: &TraceDecay,
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
        tool_name.starts_with("tracedecay_"),
        "tool_name must start with 'tracedecay_' prefix"
    );
    if !tool_accepts_registered_project_selector(tool_name)
        && rejected_tool_project_selector_present(tool_name, &args)
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "{tool_name} is scoped to the active project and does not accept project selectors"
            ),
        });
    }
    let selected_cg = selected_registered_project_reader(tool_name, &args).await?;
    let selected_scope_prefix = if selected_cg.is_some() {
        None
    } else {
        scope_prefix
    };
    let cg = selected_cg.as_ref().unwrap_or(cg);
    match tool_name {
        "tracedecay_search" => graph::handle_search(cg, args, selected_scope_prefix).await,
        "tracedecay_retrieve" => handle_retrieve(cg, &args),
        "tracedecay_context" => graph::handle_context(cg, args, selected_scope_prefix).await,
        "tracedecay_callers" => graph::handle_callers(cg, args).await,
        "tracedecay_callees" => graph::handle_callees(cg, args).await,
        "tracedecay_impact" => graph::handle_impact(cg, args).await,
        "tracedecay_node" => graph::handle_node(cg, args).await,
        "tracedecay_status" => info::handle_status(cg, server_stats, scope_prefix).await,
        "tracedecay_active_project" => {
            Ok(info::handle_active_project(cg, server_stats, scope_prefix))
        }
        "tracedecay_storage_status" => info::handle_storage_status(cg, scope_prefix).await,
        "tracedecay_project_list" => info::handle_project_list(args).await,
        "tracedecay_project_search" => info::handle_project_search(args).await,
        "tracedecay_project_context" => info::handle_project_context(cg, args).await,
        "tracedecay_files" => info::handle_files(cg, args, selected_scope_prefix).await,
        "tracedecay_affected" => git::handle_affected(cg, args).await,
        "tracedecay_dead_code" => analysis::handle_dead_code(cg, args, scope_prefix).await,
        "tracedecay_diff_context" => git::handle_diff_context(cg, args).await,
        "tracedecay_module_api" => analysis::handle_module_api(cg, args, scope_prefix).await,
        "tracedecay_circular" => analysis::handle_circular(cg, args).await,
        "tracedecay_hotspots" => analysis::handle_hotspots(cg, args, scope_prefix).await,
        "tracedecay_similar" => graph::handle_similar(cg, args).await,
        "tracedecay_rename_preview" => graph::handle_rename_preview(cg, args).await,
        "tracedecay_unused_imports" => {
            analysis::handle_unused_imports(cg, args, scope_prefix).await
        }
        "tracedecay_rank" => analysis::handle_rank(cg, args, scope_prefix).await,
        "tracedecay_largest" => analysis::handle_largest(cg, args, scope_prefix).await,
        "tracedecay_coupling" => analysis::handle_coupling(cg, args, scope_prefix).await,
        "tracedecay_inheritance_depth" => {
            analysis::handle_inheritance_depth(cg, args, scope_prefix).await
        }
        "tracedecay_distribution" => analysis::handle_distribution(cg, args, scope_prefix).await,
        "tracedecay_recursion" => analysis::handle_recursion(cg, args, scope_prefix).await,
        "tracedecay_complexity" => analysis::handle_complexity(cg, args, scope_prefix).await,
        "tracedecay_doc_coverage" => analysis::handle_doc_coverage(cg, args, scope_prefix).await,
        "tracedecay_god_class" => analysis::handle_god_class(cg, args, scope_prefix).await,
        "tracedecay_changelog" => git::handle_changelog(cg, args).await,
        "tracedecay_port_status" => info::handle_port_status(cg, args).await,
        "tracedecay_port_order" => info::handle_port_order(cg, args).await,
        "tracedecay_commit_context" => git::handle_commit_context(cg, args).await,
        "tracedecay_pr_context" => git::handle_pr_context(cg, args).await,
        "tracedecay_simplify_scan" => info::handle_simplify_scan(cg, args, scope_prefix).await,
        "tracedecay_test_map" => health::handle_test_map(cg, args, scope_prefix).await,
        "tracedecay_type_hierarchy" => info::handle_type_hierarchy(cg, args).await,
        "tracedecay_branch_search" => git::handle_branch_search(cg, args).await,
        "tracedecay_branch_diff" => git::handle_branch_diff(cg, args).await,
        "tracedecay_branch_list" => Ok(git::handle_branch_list(cg)),
        "tracedecay_str_replace" => edit::handle_str_replace(cg, args).await,
        "tracedecay_multi_str_replace" => edit::handle_multi_str_replace(cg, args).await,
        "tracedecay_insert_at" => edit::handle_insert_at(cg, args).await,
        "tracedecay_ast_grep_rewrite" => edit::handle_ast_grep_rewrite(cg, args).await,
        "tracedecay_gini" => health::handle_gini(cg, args, scope_prefix).await,
        "tracedecay_dependency_depth" => {
            health::handle_dependency_depth(cg, args, scope_prefix).await
        }
        "tracedecay_health" => health::handle_health(cg, args, scope_prefix).await,
        "tracedecay_redundancy" => redundancy::handle_redundancy(cg, args, scope_prefix).await,
        "tracedecay_runtime" => health::handle_runtime(cg, args).await,
        "tracedecay_dsm" => health::handle_dsm(cg, args, scope_prefix).await,
        "tracedecay_test_risk" => health::handle_test_risk(cg, args, scope_prefix).await,
        "tracedecay_session_start" => health::handle_session_start(cg, args, scope_prefix).await,
        "tracedecay_session_end" => health::handle_session_end(cg, args, scope_prefix).await,
        "tracedecay_body" => info::handle_body(cg, args, selected_scope_prefix).await,
        "tracedecay_todos" => info::handle_todos(cg, args, scope_prefix).await,
        "tracedecay_read" => info::handle_read(cg, args).await,
        "tracedecay_outline" => info::handle_outline(cg, args).await,
        "tracedecay_config" => info::handle_config(cg, &args),
        "tracedecay_signature_search" => {
            info::handle_signature_search(cg, args, selected_scope_prefix).await
        }
        "tracedecay_implementations" => {
            graph::handle_implementations(cg, args, selected_scope_prefix).await
        }
        "tracedecay_unsafe_patterns" => {
            analysis::handle_unsafe_patterns(cg, args, scope_prefix).await
        }
        "tracedecay_diagnostics" => analysis::handle_diagnostics(cg, args).await,
        "tracedecay_constructors" => analysis::handle_constructors(cg, args, scope_prefix).await,
        "tracedecay_field_sites" => analysis::handle_field_sites(cg, args, scope_prefix).await,
        "tracedecay_callers_for" => graph::handle_callers_for(cg, args).await,
        "tracedecay_call_chain" => graph::handle_call_chain(cg, args).await,
        "tracedecay_file_dependents" => graph::handle_file_dependents(cg, args).await,
        "tracedecay_replace_symbol" => edit::handle_replace_symbol(cg, args).await,
        "tracedecay_insert_at_symbol" => edit::handle_insert_at_symbol(cg, args).await,
        "tracedecay_find_exact_symbol" => {
            graph::handle_find_exact_symbol(cg, args, selected_scope_prefix).await
        }
        "tracedecay_by_qualified_name" => graph::handle_by_qualified_name(cg, args).await,
        "tracedecay_signature" => graph::handle_signature(cg, args).await,
        "tracedecay_impls" => graph::handle_impls(cg, args).await,
        "tracedecay_diagnose" => workflow::handle_diagnose(cg, args).await,
        "tracedecay_run_affected_tests" => workflow::handle_run_affected_tests(cg, args).await,
        "tracedecay_derives" => graph::handle_derives(cg, args).await,
        "tracedecay_fact_store" => memory::handle_fact_store(cg, args).await,
        "tracedecay_fact_feedback" => memory::handle_fact_feedback(cg, args).await,
        "tracedecay_memory_status" => memory::handle_memory_status(cg, args).await,
        "tracedecay_dashboard" => dashboard::handle_dashboard(cg, args).await,
        "tracedecay_message_search" => session::handle_message_search(cg, args).await,
        "tracedecay_lcm_status" => session::handle_lcm_status(Some(cg.project_root()), args).await,
        "tracedecay_lcm_doctor" => session::handle_lcm_doctor(Some(cg.project_root()), args).await,
        "tracedecay_lcm_load_session" => {
            session::handle_lcm_load_session(Some(cg.project_root()), args).await
        }
        "tracedecay_lcm_grep" => session::handle_lcm_grep(Some(cg.project_root()), args).await,
        "tracedecay_lcm_describe" => {
            session::handle_lcm_describe(Some(cg.project_root()), args).await
        }
        "tracedecay_lcm_expand" => session::handle_lcm_expand(Some(cg.project_root()), args).await,
        "tracedecay_lcm_expand_query" => {
            session::handle_lcm_expand_query(Some(cg.project_root()), args).await
        }
        "tracedecay_lcm_preflight" => {
            session::handle_lcm_preflight(Some(cg.project_root()), args).await
        }
        "tracedecay_lcm_compress" => {
            session::handle_lcm_compress(Some(cg.project_root()), args).await
        }
        "tracedecay_lcm_session_boundary" => {
            session::handle_lcm_session_boundary(Some(cg.project_root()), args).await
        }
        _ => Err(TraceDecayError::Config {
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
        "tracedecay_lcm_status" => session::handle_lcm_status(None, args).await,
        "tracedecay_lcm_doctor" => session::handle_lcm_doctor(None, args).await,
        "tracedecay_lcm_load_session" => session::handle_lcm_load_session(None, args).await,
        "tracedecay_lcm_grep" => session::handle_lcm_grep(None, args).await,
        "tracedecay_lcm_describe" => session::handle_lcm_describe(None, args).await,
        "tracedecay_lcm_expand" => session::handle_lcm_expand(None, args).await,
        "tracedecay_lcm_expand_query" => session::handle_lcm_expand_query(None, args).await,
        "tracedecay_lcm_preflight" => session::handle_lcm_preflight(None, args).await,
        "tracedecay_lcm_compress" => session::handle_lcm_compress(None, args).await,
        "tracedecay_lcm_session_boundary" => session::handle_lcm_session_boundary(None, args).await,
        _ => Err(TraceDecayError::Config {
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
    use std::collections::BTreeSet;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::Path;

    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use super::super::get_tool_definitions;
    use super::*;
    use crate::config::USER_DATA_DIR_ENV;

    static SELECTOR_ENV_LOCK: Mutex<()> = Mutex::const_new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct SelectorEnv {
        _home: EnvVarGuard,
        _userprofile: EnvVarGuard,
        _data_dir: EnvVarGuard,
        _global_db: EnvVarGuard,
    }

    impl SelectorEnv {
        fn new(root: &Path) -> Self {
            let home = root.join("home");
            let profile_root = home.join(".tracedecay");
            fs::create_dir_all(&profile_root).unwrap();
            let home = home.canonicalize().unwrap();
            let profile_root = home.join(".tracedecay");
            Self {
                _home: EnvVarGuard::set("HOME", &home),
                _userprofile: EnvVarGuard::set("USERPROFILE", &home),
                _data_dir: EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root),
                _global_db: EnvVarGuard::set(
                    "TRACEDECAY_GLOBAL_DB",
                    profile_root.join("global.db"),
                ),
            }
        }
    }

    fn dispatch_tool_names_from_source(function_name: &str) -> BTreeSet<String> {
        let source = include_str!("mod.rs");
        let fn_marker = format!("pub async fn {function_name}");
        let function_source = source
            .split_once(&fn_marker)
            .unwrap_or_else(|| panic!("missing function source for {function_name}"))
            .1;
        let match_source = function_source
            .split_once("match tool_name {")
            .unwrap_or_else(|| panic!("{function_name} does not match on tool_name"))
            .1;
        let handler_arms = match_source
            .split_once("_ => Err")
            .unwrap_or_else(|| panic!("{function_name} does not have an unknown-tool fallback"))
            .0;

        handler_arms
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                if !trimmed.starts_with("\"tracedecay_") || !trimmed.contains("=>") {
                    return None;
                }
                let after_opening_quote = trimmed.strip_prefix('"')?;
                let (name, after_name) = after_opening_quote.split_once('"')?;
                if after_name.trim_start().starts_with("=>") {
                    Some(name.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    fn assert_set_empty(names: BTreeSet<String>, message: &str) {
        assert!(
            names.is_empty(),
            "{message}: {}",
            names.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    // MCP registry maintenance guardrail:
    // when adding a tool, update all three surfaces together: its
    // `def_*` entry in definitions.rs, the `get_tool_definitions()` registry,
    // and the `handle_tool_call` match arm below. For profile-scoped LCM tools,
    // also advertise `storage_scope: ["project_local", "hermes_profile"]`
    // plus `hermes_home` in the schema, then add the profile handler arm. These
    // lockstep tests intentionally fail with the missing tool name when any
    // surface drifts.
    #[test]
    fn tool_definitions_and_dispatch_handlers_stay_in_lockstep() {
        let definition_names = get_tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        let mut handler_names = dispatch_tool_names_from_source("handle_tool_call");

        // This tool is intentionally hidden from the advertised surface when
        // the host is missing ast-grep; mirror that runtime filter so the
        // integrity check covers the actual MCP tools/list surface.
        if !super::super::definitions::ast_grep_available() {
            handler_names.remove("tracedecay_ast_grep_rewrite");
        }

        assert_set_empty(
            definition_names
                .difference(&handler_names)
                .cloned()
                .collect(),
            "MCP tool definitions missing handle_tool_call handlers",
        );
        assert_set_empty(
            handler_names
                .difference(&definition_names)
                .cloned()
                .collect(),
            "handle_tool_call handlers missing MCP tool definitions",
        );
    }

    #[test]
    fn profile_scoped_lcm_definitions_and_handlers_stay_in_lockstep() {
        let profile_scoped_definition_names = get_tool_definitions()
            .into_iter()
            .filter(|tool| {
                tool.input_schema["properties"]["storage_scope"]["enum"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == "hermes_profile"))
            })
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        let handler_names = dispatch_tool_names_from_source("handle_profile_scoped_lcm_tool_call");

        assert_set_empty(
            profile_scoped_definition_names
                .difference(&handler_names)
                .cloned()
                .collect(),
            "profile-scoped MCP tool definitions missing profile handler dispatch",
        );
        assert_set_empty(
            handler_names
                .difference(&profile_scoped_definition_names)
                .cloned()
                .collect(),
            "profile-scoped handler dispatches missing MCP tool definitions",
        );
    }

    #[test]
    fn graph_reader_selector_dispatch_policy_is_allowlisted() {
        let allowlisted_tool_names = [
            "tracedecay_search",
            "tracedecay_context",
            "tracedecay_callers",
            "tracedecay_callees",
            "tracedecay_impact",
            "tracedecay_node",
            "tracedecay_files",
            "tracedecay_retrieve",
            "tracedecay_body",
            "tracedecay_read",
            "tracedecay_outline",
            "tracedecay_signature_search",
            "tracedecay_implementations",
            "tracedecay_callers_for",
            "tracedecay_call_chain",
            "tracedecay_file_dependents",
            "tracedecay_find_exact_symbol",
            "tracedecay_by_qualified_name",
            "tracedecay_signature",
            "tracedecay_impls",
            "tracedecay_derives",
            "tracedecay_project_context",
            "tracedecay_fact_store",
            "tracedecay_memory_status",
            "tracedecay_message_search",
        ];

        for tool_name in allowlisted_tool_names {
            assert!(
                tool_accepts_registered_project_selector(tool_name),
                "{tool_name} should dispatch through a selected registered project"
            );
        }

        let definitions = get_tool_definitions()
            .into_iter()
            .map(|tool| (tool.name, tool.input_schema))
            .collect::<std::collections::BTreeMap<_, _>>();
        for tool_name in allowlisted_tool_names {
            let properties = &definitions[tool_name]["properties"];
            assert!(
                ["project_selector", "project_id", "project_path", "path"]
                    .iter()
                    .any(|selector_key| properties.get(*selector_key).is_some()),
                "{tool_name} should advertise at least one registered project selector because its dispatcher accepts registered project selectors"
            );
        }

        for tool_name in [
            "tracedecay_str_replace",
            "tracedecay_run_affected_tests",
            "tracedecay_status",
            "tracedecay_health",
            "tracedecay_dead_code",
        ] {
            assert!(
                !tool_accepts_registered_project_selector(tool_name),
                "{tool_name} should not be routed by the pure graph-reader selector policy"
            );
        }
    }

    #[tokio::test]
    async fn graph_reader_selector_dispatch_targets_registered_project() {
        let _env_lock = SELECTOR_ENV_LOCK.lock().await;
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let target_project = dir.path().join("target");
        fs::create_dir_all(active_project.join("src")).unwrap();
        fs::create_dir_all(target_project.join("src")).unwrap();
        fs::write(
            active_project.join("src/lib.rs"),
            "pub fn active_only_symbol() {}\n",
        )
        .unwrap();
        fs::write(
            target_project.join("src/lib.rs"),
            "pub fn target_only_symbol() {}\n",
        )
        .unwrap();

        let active = TraceDecay::init(&active_project).await.unwrap();
        let target = TraceDecay::init(&target_project).await.unwrap();
        active.index_all().await.unwrap();
        target.index_all().await.unwrap();
        let target_project_id = target
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("target project should be registered");

        let result = handle_tool_call(
            &active,
            "tracedecay_search",
            json!({
                "query": "target_only_symbol",
                "project_id": target_project_id,
                "limit": 5
            }),
            None,
            Some("tests"),
        )
        .await
        .unwrap();
        let text = result.value["content"][0]["text"].as_str().unwrap();

        assert!(
            text.contains("target_only_symbol"),
            "selected registered project search should return target graph results: {text}"
        );
        assert!(
            !text.contains("active_only_symbol"),
            "selected registered project search should not query the active graph: {text}"
        );
    }

    #[tokio::test]
    async fn unsupported_selector_tool_rejects_explicit_project_selector() {
        let _env_lock = SELECTOR_ENV_LOCK.lock().await;
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let project = dir.path().join("active");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn active_symbol() {}\n").unwrap();
        let cg = TraceDecay::init(&project).await.unwrap();
        cg.index_all().await.unwrap();

        let err = handle_tool_call(
            &cg,
            "tracedecay_status",
            json!({
                "project_id": "explicit-selector-should-not-fall-open",
            }),
            None,
            None,
        )
        .await
        .expect_err("unsupported selector tools must reject explicit selectors");

        assert!(
            format!("{err}").contains("does not accept project selectors"),
            "unexpected selector rejection error: {err}"
        );
    }

    #[tokio::test]
    async fn selected_project_retrieve_finds_selected_project_response_handle() {
        let _env_lock = SELECTOR_ENV_LOCK.lock().await;
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let target_project = dir.path().join("target");
        fs::create_dir_all(active_project.join("src")).unwrap();
        fs::create_dir_all(target_project.join("src")).unwrap();
        fs::write(
            active_project.join("src/lib.rs"),
            "pub fn active_only_symbol() {}\n",
        )
        .unwrap();

        let mut target_source = String::new();
        for i in 0..420 {
            target_source.push_str(&format!(
                "pub fn selected_project_handle_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}\n"
            ));
        }
        fs::write(target_project.join("src/lib.rs"), target_source).unwrap();

        let active = TraceDecay::init(&active_project).await.unwrap();
        let target = TraceDecay::init(&target_project).await.unwrap();
        active.index_all().await.unwrap();
        target.index_all().await.unwrap();
        let target_project_id = target
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("target project should be registered")
            .to_string();

        let result = handle_tool_call(
            &active,
            "tracedecay_search",
            json!({
                "query": "selected_project_handle_marker",
                "project_id": target_project_id,
                "limit": 420
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let envelope: Value = serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("search result text"),
        )
        .expect("truncated search envelope");
        assert_eq!(envelope["truncated"], true);
        let handle = envelope["handle"]
            .as_str()
            .expect("large selected-project search should return a handle");
        let retrieve_instruction = envelope["retrieve_instruction"]
            .as_str()
            .expect("truncated envelope should include retrieve guidance");
        assert!(
            retrieve_instruction.contains("pass the same selector"),
            "selected-project envelopes should tell clients to retrieve from the same project: {retrieve_instruction}"
        );

        let retrieved = handle_tool_call(
            &active,
            "tracedecay_retrieve",
            json!({
                "handle": handle,
                "project_id": target.store_layout().identity.project_id.as_deref().unwrap()
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(
            retrieved.value["content"][0]["text"]
                .as_str()
                .expect("retrieve result text"),
        )
        .expect("retrieve payload");

        assert_eq!(payload["expired"], false);
        assert!(
            payload["content"]
                .as_str()
                .is_some_and(|content| content.contains("selected_project_handle_marker_419")),
            "selected project retrieve should return the full selected-project response: {payload}"
        );
    }

    #[test]
    fn test_tool_definitions_complete() {
        let tools = get_tool_definitions();
        // ast_grep_rewrite is conditionally registered based on whether the
        // external `ast-grep` binary is on PATH — agents should never see a
        // tool that will instantly fail. The count and the per-tool checks
        // below adapt to the host's capability set.
        let expected_total = if super::super::definitions::ast_grep_available() {
            94
        } else {
            93
        };
        assert_eq!(tools.len(), expected_total);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"tracedecay_search"));
        assert!(tool_names.contains(&"tracedecay_retrieve"));
        assert!(tool_names.contains(&"tracedecay_context"));
        assert!(tool_names.contains(&"tracedecay_callers"));
        assert!(tool_names.contains(&"tracedecay_callees"));
        assert!(tool_names.contains(&"tracedecay_callers_for"));
        assert!(tool_names.contains(&"tracedecay_by_qualified_name"));
        assert!(tool_names.contains(&"tracedecay_signature"));
        assert!(tool_names.contains(&"tracedecay_impls"));
        assert!(tool_names.contains(&"tracedecay_diagnose"));
        assert!(tool_names.contains(&"tracedecay_run_affected_tests"));
        assert!(tool_names.contains(&"tracedecay_derives"));
        assert!(tool_names.contains(&"tracedecay_fact_store"));
        assert!(tool_names.contains(&"tracedecay_fact_feedback"));
        assert!(tool_names.contains(&"tracedecay_memory_status"));
        assert!(tool_names.contains(&"tracedecay_message_search"));
        assert!(tool_names.contains(&"tracedecay_impact"));
        assert!(tool_names.contains(&"tracedecay_node"));
        assert!(tool_names.contains(&"tracedecay_status"));
        assert!(tool_names.contains(&"tracedecay_active_project"));
        assert!(tool_names.contains(&"tracedecay_storage_status"));
        assert!(tool_names.contains(&"tracedecay_project_list"));
        assert!(tool_names.contains(&"tracedecay_project_search"));
        assert!(tool_names.contains(&"tracedecay_project_context"));
        assert!(tool_names.contains(&"tracedecay_files"));
        assert!(tool_names.contains(&"tracedecay_affected"));
        assert!(tool_names.contains(&"tracedecay_dead_code"));
        assert!(tool_names.contains(&"tracedecay_diff_context"));
        assert!(tool_names.contains(&"tracedecay_module_api"));
        assert!(tool_names.contains(&"tracedecay_circular"));
        assert!(tool_names.contains(&"tracedecay_hotspots"));
        assert!(tool_names.contains(&"tracedecay_similar"));
        assert!(tool_names.contains(&"tracedecay_rename_preview"));
        assert!(tool_names.contains(&"tracedecay_unused_imports"));
        assert!(tool_names.contains(&"tracedecay_changelog"));
        assert!(tool_names.contains(&"tracedecay_rank"));
        assert!(tool_names.contains(&"tracedecay_largest"));
        assert!(tool_names.contains(&"tracedecay_coupling"));
        assert!(tool_names.contains(&"tracedecay_inheritance_depth"));
        assert!(tool_names.contains(&"tracedecay_distribution"));
        assert!(tool_names.contains(&"tracedecay_recursion"));
        assert!(tool_names.contains(&"tracedecay_complexity"));
        assert!(tool_names.contains(&"tracedecay_doc_coverage"));
        assert!(tool_names.contains(&"tracedecay_god_class"));
        assert!(tool_names.contains(&"tracedecay_port_status"));
        assert!(tool_names.contains(&"tracedecay_port_order"));
        assert!(tool_names.contains(&"tracedecay_commit_context"));
        assert!(tool_names.contains(&"tracedecay_pr_context"));
        assert!(tool_names.contains(&"tracedecay_simplify_scan"));
        assert!(tool_names.contains(&"tracedecay_test_map"));
        assert!(tool_names.contains(&"tracedecay_type_hierarchy"));
        assert!(tool_names.contains(&"tracedecay_branch_search"));
        assert!(tool_names.contains(&"tracedecay_branch_diff"));
        assert!(tool_names.contains(&"tracedecay_branch_list"));
        assert!(tool_names.contains(&"tracedecay_str_replace"));
        assert!(tool_names.contains(&"tracedecay_multi_str_replace"));
        assert!(tool_names.contains(&"tracedecay_insert_at"));
        if super::super::definitions::ast_grep_available() {
            assert!(tool_names.contains(&"tracedecay_ast_grep_rewrite"));
        } else {
            assert!(!tool_names.contains(&"tracedecay_ast_grep_rewrite"));
        }
        assert!(tool_names.contains(&"tracedecay_gini"));
        assert!(tool_names.contains(&"tracedecay_dependency_depth"));
        assert!(tool_names.contains(&"tracedecay_health"));
        assert!(tool_names.contains(&"tracedecay_redundancy"));
        assert!(tool_names.contains(&"tracedecay_runtime"));
        assert!(tool_names.contains(&"tracedecay_dsm"));
        assert!(tool_names.contains(&"tracedecay_test_risk"));
        assert!(tool_names.contains(&"tracedecay_session_start"));
        assert!(tool_names.contains(&"tracedecay_session_end"));
        assert!(tool_names.contains(&"tracedecay_body"));
        assert!(tool_names.contains(&"tracedecay_todos"));
        assert!(tool_names.contains(&"tracedecay_fact_store"));
        assert!(tool_names.contains(&"tracedecay_fact_feedback"));
        assert!(tool_names.contains(&"tracedecay_memory_status"));
        assert!(tool_names.contains(&"tracedecay_dashboard"));
        assert!(tool_names.contains(&"tracedecay_message_search"));
        assert!(tool_names.contains(&"tracedecay_lcm_status"));
        assert!(tool_names.contains(&"tracedecay_lcm_doctor"));
        assert!(tool_names.contains(&"tracedecay_lcm_load_session"));
        assert!(tool_names.contains(&"tracedecay_lcm_grep"));
        assert!(tool_names.contains(&"tracedecay_lcm_describe"));
        assert!(tool_names.contains(&"tracedecay_lcm_expand"));
        assert!(tool_names.contains(&"tracedecay_lcm_expand_query"));
        assert!(tool_names.contains(&"tracedecay_lcm_preflight"));
        assert!(tool_names.contains(&"tracedecay_lcm_compress"));
        assert!(tool_names.contains(&"tracedecay_lcm_session_boundary"));
        assert!(tool_names.contains(&"tracedecay_read"));
        assert!(tool_names.contains(&"tracedecay_outline"));
        assert!(tool_names.contains(&"tracedecay_implementations"));
        assert!(tool_names.contains(&"tracedecay_unsafe_patterns"));
        assert!(tool_names.contains(&"tracedecay_diagnostics"));
        assert!(tool_names.contains(&"tracedecay_config"));
        assert!(tool_names.contains(&"tracedecay_signature_search"));
        assert!(tool_names.contains(&"tracedecay_constructors"));
        assert!(tool_names.contains(&"tracedecay_field_sites"));
        assert!(tool_names.contains(&"tracedecay_call_chain"));
        assert!(tool_names.contains(&"tracedecay_file_dependents"));
        assert!(tool_names.contains(&"tracedecay_replace_symbol"));
        assert!(tool_names.contains(&"tracedecay_insert_at_symbol"));
        assert!(tool_names.contains(&"tracedecay_find_exact_symbol"));
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
            "tracedecay_str_replace",
            "tracedecay_multi_str_replace",
            "tracedecay_insert_at",
            "tracedecay_replace_symbol",
            "tracedecay_insert_at_symbol",
            "tracedecay_ast_grep_rewrite",
            "tracedecay_run_affected_tests",
            "tracedecay_session_start",
            "tracedecay_session_end",
            "tracedecay_fact_store",
            "tracedecay_fact_feedback",
            "tracedecay_memory_status",
            "tracedecay_lcm_doctor",
            "tracedecay_lcm_preflight",
            "tracedecay_lcm_compress",
            "tracedecay_lcm_session_boundary",
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
            always_load.contains(&"tracedecay_context"),
            "tracedecay_context must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_search"),
            "tracedecay_search must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_status"),
            "tracedecay_status must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_active_project"),
            "tracedecay_active_project must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_storage_status"),
            "tracedecay_storage_status must be alwaysLoad"
        );
        assert_eq!(
            always_load.len(),
            5,
            "exactly 5 tools should be alwaysLoad, got {:?}",
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
        assert_eq!(parsed["retrieve_tool"], "tracedecay_retrieve");
        assert!(parsed.get("retrieve_handle").is_none());
        let handle = parsed["handle"].as_str().unwrap();
        assert!(handle.starts_with("rh_"));

        let stored = crate::mcp::response_handles::retrieve_response_handle(
            dir.path(),
            handle,
            crate::tracedecay::current_timestamp(),
        )
        .unwrap();
        match stored {
            ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
            other => panic!("stored response should be retrievable, got {other:?}"),
        }
    }

    #[test]
    fn test_truncated_json_envelope_reports_store_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".tracedecay")).unwrap();
        std::fs::write(
            dir.path().join(".tracedecay/enrollment.json"),
            r#"{"project_id":"../invalid","storage_mode":"profile_sharded"}"#,
        )
        .unwrap();
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
        assert_eq!(parsed["handle_available"], false);
        assert!(parsed.get("handle").is_none());
        assert_eq!(
            parsed["handle_status"]["reason_code"],
            "handle_store_failed"
        );
        assert!(parsed["handle_status"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be cached locally"));
    }

    #[test]
    fn test_tool_definitions_serializable() {
        let tools = get_tool_definitions();
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains("tracedecay_search"));
        assert!(json.contains("tracedecay_status"));
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
