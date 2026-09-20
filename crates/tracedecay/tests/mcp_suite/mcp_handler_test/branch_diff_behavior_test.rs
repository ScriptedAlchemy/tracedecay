#![cfg(feature = "test-transport")]

//! `tracedecay_branch_diff` as a caller observes it: a production MCP
//! `tools/call` against two local commits, not a direct call into the diff
//! helper.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

use crate::common::fixture::{git_capture, git_run};
use crate::support::{
    handle_real_server_tool_call, handle_real_server_tool_call_raw, test_temp_dir,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ObservedSymbol {
    change: String,
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
}

fn symbol(
    change: &str,
    name: &str,
    qualified_name: &str,
    kind: &str,
    file: &str,
) -> ObservedSymbol {
    ObservedSymbol {
        change: change.to_owned(),
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind: kind.to_owned(),
        file: file.to_owned(),
    }
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture dir");
    fs::write(&path, contents).expect("fixture file");
}

fn commit(root: &Path, message: &str) {
    git_run(root, &["add", "-A"]);
    git_run(root, &["commit", "-qm", message]);
}

/// `master` keeps `kept_marker` and `StableWidget`. `feature` changes
/// `body_marker`'s signature, deletes `removed_marker`, and adds
/// `added_marker` plus `AddedWidget`.
fn branched_master_repo(root: &Path) {
    git_run(root, &["init", "-b", "master"]);
    write_file(
        root,
        "src/kept.rs",
        "pub fn kept_marker() -> i32 {\n    1\n}\n",
    );
    write_file(
        root,
        "src/changed.rs",
        "pub struct StableWidget;\n\npub fn body_marker() -> i32 {\n    1\n}\n",
    );
    write_file(
        root,
        "src/removed.rs",
        "pub fn removed_marker() -> i32 {\n    1\n}\n",
    );
    commit(root, "master symbols");

    git_run(root, &["checkout", "-b", "feature"]);
    fs::remove_file(root.join("src/removed.rs")).expect("delete removed source");
    write_file(
        root,
        "src/changed.rs",
        "pub struct StableWidget;\n\npub fn body_marker(scale: i32) -> i32 {\n    scale\n}\n",
    );
    write_file(
        root,
        "src/added.rs",
        "pub struct AddedWidget;\n\npub fn added_marker() -> i32 {\n    3\n}\n",
    );
    commit(root, "feature symbols");
    git_run(root, &["checkout", "master"]);
}

fn payload_text(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("branch diff text block: {result}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("branch diff JSON ({error}): {text}"))
}

async fn diff_ok(server: &McpServer, args: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_branch_diff", args).await;
    assert_eq!(
        result.get("isError"),
        None,
        "a resolved branch diff is not a tool error: {result}"
    );
    payload_text(&result)
}

async fn diff_unavailable(server: &McpServer, args: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_branch_diff", args).await;
    assert_eq!(result["isError"], json!(true), "{result}");
    payload_text(&result)
}

fn field_str<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("missing {field}: {value}"))
}

fn symbol_of(change: &str, value: &Value) -> ObservedSymbol {
    symbol(
        change,
        field_str(value, "name"),
        field_str(value, "qualified_name"),
        field_str(value, "kind"),
        field_str(value, "file"),
    )
}

fn observed_symbols(payload: &Value) -> Vec<ObservedSymbol> {
    let changes = payload["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("changes array: {payload}"));
    let mut observed = Vec::with_capacity(changes.len());
    for change in changes {
        let tag = field_str(change, "change");
        let view = match tag {
            "added" => symbol_of("added", &change["symbol"]),
            "removed" => symbol_of("removed", &change["symbol"]),
            "changed" => {
                let base = &change["base"];
                let head = &change["head"];
                assert_eq!(base["name"], head["name"], "{change}");
                assert_eq!(base["qualified_name"], head["qualified_name"], "{change}");
                assert_eq!(base["kind"], head["kind"], "{change}");
                assert_eq!(base["file"], head["file"], "{change}");
                assert_ne!(
                    base["content_digest"], head["content_digest"],
                    "a changed symbol must not report identical content: {change}"
                );
                symbol_of("changed", head)
            }
            other => panic!("unexpected change tag {other}: {payload}"),
        };
        observed.push(view);
    }
    observed.sort();
    observed
}

fn assert_symbols(payload: &Value, expected: &[ObservedSymbol]) {
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(observed_symbols(payload), expected, "{payload}");
}

fn assert_complete(
    payload: &Value,
    base: &str,
    head: &str,
    added: u64,
    removed: u64,
    changed: u64,
) {
    assert_eq!(payload["status"], "complete", "{payload}");
    assert_eq!(payload["base"], base, "{payload}");
    assert_eq!(payload["head"], head, "{payload}");
    assert_eq!(
        payload["summary"],
        json!({"added": added, "removed": removed, "changed": changed}),
        "{payload}"
    );
    assert_eq!(
        payload["total_changes"],
        json!(added + removed + changed),
        "{payload}"
    );
    assert!(
        payload.get("next_cursor").is_none(),
        "a complete page has no continuation: {payload}"
    );
}

fn assert_revision(root: &Path, payload: &Value, base: &str, head: &str) {
    assert_eq!(
        payload["base_revision"],
        git_capture(root, &["rev-parse", base]),
        "{payload}"
    );
    assert_eq!(
        payload["head_revision"],
        git_capture(root, &["rev-parse", head]),
        "{payload}"
    );
    assert_eq!(
        payload["base_tree"],
        git_capture(root, &["rev-parse", &format!("{base}^{{tree}}")]),
        "{payload}"
    );
    assert_eq!(
        payload["head_tree"],
        git_capture(root, &["rev-parse", &format!("{head}^{{tree}}")]),
        "{payload}"
    );
}

fn master_to_feature() -> Vec<ObservedSymbol> {
    vec![
        symbol(
            "added",
            "AddedWidget",
            "src/added.rs::AddedWidget",
            "struct",
            "src/added.rs",
        ),
        symbol(
            "added",
            "added_marker",
            "src/added.rs::added_marker",
            "function",
            "src/added.rs",
        ),
        symbol(
            "changed",
            "body_marker",
            "src/changed.rs::body_marker",
            "function",
            "src/changed.rs",
        ),
        symbol(
            "removed",
            "removed_marker",
            "src/removed.rs::removed_marker",
            "function",
            "src/removed.rs",
        ),
    ]
}

fn feature_to_master() -> Vec<ObservedSymbol> {
    vec![
        symbol(
            "removed",
            "AddedWidget",
            "src/added.rs::AddedWidget",
            "struct",
            "src/added.rs",
        ),
        symbol(
            "removed",
            "added_marker",
            "src/added.rs::added_marker",
            "function",
            "src/added.rs",
        ),
        symbol(
            "changed",
            "body_marker",
            "src/changed.rs::body_marker",
            "function",
            "src/changed.rs",
        ),
        symbol(
            "added",
            "removed_marker",
            "src/removed.rs::removed_marker",
            "function",
            "src/removed.rs",
        ),
    ]
}

#[tokio::test]
async fn branch_diff_reports_the_symbols_that_differ_between_master_and_feature() {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(&project_root).expect("project root");
    branched_master_repo(&project_root);
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![project_root.clone()],
    ))
    .await
    .expect("production composition harness");
    let server = harness
        .server(&project_root)
        .expect("mounted project server");

    let missing_base =
        handle_real_server_tool_call_raw(&server, "tracedecay_branch_diff", json!({})).await;
    assert_eq!(missing_base["error"]["code"], -32602, "{missing_base}");
    assert_eq!(
        missing_base["error"]["message"], "missing required parameter: base",
        "{missing_base}"
    );
    assert_eq!(
        missing_base["error"]["data"]["tool"], "tracedecay_branch_diff",
        "{missing_base}"
    );
    assert_eq!(
        missing_base["error"]["data"]["reason_code"], "missing_required_parameter",
        "{missing_base}"
    );
    assert_eq!(
        missing_base["error"]["data"]["retryable"], false,
        "{missing_base}"
    );

    let zero_limit = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_branch_diff",
        json!({"base": "master", "head": "feature", "limit": 0}),
    )
    .await;
    assert_eq!(zero_limit["error"]["code"], -32603, "{zero_limit}");
    assert_eq!(
        zero_limit["error"]["message"],
        "tool execution failed: config error: branch-diff limit must be positive",
        "{zero_limit}"
    );

    let missing_ref = diff_unavailable(&server, json!({"base": "ghost", "head": "feature"})).await;
    assert_eq!(missing_ref["status"], "unavailable", "{missing_ref}");
    assert_eq!(
        missing_ref["reason"], "branch_ref_not_found",
        "{missing_ref}"
    );
    assert_eq!(missing_ref["retryable"], false, "{missing_ref}");
    assert_eq!(
        missing_ref["base_or_head"], "ghost..feature",
        "{missing_ref}"
    );

    let diff = diff_ok(&server, json!({"base": "master", "head": "feature"})).await;
    assert_complete(&diff, "master", "feature", 2, 1, 1);
    assert_revision(&project_root, &diff, "master", "feature");
    assert_symbols(&diff, &master_to_feature());

    let same_ref = diff_ok(&server, json!({"base": "master", "head": "master"})).await;
    assert_complete(&same_ref, "master", "master", 0, 0, 0);
    assert_eq!(same_ref["changes"], json!([]), "{same_ref}");
    assert_revision(&project_root, &same_ref, "master", "master");

    let functions = diff_ok(
        &server,
        json!({"base": "master", "head": "feature", "kind": "function"}),
    )
    .await;
    assert_complete(&functions, "master", "feature", 1, 1, 1);
    assert_symbols(
        &functions,
        &[
            symbol(
                "added",
                "added_marker",
                "src/added.rs::added_marker",
                "function",
                "src/added.rs",
            ),
            symbol(
                "changed",
                "body_marker",
                "src/changed.rs::body_marker",
                "function",
                "src/changed.rs",
            ),
            symbol(
                "removed",
                "removed_marker",
                "src/removed.rs::removed_marker",
                "function",
                "src/removed.rs",
            ),
        ],
    );

    let structs = diff_ok(
        &server,
        json!({"base": "master", "head": "feature", "kind": "struct"}),
    )
    .await;
    assert_complete(&structs, "master", "feature", 1, 0, 0);
    assert_symbols(
        &structs,
        &[symbol(
            "added",
            "AddedWidget",
            "src/added.rs::AddedWidget",
            "struct",
            "src/added.rs",
        )],
    );

    let added_file = diff_ok(
        &server,
        json!({"base": "master", "head": "feature", "file": "src/added.rs"}),
    )
    .await;
    assert_complete(&added_file, "master", "feature", 2, 0, 0);
    assert_symbols(
        &added_file,
        &[
            symbol(
                "added",
                "AddedWidget",
                "src/added.rs::AddedWidget",
                "struct",
                "src/added.rs",
            ),
            symbol(
                "added",
                "added_marker",
                "src/added.rs::added_marker",
                "function",
                "src/added.rs",
            ),
        ],
    );

    let unchanged_file = diff_ok(
        &server,
        json!({"base": "master", "head": "feature", "file": "src/kept.rs"}),
    )
    .await;
    assert_complete(&unchanged_file, "master", "feature", 0, 0, 0);
    assert_eq!(unchanged_file["changes"], json!([]), "{unchanged_file}");

    let active_head = diff_ok(&server, json!({"base": "feature"})).await;
    assert_complete(&active_head, "feature", "master", 1, 2, 1);
    assert_revision(&project_root, &active_head, "feature", "master");
    assert_symbols(&active_head, &feature_to_master());

    let forged_cursor = diff_unavailable(
        &server,
        json!({
            "base": "master",
            "head": "feature",
            "cursor": "not-a-branch-diff-cursor",
        }),
    )
    .await;
    assert_eq!(forged_cursor["status"], "unavailable", "{forged_cursor}");
    assert_eq!(
        forged_cursor["reason"], "invalid_request",
        "{forged_cursor}"
    );
    assert_eq!(forged_cursor["retryable"], false, "{forged_cursor}");
    assert_eq!(forged_cursor["base"], "master", "{forged_cursor}");
    assert_eq!(forged_cursor["head"], "feature", "{forged_cursor}");

    let mut cursor = None;
    let mut paged = Vec::new();
    for page_index in 0..8 {
        let mut args = json!({"base": "master", "head": "feature", "limit": 1});
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let page = diff_ok(&server, args).await;
        assert_eq!(page["base"], "master", "{page}");
        assert_eq!(page["head"], "feature", "{page}");
        assert_eq!(page["total_changes"], 4, "{page}");
        let page_changes = page["changes"]
            .as_array()
            .unwrap_or_else(|| panic!("page changes: {page}"));
        assert_eq!(page_changes.len(), 1, "page {page_index}: {page}");
        paged.extend(observed_symbols(&page));
        match page["status"].as_str() {
            Some("partial") => {
                assert_eq!(page["reason"], "result_limit", "{page}");
                let next = page["next_cursor"]
                    .as_str()
                    .unwrap_or_else(|| panic!("partial page cursor: {page}"));
                assert!(!next.is_empty(), "partial page cursor is empty: {page}");
                cursor = Some(next.to_owned());
            }
            Some("complete") => {
                assert_eq!(page_index, 3, "four symbols page at limit 1: {page}");
                cursor = None;
                break;
            }
            other => panic!("unexpected page status {other:?}: {page}"),
        }
    }
    assert_eq!(cursor, None, "branch diff pages did not finish");
    paged.sort();
    let mut expected = master_to_feature();
    expected.sort();
    assert_eq!(paged, expected);
}
