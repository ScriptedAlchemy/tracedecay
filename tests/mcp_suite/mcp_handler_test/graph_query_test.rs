use crate::support::*;
use serde_json::{Value, json};
use std::fs;
#[cfg(feature = "test-transport")]
use std::process::Command;
#[cfg(feature = "test-transport")]
use tempfile::TempDir;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

// 1. tracedecay_search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fallback_allowed_without_executor_never_serves_legacy_fallback() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({
            "query": "helper",
            "semantic_mode": "fallback_allowed",
            "format": "json"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(0));
    assert_eq!(payload["status"].as_str(), Some("unavailable"));
    assert_eq!(
        payload["semantic"]["mode"].as_str(),
        Some("fallback_allowed")
    );
    assert_eq!(payload["semantic"]["status"].as_str(), Some("unavailable"));
}

#[tokio::test]
async fn test_grep_literal_hit_is_enriched_with_symbol() {
    let (cg, _dir) = setup_project().await;
    // `format!("Hello, {}!", name)` lives inside `format_greeting` in utils.rs.
    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "Hello, {}!", "fixed_strings": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let results = payload["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected a literal hit: {payload}");
    let hit = results
        .iter()
        .find(|hit| hit["file"].as_str() == Some("src/utils.rs"))
        .unwrap_or_else(|| panic!("expected a hit in src/utils.rs: {payload}"));
    assert!(hit["text"].as_str().unwrap().contains("Hello, {}!"));
    // Graph enrichment: the hit resolves to its enclosing symbol + node id so
    // the natural next call is `tracedecay_body`.
    assert_eq!(hit["symbol"].as_str(), Some("format_greeting"));
    assert!(
        hit["node_id"].as_str().is_some_and(|id| !id.is_empty()),
        "hit should carry a node_id for tracedecay_body: {payload}"
    );
    // The touched file should be surfaced for token-savings accounting.
    assert!(
        result.touched_files.contains(&"src/utils.rs".to_string()),
        "grep should report matched files as touched: {:?}",
        result.touched_files
    );
}

#[tokio::test]
async fn test_grep_regex_hit() {
    let (cg, _dir) = setup_project().await;
    // Regex: a `pub fn` declaration returning `String`.
    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": r"pub fn \w+\(\) -> String"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let results = payload["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|hit| hit["text"].as_str().unwrap().contains("pub fn helper")),
        "regex should match the helper declaration: {payload}"
    );
}

#[tokio::test]
async fn test_grep_no_match_reports_empty() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "zzz_no_such_token_anywhere_zzz"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(0));
    assert_eq!(payload["match_count"], 0);
    assert!(payload["files_scanned"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_grep_respects_gitignore() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    fs::write(root.join(".gitignore"), "ignored_dir/\n").unwrap();
    fs::create_dir_all(root.join("ignored_dir")).unwrap();
    fs::write(
        root.join("ignored_dir/secret.txt"),
        "UNIQUE_GITIGNORE_TOKEN\n",
    )
    .unwrap();
    fs::write(root.join("tracked.txt"), "UNIQUE_GITIGNORE_TOKEN\n").unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "UNIQUE_GITIGNORE_TOKEN"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let files: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["file"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"tracked.txt"),
        "tracked file should match: {payload}"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("ignored_dir")),
        "gitignored file must be skipped: {payload}"
    );
}

#[tokio::test]
async fn test_grep_skips_binary_files() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    // A NUL byte makes this file "binary"; the matching text must not surface.
    fs::write(root.join("blob.bin"), b"BINARY_MARKER\0BINARY_MARKER").unwrap();
    fs::write(root.join("plain.txt"), "BINARY_MARKER\n").unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "BINARY_MARKER"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let files: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["file"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"plain.txt"), "text file should match");
    assert!(
        !files.contains(&"blob.bin"),
        "binary file must be skipped: {payload}"
    );
}

#[tokio::test]
async fn test_grep_path_glob_filters_files() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    fs::write(root.join("notes.md"), "GLOB_TOKEN in markdown\n").unwrap();
    fs::write(root.join("src/extra.rs"), "// GLOB_TOKEN in rust\n").unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "GLOB_TOKEN", "path_glob": "*.md"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let files: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        vec!["notes.md"],
        "glob should restrict to *.md: {payload}"
    );
}

#[tokio::test]
async fn test_ast_grep_search_respects_scope_prefix() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    fs::write(
        root.join("src/outside_scope.rs"),
        "fn outside() { scope_probe(1); }\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/inside_scope.rs"),
        "fn inside() { scope_probe(2); }\n",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_search",
        json!({"pattern": "scope_probe($A)", "format": "json"}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let files: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["file"].as_str().unwrap())
        .collect();

    assert_eq!(files, vec!["tests/inside_scope.rs"], "{payload}");
}

#[tokio::test]
async fn test_grep_enforces_max_results_cap() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    let mut body = String::new();
    for _ in 0..10 {
        body.push_str("CAP_TOKEN line\n");
    }
    fs::write(root.join("many.txt"), body).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "CAP_TOKEN", "max_results": 3}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    assert_eq!(
        payload["results"].as_array().map(Vec::len),
        Some(3),
        "results must honor max_results: {payload}"
    );
    assert_eq!(
        payload["truncated"], true,
        "cap should mark truncated: {payload}"
    );
}

#[tokio::test]
async fn test_grep_case_sensitivity() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    fs::write(root.join("case.txt"), "MixedCaseToken here\n").unwrap();

    // Default: case-insensitive.
    let insensitive = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "mixedcasetoken"}),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !extract_json(&insensitive.value)["results"]
            .as_array()
            .unwrap()
            .is_empty(),
        "default search should be case-insensitive"
    );

    // case_sensitive: no match for the wrong case.
    let sensitive = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "mixedcasetoken", "case_sensitive": true}),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        extract_json(&sensitive.value)["results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "case-sensitive search should not match a different case"
    );
}

#[tokio::test]
async fn test_grep_markdown_routing_hint() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "helper", "format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("Grep Results"), "markdown heading: {text}");
    assert!(
        text.contains("tracedecay_body"),
        "markdown should point the agent at tracedecay_body: {text}"
    );
}

#[tokio::test]
async fn test_grep_context_lines() {
    let (cg, _dir) = setup_project().await;
    let root = cg.project_root().to_path_buf();
    fs::write(
        root.join("ctx.txt"),
        "line_before\nCONTEXT_TARGET\nline_after\n",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({"pattern": "CONTEXT_TARGET", "context_lines": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let hit = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["file"].as_str() == Some("ctx.txt"))
        .unwrap();
    assert_eq!(hit["before"][0], "line_before");
    assert_eq!(hit["after"][0], "line_after");
}

#[tokio::test]
async fn test_grep_missing_pattern_errors() {
    let (cg, _dir) = setup_project().await;
    let err = handle_tool_call(&cg, "tracedecay_grep", json!({}), None, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("pattern"),
        "missing pattern should be reported: {err}"
    );
}

#[tokio::test]
async fn test_find_exact_symbol_lazy_indexes_ignored_dependency_candidate() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("node_modules/pkg")).unwrap();
    fs::write(
        project.join("src/app.ts"),
        r#"import type { Foo } from "pkg";
export const value = 1;
"#,
    )
    .unwrap();
    fs::write(
        project.join("node_modules/pkg/index.d.ts"),
        "export interface Foo { value: string }\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let lookup = handle_tool_call(
        &cg,
        "tracedecay_find_exact_symbol",
        json!({
            "name": "Foo",
            "limit": 5,
            "format": "json",
            "lazy_index_ignored_dependencies": true
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&lookup.value)).unwrap();
    let db = cg.open_project_store_db().await.unwrap();
    let indexed_file = db
        .get_file("node_modules/pkg/index.d.ts")
        .await
        .unwrap()
        .is_some();
    assert!(indexed_file, "{payload}");
    assert_eq!(payload["count"].as_u64(), Some(1), "{payload}");
    assert_eq!(
        payload["matches"][0]["file"].as_str(),
        Some("node_modules/pkg/index.d.ts")
    );

    let body = handle_tool_call(
        &cg,
        "tracedecay_body",
        json!({"symbol": "Foo", "limit": 5, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&body.value)).unwrap();
    assert_eq!(payload["match_count"].as_u64(), Some(1));
    assert_eq!(
        payload["matches"][0]["body"].as_str(),
        Some("export interface Foo { value: string }")
    );
}

// ---------------------------------------------------------------------------
// 3. tracedecay_callers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers",
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
// 4. tracedecay_callees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callees() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callees",
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
// 5. tracedecay_impact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_impact() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_impact",
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
// 6. tracedecay_node — existing node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_existing() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_node",
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
// 7. tracedecay_node — nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_not_found() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_node",
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
// 9. tracedecay_files — no filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_no_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_files", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty(), "files listing should not be empty");
    assert!(text.starts_with("## Files"), "should have Files header");
    assert!(
        text.contains("**indexed files:**"),
        "should include indexed files field"
    );
    assert!(
        text.contains("```text"),
        "should render compact tree/list block"
    );
    assert!(!text.contains("|"), "files markdown should not use tables");
}

// ---------------------------------------------------------------------------
// 10. tracedecay_files — path filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_files", json!({"path": "src"}), None, None)
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

    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// 11. tracedecay_files — pattern filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_pattern_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
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
// 12. tracedecay_files — flat format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_flat_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"layout": "flat"}),
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

#[tokio::test]
async fn test_files_json_format_with_grouped_layout() {
    let (cg, _dir) = setup_project().await;
    let json_result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"format": "json", "layout": "grouped"}),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert_eq!(parsed["layout"], "grouped");
    assert!(parsed["files"].as_array().unwrap().iter().any(|file| {
        file["path"].as_str() == Some("src/main.rs")
            && file["symbols"].as_i64().unwrap_or_default() >= 1
    }));
}

// ---------------------------------------------------------------------------
// 13. tracedecay_affected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_affected",
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

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn affected_central_daemon_fixture_preserves_set_and_ranks_near_tests_over_mcp() {
    let isolation = TempDir::new().unwrap();
    let project = isolation.path().join("project");
    for directory in ["src/mcp", "tests"] {
        fs::create_dir_all(project.join(directory)).unwrap();
    }
    for (path, source) in [
        (
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod daemon;\npub mod mcp;\npub mod serve;\n",
        ),
        ("src/daemon.rs", "pub fn daemon() {}\n"),
        ("src/mcp/mod.rs", "pub mod server;\n"),
        (
            "src/mcp/server.rs",
            "use crate::daemon::daemon;\npub fn mcp_server() { daemon(); }\n",
        ),
        (
            "src/serve.rs",
            "use crate::mcp::server::mcp_server;\npub fn serve() { mcp_server(); }\n",
        ),
        (
            "tests/daemon_direct_test.rs",
            "use fixture::daemon::daemon;\n#[test]\nfn daemon_direct() { daemon(); }\n",
        ),
        (
            "tests/mcp_near_test.rs",
            "use fixture::mcp::server::mcp_server;\n#[test]\nfn mcp_near() { mcp_server(); }\n",
        ),
        (
            "tests/serve_transitive_test.rs",
            "use fixture::serve::serve;\n#[test]\nfn serve_transitive() { serve(); }\n",
        ),
        (
            "tests/unrelated_test.rs",
            "#[test]\nfn unrelated() { assert!(true); }\n",
        ),
    ] {
        fs::write(project.join(path), source).unwrap();
    }
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let response = harness
        .call_tool(
            &project,
            "tracedecay_affected",
            json!({"files": ["src/daemon.rs"], "depth": 5, "format": "json"}),
        )
        .await;
    let result = response
        .expect("production invocation succeeds")
        .result
        .unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).expect("affected JSON payload");

    assert_eq!(
        payload["affected_tests"],
        json!([
            "tests/daemon_direct_test.rs",
            "tests/mcp_near_test.rs",
            "tests/serve_transitive_test.rs"
        ]),
        "the compatibility list must remain exhaustive and path-sorted"
    );
    assert_eq!(
        payload["ranked_tests"],
        json!([
            {
                "path": "tests/daemon_direct_test.rs",
                "rank": 1,
                "distance": 1,
                "proximity": "direct"
            },
            {
                "path": "tests/mcp_near_test.rs",
                "rank": 2,
                "distance": 2,
                "proximity": "near"
            },
            {
                "path": "tests/serve_transitive_test.rs",
                "rank": 3,
                "distance": 3,
                "proximity": "transitive"
            }
        ])
    );
    assert_eq!(
        payload["recommended_tests"],
        json!(["tests/daemon_direct_test.rs", "tests/mcp_near_test.rs"])
    );
    assert_eq!(
        payload["ranking_metadata"]["strategy"],
        "dependency_distance_then_path"
    );
}

// ---------------------------------------------------------------------------
// 16. tracedecay_module_api
// ---------------------------------------------------------------------------

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn test_module_api() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let result =
        handle_real_server_tool_call(&server, "tracedecay_module_api", json!({"path": "src"}))
            .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();
    assert!(
        payload["problem"].is_null() && !payload["outcome"].is_null(),
        "production invocation must return retained module API evidence: {payload}"
    );
    assert_eq!(payload["outcome"]["outcome"], json!("evidence"));
    assert_eq!(payload["outcome"]["value"]["payload"]["path"], json!("src"));
    assert!(
        payload["outcome"]["value"]["payload"]["symbols"]
            .as_array()
            .is_some_and(|symbols| symbols.iter().any(|symbol| {
                symbol["display"]["name"] == json!("helper") || symbol["name"] == json!("helper")
            })),
        "production module API must return the public fixture symbol: {payload}"
    );
    fixture.harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// 18. tracedecay_hotspots
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hotspots() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_hotspots", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("hotspot_count"),
        "should have hotspot_count key"
    );
}

// ---------------------------------------------------------------------------
// 19. tracedecay_similar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_similar() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_similar",
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
// 22. tracedecay_rank
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
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
// 23. tracedecay_rank — invalid direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank_invalid_direction() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
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
// 24. tracedecay_largest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_largest() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_largest", json!({"limit": 5}), None, None)
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
// 25. tracedecay_coupling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coupling() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_coupling",
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
// 26. tracedecay_inheritance_depth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_inheritance_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_inheritance_depth",
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
// 27. tracedecay_distribution — default and summary mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_default() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_distribution", json!({}), None, None)
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
        "tracedecay_distribution",
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
// 29. tracedecay_complexity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(text.contains("formula"), "should have formula key");
}

// ---------------------------------------------------------------------------
// 30. tracedecay_doc_coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_doc_coverage", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("total_undocumented"),
        "should have total_undocumented key"
    );
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// 31. tracedecay_god_class
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_god_class() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_god_class", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 35. Unknown tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unknown_tool() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_unknown", json!({}), None, None).await;
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_search", json!({}), None, None).await;
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
    let result = handle_tool_call(&cg, "tracedecay_node", json!({"id": node_id}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("helper"),
        "node lookup via 'id' alias should still find the node"
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
        "tracedecay_coupling",
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
        "tracedecay_rank",
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
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_callers_missing_node_id() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_callers", json!({}), None, None).await;
    assert!(result.is_err(), "callers without node_id should error");
}

#[tokio::test]
async fn test_affected_missing_files() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_affected", json!({}), None, None).await;
    assert!(result.is_err(), "affected without files should error");
}

#[tokio::test]
async fn test_module_api_missing_path() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_module_api", json!({}), None, None).await;
    assert!(result.is_err(), "module_api without path should error");
}

#[tokio::test]
async fn test_rank_missing_edge_kind() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"direction": "incoming"}),
        None,
        None,
    )
    .await;
    assert!(result.is_err(), "rank without edge_kind should error");
}

#[tokio::test]
async fn test_similar_missing_symbol() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_similar", json!({}), None, None).await;
    assert!(result.is_err(), "similar without symbol should error");
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_distribution with path prefix filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_with_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_distribution",
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
// Extra: tracedecay_files — grouped format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_grouped_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"layout": "grouped"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    assert!(
        text.contains("indexed files"),
        "grouped format should have 'indexed files' header"
    );
    assert!(
        text.contains("**layout:** grouped"),
        "grouped format should report grouped layout"
    );
    assert!(
        text.contains("```text") && text.contains("src/"),
        "grouped format should show compact tree/list block"
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_affected with custom filter glob
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected_with_custom_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_affected",
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
// Extra: tracedecay_complexity — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity_response_fields() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("ranking").is_some(), "should have ranking key");
    assert!(parsed.get("formula").is_some(), "should have formula key");
    // Check ranking items have expected fields
    if let Some(items) = parsed["ranking"].as_array()
        && let Some(first) = items.first()
    {
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

// ---------------------------------------------------------------------------
// Extra: tracedecay_doc_coverage — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage_response_structure() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_doc_coverage", json!({}), None, None)
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
    if let Some(files) = parsed["files"].as_array()
        && let Some(first) = files.first()
    {
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

#[tokio::test]
async fn test_files_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // With scope_prefix "src", should only return files under src/
    let result = handle_tool_call(&cg, "tracedecay_files", json!({}), None, Some("src"))
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
async fn test_files_explicit_path_overrides_scope() {
    let (cg, _dir) = setup_project().await;
    // Explicit path "tests" should override scope_prefix "src"
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
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

// ---------------------------------------------------------------------------
// tracedecay_body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_body_returns_full_function_source() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_body", json!({}), None, None).await;
    assert!(result.is_err(), "should error when symbol is missing");
}

// ---------------------------------------------------------------------------
// tracedecay_callers_for — bulk caller lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers_for_returns_caller_set_per_id() {
    let (cg, _dir) = setup_project().await;

    // Look up two distinct targets in one call.
    let helper_id = find_node_id(&cg, "helper").await;
    let format_id = find_node_id(&cg, "format_greeting").await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
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
        "tracedecay_callers_for",
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
        "tracedecay_callers_for",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": ["function:0000000000000000000000000000ffff"], "kind": "not_a_real_kind"}),
        None,
        None,
    )
    .await;
    let Err(err) = result else {
        panic!("expected error for unknown edge kind");
    };
    assert!(format!("{err}").contains("unknown edge kind"));
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// tracedecay_by_qualified_name — cross-run lookup
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
        "tracedecay_by_qualified_name",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_by_qualified_name",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_by_qualified_name", json!({}), None, None).await;
    let Err(err) = result else {
        panic!("expected error when qualified_name is missing");
    };
    assert!(format!("{err}").contains("qualified_name"));
}

#[tokio::test]
async fn body_prefers_function_over_field_with_same_name() {
    let (cg, _dir) = setup_function_vs_field_collision().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
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
