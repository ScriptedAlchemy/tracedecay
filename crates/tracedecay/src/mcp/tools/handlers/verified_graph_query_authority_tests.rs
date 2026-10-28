use std::fs;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_contracts::retrieval::{
    CommitContextSummaryV1, CommitSymbolEntryV1, GitContextComparisonV1,
    GitContextSourceConsistencyV1,
};
use tracedecay_domain::GitOidV1;

use super::dispatch_test_support::{
    SelectorProfile, dispatch_on_graph_authority, registered_lib_fixture, tool_json,
    verified_graph_options,
};
use super::*;

fn graph_handlers_that_await_query() -> &'static [&'static str] {
    &[
        // Callers, callees, implementations, type hierarchy, and signature
        // search are application-surface reads served by the daemon.
        "tracedecay_impact",
        "tracedecay_node",
        // `tracedecay_similar` / `tracedecay_redundancy` bind clone-family
        // executors, never `verified_graph_query_port`. Behavioral coverage is
        // `clone_family_tools_refuse_absent_executors_without_awaiting_graph_query`.
        "tracedecay_rename_preview",
        "tracedecay_find_exact_symbol",
        "tracedecay_by_qualified_name",
        "tracedecay_signature",
        "tracedecay_derives",
        "tracedecay_files",
        "tracedecay_port_status",
        "tracedecay_port_order",
        "tracedecay_todos",
        "tracedecay_dead_code",
        "tracedecay_circular",
        "tracedecay_hotspots",
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
        // `tracedecay_diagnostics` is owned by the application surface; it
        // reaches the daemon transport, never the in-process graph query.
        "tracedecay_affected",
        "tracedecay_diff_context",
        "tracedecay_pr_context",
        // `tracedecay_changelog` is git-first and never awaits the query
        // future; its typed coverage is asserted by
        // `changelog_reports_absent_query_port_as_typed_coverage`.
        // `tracedecay_commit_context` reads exact sources from Git and never
        // awaits the query future either.
        "tracedecay_health",
        "tracedecay_test_map",
        "tracedecay_test_risk",
        "tracedecay_dsm",
        "tracedecay_gini",
        "tracedecay_dependency_depth",
        "tracedecay_diagnose",
        "tracedecay_run_affected_tests",
    ]
}

/// Clone-family tools bind their own executors. With the verified graph query
/// port absent and those executors missing, each tool must still decode a
/// schema-valid request and refuse with its executor-specific typed error,
/// never the graph-port unavailable reason the waiter inventory would imply.
#[tokio::test]
async fn clone_family_tools_refuse_absent_executors_without_awaiting_graph_query() {
    let dir = TempDir::new().expect("authority isolation");
    let profile = SelectorProfile::new(dir.path());
    let project = dir.path().join("clone-family-absent-executor");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn widget() {}\n").expect("write fixture");
    init_committed_git_fixture(&project);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.data_dir(),
        &project,
        "project.clone-family-absent-executor",
    )
    .await
    .expect("registered fixture");

    let options = lower_level_ports_without_query(&cg);
    assert!(
        options.verified_graph_query_port.is_none(),
        "fixture must leave the verified graph query port unmounted"
    );
    assert!(
        options.code_index_similar_executor.is_none(),
        "fixture must leave the similar executor unmounted"
    );
    assert!(
        options.code_index_redundancy_executor.is_none(),
        "fixture must leave the redundancy executor unmounted"
    );
    let scope = options
        .admitted_project_scope
        .as_ref()
        .expect("fixture must admit a project scope for schema-valid clone args");

    let cases = [
        (
            "tracedecay_similar",
            json!({
                "project_id": scope.project_id,
                "repository_id": scope.repository_id,
                "target": {
                    "kind": "symbol_occurrence",
                    "symbol_occurrence_id": "symbol.clone-family-absent-executor",
                },
                "match_classes": ["conservative_exact"],
                "result_limit": 10,
                "work_limit": 20,
            }),
            // Shared `reason.as_str()` wire (`CapabilityUnavailable`); retired
            // opaque tokens `verified-code-*-unavailable` must stay gone.
            "code_index_unavailable",
            "the maintained clone similarity lane is unavailable: code_index_unavailable",
        ),
        (
            "tracedecay_redundancy",
            json!({
                "project_id": scope.project_id,
                "repository_id": scope.repository_id,
                "match_classes": ["conservative_exact"],
                "scope": { "kind": "repository" },
                "include_generated_paths": false,
                "family_limit": 10,
                "member_limit": 10,
                "work_limit": 20,
            }),
            "code_index_unavailable",
            "the maintained clone family lane is unavailable: code_index_unavailable",
        ),
    ];

    for (tool_name, args, expected_reason, expected_detail) in cases {
        let outcome = dispatch_on_graph_authority(&cg, tool_name, args, options.clone()).await;
        let error = outcome.expect_err(tool_name);
        let (reason_code, retryable, detail) = error
            .project_route_context()
            .unwrap_or_else(|| panic!("{tool_name} must be a typed project route, got {error}"));
        assert_eq!(reason_code, expected_reason, "{tool_name}");
        assert!(!retryable, "{tool_name}");
        assert_eq!(detail, expected_detail, "{tool_name}");
    }
    cg.close();
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
/// handlers that validate git evidence before reading the graph reach that
/// read instead of reporting the typed git refusal first.
fn init_committed_git_fixture(root: &std::path::Path) {
    for args in [
        &["init", "--initial-branch=main"][..],
        &["add", "."][..],
        &["commit", "-m", "fixture"][..],
    ] {
        git_fixture_output(root, args);
    }
}

fn git_fixture_output(root: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout)
        .expect("fixture git output is UTF-8")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn commit_context_reads_source_without_query_authority() {
    let dir = TempDir::new().expect("authority isolation");
    let profile = SelectorProfile::new(dir.path());
    let project = dir.path().join("query-port-absent-commit-context");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn original() {}\n").expect("initial source");
    init_committed_git_fixture(&project);
    let head = GitOidV1::new(git_fixture_output(&project, &["rev-parse", "HEAD"]))
        .expect("pinned fixture HEAD");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn staged_change() {}\n",
    )
    .expect("staged source");
    git_fixture_output(&project, &["add", "src/lib.rs"]);
    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn staged_change() {}\npub fn worktree_change() {}\n",
    )
    .expect("working-tree source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.data_dir(),
        &project,
        "project.query-port-absent-commit-context",
    )
    .await
    .expect("registered fixture");

    let options = lower_level_ports_without_query(&cg);
    for (args, comparison, consistency, expected_names) in [
        (
            json!({ "format": "json" }),
            GitContextComparisonV1::HeadToWorktree {
                head: Some(head.clone()),
            },
            GitContextSourceConsistencyV1::PerFileWorktreeCapture,
            vec!["staged_change", "worktree_change"],
        ),
        (
            json!({ "staged_only": true, "format": "json" }),
            GitContextComparisonV1::Staged { head: Some(head) },
            GitContextSourceConsistencyV1::ImmutableBlobs,
            vec!["staged_change"],
        ),
    ] {
        let result =
            dispatch_on_graph_authority(&cg, "tracedecay_commit_context", args, options.clone())
                .await
                .expect("commit context must read source without a graph query");
        let summary: CommitContextSummaryV1 =
            serde_json::from_value(tool_json(&result)).expect("closed commit context contract");
        assert_eq!(summary.comparison, Some(comparison));
        assert!(summary.freshness.is_none());
        let coverage = summary.source_coverage.expect("source coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.gaps, Vec::new());
        assert_eq!(coverage.consistency, consistency);
        assert_eq!(summary.changed_files.len(), 1);
        assert_eq!(summary.changed_files[0].file, "src/lib.rs");
        assert_eq!(summary.changed_files[0].symbols, expected_names.len());
        let mut names = summary
            .symbols_by_role
            .values()
            .flatten()
            .map(|entry| match entry {
                CommitSymbolEntryV1::Symbol(symbol) => {
                    assert_eq!(symbol.file, "src/lib.rs");
                    symbol.name.as_str()
                }
                CommitSymbolEntryV1::ConfigSummary(_) => panic!("expected changed Rust functions"),
            })
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, expected_names);
    }
    cg.close();
}

#[tokio::test]
async fn pr_context_brief_refuses_missing_request_controls_with_a_mounted_query_port() {
    let dir = TempDir::new().expect("authority isolation");
    let profile = SelectorProfile::new(dir.path());
    let project = dir.path().join("pr-context-missing-controls");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn original() {}\n").expect("initial source");
    init_committed_git_fixture(&project);
    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn added() {}\n",
    )
    .expect("changed source");
    git_fixture_output(&project, &["add", "src/lib.rs"]);
    git_fixture_output(&project, &["commit", "-m", "add source"]);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.data_dir(),
        &project,
        "project.pr-context-missing-controls",
    )
    .await
    .expect("registered fixture");

    let options = verified_graph_options(&cg, ToolCallRegistryOptions::default());
    assert!(options.verified_graph_query_port.is_some());
    assert!(options.application_request_id.is_some());
    assert!(options.application_deadline.is_some());
    assert!(options.application_cancellation.is_some());
    for (missing_control, expected_detail) in [
        ("request_id", "the caller request identity is unavailable"),
        ("deadline", "the caller deadline is unavailable"),
        (
            "cancellation",
            "the caller cancellation signal is unavailable",
        ),
    ] {
        let mut options = options.clone();
        match missing_control {
            "request_id" => options.application_request_id = None,
            "deadline" => options.application_deadline = None,
            "cancellation" => options.application_cancellation = None,
            _ => unreachable!("only the declared controls are tested"),
        }
        let error = dispatch_on_graph_authority(
            &cg,
            "tracedecay_pr_context",
            json!({ "base_ref": "HEAD~1", "head_ref": "HEAD", "format": "json" }),
            options,
        )
        .await
        .expect_err("a brief must refuse an absent caller control");
        let (reason_code, retryable, detail) = error
            .project_route_context()
            .unwrap_or_else(|| panic!("{missing_control} must be typed, got {error}"));
        assert_eq!(
            reason_code, "verified-code-graph-read-unavailable",
            "{missing_control}"
        );
        assert!(!retryable, "{missing_control}");
        assert_eq!(detail, expected_detail, "{missing_control}");
    }
    cg.close();
}

/// The minimal arguments that carry each handler past its request-shape
/// validation, which by contract precedes graph admission: `run_affected_tests`
/// requires an explicit caller-scoped manifest, the file-seeded git-context
/// reads require their changed files, and PR context needs valid Git refs.
/// Every other awaiting handler reaches the graph wait with empty arguments.
fn query_authority_probe_args(tool_name: &str) -> serde_json::Value {
    match tool_name {
        "tracedecay_run_affected_tests" => json!({ "changed_paths": ["src/lib.rs"] }),
        "tracedecay_affected" | "tracedecay_diff_context" => json!({ "files": ["src/lib.rs"] }),
        "tracedecay_pr_context" => json!({ "base_ref": "HEAD", "head_ref": "HEAD" }),
        _ => json!({}),
    }
}

#[tokio::test]
async fn absent_query_port_fails_closed_for_every_awaiting_graph_handler() {
    let dir = TempDir::new().expect("authority isolation");
    let profile = SelectorProfile::new(dir.path());
    let project = dir.path().join("query-port-absent");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn widget() {}\n").expect("write fixture");
    init_committed_git_fixture(&project);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.data_dir(),
        &project,
        "project.query-port-absent",
    )
    .await
    .expect("registered fixture");

    let options = lower_level_ports_without_query(&cg);
    let mut seen = 0usize;
    for tool_name in graph_handlers_that_await_query() {
        let outcome = dispatch_on_graph_authority(
            &cg,
            tool_name,
            query_authority_probe_args(tool_name),
            options.clone(),
        )
        .await;
        let error = outcome.expect_err(tool_name);
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

/// Changelog is git-first: its diff is the answer and the verified graph query
/// only enriches it, so an absent query port is a typed
/// `symbol_changes_coverage` section carrying the same `code_index_unavailable`
/// token `CapabilityUnavailable` publishes, not the fail-closed refusal the
/// handlers in `graph_handlers_that_await_query` return.
#[tokio::test]
async fn changelog_reports_absent_query_port_as_typed_coverage() {
    let dir = TempDir::new().expect("authority isolation");
    let profile = SelectorProfile::new(dir.path());
    let project = dir.path().join("query-port-absent-changelog");
    fs::create_dir_all(project.join("src")).expect("fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn widget() {}\n").expect("write fixture");
    init_committed_git_fixture(&project);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.data_dir(),
        &project,
        "project.query-port-absent-changelog",
    )
    .await
    .expect("registered fixture");

    let result = dispatch_on_graph_authority(
        &cg,
        "tracedecay_changelog",
        // JSON so the coverage section is asserted as fields, not Markdown.
        json!({ "from_ref": "HEAD", "to_ref": "HEAD", "format": "json" }),
        lower_level_ports_without_query(&cg),
    )
    .await
    .expect("changelog must answer without a mounted verified graph query");
    let payload: serde_json::Value = tool_json(&result);
    assert_eq!(payload["status"], "partial");
    assert_eq!(payload["symbol_changes_coverage"]["status"], "unavailable");
    assert_eq!(
        payload["symbol_changes_coverage"]["reason"],
        "code_index_unavailable"
    );
    assert_eq!(payload["symbol_changes_coverage"]["retryable"], false);
    cg.close();
}

#[tokio::test]
async fn search_and_context_report_absent_query_port_as_typed_evidence() {
    let dir = TempDir::new().expect("authority isolation");
    let (cg, _runtime) = registered_lib_fixture(
        dir.path(),
        "query-port-absent-search",
        "pub fn widget() {}\n",
        "project.query-port-absent-search",
    )
    .await;

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
        let error = dispatch_on_graph_authority(&cg, tool_name, args, options.clone()).await;
        match error {
            Ok(result) => {
                let payload: serde_json::Value = tool_json(&result);
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
