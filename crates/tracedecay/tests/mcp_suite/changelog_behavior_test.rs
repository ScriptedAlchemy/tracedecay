use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

use super::support::{TestTempDir, test_temp_dir};
use crate::common::fixture::GitFixture;

struct ChangelogRepo {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

async fn open_repo(prepare: impl FnOnce(&GitFixture)) -> ChangelogRepo {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    let fixture = GitFixture::primary(&project_root);
    prepare(&fixture);
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![project_root.clone()],
    ))
    .await
    .expect("production composition for changelog");
    ChangelogRepo {
        harness,
        project_root,
        _isolation: isolation,
    }
}

async fn call_changelog(repo: &ChangelogRepo, arguments: Value) -> JsonRpcResponse {
    repo.harness
        .call_tool(&repo.project_root, "tracedecay_changelog", arguments)
        .await
        .expect("changelog tools/call")
}

fn success_result(response: &JsonRpcResponse) -> &Value {
    assert!(
        response.error.is_none(),
        "changelog must answer inside a tool result, not a JSON-RPC error: {:?}",
        response.error
    );
    response
        .result
        .as_ref()
        .expect("changelog tools/call result")
}

fn json_text(result: &Value) -> &str {
    result["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                text.trim_start().starts_with('{').then_some(text)
            })
        })
        .unwrap_or_else(|| panic!("changelog JSON content missing from {result}"))
}

fn payload(result: &Value) -> Value {
    serde_json::from_str(json_text(result)).unwrap_or_else(|error| {
        panic!(
            "changelog JSON should parse: {error}\n{}",
            json_text(result)
        )
    })
}

fn observable_symbols(body: &Value, key: &str) -> Vec<Value> {
    let mut rows = body[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be an array in {body}"))
        .iter()
        .map(|symbol| {
            json!({
                "kind": symbol["kind"],
                "qualified_name": symbol["qualified_name"],
                "name": symbol["name"],
                "file": symbol["file"],
                "content_digest": symbol["content_digest"],
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left["qualified_name"]
            .as_str()
            .cmp(&right["qualified_name"].as_str())
    });
    rows
}

#[tokio::test]
async fn changelog_rejects_missing_and_non_object_arguments() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        fixture.commit_all("initial");
    })
    .await;

    let missing_from = call_changelog(&repo, json!({"to_ref": "HEAD", "format": "json"})).await;
    let missing_from = missing_from
        .error
        .expect("missing from_ref is a JSON-RPC error");
    assert_eq!(missing_from.code, -32602);
    assert_eq!(missing_from.message, "missing required parameter: from_ref");
    assert_eq!(
        missing_from.data,
        Some(json!({
            "tool": "tracedecay_changelog",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: from_ref"
        }))
    );

    let missing_to = call_changelog(&repo, json!({"from_ref": "HEAD", "format": "json"})).await;
    let missing_to = missing_to
        .error
        .expect("missing to_ref is a JSON-RPC error");
    assert_eq!(missing_to.code, -32602);
    assert_eq!(missing_to.message, "missing required parameter: to_ref");
    assert_eq!(
        missing_to.data,
        Some(json!({
            "tool": "tracedecay_changelog",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: to_ref"
        }))
    );

    let not_object = call_changelog(&repo, json!(["HEAD", "HEAD"])).await;
    let not_object = not_object
        .error
        .expect("a non-object argument list is a JSON-RPC error");
    assert_eq!(not_object.code, -32603);
    assert_eq!(
        not_object.message,
        "tool execution failed: config error: invalid arguments: tracedecay_changelog expects a JSON object"
    );
    assert_eq!(
        not_object.data,
        Some(json!({
            "tool": "tracedecay_changelog",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool changelog ...` (`tracedecay tool changelog --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
        }))
    );
}

#[tokio::test]
async fn changelog_unknown_ref_is_a_typed_git_error() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        fixture.commit_all("initial");
    })
    .await;

    let response = call_changelog(
        &repo,
        json!({
            "from_ref": "no-such-changelog-ref",
            "to_ref": "HEAD",
            "format": "json"
        }),
    )
    .await;
    let result = success_result(&response);
    assert_eq!(result["isError"], true);
    let body = payload(result);
    assert_eq!(body["error"]["kind"], "git");
    assert_eq!(body["error"]["operation"], "diff");
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("git error message missing: {body}"));
    assert!(
        message.starts_with("cannot resolve 'no-such-changelog-ref':"),
        "unresolvable ref must name itself in the git error: {message}"
    );
}

#[tokio::test]
async fn changelog_between_commits_lists_the_file_and_withholds_branch_symbols() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn original() {}\n").expect("source");
        fixture.commit_all("initial");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn original() {}\npub fn added() {}\n",
        )
        .expect("source");
        fixture.commit_all("add function");
    })
    .await;

    let response = call_changelog(
        &repo,
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD", "format": "json"}),
    )
    .await;
    let result = success_result(&response);
    assert!(
        result.get("isError").is_none(),
        "a revision-expression diff is a partial answer, not a tool error: {result}"
    );
    assert_eq!(
        json_text(result),
        r#"{"changed_file_count":1,"changed_files":["src/lib.rs"],"from_ref":"HEAD~1","status":"partial","symbol_changes_coverage":{"reason":"exact_local_branch_required","retryable":false,"status":"unavailable"},"symbols_added":[],"symbols_modified":[],"symbols_removed":[],"to_ref":"HEAD"}"#
    );

    let markdown = call_changelog(&repo, json!({"from_ref": "HEAD~1", "to_ref": "HEAD"})).await;
    let markdown = success_result(&markdown);
    let rendered = markdown["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("default changelog text missing: {markdown}"));
    assert_eq!(
        rendered,
        "\
**changed_file_count:** 1
**from_ref:** HEAD~1
**status:** partial
**to_ref:** HEAD

## changed_files
- src/lib.rs

## symbol_changes_coverage
**reason:** exact_local_branch_required
**retryable:** false
**status:** unavailable
symbols_added: none
symbols_modified: none
symbols_removed: none
"
    );
}

#[tokio::test]
async fn changelog_deleted_subtree_lists_only_the_removed_file() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("crates/sub")).expect("subtree");
        std::fs::write(root.join("crates/sub/keep.rs"), "pub fn k() {}\n").expect("source");
        std::fs::write(root.join("main.rs"), "fn main() {}\n").expect("source");
        fixture.commit_all("initial");
        std::fs::remove_dir_all(root.join("crates")).expect("drop subtree");
        fixture.commit_all("drop crates");
    })
    .await;

    let response = call_changelog(
        &repo,
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD", "format": "json"}),
    )
    .await;
    let result = success_result(&response);
    assert_eq!(
        json_text(result),
        r#"{"changed_file_count":1,"changed_files":["crates/sub/keep.rs"],"from_ref":"HEAD~1","status":"partial","symbol_changes_coverage":{"reason":"exact_local_branch_required","retryable":false,"status":"unavailable"},"symbols_added":[],"symbols_modified":[],"symbols_removed":[],"to_ref":"HEAD"}"#
    );
}

#[tokio::test]
async fn changelog_same_branch_tip_reports_no_changes() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        fixture.commit_all("initial");
    })
    .await;

    let response = call_changelog(
        &repo,
        json!({"from_ref": "main", "to_ref": "main", "format": "json"}),
    )
    .await;
    let result = success_result(&response);
    assert!(
        result.get("isError").is_none(),
        "identical tips are an empty changelog, not an error: {result}"
    );
    let body = payload(result);
    assert_eq!(body["status"], "complete");
    assert_eq!(body["from_ref"], "main");
    assert_eq!(body["to_ref"], "main");
    assert_eq!(body["changed_file_count"], 0);
    assert_eq!(body["changed_files"], json!([]));
    assert_eq!(body["symbols_added"], json!([]));
    assert_eq!(body["symbols_removed"], json!([]));
    assert_eq!(body["symbols_modified"], json!([]));
    assert_eq!(
        body["symbol_changes_coverage"],
        json!({"status": "complete"})
    );
    let base = body["base_generation"]
        .as_str()
        .unwrap_or_else(|| panic!("identical tips must name the base generation: {body}"));
    let head = body["head_generation"]
        .as_str()
        .unwrap_or_else(|| panic!("identical tips must name the head generation: {body}"));
    assert_eq!(base, head, "the same tip is one generation: {body}");
}

#[tokio::test]
async fn changelog_between_local_branches_names_added_removed_and_modified_symbols() {
    let repo = open_repo(|fixture| {
        let root = fixture.root();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn kept() {}\npub fn removed_fn() {}\npub fn changed_fn() { let _ = 1; }\n",
        )
        .expect("base source");
        fixture.commit_all("initial");
        fixture.run(&["switch", "-c", "feature"]);
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn kept() {}\npub fn changed_fn() { let _ = 2; }\npub fn added_fn() {}\n",
        )
        .expect("feature source");
        fixture.commit_all("revise symbols");
        fixture.run(&["switch", "main"]);
    })
    .await;

    let response = call_changelog(
        &repo,
        json!({"from_ref": "main", "to_ref": "feature", "format": "json"}),
    )
    .await;
    let result = success_result(&response);
    assert!(
        result.get("isError").is_none(),
        "a local-branch changelog is an answer, not a tool error: {result}"
    );
    let body = payload(result);
    assert_eq!(body["status"], "complete", "{body}");
    assert_eq!(body["from_ref"], "main");
    assert_eq!(body["to_ref"], "feature");
    assert_eq!(body["changed_files"], json!(["src/lib.rs"]));
    assert_eq!(body["changed_file_count"], 1);
    assert_eq!(
        body["symbol_changes_coverage"],
        json!({"status": "complete"})
    );
    assert_ne!(
        body["base_generation"].as_str().expect("base generation"),
        body["head_generation"].as_str().expect("head generation"),
        "different tips must not share a generation: {body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_added"),
        vec![json!({
            "kind": "function",
            "qualified_name": "src/lib.rs::added_fn",
            "name": "added_fn",
            "file": "src/lib.rs",
            "content_digest": "sha256:19b08a1214d48a2af703ce3ef9538939a8e5d08a55bd9a568e30263d2d8ae9a6",
        })],
        "{body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_removed"),
        vec![json!({
            "kind": "function",
            "qualified_name": "src/lib.rs::removed_fn",
            "name": "removed_fn",
            "file": "src/lib.rs",
            "content_digest": "sha256:d2609bdc15fd11af59b21c574e6c7a560b6ff1963e73c606b7df32e021a5a135",
        })],
        "{body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_modified"),
        vec![json!({
            "kind": "function",
            "qualified_name": "src/lib.rs::changed_fn",
            "name": "changed_fn",
            "file": "src/lib.rs",
            "content_digest": "sha256:faa06ddf52ace2f77e20545f60cbf1af7ca9325f216d43cee9206803af884ff3",
        })],
        "{body}"
    );
}
