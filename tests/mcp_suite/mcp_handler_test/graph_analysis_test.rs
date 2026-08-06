use crate::support::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

#[tokio::test]
async fn test_branch_list_reports_live_vs_serving_drift_state() {
    fn git(project: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("git command failed to spawn");
        assert!(
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    let dir = test_temp_dir();
    let project = dir.path();
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git(project, &["init"]);
    git(project, &["config", "user.email", "test@test.com"]);
    git(project, &["config", "user.name", "Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
    git(project, &["branch", "-M", "main"]);

    let cg = TestTraceDecay::new(TraceDecay::init(project).await.unwrap());
    cg.index_all().await.unwrap();
    let tracedecay_dir = resolve_layout_for_current_profile(project)
        .unwrap()
        .data_root;
    tracedecay::branch_meta::save_branch_meta(
        &tracedecay_dir,
        &tracedecay::branch_meta::BranchMeta::new("main"),
    )
    .unwrap();

    let cg = TestTraceDecay::new(TraceDecay::open(project).await.unwrap());
    git(project, &["checkout", "-b", "feature"]);

    let result = handle_tool_call(&cg, "tracedecay_branch_list", json!({}), None, None)
        .await
        .unwrap();
    let report: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(report["current_branch"], json!("feature"));
    assert_eq!(report["open_active_branch"], json!("main"));
    assert_eq!(report["serving_branch"], json!("main"));
    assert_eq!(report["branch_drifted"], json!(true));
    assert_eq!(report["branch_resolution"], json!("stale_serving_branch"));
}

// ---------------------------------------------------------------------------
// 14. tracedecay_dead_code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("dead_code_count"),
        "should have dead_code_count key"
    );
}

// ---------------------------------------------------------------------------
// 15. tracedecay_diff_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_diff_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
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
// 17. tracedecay_circular
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_circular() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 20. tracedecay_rename_preview
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_preview() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rename_preview",
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
// 21. tracedecay_unused_imports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unused_imports() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("unused_import_count"),
        "should have unused_import_count key"
    );
}

// ---------------------------------------------------------------------------
// 28. tracedecay_recursion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_recursion() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 32. tracedecay_changelog — requires git refs, expect graceful error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_changelog_no_git() {
    let (cg, _env, _dir) = setup_empty_project().await;
    // The temp dir is not a git repo, so this should return a structured git
    // error in the tool payload rather than success-looking prose.
    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("git"));
    assert_eq!(output["error"]["operation"].as_str(), Some("diff"));
    assert!(
        output["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("failed to open git repo")
    );
}

#[tokio::test]
async fn run_affected_tests_requires_manifest_scoped_changed_paths() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_run_affected_tests",
        json!({"timeout_secs": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("invalid_request"));
    assert_eq!(output["error"]["operation"].as_str(), Some("changed_paths"));
    assert!(
        output["note"].is_null(),
        "missing scope input must not be reported as a no-change note: {output}"
    );
}

#[tokio::test]
async fn pr_context_no_git_returns_structured_git_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_pr_context",
        json!({"base_ref": "HEAD~1", "head_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("git"));
    assert_eq!(output["error"]["operation"].as_str(), Some("diff"));
}

// ---------------------------------------------------------------------------
// 33. tracedecay_port_status — no matching dirs expected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_status() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
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
    close_test_graph(cg).await;
}

/// Regression: port_status used to match symbols purely on (name,
/// kind_compat_group), so common method names like `new`, `process`, `fmt`,
/// or `reset` produced wild cross-type "matches" — e.g. `Biquad::new` would
/// pair with an unrelated `Adaa::new` simply because both methods are named
/// "new". The match key must also include the parent type so siblings of
/// distinct owners stay unmatched.
#[tokio::test]
async fn port_status_does_not_match_methods_of_different_parents() {
    let dir = test_temp_dir();
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

    let (cg, _env) = init_test_project(project).await;
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
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
    let dir = test_temp_dir();
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

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
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
// 34. tracedecay_port_order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_order() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
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
// Extra: rename_preview with nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_preview_not_found() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rename_preview",
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

#[tokio::test]
async fn test_diff_context_missing_files() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_diff_context", json!({}), None, None).await;
    assert!(result.is_err(), "diff_context without files should error");
}

#[tokio::test]
async fn test_changelog_missing_refs() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_changelog", json!({}), None, None).await;
    assert!(result.is_err(), "changelog without from_ref should error");
}

#[tokio::test]
async fn test_port_status_missing_dirs() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_port_status", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_status without source_dir should error"
    );
}

#[tokio::test]
async fn test_port_order_missing_source_dir() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_port_order", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_order without source_dir should error"
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_changelog with a real git repo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commit_context_clean_worktree_returns_json() {
    let dir = test_temp_dir();
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
    fs::write(project.join(".gitignore"), ".tracedecay/\nhome/\n").unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn clean() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);

    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["summary"].as_str(), Some("No changes detected."));
    assert_eq!(output["changed_files"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        output["symbols_by_role"]
            .as_object()
            .map(serde_json::Map::len),
        Some(0)
    );
    assert!(output["recent_commits"].as_array().is_some());
}

#[tokio::test]
async fn test_changelog_with_real_git() {
    let dir = test_temp_dir();
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

    let (cg, _env) = init_test_project(project).await;
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
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
// Extra: tracedecay_dead_code with custom kinds parameter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code_custom_kinds() {
    let (cg, _dir) = setup_project().await;
    // Ask only for struct dead code
    let result = handle_tool_call(
        &cg,
        "tracedecay_dead_code",
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

/// Regression: `branch_diff` previously errored with `MCP error -32603: base
/// and head are the same branch` when base == head. `pr_context` handles the
/// same case gracefully (empty arrays); branch_diff must match that shape so
/// callers can rely on consistent behaviour.
#[tokio::test]
async fn branch_diff_returns_empty_when_base_equals_head() {
    let (cg, _env, _dir) = setup_empty_project().await;

    // branch_diff requires branch tracking metadata to be present.
    let tracedecay_dir = project_data_dir(&cg);
    let meta = tracedecay::branch_meta::BranchMeta::new("master");
    tracedecay::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_branch_diff",
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
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// tracedecay_gini
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_gini() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_gini",
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
    let result = handle_tool_call(&cg, "tracedecay_gini", json!({}), None, None)
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
// tracedecay_dependency_depth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dependency_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dependency_depth",
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
// tracedecay_health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_health_summary() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_health", json!({}), None, None)
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_health",
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

/// Issue #83: tracedecay_redundancy must surface AST-isomorphic duplicate
/// pairs and rank them by composite similarity. Plant two structurally
/// identical functions in a fixture and assert the pair surfaces in the
/// top hit with the `definite` severity bucket.
#[tokio::test]
async fn test_redundancy_finds_planted_duplicate() {
    let dir = test_temp_dir();
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

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_redundancy",
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
    assert!(
        top["ranking_score"].as_f64().unwrap_or(0.0) > 0.0,
        "top pair should expose ranking_score; full output: {text}"
    );
    assert!(
        top["signals"]["body_vector_cosine"].as_f64().is_some(),
        "top pair should expose body_vector_cosine; full output: {text}"
    );
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
    let groups = parsed["groups"].as_array().expect("groups array");
    assert!(
        groups
            .iter()
            .any(|group| group["size"].as_u64().unwrap_or(0) >= 2),
        "expected at least one duplicate group, got: {text}"
    );

    // Calling again should be a cache hit (no panic, same result).
    let result2 = handle_tool_call(
        &cg,
        "tracedecay_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(extract_text(&result2.value)).unwrap();
    assert_eq!(parsed2["pair_count"], parsed["pair_count"]);

    // The redundancy run persists its ranked pairs into the freshness-validated
    // `redundancy_pairs` cache so other surfaces can read them without
    // recomputing. At least the planted compute_a/compute_b pair lands.
    let cached_count = cg
        .db()
        .query_scalar_i64(
            "inspect redundancy cache",
            "SELECT COUNT(*) FROM redundancy_pairs",
        )
        .await
        .unwrap();
    assert!(
        cached_count >= 1,
        "redundancy run should populate the redundancy_pairs cache, got {cached_count}"
    );

    // Resolve compute_a's node id to exercise the fresh-pairs reader.
    let compute_a_id = cg
        .db()
        .query_scalar_text(
            "resolve redundancy fixture node",
            "SELECT id FROM nodes WHERE name = 'compute_a'",
        )
        .await
        .unwrap();

    // The reader serves the planted pair while both source hashes are fresh.
    let fresh = cg
        .db()
        .fresh_redundancy_pairs_for_node(&compute_a_id)
        .await
        .unwrap();
    assert!(
        !fresh.is_empty(),
        "fresh-pairs reader should return the planted duplicate for compute_a"
    );

    // Changing compute_a's cached fingerprint source_hash makes every pair it
    // participates in stale — the reader's freshness join must drop them.
    cg.db()
        .execute_write(
            "stale redundancy fingerprint fixture",
            "UPDATE node_fingerprints SET source_hash = 'stale-hash' WHERE node_id = ?1",
            (compute_a_id.clone(),),
        )
        .await
        .unwrap();
    let stale = cg
        .db()
        .fresh_redundancy_pairs_for_node(&compute_a_id)
        .await
        .unwrap();
    assert!(
        stale.is_empty(),
        "reader must filter rows whose stored source_hash no longer matches the fingerprint"
    );
}

/// Issue #82: `details=true` must surface raw counts + interpretation per
/// dimension, not just the scalar score, so callers don't have to compose
/// six separate tools to reproduce the breakdown.
#[tokio::test]
async fn test_health_detailed_includes_raw_signals() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_health",
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
// tracedecay_dsm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dsm_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "shape": "stats" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.starts_with("## Design Structure Matrix"));
    assert!(
        text.contains("**files:**"),
        "files field should exist, got: {}",
        text
    );
    assert!(
        text.contains("**density:**"),
        "density field should exist, got: {}",
        text
    );
    assert!(
        text.contains("### Top Clusters"),
        "default DSM markdown should include top clusters, got: {}",
        text
    );
}

#[tokio::test]
async fn test_dsm_json_returns_stats_shape() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "format": "json" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed["stats"].get("files").is_some(),
        "files field should exist, got: {}",
        text
    );
    assert!(
        parsed["stats"].get("density").is_some(),
        "density field should exist"
    );
    assert_eq!(parsed["shape"], "stats");
}

#[tokio::test]
async fn test_dsm_clusters() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "shape": "clusters" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("### Top Clusters"),
        "clusters section should exist, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// tracedecay_test_risk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_test_risk() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
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
    assert_eq!(
        summary["attribution"]["depth"].as_u64(),
        Some(3),
        "test-risk summary should advertise the calibrated attribution depth"
    );
    assert!(
        summary["buckets"]["attributed"].as_u64().is_some(),
        "summary should include calibrated attribution buckets, got: {}",
        text
    );
    assert_eq!(
        summary["confidence"].as_str(),
        Some("static_lower_bound"),
        "summary should label the calibrated coverage signal honestly"
    );
    assert!(parsed.get("risks").is_some(), "risks array should exist");
}

#[tokio::test]
async fn test_test_risk_distinguishes_direct_and_closure_attribution() {
    let (cg, _dir) = setup_integration_test_risk_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    assert_eq!(summary["total_functions"].as_u64(), Some(3));
    assert_eq!(summary["tested"].as_u64(), Some(2));
    assert_eq!(summary["coverage_pct"].as_f64(), Some(67.0));
    assert_eq!(
        summary["attribution"]["direct_unit_attributed"].as_u64(),
        Some(1)
    );
    assert_eq!(
        summary["attribution"]["closure_attributed"].as_u64(),
        Some(1)
    );
    assert_eq!(summary["buckets"]["attributed"].as_u64(), Some(2));
    assert_eq!(summary["buckets"]["orphan_entry"].as_u64(), Some(1));
    assert_eq!(summary["confidence"].as_str(), Some("static_lower_bound"));

    let risks = parsed["risks"]
        .as_array()
        .expect("risks should be an array");
    let public_entry = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("public_entry"))
        .expect("public_entry should appear in risk output");
    let format_greeting = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("format_greeting"))
        .expect("format_greeting should appear in risk output");
    let unused_public_api = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("unused_public_api"))
        .expect("unused_public_api should appear in risk output");

    assert_eq!(public_entry["has_test"].as_bool(), Some(true));
    assert_eq!(
        public_entry["attribution_method"].as_str(),
        Some("direct_unit")
    );
    assert_eq!(public_entry["attribution_depth"].as_u64(), Some(1));

    assert_eq!(format_greeting["has_test"].as_bool(), Some(true));
    assert_eq!(
        format_greeting["attribution_method"].as_str(),
        Some("closure")
    );
    assert_eq!(format_greeting["attribution_depth"].as_u64(), Some(2));

    assert_eq!(unused_public_api["has_test"].as_bool(), Some(false));
    assert_eq!(
        unused_public_api["attribution_method"].as_str(),
        Some("none")
    );
    assert!(
        summary["confidence_note"]
            .as_str()
            .is_some_and(|note| note.contains("closure")),
        "confidence note should explain the conservative closure signal, got: {}",
        text
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_risk_attributes_ts_describe_it_tests() {
    let (cg, _dir) = setup_ts_describe_it_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    // Only `add` is a source function (the it-callback lives in a .test.ts file
    // and is excluded from the denominator). It is directly unit-attributed via
    // the describe/it callback.
    assert_eq!(
        summary["total_functions"].as_u64(),
        Some(1),
        "only add() should count as a source function, got: {text}"
    );
    assert_eq!(
        summary["attribution"]["direct_unit_attributed"].as_u64(),
        Some(1),
        "add() should be direct-unit attributed via the it() callback, got: {text}"
    );

    let risks = parsed["risks"].as_array().expect("risks array");
    let add = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("add"))
        .expect("add should appear in risk output");
    assert_eq!(add["has_test"].as_bool(), Some(true));
    assert_eq!(add["attribution_method"].as_str(), Some("direct_unit"));
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_map_lists_ts_it_title_as_covering_test() {
    let (cg, _dir) = setup_ts_describe_it_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_map",
        json!({ "file": "src/math.ts" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    let coverage = parsed["coverage"].as_array().expect("coverage array");
    let add_cov = coverage
        .iter()
        .find(|c| c["source_name"].as_str() == Some("add"))
        .expect("add should be covered, got: {text}");
    let tests = add_cov["tests"].as_array().expect("tests array");
    assert!(
        tests
            .iter()
            .any(|t| t["test_name"].as_str() == Some("adds two numbers")),
        "test_map should list the it title as the covering test, got: {text}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_risk_excludes_non_src_functions_from_denominator_and_risks() {
    let (cg, _dir) = setup_test_risk_non_src_fixture().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    assert_eq!(summary["total_functions"].as_u64(), Some(3));
    assert_eq!(summary["buckets"]["attributed"].as_u64(), Some(2));
    assert_eq!(summary["buckets"]["orphan_entry"].as_u64(), Some(1));
    assert_eq!(summary["buckets"]["excluded"].as_u64(), Some(2));
    assert_eq!(
        summary["top_risk_untested"].as_str(),
        Some("unused_public_api")
    );

    let risks = parsed["risks"]
        .as_array()
        .expect("risks should be an array");
    assert!(
        risks
            .iter()
            .all(|item| item["file"].as_str() != Some("build.rs")),
        "non-src build script functions should be excluded from risk rows, got: {}",
        text
    );
    assert!(
        risks
            .iter()
            .all(|item| item["name"].as_str() != Some("build_script_helper")),
        "build script helper should not be ranked as source risk, got: {}",
        text
    );
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// tracedecay_todos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_todos_finds_markers() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tracedecay_todos", json!({}), None, None)
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
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_todos",
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
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 0);
    close_test_graph(cg).await;
}

/// Regression for bug #5: `tracedecay_diff_context.impacted_symbols` must not
/// list the same downstream node more than once. The sonium report showed
/// the same id appearing 6+ times consecutively when several modified
/// symbols all reached the same dependent.
#[tokio::test]
async fn diff_context_dedupes_impacted_symbols() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
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

/// Regression for bug #6 / review P1: `tracedecay_recursion` must preserve
/// genuine direct recursion while filtering length-1 self-edge artifacts.
#[tokio::test]
async fn recursion_keeps_direct_recursion() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
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
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
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
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
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

/// Regression for bug #4: `tracedecay_changelog`'s response must not list
/// directories under `files_not_indexed`. We construct a small git repo
/// with a real commit history that touches both a real file and a
/// (synthesised) directory path then verify the handler filters out the
/// directory.
#[tokio::test]
async fn changelog_filters_directory_paths() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly, not the index, and including the index sync subjects
    // this test to a pre-existing SyncLock contention flake.

    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
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

/// Regression for bug #8b: `tracedecay_unused_imports` must actually flag
/// unused imports. The previous implementation tested `incoming.is_empty()`
/// for every Use node, but Use nodes always have at least one incoming
/// edge (from their containing module/file via Contains), so the
/// condition never fired and the tool returned 0 on every real codebase.
#[tokio::test]
async fn unused_imports_detects_truly_unused() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
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

/// Regression for the "empty results on small crates" bug: an import named only
/// in a nearby comment (like the audit fixture's own
/// `// Planted unused import: BTreeMap …`) was read as "used" by the text scan
/// and never flagged. Asserts the masked scan flags it in BOTH markdown and
/// JSON with file:line, and that a genuinely-used import is NOT flagged.
#[tokio::test]
async fn unused_imports_reports_in_markdown_and_json() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // `HashMap` is used; `BTreeMap` is unused but named in the comment above it.
    fs::write(
        project.join("src/lib.rs"),
        "use std::collections::HashMap;\n\
         // Planted unused import: BTreeMap is referenced nowhere in real code.\n\
         use std::collections::BTreeMap;\n\
         \n\
         pub fn used_one() -> HashMap<u32, u32> { HashMap::new() }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    // Markdown (runtime default; request explicitly so the test helper does not
    // force-inject `format=json`).
    let md = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&md.value);
    assert!(text.contains("## Unused Imports"), "got: {text}");
    // `line` is the node's 0-based start line (source line 3 → 2), matching the
    // rest of the graph API.
    assert!(
        text.contains("**BTreeMap unused in src/lib.rs:2**"),
        "markdown must report the unused import with file:line: {text}"
    );
    assert!(
        !text.contains("HashMap unused"),
        "used import must not be flagged: {text}"
    );

    // JSON output must carry the same structured finding.
    let js = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&js.value)).unwrap();
    assert_eq!(payload["unused_import_count"], 1, "payload: {payload}");
    let imports = payload["imports"].as_array().unwrap();
    assert_eq!(imports.len(), 1, "payload: {payload}");
    let m = &imports[0];
    assert_eq!(m["unused"], "BTreeMap");
    assert_eq!(m["file"], "src/lib.rs");
    assert_eq!(m["line"], 2);
    // The used import must never appear.
    assert!(
        imports
            .iter()
            .all(|u| u["unused"].as_str() != Some("HashMap")),
        "used HashMap must not be flagged: {payload}"
    );
}

/// Rust's format macros implicitly capture identifiers named inside the format
/// string. Those captures are real references even though they are lexically
/// inside a string literal, so masking string noise must preserve them.
#[tokio::test]
async fn unused_imports_keeps_implicit_format_capture() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "use std::f64::consts::PI;\n\
         pub fn print_pi() { println!(\"{PI}\"); }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let imports = payload["imports"].as_array().unwrap();
    assert!(
        imports
            .iter()
            .all(|item| item["unused"].as_str() != Some("PI")),
        "PI is used by the implicit format capture and must not be flagged: {payload}"
    );
}

/// Regression for bug #8a: `tracedecay_dead_code` must support `include_public`
/// so agents can audit pub items with no callers in the indexed scope. The
/// previous SQL hard-coded `visibility != 'public'`, so on a codebase that
/// is mostly `pub` the tool reported 0 dead symbols.
#[tokio::test]
async fn dead_code_with_include_public_finds_pub_unreferenced() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let default_result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
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
        "tracedecay_dead_code",
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
    use tracedecay::graph::queries::GraphQueryManager;
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
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

/// Regression: `tracedecay_diagnose` must normalize span paths before
/// looking them up in the graph. cargo emits absolute and (on Windows)
/// backslash-separated paths; the graph stores project-relative,
/// forward-slash paths. Without normalization a diagnostic with span
/// `/abs/path/to/project/src/lib.rs:42:1` or `src\lib.rs:42:1` resolves
/// to `node: null` even though the file is indexed.
#[tokio::test]
async fn diagnose_normalizes_absolute_and_backslash_paths() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let abs_path = project.join("src/lib.rs");
    let abs_str = abs_path.to_string_lossy().to_string();
    let backslash_str = "src\\lib.rs";
    let cargo_output = format!(
        "error[E0001]: synthetic error\n  --> {abs_str}:1:1\n   |\n\nerror[E0002]: backslash form\n  --> {backslash_str}:1:1\n   |\n"
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
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

/// `tracedecay_diagnose` cross-references the redundancy index: when the
/// enclosing node of a diagnostic has a cached fingerprint, near-duplicate
/// functions surface under `near_duplicates`. Plant two AST-isomorphic
/// functions, warm the fingerprint cache via `tracedecay_redundancy`, then
/// diagnose a synthetic error on one and assert the other is reported.
#[tokio::test]
async fn diagnose_surfaces_near_duplicates_from_redundancy_cache() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
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
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    // Warm the fingerprint cache so diagnose has something to read.
    handle_tool_call(
        &cg,
        "tracedecay_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();

    // Craft a diagnostic whose span lands inside compute_a.
    let a_id = find_node_id(&cg, "compute_a").await;
    let a_node = cg.get_node(&a_id).await.unwrap().expect("compute_a node");
    let diag_line = a_node.start_line + 1; // 0-based start -> 1-based span line
    let cargo_output =
        format!("error[E0001]: synthetic error\n  --> src/lib.rs:{diag_line}:5\n   |\n");

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();

    let diagnostics = output["diagnostics"].as_array().expect("diagnostics");
    let diag = diagnostics
        .iter()
        .find(|d| d["node"]["name"].as_str() == Some("compute_a"))
        .unwrap_or_else(|| panic!("diagnostic did not map to compute_a: {output:#}"));
    let dupes = diag["near_duplicates"]
        .as_array()
        .unwrap_or_else(|| panic!("near_duplicates missing: {output:#}"));
    assert!(
        dupes
            .iter()
            .any(|d| d["name"].as_str() == Some("compute_b")),
        "expected compute_b in near_duplicates, got: {output:#}"
    );
    let top = &dupes[0];
    assert_eq!(top["name"].as_str(), Some("compute_b"));
    assert_eq!(top["overlap_kind"].as_str(), Some("ast_isomorphic"));
    assert_eq!(top["severity"].as_str(), Some("definite"));
    assert!(top["ranking_score"].as_f64().unwrap_or(0.0) > 0.0);
}

/// When no fingerprint is cached for the enclosing node, `diagnose` must not
/// parse or warm files — it silently reports an empty `near_duplicates` list.
#[tokio::test]
async fn diagnose_near_duplicates_absent_without_cached_fingerprint() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn compute_a(value: i32) -> i32 {
    let mut acc = 0;
    for i in 0..value {
        acc += i;
    }
    acc
}

pub fn compute_b(input: i32) -> i32 {
    let mut total = 0;
    for j in 0..input {
        total += j;
    }
    total
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    // Deliberately do NOT run tracedecay_redundancy — no fingerprints cached.

    let a_id = find_node_id(&cg, "compute_a").await;
    let a_node = cg.get_node(&a_id).await.unwrap().expect("compute_a node");
    let diag_line = a_node.start_line + 1;
    let cargo_output =
        format!("error[E0001]: synthetic error\n  --> src/lib.rs:{diag_line}:5\n   |\n");

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();

    let diagnostics = output["diagnostics"].as_array().expect("diagnostics");
    let diag = diagnostics
        .iter()
        .find(|d| d["node"]["name"].as_str() == Some("compute_a"))
        .unwrap_or_else(|| panic!("diagnostic did not map to compute_a: {output:#}"));
    let dupes = diag["near_duplicates"]
        .as_array()
        .unwrap_or_else(|| panic!("near_duplicates missing: {output:#}"));
    assert!(
        dupes.is_empty(),
        "expected no near_duplicates without cached fingerprints, got: {output:#}"
    );
}

/// Regression: the resolver's kind-compatibility filter must apply to
/// the same-file blocklist branches too. Without it, common names like
/// `new`/`default`/`clone` can still bind a `Calls` reference to a
/// non-callable same-file symbol — e.g. a const literally named
/// `default` — when it's the only same-file match for a blocklisted
/// name.
#[tokio::test]
async fn resolver_blocklist_branch_respects_kind_filter() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let caller_id = find_node_id(&cg, "caller").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callees",
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
/// `tracedecay_rank --edge-kind implements`. Implements/Extends/derives
/// references must only resolve to trait-shaped targets.
#[tokio::test]
async fn implements_refs_dont_resolve_to_enum_variants() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
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

/// Regression for bug #10: `tracedecay_circular` must report one entry per
/// strongly-connected component, not every walk through the cycle. The
/// sonium codebase had 73 "cycles" that were all different DFS paths
/// through the same SCC. After the SCC refactor, the same data yields
/// one entry per genuine component.
#[tokio::test]
async fn circular_reports_one_entry_per_scc_not_per_walk() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycle_count = output["cycle_count"].as_u64().unwrap();
    assert_eq!(
        cycle_count, 1,
        "three-file SCC must report exactly one cycle entry, got {cycle_count}"
    );
    let cycle = &output["cycles"][0];
    assert_eq!(
        cycle["member_count"].as_u64(),
        Some(3),
        "the cycle should account for all three files in the SCC; got {cycle:?}"
    );
    assert_eq!(
        cycle["members"].as_array().map(Vec::len),
        Some(3),
        "three members fit the default member bound; got {cycle:?}"
    );
    assert_eq!(cycle["omitted_member_count"].as_u64(), Some(0));
}

/// Regression for bug #12: `tracedecay_port_order`'s `cycles` output must
/// expose the SCCs forming each cycle separately, instead of collapsing
/// all unsorted nodes into a single mega-blob. Without this, on a real
/// codebase the cycle entry contained 200+ unrelated symbols and the
/// agent had no way to know what to break first.
#[tokio::test]
async fn port_order_reports_separate_scc_groups() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
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

/// Regression for new bug-report batch (#25): `tracedecay_port_order` must
/// expose intra-cycle ordering signals so an agent can pick a starting
/// point inside a 200-symbol SCC instead of staring at an undifferentiated
/// blob. We expect each cycle entry to carry per-symbol in-cycle degree
/// data, a file-level member-count breakdown, and explicit `entry_point`
/// / `break_point_candidate` suggestions.
#[tokio::test]
async fn port_order_provides_intra_cycle_ordering() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
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
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
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

/// Regression for bug #9: `tracedecay_inheritance_depth` must surface Rust
/// supertrait chains (`trait T: U`) as `Extends` edges.
#[tokio::test]
async fn inheritance_depth_walks_rust_supertraits() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_inheritance_depth", json!({}), None, None)
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

/// Regression for new bug-report batch (#26): `tracedecay_circular` must
/// emit *disjoint* SCCs — no file should appear in more than one cycle
/// entry. The sonium run reported 216 cycles "sharing long tails", which
/// would only be possible if the SCC condensation step were broken. This
/// stress test wires up many disjoint cycles plus DAG-style tails between
/// them and asserts no file leaks into a second cycle entry.
#[tokio::test]
async fn circular_emits_disjoint_sccs_under_load() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let mut lib_rs = String::new();
    // Build 5 disjoint 3-file cycles with shared DAG tails between them.
    // Cycle k = (a_k -> b_k -> c_k -> a_k); plus a one-way edge from c_k
    // to a_{k+1} that introduces a non-cyclic "shared tail" between the
    // SCCs. Tarjan must still emit each cycle as its own SCC.
    for k in 0..5 {
        let _ = write!(lib_rs, "pub mod a{k};\npub mod b{k};\npub mod c{k};\n");
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    // Disjointness is only observable when every member is listed, so raise the
    // member bound above this fixture's 15-file component.
    let result = handle_tool_call(
        &cg,
        "tracedecay_circular",
        json!({"member_limit": 200}),
        None,
        None,
    )
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
        assert_eq!(
            cycle["omitted_member_count"].as_u64(),
            Some(0),
            "the raised member bound must list every member; got {cycle:?}"
        );
        let files = cycle["members"].as_array().unwrap();
        for f in files {
            let s = f.as_str().unwrap().to_string();
            assert!(
                seen.insert(s.clone()),
                "file {s} appears in more than one cycle entry; SCCs must be disjoint"
            );
        }
    }
}

/// Regression for new bug-report batch (#24): `tracedecay_diff_context`'s
/// `modified_symbols` must dedup by node id, even when callers pass the
/// same path multiple times in `files`. The sonium run showed an
/// `hmatrix.rs` file node listed 7× in a row because the caller had the
/// same file path duplicated upstream.
#[tokio::test]
async fn diff_context_dedupes_modified_symbols_on_duplicate_input() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct S; pub fn one() {} pub fn two() {}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
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
/// removed in a diff, `tracedecay_changelog` must not report the deleted
/// directory under `files_not_indexed`. The previous `is_dir()` filter
/// missed this case because the path was gone from disk by the time we
/// checked. The fix uses gix's `entry_mode` flag to skip tree entries
/// before they're ever pushed into the change list.
#[tokio::test]
async fn changelog_filters_deleted_directory_entries() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly and the sync lock has a pre-existing parallel-test flake.
    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
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

/// Regression for new bug-report batch (#22): `tracedecay_pr_context` must
/// NOT explode Cargo.toml (or any .toml/.yaml/.json config file) into one
/// symbol per `[name]`, `[version]`, `[dependencies]` key. On real PRs a
/// Cargo.toml change with ~30 dependency lines produced ~70 entries that
/// pushed the response past 760k tokens. Config files should collapse to
/// a single summary symbol.
#[tokio::test]
async fn pr_context_collapses_cargo_toml_keys() {
    let dir = test_temp_dir();
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
        let _ = writeln!(bloated, "dep{i} = \"0.1.{i}\"");
    }
    fs::write(project.join("Cargo.toml"), &bloated).unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "deps"]);

    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all()` — pr_context reads the diff
    // from git directly and classifies Cargo.toml as `config` before any
    // index lookup, so we don't need the index to verify the collapse
    // behaviour. Calling `index_all()` here triggers the pre-existing
    // SyncLock parallel-test flake (#test_changelog_with_real_git).

    let result = handle_tool_call(
        &cg,
        "tracedecay_pr_context",
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

/// Regression for new bug-report batch (#21): `tracedecay_unused_imports`
/// must flag genuinely unused identifiers inside grouped `use foo::{A, B}`
/// imports. Real-world Rust style is dominated by grouped imports
/// (`use std::collections::{HashMap, HashSet, BTreeMap};`); without
/// per-identifier splitting, the heuristic could never flag anything from
/// a grouped import, which is why the user's run reported 0 / 3,404 use
/// nodes.
#[tokio::test]
async fn unused_imports_handles_grouped_use() {
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
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

/// Regression for new bug-report batch (#20): `tracedecay_dead_code` must not
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
    let dir = test_temp_dir();
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
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
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

// ---------------------------------------------------------------------------
// McpServer::refresh_file_token_map
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_file_token_map_picks_up_new_files() {
    let tmp = test_temp_dir();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.sync().await.unwrap();

    let server = tracedecay::mcp::McpServer::new(cg.into_inner(), None).await;
    let initial_map = server.file_token_map_snapshot();
    let initial_keys: std::collections::HashSet<_> = initial_map.keys().cloned().collect();

    // Add a new file, sync it, then refresh.
    std::fs::write(project.join("b.rs"), "fn b() { let y = 2; }").unwrap();
    let cg2 = TestTraceDecay::new(
        tracedecay::tracedecay::TraceDecay::open(project)
            .await
            .unwrap(),
    );
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
    let tmp = test_temp_dir();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.sync().await.unwrap();
    let mut config = tracedecay::config::load_config(project).expect("load test config");
    config.sync.session_start_sync = false;
    tracedecay::config::save_config(project, &config).expect("disable unrelated catch-up");

    let server = tracedecay::mcp::McpServer::new(cg.into_inner(), None).await;

    let initial_count = server.file_token_map_snapshot().len();

    // Edit a file, then drive the lazy staleness check that replaced the
    // notify-based watcher (#80). MCP `tools/call` triggers this on the
    // hot path; here we exercise the same pipeline directly so the test
    // doesn't have to wait through the 30 s cooldown gate in
    // `maybe_sync_if_stale`.
    std::fs::write(project.join("b.rs"), "fn b() {}").unwrap();
    let server_cg = server.cg().await;
    let mut stale = Vec::new();
    for _ in 0..20 {
        stale = server_cg.find_stale_files().await;
        if !stale.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !stale.is_empty(),
        "find_stale_files should detect newly written b.rs"
    );
    server_cg.sync_if_stale_silent(&stale).await.unwrap();
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

#[tokio::test]
async fn simplify_scan_surfaces_store_failure_instead_of_no_findings() {
    let (cg, _dir) = setup_project().await;
    break_edges_table(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_simplify_scan",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "a failing store query must produce a tool error, not an empty findings list"
    );
}

#[tokio::test]
async fn type_hierarchy_surfaces_store_failure_instead_of_empty_tree() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    break_edges_table(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "a failing store query must produce a tool error, not an empty hierarchy"
    );
}

/// Regression test for the empty-output bug: `tracedecay_unsafe_patterns`
/// detected the unsafe block (the JSON payload was correct) but the Markdown
/// renderer dropped every finding and printed "No diagnostics.", so agents saw
/// nothing. The default (Markdown) response must now surface the site.
#[tokio::test]
async fn unsafe_patterns_reports_unsafe_block_in_markdown_and_json() {
    let (cg, _project) = setup_unsafe_block_fixture().await;

    // Markdown is the runtime default; request it explicitly so the test helper
    // (which force-injects `format=json` for tools outside its allowlist) does
    // not override it.
    let md = handle_tool_call(
        &cg,
        "tracedecay_unsafe_patterns",
        json!({"format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&md.value);
    assert!(
        !text.contains("No diagnostics"),
        "renderer regression: markdown swallowed the finding: {text}"
    );
    assert!(text.contains("## Risky Patterns"), "got: {text}");
    assert!(
        text.contains("UNSAFE_BLOCK at src/lib.rs:7"),
        "markdown must report the unsafe block with file:line: {text}"
    );
    assert!(
        text.contains("raw_total_len"),
        "markdown must name the enclosing symbol: {text}"
    );

    // JSON output must carry the same structured match.
    let js = handle_tool_call(
        &cg,
        "tracedecay_unsafe_patterns",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&js.value)).unwrap();
    assert_eq!(payload["match_count"], 1, "payload: {payload}");
    assert_eq!(payload["by_kind"]["unsafe_block"], 1, "payload: {payload}");
    let m = &payload["matches"][0];
    assert_eq!(m["kind"], "unsafe_block");
    assert_eq!(m["file"], "src/lib.rs");
    assert_eq!(m["line"], 7);
    assert!(
        m["enclosing"]
            .as_str()
            .unwrap_or_default()
            .ends_with("raw_total_len"),
        "payload: {payload}"
    );

    // The safe function must NOT be flagged.
    assert!(
        !text.contains("safe_add"),
        "safe code should produce no findings: {text}"
    );
}
