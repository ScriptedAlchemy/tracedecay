//! `tracedecay_str_replace` as an MCP client sees it.
//!
//! Every case dispatches through the production source-edit server, then
//! compares the file bytes and the JSON the tool returns with literals. Digest
//! fields are used only as the preview token the apply call must present; they
//! are not restated as the expected result.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, expect_tool_error, extract_first_json_content,
    handle_production_source_edit_tool_call, init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tracedecay_mcp::ToolResult;

const PRICE_FILE: &str = "src/price.rs";
const OPERATION: &str = "use-case.application.source-edit.str-replace";

async fn open_file(
    relative: &str,
    bytes: &[u8],
) -> (ProductionSourceEditFixture, TestTempDir, PathBuf) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    let file = project.join(relative);
    fs::create_dir_all(file.parent().expect("fixture file has a parent")).unwrap();
    fs::write(&file, bytes).unwrap();
    let (fixture, ()) = init_production_source_edit_project(&project).await;
    (fixture, dir, file)
}

async fn call_replace(
    fixture: &ProductionSourceEditFixture,
    args: Value,
) -> tracedecay_domain::errors::Result<ToolResult> {
    handle_production_source_edit_tool_call(fixture, "tracedecay_str_replace", args, None, None)
        .await
}

fn tool_json(result: &ToolResult) -> Value {
    extract_first_json_content(&result.value)
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
    .await
    .expect("preview call");
    let preview = tool_json(&preview);
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();
    assert_eq!(fs::read(&file).unwrap(), initial);

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.unique-span",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("apply call");
    let parsed = tool_json(&result);

    assert_eq!(fs::read_to_string(&file).unwrap(), applied);
    assert_eq!(
        json!({
            "success": parsed["success"],
            "file_path": parsed["file_path"],
            "matched_str": parsed["matched_str"],
            "new_str": parsed["new_str"],
            "replaced_span": parsed["replaced_span"],
            "message": parsed["message"],
            "replayed": parsed["replayed"],
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
        "{parsed}"
    );
    assert!(parsed.get("dry_run").is_none(), "{parsed}");
    assert_eq!(parsed["effect"]["effect_class"], "source_edit");
    assert_eq!(
        parsed["effect"]["idempotency_key"],
        "str-replace.behavior.unique-span"
    );
    assert_eq!(parsed["effect"]["receipt"]["outcome"], "completed");
    assert_payload(
        &parsed,
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

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await
    .expect("dry run");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["replayed"], false);
    assert_eq!(parsed["file_path"], PRICE_FILE);
    assert_eq!(parsed["matched_str"], "12");
    assert_eq!(parsed["new_str"], "40");
    assert_eq!(parsed["replaced_span"], "12");
    assert_eq!(
        parsed["message"],
        "dry run. Nothing written; preview only (replacement successful)"
    );
    assert_eq!(
        parsed["diff"],
        "@@ -1,2 +1,2 @@\n-fn price() -> u32 { 12 }\n+fn price() -> u32 { 40 }\n fn keep() -> u32 { 7 }"
    );
    assert_eq!(parsed["effect"]["receipt"]["outcome"], "completed");
    assert_payload(
        &parsed,
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
    .await
    .expect("missing-span preview");
    let preview = tool_json(&preview);
    assert_eq!(preview["success"], false);
    assert_eq!(preview["message"], "old_str not found in src/price.rs");
    assert_eq!(fs::read(&file).unwrap(), initial);
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "99",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.missing-span",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("missing-span apply");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], false);
    assert_eq!(parsed["replayed"], false);
    assert_eq!(parsed["file_path"], PRICE_FILE);
    assert_eq!(parsed["matched_str"], "99");
    assert_eq!(parsed["new_str"], "40");
    assert_eq!(parsed["message"], "old_str not found in src/price.rs");
    assert!(parsed.get("replaced_span").is_none(), "{parsed}");
    assert_eq!(parsed["effect"]["receipt"]["outcome"], "failed");
    assert_payload(
        &parsed,
        false,
        &[PRICE_FILE],
        "source edit failed; detailed edit output was not retained",
        false,
    );
}

#[tokio::test]
async fn str_replace_refuses_an_ambiguous_span_and_leaves_the_file() {
    let initial = b"fn price() -> u32 { 12 }\nfn other() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "dry_run": true
        }),
    )
    .await
    .expect("ambiguous preview");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], false);
    assert_eq!(parsed["file_path"], PRICE_FILE);
    assert_eq!(parsed["matched_str"], "12");
    assert_eq!(parsed["new_str"], "40");
    assert_eq!(
        parsed["message"],
        "old_str matches 2 times, must match exactly once"
    );
    assert!(parsed.get("replaced_span").is_none(), "{parsed}");
    assert!(parsed.get("diff").is_none(), "{parsed}");
}

#[tokio::test]
async fn str_replace_apply_without_preview_state_is_refused() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted source-edit server");

    let denied = server
        .call_tool_for_test(
            "tracedecay_str_replace",
            json!({
                "path": PRICE_FILE,
                "old_str": "12",
                "new_str": "40"
            }),
        )
        .await;

    assert_eq!(
        expect_tool_error(denied),
        "config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
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
    .await
    .expect("stale preview");
    let expected_state = tool_json(&preview)["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();
    fs::write(&file, concurrent).unwrap();

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.stale-preview",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("stale apply");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), concurrent);
    assert_eq!(parsed["success"], false);
    assert_eq!(parsed["failed"], true);
    assert_eq!(parsed["replayed"], false);
    assert_eq!(parsed["message"], "source edit failed before the effect");
    assert!(parsed["effect"]["receipt"]["committed_state"].is_null());
    assert_eq!(parsed["effect"]["receipt"]["outcome"], "failed");
    assert_payload(
        &parsed,
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
    .await
    .expect("replay preview");
    let expected_state = tool_json(&preview)["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();
    let args = json!({
        "path": PRICE_FILE,
        "old_str": "12",
        "new_str": "12 + 1",
        "idempotency_key": "str-replace.behavior.replay",
        "expected_state": expected_state
    });

    let first = tool_json(
        &call_replace(&fixture, args.clone())
            .await
            .expect("first apply"),
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), once);
    assert_eq!(first["success"], true);
    assert_eq!(first["replayed"], false);
    assert_eq!(first["matched_str"], "12");
    assert_eq!(first["replaced_span"], "12");
    assert_eq!(first["message"], "replacement successful");

    let replay = tool_json(&call_replace(&fixture, args).await.expect("replay"));

    assert_eq!(fs::read_to_string(&file).unwrap(), once);
    assert_eq!(replay["success"], true);
    assert_eq!(replay["replayed"], true);
    assert!(replay.get("matched_str").is_none(), "{replay}");
    assert!(replay.get("replaced_span").is_none(), "{replay}");
    assert_eq!(replay["effect"]["effect_id"], first["effect"]["effect_id"]);
    assert_eq!(
        replay["message"],
        "source edit completed; detailed edit output was not retained"
    );
    assert_eq!(replay["durable_metadata_only"], true);
    assert_eq!(replay["operation"], OPERATION);
    assert_eq!(replay["files"], json!([PRICE_FILE]));
}

#[tokio::test]
async fn str_replace_refuses_a_path_outside_the_worktree() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let outside_bytes = b"SECRET\n";
    let (fixture, dir, file) = open_file(PRICE_FILE, initial).await;
    let outside = dir.path().join("outside.rs");
    fs::write(&outside, outside_bytes).unwrap();
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted source-edit server");

    let result = server
        .call_tool_for_test(
            "tracedecay_str_replace",
            json!({
                "path": "../outside.rs",
                "old_str": "SECRET",
                "new_str": "LEAKED",
                "dry_run": true,
                "format": "json"
            }),
        )
        .await
        .expect("path refusal is a tool result");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&outside).unwrap(), outside_bytes);
    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], false);
    assert_eq!(parsed["failed"], true);
    assert_eq!(
        parsed["message"],
        "source edit failed before the effect: config error: path is not within the project"
    );
    assert_payload(
        &parsed,
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
    .await
    .expect("delete preview");
    let expected_state = tool_json(&preview)["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "REMOVE_ME\n",
            "new_str": "",
            "idempotency_key": "str-replace.behavior.delete-span",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("delete apply");
    let parsed = tool_json(&result);

    assert_eq!(fs::read_to_string(&file).unwrap(), "alpha\nomega\n");
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["matched_str"], "REMOVE_ME\n");
    assert_eq!(parsed["new_str"], "");
    assert_eq!(parsed["replaced_span"], "REMOVE_ME\n");
    assert_eq!(parsed["message"], "replacement successful");
    assert_eq!(parsed["replayed"], false);
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
    .await
    .expect("crlf preview");
    let expected_state = tool_json(&preview)["expected_state"]
        .as_str()
        .expect("preview token")
        .to_owned();

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "40",
            "idempotency_key": "str-replace.behavior.crlf",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("crlf apply");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), applied);
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["message"], "replacement successful");
    assert_eq!(parsed["replaced_span"], "12");
}

#[tokio::test]
async fn str_replace_identical_replacement_previews_no_changes() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "12",
            "new_str": "12",
            "dry_run": true
        }),
    )
    .await
    .expect("identical preview");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["diff"], "(no changes)");
    assert_eq!(
        parsed["message"],
        "dry run. Nothing written; preview only (replacement successful)"
    );
    assert_eq!(parsed["matched_str"], "12");
    assert_eq!(parsed["new_str"], "12");
    assert_eq!(parsed["replaced_span"], "12");
}

#[tokio::test]
async fn str_replace_empty_old_str_reports_every_match() {
    let initial = b"ab\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;

    let result = call_replace(
        &fixture,
        json!({
            "path": PRICE_FILE,
            "old_str": "",
            "new_str": "x",
            "dry_run": true
        }),
    )
    .await
    .expect("empty old_str");
    let parsed = tool_json(&result);

    assert_eq!(fs::read(&file).unwrap(), initial);
    assert_eq!(parsed["success"], false);
    assert_eq!(
        parsed["message"],
        "old_str matches 4 times, must match exactly once"
    );
    assert_eq!(parsed["matched_str"], "");
    assert_eq!(parsed["new_str"], "x");
    assert!(parsed.get("diff").is_none(), "{parsed}");
}

#[tokio::test]
async fn str_replace_missing_old_str_is_a_parameter_error() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted source-edit server");

    let denied = server
        .call_tool_for_test(
            "tracedecay_str_replace",
            json!({
                "path": PRICE_FILE,
                "new_str": "40",
                "dry_run": true
            }),
        )
        .await;

    assert_eq!(
        expect_tool_error(denied),
        "config error: missing required parameter: old_str"
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
}

#[tokio::test]
async fn str_replace_refuses_project_selectors() {
    let initial = b"fn price() -> u32 { 12 }\n";
    let (fixture, _dir, file) = open_file(PRICE_FILE, initial).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted source-edit server");

    let denied = server
        .call_tool_for_test(
            "tracedecay_str_replace",
            json!({
                "project_selector": {"include_all_registered": true},
                "path": PRICE_FILE,
                "old_str": "12",
                "new_str": "40"
            }),
        )
        .await;

    assert_eq!(
        expect_tool_error(denied),
        "config error: tracedecay_str_replace is scoped to the active project and does not accept project selectors"
    );
    assert_eq!(fs::read(&file).unwrap(), initial);
}
