//! Observable `tracedecay_insert_at` behavior through the production MCP server.
//!
//! Each test opens a real project, sends `tools/call` over the server connection
//! the daemon uses, and checks the file bytes and response fields a caller sees.

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, close_production_source_edit_fixture, extract_json,
    handle_real_server_tool_call, handle_real_server_tool_call_raw,
    init_production_source_edit_project, test_temp_dir,
};

const AFTER_ORIGINAL: &str = "alpha\nbeta\ngamma\n";
const AFTER_APPLIED: &str = "alpha\nbeta\ninserted line\ngamma\n";
const AFTER_DIFF: &str = "@@ -1,3 +1,4 @@\n alpha\n beta\n+inserted line\n gamma";
const BEFORE_ORIGINAL: &str = "one\ntwo\nthree\n";
const BEFORE_APPLIED: &str = "one\nzero\ntwo\nthree\n";
const BEFORE_DIFF: &str = "@@ -1,3 +1,4 @@\n one\n+zero\n two\n three";
const LINE_ORIGINAL: &str = "red\ngreen\nblue\n";
const LINE_AFTER_APPLIED: &str = "red\ngreen\nyellow\nblue\n";
const LINE_AFTER_DIFF: &str = "@@ -1,3 +1,4 @@\n red\n green\n+yellow\n blue";
const LINE_BEFORE_ORIGINAL: &str = "red\ngreen\n";
const LINE_BEFORE_APPLIED: &str = "lead\nred\ngreen\n";
const LINE_BEFORE_DIFF: &str = "@@ -1,2 +1,3 @@\n+lead\n red\n green";
const REFUSAL_ORIGINAL: &str = "only once\nonly once\nend\n";

struct InsertProject {
    _dir: TestTempDir,
    fixture: ProductionSourceEditFixture,
    server: Arc<McpServer>,
}

async fn open_project(files: &[(&str, &str)]) -> InsertProject {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    for (relative, contents) in files {
        let path = project_root.join(relative);
        fs::create_dir_all(path.parent().unwrap_or(&project_root)).unwrap();
        fs::write(path, contents).unwrap();
    }
    let fixture = init_production_source_edit_project(&project_root).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    InsertProject {
        _dir: dir,
        fixture,
        server,
    }
}

impl InsertProject {
    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.fixture.project_root.join(relative)).unwrap()
    }

    async fn call(&self, args: Value) -> Value {
        handle_real_server_tool_call(&self.server, "tracedecay_insert_at", args).await
    }

    async fn close(self) {
        close_production_source_edit_fixture(self.fixture).await;
    }
}

fn body(result: &Value) -> Value {
    extract_json(result)
}

fn assert_rpc_error(response: &Value, code: i64, message: &str) {
    assert!(
        response.get("result").is_none() || response["result"].is_null(),
        "refused insert must not return a tool result: {response}"
    );
    assert_eq!(response["error"]["code"], code, "{response}");
    assert_eq!(response["error"]["message"], message);
}

#[tokio::test]
async fn insert_at_after_unique_anchor_previews_applies_replays_and_refuses_stale_bytes() {
    let project = open_project(&[("src/main.rs", AFTER_ORIGINAL)]).await;

    let preview_result = project
        .call(json!({
            "path": "src/main.rs",
            "anchor": "beta",
            "content": "inserted line\n",
            "dry_run": true
        }))
        .await;
    let preview = body(&preview_result);
    assert_eq!(preview["success"], true);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["file_path"], "src/main.rs");
    assert_eq!(preview["anchor_line"], 2);
    assert_eq!(preview["before"], false);
    assert_eq!(preview["content"], "inserted line\n");
    assert_eq!(
        preview["message"],
        "dry run. Nothing written; preview only (inserted at line 2)"
    );
    assert_eq!(preview["diff"], AFTER_DIFF);
    assert_eq!(project.read("src/main.rs"), AFTER_ORIGINAL);
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns the candidate digest")
        .to_owned();

    let apply_args = json!({
        "path": "src/main.rs",
        "anchor": "beta",
        "content": "inserted line\n",
        "idempotency_key": "mcp-test.insert-at.after",
        "expected_state": expected_state
    });
    let applied_result = project.call(apply_args.clone()).await;
    let applied = body(&applied_result);
    assert_eq!(applied["success"], true);
    assert_eq!(applied["replayed"], false);
    assert_eq!(applied["file_path"], "src/main.rs");
    assert_eq!(applied["anchor_line"], 2);
    assert_eq!(applied["before"], false);
    assert_eq!(applied["content"], "inserted line\n");
    assert_eq!(applied["message"], "inserted at line 2");
    assert_eq!(applied["effect"]["effect_class"], "source_edit");
    assert_eq!(
        applied["effect"]["idempotency_key"],
        "mcp-test.insert-at.after"
    );
    assert_eq!(applied["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(
        applied["effect"]["receipt"]["expected_state"],
        expected_state
    );
    assert_eq!(
        applied["effect"]["payload"]["operation"],
        "use-case.application.source-edit.insert-at"
    );
    assert_eq!(applied["effect"]["payload"]["success"], true);
    assert_eq!(
        applied["effect"]["payload"]["files"],
        json!(["src/main.rs"])
    );
    assert_eq!(applied["effect"]["payload"]["line"], 2);
    assert_eq!(applied["effect"]["payload"]["before"], false);
    assert_eq!(applied["effect"]["payload"]["durable_metadata_only"], true);
    assert!(
        applied["effect"]["receipt"]["committed_state"].is_string(),
        "completed insert receipt names the committed bytes: {applied}"
    );
    assert_eq!(project.read("src/main.rs"), AFTER_APPLIED);

    let replayed_result = project.call(apply_args).await;
    let replayed = body(&replayed_result);
    assert_eq!(replayed["success"], true);
    assert_eq!(replayed["failed"], false);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(
        replayed["message"],
        "source edit completed; detailed edit output was not retained"
    );
    assert_eq!(replayed["content"], Value::Null);
    assert_eq!(replayed["diff"], Value::Null);
    assert_eq!(replayed["file_path"], Value::Null);
    assert_eq!(replayed["effect"]["payload"]["durable_metadata_only"], true);
    assert_eq!(
        replayed["effect"]["payload"]["files"],
        json!(["src/main.rs"])
    );
    assert_eq!(replayed["effect"]["payload"]["line"], 2);
    assert_eq!(replayed["effect"]["payload"]["before"], false);
    assert_eq!(replayed["effect"]["payload"], applied["effect"]["payload"]);
    assert_eq!(
        replayed["effect"]["effect_id"],
        applied["effect"]["effect_id"]
    );
    assert_eq!(replayed["effect"]["receipt"], applied["effect"]["receipt"]);
    assert_eq!(project.read("src/main.rs"), AFTER_APPLIED);

    let stale_preview = body(
        &project
            .call(json!({
                "path": "src/main.rs",
                "anchor": "gamma",
                "content": "tail\n",
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(project.read("src/main.rs"), AFTER_APPLIED);
    let stale_expected_state = stale_preview["expected_state"]
        .as_str()
        .expect("second preview returns the candidate digest")
        .to_owned();
    let concurrent = "alpha\nbeta\nCONCURRENT\ngamma\n";
    fs::write(project.fixture.project_root.join("src/main.rs"), concurrent).unwrap();
    let stale = body(
        &project
            .call(json!({
                "path": "src/main.rs",
                "anchor": "gamma",
                "content": "tail\n",
                "idempotency_key": "mcp-test.insert-at.stale",
                "expected_state": stale_expected_state
            }))
            .await,
    );
    assert_eq!(stale["success"], false);
    assert_eq!(stale["failed"], true);
    assert_eq!(stale["replayed"], false);
    assert_eq!(stale["message"], "source edit failed before the effect");
    assert_eq!(stale["effect"]["receipt"]["outcome"], "failed");
    assert!(stale["effect"]["receipt"]["committed_state"].is_null());
    assert_eq!(project.read("src/main.rs"), concurrent);

    project.close().await;
}

#[tokio::test]
async fn insert_at_before_anchor_and_line_numbers_write_exact_bytes() {
    let project = open_project(&[
        ("src/before.rs", BEFORE_ORIGINAL),
        ("src/line_after.rs", LINE_ORIGINAL),
        ("src/line_before.rs", LINE_BEFORE_ORIGINAL),
    ])
    .await;

    let before_preview = body(
        &project
            .call(json!({
                "path": "src/before.rs",
                "anchor": "two",
                "content": "zero\n",
                "before": true,
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(before_preview["success"], true);
    assert_eq!(before_preview["before"], true);
    assert_eq!(before_preview["anchor_line"], 2);
    assert_eq!(
        before_preview["message"],
        "dry run. Nothing written; preview only (inserted at line 2)"
    );
    assert_eq!(before_preview["diff"], BEFORE_DIFF);
    assert_eq!(project.read("src/before.rs"), BEFORE_ORIGINAL);
    let before_state = before_preview["expected_state"]
        .as_str()
        .expect("before preview returns the candidate digest")
        .to_owned();
    let before_applied = body(
        &project
            .call(json!({
                "path": "src/before.rs",
                "anchor": "two",
                "content": "zero\n",
                "before": true,
                "idempotency_key": "mcp-test.insert-at.before",
                "expected_state": before_state
            }))
            .await,
    );
    assert_eq!(before_applied["success"], true);
    assert_eq!(before_applied["message"], "inserted at line 2");
    assert_eq!(before_applied["anchor_line"], 2);
    assert_eq!(before_applied["before"], true);
    assert_eq!(project.read("src/before.rs"), BEFORE_APPLIED);

    let line_after_preview = body(
        &project
            .call(json!({
                "path": "src/line_after.rs",
                "anchor": "2",
                "content": "yellow",
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(line_after_preview["success"], true);
    assert_eq!(line_after_preview["before"], false);
    assert_eq!(line_after_preview["anchor_line"], 2);
    assert_eq!(line_after_preview["content"], "yellow");
    assert_eq!(
        line_after_preview["message"],
        "dry run. Nothing written; preview only (inserted at line 2)"
    );
    assert_eq!(line_after_preview["diff"], LINE_AFTER_DIFF);
    assert_eq!(project.read("src/line_after.rs"), LINE_ORIGINAL);
    let line_after_state = line_after_preview["expected_state"]
        .as_str()
        .expect("line preview returns the candidate digest")
        .to_owned();
    let line_after = body(
        &project
            .call(json!({
                "path": "src/line_after.rs",
                "anchor": "2",
                "content": "yellow",
                "idempotency_key": "mcp-test.insert-at.line-after",
                "expected_state": line_after_state
            }))
            .await,
    );
    assert_eq!(line_after["success"], true);
    assert_eq!(line_after["message"], "inserted at line 2");
    assert_eq!(project.read("src/line_after.rs"), LINE_AFTER_APPLIED);

    let line_before_preview = body(
        &project
            .call(json!({
                "path": "src/line_before.rs",
                "anchor": "1",
                "content": "lead",
                "before": true,
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(line_before_preview["anchor_line"], 1);
    assert_eq!(line_before_preview["before"], true);
    assert_eq!(
        line_before_preview["message"],
        "dry run. Nothing written; preview only (inserted at line 1)"
    );
    assert_eq!(line_before_preview["diff"], LINE_BEFORE_DIFF);
    assert_eq!(project.read("src/line_before.rs"), LINE_BEFORE_ORIGINAL);
    let line_before_state = line_before_preview["expected_state"]
        .as_str()
        .expect("leading-line preview returns the candidate digest")
        .to_owned();
    let line_before = body(
        &project
            .call(json!({
                "path": "src/line_before.rs",
                "anchor": "1",
                "content": "lead",
                "before": true,
                "idempotency_key": "mcp-test.insert-at.line-before",
                "expected_state": line_before_state
            }))
            .await,
    );
    assert_eq!(line_before["success"], true);
    assert_eq!(line_before["message"], "inserted at line 1");
    assert_eq!(line_before["anchor_line"], 1);
    assert_eq!(project.read("src/line_before.rs"), LINE_BEFORE_APPLIED);

    project.close().await;
}

#[tokio::test]
async fn insert_at_refuses_unusable_anchors_missing_files_and_escaped_paths() {
    let project = open_project(&[("src/refuse.rs", REFUSAL_ORIGINAL)]).await;
    let outside = project
        .fixture
        .project_root
        .parent()
        .expect("project has an isolation parent")
        .join("outside.txt");
    fs::write(&outside, "DO NOT TOUCH\n").unwrap();

    let missing = body(
        &project
            .call(json!({
                "path": "src/refuse.rs",
                "anchor": "missing",
                "content": "nope\n",
                "before": true,
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(missing["success"], false);
    assert_eq!(missing["anchor_line"], 0);
    assert_eq!(missing["message"], "anchor 'missing' not found");
    assert_eq!(missing["content"], "nope\n");
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let ambiguous_result = project
        .call(json!({
            "path": "src/refuse.rs",
            "anchor": "only once",
            "content": "nope\n",
            "dry_run": true
        }))
        .await;
    assert_eq!(ambiguous_result["isError"], true);
    let ambiguous = body(&ambiguous_result);
    assert_eq!(ambiguous["success"], false);
    assert_eq!(ambiguous["anchor_line"], 2);
    assert_eq!(
        ambiguous["message"],
        "anchor 'only once' matches 2 lines, must match exactly one"
    );
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let out_of_range = body(
        &project
            .call(json!({
                "path": "src/refuse.rs",
                "anchor": "9",
                "content": "nope",
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(out_of_range["success"], false);
    assert_eq!(out_of_range["anchor_line"], 9);
    assert_eq!(
        out_of_range["message"],
        "line number 9 out of range (file has 3 lines)"
    );
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let zero = body(
        &project
            .call(json!({
                "path": "src/refuse.rs",
                "anchor": "0",
                "content": "nope",
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(zero["success"], false);
    assert_eq!(
        zero["message"],
        "line number 0 out of range (file has 3 lines)"
    );
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let unicode_anchor = format!("{}é", "a".repeat(99));
    let unicode = body(
        &project
            .call(json!({
                "path": "src/refuse.rs",
                "anchor": unicode_anchor,
                "content": "nope",
                "dry_run": true
            }))
            .await,
    );
    assert_eq!(unicode["success"], false);
    assert_eq!(
        unicode["message"],
        format!("anchor '{unicode_anchor}' not found")
    );
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let absent_result = project
        .call(json!({
            "path": "src/missing.rs",
            "anchor": "anything",
            "content": "nope",
            "dry_run": true
        }))
        .await;
    assert_eq!(absent_result["isError"], true);
    let absent = body(&absent_result);
    assert_eq!(absent["success"], false);
    assert_eq!(absent["failed"], true);
    assert_eq!(absent["replayed"], false);
    assert_eq!(
        absent["message"],
        "source edit failed before the effect: config error: failed to read src/missing.rs: file was not found"
    );
    assert_eq!(absent["effect"]["receipt"]["outcome"], "failed");
    assert!(absent["effect"]["receipt"]["committed_state"].is_null());
    assert!(!project.fixture.project_root.join("src/missing.rs").exists());
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let bare_apply = handle_real_server_tool_call_raw(
        &project.server,
        "tracedecay_insert_at",
        json!({
            "path": "src/refuse.rs",
            "anchor": "end",
            "content": "nope\n"
        }),
    )
    .await;
    assert_rpc_error(
        &bare_apply,
        -32603,
        "tool execution failed: config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview",
    );
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    let missing_anchor = handle_real_server_tool_call_raw(
        &project.server,
        "tracedecay_insert_at",
        json!({
            "path": "src/refuse.rs",
            "content": "nope"
        }),
    )
    .await;
    assert_rpc_error(
        &missing_anchor,
        -32602,
        "missing required parameter: anchor",
    );

    let escaped_result = project
        .call(json!({
            "path": "../outside.txt",
            "anchor": "DO NOT",
            "content": "leaked\n",
            "dry_run": true
        }))
        .await;
    assert_eq!(escaped_result["isError"], true);
    let escaped = body(&escaped_result);
    assert_eq!(escaped["success"], false);
    assert_eq!(escaped["failed"], true);
    assert_eq!(escaped["replayed"], false);
    assert_eq!(
        escaped["message"],
        "source edit failed before the effect: config error: path is not within the project"
    );
    assert_eq!(escaped["effect"]["receipt"]["outcome"], "failed");
    assert!(escaped["effect"]["receipt"]["committed_state"].is_null());
    assert_eq!(fs::read_to_string(outside).unwrap(), "DO NOT TOUCH\n");
    assert_eq!(project.read("src/refuse.rs"), REFUSAL_ORIGINAL);

    project.close().await;
}
