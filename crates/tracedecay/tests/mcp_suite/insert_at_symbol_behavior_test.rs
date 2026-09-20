#![cfg(feature = "test-transport")]

//! Caller-visible `tracedecay_insert_at_symbol` behavior through the production
//! MCP `tools/call` path. Expected source, diffs, messages, and refusals are
//! literals the test owns; they are not read back from the tool.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, close_production_source_edit_fixture, extract_json,
    extract_text, init_production_source_edit_project, test_temp_dir, warm_code_index_search,
};

const AFTER_SOURCE: &str = "\
pub const anchor: u32 = 1;
fn total() {
    let quantity = 2;
}
";

const AVERAGE_FN: &str = "\
fn average_unit_price() -> u32 {
    7
}";

const AFTER_APPLIED: &str = "\
pub const anchor: u32 = 1;
fn total() {
    let quantity = 2;
}
fn average_unit_price() -> u32 {
    7
}
";

const AFTER_DIFF: &str = "\
@@ -2,3 +2,6 @@
 fn total() {
     let quantity = 2;
 }
+fn average_unit_price() -> u32 {
+    7
+}";

const BEFORE_SOURCE: &str = "\
pub const N: u32 = 0;
/// Doc for foo.
fn foo() {}
";

const BEFORE_APPLIED: &str = "\
pub const N: u32 = 0;
// INSERTED
/// Doc for foo.
fn foo() {}
";

const BEFORE_DIFF: &str = "\
@@ -1,3 +1,4 @@
 pub const N: u32 = 0;
+// INSERTED
 /// Doc for foo.
 fn foo() {}";

const SHARED_SOURCE: &str = "\
pub const anchor: u32 = 1;
fn total() {
    let quantity = 2;
}
fn shared() {}
";

const OTHER_SHARED_SOURCE: &str = "\
pub const N: u32 = 0;
fn shared() {}
";

const OTHER_SHARED_CONCURRENT: &str = "\
pub const N: u32 = 0;
fn shared() { let changed = 1; }
";

const SHARED_AFTER_APPLIED: &str = "\
pub const anchor: u32 = 1;
fn total() {
    let quantity = 2;
}
fn shared() {}
// QUALIFIED_AFTER
";

const SHARED_DIFF: &str = "\
@@ -3,3 +3,4 @@
     let quantity = 2;
 }
 fn shared() {}
+// QUALIFIED_AFTER";

const AFTER_DRY_MESSAGE: &str =
    "dry run. Nothing written; preview only (inserted after total (function) at line 5)";
const AFTER_APPLY_MESSAGE: &str = "inserted after total (function) at line 5";
const BEFORE_DRY_MESSAGE: &str =
    "dry run. Nothing written; preview only (inserted before foo (function) at line 2)";
const BEFORE_APPLY_MESSAGE: &str = "inserted before foo (function) at line 2";
const SHARED_DRY_MESSAGE: &str =
    "dry run. Nothing written; preview only (inserted after shared (function) at line 6)";
const SHARED_APPLY_MESSAGE: &str = "inserted after shared (function) at line 6";
const REPLAY_MESSAGE: &str = "source edit completed; detailed edit output was not retained";
const STALE_MESSAGE: &str = "source edit failed before the effect";
const INSERT_OPERATION: &str = "use-case.application.source-edit.insert-at-symbol";

const AMBIGUOUS_SHARED: &str =
    "symbol 'shared' is ambiguous (2 matches); pass a fully qualified name";
const MISSING_SYMBOL: &str = "symbol 'missing_fn' not found";
const MISSING_CONTENT: &str = "missing required parameter: content";
const BAD_POSITION: &str = "position must be \"before\" or \"after\", got \"beside\"";
const MISSING_PREVIEW: &str = "source edit apply requires a fresh idempotency_key and the expected_state returned by a preview";

fn write_pair(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/after.rs"), AFTER_SOURCE).unwrap();
    fs::write(project.join("src/before.rs"), BEFORE_SOURCE).unwrap();
}

async fn open_sources(files: &[(&str, &str)]) -> (ProductionSourceEditFixture, TestTempDir) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    for (relative, body) in files {
        fs::write(project.join(relative), body).unwrap();
    }
    let fixture = init_production_source_edit_project(&project).await;
    (fixture, dir)
}

async fn open_pair() -> (ProductionSourceEditFixture, TestTempDir) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_pair(&project);
    let fixture = init_production_source_edit_project(&project).await;
    (fixture, dir)
}

async fn settle(fixture: &ProductionSourceEditFixture) {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, "total").await;
}

fn response_text(response: &JsonRpcResponse) -> String {
    if let Some(result) = &response.result {
        return extract_text(result).to_owned();
    }
    response
        .error
        .as_ref()
        .map(|error| error.message.clone())
        .unwrap_or_default()
}

/// Symbol edits refuse a seated generation while the first rebuild is in
/// flight. The refusal is retryable; wait until the call is no longer that
/// typed stale state before asserting the edit itself.
async fn call_insert(fixture: &ProductionSourceEditFixture, arguments: Value) -> JsonRpcResponse {
    for _ in 0..80 {
        let response = fixture
            .harness
            .call_tool(
                &fixture.project_root,
                "tracedecay_insert_at_symbol",
                arguments.clone(),
            )
            .await
            .expect("tracedecay_insert_at_symbol production MCP call");
        if !response_text(&response).contains("code-graph-stale") {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("code graph stayed stale for tracedecay_insert_at_symbol");
}

fn success_body(response: &JsonRpcResponse) -> Value {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    assert!(
        response.error.is_none(),
        "tracedecay_insert_at_symbol failed: {:?}",
        response.error
    );
    let result = response
        .result
        .as_ref()
        .expect("tracedecay_insert_at_symbol result");
    assert!(
        result.get("isError").is_none(),
        "successful insert must not set isError: {result}"
    );
    extract_json(result)
}

fn failure_body(response: &JsonRpcResponse) -> Value {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    assert!(
        response.error.is_none(),
        "pre-effect refusal must stay a tool result, not a transport error: {:?}",
        response.error
    );
    let result = response
        .result
        .as_ref()
        .expect("tracedecay_insert_at_symbol result");
    assert_eq!(result["isError"], json!(true), "{result}");
    extract_json(result)
}

fn assert_pre_effect_refusal(response: &JsonRpcResponse, detail: &str) {
    let body = failure_body(response);
    assert_eq!(
        body["message"],
        format!("source edit failed before the effect: config error: {detail}")
    );
    assert_eq!(body["success"], false);
    assert_eq!(body["failed"], true);
    assert_eq!(body["replayed"], false);
    assert_eq!(body["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(body["effect"]["payload"]["message"], STALE_MESSAGE);
    assert_eq!(body["effect"]["payload"]["success"], false);
    assert_eq!(body["effect"]["payload"]["failed"], true);
    assert_eq!(body["effect"]["payload"]["files"], json!([]));
    assert_eq!(body["effect"]["payload"]["operation"], INSERT_OPERATION);
    let state = body["expected_state"]
        .as_str()
        .unwrap_or_else(|| panic!("expected_state missing from {body}"));
    assert!(
        state.starts_with("sha256:") && state.len() == "sha256:".len() + 64,
        "refusal expected_state: {state}"
    );
}

fn assert_rpc_error(response: &JsonRpcResponse, code: i32, message: impl AsRef<str>, data: Value) {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    assert!(response.result.is_none(), "{response:?}");
    let error = response
        .error
        .as_ref()
        .expect("tracedecay_insert_at_symbol error");
    assert_eq!(error.code, code);
    assert_eq!(error.message, message.as_ref());
    assert_eq!(error.data.as_ref(), Some(&data));
}

fn take_state(payload: &mut Value) -> String {
    let state = payload["expected_state"]
        .as_str()
        .unwrap_or_else(|| panic!("expected_state missing from {payload}"))
        .to_owned();
    assert!(
        state.starts_with("sha256:") && state.len() == "sha256:".len() + 64,
        "expected_state must be a sha256 digest, got {state}"
    );
    let object = payload.as_object_mut().expect("insert payload");
    object.remove("expected_state");
    object.remove("predicted_state");
    object.remove("effect");
    state
}

fn assert_stable(mut payload: Value, expected: Value) -> String {
    let state = take_state(&mut payload);
    assert_eq!(payload, expected, "insert payload: {payload}");
    state
}

fn durable_payload(line: u32, before: bool, file: &str) -> Value {
    json!({
        "operation": INSERT_OPERATION,
        "success": true,
        "files": [file],
        "change_count": null,
        "line": line,
        "before": before,
        "import_count": null,
        "finding_count": null,
        "failed": false,
        "cancelled": false,
        "timed_out": false,
        "effect_unknown": false,
        "reconciled": false,
        "durable_metadata_only": true,
        "message": REPLAY_MESSAGE,
    })
}

fn assert_completed_effect(
    payload: &Value,
    idempotency_key: &str,
    expected_state: &str,
    line: u32,
    before: bool,
    file: &str,
) {
    assert_eq!(payload["effect"]["effect_class"], "source_edit");
    assert_eq!(payload["effect"]["idempotency_key"], idempotency_key);
    assert_eq!(payload["effect"]["expected_state"], expected_state);
    assert_eq!(payload["effect"]["reconciliation"], "reconciled");
    assert_eq!(payload["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(payload["effect"]["receipt"]["effect_class"], "source_edit");
    assert_eq!(
        payload["effect"]["receipt"]["idempotency_key"],
        idempotency_key
    );
    assert_eq!(
        payload["effect"]["receipt"]["expected_state"],
        expected_state
    );
    let committed = payload["effect"]["receipt"]["committed_state"]
        .as_str()
        .unwrap_or_else(|| panic!("committed_state missing from {payload}"));
    assert_ne!(committed, expected_state, "apply must publish a new state");
    assert!(
        committed.starts_with("sha256:") && committed.len() == "sha256:".len() + 64,
        "committed_state must be a sha256 digest, got {committed}"
    );
    assert_eq!(
        payload["effect"]["payload"],
        durable_payload(line, before, file)
    );
}

fn read_file(project: &Path, relative: &str) -> String {
    fs::read_to_string(project.join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[tokio::test]
async fn insert_at_symbol_places_literal_source_before_and_after_the_symbol() {
    let (fixture, _dir) = open_pair().await;
    settle(&fixture).await;
    let project = fixture.project_root.clone();

    let after_preview = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "content": AVERAGE_FN,
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    let after_preview = success_body(&after_preview);
    let after_state = assert_stable(
        after_preview,
        json!({
            "success": true,
            "file_path": "src/after.rs",
            "anchor_line": 5,
            "content": AVERAGE_FN,
            "before": false,
            "dry_run": true,
            "diff": AFTER_DIFF,
            "message": AFTER_DRY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(read_file(&project, "src/after.rs"), AFTER_SOURCE);
    assert_eq!(read_file(&project, "src/before.rs"), BEFORE_SOURCE);

    let after_key = "mcp-test.insert-at-symbol.after-total";
    let after_apply = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "content": AVERAGE_FN,
            "idempotency_key": after_key,
            "expected_state": after_state,
            "format": "json"
        }),
    )
    .await;
    let after_apply = success_body(&after_apply);
    let after_effect_id = after_apply["effect"]["effect_id"].clone();
    assert_completed_effect(
        &after_apply,
        after_key,
        &after_state,
        5,
        false,
        "src/after.rs",
    );
    let applied_state = assert_stable(
        after_apply,
        json!({
            "success": true,
            "file_path": "src/after.rs",
            "anchor_line": 5,
            "content": AVERAGE_FN,
            "before": false,
            "message": AFTER_APPLY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(applied_state, after_state);
    assert_eq!(read_file(&project, "src/after.rs"), AFTER_APPLIED);
    assert_eq!(read_file(&project, "src/before.rs"), BEFORE_SOURCE);

    let replay = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "content": AVERAGE_FN,
            "idempotency_key": after_key,
            "expected_state": after_state,
            "format": "json"
        }),
    )
    .await;
    let replay = success_body(&replay);
    assert_eq!(replay["effect"]["effect_id"], after_effect_id);
    assert_completed_effect(&replay, after_key, &after_state, 5, false, "src/after.rs");
    let _replay_state = assert_stable(
        replay,
        json!({
            "success": true,
            "failed": false,
            "message": REPLAY_MESSAGE,
            "replayed": true,
        }),
    );
    assert_eq!(read_file(&project, "src/after.rs"), AFTER_APPLIED);

    let before_preview = call_insert(
        &fixture,
        json!({
            "symbol": "foo",
            "content": "// INSERTED",
            "position": "before",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    let before_preview = success_body(&before_preview);
    let before_state = assert_stable(
        before_preview,
        json!({
            "success": true,
            "file_path": "src/before.rs",
            "anchor_line": 2,
            "content": "// INSERTED",
            "before": true,
            "dry_run": true,
            "diff": BEFORE_DIFF,
            "message": BEFORE_DRY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(read_file(&project, "src/before.rs"), BEFORE_SOURCE);

    let before_key = "mcp-test.insert-at-symbol.before-foo";
    let before_apply = call_insert(
        &fixture,
        json!({
            "symbol": "foo",
            "content": "// INSERTED",
            "position": "before",
            "idempotency_key": before_key,
            "expected_state": before_state,
            "format": "json"
        }),
    )
    .await;
    let before_apply = success_body(&before_apply);
    assert_completed_effect(
        &before_apply,
        before_key,
        &before_state,
        2,
        true,
        "src/before.rs",
    );
    assert_stable(
        before_apply,
        json!({
            "success": true,
            "file_path": "src/before.rs",
            "anchor_line": 2,
            "content": "// INSERTED",
            "before": true,
            "message": BEFORE_APPLY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(read_file(&project, "src/before.rs"), BEFORE_APPLIED);
    assert_eq!(read_file(&project, "src/after.rs"), AFTER_APPLIED);

    close_production_source_edit_fixture(fixture).await;
}

#[tokio::test]
async fn insert_at_symbol_refuses_missing_ambiguous_invalid_and_stale_targets() {
    let (fixture, _dir) = open_sources(&[
        ("src/after.rs", SHARED_SOURCE),
        ("src/before.rs", OTHER_SHARED_SOURCE),
    ])
    .await;
    let project = fixture.project_root.clone();
    settle(&fixture).await;
    let unchanged_after = read_file(&project, "src/after.rs");
    let unchanged_before = read_file(&project, "src/before.rs");

    let missing = call_insert(
        &fixture,
        json!({
            "symbol": "missing_fn",
            "content": "// NO",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    assert_pre_effect_refusal(&missing, MISSING_SYMBOL);

    let missing_content = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    assert_rpc_error(
        &missing_content,
        -32602,
        MISSING_CONTENT,
        json!({
            "tool": "tracedecay_insert_at_symbol",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": MISSING_CONTENT,
        }),
    );

    let bad_position = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "content": "// NO",
            "position": "beside",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    assert_pre_effect_refusal(&bad_position, BAD_POSITION);

    let unpreviewed = call_insert(
        &fixture,
        json!({
            "symbol": "total",
            "content": "// NO",
            "format": "json"
        }),
    )
    .await;
    assert_rpc_error(
        &unpreviewed,
        -32603,
        format!("tool execution failed: config error: {MISSING_PREVIEW}"),
        json!({
            "tool": "tracedecay_insert_at_symbol",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool insert_at_symbol ...` (`tracedecay tool insert_at_symbol --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
        }),
    );

    let ambiguous = call_insert(
        &fixture,
        json!({
            "symbol": "shared",
            "content": "// NO",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    assert_pre_effect_refusal(&ambiguous, AMBIGUOUS_SHARED);
    assert_eq!(read_file(&project, "src/after.rs"), unchanged_after);
    assert_eq!(read_file(&project, "src/before.rs"), unchanged_before);

    let qualified_preview = call_insert(
        &fixture,
        json!({
            "symbol": "src/after.rs::shared",
            "content": "// QUALIFIED_AFTER",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    let qualified_preview = success_body(&qualified_preview);
    let qualified_state = assert_stable(
        qualified_preview,
        json!({
            "success": true,
            "file_path": "src/after.rs",
            "anchor_line": 6,
            "content": "// QUALIFIED_AFTER",
            "before": false,
            "dry_run": true,
            "diff": SHARED_DIFF,
            "message": SHARED_DRY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(read_file(&project, "src/after.rs"), unchanged_after);

    let qualified_key = "mcp-test.insert-at-symbol.qualified-shared";
    let qualified_apply = call_insert(
        &fixture,
        json!({
            "symbol": "src/after.rs::shared",
            "content": "// QUALIFIED_AFTER",
            "idempotency_key": qualified_key,
            "expected_state": qualified_state,
            "format": "json"
        }),
    )
    .await;
    let qualified_apply = success_body(&qualified_apply);
    assert_completed_effect(
        &qualified_apply,
        qualified_key,
        &qualified_state,
        6,
        false,
        "src/after.rs",
    );
    assert_stable(
        qualified_apply,
        json!({
            "success": true,
            "file_path": "src/after.rs",
            "anchor_line": 6,
            "content": "// QUALIFIED_AFTER",
            "before": false,
            "message": SHARED_APPLY_MESSAGE,
            "replayed": false,
        }),
    );
    assert_eq!(read_file(&project, "src/after.rs"), SHARED_AFTER_APPLIED);
    assert_eq!(read_file(&project, "src/before.rs"), unchanged_before);

    let stale_preview = call_insert(
        &fixture,
        json!({
            "symbol": "src/before.rs::shared",
            "content": "// STALE",
            "dry_run": true,
            "format": "json"
        }),
    )
    .await;
    let stale_preview = success_body(&stale_preview);
    let stale_state = stale_preview["expected_state"]
        .as_str()
        .expect("stale preview expected_state")
        .to_owned();
    fs::write(project.join("src/before.rs"), OTHER_SHARED_CONCURRENT).unwrap();
    settle(&fixture).await;
    let stale_apply = call_insert(
        &fixture,
        json!({
            "symbol": "src/before.rs::shared",
            "content": "// STALE",
            "idempotency_key": "mcp-test.insert-at-symbol.stale",
            "expected_state": stale_state,
            "format": "json"
        }),
    )
    .await;
    let stale_apply = failure_body(&stale_apply);
    assert_eq!(stale_apply["success"], false);
    assert_eq!(stale_apply["failed"], true);
    assert_eq!(stale_apply["message"], STALE_MESSAGE);
    assert_eq!(stale_apply["replayed"], false);
    assert_eq!(stale_apply["expected_state"], stale_state);
    assert_eq!(stale_apply["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(stale_apply["effect"]["payload"]["message"], STALE_MESSAGE);
    assert_eq!(stale_apply["effect"]["payload"]["success"], false);
    assert_eq!(stale_apply["effect"]["payload"]["failed"], true);
    assert_eq!(
        stale_apply["effect"]["payload"]["operation"],
        INSERT_OPERATION
    );
    assert_eq!(read_file(&project, "src/after.rs"), SHARED_AFTER_APPLIED);
    assert_eq!(
        read_file(&project, "src/before.rs"),
        OTHER_SHARED_CONCURRENT
    );

    close_production_source_edit_fixture(fixture).await;
}
