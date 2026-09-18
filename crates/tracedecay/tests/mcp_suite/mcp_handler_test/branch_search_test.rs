#![cfg(feature = "test-transport")]

//! `tracedecay_branch_search` through the production MCP `tools/call` path.
//!
//! The tool answers from the immutable code-index generation sealed for the
//! named local branch's exact commit. A symbol that exists only in the dirty
//! worktree belongs to a different generation and must not be served as a
//! hit on that commit.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::test_support::git::GIT_FIXTURE_CONFIG;

const COMMITTED_SOURCE: &str = "pub fn committed_anchor() -> usize { 1 }\n";
const DIRTY_SOURCE: &str = "\
pub fn committed_anchor() -> usize { 1 }
pub fn dirty_anchor() -> usize { 2 }
";

#[tokio::test]
async fn branch_search_reads_committed_symbols_and_reports_a_missing_branch() {
    let isolation = TempDir::new().expect("branch-search isolation");
    let project = isolation.path().join("project");
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"branch-search-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
    fs::write(project.join("src/lib.rs"), COMMITTED_SOURCE).expect("committed source");
    git(&project, &["init", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "committed fixture"]);
    let commit = git(&project, &["rev-parse", "HEAD"]);
    let tree = git(&project, &["rev-parse", "HEAD^{tree}"]);

    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [project.clone()],
    ))
    .await
    .expect("production MCP composition");

    let (missing_refused, missing) = call(
        &harness,
        &project,
        json!({
            "branch": "missing-branch",
            "query": "committed_anchor",
            "limit": 5,
        }),
    )
    .await;
    assert!(
        missing_refused,
        "an unknown local branch is a typed refusal, not an empty hit list: {missing}"
    );
    assert_eq!(
        missing,
        json!({
            "status": "unavailable",
            "branch": "missing-branch",
            "reason": "branch_ref_not_found",
            "retryable": false,
        }),
        "unknown-branch payload"
    );

    let committed_hit = json!({
        "name": "committed_anchor",
        "qualified_name": "::committed_anchor",
        "kind": "function",
        "path": "src/lib.rs",
        "branch": "main",
        "source_reference": "refs/heads/main",
        "source_revision": commit,
        "source_tree": tree,
    });
    let (committed_refused, committed) = call(
        &harness,
        &project,
        json!({"branch": "main", "query": "committed_anchor", "limit": 5}),
    )
    .await;
    assert!(
        !committed_refused,
        "the committed symbol is a result, not a refusal: {committed}"
    );
    assert_complete_page(&committed, &commit, &tree, json!([committed_hit]));

    fs::write(project.join("src/lib.rs"), DIRTY_SOURCE).expect("dirty worktree source");
    let _refresh = call_raw(
        &harness,
        &project,
        "tracedecay_admin_sync",
        json!({"force": true, "format": "json"}),
    )
    .await;
    let dirty_worktree = wait_until_worktree_search_serves_dirty_anchor(&harness, &project).await;

    let (still_committed_refused, still_committed) = call(
        &harness,
        &project,
        json!({"branch": "main", "query": "committed_anchor", "limit": 5}),
    )
    .await;
    assert!(
        !still_committed_refused,
        "the committed generation stays readable behind a dirty worktree: {still_committed}"
    );
    assert_complete_page(
        &still_committed,
        &commit,
        &tree,
        json!([{
            "name": "committed_anchor",
            "qualified_name": "::committed_anchor",
            "kind": "function",
            "path": "src/lib.rs",
            "branch": "main",
            "source_reference": "refs/heads/main",
            "source_revision": commit,
            "source_tree": tree,
        }]),
    );
    assert_ne!(
        still_committed["code_generation"], dirty_worktree["code_generation"],
        "branch search must not answer from the dirty worktree generation: branch={still_committed} worktree={dirty_worktree}"
    );

    let (dirty_refused, dirty) = call(
        &harness,
        &project,
        json!({"branch": "main", "query": "dirty_anchor", "limit": 5}),
    )
    .await;
    assert!(
        !dirty_refused,
        "a miss on the committed generation is an empty page, not a refusal: {dirty}"
    );
    assert_complete_page(&dirty, &commit, &tree, json!([]));

    harness.shutdown().await;
}

fn assert_complete_page(payload: &Value, commit: &str, tree: &str, hits: Value) {
    assert_eq!(payload["status"], json!("complete"), "{payload}");
    assert_eq!(payload["reason"], Value::Null, "{payload}");
    assert_eq!(payload["branch"], json!("main"), "{payload}");
    assert_eq!(
        payload["source_reference"],
        json!("refs/heads/main"),
        "{payload}"
    );
    assert_eq!(payload["source_revision"], json!(commit), "{payload}");
    assert_eq!(payload["source_tree"], json!(tree), "{payload}");
    assert_eq!(payload["next_cursor"], Value::Null, "{payload}");
    let generation = payload["code_generation"]
        .as_str()
        .unwrap_or_else(|| panic!("branch search omitted its code generation: {payload}"));
    let results = payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("branch search omitted results: {payload}"));
    for hit in results {
        assert_eq!(hit["code_generation"], json!(generation), "{hit}");
    }
    assert_eq!(visible_hits(payload), hits, "{payload}");
}

fn visible_hits(payload: &Value) -> Value {
    Value::Array(
        payload["results"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|hit| {
                json!({
                    "name": hit["name"],
                    "qualified_name": hit["qualified_name"],
                    "kind": hit["kind"],
                    "path": hit["path"],
                    "branch": hit["branch"],
                    "source_reference": hit["source_reference"],
                    "source_revision": hit["source_revision"],
                    "source_tree": hit["source_tree"],
                })
            })
            .collect(),
    )
}

fn git(project: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(GIT_FIXTURE_CONFIG)
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} failed to spawn: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
}

async fn wait_until_worktree_search_serves_dirty_anchor(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..60 {
        let (refused, payload) = call_raw(
            harness,
            project,
            "tracedecay_search",
            json!({"query": "dirty_anchor", "limit": 5, "format": "json"}),
        )
        .await;
        last = payload;
        let served = !refused
            && last["code_generation"].as_str().is_some()
            && crate::common::incomplete_code_index_query_lanes(&last).is_empty()
            && search_names(&last).contains(&"dirty_anchor");
        if served {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("dirty worktree search never served dirty_anchor: {last}");
}

fn search_names(payload: &Value) -> Vec<&str> {
    payload["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|result| result["display"]["name"].as_str())
        .collect()
}

async fn call(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    arguments: Value,
) -> (bool, Value) {
    let (refused, payload) = call_raw(
        harness,
        project,
        "tracedecay_branch_search",
        with_json_format(arguments),
    )
    .await;
    if payload.get("truncated") != Some(&json!(true)) {
        return (refused, payload);
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("truncated branch search omitted its retrieve handle: {payload}"))
        .to_owned();
    let mut content = String::new();
    let mut offset = 0_u64;
    loop {
        let (page_refused, page) = call_raw(
            harness,
            project,
            "tracedecay_retrieve",
            json!({"handle": handle, "format": "json", "offset": offset}),
        )
        .await;
        assert!(
            !page_refused,
            "branch search response handle was refused: {page}"
        );
        content.push_str(
            page["content"]
                .as_str()
                .unwrap_or_else(|| panic!("branch search handle carried no content: {page}")),
        );
        if page["has_more"] != json!(true) {
            break;
        }
        let next_offset = page["next_offset"].as_u64().unwrap_or_else(|| {
            panic!("retrieve reported more pages without a next offset: {page}")
        });
        assert!(
            next_offset > offset,
            "retrieve did not advance past offset {offset}: {page}"
        );
        offset = next_offset;
    }
    let full = serde_json::from_str(&content).unwrap_or_else(|error| {
        panic!("truncated branch search handle is not JSON: {error}; content={content}")
    });
    (refused, full)
}

fn with_json_format(mut arguments: Value) -> Value {
    if let Some(arguments) = arguments.as_object_mut() {
        arguments
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    arguments
}

async fn call_raw(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool_name: &str,
    arguments: Value,
) -> (bool, Value) {
    let response = harness
        .call_tool(project, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} failed over MCP: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a transport error: {response:?}"
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no MCP result"));
    let refused = result["isError"] == json!(true);
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool_name} returned no text block: {result}"));
    let payload = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} text is not JSON: {error}; text={text}"));
    (refused, payload)
}
