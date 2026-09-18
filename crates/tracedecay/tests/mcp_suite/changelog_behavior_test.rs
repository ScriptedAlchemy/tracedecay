//! Production `tools/call` behavior of `tracedecay_changelog`.
//!
//! Each call goes through [`ProductionProjectCompositionHarnessV1::call_tool`],
//! the JSON-RPC entry the daemon serves. Assertions name the text or fields a
//! caller observes for one concrete repository.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

use super::support::{TestTempDir, test_temp_dir};

struct ChangelogRepo {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new(crate::common::git_program())
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} should spawn: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=test@tracedecay.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

async fn open_repo(prepare: impl FnOnce(&Path)) -> ChangelogRepo {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    std::fs::create_dir_all(&project_root).expect("changelog fixture directory");
    prepare(&project_root);
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

fn init_main(root: &Path) {
    git(root, &["init", "-b", "main"]);
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

/// `(kind, qualified_name, name, file, content_digest)` in the order a caller
/// can sort without reading occurrence ids. The digest is the body the index
/// sealed for that symbol, so a modification is a different digest, not a rename.
fn observable_symbols(body: &Value, key: &str) -> Vec<(String, String, String, String, String)> {
    let mut rows = body[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be an array in {body}"))
        .iter()
        .map(|symbol| {
            (
                symbol["kind"].as_str().unwrap_or("").to_owned(),
                symbol["qualified_name"].as_str().unwrap_or("").to_owned(),
                symbol["name"].as_str().unwrap_or("").to_owned(),
                symbol["file"].as_str().unwrap_or("").to_owned(),
                symbol["content_digest"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

#[tokio::test]
async fn changelog_rejects_missing_and_non_object_arguments() {
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        commit(root, "initial");
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
    let missing_to = missing_to.error.expect("missing to_ref is a JSON-RPC error");
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
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        commit(root, "initial");
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
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn original() {}\n").expect("source");
        commit(root, "initial");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn original() {}\npub fn added() {}\n",
        )
        .expect("source");
        commit(root, "add function");
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

    let markdown = call_changelog(
        &repo,
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
    )
    .await;
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
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("crates/sub")).expect("subtree");
        std::fs::write(root.join("crates/sub/keep.rs"), "pub fn k() {}\n").expect("source");
        std::fs::write(root.join("main.rs"), "fn main() {}\n").expect("source");
        commit(root, "initial");
        std::fs::remove_dir_all(root.join("crates")).expect("drop subtree");
        commit(root, "drop crates");
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
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        commit(root, "initial");
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
    let repo = open_repo(|root| {
        init_main(root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn kept() {}\npub fn removed_fn() {}\npub fn changed_fn() { let _ = 1; }\n",
        )
        .expect("base source");
        commit(root, "initial");
        git(root, &["switch", "-c", "feature"]);
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn kept() {}\npub fn changed_fn() { let _ = 2; }\npub fn added_fn() {}\n",
        )
        .expect("feature source");
        commit(root, "revise symbols");
        git(root, &["switch", "main"]);
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
        body["base_generation"].as_str(),
        body["head_generation"].as_str(),
        "different tips must not share a generation: {body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_added"),
        vec![(
            "function".to_owned(),
            "src/lib.rs::added_fn".to_owned(),
            "added_fn".to_owned(),
            "src/lib.rs".to_owned(),
            "sha256:19b08a1214d48a2af703ce3ef9538939a8e5d08a55bd9a568e30263d2d8ae9a6".to_owned(),
        )],
        "{body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_removed"),
        vec![(
            "function".to_owned(),
            "src/lib.rs::removed_fn".to_owned(),
            "removed_fn".to_owned(),
            "src/lib.rs".to_owned(),
            "sha256:d2609bdc15fd11af59b21c574e6c7a560b6ff1963e73c606b7df32e021a5a135".to_owned(),
        )],
        "{body}"
    );
    assert_eq!(
        observable_symbols(&body, "symbols_modified"),
        vec![(
            "function".to_owned(),
            "src/lib.rs::changed_fn".to_owned(),
            "changed_fn".to_owned(),
            "src/lib.rs".to_owned(),
            "sha256:faa06ddf52ace2f77e20545f60cbf1af7ca9325f216d43cee9206803af884ff3".to_owned(),
        )],
        "{body}"
    );
}
