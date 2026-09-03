use std::fs;

use serde_json::json;
use tempfile::TempDir;

use super::dispatch_test_support::{SelectorEnv, verified_graph_options};
use super::*;
use crate::config::lock_user_data_dir_test_env;

fn graph_handlers_that_await_query() -> &'static [&'static str] {
    &[
        "tracedecay_callers",
        "tracedecay_callees",
        "tracedecay_impact",
        "tracedecay_node",
        "tracedecay_similar",
        "tracedecay_rename_preview",
        "tracedecay_implementations",
        "tracedecay_callers_for",
        "tracedecay_find_exact_symbol",
        "tracedecay_by_qualified_name",
        "tracedecay_signature",
        "tracedecay_impls",
        "tracedecay_derives",
        "tracedecay_files",
        "tracedecay_port_status",
        "tracedecay_port_order",
        "tracedecay_type_hierarchy",
        "tracedecay_body",
        "tracedecay_todos",
        "tracedecay_read",
        "tracedecay_outline",
        "tracedecay_signature_search",
        "tracedecay_dead_code",
        "tracedecay_circular",
        "tracedecay_hotspots",
        "tracedecay_unused_imports",
        "tracedecay_rank",
        "tracedecay_largest",
        "tracedecay_coupling",
        "tracedecay_inheritance_depth",
        "tracedecay_distribution",
        "tracedecay_recursion",
        "tracedecay_complexity",
        "tracedecay_doc_coverage",
        "tracedecay_god_class",
        "tracedecay_unsafe_patterns",
        "tracedecay_constructors",
        "tracedecay_field_sites",
        "tracedecay_diagnostics",
        "tracedecay_affected",
        "tracedecay_diff_context",
        "tracedecay_changelog",
        "tracedecay_commit_context",
        "tracedecay_health",
        "tracedecay_test_map",
        "tracedecay_test_risk",
        "tracedecay_dsm",
        "tracedecay_gini",
        "tracedecay_dependency_depth",
        "tracedecay_redundancy",
        "tracedecay_diagnose",
        "tracedecay_run_affected_tests",
    ]
}

fn lower_level_ports_without_query(cg: &TraceDecay) -> ToolCallRegistryOptions<'_> {
    let mut options = verified_graph_options(cg, ToolCallRegistryOptions::default());
    assert!(
        options.code_graph_read_admission_port.is_some(),
        "fixture must keep lower-level admission"
    );
    assert!(
        options.code_graph_projection_read_port.is_some(),
        "fixture must keep lower-level projection"
    );
    options.verified_graph_query_port = None;
    options
}

/// Initializes the fixture project as a git repository with one commit, so
/// handlers that validate git evidence before awaiting graph admission reach
/// their graph wait instead of reporting the typed git refusal first.
fn init_committed_git_fixture(root: &std::path::Path) {
    for args in [
        &["init", "--initial-branch=main"][..],
        &["add", "."][..],
        &["commit", "-m", "fixture"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The minimal arguments that carry each handler past its request-shape
/// validation, which by contract precedes graph admission: `changelog` proves
/// its git evidence first and `run_affected_tests` requires an explicit
/// caller-scoped manifest, while every other awaiting handler reaches the
/// graph wait with empty arguments.
fn query_authority_probe_args(tool_name: &str) -> serde_json::Value {
    match tool_name {
        "tracedecay_changelog" => json!({ "from_ref": "HEAD", "to_ref": "HEAD" }),
        "tracedecay_run_affected_tests" => json!({ "changed_paths": ["src/lib.rs"] }),
        _ => json!({}),
    }
}

#[tokio::test]
async fn absent_query_port_fails_closed_for_every_awaiting_graph_handler() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("authority isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("query-port-absent");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn widget() {}\n").expect("write fixture");
    init_committed_git_fixture(&project);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.query-port-absent",
    )
    .await
    .expect("registered fixture");

    let options = lower_level_ports_without_query(&cg);
    let mut seen = 0usize;
    for tool_name in graph_handlers_that_await_query() {
        let error = handle_tool_call_with_registry_options(
            &cg,
            tool_name,
            query_authority_probe_args(tool_name),
            None,
            None,
            options.clone(),
        )
        .await
        .expect_err(tool_name);
        let (reason_code, retryable, detail) = error
            .project_route_context()
            .unwrap_or_else(|| panic!("{tool_name} must be a typed project route, got {error}"));
        assert_eq!(
            reason_code, "verified-code-graph-read-unavailable",
            "{tool_name}"
        );
        assert!(!retryable, "{tool_name}");
        assert_eq!(
            detail, "the exact project verified graph query is not mounted",
            "{tool_name}"
        );
        seen += 1;
    }
    assert_eq!(seen, graph_handlers_that_await_query().len());
    cg.close();
}

#[tokio::test]
async fn search_and_context_report_absent_query_port_as_typed_evidence() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("authority isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("query-port-absent-search");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn widget() {}\n").expect("write fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.query-port-absent-search",
    )
    .await
    .expect("registered fixture");

    let options = lower_level_ports_without_query(&cg);
    for tool_name in ["tracedecay_search", "tracedecay_context"] {
        let args = if tool_name == "tracedecay_search" {
            json!({ "query": "widget", "limit": 1, "format": "json" })
        } else {
            json!({
                "task": "explain widget",
                "include_memory": false,
                "format": "json",
            })
        };
        let error = handle_tool_call_with_registry_options(
            &cg,
            tool_name,
            args,
            None,
            None,
            options.clone(),
        )
        .await;
        match error {
            Ok(result) => {
                let payload: serde_json::Value = serde_json::from_str(
                    result.value["content"][0]["text"]
                        .as_str()
                        .expect("json text"),
                )
                .expect("payload");
                assert_eq!(
                    payload["verified_graph_evidence"]["reason_code"],
                    "verified-code-graph-read-unavailable",
                    "{tool_name}"
                );
            }
            Err(error) => {
                let (reason_code, retryable, _) = error
                    .project_route_context()
                    .unwrap_or_else(|| panic!("{tool_name} must be typed, got {error}"));
                assert_eq!(reason_code, "verified-code-graph-read-unavailable");
                assert!(!retryable);
            }
        }
    }
    cg.close();
}
