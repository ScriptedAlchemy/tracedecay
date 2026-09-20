//! `tracedecay_str_replace` as an MCP host calls it.
//!
//! Every case is one `tools/call` on the production server the daemon
//! composition mounts. The test reads the file bytes and the JSON-RPC answer
//! the host receives. A missing parameter is `-32602`; a config refusal is
//! `-32603`. A span miss is a tool result with `isError`, not a protocol
//! error. `format: json` is the public argument a host sends when it wants
//! the structured payload; digest fields are used only as the preview token
//! an apply must present, never as an expected result.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, extract_first_json_content,
    init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tracedecay_mcp::{JsonRpcError, JsonRpcResponse};

const PRICE_FILE: &str = "src/price.rs";
const OPERATION: &str = "use-case.application.source-edit.str-replace";

struct ToolAnswer {
    result: Value,
    payload: Value,
}

async fn open_file(
    relative: &str,
    bytes: &[u8],
) -> (ProductionSourceEditFixture, TestTempDir, PathBuf) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    let file = project.join(relative);
    fs::create_dir_all(file.parent().expect("fixture file has a parent")).unwrap();
    fs::write(&file, bytes).unwrap();
    let fixture = init_production_source_edit_project(&project).await;
    (fixture, dir, file)
}

async fn tools_call(
    fixture: &ProductionSourceEditFixture,
    mut arguments: Value,
) -> JsonRpcResponse {
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_str_replace", arguments)
        .await
        .expect("production tools/call");
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    response
}

async fn call_replace(fixture: &ProductionSourceEditFixture, arguments: Value) -> ToolAnswer {
    let response = tools_call(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "str_replace returned a protocol error: {:?}",
        response.error
    );
    let result = response
        .result
        .expect("tools/call result for tracedecay_str_replace");
    let payload = extract_first_json_content(&result);
    ToolAnswer { result, payload }
}

async fn protocol_error(fixture: &ProductionSourceEditFixture, arguments: Value) -> JsonRpcError {
    let response = tools_call(fixture, arguments).await;
    assert!(
        response.result.is_none(),
        "protocol refusal must not also return a tool result: {response:?}"
    );
    response
        .error
        .expect("tools/call protocol error for tracedecay_str_replace")
}

fn assert_client_outcome(answer: &ToolAnswer, success: bool) {
    assert_eq!(answer.payload["success"], success, "{}", answer.payload);
    if success {
        assert!(
            answer.result.get("isError").is_none(),
            "a completed edit must not be an MCP error: {}",
            answer.result
        );
    } else {
        assert_eq!(
            answer.result["isError"], true,
            "a refused edit must be an MCP error: {}",
            answer.result
        );
    }
}

fn assert_payload(actual: &Value, success: bool, files: &[&str], message: &str, failed: bool) {
    assert_eq!(
        actual["effect"]["payload"],
        json!({
            "operation": OPERATION,
            "success": success,
            "files": files,
            "change_count": null,
            "line": null,
            "before": null,
            "import_count": null,
            "finding_count": null,
            "failed": failed,
            "cancelled": false,
            "timed_out": false,
            "effect_unknown": false,
            "reconciled": false,
            "durable_metadata_only": true,
            "message": message,
        }),
        "durable payload in {actual}"
    );
}

fn preview_token(answer: &ToolAnswer) -> String {
    answer.payload["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned()
}

#[tokio::test]
async fn str_replace_writes_the_unique_span_and_reports_the_completed_edit() {
    let initial = b"fn price() -> u32 { 12 }\nfn keep() -> u32 { 7 }\n";
    let applied = "fn price() -> u32 { 40 }\nfn keep() -> u32 { 7 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;
    let expected_state = preview_token(&preview);
    assert_eq!(fs::read(&file).unwrap(), initial);

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.unique-span",
            "expected_state": expected_state
        }),
    )
    .await;

    assert_eq!(fs::read_to_string(&file).unwrap(), applied);
    assert_client_outcome(&answer, true);
    assert_eq!(
        json!({
            "success": answer.payload["success"],
            "file_path": answer.payload["file_path"],
            "matched_str": answer.payload["matched_str"],
            "new_str": answer.payload["new_str"],
            "replaced_span": answer.payload["replaced_span"],
            "message": answer.payload["message"],
            "replayed": answer.payload["replayed"],
        }),
        json!({
            "success": true,
            "file_path": PRICE_FILE,
            "matched_str": "12",
            "new_str": "40",
            "replaced_span": "12",
            "message": "replacement successful",
            "replayed": false,
        }),
        "{}",
        answer.payload
    );
    assert!(
        answer.payload.get("dry_run").is_none(),
        "{}",
        answer.payload
    );
    assert_eq!(answer.payload["effect"]["effect_class"], "source_edit");
    assert_eq!(
        answer.payload["effect"]["idempotency_key"],
        "str-replace.behavior.unique-span"
    );
    assert_eq!(answer.payload["effect"]["receipt"]["outcome"], "completed");
    assert_payload(
        &answer.payload,
        true,
        &[PRICE_FILE],
        "source edit completed; detailed edit output was not retained",
        false,
    );
}

#[tokio::test]
async fn str_replace_dry_run_previews_the_exact_diff_without_writing() {
    let initial = b"fn price() -> u32 { 12 }\nfn keep() -> u32 { 7 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, true);
    assert_eq!(answer.payload["dry_run"], true);
    assert_eq!(answer.payload["replayed"], false);
    assert_eq!(answer.payload["file_path"], PRICE_FILE);
    assert_eq!(answer.payload["matched_str"], "12");
    assert_eq!(answer.payload["new_str"], "40");
    assert_eq!(answer.payload["replaced_span"], "12");
    assert_eq!(
        answer.payload["message"],
        "dry run. Nothing written; preview only (replacement successful)"
    );
    assert_eq!(
        answer.payload["diff"],
        "@@ -1,2 +1,2 @@\n-fn price() -> u32 { 12 }\n+fn price() -> u32 { 40 }\n fn keep() -> u32 { 7 }"
    );
    assert_eq!(answer.payload["effect"]["receipt"]["outcome"], "completed");
    assert_payload(
        &answer.payload,
        true,
        &[PRICE_FILE],
        "source edit completed; detailed edit output was not retained",
        false,
    );
}

#[tokio::test]
async fn str_replace_reports_a_missing_span_and_leaves_the_file() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "99",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;
    assert_client_outcome(&preview, false);
    assert_eq!(
        preview.payload["message"],
        "old_str not found in src/price.rs"
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
    let expected_state = preview_token(&preview);

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "99",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.missing-span",
            "expected_state": expected_state
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, false);
    assert_eq!(answer.payload["replayed"], false);
    assert_eq!(answer.payload["file_path"], PRICE_FILE);
    assert_eq!(answer.payload["matched_str"], "99");
    assert_eq!(answer.payload["new_str"], "40");
    assert_eq!(
        answer.payload["message"],
        "old_str not found in src/price.rs"
    );
    assert!(
        answer.payload.get("replaced_span").is_none(),
        "{}",
        answer.payload
    );
    assert_eq!(answer.payload["effect"]["receipt"]["outcome"], "failed");
    // A span miss is an edit that ran and found nothing, not a pre-effect
    // refusal. The receipt outcome is `failed`; the retained metadata message
    // the host receives is the completed-edit sentence with `success: false`.
    assert_payload(
        &answer.payload,
        false,
        &[PRICE_FILE],
        "source edit completed; detailed edit output was not retained",
        false,
    );
}

#[tokio::test]
async fn str_replace_refuses_an_ambiguous_span_and_leaves_the_file() {
    let initial = b"fn price() -> u32 { 12 }\nfn other() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, false);
    assert_eq!(answer.payload["file_path"], PRICE_FILE);
    assert_eq!(answer.payload["matched_str"], "12");
    assert_eq!(answer.payload["new_str"], "40");
    assert_eq!(
        answer.payload["message"],
        "old_str matches 2 times, must match exactly once"
    );
    assert!(
        answer.payload.get("replaced_span").is_none(),
        "{}",
        answer.payload
    );
    assert!(answer.payload.get("diff").is_none(), "{}", answer.payload);
}

#[tokio::test]
async fn str_replace_apply_without_preview_state_is_refused() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let error = protocol_error(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40"
        }),
    )
    .await;

    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
    );
    assert_eq!(
        error.data.as_ref().and_then(|data| data["tool"].as_str()),
        Some("tracedecay_str_replace")
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
}

#[tokio::test]
async fn str_replace_refuses_a_stale_preview_and_keeps_concurrent_bytes() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let concurrent = b"fn price() -> u32 { 12 }\n// concurrent bytes\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;
    let expected_state = preview_token(&preview);
    fs::write(&file, concurrent).unwrap();

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.stale-preview",
            "expected_state": expected_state
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), concurrent);
    assert_client_outcome(&answer, false);
    assert_eq!(answer.payload["failed"], true);
    assert_eq!(answer.payload["replayed"], false);
    assert_eq!(
        answer.payload["message"],
        "source edit failed before the effect"
    );
    assert!(answer.payload["effect"]["receipt"]["committed_state"].is_null());
    assert_eq!(answer.payload["effect"]["receipt"]["outcome"], "failed");
    assert_payload(
        &answer.payload,
        false,
        &[],
        "source edit failed before the effect",
        true,
    );
}

#[tokio::test]
async fn str_replace_replay_does_not_apply_the_same_span_twice() {
    let initial = b"fn price() -> u32 { 12 }\n";
    // `12` is still inside `12 + 1`, so executing the same call again would
    // write `12 + 1 + 1`. Replay must leave the first result.
    let once = "fn price() -> u32 { 12 + 1 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "12 + 1",
            "dry_run": true
        }),
    )
    .await;
    let expected_state = preview_token(&preview);
    let args = json!({
        "path": PRICE_FILE,
        "old_str": "12",
        "new_str": "12 + 1",
        "idempotency_key": "str-replace.behavior.replay",
        "expected_state": expected_state
    });

    let first = call_replace(&fixture, args.clone()).await;
    assert_eq!(fs::read_to_string(&file).unwrap(), once);
    assert_client_outcome(&first, true);
    assert_eq!(first.payload["replayed"], false);
    assert_eq!(first.payload["matched_str"], "12");
    assert_eq!(first.payload["replaced_span"], "12");
    assert_eq!(first.payload["message"], "replacement successful");

    let replay = call_replace(&fixture, args).await;

    assert_eq!(fs::read_to_string(&file).unwrap(), once);
    assert_client_outcome(&replay, true);
    assert_eq!(replay.payload["replayed"], true);
    assert!(
        replay.payload.get("matched_str").is_none(),
        "{}",
        replay.payload
    );
    assert!(
        replay.payload.get("replaced_span").is_none(),
        "{}",
        replay.payload
    );
    assert_eq!(
        replay.payload["effect"]["effect_id"],
        first.payload["effect"]["effect_id"]
    );
    assert_eq!(
        replay.payload["message"],
        "source edit completed; detailed edit output was not retained"
    );
    assert_payload(
        &replay.payload,
        true,
        &[PRICE_FILE],
        "source edit completed; detailed edit output was not retained",
        false,
    );
}

#[tokio::test]
async fn str_replace_refuses_a_path_outside_the_worktree() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let outside_bytes = b"SECRET\n";
    let (fixture, dir, file) = open_file(PRICE_FILE, initial).await;
    let outside = dir.path().join("outside.rs");
    fs::write(&outside, outside_bytes).unwrap();

    let answer = call_replace(
        &fixture,
        json!({
            "path": "../outside.rs",
            "old_str": "SECRET",
            "new_str": "LEAKED",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(fs::read(&outside).unwrap(), outside_bytes);
    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, false);
    assert_eq!(answer.payload["failed"], true);
    assert_eq!(
        answer.payload["message"],
        "source edit failed before the effect: config error: path is not within the project"
    );
    assert_payload(
        &answer.payload,
        false,
        &[],
        "source edit failed before the effect",
        true,
    );
}

#[tokio::test]
async fn str_replace_deletes_a_unique_span_when_the_replacement_is_empty() {
    let initial = b"alpha\nREMOVE_ME\nomega\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "REMOVE_ME\n",
            "new_str": "",
            "dry_run": true
        }),
    )
    .await;
    let expected_state = preview_token(&preview);

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "REMOVE_ME\n",
            "new_str": "",
            "idempotency_key": "str-replace.behavior.delete-span",
            "expected_state": expected_state
        }),
    )
    .await;

    assert_eq!(fs::read_to_string(&file).unwrap(), "alpha\nomega\n");
    assert_client_outcome(&answer, true);
    assert_eq!(answer.payload["matched_str"], "REMOVE_ME\n");
    assert_eq!(answer.payload["new_str"], "");
    assert_eq!(answer.payload["replaced_span"], "REMOVE_ME\n");
    assert_eq!(answer.payload["message"], "replacement successful");
    assert_eq!(answer.payload["replayed"], false);
}

#[tokio::test]
async fn str_replace_preserves_crlf_bytes_around_the_span() {
    let initial = b"price: 12\r\nkeep: yes\r\n";
    let applied = b"price: 40\r\nkeep: yes\r\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let preview = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;
    let expected_state = preview_token(&preview);

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.crlf",
            "expected_state": expected_state
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), applied);
    assert_client_outcome(&answer, true);
    assert_eq!(answer.payload["message"], "replacement successful");
    assert_eq!(answer.payload["replaced_span"], "12");
}

#[tokio::test]
async fn str_replace_identical_replacement_previews_no_changes() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "12",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, true);
    assert_eq!(answer.payload["dry_run"], true);
    assert_eq!(answer.payload["diff"], "(no changes)");
    assert_eq!(
        answer.payload["message"],
        "dry run. Nothing written; preview only (replacement successful)"
    );
    assert_eq!(answer.payload["matched_str"], "12");
    assert_eq!(answer.payload["new_str"], "12");
    assert_eq!(answer.payload["replaced_span"], "12");
}

#[tokio::test]
async fn str_replace_empty_old_str_reports_every_match() {
    let initial = b"ab\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let answer = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "",
            "new_str": "x",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_client_outcome(&answer, false);
    assert_eq!(
        answer.payload["message"],
        "old_str matches 4 times, must match exactly once"
    );
    assert_eq!(answer.payload["matched_str"], "");
    assert_eq!(answer.payload["new_str"], "x");
    assert!(answer.payload.get("diff").is_none(), "{}", answer.payload);
}

#[tokio::test]
async fn str_replace_missing_old_str_is_a_parameter_error() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let error = protocol_error(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing required parameter: old_str");
    assert_eq!(
        error.data,
        Some(json!({
            "tool": "tracedecay_str_replace",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: old_str"
        }))
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
}

#[tokio::test]
async fn str_replace_refuses_project_selectors() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let error = protocol_error(
        &fixture,
        json!({
            "project_selector": {"include_all_registered": true},
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40"
        }),
    )
    .await;

    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: tracedecay_str_replace is scoped to the active project and does not accept project selectors"
    );
    assert_eq!(
        error.data.as_ref().and_then(|data| data["tool"].as_str()),
        Some("tracedecay_str_replace")
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
}
