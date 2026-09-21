use crate::support::{
    ProductionSourceEditFixture, TestTempDir, expect_tool_error, extract_first_json_content,
    init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use tracedecay_mcp::ToolResult;

const STALE_STATE: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const PATTERN: &str = "reserve_stock($SKU, $QTY)";
const REWRITE: &str = "reserve_stock($QTY, $SKU)";
const CHECKOUT: &str = "src/checkout.rs";
const OTHER: &str = "src/other.rs";
const OTHER_SOURCE: &str = "fn other() {}\n";

const CHECKOUT_BEFORE: &str = "\
fn caller() {
    reserve_stock(sku, 0);
    reserve_stock(
        sku,
        1,
    );
}
fn reserve_stock(sku: &str, qty: i32) {}
// reserve_stock(sku, 0) stays in the comment
let label = \"reserve_stock(sku, 0)\";
";

const CHECKOUT_AFTER: &str = "\
fn caller() {
    reserve_stock(0, sku);
    reserve_stock(1, sku);
}
fn reserve_stock(sku: &str, qty: i32) {}
// reserve_stock(sku, 0) stays in the comment
let label = \"reserve_stock(sku, 0)\";
";

const CHECKOUT_DIFF: &str = "\
@@ -1,9 +1,6 @@
 fn caller() {
-    reserve_stock(sku, 0);
-    reserve_stock(
-        sku,
-        1,
-    );
+    reserve_stock(0, sku);
+    reserve_stock(1, sku);
 }
 fn reserve_stock(sku: &str, qty: i32) {}
 // reserve_stock(sku, 0) stays in the comment";

const NO_MATCH_MESSAGE: &str = "ast-grep failed (exit 1). stdout: []";
const OPERATION: &str = "use-case.application.source-edit.ast-grep-rewrite";

fn require_ast_grep() {
    assert!(
        tracedecay_mcp::ast_grep_available(),
        "structural rewrite proof requires the host ast-grep CLI"
    );
}

async fn open_project(files: &[(&str, &str)]) -> (ProductionSourceEditFixture, TestTempDir) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    for (relative, contents) in files {
        let path = project.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has a parent")).unwrap();
        fs::write(&path, contents).unwrap();
    }
    let fixture = init_production_source_edit_project(&project).await;
    (fixture, dir)
}

async fn call_rewrite(
    fixture: &ProductionSourceEditFixture,
    args: Value,
) -> tracedecay_domain::errors::Result<ToolResult> {
    let mut args = args;
    if let Some(object) = args.as_object_mut() {
        object
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("registered source-edit server")
        .call_tool_for_test("tracedecay_ast_grep_rewrite", args)
        .await
}

fn edit_json(result: &ToolResult) -> Value {
    extract_first_json_content(&result.value)
}

fn assert_file(fixture: &ProductionSourceEditFixture, relative: &str, contents: &str) {
    let actual = fs::read(fixture.project_root.join(relative)).unwrap();
    assert_eq!(
        actual,
        contents.as_bytes(),
        "bytes of {relative} after tracedecay_ast_grep_rewrite"
    );
}

fn assert_effect(parsed: &Value, outcome: &str, payload_success: bool, payload_message: &str) {
    assert_eq!(
        parsed["effect"]["effect_class"],
        json!("source_edit"),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["receipt"]["outcome"],
        json!(outcome),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["operation"],
        json!(OPERATION),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["files"],
        json!([CHECKOUT]),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["success"],
        json!(payload_success),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["durable_metadata_only"],
        json!(true),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["message"],
        json!(payload_message),
        "{parsed}"
    );
}

#[tokio::test]
async fn ast_grep_rewrite_dry_run_then_apply_swaps_every_call_and_leaves_the_rest() {
    require_ast_grep();
    let (fixture, _dir) = open_project(&[(CHECKOUT, CHECKOUT_BEFORE), (OTHER, OTHER_SOURCE)]).await;

    let preview = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "dry_run": true
        }),
    )
    .await
    .expect("dry run");
    assert_eq!(preview.semantic_error(), Some(false), "{preview:?}");
    assert!(preview.touched_files.is_empty(), "{preview:?}");
    let preview_json = edit_json(&preview);
    assert_eq!(preview_json["success"], json!(true), "{preview_json}");
    assert_eq!(preview_json["file_path"], json!(CHECKOUT), "{preview_json}");
    assert_eq!(preview_json["pattern"], json!(PATTERN), "{preview_json}");
    assert_eq!(preview_json["rewrite"], json!(REWRITE), "{preview_json}");
    assert_eq!(preview_json["dry_run"], json!(true), "{preview_json}");
    assert_eq!(preview_json["diff"], json!(CHECKOUT_DIFF), "{preview_json}");
    assert_eq!(
        preview_json["message"],
        json!("dry run. Nothing written; preview only (ast-grep rewrite completed)"),
        "{preview_json}"
    );
    assert_eq!(preview_json["replayed"], json!(false), "{preview_json}");
    assert_effect(
        &preview_json,
        "completed",
        true,
        "source edit completed; detailed edit output was not retained",
    );
    let expected_state = preview_json["expected_state"]
        .as_str()
        .expect("preview expected_state")
        .to_owned();
    let predicted_state = preview_json["predicted_state"]
        .as_str()
        .expect("preview predicted_state")
        .to_owned();
    assert_ne!(
        expected_state, predicted_state,
        "a real rewrite must change the candidate digest"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_BEFORE);
    assert_file(&fixture, OTHER, OTHER_SOURCE);

    let applied = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "idempotency_key": "ast-grep-rewrite.behavior.apply",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("apply");
    assert_eq!(applied.semantic_error(), Some(false), "{applied:?}");
    assert_eq!(
        applied.touched_files,
        vec![CHECKOUT.to_owned()],
        "{applied:?}"
    );
    let applied_json = edit_json(&applied);
    assert_eq!(applied_json["success"], json!(true), "{applied_json}");
    assert_eq!(applied_json["file_path"], json!(CHECKOUT), "{applied_json}");
    assert_eq!(applied_json["pattern"], json!(PATTERN), "{applied_json}");
    assert_eq!(applied_json["rewrite"], json!(REWRITE), "{applied_json}");
    assert!(applied_json.get("dry_run").is_none(), "{applied_json}");
    assert!(applied_json.get("diff").is_none(), "{applied_json}");
    assert_eq!(
        applied_json["message"],
        json!("ast-grep rewrite completed"),
        "{applied_json}"
    );
    assert_eq!(applied_json["replayed"], json!(false), "{applied_json}");
    assert_eq!(
        applied_json["expected_state"],
        json!(expected_state),
        "{applied_json}"
    );
    assert_eq!(
        applied_json["predicted_state"],
        json!(predicted_state),
        "{applied_json}"
    );
    assert_eq!(
        applied_json["effect"]["receipt"]["expected_state"],
        json!(expected_state),
        "{applied_json}"
    );
    assert_eq!(
        applied_json["effect"]["receipt"]["committed_state"],
        json!(predicted_state),
        "{applied_json}"
    );
    assert_effect(
        &applied_json,
        "completed",
        true,
        "source edit completed; detailed edit output was not retained",
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_AFTER);
    assert_file(&fixture, OTHER, OTHER_SOURCE);
}

#[tokio::test]
async fn ast_grep_rewrite_exact_retry_replays_and_a_different_input_conflicts() {
    require_ast_grep();
    let (fixture, _dir) = open_project(&[(CHECKOUT, CHECKOUT_BEFORE)]).await;
    let preview = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "dry_run": true
        }),
    )
    .await
    .expect("dry run");
    let preview_json = edit_json(&preview);
    let expected_state = preview_json["expected_state"]
        .as_str()
        .expect("preview expected_state")
        .to_owned();
    let first = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "idempotency_key": "ast-grep-rewrite.behavior.replay",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("apply");
    assert_eq!(first.semantic_error(), Some(false), "{first:?}");
    assert_eq!(first.touched_files, vec![CHECKOUT.to_owned()], "{first:?}");
    let first_json = edit_json(&first);
    assert_eq!(
        first_json["message"],
        json!("ast-grep rewrite completed"),
        "{first_json}"
    );
    assert_eq!(first_json["replayed"], json!(false), "{first_json}");
    assert_eq!(first_json["pattern"], json!(PATTERN), "{first_json}");
    let effect_id = first_json["effect"]["effect_id"].clone();
    let bound_state = first_json["expected_state"]
        .as_str()
        .expect("apply expected_state")
        .to_owned();
    let predicted_state = first_json["predicted_state"]
        .as_str()
        .expect("apply predicted_state")
        .to_owned();
    assert_file(&fixture, CHECKOUT, CHECKOUT_AFTER);

    let retry = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "idempotency_key": "ast-grep-rewrite.behavior.replay",
            "expected_state": bound_state
        }),
    )
    .await
    .expect("exact retry");
    assert_eq!(retry.semantic_error(), Some(false), "{retry:?}");
    assert!(retry.touched_files.is_empty(), "{retry:?}");
    let retry_json = edit_json(&retry);
    assert_eq!(retry_json["success"], json!(true), "{retry_json}");
    assert_eq!(retry_json["failed"], json!(false), "{retry_json}");
    assert_eq!(retry_json["replayed"], json!(true), "{retry_json}");
    assert!(
        retry_json.get("pattern").is_none(),
        "a replay does not restate the live rewrite body: {retry_json}"
    );
    assert_eq!(
        retry_json["message"],
        json!("source edit completed; detailed edit output was not retained"),
        "{retry_json}"
    );
    assert_eq!(
        retry_json["expected_state"],
        json!(bound_state),
        "{retry_json}"
    );
    assert_eq!(
        retry_json["predicted_state"],
        json!(predicted_state),
        "{retry_json}"
    );
    assert_eq!(retry_json["effect"]["effect_id"], effect_id, "{retry_json}");
    assert_eq!(
        retry_json["effect"]["payload"]["operation"],
        json!(OPERATION),
        "{retry_json}"
    );
    assert_eq!(
        retry_json["effect"]["payload"]["files"],
        json!([CHECKOUT]),
        "{retry_json}"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_AFTER);

    let conflict = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": "reserve_stock($SKU, $QTY)",
            "idempotency_key": "ast-grep-rewrite.behavior.replay",
            "expected_state": bound_state
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(conflict),
        "project route error (source_edit.idempotency_conflict): source edit idempotency key conflicts with a prior input"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_AFTER);
}

#[tokio::test]
async fn ast_grep_rewrite_refuses_unmatched_patterns_paths_and_stale_previews() {
    require_ast_grep();
    let (fixture, dir) = open_project(&[
        (CHECKOUT, CHECKOUT_BEFORE),
        ("src/notes.txt", "reserve_stock(sku, 0);\n"),
    ])
    .await;

    let unmatched = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": "missing_call($X)",
            "rewrite": "gone($X)",
            "dry_run": true
        }),
    )
    .await
    .expect("unmatched pattern");
    assert_eq!(unmatched.semantic_error(), Some(true), "{unmatched:?}");
    assert_eq!(
        unmatched.failure_message(),
        Some(NO_MATCH_MESSAGE),
        "{unmatched:?}"
    );
    let unmatched_json = edit_json(&unmatched);
    assert_eq!(unmatched_json["success"], json!(false), "{unmatched_json}");
    assert_eq!(
        unmatched_json["file_path"],
        json!(CHECKOUT),
        "{unmatched_json}"
    );
    assert_eq!(
        unmatched_json["pattern"],
        json!("missing_call($X)"),
        "{unmatched_json}"
    );
    assert_eq!(
        unmatched_json["rewrite"],
        json!("gone($X)"),
        "{unmatched_json}"
    );
    assert_eq!(unmatched_json["dry_run"], json!(true), "{unmatched_json}");
    assert!(unmatched_json.get("diff").is_none(), "{unmatched_json}");
    assert_eq!(
        unmatched_json["message"],
        json!(NO_MATCH_MESSAGE),
        "{unmatched_json}"
    );
    assert_eq!(unmatched_json["replayed"], json!(false), "{unmatched_json}");
    assert_effect(
        &unmatched_json,
        "failed",
        false,
        "source edit completed; detailed edit output was not retained",
    );
    assert_eq!(
        unmatched_json["effect"]["payload"]["failed"],
        json!(false),
        "{unmatched_json}"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_BEFORE);

    let prose = call_rewrite(
        &fixture,
        json!({
            "path": "src/notes.txt",
            "pattern": "reserve_stock(sku, 0)",
            "rewrite": "ship_stock(sku, 0)",
            "dry_run": true
        }),
    )
    .await
    .expect("extension without a parser");
    assert_eq!(prose.semantic_error(), Some(true));
    let prose_json = edit_json(&prose);
    assert_eq!(prose_json["success"], json!(false), "{prose_json}");
    assert_eq!(
        prose_json["file_path"],
        json!("src/notes.txt"),
        "{prose_json}"
    );
    assert_eq!(
        prose_json["pattern"],
        json!("reserve_stock(sku, 0)"),
        "{prose_json}"
    );
    assert_eq!(
        prose_json["rewrite"],
        json!("ship_stock(sku, 0)"),
        "{prose_json}"
    );
    assert_eq!(
        prose_json["message"],
        json!(NO_MATCH_MESSAGE),
        "{prose_json}"
    );
    assert_file(&fixture, "src/notes.txt", "reserve_stock(sku, 0);\n");
    assert_file(&fixture, CHECKOUT, CHECKOUT_BEFORE);

    let outside = dir.path().join("secret.rs");
    fs::write(&outside, "fn secret() {}\n").unwrap();
    let escaped = call_rewrite(
        &fixture,
        json!({
            "path": "../secret.rs",
            "pattern": "secret",
            "rewrite": "leaked",
            "dry_run": true
        }),
    )
    .await
    .expect("path outside the worktree is a tool result");
    assert_eq!(escaped.semantic_error(), Some(true), "{escaped:?}");
    let escaped_json = edit_json(&escaped);
    assert_eq!(escaped_json["success"], json!(false), "{escaped_json}");
    assert_eq!(escaped_json["failed"], json!(true), "{escaped_json}");
    assert_eq!(
        escaped_json["message"],
        json!("source edit failed before the effect: config error: path is not within the project"),
        "{escaped_json}"
    );
    assert_eq!(fs::read(&outside).unwrap(), b"fn secret() {}\n");

    let missing = call_rewrite(
        &fixture,
        json!({
            "path": "src/missing.rs",
            "pattern": "anything",
            "rewrite": "else",
            "dry_run": true
        }),
    )
    .await
    .expect("missing file is a tool result");
    assert_eq!(missing.semantic_error(), Some(true));
    let missing_json = edit_json(&missing);
    assert_eq!(missing_json["success"], json!(false), "{missing_json}");
    assert_eq!(missing_json["failed"], json!(true), "{missing_json}");
    assert_eq!(
        missing_json["message"],
        json!(
            "source edit failed before the effect: config error: failed to read src/missing.rs: file was not found"
        ),
        "{missing_json}"
    );

    let directory = call_rewrite(
        &fixture,
        json!({
            "path": "src",
            "pattern": "fn",
            "rewrite": "pub fn",
            "dry_run": true
        }),
    )
    .await
    .expect("directory is a tool result");
    assert_eq!(directory.semantic_error(), Some(true));
    let directory_json = edit_json(&directory);
    assert_eq!(directory_json["success"], json!(false), "{directory_json}");
    assert_eq!(directory_json["failed"], json!(true), "{directory_json}");
    assert_eq!(
        directory_json["message"],
        json!(
            "source edit failed before the effect: config error: source edit path is not a regular file beneath the authorized worktree"
        ),
        "{directory_json}"
    );

    let missing_arg = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(missing_arg),
        "config error: missing required parameter: rewrite"
    );

    let unpreviewed = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(unpreviewed),
        "config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_BEFORE);

    let stale = call_rewrite(
        &fixture,
        json!({
            "path": CHECKOUT,
            "pattern": PATTERN,
            "rewrite": REWRITE,
            "idempotency_key": "ast-grep-rewrite.behavior.stale",
            "expected_state": STALE_STATE
        }),
    )
    .await
    .expect("stale apply returns a failed edit");
    assert_eq!(stale.semantic_error(), Some(true), "{stale:?}");
    let stale_json = edit_json(&stale);
    assert_eq!(stale_json["success"], json!(false), "{stale_json}");
    assert_eq!(stale_json["failed"], json!(true), "{stale_json}");
    assert_eq!(
        stale_json["message"],
        json!("source edit failed before the effect"),
        "{stale_json}"
    );
    assert_eq!(stale_json["replayed"], json!(false), "{stale_json}");
    assert_eq!(
        stale_json["effect"]["receipt"]["outcome"],
        json!("failed"),
        "{stale_json}"
    );
    assert_eq!(
        stale_json["effect"]["receipt"]["expected_state"],
        json!(STALE_STATE),
        "{stale_json}"
    );
    assert!(
        stale_json["effect"]["receipt"]["committed_state"].is_null(),
        "{stale_json}"
    );
    assert_file(&fixture, CHECKOUT, CHECKOUT_BEFORE);
}
