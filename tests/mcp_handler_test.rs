//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Each test exercises a real `TokenSave` instance with indexed test data,
//! ensuring that the MCP dispatch layer formats results correctly.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::Path;

use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::db::Database;
use tokensave::global_db::GlobalDb;
use tokensave::mcp::{get_tool_definitions, handle_tool_call};
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::lcm::{
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmSourceRef, LcmSummaryNodeDraft,
};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};
use tokensave::tokensave::TokenSave;

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

/// Creates a temporary Rust project with cross-file calls, structs, impls,
/// test files, and doc comments, then initialises and indexes a `TokenSave`.
async fn setup_project() -> (TokenSave, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/utils.rs"),
        r#"
/// Returns a greeting string.
pub fn helper() -> String {
    format_greeting("world")
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    // Test file so affected-tests can find something
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests/test_utils.rs"),
        r#"
use crate::utils::helper;

#[test]
fn test_helper() { assert!(!helper().is_empty()); }
"#,
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    (cg, dir)
}

/// Extracts the text content from a `ToolResult` value (the standard
/// `content[0].text` envelope).
fn extract_text(value: &Value) -> &str {
    value["content"][0]["text"]
        .as_str()
        .unwrap_or("<missing text>")
}

fn expect_tool_error<T>(result: tokensave::errors::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected tool call to fail"),
        Err(err) => format!("{err}"),
    }
}

#[test]
fn lcm_tool_schemas_are_registered_with_stable_names() {
    let tools = get_tool_definitions();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for expected in [
        "tokensave_lcm_status",
        "tokensave_lcm_load_session",
        "tokensave_lcm_grep",
        "tokensave_lcm_describe",
        "tokensave_lcm_expand",
        "tokensave_lcm_expand_query",
        "tokensave_lcm_preflight",
        "tokensave_lcm_compress",
        "tokensave_lcm_doctor",
    ] {
        assert!(names.contains(expected), "missing {expected}");
    }

    for read_only in [
        "tokensave_lcm_status",
        "tokensave_lcm_load_session",
        "tokensave_lcm_grep",
        "tokensave_lcm_describe",
        "tokensave_lcm_expand",
        "tokensave_lcm_expand_query",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == read_only)
            .unwrap_or_else(|| panic!("{read_only} definition"));
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], true);
    }

    for mutating in [
        "tokensave_lcm_preflight",
        "tokensave_lcm_compress",
        "tokensave_lcm_doctor",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == mutating)
            .unwrap_or_else(|| panic!("{mutating} definition"));
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], false);
    }

    for scoped in [
        "tokensave_lcm_status",
        "tokensave_lcm_load_session",
        "tokensave_lcm_grep",
        "tokensave_lcm_describe",
        "tokensave_lcm_expand",
        "tokensave_lcm_expand_query",
        "tokensave_lcm_preflight",
        "tokensave_lcm_compress",
        "tokensave_lcm_doctor",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == scoped)
            .unwrap_or_else(|| panic!("{scoped} definition"));
        let storage_scope = &tool.input_schema["properties"]["storage_scope"];
        assert_eq!(
            storage_scope["enum"],
            json!(["project_local", "hermes_profile"]),
            "{scoped} must advertise supported storage scopes"
        );
        assert!(
            storage_scope["description"]
                .as_str()
                .unwrap_or_default()
                .contains("hermes_home"),
            "{scoped} storage_scope should document hermes_profile requirements"
        );
        let hermes_home = &tool.input_schema["properties"]["hermes_home"];
        assert_eq!(
            hermes_home["type"],
            json!("string"),
            "{scoped} must expose hermes_home"
        );
        assert!(
            hermes_home["description"]
                .as_str()
                .unwrap_or_default()
                .contains("absolute"),
            "{scoped} hermes_home should document absolute path requirements"
        );
    }

    let load = tools
        .iter()
        .find(|tool| tool.name == "tokensave_lcm_load_session")
        .expect("tokensave_lcm_load_session definition");
    assert_eq!(load.input_schema["required"], json!(["session_id"]));
    assert!(load.input_schema["properties"]
        .get("content_limit")
        .is_some());
    assert_eq!(
        load.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );
    assert_eq!(
        load.input_schema["properties"]["content_limit"]["maximum"],
        json!(20000)
    );

    let grep = tools
        .iter()
        .find(|tool| tool.name == "tokensave_lcm_grep")
        .expect("tokensave_lcm_grep definition");
    assert_eq!(
        grep.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );

    let expand = tools
        .iter()
        .find(|tool| tool.name == "tokensave_lcm_expand")
        .expect("tokensave_lcm_expand definition");
    assert_eq!(
        expand.input_schema["required"],
        json!(["session_id", "target"])
    );
    assert!(expand.input_schema["properties"].get("target").is_some());
    assert_eq!(
        expand.input_schema["properties"]["target"]["properties"]["store_id"]["type"],
        json!("integer")
    );

    let doctor = tools
        .iter()
        .find(|tool| tool.name == "tokensave_lcm_doctor")
        .expect("tokensave_lcm_doctor definition");
    assert_eq!(
        doctor.input_schema["properties"]["mode"]["enum"],
        json!(["diagnose", "repair", "retention", "clean"])
    );
    assert_eq!(
        doctor.input_schema["properties"]["apply"]["type"],
        json!("boolean")
    );
}

/// Searches for `name` via the search handler and returns the first matching
/// node id whose name field equals `name`.
async fn find_node_id(cg: &TokenSave, name: &str) -> String {
    let result = handle_tool_call(cg, "tokensave_search", json!({"query": name}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let items: Vec<Value> = serde_json::from_str(text).unwrap();
    items
        .iter()
        .find(|item| item["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("node '{}' not found via search", name))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// 1. tokensave_search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_search() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_search",
        json!({"query": "helper", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    assert!(
        text.contains("helper"),
        "search results should contain 'helper'"
    );
}

// ---------------------------------------------------------------------------
// 2. tokensave_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_context",
        json!({"task": "understand the helper function"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 3. tokensave_callers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_callers",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 4. tokensave_callees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callees() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_callees",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 5. tokensave_impact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_impact() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_impact",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("node_count"));
}

// ---------------------------------------------------------------------------
// 6. tokensave_node — existing node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_existing() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_node",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("helper"),
        "node detail should contain the name"
    );
    assert!(
        text.contains("start_line"),
        "node detail should contain start_line"
    );
    assert!(
        text.contains("signature"),
        "node detail should contain signature"
    );
    assert!(
        text.contains("visibility"),
        "node detail should contain visibility"
    );
}

// ---------------------------------------------------------------------------
// 7. tokensave_node — nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_not_found() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_node",
        json!({"node_id": "nonexistent_id_12345"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("Node not found"),
        "should report 'Node not found', got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// 8. tokensave_status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_status",
        json!({}),
        Some(json!({"uptime": 100})),
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("node_count"),
        "status should include node_count"
    );
    assert!(
        text.contains("server"),
        "status should include server stats"
    );
}

// ---------------------------------------------------------------------------
// 9. tokensave_files — no filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_no_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_files", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty(), "files listing should not be empty");
    assert!(
        text.contains("indexed files"),
        "should have 'indexed files' header"
    );
}

// ---------------------------------------------------------------------------
// 10. tokensave_files — path filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_files", json!({"path": "src"}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // The test file lives under tests/, so if path filter works it should
    // only contain src/ files.
    assert!(
        !text.contains("tests/test_utils"),
        "path filter should exclude files outside 'src'"
    );
}

// ---------------------------------------------------------------------------
// 11. tokensave_files — pattern filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_pattern_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_files",
        json!({"pattern": "*.rs"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 12. tokensave_files — flat format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_flat_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_files",
        json!({"format": "flat"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // Flat format includes "bytes" per entry
    assert!(text.contains("bytes"), "flat format should show byte sizes");
}

// ---------------------------------------------------------------------------
// 13. tokensave_affected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_affected",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("affected_tests"),
        "should have affected_tests key"
    );
    assert!(text.contains("count"), "should have count key");
}

// ---------------------------------------------------------------------------
// 14. tokensave_dead_code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("dead_code_count"),
        "should have dead_code_count key"
    );
}

// ---------------------------------------------------------------------------
// 15. tokensave_diff_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_diff_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_diff_context",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("changed_files"),
        "should have changed_files key"
    );
    assert!(
        text.contains("modified_symbols"),
        "should have modified_symbols key"
    );
}

// ---------------------------------------------------------------------------
// 16. tokensave_module_api
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_module_api() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_module_api",
        json!({"path": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("public_symbol_count"),
        "should have public_symbol_count key"
    );
    // helper is pub so it should appear
    assert!(
        text.contains("helper"),
        "pub fn helper should appear in module API"
    );
}

// ---------------------------------------------------------------------------
// 17. tokensave_circular
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_circular() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 18. tokensave_hotspots
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hotspots() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_hotspots", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("hotspot_count"),
        "should have hotspot_count key"
    );
}

// ---------------------------------------------------------------------------
// 19. tokensave_similar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_similar() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_similar",
        json!({"symbol": "helper"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    assert!(
        text.contains("helper"),
        "similar results should include 'helper'"
    );
}

// ---------------------------------------------------------------------------
// 20. tokensave_rename_preview
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_preview() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rename_preview",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("reference_count"),
        "should have reference_count key"
    );
    assert!(text.contains("node"), "should have node key");
}

// ---------------------------------------------------------------------------
// 21. tokensave_unused_imports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unused_imports() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("unused_import_count"),
        "should have unused_import_count key"
    );
}

// ---------------------------------------------------------------------------
// 22. tokensave_rank
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rank",
        json!({"edge_kind": "calls", "direction": "incoming"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 23. tokensave_rank — invalid direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank_invalid_direction() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rank",
        json!({"edge_kind": "calls", "direction": "sideways"}),
        None,
        None,
    )
    .await;
    match result {
        Err(err) => {
            let err_msg = format!("{}", err);
            assert!(
                err_msg.contains("invalid direction"),
                "error should mention 'invalid direction', got: {}",
                err_msg,
            );
        }
        Ok(_) => panic!("invalid direction should produce an error"),
    }
}

// ---------------------------------------------------------------------------
// 24. tokensave_largest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_largest() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_largest", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 25. tokensave_coupling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coupling() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_coupling",
        json!({"direction": "fan_in"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
}

// ---------------------------------------------------------------------------
// 26. tokensave_inheritance_depth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_inheritance_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_inheritance_depth",
        json!({"limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 27. tokensave_distribution — default and summary mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_default() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_distribution", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("per_file"), "default mode should be per_file");
}

#[tokio::test]
async fn test_distribution_summary() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_distribution",
        json!({"summary": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("summary"),
        "summary mode should report 'summary'"
    );
    assert!(
        text.contains("distribution"),
        "should have distribution key"
    );
}

// ---------------------------------------------------------------------------
// 28. tokensave_recursion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_recursion() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 29. tokensave_complexity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(text.contains("formula"), "should have formula key");
}

// ---------------------------------------------------------------------------
// 30. tokensave_doc_coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_doc_coverage", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("total_undocumented"),
        "should have total_undocumented key"
    );
}

// ---------------------------------------------------------------------------
// 31. tokensave_god_class
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_god_class() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_god_class", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 32. tokensave_changelog — requires git refs, expect graceful error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_changelog_no_git() {
    let (cg, _dir) = setup_project().await;
    // The temp dir is not a git repo, so this should return a "git diff failed"
    // message rather than a hard error.
    let result = handle_tool_call(
        &cg,
        "tokensave_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("git diff failed"),
        "changelog on non-git dir should report git diff failure, got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// 33. tokensave_port_status — no matching dirs expected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_status() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_port_status",
        json!({"source_dir": "src", "target_dir": "tests"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("coverage_percent"),
        "should have coverage_percent key"
    );
}

/// Regression: port_status used to match symbols purely on (name,
/// kind_compat_group), so common method names like `new`, `process`, `fmt`,
/// or `reset` produced wild cross-type "matches" — e.g. `Biquad::new` would
/// pair with an unrelated `Adaa::new` simply because both methods are named
/// "new". The match key must also include the parent type so siblings of
/// distinct owners stay unmatched.
#[tokio::test]
async fn port_status_does_not_match_methods_of_different_parents() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src_a")).unwrap();
    fs::create_dir_all(project.join("src_b")).unwrap();

    fs::write(
        project.join("src_a/biquad.rs"),
        "pub struct Biquad;\n\
         impl Biquad {\n    pub fn new() -> Self { Self }\n    pub fn process(&self) {}\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src_b/adaa.rs"),
        "pub struct Adaa;\n\
         impl Adaa {\n    pub fn new() -> Self { Self }\n    pub fn process(&self) {}\n}\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_port_status",
        json!({
            "source_dir": "src_a",
            "target_dir": "src_b",
            "kinds": ["method"],
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be JSON");
    let matched: Vec<&Value> = output["matched_symbols"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    // None of the source methods should match because the parent types differ.
    assert!(
        matched.is_empty(),
        "Biquad::* and Adaa::* must not cross-match — got matches: {matched:?}"
    );
    assert_eq!(
        output["matched"].as_u64(),
        Some(0),
        "matched count must be 0; output={output}"
    );
}

/// Sanity: when the same parent type name exists in both dirs, methods do
/// match — confirming the parent-aware key isn't too strict.
#[tokio::test]
async fn port_status_matches_methods_with_same_parent_type() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src_a")).unwrap();
    fs::create_dir_all(project.join("src_b")).unwrap();

    fs::write(
        project.join("src_a/biquad.rs"),
        "pub struct Biquad;\n\
         impl Biquad { pub fn process(&self) {} }\n",
    )
    .unwrap();
    fs::write(
        project.join("src_b/biquad_port.rs"),
        "pub struct Biquad;\n\
         impl Biquad { pub fn process(&self) {} }\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_port_status",
        json!({
            "source_dir": "src_a",
            "target_dir": "src_b",
            "kinds": ["method"],
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be JSON");
    assert_eq!(
        output["matched"].as_u64(),
        Some(1),
        "Biquad::process should match Biquad::process; output={output}"
    );
}

// ---------------------------------------------------------------------------
// 34. tokensave_port_order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_order() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("total_symbols"),
        "should have total_symbols key"
    );
    assert!(text.contains("levels"), "should have levels key");
}

// ---------------------------------------------------------------------------
// 35. Unknown tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unknown_tool() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_unknown", json!({}), None, None).await;
    match result {
        Err(err) => {
            let err_msg = format!("{}", err);
            assert!(
                err_msg.contains("unknown tool"),
                "error should mention 'unknown tool', got: {}",
                err_msg,
            );
        }
        Ok(_) => panic!("unknown tool should produce an error"),
    }
}

// ---------------------------------------------------------------------------
// 36. Missing required params — search without query
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_missing_required_params() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_search", json!({}), None, None).await;
    let err_msg = match result {
        Err(err) => format!("{}", err),
        Ok(_) => panic!("missing query should produce an error"),
    };
    assert!(
        err_msg.contains("missing required parameter"),
        "error should mention 'missing required parameter', got: {}",
        err_msg,
    );
}

// ---------------------------------------------------------------------------
// 37. Node ID alias — using "id" instead of "node_id"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_id_alias() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    // Use "id" instead of "node_id"
    let result = handle_tool_call(&cg, "tokensave_node", json!({"id": node_id}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("helper"),
        "node lookup via 'id' alias should still find the node"
    );
}

// ---------------------------------------------------------------------------
// Extra: tokensave_status without server_stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status_without_server_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("node_count"),
        "status should include node_count"
    );
    // Should NOT contain "server" key when None is passed
    assert!(
        !text.contains("\"server\""),
        "status without server_stats should not include 'server' key"
    );
}

// ---------------------------------------------------------------------------
// Extra: touched_files populated for search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_search_populates_touched_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_search",
        json!({"query": "helper"}),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !result.touched_files.is_empty(),
        "search results should populate touched_files"
    );
}

// ---------------------------------------------------------------------------
// Extra: rename_preview with nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_preview_not_found() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rename_preview",
        json!({"node_id": "nonexistent_id_12345"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("Node not found"),
        "rename_preview with bad id should report 'Node not found', got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// Extra: coupling with fan_out direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coupling_fan_out() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_coupling",
        json!({"direction": "fan_out"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("fan_out"), "should report fan_out direction");
}

// ---------------------------------------------------------------------------
// Extra: rank with outgoing direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank_outgoing() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rank",
        json!({"edge_kind": "calls", "direction": "outgoing"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("outgoing"),
        "should reflect outgoing direction"
    );
}

// ---------------------------------------------------------------------------
// Extra: missing required params for other handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context_missing_task() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_context", json!({}), None, None).await;
    assert!(result.is_err(), "context without task should error");
}

#[tokio::test]
async fn test_callers_missing_node_id() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_callers", json!({}), None, None).await;
    assert!(result.is_err(), "callers without node_id should error");
}

#[tokio::test]
async fn test_affected_missing_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_affected", json!({}), None, None).await;
    assert!(result.is_err(), "affected without files should error");
}

#[tokio::test]
async fn test_module_api_missing_path() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_module_api", json!({}), None, None).await;
    assert!(result.is_err(), "module_api without path should error");
}

#[tokio::test]
async fn test_rank_missing_edge_kind() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rank",
        json!({"direction": "incoming"}),
        None,
        None,
    )
    .await;
    assert!(result.is_err(), "rank without edge_kind should error");
}

#[tokio::test]
async fn test_similar_missing_symbol() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_similar", json!({}), None, None).await;
    assert!(result.is_err(), "similar without symbol should error");
}

#[tokio::test]
async fn test_diff_context_missing_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_diff_context", json!({}), None, None).await;
    assert!(result.is_err(), "diff_context without files should error");
}

#[tokio::test]
async fn test_changelog_missing_refs() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_changelog", json!({}), None, None).await;
    assert!(result.is_err(), "changelog without from_ref should error");
}

#[tokio::test]
async fn test_port_status_missing_dirs() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_port_status", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_status without source_dir should error"
    );
}

#[tokio::test]
async fn test_port_order_missing_source_dir() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_port_order", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_order without source_dir should error"
    );
}

// ---------------------------------------------------------------------------
// Extra: tokensave_changelog with a real git repo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_changelog_with_real_git() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // Initialize git repo and make a first commit
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(project)
        .output()
        .expect("git init failed");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(project)
        .output()
        .unwrap();

    fs::write(project.join("src/lib.rs"), "pub fn original() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(project)
        .output()
        .unwrap();

    // Make a second commit with changes
    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn added() {}\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "add function"])
        .current_dir(project)
        .output()
        .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    // Should not report "git diff failed" since it's a real git repo
    assert!(
        !text.contains("git diff failed"),
        "changelog in git repo should not fail, got: {}",
        text,
    );
    assert!(
        text.contains("changed_file_count") || text.contains("lib.rs"),
        "changelog should mention changed files, got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// Extra: tokensave_distribution with path prefix filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_with_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_distribution",
        json!({"path": "src/"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("per_file"), "default mode should be per_file");
    // Should only contain src/ files, not tests/
    assert!(
        !text.contains("tests/test_utils"),
        "path filter should exclude files outside 'src/'",
    );
}

// ---------------------------------------------------------------------------
// Extra: tokensave_files — grouped format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_grouped_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_files",
        json!({"format": "grouped"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // Grouped format shows directory headers like "src/ (N files)"
    assert!(
        text.contains("indexed files"),
        "grouped format should have 'indexed files' header"
    );
    assert!(
        text.contains("files)"),
        "grouped format should show file counts per directory"
    );
}

// ---------------------------------------------------------------------------
// Extra: tokensave_dead_code with custom kinds parameter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code_custom_kinds() {
    let (cg, _dir) = setup_project().await;
    // Ask only for struct dead code
    let result = handle_tool_call(
        &cg,
        "tokensave_dead_code",
        json!({"kinds": ["struct"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("dead_code_count"),
        "should have dead_code_count key"
    );
    // Parse and verify any returned items are structs
    let parsed: Value = serde_json::from_str(text).unwrap_or(json!({}));
    if let Some(items) = parsed["dead_code"].as_array() {
        for item in items {
            assert_eq!(
                item["kind"].as_str().unwrap_or(""),
                "struct",
                "dead code items should be structs when kinds=['struct']"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Extra: tokensave_affected with custom filter glob
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected_with_custom_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_affected",
        json!({"files": ["src/utils.rs"], "filter": "**/*test*"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("affected_tests"),
        "should have affected_tests key"
    );
    assert!(text.contains("count"), "should have count key");
}

// ---------------------------------------------------------------------------
// Extra: tokensave_complexity — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity_response_fields() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("ranking").is_some(), "should have ranking key");
    assert!(parsed.get("formula").is_some(), "should have formula key");
    // Check ranking items have expected fields
    if let Some(items) = parsed["ranking"].as_array() {
        if let Some(first) = items.first() {
            assert!(
                first.get("cyclomatic_complexity").is_some(),
                "ranking item should have cyclomatic_complexity"
            );
            assert!(
                first.get("branches").is_some(),
                "ranking item should have branches"
            );
            assert!(
                first.get("max_nesting").is_some(),
                "ranking item should have max_nesting"
            );
            assert!(
                first.get("fan_out").is_some(),
                "ranking item should have fan_out"
            );
            assert!(
                first.get("score").is_some(),
                "ranking item should have score"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Extra: tokensave_doc_coverage — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage_response_structure() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_doc_coverage", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("total_undocumented").is_some(),
        "should have total_undocumented"
    );
    assert!(parsed.get("file_count").is_some(), "should have file_count");
    assert!(parsed.get("files").is_some(), "should have files array");
    // If there are files, check their structure
    if let Some(files) = parsed["files"].as_array() {
        if let Some(first) = files.first() {
            assert!(first.get("file").is_some(), "file entry should have 'file'");
            assert!(
                first.get("count").is_some(),
                "file entry should have 'count'"
            );
            assert!(
                first.get("symbols").is_some(),
                "file entry should have 'symbols'"
            );
        }
    }
}

#[tokio::test]
async fn test_files_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // With scope_prefix "src", should only return files under src/
    let result = handle_tool_call(&cg, "tokensave_files", json!({}), None, Some("src"))
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.contains("tests/"),
        "scope_prefix 'src' should exclude test files"
    );
    assert!(text.contains("main.rs"), "should include src/main.rs");
}

#[tokio::test]
async fn test_search_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // Search for "helper" but scoped to "tests" — should only return test file results
    let result = handle_tool_call(
        &cg,
        "tokensave_search",
        json!({"query": "helper", "limit": 20}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Vec<serde_json::Value> = serde_json::from_str(text).unwrap_or_default();
    for item in &items {
        let file = item["file"].as_str().unwrap_or("");
        assert!(
            file.starts_with("tests"),
            "scoped search should only return files under 'tests', got: {}",
            file
        );
    }
}

#[tokio::test]
async fn test_files_explicit_path_overrides_scope() {
    let (cg, _dir) = setup_project().await;
    // Explicit path "tests" should override scope_prefix "src"
    let result = handle_tool_call(
        &cg,
        "tokensave_files",
        json!({"path": "tests"}),
        None,
        Some("src"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.contains("src/main.rs"),
        "explicit path 'tests' should exclude src files"
    );
}

#[tokio::test]
async fn test_context_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // Context scoped to "tests" should return results (even if limited to test files)
    let result = handle_tool_call(
        &cg,
        "tokensave_context",
        json!({"task": "understand helper"}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.is_empty(),
        "context should return results even when scoped"
    );
}

#[tokio::test]
async fn test_status_reports_scope_prefix() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_status", json!({}), None, Some("src/mcp"))
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("scope_prefix"),
        "status should report scope_prefix"
    );
    assert!(
        text.contains("src/mcp"),
        "status should show the actual prefix value"
    );
}

#[tokio::test]
async fn test_status_no_scope_prefix() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("scope_prefix").is_none() || parsed["scope_prefix"].is_null(),
        "status should not have scope_prefix when None"
    );
}

// ---------------------------------------------------------------------------
// Edit tools: tokensave_str_replace, tokensave_multi_str_replace, tokensave_insert_at
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_str_replace_success() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn hello() {}\nfn world() {}\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn hello() {}",
            "new_str": "fn hello_updated() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["matched_str"], "fn hello() {}");
    assert_eq!(parsed["new_str"], "fn hello_updated() {}");

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn hello_updated() {}"));
    assert!(!content.contains("fn hello() {}"));
}

#[tokio::test]
async fn test_str_replace_not_found() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn hello() {}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn not_exists() {}",
            "new_str": "fn replaced() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_str_replace_multiple_matches_fails() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn foo() {}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn foo() {}",
            "new_str": "fn bar() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("matches 2 times"));
}

#[tokio::test]
async fn test_multi_str_replace_success() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn foo() {}\nfn bar() {}\nfn baz() {}\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn foo() {}", "fn foo_replaced() {}"],
                ["fn bar() {}", "fn bar_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo_replaced()"));
    assert!(content.contains("fn bar_replaced()"));
    assert!(content.contains("fn baz() {}"));
}

#[tokio::test]
async fn test_multi_str_replace_atomic_failure() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn baz() {}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn not_exists() {}", "fn replaced() {}"],
                ["fn baz() {}", "fn baz_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("must match exactly once"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo() {}"));
    assert!(content.contains("fn baz() {}"));
    assert!(!content.contains("fn replaced()"));
}

#[tokio::test]
async fn test_multi_str_replace_unicode_preview_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn main() {}\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let missing_old = format!("{}é", "a".repeat(19));
    let result = handle_tool_call(
        &cg,
        "tokensave_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                [missing_old, "replacement"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    let message = parsed["message"].as_str().unwrap();
    assert!(message.contains("matches 0 times"));
    assert!(message.contains("must match exactly once"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_str_replace_unsupported_file_type_succeeds() {
    // Regression: editing unsupported types (e.g. .css) previously wrote the
    // file then returned a reindex error, silently mutating the file.
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    fs::write(project.join("style.css"), ".foo {\n\tfont-size: 14px;\n}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_str_replace",
        json!({
            "path": "style.css",
            "old_str": "\tfont-size: 14px;",
            "new_str": "\tfont-size: 0.85rem;"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(!content.contains("14px"));
}

#[tokio::test]
async fn ast_grep_rewrite_has_literal_fallback_when_binary_missing() {
    if tokensave::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn old_name() {}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name", "rewrite": "new_name"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    assert!(
        fs::read_to_string(project.join("src/lib.rs"))
            .unwrap()
            .contains("new_name"),
        "literal fallback should update the file"
    );
}

#[tokio::test]
async fn ast_grep_rewrite_uses_current_cli_update_flag() {
    if !tokensave::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn caller() { old_name(); }\npub fn old_name() {}\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name()", "rewrite": "new_name()"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.contains("new_name();"),
        "ast-grep rewrite should apply with the installed CLI: {content}"
    );
    assert!(
        !output["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unexpected argument '-d'"),
        "rewrite must not use the removed -d flag: {output}"
    );
}

/// Regression: `branch_diff` previously errored with `MCP error -32603: base
/// and head are the same branch` when base == head. `pr_context` handles the
/// same case gracefully (empty arrays); branch_diff must match that shape so
/// callers can rely on consistent behaviour.
#[tokio::test]
async fn branch_diff_returns_empty_when_base_equals_head() {
    let (cg, _dir) = setup_project().await;

    // branch_diff requires branch tracking metadata to be present.
    let tokensave_dir = tokensave::config::get_tokensave_dir(cg.project_root());
    let meta = tokensave::branch_meta::BranchMeta::new("master");
    tokensave::branch_meta::save_branch_meta(&tokensave_dir, &meta).unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_branch_diff",
        json!({"base": "master", "head": "master"}),
        None,
        None,
    )
    .await
    .expect("branch_diff must not error when base == head");

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be valid JSON");
    assert_eq!(output["summary"]["added"].as_u64(), Some(0));
    assert_eq!(output["summary"]["removed"].as_u64(), Some(0));
    assert_eq!(output["summary"]["changed"].as_u64(), Some(0));
    assert_eq!(output["added"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["changed"].as_array().map(Vec::len), Some(0));
}

/// Regression: when ast-grep exits non-zero with empty stderr (no language
/// inferred from the file extension, or pattern matches nothing), the tool
/// used to surface `"ast-grep failed: "` — a useless empty trailer. The
/// message must instead explain the likely cause so the caller can act on it.
#[tokio::test]
async fn ast_grep_rewrite_surfaces_useful_error_on_empty_stderr() {
    if !tokensave::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn foo() {}\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_ast_grep_rewrite",
        json!({
            "path": "src/lib.rs",
            "pattern": "__NONEXISTENT_PATTERN__",
            "rewrite": "whatever"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(false), "{output}");
    let message = output["message"].as_str().unwrap_or_default();
    assert!(
        !message.trim_end_matches(':').trim().eq("ast-grep failed"),
        "message must not end as an empty 'ast-grep failed:' — got: {message:?}"
    );
    assert!(
        message.contains("exit") || message.contains("0 nodes") || message.contains("no language"),
        "message must explain the likely cause (exit code / no language / 0 matches), got: {message:?}"
    );
}

#[tokio::test]
async fn test_multi_str_replace_unsupported_file_type_succeeds() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    fs::write(
        project.join("style.css"),
        ".foo {\n\tfont-size: 14px;\n}\n.bar {\n\tfont-size: 16px;\n}\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_multi_str_replace",
        json!({
            "path": "style.css",
            "replacements": [
                ["\tfont-size: 14px;", "\tfont-size: 0.85rem;"],
                ["\tfont-size: 16px;", "\tfont-size: 1rem;"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(content.contains("1rem"));
    assert!(!content.contains("14px"));
    assert!(!content.contains("16px"));
}

#[tokio::test]
async fn test_insert_at_string_anchor_before() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "line two",
            "content": "inserted line",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "inserted line");
    assert_eq!(lines[2], "line two");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_line_number() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "2",
            "content": "inserted at line 2",
            "before": false
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["anchor_line"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "line two");
    assert_eq!(lines[2], "inserted at line 2");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_anchor_not_found() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "line one\nline two\n").unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "nonexistent",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_insert_at_unicode_anchor_prefix_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "line one\nline two\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let long_anchor = format!("{}é", "a".repeat(99));
    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": long_anchor,
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_insert_at_ambiguous_anchor() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line foo\nline foo\nline bar\n",
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "foo",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("matches 2 lines"));
}

// Regression: insert_at must not strip trailing newline (#57)
#[tokio::test]
async fn test_insert_at_preserves_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn hello() {}\n\nfn world() {}\n";
    fs::write(project.join("src/lib.rs"), original).unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_insert_at",
        json!({
            "path": "src/lib.rs",
            "anchor": "fn world",
            "content": "fn extra() {}",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "file must end with newline after insert_at, got: {:?}",
        &content[content.len().saturating_sub(20)..]
    );
    assert_eq!(content, "fn hello() {}\n\nfn extra() {}\nfn world() {}\n");
}

// ---------------------------------------------------------------------------
// tokensave_gini
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_gini() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_gini",
        json!({ "metric": "lines" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("gini").is_some(),
        "gini field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("interpretation").is_some(),
        "interpretation field should exist"
    );
}

#[tokio::test]
async fn test_gini_default_metric() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_gini", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("gini").is_some(),
        "gini field should exist with default args, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// tokensave_dependency_depth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dependency_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_dependency_depth",
        json!({ "limit": 5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("max_depth").is_some(),
        "max_depth field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("ideal_depth").is_some(),
        "ideal_depth field should exist"
    );
}

// ---------------------------------------------------------------------------
// tokensave_health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_health_summary() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_health", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("quality_signal").is_some(),
        "quality_signal field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("files_analyzed").is_some(),
        "files_analyzed field should exist"
    );
}

#[tokio::test]
async fn test_health_detailed() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_health",
        json!({ "details": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("quality_signal").is_some(),
        "quality_signal should exist, got: {}",
        text
    );
    let dims = parsed.get("dimensions").expect("dimensions should exist");
    assert!(dims.get("acyclicity").is_some(), "acyclicity score missing");
    assert!(dims.get("depth").is_some(), "depth score missing");
    assert!(dims.get("equality").is_some(), "equality score missing");
    assert!(dims.get("redundancy").is_some(), "redundancy score missing");
    assert!(dims.get("modularity").is_some(), "modularity score missing");
}

/// Issue #83: tokensave_redundancy must surface AST-isomorphic duplicate
/// pairs and rank them by composite similarity. Plant two structurally
/// identical functions in a fixture and assert the pair surfaces in the
/// top hit with the `definite` severity bucket.
#[tokio::test]
async fn test_redundancy_finds_planted_duplicate() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // Two functions: identical structure, renamed identifiers.
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn compute_a(value: i32) -> i32 {
    let mut acc = 0;
    for i in 0..value {
        if i % 2 == 0 {
            acc += i;
        } else {
            acc -= i;
        }
    }
    acc
}

pub fn compute_b(input: i32) -> i32 {
    let mut total = 0;
    for j in 0..input {
        if j % 2 == 0 {
            total += j;
        } else {
            total -= j;
        }
    }
    total
}

pub fn unrelated(x: i32) -> i32 {
    x * 2
}
"#,
    )
    .unwrap();

    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    let pair_count = parsed["pair_count"].as_u64().unwrap_or(0);
    assert!(
        pair_count >= 1,
        "expected at least 1 duplicate pair, got: {text}"
    );

    let pairs = parsed["pairs"].as_array().expect("pairs array");
    let top = &pairs[0];
    let kind = top["overlap_kind"].as_str().unwrap_or("");
    assert_eq!(
        kind, "ast_isomorphic",
        "top pair should be AST-isomorphic; full output: {text}"
    );
    let severity = top["severity"].as_str().unwrap_or("");
    assert_eq!(
        severity, "definite",
        "AST-identical pair should be 'definite'"
    );
    let names: Vec<&str> = vec![
        top["a"]["name"].as_str().unwrap_or(""),
        top["b"]["name"].as_str().unwrap_or(""),
    ];
    assert!(
        names.contains(&"compute_a") && names.contains(&"compute_b"),
        "expected compute_a/compute_b in pair, got {names:?}"
    );

    // Calling again should be a cache hit (no panic, same result).
    let result2 = handle_tool_call(
        &cg,
        "tokensave_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(extract_text(&result2.value)).unwrap();
    assert_eq!(parsed2["pair_count"], parsed["pair_count"]);
}

/// Issue #80: `tokensave_runtime` must surface process + DB telemetry so
/// users hitting unexpected CPU/RAM can capture a structured snapshot
/// without leaving the chat session.
#[tokio::test]
async fn test_runtime_snapshot_exposes_process_and_db_signals() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_runtime", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    // Top-level envelope.
    assert!(parsed.get("captured_at").is_some());
    assert!(parsed["tokensave_version"].is_string());
    assert!(parsed["host_os"].is_string());

    // Process block — PID must match our own.
    let proc = &parsed["process"];
    assert_eq!(
        proc["pid"].as_u64().unwrap_or(0),
        u64::from(std::process::id()),
        "snapshot must report this process's PID"
    );
    assert!(
        proc["rss_bytes"].as_u64().unwrap_or(0) > 0,
        "RSS should be non-zero"
    );
    assert!(proc["system_cpu_count"].as_u64().unwrap_or(0) >= 1);
    assert!(proc["system_total_memory_bytes"].as_u64().unwrap_or(0) > 0);

    // Database block — the DB file we just opened must be present and sized.
    let db = &parsed["database"];
    assert!(db["db_path"].is_string());
    assert!(
        db["db_size_bytes"].as_u64().unwrap_or(0) > 0,
        "DB file should have non-zero size"
    );
    assert!(
        db["node_count"].as_u64().unwrap_or(0) > 0,
        "fixture indexed > 0 nodes"
    );
    // journal_mode pragma should be readable on a libsql connection.
    assert!(db["journal_mode"].is_string() || db["journal_mode"].is_null());
}

/// Issue #82: `details=true` must surface raw counts + interpretation per
/// dimension, not just the scalar score, so callers don't have to compose
/// six separate tools to reproduce the breakdown.
#[tokio::test]
async fn test_health_detailed_includes_raw_signals() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_health",
        json!({ "details": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let dims = parsed.get("dimensions").expect("dimensions should exist");

    for dim in [
        "acyclicity",
        "depth",
        "equality",
        "redundancy",
        "modularity",
        "coverage_discipline",
    ] {
        let d = dims.get(dim).unwrap_or_else(|| panic!("missing {dim}"));
        assert!(
            d.get("score").is_some(),
            "{dim}: 'score' field missing in details view"
        );
        assert!(
            d.get("source").is_some(),
            "{dim}: 'source' formula attribution missing"
        );
    }

    // Specific raw signals that the issue called out as missing today.
    assert!(dims["equality"].get("gini").is_some());
    assert!(dims["equality"].get("interpretation").is_some());
    assert!(dims["acyclicity"].get("edges_in_cycles").is_some());
    assert!(dims["depth"].get("max_chain").is_some());
    assert!(dims["depth"].get("ideal_chain").is_some());
    assert!(dims["modularity"].get("interpretation").is_some());
    assert!(dims["redundancy"].get("dead_count").is_some());
}

// ---------------------------------------------------------------------------
// tokensave_dsm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dsm_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_dsm",
        json!({ "format": "stats" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("files").is_some(),
        "files field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("density").is_some(),
        "density field should exist"
    );
}

#[tokio::test]
async fn test_dsm_clusters() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_dsm",
        json!({ "format": "clusters" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("clusters").is_some(),
        "clusters array should exist, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// tokensave_test_risk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_test_risk() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_test_risk",
        json!({ "limit": 10 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = parsed.get("summary").expect("summary should exist");
    assert!(
        summary
            .get("total_functions")
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v > 0),
        "total_functions should be > 0, got: {}",
        text
    );
    assert!(parsed.get("risks").is_some(), "risks array should exist");
}

// ---------------------------------------------------------------------------
// Session start / end tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_session_start() {
    let (cg, dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_session_start", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(output["quality_signal"].as_u64().is_some());
    assert_eq!(output["status"].as_str().unwrap(), "baseline_saved");
    let baseline_path = dir.path().join(".tokensave/session_baseline.json");
    assert!(baseline_path.exists(), "baseline file should exist");
}

#[tokio::test]
async fn test_session_end() {
    let (cg, dir) = setup_project().await;
    handle_tool_call(&cg, "tokensave_session_start", json!({}), None, None)
        .await
        .unwrap();
    let result = handle_tool_call(&cg, "tokensave_session_end", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(output["signal_before"].as_u64().is_some());
    assert!(output["signal_after"].as_u64().is_some());
    assert!(output["delta"].is_number());
    let baseline_path = dir.path().join(".tokensave/session_baseline.json");
    assert!(
        !baseline_path.exists(),
        "baseline should be removed after session_end"
    );
}

#[tokio::test]
async fn test_session_end_no_baseline() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_session_end", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["status"].as_str().unwrap(), "no_baseline");
}

// ---------------------------------------------------------------------------
// tokensave_body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_body_returns_full_function_source() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_body",
        json!({"symbol": "format_greeting"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 1);
    let m = &output["matches"][0];
    let body = m["body"].as_str().unwrap();
    assert!(
        body.contains("fn format_greeting"),
        "body should contain the function signature, got: {body}"
    );
    assert!(
        body.contains("Hello"),
        "body should contain the function body, got: {body}"
    );
    // Regression for issue #62: the function's outer closing brace must be
    // included so the body is byte-exact usable as an Edit `old_string`.
    assert!(
        body.trim_end().ends_with('}'),
        "body should end with the function's closing brace, got: {body:?}"
    );
    // Line numbers are surfaced 1-based so they match what the user sees in
    // their editor and what Edit-style tools expect.
    let start_line = m["start_line"].as_u64().unwrap() as usize;
    let end_line = m["end_line"].as_u64().unwrap() as usize;
    assert!(start_line >= 1, "start_line should be 1-based");
    assert!(
        end_line >= start_line,
        "end_line should not precede start_line"
    );
    let file_rel = m["file"].as_str().unwrap();
    let file_abs = _dir.path().join(file_rel);
    let source = std::fs::read_to_string(&file_abs).unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let end_line_text = lines
        .get(end_line - 1)
        .copied()
        .unwrap_or_else(|| panic!("end_line {end_line} out of bounds in {file_rel}"));
    assert!(
        end_line_text.trim_end().ends_with('}'),
        "end_line ({end_line}) should point at the closing brace; line text: {end_line_text:?}"
    );
}

#[tokio::test]
async fn test_body_unknown_symbol() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_body",
        json!({"symbol": "no_such_symbol_anywhere"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("No symbol named"),
        "should report no match, got: {text}"
    );
}

#[tokio::test]
async fn test_body_missing_symbol_param() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_body", json!({}), None, None).await;
    assert!(result.is_err(), "should error when symbol is missing");
}

// ---------------------------------------------------------------------------
// tokensave_todos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_todos_finds_markers() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() {
    // TODO: refactor this
    let x = 1;
    // FIXME: handle the error case
    let y = 2;
    println!("{} {}", x, y);
}

fn helper() {
    // not a marker: rendered todoist
    let _ = 0;
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tokensave_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let count = output["match_count"].as_u64().unwrap();
    assert_eq!(count, 2, "should find exactly TODO and FIXME, got: {text}");
    let kinds: Vec<&str> = output["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"TODO"));
    assert!(kinds.contains(&"FIXME"));
    let enclosing: Vec<&str> = output["markers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["enclosing"].as_str())
        .collect();
    assert!(
        enclosing.iter().any(|e| e.contains("main")),
        "TODO inside main should report main as enclosing, got: {enclosing:?}"
    );
}

#[tokio::test]
async fn test_todos_filters_by_kind() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() {
    // TODO: a
    // FIXME: b
    // HACK: c
    let _ = 0;
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_todos",
        json!({"kinds": ["FIXME"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 1);
    assert_eq!(output["markers"][0]["kind"].as_str().unwrap(), "FIXME");
}

#[tokio::test]
async fn test_todos_empty_when_clean() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// tokensave_callers_for — bulk caller lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers_for_returns_caller_set_per_id() {
    let (cg, _dir) = setup_project().await;

    // Look up two distinct targets in one call.
    let helper_id = find_node_id(&cg, "helper").await;
    let format_id = find_node_id(&cg, "format_greeting").await;

    let result = handle_tool_call(
        &cg,
        "tokensave_callers_for",
        json!({"node_ids": [helper_id.clone(), format_id.clone()]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();

    // Response shape: { callers: { id: [...], id2: [...] }, truncated: bool, max_per_item: N }
    assert_eq!(output["truncated"], json!(false));
    assert!(output["max_per_item"].as_u64().unwrap() > 0);

    let callers = &output["callers"];
    let helper_callers = callers[&helper_id].as_array().unwrap();
    let format_callers = callers[&format_id].as_array().unwrap();

    // helper is called from main; format_greeting is called from helper.
    assert!(
        !helper_callers.is_empty(),
        "expected helper to have at least one caller"
    );
    assert!(
        !format_callers.is_empty(),
        "expected format_greeting to have at least one caller"
    );
}

#[tokio::test]
async fn test_callers_for_includes_unmatched_ids_as_empty() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    let bogus_id = "function:0000000000000000000000000000ffff".to_string();

    let result = handle_tool_call(
        &cg,
        "tokensave_callers_for",
        json!({"node_ids": [helper_id.clone(), bogus_id.clone()]}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let callers = &output["callers"];
    assert!(callers[&bogus_id].as_array().unwrap().is_empty());
    assert!(!callers[&helper_id].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_callers_for_respects_max_per_item() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    // Cap at 0 — every caller should be marked truncated.
    let result = handle_tool_call(
        &cg,
        "tokensave_callers_for",
        json!({"node_ids": [helper_id.clone()], "max_per_item": 0}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(output["truncated"], json!(true));
    assert!(output["callers"][&helper_id].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_callers_for_rejects_empty_input() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_callers_for",
        json!({"node_ids": []}),
        None,
        None,
    )
    .await;
    let Err(err) = result else {
        panic!("expected error for empty node_ids");
    };
    assert!(format!("{err}").contains("non-empty"));
}

#[tokio::test]
async fn test_callers_for_rejects_unknown_kind() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_callers_for",
        json!({"node_ids": [helper_id], "kind": "not_a_real_kind"}),
        None,
        None,
    )
    .await;
    let Err(err) = result else {
        panic!("expected error for unknown edge kind");
    };
    assert!(format!("{err}").contains("unknown edge kind"));
}

// ---------------------------------------------------------------------------
// tokensave_by_qualified_name — cross-run lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_by_qualified_name_finds_indexed_node() {
    let (cg, _dir) = setup_project().await;
    // Find the qualified name of `helper` first.
    let helper = cg
        .get_node(&find_node_id(&cg, "helper").await)
        .await
        .unwrap()
        .unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_by_qualified_name",
        json!({"qualified_name": helper.qualified_name}),
        None,
        None,
    )
    .await
    .unwrap();
    let items: Vec<Value> = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert!(
        !items.is_empty(),
        "expected at least one match for helper qname"
    );
    assert!(items.iter().any(|i| i["name"] == "helper"));
    // The handler exposes attrs_start_line in the response shape.
    assert!(items[0].get("attrs_start_line").is_some());
}

#[tokio::test]
async fn test_by_qualified_name_returns_empty_for_unknown() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_by_qualified_name",
        json!({"qualified_name": "crate::does::not::exist"}),
        None,
        None,
    )
    .await
    .unwrap();
    let items: Vec<Value> = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_by_qualified_name_requires_param() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tokensave_by_qualified_name", json!({}), None, None).await;
    let Err(err) = result else {
        panic!("expected error when qualified_name is missing");
    };
    assert!(format!("{err}").contains("qualified_name"));
}

#[tokio::test]
async fn memory_fact_store_add_search_update_remove_and_wrappers() {
    let (cg, _dir) = setup_project().await;

    let added = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "add",
            "content": "Project Phoenix uses Amari Memory in src/memory/types.rs",
            "category": "project",
            "entity": "Project Phoenix",
            "entities": ["Amari Memory"],
            "tags": ["memory", "holographic"],
            "source": "mcp-test",
            "metadata": {"plan": "holographic"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_i64()
        .expect("fact_store add should return numeric id");
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());
    assert_eq!(added["action"], "add");
    assert_eq!(added["fact"]["category"], "project");
    assert_eq!(added["fact"]["source"], "mcp-test");

    let search = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "search",
            "query": "Amari Memory",
            "category": "project",
            "min_trust": 0.1,
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let search: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    assert_eq!(search["action"], "search");
    assert_eq!(search["count"].as_u64(), Some(1));
    assert_eq!(search["results"], search["facts"]);
    assert!(
        search["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_i64() == Some(fact_id)),
        "search results should include added fact: {search}"
    );

    for (action, payload) in [
        ("probe", json!({"entity": "Project Phoenix"})),
        ("related", json!({"entity": "Amari Memory"})),
        (
            "reason",
            json!({"entities": ["Project Phoenix", "Amari Memory"]}),
        ),
        (
            "contradict",
            json!({"category": "project", "threshold": 0.8}),
        ),
        ("list", json!({"category": "project", "min_trust": 0.1})),
    ] {
        let mut args = payload;
        args["action"] = json!(action);
        let result = handle_tool_call(&cg, "tokensave_fact_store", args, None, None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        assert_eq!(output["action"], action, "{action} should echo action");
        assert!(
            output["results"].is_array(),
            "{action} should include results array: {output}"
        );
        assert!(
            output["count"].is_number(),
            "{action} should include count: {output}"
        );
        if action == "related" {
            assert!(
                output["count"].as_u64().unwrap_or_default() > 0,
                "related should return facts connected through adjacent entities: {output}"
            );
        }
    }

    let updated = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "update",
            "fact_id": fact_id,
            "content": "Project Phoenix uses deterministic Amari Memory",
            "entities": ["Project Phoenix", "Amari Memory"],
            "metadata": {"updated": true}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(
        updated["fact"]["content"],
        "Project Phoenix uses deterministic Amari Memory"
    );
    assert_eq!(updated["count"].as_u64(), Some(1));

    let removed = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({"action": "remove", "fact_id": fact_id.to_string()}),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
}

#[tokio::test]
async fn memory_recall_updates_retrieval_count() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "add",
            "content": "Retrieval counters move after search",
            "entity": "Counter Entity"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();

    handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({"action": "search", "query": "Retrieval counters", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();

    let status = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({"action": "list", "min_trust": 0.0, "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    let fact = status["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["fact_id"].as_i64() == Some(fact_id))
        .unwrap();
    assert!(
        fact["retrieval_count"].as_i64().unwrap_or_default() > 0,
        "returned facts should increment retrieval_count: {status}"
    );
}

#[tokio::test]
async fn memory_fact_store_update_trust_delta_uses_direct_fact_lookup() {
    let (cg, _dir) = setup_project().await;
    let first = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "add",
            "content": "First fact should remain updateable after many later facts",
            "trust": 0.4
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let first: Value = serde_json::from_str(extract_text(&first.value)).unwrap();
    let first_id = first["fact"]["fact_id"].as_i64().unwrap();

    for i in 0..205 {
        handle_tool_call(
            &cg,
            "tokensave_fact_store",
            json!({
                "action": "add",
                "content": format!("Later fact {i} should not hide the first fact"),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    let updated = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "update",
            "fact_id": first_id,
            "trust_delta": 0.2
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(updated["fact"]["fact_id"].as_i64(), Some(first_id));
    assert!(
        (updated["fact"]["trust_score"].as_f64().unwrap() - 0.6).abs() < 0.000_001,
        "trust_delta should apply through direct fact lookup: {updated}"
    );
}

#[tokio::test]
async fn memory_feedback_and_status_include_trust_fields() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "add",
            "content": "Helpful memory fact for feedback",
            "category": "general"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());

    let helpful = handle_tool_call(
        &cg,
        "tokensave_fact_feedback",
        json!({"fact_id": fact_id, "helpful": true, "source": "mcp-test", "note": "matched"}),
        None,
        None,
    )
    .await
    .unwrap();
    let helpful: Value = serde_json::from_str(extract_text(&helpful.value)).unwrap();
    assert!(helpful["feedback"]["event_id"].as_i64().unwrap() > 0);
    assert_eq!(helpful["feedback"]["fact_id"], fact_id);
    assert_eq!(helpful["feedback"]["action"], "helpful");
    assert_eq!(helpful["feedback"]["old_trust"], 0.5);
    assert!(helpful["feedback"]["new_trust"].as_f64().unwrap() > 0.5);
    assert!(helpful["feedback"]["trust_delta"].as_f64().unwrap() > 0.0);
    assert_eq!(helpful["feedback"]["helpful_count"], 1);
    assert_eq!(helpful["feedback"]["unhelpful_count"], 0);

    let unhelpful = handle_tool_call(
        &cg,
        "tokensave_fact_feedback",
        json!({"fact_id": fact_id, "unhelpful": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let unhelpful: Value = serde_json::from_str(extract_text(&unhelpful.value)).unwrap();
    assert_eq!(unhelpful["feedback"]["action"], "unhelpful");
    assert!(
        unhelpful["feedback"]["new_trust"].as_f64().unwrap()
            < helpful["feedback"]["new_trust"].as_f64().unwrap()
    );
    assert_eq!(unhelpful["feedback"]["helpful_count"], 1);
    assert_eq!(unhelpful["feedback"]["unhelpful_count"], 1);

    let status = handle_tool_call(&cg, "tokensave_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    assert!(status["memory"]["fact_count"].as_u64().unwrap() >= 1);
    assert!(status["memory"].get("trust_0_025_count").is_some());
    assert!(status["memory"].get("trust_025_050_count").is_some());
    assert!(status["memory"].get("trust_050_075_count").is_some());
    assert!(status["memory"].get("trust_075_100_count").is_some());
    assert!(status["memory"].get("helpful_count").is_some());
    assert!(status["memory"].get("unhelpful_count").is_some());
    assert!(status["memory"].get("missing_vector_count").is_some());
}

#[tokio::test]
async fn memory_tools_validate_malformed_inputs() {
    let (cg, _dir) = setup_project().await;

    let missing_action = handle_tool_call(&cg, "tokensave_fact_store", json!({}), None, None).await;
    assert!(expect_tool_error(missing_action).contains("action"));

    let bad_action = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({"action": "teleport"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_action).contains("unknown fact_store action"));

    let bad_category = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({"action": "list", "category": "definitely-not-a-category"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_category).contains("category"));

    let missing_feedback_action = handle_tool_call(
        &cg,
        "tokensave_fact_feedback",
        json!({"fact_id": 123}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(missing_feedback_action).contains("helpful"));
}

#[tokio::test]
async fn message_search_reads_project_local_session_db() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "cursor-session".to_string(),
        project_key: cg.project_root().to_string_lossy().to_string(),
        project_path: cg.project_root().to_string_lossy().to_string(),
        title: Some("Cursor transcript".to_string()),
        started_at: Some(1),
        ended_at: None,
        transcript_path: Some("cursor-session.jsonl".to_string()),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&session).await);
    let child_session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "worker-1".to_string(),
        project_key: cg.project_root().to_string_lossy().to_string(),
        project_path: cg.project_root().to_string_lossy().to_string(),
        title: Some("Cursor subagent".to_string()),
        started_at: Some(3),
        ended_at: None,
        transcript_path: Some("worker-1.jsonl".to_string()),
        metadata_json: None,
        parent_session_id: Some("cursor-session".to_string()),
        is_subagent: true,
        agent_id: Some("worker-1".to_string()),
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&child_session).await);
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "cursor-message".to_string(),
            session_id: "cursor-session".to_string(),
            role: "user".to_string(),
            timestamp: Some(2),
            ordinal: 1,
            text: "Project-local transcript search is working.".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("cursor-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "worker-message".to_string(),
            session_id: "worker-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(4),
            ordinal: 1,
            text: "Subagent citrus evidence is linked to its parent.".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("worker-1.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );

    let result = handle_tool_call(
        &cg,
        "tokensave_message_search",
        json!({"query": "transcript search", "provider": "cursor", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["count"], 1);
    assert_eq!(
        parsed["results"][0]["message"]["message_id"],
        "cursor-message"
    );
    assert_eq!(
        parsed["results"][0]["session"]["project_key"],
        cg.project_root().to_string_lossy().to_string()
    );

    let subagent_result = handle_tool_call(
        &cg,
        "tokensave_message_search",
        json!({
            "query": "citrus evidence",
            "provider": "cursor",
            "parent_session_id": "cursor-session",
            "scope": "subagents_only"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let subagent_parsed: Value =
        serde_json::from_str(extract_text(&subagent_result.value)).unwrap();
    assert_eq!(subagent_parsed["status"], "ok");
    assert_eq!(subagent_parsed["scope"], "subagents_only");
    assert_eq!(subagent_parsed["parent_session_id"], "cursor-session");
    assert_eq!(subagent_parsed["count"], 1);
    assert_eq!(
        subagent_parsed["results"][0]["session"]["parent_session_id"],
        "cursor-session"
    );
    assert_eq!(
        subagent_parsed["results"][0]["session"]["is_subagent"],
        true
    );
}

async fn seed_lcm_session_message(
    cg: &TokenSave,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

async fn seed_lcm_tool_result_message(
    cg: &TokenSave,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "tool".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("tool_result".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

async fn seed_lcm_session_message_with_role_source_timestamp(
    cg: &TokenSave,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
    role: &str,
    source: &str,
    timestamp: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: Some(timestamp),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: Some(serde_json::json!({"source": source}).to_string()),
        })
        .await
    );
}

async fn open_hermes_profile_session_db(hermes_home: &Path) -> GlobalDb {
    GlobalDb::open_at(&hermes_home.join(".tokensave/sessions.db"))
        .await
        .expect("profile-local session db should open")
}

async fn seed_lcm_session_message_in_db(
    db: &GlobalDb,
    project_path: &Path,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: project_path.to_string_lossy().to_string(),
            project_path: project_path.to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

async fn project_lcm_conn(cg: &TokenSave) -> libsql::Connection {
    let db = libsql::Builder::new_local(cg.project_root().join(".tokensave/sessions.db"))
        .build()
        .await
        .unwrap();
    db.connect().unwrap()
}

async fn lcm_fts_match_count(cg: &TokenSave, query: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages_fts WHERE lcm_raw_messages_fts MATCH ?1",
            libsql::params![query],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_store_id(cg: &TokenSave, message_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages WHERE provider = 'cursor' AND message_id = ?1",
            libsql::params![message_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_message_count(cg: &TokenSave, session_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_message_count_at_path(db_path: &Path, session_id: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_summary_node_count(cg: &TokenSave, session_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_summary_nodes WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_schema_migration_count(cg: &TokenSave) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn wipe_lcm_raw_fts(cg: &TokenSave) {
    project_lcm_conn(cg)
        .await
        .execute_batch("DELETE FROM lcm_raw_messages_fts;")
        .await
        .unwrap();
}

async fn wipe_lcm_raw_fts_for_message(cg: &TokenSave, message_id: &str) {
    let store_id = lcm_raw_store_id(cg, message_id).await;
    project_lcm_conn(cg)
        .await
        .execute(
            "DELETE FROM lcm_raw_messages_fts WHERE rowid = ?1",
            libsql::params![store_id],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn lcm_doctor_clean_dry_run_reports_noise_and_filtered_sessions_without_mutating() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must not leak",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "scratch-shell-a",
        "scratch-shell-message",
        "scratch one-shot body that must not leak",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        3,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        4,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": false,
            "ignore_session_patterns": ["cron-*"],
            "stateless_session_patterns": ["scratch-shell-*"],
            "ignore_message_patterns": ["Cronjob Response:*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["mode"], "clean");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["cleanup"]["read_only"], true);
    assert_eq!(
        payload["diagnostics"]["cleanup"]["ignored_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["stateless_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        1
    );
    assert!(payload["repairs"]["planned_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "clean_lcm_noise"));
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "scratch-shell-a").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 2);
    assert!(!text.contains("scheduled report body that must not leak"));
    assert!(!text.contains("scratch one-shot body that must not leak"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_clean_apply_backs_up_and_deletes_only_safe_candidates() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must be deleted only after backup",
        1,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let cron_store_id = lcm_raw_store_id(&cg, "cron-20260414-message").await;
    db.lcm_insert_summary_node(LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "cron-20260414".to_string(),
        session_id: "cron-20260414".to_string(),
        depth: 0,
        summary_text: "scheduled report summary".to_string(),
        source_refs: vec![LcmSourceRef::RawMessage {
            store_id: cron_store_id,
        }],
        source_token_count: 12,
        summary_token_count: 3,
        source_time_start: Some(1),
        source_time_end: Some(2),
        expand_hint: Some("test clean candidate".to_string()),
        metadata_json: None,
    })
    .await
    .unwrap();
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        3,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_session_patterns": ["cron-*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    let backup_path = payload["repairs"]["backup"]["path"]
        .as_str()
        .expect("clean apply should report backup path");

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    assert_eq!(payload["repairs"]["backup"]["ok"], true);
    assert!(Path::new(backup_path).is_file());
    assert_eq!(
        lcm_raw_message_count_at_path(Path::new(backup_path), "cron-20260414").await,
        1
    );
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 0);
    assert_eq!(lcm_summary_node_count(&cg, "cron-20260414").await, 0);
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 1);
    assert!(!text.contains("scheduled report body that must be deleted only after backup"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_clean_apply_deletes_all_matching_noise_beyond_diagnostic_samples() {
    let (cg, _dir) = setup_project().await;
    for idx in 0..25 {
        seed_lcm_session_message(
            &cg,
            "normal-session",
            &format!("cron-noise-{idx}"),
            format!("Cronjob Response: noisy heartbeat {idx}"),
            idx + 1,
        )
        .await;
    }
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        30,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_message_patterns": ["^Cronjob Response:"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        25
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["message_candidates"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        payload["repairs"]["applied_actions"][0]["deleted"]["raw_messages"],
        25
    );
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 1);
    assert!(!text.contains("Cronjob Response: noisy heartbeat"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_reports_missing_and_orphan_payloads_without_payload_bodies() {
    let (cg, _dir) = setup_project().await;
    let secret = format!(
        "LCM_DOCTOR_SECRET_PAYLOAD\n{}",
        "doctor-secret ".repeat(30_000)
    );
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload",
        "lcm-doctor-payload-message",
        secret,
        1,
    )
    .await;

    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-doctor-payload-message")
        .await
        .expect("externalized raw message should load");
    let payload_ref = raw.payload_ref.expect("external payload ref");
    fs::remove_file(
        cg.project_root()
            .join(".tokensave/lcm-payloads")
            .join(&payload_ref),
    )
    .unwrap();
    fs::write(
        cg.project_root()
            .join(".tokensave/lcm-payloads/payload_unreferenced_test.payload"),
        "orphan body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 1);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 1);
    let text = extract_text(&result.value);
    assert!(!text.contains("LCM_DOCTOR_SECRET_PAYLOAD"));
    assert!(!text.contains("orphan body that must not be returned"));
}

#[tokio::test]
async fn lcm_doctor_reports_placeholder_recovery_and_gc_candidates_without_bodies() {
    let (cg, _dir) = setup_project().await;
    let missing_ref = "payload_missing_placeholder_test.payload";
    let placeholder = format!(
        "[Externalized LCM ingest payload: kind=ingest_payload; role=user; field=content; chars=2048; bytes=2048; ref={missing_ref}]"
    );
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-placeholder",
        "lcm-doctor-placeholder-message",
        placeholder,
        1,
    )
    .await;

    let payload_dir = cg.project_root().join(".tokensave/lcm-payloads");
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(
        payload_dir.join("payload_gc_candidate_test.payload"),
        "gc candidate body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-placeholder",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        1
    );
    assert_eq!(payload["diagnostics"]["payloads"]["gc_candidate_files"], 1);
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["payload_ref"] == missing_ref)
    );
    assert!(
        payload["diagnostics"]["payloads"]["gc_candidate_payload_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("payload_gc_candidate_test.payload"))
    );
    let text = extract_text(&result.value);
    assert!(!text.contains("gc candidate body that must not be returned"));
}

#[tokio::test]
async fn lcm_doctor_counts_nested_externalized_payload_refs_as_referenced() {
    let (cg, _dir) = setup_project().await;
    let media_payload = format!(
        "data:image/png;base64,{}",
        "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(160)
    );
    let content = json!({
        "content": [
            {"type": "text", "text": "doctor nested payload canary"},
            {"type": "image_url", "image_url": {"url": media_payload}},
        ]
    })
    .to_string();
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-nested-payload",
        "lcm-doctor-nested-payload-message",
        content,
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-nested-payload",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["unreferenced_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
}

#[tokio::test]
async fn lcm_doctor_ignores_plain_text_ref_tokens_as_placeholders() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-plain-ref",
        "lcm-doctor-plain-ref-message",
        "plain documentation mentions ref=payload_plain_text_false_positive.payload outside a placeholder",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-plain-ref",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn lcm_doctor_scoped_payload_diagnostics_ignore_other_session_payload_files() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-target",
        "lcm-doctor-payload-target-message",
        format!("target payload\n{}", "target-body ".repeat(30_000)),
        1,
    )
    .await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-other",
        "lcm-doctor-payload-other-message",
        format!("other payload\n{}", "other-body ".repeat(30_000)),
        2,
    )
    .await;

    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let other_raw = db
        .lcm_load_raw_message("cursor", "lcm-doctor-payload-other-message")
        .await
        .expect("other externalized raw message should load");
    let other_payload_ref = other_raw.payload_ref.expect("other external payload ref");

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-payload-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 0);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 0);
    assert!(!payload["diagnostics"]["payloads"]["orphan_payload_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some(&other_payload_ref)));
    assert!(!extract_text(&result.value).contains(&other_payload_ref));
}

#[tokio::test]
async fn lcm_doctor_reports_scoped_fts_rebuild_when_other_session_matches_probe_term() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-target",
        "lcm-doctor-fts-target-message",
        "scopedneedle target text",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-other",
        "lcm-doctor-fts-other-message",
        "scopedneedle other text",
        2,
    )
    .await;
    wipe_lcm_raw_fts_for_message(&cg, "lcm-doctor-fts-target-message").await;
    assert_eq!(lcm_fts_match_count(&cg, "scopedneedle").await, 1);

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-fts-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["fts"]["raw"]["rebuild_needed"], true);
}

#[tokio::test]
async fn lcm_doctor_counts_summary_source_rows_with_missing_owner_node() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-orphan-owner",
        "lcm-doctor-orphan-owner-message",
        "orphan owner source text",
        1,
    )
    .await;
    let store_id = lcm_raw_store_id(&cg, "lcm-doctor-orphan-owner-message").await;
    let conn = project_lcm_conn(&cg).await;
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
             VALUES ('missing-summary-owner', 'raw_message', ?1, 0)",
        libsql::params![store_id.to_string()],
    )
    .await
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-orphan-owner",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["summaries"]["broken_sources"], 1);
}

#[tokio::test]
async fn lcm_doctor_scopes_orphan_lifecycle_debt_to_requested_session() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-debt-target",
        "lcm-doctor-debt-target-message",
        "target session text",
        1,
    )
    .await;
    let conn = project_lcm_conn(&cg).await;
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_maintenance_debt(
                provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
             )
             VALUES ('cursor', 'lcm-doctor-debt-other', 'orphan-debt', 'raw_backlog', 1, 2)",
        (),
    )
    .await
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-debt-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["lifecycle"]["orphan_debt"], 0);
}

#[tokio::test]
async fn lcm_doctor_diagnose_does_not_create_missing_project_session_db() {
    let (cg, _dir) = setup_project().await;
    let db_path = tokensave::sessions::cursor::project_session_db_path(cg.project_root());
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert!(
        !db_path.exists(),
        "diagnose must not create session storage"
    );
}

#[tokio::test]
async fn lcm_doctor_repair_dry_run_does_not_run_schema_migration() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-read-only-existing",
        "lcm-doctor-read-only-existing-message",
        "read only existing database text",
        1,
    )
    .await;
    project_lcm_conn(&cg)
        .await
        .execute(
            "DELETE FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(lcm_schema_migration_count(&cg).await, 0);

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["schema"]["migration_present"], false);
    assert_eq!(lcm_schema_migration_count(&cg).await, 0);
}

#[tokio::test]
async fn lcm_doctor_repair_dry_run_reports_fts_rebuild_without_mutating() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-dry-run",
        "lcm-doctor-dry-run-message",
        "dry run searchable needle",
        1,
    )
    .await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 1);
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["fts"]["rebuild_needed"], true);
    assert!(payload["repairs"]["planned_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "rebuild_raw_fts"));
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);
}

#[tokio::test]
async fn lcm_doctor_repair_apply_rebuilds_damaged_fts() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-apply",
        "lcm-doctor-apply-message",
        "apply repair searchable needle",
        1,
    )
    .await;
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    assert!(payload["repairs"]["applied_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "rebuild_raw_fts"));
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 1);
}

#[tokio::test]
async fn lcm_doctor_retention_reports_candidates_without_deleting() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-retention",
        "lcm-doctor-retention-message",
        "old session retention candidate",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({"provider": "cursor", "mode": "retention"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "retention");
    assert_eq!(payload["diagnostics"]["retention"]["read_only"], true);
    assert!(payload["diagnostics"]["retention"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate["session_id"] == "lcm-doctor-retention"));
    assert_eq!(lcm_raw_message_count(&cg, "lcm-doctor-retention").await, 1);
}

#[tokio::test]
async fn lcm_doctor_uses_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-profile",
        "lcm-doctor-profile-project",
        "project-local doctor should not be counted",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-doctor-profile",
        "lcm-doctor-profile-message",
        "profile-local doctor should be counted",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_doctor",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path().to_string_lossy()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert_eq!(payload["diagnostics"]["raw_message_count"], 1);
}

#[tokio::test]
async fn lcm_session_handlers_expose_bounded_read_apis_and_placeholders() {
    let (cg, _dir) = setup_project().await;
    let full_text = format!("orchard dispatch {}", "external-payload-body ".repeat(400));
    seed_lcm_session_message(&cg, "lcm-session", "lcm-message", full_text, 1).await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-message")
        .await
        .expect("LCM raw message should be created by compatibility ingest");

    let status = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({"provider": "cursor"}),
        None,
        None,
    )
    .await
    .unwrap();
    let status_payload: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status_payload["status"], "ok");
    assert_eq!(status_payload["lcm"]["raw_message_count"], 1);

    let loaded = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "content_limit": 24
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["messages"].as_array().unwrap().len(), 1);
    assert!(loaded_payload["messages"][0]["content_range"]["truncated"]
        .as_bool()
        .unwrap());
    assert_eq!(
        loaded_payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        24
    );

    let grep = handle_tool_call(
        &cg,
        "tokensave_lcm_grep",
        json!({"provider": "cursor", "query": "orchard dispatch", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["hits"].as_array().unwrap().len(), 1);
    assert!(
        grep_payload["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 4096,
        "grep snippets must stay bounded"
    );

    let described = handle_tool_call(
        &cg,
        "tokensave_lcm_describe",
        json!({"provider": "cursor", "session_id": "lcm-session"}),
        None,
        None,
    )
    .await
    .unwrap();
    let described_payload: Value = serde_json::from_str(extract_text(&described.value)).unwrap();
    assert_eq!(described_payload["status"], "ok");
    assert_eq!(described_payload["description"]["raw_message_count"], 1);
    assert!(described_payload["description"]["raw_messages"][0]
        .get("content_preview")
        .is_some());
    assert!(
        described_payload["description"]["raw_messages"][0]
            .get("content")
            .is_none(),
        "describe must not expose raw payload bodies"
    );

    let expanded = handle_tool_call(
        &cg,
        "tokensave_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "target": {"kind": "raw_message", "store_id": raw.store_id},
            "content_offset": 8,
            "content_limit": 16
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let expanded_payload: Value = serde_json::from_str(extract_text(&expanded.value)).unwrap();
    assert_eq!(expanded_payload["status"], "ok");
    assert_eq!(expanded_payload["expansion"]["kind"], "raw_message");
    assert_eq!(
        expanded_payload["expansion"]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        16
    );
    assert!(expanded_payload["expansion"]["content_range"]["truncated"]
        .as_bool()
        .unwrap());

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "prompt": "Summarize orchard dispatch",
            "query": "orchard dispatch",
            "context_max_tokens": 128,
            "max_tokens": 64
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["prompt"], "Summarize orchard dispatch");
    assert!(payload["context_blocks"]
        .as_array()
        .expect("context blocks")
        .iter()
        .any(|block| block["kind"] == "raw_message"));
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("EXPANDED CONTEXT"));
    assert!(extract_text(&result.value).len() <= 15_000);

    let preflight = handle_tool_call(
        &cg,
        "tokensave_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-preflight", "role": "user", "content": "hello"}]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preflight_payload: Value = serde_json::from_str(extract_text(&preflight.value)).unwrap();
    assert_eq!(preflight_payload["status"], "ok");
    assert_eq!(preflight_payload["should_compress"], false);

    let compress = handle_tool_call(
        &cg,
        "tokensave_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-compress", "role": "user", "content": "hello again"}],
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let compress_payload: Value = serde_json::from_str(extract_text(&compress.value)).unwrap();
    assert_eq!(compress_payload["status"], "ok");
    assert_eq!(compress_payload["summary_nodes_created"], 0);
    assert_eq!(compress_payload["compression_attempts"], 0);
    assert_eq!(compress_payload["fallback_used"], false);
    assert!(
        compress_payload.get("retry_status").is_some(),
        "compress response must expose retry_status for bridge contract"
    );
    assert_eq!(compress_payload["retry_status"], Value::Null);

    for (index, content) in [
        "old-1 token",
        "old-2 token",
        "old-3 token",
        "old-4 token",
        "old-5 token",
        "old-6 token",
        "fresh-1",
        "fresh-2",
    ]
    .iter()
    .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-critical-session",
            &format!("lcm-critical-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let critical_compress = handle_tool_call(
        &cg,
        "tokensave_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-critical-session",
            "messages": [],
            "current_tokens": 40,
            "max_assembly_tokens": 2,
            "leaf_chunk_tokens": 1,
            "max_source_messages": 3,
            "summarizer": {"mode": "fake", "summary_text": "catchup summary"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let critical_payload: Value =
        serde_json::from_str(extract_text(&critical_compress.value)).unwrap();
    assert_eq!(critical_payload["status"], "best_effort");
    assert_eq!(critical_payload["summary_nodes_created"], 4);
    assert_eq!(critical_payload["compression_attempts"], 4);
    assert_eq!(critical_payload["fallback_used"], false);
    assert_eq!(
        critical_payload["retry_status"],
        "critical_pressure_catch_up"
    );
}

#[tokio::test]
async fn lcm_status_response_is_valid_json_and_omits_payload_secrets() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "lcm-status-session".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("LCM status diagnostics".to_string()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: Some("lcm-status-session.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );

    let secret = format!("MCP_STATUS_SECRET_PAYLOAD\n{}", "Q".repeat(300_000));
    db.lcm_store(cg.project_root().join(".tokensave"))
        .ingest_raw_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "lcm-status-secret-message".to_string(),
            session_id: "lcm-status-session".to_string(),
            role: "tool".to_string(),
            timestamp: Some(2),
            ordinal: 1,
            text: secret,
            kind: Some("tool_result".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("lcm-status-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
        .expect("external payload should ingest");

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-session",
            "storage_scope": "project_local"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).expect("LCM status response must be JSON");

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "project_local");
    assert_eq!(payload["lcm"]["payload"]["externalized_count"], 1);
    assert_eq!(payload["lcm"]["payload"]["missing_count"], 0);
    assert_eq!(payload["lcm"]["payload"]["unreferenced_count"], 0);
    assert_eq!(payload["lcm"]["redaction"]["enabled"], false);
    assert!(!text.contains("MCP_STATUS_SECRET_PAYLOAD"));
}

#[tokio::test]
async fn lcm_status_reports_lifecycle_fields_and_resolved_storage_scope() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-1",
        "frontier seed one",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-2",
        "frontier seed two",
        2,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let first = db
        .lcm_load_raw_message("cursor", "lcm-status-frontier-message-1")
        .await
        .expect("first raw message should load");
    let second = db
        .lcm_load_raw_message("cursor", "lcm-status-frontier-message-2")
        .await
        .expect("second raw message should load");
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "lcm-status-frontier".into(),
        current_session_id: "lcm-status-frontier".into(),
        current_frontier_store_id: Some(second.store_id),
        last_finalized_session_id: Some("lcm-status-prior".into()),
        last_finalized_frontier_store_id: Some(first.store_id),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: first.store_id,
            to_store_id: second.store_id,
        }],
    })
    .await
    .expect("lifecycle state should update");

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-frontier",
            "storage_scope": "project_local"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "project_local");
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_session_id"],
        "lcm-status-frontier"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_frontier_store_id"],
        second.store_id
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_session_id"],
        "lcm-status-prior"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_frontier_store_id"],
        first.store_id
    );
    assert_eq!(payload["lcm"]["lifecycle"]["maintenance_debt_count"], 1);

    let profile_result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-frontier",
            "storage_scope": "hermes_profile"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let profile_payload: Value = serde_json::from_str(extract_text(&profile_result.value)).unwrap();
    assert_eq!(profile_payload["status"], "unavailable");
    assert_eq!(profile_payload["storage_scope"], "hermes_profile");
    assert!(
        profile_payload.get("lcm").is_none(),
        "hermes_profile requests must not return project-local LCM counts"
    );
}

#[tokio::test]
async fn lcm_describe_supports_summary_node_and_external_payload_targets() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-source",
        "describe source body must not leak through metadata",
        1,
    )
    .await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-tool",
        format!("describe external secret {}", "payload ".repeat(40_000)),
        2,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let source = db
        .lcm_load_raw_message("cursor", "lcm-describe-source")
        .await
        .expect("source raw message should exist");
    let external = db
        .lcm_load_raw_message("cursor", "lcm-describe-tool")
        .await
        .expect("external raw message should exist");
    let payload_ref = external.payload_ref.expect("payload ref");
    let summary = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conversation-1".to_string(),
            session_id: "lcm-describe-targets".to_string(),
            depth: 0,
            summary_text: "summary secret body must not leak through metadata".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage {
                store_id: source.store_id,
            }],
            source_token_count: 30,
            summary_token_count: 5,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("describe target summary".to_string()),
            metadata_json: None,
        })
        .await
        .expect("summary should insert");

    let node_result = handle_tool_call(
        &cg,
        "tokensave_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "summary_node", "node_id": summary.node_id.clone()}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let node_payload: Value = serde_json::from_str(extract_text(&node_result.value)).unwrap();
    assert_eq!(node_payload["status"], "ok");
    assert_eq!(node_payload["description"]["target"], "summary_node");
    assert_eq!(
        node_payload["description"]["summary_node"]["node_id"],
        summary.node_id
    );
    assert_eq!(
        node_payload["description"]["summary_node"]["source_count"],
        1
    );

    let payload_result = handle_tool_call(
        &cg,
        "tokensave_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload_payload: Value = serde_json::from_str(extract_text(&payload_result.value)).unwrap();
    assert_eq!(payload_payload["status"], "ok");
    assert_eq!(payload_payload["description"]["target"], "external_payload");
    assert_eq!(
        payload_payload["description"]["external_payload"]["payload_ref"],
        payload_ref
    );
    assert!(
        payload_payload["description"]["external_payload"]["content_preview"]
            .as_str()
            .unwrap()
            .contains(payload_ref.as_str())
    );

    let rendered = format!(
        "{}\n{}",
        extract_text(&node_result.value),
        extract_text(&payload_result.value)
    );
    assert!(!rendered.contains("summary secret body"));
    assert!(!rendered.contains("describe source body"));
    assert!(!rendered.contains("describe external secret"));
}

#[tokio::test]
async fn lcm_grep_and_load_session_honor_native_filters_and_content_clamp() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-old-cli-assistant",
        "orchard native old cli assistant",
        1,
        "assistant",
        "cli",
        10,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-cli-user",
        "orchard native new cli user",
        2,
        "user",
        "cli",
        20,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-api-assistant",
        "orchard native new api assistant",
        3,
        "assistant",
        "api",
        30,
    )
    .await;

    let grep = handle_tool_call(
        &cg,
        "tokensave_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard native",
            "scope": "session",
            "session_id": "lcm-native-filters",
            "sort": "recency",
            "source": "cli",
            "role": "assistant",
            "start_time": 5,
            "end_time": 25,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["count"], 1);
    assert_eq!(
        grep_payload["hits"][0]["message_id"],
        "lcm-native-old-cli-assistant"
    );
    assert_eq!(grep_payload["sort"], "recency");

    let loaded = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-native-filters",
            "roles": ["assistant", "user"],
            "time_from": 1,
            "time_to": 25,
            "content_limit": 25_000,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["content_limit"], 20_000);
    assert_eq!(loaded_payload["content_limit_clamped_from"], 25_000);
    assert_eq!(
        loaded_payload["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["message_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["lcm-native-old-cli-assistant", "lcm-native-new-cli-user"]
    );
}

#[tokio::test]
async fn lcm_status_uses_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-status",
        "lcm-profile-status-project",
        "project-local distractor status",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-status",
        "lcm-profile-status-profile-1",
        "profile-local status seed one",
        1,
    )
    .await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-status",
        "lcm-profile-status-profile-2",
        "profile-local status seed two",
        2,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-status",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "hermes_profile");
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert!(hermes_home.path().join(".tokensave/sessions.db").exists());
}

#[tokio::test]
async fn lcm_load_and_grep_use_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-read",
        "lcm-profile-read-project",
        "project-local distractor profile query",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-read",
        "lcm-profile-read-profile",
        "profile-local pear evidence",
        1,
    )
    .await;

    let loaded = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        loaded_payload["messages"][0]["content"],
        "profile-local pear evidence"
    );

    let grep = handle_tool_call(
        &cg,
        "tokensave_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "profile-local pear",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path(),
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["count"], 1);
    assert_eq!(grep_payload["hits"][0]["session_id"], "lcm-profile-read");
    assert!(grep_payload["hits"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("profile-local pear evidence"));

    let expanded = handle_tool_call(
        &cg,
        "tokensave_lcm_expand_query",
        json!({
            "provider": "cursor",
            "prompt": "Explain profile pear evidence",
            "query": "profile-local pear",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path(),
            "context_max_tokens": 1024,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let expanded_payload: Value = serde_json::from_str(extract_text(&expanded.value)).unwrap();
    assert_eq!(expanded_payload["status"], "ok");
    assert_eq!(expanded_payload["storage_scope"], "hermes_profile");
    assert_eq!(expanded_payload["needs_synthesis"], true);
    assert!(expanded_payload["context_blocks"][0]["content"]
        .as_str()
        .unwrap()
        .contains("profile-local pear evidence"));
}

#[tokio::test]
async fn lcm_hermes_profile_requires_explicit_valid_home_without_fallback() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-missing-home",
        "lcm-profile-missing-home-project",
        "project-local must not leak without hermes home",
        1,
    )
    .await;

    let status = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-missing-home",
            "storage_scope": "hermes_profile"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let status_payload: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status_payload["status"], "unavailable");
    assert_eq!(status_payload["storage_scope"], "hermes_profile");
    assert!(status_payload["message"]
        .as_str()
        .unwrap()
        .contains("hermes_home"));
    assert!(status_payload.get("lcm").is_none());

    let loaded = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-missing-home",
            "storage_scope": "hermes_profile",
            "hermes_home": "relative-hermes-home"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "unavailable");
    assert_eq!(loaded_payload["storage_scope"], "hermes_profile");
    assert!(loaded_payload["message"]
        .as_str()
        .unwrap()
        .contains("absolute hermes_home"));
    assert!(loaded_payload.get("messages").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn lcm_hermes_profile_rejects_symlinked_tokensave_dir_escape() {
    let (cg, _dir) = setup_project().await;
    let hermes_home = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    unix_fs::symlink(outside.path(), hermes_home.path().join(".tokensave")).unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert!(
        payload["message"].as_str().unwrap().contains(".tokensave"),
        "rejection should identify the unsafe profile storage component: {payload}"
    );
    assert!(
        !outside.path().join("sessions.db").exists(),
        "profile DB must not be created through a symlink escape"
    );
}

#[tokio::test]
async fn lcm_hermes_profile_rejects_non_directory_home() {
    let (cg, _dir) = setup_project().await;
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join("hermes-home-file");
    fs::write(&hermes_home, "not a directory").unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_status",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert!(
        payload["message"]
            .as_str()
            .unwrap()
            .contains("not a directory"),
        "non-directory hermes_home should be rejected clearly: {payload}"
    );
    assert!(payload.get("lcm").is_none());
}

#[tokio::test]
async fn lcm_grep_rejects_invalid_scope_without_searching_all_sessions() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-scope-a",
        "lcm-scope-message-a",
        "fail closed unique-cross-session-token alpha",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-scope-b",
        "lcm-scope-message-b",
        "fail closed unique-cross-session-token beta",
        2,
    )
    .await;

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tokensave_lcm_grep",
            json!({
                "provider": "cursor",
                "query": "unique-cross-session-token",
                "scope": "everything",
                "limit": 10
            }),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("scope"),
        "invalid scope should report an argument error, got {err}"
    );
}

#[tokio::test]
async fn lcm_load_session_rejects_fractional_negative_and_wrong_type_numeric_args() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-numeric",
        "lcm-numeric-message",
        "numeric validation test body",
        1,
    )
    .await;

    for (case, args) in [
        (
            "fractional limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": 1.5}),
        ),
        (
            "negative limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": -1}),
        ),
        (
            "string limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": "1"}),
        ),
    ] {
        let err = expect_tool_error(
            handle_tool_call(&cg, "tokensave_lcm_load_session", args, None, None).await,
        );
        assert!(
            err.contains("limit"),
            "{case} should report an argument error mentioning limit, got {err}"
        );
    }
}

#[tokio::test]
async fn lcm_load_session_accepts_valid_integer_args() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-valid-integers",
        "lcm-valid-integers-message",
        "valid integer argument body",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-valid-integers",
            "limit": 1,
            "content_offset": 0,
            "content_limit": 8,
            "start_time": 1,
            "end_time": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["messages"][0]["content"].as_str().unwrap(),
        "valid in"
    );
}

#[tokio::test]
async fn lcm_large_json_response_stays_parseable_after_truncation() {
    let (cg, _dir) = setup_project().await;
    for index in 0..4 {
        seed_lcm_session_message(
            &cg,
            "lcm-large-json",
            &format!("lcm-large-json-message-{index}"),
            format!("large json response {index} {}", "payload ".repeat(2000)),
            index + 1,
        )
        .await;
    }

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-json",
            "limit": 4,
            "content_limit": 8192
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value))
        .expect("truncated LCM tool text should remain valid JSON");
    assert_eq!(payload["truncated"], true);
    assert!(payload["preview"].as_str().unwrap().len() <= 15_000);
}

#[tokio::test]
async fn lcm_expand_query_large_response_preserves_synthesis_contract() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-large-expand-query",
        "lcm-large-expand-query-message",
        format!(
            "oversized expand-query evidence {}",
            "context ".repeat(4000)
        ),
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-expand-query",
            "prompt": "Summarize oversized expand-query evidence",
            "query": "oversized expand-query evidence",
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("large expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(
        payload["prompt"],
        "Summarize oversized expand-query evidence"
    );
    assert!(payload["synthesis_prompt"]["system"]
        .as_str()
        .unwrap()
        .contains("expanded LCM retrieval context"));
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("Summarize oversized expand-query evidence"));
    assert!(payload["context_truncated"].as_bool().is_some());
    assert!(payload["context_budget"]["used_chars"].as_u64().is_some());
    assert!(!payload["matches"].as_array().unwrap().is_empty());
    assert!(
        payload["context_blocks"].as_array().unwrap().len() <= 3,
        "MCP expand-query context should stay compact"
    );
    assert!(text.len() <= 15_000);
}

#[tokio::test]
async fn lcm_expand_query_oversized_prompt_preserves_synthesis_contract() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-huge-prompt-expand-query",
        "lcm-huge-prompt-expand-query-message",
        "contract overflow evidence lives in this raw message",
        1,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-huge-prompt-expand-query-message")
        .await
        .expect("raw message should exist");
    let summary = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conversation-1".to_string(),
            session_id: "lcm-huge-prompt-expand-query".to_string(),
            depth: 0,
            summary_text: "summary contract overflow evidence".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage {
                store_id: raw.store_id,
            }],
            source_token_count: 30,
            summary_token_count: 5,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("contract overflow summary".to_string()),
            metadata_json: None,
        })
        .await
        .expect("summary should insert");
    let huge_prompt = format!(
        "Explain contract overflow evidence. {}",
        "PROMPT_OVERFLOW ".repeat(12_000)
    );
    let huge_query = format!(
        "contract overflow evidence {}",
        "QUERY_OVERFLOW ".repeat(12_000)
    );

    let result = handle_tool_call(
        &cg,
        "tokensave_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-huge-prompt-expand-query",
            "prompt": huge_prompt,
            "query": huge_query,
            "node_ids": [summary.node_id],
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("oversized expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["mcp_response_truncated"], true);
    assert!(payload["prompt"].as_str().unwrap().chars().count() <= 2_048);
    assert!(payload["query"].as_str().unwrap().chars().count() <= 1_024);
    assert!(payload["prompt_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["query_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["contract_truncated"].as_bool().unwrap());
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("QUESTION:"));
    assert!(text.len() <= 15_000);
}

#[tokio::test]
async fn message_search_preserves_provider_project_parent_scope_shape_after_lcm() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "parent".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("Parent session".to_string()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: Some("parent.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "child".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("Child session".to_string()),
            started_at: Some(2),
            ended_at: None,
            transcript_path: Some("child.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: Some("parent".to_string()),
            is_subagent: true,
            agent_id: Some("child".to_string()),
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "child-message".to_string(),
            session_id: "child".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(3),
            ordinal: 1,
            text: "orchard dispatch compatibility result".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("child.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );

    let result = handle_tool_call(
        &cg,
        "tokensave_message_search",
        json!({
            "query": "orchard dispatch",
            "provider": "cursor",
            "project_key": cg.project_root().to_string_lossy(),
            "scope": "subagents_only",
            "parent_session_id": "parent",
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["scope"], "subagents_only");
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["parent_session_id"], "parent");
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert!(payload["results"][0]["message"].get("text").is_some());
    assert_eq!(payload["results"][0]["session"]["is_subagent"], true);
}

#[tokio::test]
async fn lcm_status_cli_bridge_accepts_json_args() {
    let (cg, _dir) = setup_project().await;
    let outside_cwd = TempDir::new().unwrap();
    let project_arg = cg.project_root().display().to_string();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tokensave"))
        .current_dir(outside_cwd.path())
        .args([
            "tool",
            "--project",
            &project_arg,
            "tokensave_lcm_status",
            "--json",
            "--args",
            r#"{"provider":"cursor"}"#,
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "tokensave tool exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["content"][0]["type"], "text");
    let payload: Value =
        serde_json::from_str(json["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload["status"], "ok");
}

#[test]
fn memory_tool_definitions_include_hermes_payload_fields() {
    let tools = get_tool_definitions();
    let tool_names: std::collections::HashSet<_> =
        tools.iter().map(|tool| tool.name.as_str()).collect();
    let fact_store = tools
        .iter()
        .find(|tool| tool.name == "tokensave_fact_store")
        .expect("tokensave_fact_store definition");
    let feedback = tools
        .iter()
        .find(|tool| tool.name == "tokensave_fact_feedback")
        .expect("tokensave_fact_feedback definition");
    let status = tools
        .iter()
        .find(|tool| tool.name == "tokensave_memory_status")
        .expect("tokensave_memory_status definition");

    assert_eq!(
        fact_store.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(
        feedback.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(status.annotations.as_ref().unwrap()["readOnlyHint"], false);

    for field in [
        "action",
        "content",
        "query",
        "entity",
        "entities",
        "fact_id",
        "category",
        "tags",
        "min_trust",
        "trust",
        "trust_delta",
        "threshold",
        "limit",
        "source",
        "metadata",
        "note",
    ] {
        assert!(
            fact_store.input_schema["properties"].get(field).is_some(),
            "fact_store schema missing Hermes field {field}"
        );
    }
    assert_eq!(
        feedback.input_schema["required"],
        serde_json::json!(["fact_id"])
    );
    assert_eq!(
        fact_store.input_schema["properties"]["trust"]["type"],
        "number"
    );
    assert_eq!(fact_store.input_schema["properties"]["trust"]["minimum"], 0);
    assert_eq!(fact_store.input_schema["properties"]["trust"]["maximum"], 1);

    assert!(
        !tool_names.contains("tokensave_record_decision"),
        "unshipped legacy decision tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tokensave_record_code_area"),
        "unshipped legacy code-area tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tokensave_session_recall"),
        "unshipped legacy recall tool should not be exposed"
    );
}

#[test]
fn message_search_provider_schema_matches_ingested_providers() {
    let tools = get_tool_definitions();
    let message_search = tools
        .iter()
        .find(|tool| tool.name == "tokensave_message_search")
        .expect("tokensave_message_search definition");

    assert_eq!(
        message_search.input_schema["properties"]["provider"]["enum"],
        serde_json::json!(["cursor", "claude", "codex", "vibe", "cline", "roo-code", "kilo"])
    );
    assert_eq!(
        message_search.input_schema["properties"]["scope"]["enum"],
        serde_json::json!(["all", "parents_only", "subagents_only"])
    );
    assert!(message_search.input_schema["properties"]
        .get("parent_session_id")
        .is_some());
    assert!(message_search.input_schema["properties"]
        .get("include_subagents")
        .is_some());
}

#[tokio::test]
async fn memory_status_repairs_dirty_banks_before_reporting() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tokensave_fact_store",
        json!({
            "action": "add",
            "content": "Status should repair dirty holographic banks",
            "category": "project",
            "entity": "Holographic Banks"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    let db_path = cg.project_root().join(".tokensave").join("tokensave.db");
    let (db, _) = Database::open(&db_path).await.unwrap();
    db.conn()
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = NULL, hrr_algebra = 'legacy', hrr_dim = 8
             WHERE fact_id = ?1",
            libsql::params![fact_id],
        )
        .await
        .unwrap();
    db.close();

    let status = handle_tool_call(&cg, "tokensave_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    assert!(
        status["memory"]["bank_count"].as_u64().unwrap_or_default() >= 2,
        "memory_status should rebuild all and category banks before reporting: {status}"
    );
    assert_eq!(
        status["memory"]["missing_vector_count"].as_u64(),
        Some(0),
        "status-triggered bank repair should not leave missing vectors"
    );
    assert_eq!(
        status["memory"]["repair"]["missing_vectors_repaired"].as_u64(),
        Some(1),
        "status should report derived vector repairs: {status}"
    );
    assert!(
        status["memory"]["repair"]["banks_rebuilt"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "status should report bank repair work after vector repair: {status}"
    );
}

// ---------------------------------------------------------------------------
// Bug-report regressions: sonium-codebase issues
// ---------------------------------------------------------------------------

/// Regression for bug #1: `tokensave_body` should prefer the `fn foo()` over
/// a field/variant also named `foo`. Setup mirrors what sonium hit when
/// searching for `gmres`: the codebase has both a `pub fn gmres(...)` and a
/// struct field literally named `gmres`. The function — the body the user
/// actually wants — must outrank the field.
async fn setup_function_vs_field_collision() -> (TokenSave, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct Solvers {
    pub gmres: u32,
}

pub fn gmres(x: u32) -> u32 {
    x + 1
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    (cg, dir)
}

#[tokio::test]
async fn body_prefers_function_over_field_with_same_name() {
    let (cg, _dir) = setup_function_vs_field_collision().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_body",
        json!({"symbol": "gmres"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let matches = output["matches"].as_array().unwrap();
    let first = &matches[0];
    assert_eq!(
        first["kind"].as_str(),
        Some("function"),
        "first match should be the function definition, got {first}"
    );
    let body = first["body"].as_str().unwrap();
    assert!(
        body.contains("pub fn gmres"),
        "body should be the function source, got: {body}"
    );
}

/// Regression for bug #5: `tokensave_diff_context.impacted_symbols` must not
/// list the same downstream node more than once. The sonium report showed
/// the same id appearing 6+ times consecutively when several modified
/// symbols all reached the same dependent.
#[tokio::test]
async fn diff_context_dedupes_impacted_symbols() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Two functions in `mod.rs` both call `shared` in `dep.rs`. Without dedup,
    // `shared` appears twice in `impacted_symbols`.
    fs::write(
        project.join("src/lib.rs"),
        r#"
mod dep;
pub fn first() { dep::shared(); }
pub fn second() { dep::shared(); }
"#,
    )
    .unwrap();
    fs::write(project.join("src/dep.rs"), "pub fn shared() {}\n").unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_diff_context",
        json!({"files": ["src/lib.rs"], "depth": 3}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let impacted = output["impacted_symbols"].as_array().unwrap();
    let mut ids: Vec<&str> = impacted.iter().filter_map(|v| v["id"].as_str()).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    let after = ids.len();
    assert_eq!(
        before, after,
        "impacted_symbols must not contain duplicates by id; got {before} entries, {after} unique"
    );
}

/// Regression for bug #6 / review P1: `tokensave_recursion` must preserve
/// genuine direct recursion while filtering length-1 self-edge artifacts.
#[tokio::test]
async fn recursion_keeps_direct_recursion() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn recurse(n: u32) -> u32 {
    if n == 0 { 0 } else { recurse(n - 1) }
}

pub fn nonrecursive() -> u32 { 42 }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let has_recurse = cycles.iter().any(|cycle| {
        cycle["chain"].as_array().is_some_and(|chain| {
            chain
                .iter()
                .filter_map(|n| n["name"].as_str())
                .filter(|name| *name == "recurse")
                .count()
                >= 2
        })
    });
    assert!(
        has_recurse,
        "direct self-recursive function should be reported; got {cycles:?}"
    );
}

#[tokio::test]
async fn recursion_filters_self_edge_artifacts() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let mentions_push = cycles.iter().any(|cycle| {
        cycle["chain"]
            .as_array()
            .is_some_and(|chain| chain.iter().any(|n| n["name"].as_str() == Some("push")))
    });
    assert!(
        !mentions_push,
        "`self.rows.push(...)` should not be reported as recursive; got {cycles:?}"
    );
}

#[tokio::test]
async fn recursion_reports_real_cycle_path() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn a() { b(); }
pub fn b() { c(); }
pub fn c() { a(); }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let chain = cycles
        .iter()
        .find_map(|cycle| {
            let chain = cycle["chain"].as_array()?;
            let names: Vec<&str> = chain.iter().filter_map(|n| n["name"].as_str()).collect();
            (names.len() == 4).then_some(names)
        })
        .expect("expected a three-node cycle path");
    let valid_edges = [("a", "b"), ("b", "c"), ("c", "a")];
    for pair in chain.windows(2) {
        assert!(
            valid_edges.contains(&(pair[0], pair[1])),
            "chain must follow real call edges; got {chain:?}"
        );
    }
}

/// Regression for bug #4: `tokensave_changelog`'s response must not list
/// directories under `files_not_indexed`. We construct a small git repo
/// with a real commit history that touches both a real file and a
/// (synthesised) directory path then verify the handler filters out the
/// directory.
#[tokio::test]
async fn changelog_filters_directory_paths() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(project)
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(project)
        .output()
        .unwrap();
    fs::create_dir_all(project.join("src/sub")).unwrap();
    fs::write(project.join("src/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(project)
        .output()
        .unwrap();
    fs::write(
        project.join("src/sub/keep.rs"),
        "pub fn k() { let _ = 1; }\n",
    )
    .unwrap();
    fs::write(project.join("src/sub/added.rs"), "pub fn a() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "two"])
        .current_dir(project)
        .output()
        .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly, not the index, and including the index sync subjects
    // this test to a pre-existing SyncLock contention flake.

    let result = handle_tool_call(
        &cg,
        "tokensave_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let changed: Vec<&str> = output["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for entry in &changed {
        let p = project.join(entry);
        assert!(
            !p.is_dir(),
            "changed_files must not include directories; got {entry:?}"
        );
    }
}

/// Regression for bug #8b: `tokensave_unused_imports` must actually flag
/// unused imports. The previous implementation tested `incoming.is_empty()`
/// for every Use node, but Use nodes always have at least one incoming
/// edge (from their containing module/file via Contains), so the
/// condition never fired and the tool returned 0 on every real codebase.
#[tokio::test]
async fn unused_imports_detects_truly_unused() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
use std::collections::HashMap;
use std::collections::HashSet;
mod inner;

pub fn used_one() -> HashMap<u32, u32> { HashMap::new() }
"#,
    )
    .unwrap();
    fs::write(project.join("src/inner.rs"), "pub fn inner_fn() {}\n").unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tokensave_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let imports = output["imports"].as_array().unwrap();
    let names: Vec<&str> = imports.iter().filter_map(|u| u["name"].as_str()).collect();
    // `HashSet` is imported but never used in the file body.
    assert!(
        names.iter().any(|n| n.contains("HashSet")),
        "HashSet should be reported as unused; got names={names:?}"
    );
}

/// Regression for bug #8a: `tokensave_dead_code` must support `include_public`
/// so agents can audit pub items with no callers in the indexed scope. The
/// previous SQL hard-coded `visibility != 'public'`, so on a codebase that
/// is mostly `pub` the tool reported 0 dead symbols.
#[tokio::test]
async fn dead_code_with_include_public_finds_pub_unreferenced() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn called() {}
pub fn never_called_anywhere() {}
pub fn caller() { called(); }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let default_result = handle_tool_call(&cg, "tokensave_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let default_text = extract_text(&default_result.value);
    let default_output: Value = serde_json::from_str(default_text).unwrap();
    assert_eq!(
        default_output["dead_code_count"].as_u64().unwrap_or(99),
        0,
        "default dead_code (no include_public) must still skip pub items"
    );

    let with_pub = handle_tool_call(
        &cg,
        "tokensave_dead_code",
        json!({"include_public": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let with_pub_text = extract_text(&with_pub.value);
    let with_pub_output: Value = serde_json::from_str(with_pub_text).unwrap();
    let symbols: Vec<&str> = with_pub_output["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        symbols.contains(&"never_called_anywhere"),
        "with include_public, the pub unreferenced fn should appear; got {symbols:?}"
    );
}

/// Regression for bug #7: `build_file_adjacency` previously included
/// `implements` and `extends` edges, which are heavily resolver-fuzzy-bound
/// to nonsense targets in unrelated files. After the fix, only `uses` and
/// `calls` edges count for file-level dependency depth.
#[tokio::test]
async fn dependency_depth_excludes_implements_and_extends() {
    // Public helper exposed from the lib for unit-test inspection.
    use tokensave::graph::queries::GraphQueryManager;
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // file_a derives Debug — extractor emits derives_macro and the
    // resolver historically pollutes implements edges across files.
    fs::write(
        project.join("src/lib.rs"),
        r#"
mod a;
mod b;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/a.rs"),
        r#"
#[derive(Debug, Clone)]
pub struct A;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        r#"
pub trait T {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let qm = GraphQueryManager::new(cg.db());
    let adj = qm.build_file_adjacency(None).await.unwrap();
    // Neither a.rs nor b.rs imports the other; the only edges between
    // them would come from implements/extends junk. After the fix, adj
    // should report no cross-file deps between the two leaf files.
    let from_a = adj.get("src/a.rs").cloned().unwrap_or_default();
    let from_b = adj.get("src/b.rs").cloned().unwrap_or_default();
    assert!(
        !from_a.contains("src/b.rs"),
        "src/a.rs must not depend on src/b.rs; got adj={from_a:?}"
    );
    assert!(
        !from_b.contains("src/a.rs"),
        "src/b.rs must not depend on src/a.rs; got adj={from_b:?}"
    );
}

/// Regression: `tokensave_run_affected_tests` must dispatch the test
/// functions that are themselves in `changed_paths`. Previously the
/// handler walked callers of every node in the changed file — but
/// `#[test]` functions are leaves with no callers, so a PR that only
/// edits `tests/foo.rs` would return "no tests cover the changed
/// paths" and skip running anything.
#[tokio::test]
async fn run_affected_tests_dispatches_directly_changed_test_files() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn util() -> u32 { 1 }\n").unwrap();
    fs::write(
        project.join("Cargo.toml"),
        r#"[package]
name = "t"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        project.join("tests/edited_only.rs"),
        r#"
#[test]
fn edited_only_test() {
    assert_eq!(2, 2);
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_run_affected_tests",
        json!({
            "changed_paths": ["tests/edited_only.rs"],
            "timeout_secs": 60,
            "max_tests": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    // If no tests get dispatched the handler short-circuits with a
    // note: "no tests cover the changed paths (1 file(s))". After the
    // fix, the test in the edited file itself must be dispatched.
    let dispatched = output["dispatched_tests"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        dispatched.iter().any(|n| n.contains("edited_only_test")),
        "expected edited_only_test to be dispatched; got dispatched={dispatched:?} note={:?}",
        output["note"]
    );
}

/// Regression: `tokensave_diagnose` must normalize span paths before
/// looking them up in the graph. cargo emits absolute and (on Windows)
/// backslash-separated paths; the graph stores project-relative,
/// forward-slash paths. Without normalization a diagnostic with span
/// `/abs/path/to/project/src/lib.rs:42:1` or `src\lib.rs:42:1` resolves
/// to `node: null` even though the file is indexed.
#[tokio::test]
async fn diagnose_normalizes_absolute_and_backslash_paths() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let abs_path = project.join("src/lib.rs");
    let abs_str = abs_path.to_string_lossy().to_string();
    let backslash_str = "src\\lib.rs";
    let cargo_output = format!(
        "error[E0001]: synthetic error\n  --> {abs_str}:1:1\n   |\n\nerror[E0002]: backslash form\n  --> {backslash_str}:1:1\n   |\n"
    );

    let result = handle_tool_call(
        &cg,
        "tokensave_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let mapped = output["mapped_to_node"].as_u64().unwrap_or(0);
    assert_eq!(
        mapped, 2,
        "both diagnostics should map to nodes after path normalization; got mapped={mapped} full={output:#}"
    );
}

/// Regression: PR8's resolver kind-compatibility filter must apply to
/// the same-file blocklist branches too. Without it, common names like
/// `new`/`default`/`clone` can still bind a `Calls` reference to a
/// non-callable same-file symbol — e.g. a const literally named
/// `default` — when it's the only same-file match for a blocklisted
/// name.
#[tokio::test]
async fn resolver_blocklist_branch_respects_kind_filter() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Use a struct named after a blocklisted identifier ("new") plus a
    // call site that the parser definitely treats as a call_expression.
    // Pre-fix the resolver's same-file blocklist branch would bind the
    // Calls ref to this struct because no other "new" lives in the file.
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct new;

pub fn caller() {
    let _ = new();
    helper();
}

pub fn helper() {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let caller_id = find_node_id(&cg, "caller").await;
    let result = handle_tool_call(
        &cg,
        "tokensave_callees",
        json!({"node_id": caller_id, "max_depth": 1, "resolve_dispatch": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Value = serde_json::from_str(text).unwrap();
    let arr = items.as_array().unwrap();
    for entry in arr {
        let kind = entry["kind"].as_str().unwrap_or("");
        let name = entry["name"].as_str().unwrap_or("");
        let callable = matches!(
            kind,
            "function" | "method" | "struct_method" | "constructor" | "macro" | "arrow_function"
        );
        assert!(
            callable,
            "caller's callees must be callable kinds; got name={name} kind={kind} full={arr:#?}"
        );
    }
}

/// Regression for bug #11: when an `impl Trait for X` reference cannot
/// resolve to a real trait node (e.g. `Default` lives in std and isn't
/// indexed), the resolver MUST NOT fuzzy-bind it to an unrelated node
/// kind. The sonium codebase had a parser `Token` enum whose `Default`
/// variant became the target of 150 stray `implements` edges from
/// manual `impl Default for X` blocks, completely poisoning
/// `tokensave_rank --edge-kind implements`. Implements/Extends/derives
/// references must only resolve to trait-shaped targets.
#[tokio::test]
async fn implements_refs_dont_resolve_to_enum_variants() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub enum Token { Default, Plus }

pub struct A;
impl Default for A { fn default() -> Self { A } }

pub struct B;
impl Default for B { fn default() -> Self { B } }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_rank",
        json!({"edge_kind": "implements", "direction": "incoming"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let ranking = output["ranking"].as_array().unwrap();
    for entry in ranking {
        let kind = entry["kind"].as_str().unwrap_or("");
        let name = entry["name"].as_str().unwrap_or("");
        assert!(
            kind != "enum_variant" && kind != "field",
            "implements edges must not target {kind} (got name={name})"
        );
    }
}

/// Regression for bug #10: `tokensave_circular` must report one entry per
/// strongly-connected component, not every walk through the cycle. The
/// sonium codebase had 73 "cycles" that were all different DFS paths
/// through the same SCC. After the SCC refactor, the same data yields
/// one entry per genuine component.
#[tokio::test]
async fn circular_reports_one_entry_per_scc_not_per_walk() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Three-file cycle: a uses b, b uses c, c uses a. Multiple DFS walks
    // through this triangle would have reported 3+ "cycles" pre-fix
    // (a→b→c→a, b→c→a→b, c→a→b→c).
    fs::write(project.join("src/lib.rs"), "mod a; mod b; mod c;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "use crate::b::b_fn;\npub fn a_fn() { b_fn(); }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::c::c_fn;\npub fn b_fn() { c_fn(); }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/c.rs"),
        "use crate::a::a_fn;\npub fn c_fn() { a_fn(); }\n",
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycle_count = output["cycle_count"].as_u64().unwrap();
    assert_eq!(
        cycle_count, 1,
        "three-file SCC must report exactly one cycle entry, got {cycle_count}"
    );
    let cycle = output["cycles"][0].as_array().unwrap();
    assert_eq!(
        cycle.len(),
        3,
        "the cycle should list all three files in the SCC; got {cycle:?}"
    );
}

/// Regression for bug #12: `tokensave_port_order`'s `cycles` output must
/// expose the SCCs forming each cycle separately, instead of collapsing
/// all unsorted nodes into a single mega-blob. Without this, on a real
/// codebase the cycle entry contained 200+ unrelated symbols and the
/// agent had no way to know what to break first.
#[tokio::test]
async fn port_order_reports_separate_scc_groups() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Two disjoint mutually-recursive pairs: (a, b) and (c, d). Before
    // the fix, both pairs would be lumped into a single "Mutual
    // dependency" entry. After the fix, each pair appears as its own
    // cycle group.
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub fn a() { b(); }
pub fn b() { a(); }
pub fn c() { d(); }
pub fn d() { c(); }
pub fn leaf() {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(
        cycles.len() >= 2,
        "expected at least 2 disjoint cycle groups; got {} entries: {cycles:?}",
        cycles.len()
    );
    // No cycle entry should mix both (a,b) and (c,d) names — that would
    // mean the fix didn't actually separate them. (Each symbol is now an
    // object: {name, kind, file, line, in_cycle_out_degree, ...}.)
    for c in cycles {
        let names: Vec<&str> = c["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["name"].as_str().or_else(|| s.as_str()))
            .collect();
        let has_ab = names.iter().any(|n| *n == "a" || *n == "b");
        let has_cd = names.iter().any(|n| *n == "c" || *n == "d");
        assert!(
            !(has_ab && has_cd),
            "one cycle entry contains both SCCs (a/b mixed with c/d): {names:?}"
        );
    }
}

/// Regression for new bug-report batch (#25): `tokensave_port_order` must
/// expose intra-cycle ordering signals so an agent can pick a starting
/// point inside a 200-symbol SCC instead of staring at an undifferentiated
/// blob. We expect each cycle entry to carry per-symbol in-cycle degree
/// data, a file-level member-count breakdown, and explicit `entry_point`
/// / `break_point_candidate` suggestions.
#[tokio::test]
async fn port_order_provides_intra_cycle_ordering() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // a → b → c → a, plus a "hub" h that all three call into and that
    // calls a back. h is the central node (highest in-cycle in-degree).
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub fn a() { b(); h(); }
pub fn b() { c(); h(); }
pub fn c() { a(); h(); }
pub fn h() { a(); }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(!cycles.is_empty(), "expected at least one cycle");
    let cycle = &cycles[0];
    assert!(
        cycle["files"].as_array().is_some(),
        "cycle must carry a `files` breakdown"
    );
    let files_arr = cycle["files"].as_array().unwrap();
    for f in files_arr {
        assert!(
            f.is_object() && f["members_in_cycle"].as_u64().is_some(),
            "files entries must be objects with `members_in_cycle`, got {f}"
        );
    }
    let symbols = cycle["symbols"].as_array().unwrap();
    for s in symbols {
        assert!(
            s["in_cycle_out_degree"].as_u64().is_some(),
            "each symbol must report in_cycle_out_degree; got {s}"
        );
        assert!(
            s["in_cycle_in_degree"].as_u64().is_some(),
            "each symbol must report in_cycle_in_degree; got {s}"
        );
    }
    assert!(
        cycle["entry_point"].is_object(),
        "cycle must surface a suggested entry_point; got {cycle}"
    );
    assert!(
        cycle["break_point_candidate"].is_object(),
        "cycle must surface a break_point_candidate; got {cycle}"
    );
    // The break point should be `h` (most internal callers).
    assert_eq!(
        cycle["break_point_candidate"]["name"].as_str(),
        Some("h"),
        "break_point_candidate should be the hub function `h`; got {cycle}"
    );
}

/// Regression for the Sonium port-order report: self-edges from fuzzy
/// resolution (`self.rows.push(...)` inside a method named `push`) should
/// not make singleton symbols appear as cycles.
#[tokio::test]
async fn port_order_ignores_self_edges() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(
        cycles.is_empty(),
        "self-edge-only methods should stay out of port_order cycles: {cycles:?}"
    );
}

/// Regression for bug #9: `tokensave_inheritance_depth` must surface Rust
/// supertrait chains (`trait T: U`) as `Extends` edges.
#[tokio::test]
async fn inheritance_depth_walks_rust_supertraits() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub trait Base {}
pub trait Middle: Base {}
pub trait Leaf: Middle {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_inheritance_depth", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let ranking = output["ranking"].as_array().unwrap();
    let names: Vec<&str> = ranking.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(
        names.contains(&"Leaf"),
        "expected Leaf trait in inheritance_depth ranking; got {names:?}"
    );
    let leaf = ranking
        .iter()
        .find(|r| r["name"].as_str() == Some("Leaf"))
        .unwrap();
    let depth = leaf["depth"].as_u64().unwrap();
    assert!(depth >= 2, "Leaf depth should be >= 2 hops, got {depth}");
}

/// Regression for new bug-report batch (#26): `tokensave_circular` must
/// emit *disjoint* SCCs — no file should appear in more than one cycle
/// entry. The sonium run reported 216 cycles "sharing long tails", which
/// would only be possible if the SCC condensation step were broken. This
/// stress test wires up many disjoint cycles plus DAG-style tails between
/// them and asserts no file leaks into a second cycle entry.
#[tokio::test]
async fn circular_emits_disjoint_sccs_under_load() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let mut lib_rs = String::new();
    // Build 5 disjoint 3-file cycles with shared DAG tails between them.
    // Cycle k = (a_k -> b_k -> c_k -> a_k); plus a one-way edge from c_k
    // to a_{k+1} that introduces a non-cyclic "shared tail" between the
    // SCCs. Tarjan must still emit each cycle as its own SCC.
    for k in 0..5 {
        lib_rs.push_str(&format!("pub mod a{k};\npub mod b{k};\npub mod c{k};\n"));
    }
    fs::write(project.join("src/lib.rs"), lib_rs).unwrap();
    for k in 0..5 {
        let next = (k + 1) % 5;
        fs::write(
            project.join(format!("src/a{k}.rs")),
            format!("use crate::b{k}::b_fn;\npub fn a_fn() {{ b_fn(); }}\n"),
        )
        .unwrap();
        fs::write(
            project.join(format!("src/b{k}.rs")),
            format!("use crate::c{k}::c_fn;\npub fn b_fn() {{ c_fn(); }}\n"),
        )
        .unwrap();
        fs::write(
            project.join(format!("src/c{k}.rs")),
            format!(
                "use crate::a{k}::a_fn;\nuse crate::a{next}::a_fn as next_a;\npub fn c_fn() {{ a_fn(); next_a(); }}\n"
            ),
        )
        .unwrap();
    }
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    // All cycles forming one giant SCC since c_k → a_{k+1} chains them.
    // The critical invariant is *disjointness*: no file appears twice.
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for cycle in cycles {
        let files = cycle.as_array().unwrap();
        for f in files {
            let s = f.as_str().unwrap().to_string();
            assert!(
                seen.insert(s.clone()),
                "file {s} appears in more than one cycle entry; SCCs must be disjoint"
            );
        }
    }
}

/// Regression for new bug-report batch (#24): `tokensave_diff_context`'s
/// `modified_symbols` must dedup by node id, even when callers pass the
/// same path multiple times in `files`. The sonium run showed an
/// `hmatrix.rs` file node listed 7× in a row because the caller had the
/// same file path duplicated upstream.
#[tokio::test]
async fn diff_context_dedupes_modified_symbols_on_duplicate_input() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct S; pub fn one() {} pub fn two() {}\n",
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tokensave_diff_context",
        json!({"files": ["src/lib.rs", "src/lib.rs", "src/lib.rs"], "depth": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let modified = output["modified_symbols"].as_array().unwrap();
    let mut ids: Vec<&str> = modified.iter().filter_map(|v| v["id"].as_str()).collect();
    let before = ids.len();
    ids.sort();
    ids.dedup();
    let after = ids.len();
    assert_eq!(
        before, after,
        "modified_symbols must not contain duplicate ids even when input has the same file 3×; got {before} entries, {after} unique"
    );
}

/// Regression for new bug-report batch (#23): when a whole subtree is
/// removed in a diff, `tokensave_changelog` must not report the deleted
/// directory under `files_not_indexed`. The previous `is_dir()` filter
/// missed this case because the path was gone from disk by the time we
/// checked. The fix uses gix's `entry_mode` flag to skip tree entries
/// before they're ever pushed into the change list.
#[tokio::test]
async fn changelog_filters_deleted_directory_entries() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fn git(cwd: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} failed"));
    }
    git(project, &["init"]);
    git(project, &["config", "user.email", "t@t"]);
    git(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("crates/sub")).unwrap();
    fs::write(project.join("crates/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);
    // Remove the whole subtree so gix's tree-diff yields a directory-mode
    // deletion entry.
    fs::remove_dir_all(project.join("crates")).unwrap();
    git(project, &["add", "-A"]);
    git(project, &["commit", "-m", "drop crates"]);
    let cg = TokenSave::init(project).await.unwrap();
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly and the sync lock has a pre-existing parallel-test flake.
    let result = handle_tool_call(
        &cg,
        "tokensave_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let changed: Vec<String> = output["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let problematic: Vec<&String> = changed.iter().filter(|p| !p.ends_with(".rs")).collect();
    assert!(
        problematic.is_empty(),
        "changed_files should be file paths only (no directories like 'crates' or 'crates/sub'); got problematic={problematic:?} full={changed:?}"
    );
}

/// Regression for new bug-report batch (#22): `tokensave_pr_context` must
/// NOT explode Cargo.toml (or any .toml/.yaml/.json config file) into one
/// symbol per `[name]`, `[version]`, `[dependencies]` key. On real PRs a
/// Cargo.toml change with ~30 dependency lines produced ~70 entries that
/// pushed the response past 760k tokens. Config files should collapse to
/// a single summary symbol.
#[tokio::test]
async fn pr_context_collapses_cargo_toml_keys() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fn git(cwd: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} failed"));
    }
    git(project, &["init"]);
    git(project, &["config", "user.email", "t@t"]);
    git(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);
    // Second commit: bloat Cargo.toml with many deps.
    let mut bloated = String::from(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    for i in 0..50 {
        bloated.push_str(&format!("dep{i} = \"0.1.{i}\"\n"));
    }
    fs::write(project.join("Cargo.toml"), &bloated).unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "deps"]);

    let cg = TokenSave::init(project).await.unwrap();
    // Intentionally skipping `index_all()` — pr_context reads the diff
    // from git directly and classifies Cargo.toml as `config` before any
    // index lookup, so we don't need the index to verify the collapse
    // behaviour. Calling `index_all()` here triggers the pre-existing
    // SyncLock parallel-test flake (#test_changelog_with_real_git).

    let result = handle_tool_call(
        &cg,
        "tokensave_pr_context",
        json!({"base_ref": "HEAD~1", "head_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let added = output["added"].as_array().unwrap();
    let modified = output["modified"].as_array().unwrap();
    let count_cargo = |arr: &[Value]| -> usize {
        arr.iter()
            .filter(|v| v["file"].as_str() == Some("Cargo.toml"))
            .count()
    };
    let cargo_total = count_cargo(added) + count_cargo(modified);
    assert!(
        cargo_total <= 1,
        "Cargo.toml should collapse to at most one summary symbol; got {cargo_total} entries. added={added:?}, modified={modified:?}"
    );
    // And the surviving entry must be a config summary, not a regular key.
    let summary = modified
        .iter()
        .find(|v| v["file"].as_str() == Some("Cargo.toml"));
    assert!(
        summary.is_some(),
        "expected one config_summary entry for Cargo.toml in modified; got {modified:?}"
    );
    assert_eq!(
        summary.unwrap()["kind"].as_str(),
        Some("config_summary"),
        "Cargo.toml entry should be kind=config_summary"
    );
}

/// Regression for new bug-report batch (#21): `tokensave_unused_imports`
/// must flag genuinely unused identifiers inside grouped `use foo::{A, B}`
/// imports. Real-world Rust style is dominated by grouped imports
/// (`use std::collections::{HashMap, HashSet, BTreeMap};`); without
/// per-identifier splitting, the heuristic could never flag anything from
/// a grouped import, which is why the user's run reported 0 / 3,404 use
/// nodes.
#[tokio::test]
async fn unused_imports_handles_grouped_use() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
use std::collections::{HashMap, HashSet};

pub fn used() -> HashMap<u32, u32> { HashMap::new() }
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tokensave_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let imports = output["imports"].as_array().unwrap();
    let payloads: Vec<String> = imports
        .iter()
        .map(|u| {
            format!(
                "{}::{}",
                u["name"].as_str().unwrap_or(""),
                u["unused"].as_str().unwrap_or("")
            )
        })
        .collect();
    let mentions_hashset = imports.iter().any(|u| {
        u["unused"].as_str().is_some_and(|s| s.contains("HashSet"))
            || u["name"].as_str().is_some_and(|n| n.contains("HashSet"))
    });
    assert!(
        mentions_hashset,
        "HashSet from grouped use should be reported as unused; got {payloads:?}"
    );
    // Critically, the *used* identifier HashMap must NOT be reported. If the
    // handler treats the whole grouped use as one opaque identifier it'll
    // either flag both or neither — both modes are wrong.
    let any_falsely_flags_hashmap = imports
        .iter()
        .any(|u| u["unused"].as_str().is_some_and(|s| s == "HashMap"));
    assert!(
        !any_falsely_flags_hashmap,
        "HashMap is used (HashMap::new()) and must not appear in `unused`; got {payloads:?}"
    );
}

/// Regression for new bug-report batch (#20): `tokensave_dead_code` must not
/// consider non-reference edges like `annotates` or `derives_macro` as
/// "this function is alive" evidence. Previously, a private helper with no
/// callers but an `#[inline]` (or any other attribute) on it had an
/// incoming `annotates` edge from the synthesised annotation_usage node,
/// which the SQL `NOT EXISTS (target = id AND kind != 'contains')` filter
/// accepted as a live reference. Real-world Rust codebases use attributes
/// pervasively, which is why the user's run found zero dead functions
/// across 5,715.
#[tokio::test]
async fn dead_code_flags_unreferenced_fn_with_attribute() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
fn caller() {
    used_helper();
}

#[inline]
fn used_helper() {}

#[inline]
fn dead_helper_with_attr() {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tokensave_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let symbols = output["symbols"].as_array().unwrap();
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"dead_helper_with_attr"),
        "private fn with #[inline] and no callers should be dead; got {names:?}"
    );
    assert!(
        !names.contains(&"used_helper"),
        "used_helper has a real caller and must NOT appear; got {names:?}"
    );
}

/// Regression for new bug-report batch (#19): `tokensave_search` must rank
/// trait/struct/function definitions above `use` re-exports of the same name.
/// Previously, several `use foo::LinearOperator;` lines could outrank the
/// `pub trait LinearOperator { … }` definition because BM25 scored short
/// re-export rows highly. We now force a kind tier ahead of BM25 score so a
/// real def always beats `use` rows.
#[tokio::test]
async fn search_ranks_trait_definition_above_use_reexports() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src/a")).unwrap();
    fs::create_dir_all(project.join("src/b")).unwrap();
    fs::create_dir_all(project.join("src/c")).unwrap();
    fs::create_dir_all(project.join("src/d")).unwrap();
    fs::create_dir_all(project.join("src/e")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub mod operator;
pub mod a;
pub mod b;
pub mod c;
pub mod d;
pub mod e;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/operator.rs"),
        "pub trait LinearOperator { fn apply(&self); }\n",
    )
    .unwrap();
    for sub in ["a", "b", "c", "d", "e"] {
        fs::write(
            project.join(format!("src/{sub}/mod.rs")),
            "pub use crate::operator::LinearOperator;\n",
        )
        .unwrap();
    }
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tokensave_search",
        json!({"query": "LinearOperator", "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Value = serde_json::from_str(text).unwrap();
    let arr = items.as_array().unwrap();
    let first_kind = arr[0]["kind"].as_str().unwrap_or("");
    assert_eq!(
        first_kind, "trait",
        "first search hit for LinearOperator should be the trait definition, got '{first_kind}' (full: {arr:?})"
    );
}

// ---------------------------------------------------------------------------
// McpServer::refresh_file_token_map
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_file_token_map_picks_up_new_files() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let cg = tokensave::tokensave::TokenSave::init(project)
        .await
        .unwrap();
    cg.sync().await.unwrap();

    let server = tokensave::mcp::McpServer::new(cg, None).await;
    let initial_map = server.file_token_map_snapshot();
    let initial_keys: std::collections::HashSet<_> = initial_map.keys().cloned().collect();

    // Add a new file, sync it, then refresh.
    std::fs::write(project.join("b.rs"), "fn b() { let y = 2; }").unwrap();
    let cg2 = tokensave::tokensave::TokenSave::open(project)
        .await
        .unwrap();
    cg2.sync().await.unwrap();

    server.refresh_file_token_map().await;
    let after_map = server.file_token_map_snapshot();
    let after_keys: std::collections::HashSet<_> = after_map.keys().cloned().collect();

    assert!(
        after_keys.len() > initial_keys.len(),
        "refresh should pick up b.rs"
    );
}

// ---------------------------------------------------------------------------
// McpServer-owned embedded watcher
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_owns_watcher_and_refreshes_token_map_on_change() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let cg = tokensave::tokensave::TokenSave::init(project)
        .await
        .unwrap();
    cg.sync().await.unwrap();

    let server = tokensave::mcp::McpServer::new(cg, None).await;
    assert!(
        server
            .wait_for_startup_catch_up(std::time::Duration::from_secs(2))
            .await,
        "startup catch-up sync should finish before mutating the project"
    );

    let initial_count = server.file_token_map_snapshot().len();

    // Edit a file, then drive the lazy staleness check that replaced the
    // notify-based watcher (#80). MCP `tools/call` triggers this on the
    // hot path; here we exercise the same pipeline directly so the test
    // doesn't have to wait through the 30 s cooldown gate in
    // `maybe_sync_if_stale`.
    std::fs::write(project.join("b.rs"), "fn b() {}").unwrap();
    let stale = server.cg().find_stale_files().await;
    assert!(
        !stale.is_empty(),
        "find_stale_files should detect newly written b.rs"
    );
    server.cg().sync_if_stale_silent(&stale).await.unwrap();
    let mut after_count = initial_count;
    for _ in 0..10 {
        server.refresh_file_token_map().await;
        after_count = server.file_token_map_snapshot().len();
        if after_count > initial_count {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        after_count > initial_count,
        "lazy sync should have refreshed map ({initial_count} -> {after_count})"
    );

    server.shutdown().await;
}
