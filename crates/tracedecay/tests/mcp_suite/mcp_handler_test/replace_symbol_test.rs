//! Observable `tracedecay_replace_symbol` behavior through the production MCP
//! dispatch: the bytes left on disk and the tool payload a caller reads.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir,
    close_production_source_edit_fixture as close_test_graph,
    handle_production_source_edit_tool_call as handle_tool_call,
    init_production_source_edit_project as init_test_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const NEIGHBORS: &str = "\
fn keep_before() {
    let _ = 1;
}

fn target() {
    let _ = 1;
}

fn keep_after() {
    let _ = 3;
}
";

const NEIGHBORS_APPLIED: &str = "\
fn keep_before() {
    let _ = 1;
}

fn target() {
    let _ = 9;
}

fn keep_after() {
    let _ = 3;
}
";

const TARGET_NEW_SOURCE: &str = "fn target() {\n    let _ = 9;\n}";

const TARGET_OLD_SPAN: &str = "fn target() {\n    let _ = 1;\n}";

const TARGET_PREVIEW_DIFF: &str = "\
@@ -3,7 +3,7 @@
 }
 
 fn target() {
-    let _ = 1;
+    let _ = 9;
 }
 
 fn keep_after() {";

const DOCUMENTED: &str = "\
pub const N: u32 = 0;
/// Counts widgets.
#[inline]
fn count() -> u32 {
    1
}
fn keep() -> u32 {
    0
}
";

const DOCUMENTED_APPLIED: &str = "\
pub const N: u32 = 0;
fn count() -> u32 {
    2
}
fn keep() -> u32 {
    0
}
";

const COUNT_NEW_SOURCE: &str = "fn count() -> u32 {\n    2\n}";

const COUNT_OLD_SPAN: &str = "\
/// Counts widgets.
#[inline]
fn count() -> u32 {
    1
}";

const CALLABLE_SHADOW: &str = "\
const target: i32 = 1;

fn other() {
    let _ = 0;
}

fn target() {
    let _ = 1;
}
";

const CALLABLE_SHADOW_APPLIED: &str = "\
const target: i32 = 1;

fn other() {
    let _ = 0;
}

fn target() {
    let _ = 9;
}
";

const LEFT_WIDGET: &str = "\
pub fn widget() {
    let _ = 1;
}
";

const LEFT_WIDGET_APPLIED: &str = "\
pub fn widget() {
    let _ = 9;
}
";

const RIGHT_WIDGET: &str = "\
pub fn widget() {
    let _ = 2;
}
";

const WIDGET_NEW_SOURCE: &str = "pub fn widget() {\n    let _ = 9;\n}";

fn tool_payload(value: &Value) -> Value {
    let text = value["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                text.find('{').map(|start| &text[start..])
            })
        })
        .unwrap_or_else(|| panic!("missing JSON content item in {value}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("replace_symbol payload was not JSON ({error}): {text}"))
}

async fn open_sources(
    files: &[(&str, &str)],
) -> (TestTempDir, PathBuf, ProductionSourceEditFixture) {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    for (relative, contents) in files {
        let path = project_root.join(relative);
        fs::create_dir_all(path.parent().expect("source file has a parent")).unwrap();
        fs::write(&path, contents).unwrap();
    }
    let fixture = init_test_project(&project_root).await;
    (dir, project_root, fixture)
}

async fn call_replace(fixture: &ProductionSourceEditFixture, args: Value) -> Value {
    let result = handle_tool_call(fixture, "tracedecay_replace_symbol", args, None, None)
        .await
        .expect("tracedecay_replace_symbol dispatch");
    tool_payload(&result.value)
}

fn read_project_file(project: &Path, relative: &str) -> String {
    fs::read_to_string(project.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative} after replace_symbol: {error}"))
}

#[tokio::test]
async fn replace_symbol_proves_apply_rewrites_only_the_named_function() {
    let (_dir, project, fixture) = open_sources(&[("src/main.rs", NEIGHBORS)]).await;

    let preview = call_replace(
        &fixture,
        json!({
            "symbol": "target",
            "new_source": TARGET_NEW_SOURCE,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["success"], true);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["file_path"], "src/main.rs");
    assert_eq!(preview["matched_str"], "target (function)");
    assert_eq!(preview["new_str"], TARGET_NEW_SOURCE);
    assert_eq!(preview["replaced_span"], TARGET_OLD_SPAN);
    assert_eq!(
        preview["message"],
        "dry run. Nothing written; preview only (replaced src/main.rs:5-7)"
    );
    assert_eq!(preview["diff"], TARGET_PREVIEW_DIFF);
    assert_eq!(preview["replayed"], false);
    assert_eq!(read_project_file(&project, "src/main.rs"), NEIGHBORS);

    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state");
    let apply = call_replace(
        &fixture,
        json!({
            "symbol": "target",
            "new_source": TARGET_NEW_SOURCE,
            "idempotency_key": "mcp-test.replace-symbol.apply-target",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(apply["success"], true);
    assert_eq!(apply["replayed"], false);
    assert_eq!(apply["file_path"], "src/main.rs");
    assert_eq!(apply["matched_str"], "target (function)");
    assert_eq!(apply["new_str"], TARGET_NEW_SOURCE);
    assert_eq!(apply["replaced_span"], TARGET_OLD_SPAN);
    assert_eq!(apply["message"], "replaced src/main.rs:5-7");
    assert_eq!(apply.get("dry_run"), None);
    assert_eq!(apply.get("diff"), None);
    assert_eq!(apply["effect"]["effect_class"], "source_edit");
    assert_eq!(
        apply["effect"]["idempotency_key"],
        "mcp-test.replace-symbol.apply-target"
    );
    assert_eq!(apply["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(apply["effect"]["payload"]["success"], true);
    assert_eq!(
        apply["effect"]["payload"]["operation"],
        "use-case.application.source-edit.replace-symbol"
    );
    assert_eq!(apply["effect"]["payload"]["files"], json!(["src/main.rs"]));
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        NEIGHBORS_APPLIED
    );

    let replay = call_replace(
        &fixture,
        json!({
            "symbol": "target",
            "new_source": TARGET_NEW_SOURCE,
            "idempotency_key": "mcp-test.replace-symbol.apply-target",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(replay["success"], true);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["effect"]["effect_id"], apply["effect"]["effect_id"]);
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        NEIGHBORS_APPLIED
    );

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn replace_symbol_proves_omitted_docs_and_attributes_are_removed() {
    let (_dir, project, fixture) = open_sources(&[("src/main.rs", DOCUMENTED)]).await;

    let preview = call_replace(
        &fixture,
        json!({
            "symbol": "count",
            "new_source": COUNT_NEW_SOURCE,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["success"], true);
    assert_eq!(preview["replaced_span"], COUNT_OLD_SPAN);
    assert_eq!(preview["matched_str"], "count (function)");
    assert_eq!(preview["file_path"], "src/main.rs");
    assert_eq!(
        preview["message"],
        "dry run. Nothing written; preview only (replaced src/main.rs:2-6)"
    );
    assert_eq!(read_project_file(&project, "src/main.rs"), DOCUMENTED);

    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state");
    let apply = call_replace(
        &fixture,
        json!({
            "symbol": "count",
            "new_source": COUNT_NEW_SOURCE,
            "idempotency_key": "mcp-test.replace-symbol.drop-docs",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(apply["success"], true);
    assert_eq!(apply["replaced_span"], COUNT_OLD_SPAN);
    assert_eq!(apply["message"], "replaced src/main.rs:2-6");
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        DOCUMENTED_APPLIED
    );

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn replace_symbol_proves_bare_name_prefers_the_callable() {
    let (_dir, project, fixture) = open_sources(&[("src/main.rs", CALLABLE_SHADOW)]).await;

    let preview = call_replace(
        &fixture,
        json!({
            "symbol": "target",
            "new_source": TARGET_NEW_SOURCE,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["success"], true);
    assert_eq!(preview["matched_str"], "target (function)");
    assert_eq!(preview["replaced_span"], TARGET_OLD_SPAN);
    assert_eq!(read_project_file(&project, "src/main.rs"), CALLABLE_SHADOW);

    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state");
    let apply = call_replace(
        &fixture,
        json!({
            "symbol": "target",
            "new_source": TARGET_NEW_SOURCE,
            "idempotency_key": "mcp-test.replace-symbol.callable-wins",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(apply["success"], true);
    assert_eq!(apply["matched_str"], "target (function)");
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        CALLABLE_SHADOW_APPLIED
    );

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn replace_symbol_proves_qualified_name_edits_only_that_file() {
    let (_dir, project, fixture) =
        open_sources(&[("src/left.rs", LEFT_WIDGET), ("src/right.rs", RIGHT_WIDGET)]).await;

    let ambiguous = call_replace(
        &fixture,
        json!({
            "symbol": "widget",
            "new_source": WIDGET_NEW_SOURCE,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(ambiguous["success"], false);
    assert_eq!(ambiguous["failed"], true);
    assert_eq!(
        ambiguous["message"],
        "source edit failed before the effect: config error: symbol 'widget' is ambiguous (2 matches); pass a fully qualified name"
    );
    assert_eq!(read_project_file(&project, "src/left.rs"), LEFT_WIDGET);
    assert_eq!(read_project_file(&project, "src/right.rs"), RIGHT_WIDGET);

    let preview = call_replace(
        &fixture,
        json!({
            "symbol": "src/left.rs::widget",
            "new_source": WIDGET_NEW_SOURCE,
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["success"], true, "{preview}");
    assert_eq!(preview["file_path"], "src/left.rs");
    assert_eq!(preview["matched_str"], "widget (function)");
    assert_eq!(
        preview["replaced_span"],
        "pub fn widget() {\n    let _ = 1;\n}"
    );
    assert_eq!(read_project_file(&project, "src/left.rs"), LEFT_WIDGET);
    assert_eq!(read_project_file(&project, "src/right.rs"), RIGHT_WIDGET);

    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state");
    let apply = call_replace(
        &fixture,
        json!({
            "symbol": "src/left.rs::widget",
            "new_source": WIDGET_NEW_SOURCE,
            "idempotency_key": "mcp-test.replace-symbol.qualified-left",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(apply["success"], true, "{apply}");
    assert_eq!(apply["file_path"], "src/left.rs");
    assert_eq!(apply["message"], "replaced src/left.rs:1-3");
    assert_eq!(
        read_project_file(&project, "src/left.rs"),
        LEFT_WIDGET_APPLIED
    );
    assert_eq!(read_project_file(&project, "src/right.rs"), RIGHT_WIDGET);

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn replace_symbol_proves_missing_symbol_leaves_the_file_unchanged() {
    let original = "fn keep() {\n    let _ = 1;\n}\n";
    let (_dir, project, fixture) = open_sources(&[("src/main.rs", original)]).await;

    let missing = call_replace(
        &fixture,
        json!({
            "symbol": "missing_symbol",
            "new_source": "fn missing_symbol() {}\n",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(missing["success"], false);
    assert_eq!(missing["failed"], true);
    assert_eq!(
        missing["message"],
        "source edit failed before the effect: config error: symbol 'missing_symbol' not found"
    );
    assert_eq!(read_project_file(&project, "src/main.rs"), original);

    close_test_graph(fixture).await;
}
