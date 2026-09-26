//! `tracedecay_branch_list` answers with exact local `refs/heads` snapshots.
//!
//! A branch name is a git ref, not a branch-database selector. Remote-tracking
//! refs and tags are not local branches, so they must not appear in the page.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::support::test_temp_dir;

const HEAD_COMMIT: &str = "dbc21220c25f50fce6ac93b6e7859062cd3d3ca8";

const COMPLETE_JSON: &str = "\
{\"examined\":4,\"limit\":100,\"next_after\":null,\"reason\":null,\"snapshot_count\":4,\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"},\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}],\"status\":\"complete\"}";

const CLAMPED_JSON: &str = "\
{\"examined\":4,\"limit\":128,\"next_after\":null,\"reason\":null,\"snapshot_count\":4,\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"},\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}],\"status\":\"complete\"}";

const FIRST_PAGE_JSON: &str = "\
{\"examined\":4,\"limit\":1,\"next_after\":\"alpha\",\"reason\":\"reference_limit\",\"snapshot_count\":1,\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"}],\"status\":\"partial\"}";

const SECOND_PAGE_JSON: &str = "\
{\"examined\":4,\"limit\":1,\"next_after\":\"beta\",\"reason\":\"reference_limit\",\"snapshot_count\":1,\"snapshots\":[\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}],\"status\":\"partial\"}";

const LAST_PAGE_JSON: &str = "\
{\"examined\":4,\"limit\":2,\"next_after\":null,\"reason\":null,\"snapshot_count\":2,\"snapshots\":[\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}],\"status\":\"complete\"}";

const EMPTY_TAIL_JSON: &str = "\
{\"examined\":4,\"limit\":1,\"next_after\":null,\"reason\":null,\"snapshot_count\":0,\"snapshots\":[],\"status\":\"complete\"}";

const DEFAULT_MARKDOWN: &str = "\
**examined:** 4
**limit:** 100
**snapshot_count:** 4
**status:** complete

## snapshots
- **alpha**
  **source_revision:** 9a3b18ef93f54c758f7915a168eed23bf555218c
  **source_tree:** a086253f56c28f8ef6f00acf50eed179d41075f7
- **beta**
  **source_revision:** dbc21220c25f50fce6ac93b6e7859062cd3d3ca8
  **source_tree:** 64f955d5ec78273e82903eb78610cf7d682d5fb0
- **main**
  **source_revision:** dbc21220c25f50fce6ac93b6e7859062cd3d3ca8
  **source_tree:** 64f955d5ec78273e82903eb78610cf7d682d5fb0
- **zeta**
  **source_revision:** dbc21220c25f50fce6ac93b6e7859062cd3d3ca8
  **source_tree:** 64f955d5ec78273e82903eb78610cf7d682d5fb0
";

fn git(root: &Path, args: &[&str], date: Option<&str>) {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.hooksPath=.git/no-hooks",
            "-c",
            "gc.auto=0",
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args);
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} failed to start: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_branch_fixture(root: &Path) {
    git(root, &["init", "-q", "-b", "main"], None);
    std::fs::write(root.join("fixture.txt"), "base\n").expect("write base");
    git(root, &["add", "fixture.txt"], None);
    git(
        root,
        &["commit", "-q", "-m", "base"],
        Some("2020-01-01T00:00:00Z"),
    );
    git(root, &["branch", "alpha"], None);
    std::fs::write(root.join("fixture.txt"), "next\n").expect("write next");
    git(root, &["add", "fixture.txt"], None);
    git(
        root,
        &["commit", "-q", "-m", "next"],
        Some("2020-01-02T00:00:00Z"),
    );
    git(root, &["branch", "beta"], None);
    git(root, &["branch", "zeta"], None);
    git(
        root,
        &["update-ref", "refs/remotes/origin/main", HEAD_COMMIT],
        None,
    );
    git(root, &["tag", "v1", HEAD_COMMIT], None);
}

async fn call_branch_list(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project_root, "tracedecay_branch_list", arguments)
        .await
        .expect("branch list tools/call");
    serde_json::to_value(response).expect("branch list response JSON")
}

fn payload_text(response: &Value) -> &str {
    assert!(
        response["error"].is_null(),
        "branch list must not return a JSON-RPC error: {response}"
    );
    response["result"]["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                (text.starts_with('{') || text.contains("source_revision")).then_some(text)
            })
        })
        .unwrap_or_else(|| panic!("branch list returned no payload text: {response}"))
}

fn assert_ok_payload(response: &Value, expected: &str) {
    assert_eq!(payload_text(response), expected);
    assert!(
        response["result"]["isError"].is_null(),
        "a complete or partial page is not a semantic error: {response}"
    );
}

fn assert_unavailable(response: &Value, expected: &str) {
    assert_eq!(payload_text(response), expected);
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "an unavailable snapshot must be a semantic error: {response}"
    );
}

#[tokio::test]
async fn branch_list_reports_exact_local_refs_and_typed_rejections() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    write_branch_fixture(&project_root);
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        dir.path(),
        vec![project_root.clone()],
    ))
    .await
    .expect("production composition for branch list");
    let call = |arguments: Value| call_branch_list(&harness, &project_root, arguments);

    let markdown = call(json!({})).await;
    assert_ok_payload(&markdown, DEFAULT_MARKDOWN);

    let json_page = call(json!({"format": "json"})).await;
    assert_ok_payload(&json_page, COMPLETE_JSON);

    let empty_after = call(json!({"format": "json", "after": ""})).await;
    assert_ok_payload(&empty_after, COMPLETE_JSON);

    let clamped = call(json!({"format": "json", "limit": 200})).await;
    assert_ok_payload(&clamped, CLAMPED_JSON);

    let first = call(json!({"format": "json", "limit": 1})).await;
    assert_ok_payload(&first, FIRST_PAGE_JSON);

    let second = call(json!({"format": "json", "limit": 1, "after": "alpha"})).await;
    assert_ok_payload(&second, SECOND_PAGE_JSON);

    let last = call(json!({"format": "json", "limit": 2, "after": "beta"})).await;
    assert_ok_payload(&last, LAST_PAGE_JSON);

    let tail = call(json!({"format": "json", "limit": 1, "after": "zeta"})).await;
    assert_ok_payload(&tail, EMPTY_TAIL_JSON);
    assert_ne!(
        payload_text(&first),
        payload_text(&tail),
        "the page after the last local ref is empty only because the first page was not"
    );

    let invalid = call(json!({"format": "json", "after": "bad..name"})).await;
    assert_unavailable(
        &invalid,
        "{\"reason\":\"branch_ref_invalid\",\"retryable\":false,\"status\":\"unavailable\"}",
    );

    let zero = call(json!({"limit": 0})).await;
    assert!(zero["result"].is_null(), "{zero}");
    assert_eq!(zero["error"]["code"], json!(-32603));
    assert_eq!(
        zero["error"]["message"],
        json!("tool execution failed: config error: branch-list limit must be positive")
    );
    assert_eq!(
        zero["error"]["data"]["tool"],
        json!("tracedecay_branch_list")
    );

    std::fs::rename(project_root.join(".git"), project_root.join(".git-hidden"))
        .expect("hide git dir");
    let missing = call(json!({"format": "json"})).await;
    assert_unavailable(
        &missing,
        "{\"reason\":\"repository_unavailable\",\"retryable\":true,\"status\":\"unavailable\"}",
    );
    assert_ne!(payload_text(&json_page), payload_text(&missing));
    harness.shutdown().await;
}
