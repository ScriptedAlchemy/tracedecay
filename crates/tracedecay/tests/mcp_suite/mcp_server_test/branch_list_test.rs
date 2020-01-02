//! `tracedecay_branch_list` answers with exact local `refs/heads` snapshots.
//!
//! A branch name is a git ref, not a branch-database selector. Remote-tracking
//! refs and tags are not local branches, so they must not appear in the page.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use super::support::{jsonrpc_request, response_with_id};
use crate::mcp_server_test::run_client_connection_with_messages;
use crate::support::{init_test_project, real_mcp_server, test_temp_dir};

const HEAD_COMMIT: &str = "dbc21220c25f50fce6ac93b6e7859062cd3d3ca8";

const COMPLETE_JSON: &str = "\
{\"status\":\"complete\",\"reason\":null,\"snapshot_count\":4,\"examined\":4,\
\"limit\":100,\"next_after\":null,\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"},\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}]}";

const CLAMPED_JSON: &str = "\
{\"status\":\"complete\",\"reason\":null,\"snapshot_count\":4,\"examined\":4,\
\"limit\":128,\"next_after\":null,\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"},\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}]}";

const FIRST_PAGE_JSON: &str = "\
{\"status\":\"partial\",\"reason\":\"reference_limit\",\"snapshot_count\":1,\"examined\":4,\
\"limit\":1,\"next_after\":\"alpha\",\"snapshots\":[\
{\"branch\":\"alpha\",\"source_revision\":\"9a3b18ef93f54c758f7915a168eed23bf555218c\",\"source_tree\":\"a086253f56c28f8ef6f00acf50eed179d41075f7\"}]}";

const SECOND_PAGE_JSON: &str = "\
{\"status\":\"partial\",\"reason\":\"reference_limit\",\"snapshot_count\":1,\"examined\":4,\
\"limit\":1,\"next_after\":\"beta\",\"snapshots\":[\
{\"branch\":\"beta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}]}";

const LAST_PAGE_JSON: &str = "\
{\"status\":\"complete\",\"reason\":null,\"snapshot_count\":2,\"examined\":4,\
\"limit\":2,\"next_after\":null,\"snapshots\":[\
{\"branch\":\"main\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"},\
{\"branch\":\"zeta\",\"source_revision\":\"dbc21220c25f50fce6ac93b6e7859062cd3d3ca8\",\"source_tree\":\"64f955d5ec78273e82903eb78610cf7d682d5fb0\"}]}";

const EMPTY_TAIL_JSON: &str = "\
{\"status\":\"complete\",\"reason\":null,\"snapshot_count\":0,\"examined\":4,\
\"limit\":1,\"next_after\":null,\"snapshots\":[]}";

const DEFAULT_MARKDOWN: &str = "\
**status:** complete
**snapshot_count:** 4
**examined:** 4
**limit:** 100

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

async fn call_branch_list(server: &Arc<McpServer>, id: i64, arguments: Value) -> Value {
    let responses = run_client_connection_with_messages(
        Arc::clone(server),
        vec![jsonrpc_request(
            json!(id),
            "tools/call",
            json!({
                "name": "tracedecay_branch_list",
                "arguments": arguments,
            }),
        )],
    )
    .await;
    response_with_id(&responses, json!(id))
}

fn payload_text<'a>(response: &'a Value) -> &'a str {
    assert!(
        response["error"].is_null(),
        "branch list must not return a JSON-RPC error: {response}"
    );
    response["result"]["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                (text.starts_with('{') || text.starts_with("**status:**")).then_some(text)
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
    write_branch_fixture(dir.path());
    let (cg, _env) = init_test_project(dir.path()).await;
    let server = real_mcp_server(cg).await;

    let markdown = call_branch_list(&server, 1, json!({})).await;
    assert_ok_payload(&markdown, DEFAULT_MARKDOWN);

    let json_page = call_branch_list(&server, 2, json!({"format": "json"})).await;
    assert_ok_payload(&json_page, COMPLETE_JSON);

    let empty_after = call_branch_list(&server, 3, json!({"format": "json", "after": ""})).await;
    assert_ok_payload(&empty_after, COMPLETE_JSON);

    let clamped = call_branch_list(&server, 4, json!({"format": "json", "limit": 200})).await;
    assert_ok_payload(&clamped, CLAMPED_JSON);

    let first = call_branch_list(&server, 5, json!({"format": "json", "limit": 1})).await;
    assert_ok_payload(&first, FIRST_PAGE_JSON);

    let second = call_branch_list(
        &server,
        6,
        json!({"format": "json", "limit": 1, "after": "alpha"}),
    )
    .await;
    assert_ok_payload(&second, SECOND_PAGE_JSON);

    let last = call_branch_list(
        &server,
        7,
        json!({"format": "json", "limit": 2, "after": "beta"}),
    )
    .await;
    assert_ok_payload(&last, LAST_PAGE_JSON);

    let tail = call_branch_list(
        &server,
        8,
        json!({"format": "json", "limit": 1, "after": "zeta"}),
    )
    .await;
    assert_ok_payload(&tail, EMPTY_TAIL_JSON);
    assert_ne!(
        payload_text(&first),
        payload_text(&tail),
        "the page after the last local ref is empty only because the first page was not"
    );

    let invalid =
        call_branch_list(&server, 9, json!({"format": "json", "after": "bad..name"})).await;
    assert_unavailable(
        &invalid,
        "{\"status\":\"unavailable\",\"reason\":\"branch_ref_invalid\",\"retryable\":false}",
    );

    let zero = call_branch_list(&server, 10, json!({"limit": 0})).await;
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
    assert_eq!(
        zero["error"]["data"]["cli_fallback"],
        json!(
            "This tool is also available from the shell: `tracedecay tool branch_list ...` \
             (`tracedecay tool branch_list --help` for parameters). If MCP calls keep \
             failing or timing out, fall back to that CLI instead of querying \
             .tracedecay databases directly."
        )
    );

    std::fs::rename(dir.path().join(".git"), dir.path().join(".git-hidden")).expect("hide git dir");
    let missing = call_branch_list(&server, 11, json!({"format": "json"})).await;
    assert_unavailable(
        &missing,
        "{\"status\":\"unavailable\",\"reason\":\"repository_unavailable\",\"retryable\":true}",
    );
    assert_ne!(payload_text(&json_page), payload_text(&missing));
}
