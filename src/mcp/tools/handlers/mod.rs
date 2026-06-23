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
mod support;
pub mod workflow;

use std::path::Path;

use serde_json::{json, Value};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::response_handles::{retrieve_response_handle, ResponseHandleLookup};
use crate::tracedecay::current_timestamp;
use crate::tracedecay::TraceDecay;

use super::dispatch_policy::{
    tool_accepts_registered_project_selector, tool_dispatches_registered_project_reader,
};
use super::ToolResult;
use support::{profile_root_for_global_db, project_registry_context, project_selector_present};

fn rejected_tool_project_selector_present(tool_name: &str, args: &Value) -> bool {
    let top_level_path_keys = if tool_name.starts_with("tracedecay_lcm_") {
        &["project_path"][..]
    } else {
        &["project_path", "project_root"][..]
    };
    project_selector_present(args, top_level_path_keys)
}

async fn selected_registered_project_reader(
    tool_name: &str,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<Option<TraceDecay>> {
    if !tool_dispatches_registered_project_reader(tool_name) {
        return Ok(None);
    }
    let Some(context) = project_registry_context(
        args,
        &["project_path", "project_root"],
        global_db,
        allow_default_registry_fallback,
    )
    .await?
    else {
        return Ok(None);
    };

    let profile_root = profile_root_for_global_db(global_db, allow_default_registry_fallback)?;
    TraceDecay::open_read_only_with_options(
        Path::new(&context.project.canonical_root),
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: global_db.map(|db| db.db_path().to_path_buf()),
        },
    )
    .await
    .map(Some)
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
    let formatted = serde_json::to_string(&payload).unwrap_or_default();
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
    handle_tool_call_with_registry(cg, tool_name, args, server_stats, scope_prefix, None, true)
        .await
}

pub async fn handle_tool_call_with_registry(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
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
    let selected_cg = selected_registered_project_reader(
        tool_name,
        &args,
        global_db,
        allow_default_registry_fallback,
    )
    .await?;
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
        "tracedecay_status" => info::handle_status(cg, args, server_stats, scope_prefix).await,
        "tracedecay_active_project" => {
            Ok(info::handle_active_project(cg, server_stats, scope_prefix))
        }
        "tracedecay_storage_status" => info::handle_storage_status(cg, args, scope_prefix).await,
        "tracedecay_project_list" => {
            info::handle_project_list(args, global_db, allow_default_registry_fallback).await
        }
        "tracedecay_project_search" => {
            info::handle_project_search(args, global_db, allow_default_registry_fallback).await
        }
        "tracedecay_project_context" => {
            info::handle_project_context(cg, args, global_db, allow_default_registry_fallback).await
        }
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
        "tracedecay_fact_store" => {
            memory::handle_fact_store(cg, args, global_db, allow_default_registry_fallback).await
        }
        "tracedecay_fact_feedback" => memory::handle_fact_feedback(cg, args).await,
        "tracedecay_memory_status" => {
            memory::handle_memory_status(cg, args, global_db, allow_default_registry_fallback).await
        }
        "tracedecay_dashboard" => dashboard::handle_dashboard(cg, args).await,
        "tracedecay_message_search" => {
            session::handle_message_search(cg, args, global_db, allow_default_registry_fallback)
                .await
        }
        "tracedecay_lcm_status" => {
            session::handle_lcm_status(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_doctor" => {
            session::handle_lcm_doctor(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_load_session" => {
            session::handle_lcm_load_session(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_grep" => {
            session::handle_lcm_grep(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_describe" => {
            session::handle_lcm_describe(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_expand" => {
            session::handle_lcm_expand(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_expand_query" => {
            session::handle_lcm_expand_query(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_preflight" => {
            session::handle_lcm_preflight(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_compress" => {
            session::handle_lcm_compress(session::LcmHandlerContext::active(cg), args).await
        }
        "tracedecay_lcm_session_boundary" => {
            session::handle_lcm_session_boundary(session::LcmHandlerContext::active(cg), args).await
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
        "tracedecay_lcm_status" => {
            session::handle_lcm_status(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_doctor" => {
            session::handle_lcm_doctor(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_load_session" => {
            session::handle_lcm_load_session(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_grep" => {
            session::handle_lcm_grep(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_describe" => {
            session::handle_lcm_describe(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_expand" => {
            session::handle_lcm_expand(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_expand_query" => {
            session::handle_lcm_expand_query(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_preflight" => {
            session::handle_lcm_preflight(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_compress" => {
            session::handle_lcm_compress(session::LcmHandlerContext::projectless(), args).await
        }
        "tracedecay_lcm_session_boundary" => {
            session::handle_lcm_session_boundary(session::LcmHandlerContext::projectless(), args)
                .await
        }
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
    use std::fmt::Write as _;
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

        // These tools are intentionally hidden from the advertised surface when
        // the host ast-grep CLI capability they need is unavailable; mirror the
        // runtime filters so the integrity check covers the actual tools/list
        // surface.
        if !super::super::definitions::ast_grep_available() {
            handler_names.remove("tracedecay_ast_grep_rewrite");
        }
        if !super::super::definitions::ast_grep_outline_available() {
            handler_names.remove("tracedecay_outline");
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
        for tool in get_tool_definitions() {
            let properties = &tool.input_schema["properties"];
            let schema_has_registered_project_selector =
                ["project_selector", "project_id", "project_path"]
                    .iter()
                    .any(|selector_key| properties.get(*selector_key).is_some());
            assert_eq!(
                tool_accepts_registered_project_selector(&tool.name),
                schema_has_registered_project_selector,
                "{} registered-project selector schema and dispatch policy should stay in lockstep",
                tool.name
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
            .expect("target project should be registered")
            .to_string();

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

        active.checkpoint().await.unwrap();
        target.checkpoint().await.unwrap();
        active.close();
        target.close();
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
            let _ = writeln!(
                target_source,
                "pub fn selected_project_handle_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}"
            );
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
                "limit": 420,
                "format": "json"
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
        // ast-grep-backed tools are conditionally registered based on the
        // host CLI capabilities they need; agents should never see a tool that
        // will instantly fail. The count and per-tool checks below adapt to
        // the host's capability set.
        let expected_total = 92
            + usize::from(super::super::definitions::ast_grep_available())
            + usize::from(super::super::definitions::ast_grep_outline_available());
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
        if super::super::definitions::ast_grep_outline_available() {
            assert!(tool_names.contains(&"tracedecay_outline"));
        } else {
            assert!(!tool_names.contains(&"tracedecay_outline"));
        }
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
    fn test_tool_definitions_serializable() {
        let tools = get_tool_definitions();
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains("tracedecay_search"));
        assert!(json.contains("tracedecay_status"));
    }
}
