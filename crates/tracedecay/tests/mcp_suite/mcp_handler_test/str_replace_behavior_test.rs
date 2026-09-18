//! Observable behavior of `tracedecay_str_replace` through the production MCP
//! dispatch. These tests call the tool the way a host does and compare the
//! file bytes and the response fields the caller can read.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, expect_tool_error, extract_first_json_content,
    init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use tracedecay_mcp::ToolResult;

const STALE_STATE: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

const LIB_BEFORE: &str = "fn keep() {}\nfn old_name() {}\nfn tail() {}\n";
const LIB_AFTER: &str = "fn keep() {}\nfn new_name() {}\nfn tail() {}\n";
const LIB_DIFF: &str = "\
@@ -1,3 +1,3 @@
 fn keep() {}
-fn old_name() {}
+fn new_name() {}
 fn tail() {}";

async fn open_project(files: &[(&str, &str)]) -> (ProductionSourceEditFixture, TestTempDir) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    for (relative, contents) in files {
        let path = project.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has a parent")).unwrap();
        fs::write(&path, contents).unwrap();
    }
    let (fixture, _) = init_production_source_edit_project(&project).await;
    (fixture, dir)
}

async fn call_str_replace(
    fixture: &ProductionSourceEditFixture,
    args: Value,
) -> tracedecay_domain::errors::Result<ToolResult> {
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("registered source-edit server")
        .call_tool_for_test("tracedecay_str_replace", args)
        .await
}

fn edit_json(result: &ToolResult) -> Value {
    extract_first_json_content(&result.value)
}

fn project_file(fixture: &ProductionSourceEditFixture, relative: &str) -> std::path::PathBuf {
    fixture.project_root.join(relative)
}

fn assert_file(fixture: &ProductionSourceEditFixture, relative: &str, contents: &str) {
    let actual = fs::read(project_file(fixture, relative)).unwrap();
    assert_eq!(
        actual,
        contents.as_bytes(),
        "bytes of {relative} after tracedecay_str_replace"
    );
}

fn assert_refused(parsed: &Value, path: &str, old_str: &str, new_str: &str, message: &str) {
    assert_eq!(parsed["success"], json!(false), "{parsed}");
    assert_eq!(parsed["file_path"], json!(path), "{parsed}");
    assert_eq!(parsed["matched_str"], json!(old_str), "{parsed}");
    assert_eq!(parsed["new_str"], json!(new_str), "{parsed}");
    assert_eq!(parsed["message"], json!(message), "{parsed}");
    assert_eq!(parsed["replayed"], json!(false), "{parsed}");
    assert_eq!(parsed["dry_run"], json!(true), "{parsed}");
    assert!(parsed.get("replaced_span").is_none(), "{parsed}");
    assert!(parsed.get("diff").is_none(), "{parsed}");
}

fn assert_applied(parsed: &Value, path: &str, old_str: &str, new_str: &str) {
    assert_eq!(parsed["success"], json!(true), "{parsed}");
    assert_eq!(parsed["file_path"], json!(path), "{parsed}");
    assert_eq!(parsed["matched_str"], json!(old_str), "{parsed}");
    assert_eq!(parsed["new_str"], json!(new_str), "{parsed}");
    assert_eq!(parsed["replaced_span"], json!(old_str), "{parsed}");
    assert_eq!(
        parsed["message"],
        json!("replacement successful"),
        "{parsed}"
    );
    assert_eq!(parsed["replayed"], json!(false), "{parsed}");
    assert!(parsed.get("dry_run").is_none(), "{parsed}");
    assert!(parsed.get("diff").is_none(), "{parsed}");
    assert_eq!(
        parsed["effect"]["effect_class"],
        json!("source_edit"),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["receipt"]["outcome"],
        json!("completed"),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["success"],
        json!(true),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["operation"],
        json!("use-case.application.source-edit.str-replace"),
        "{parsed}"
    );
    assert_eq!(
        parsed["effect"]["payload"]["files"],
        json!([path]),
        "{parsed}"
    );
}

async fn replace_once(
    fixture: &ProductionSourceEditFixture,
    path: &str,
    old_str: &str,
    new_str: &str,
    idempotency_key: &str,
) -> Value {
    let preview = call_str_replace(
        fixture,
        json!({
            "path": path,
            "old_str": old_str,
            "new_str": new_str,
            "dry_run": true
        }),
    )
    .await
    .expect("dry run");
    assert_eq!(preview.semantic_error(), Some(false), "{preview:?}");
    let preview_json = edit_json(&preview);
    assert_eq!(preview_json["success"], json!(true), "{preview_json}");
    assert_eq!(preview_json["dry_run"], json!(true), "{preview_json}");
    assert_eq!(
        preview_json["message"],
        json!("dry run. Nothing written; preview only (replacement successful)"),
        "{preview_json}"
    );
    let expected_state = preview_json["expected_state"]
        .as_str()
        .expect("preview expected_state")
        .to_owned();
    let applied = call_str_replace(
        fixture,
        json!({
            "path": path,
            "old_str": old_str,
            "new_str": new_str,
            "idempotency_key": idempotency_key,
            "expected_state": expected_state
        }),
    )
    .await
    .expect("apply");
    assert_eq!(applied.semantic_error(), Some(false), "{applied:?}");
    edit_json(&applied)
}

#[tokio::test]
async fn str_replace_dry_run_then_apply_replaces_one_unique_span() {
    let (fixture, _dir) = open_project(&[
        ("src/lib.rs", LIB_BEFORE),
        ("src/other.rs", "fn other() {}\n"),
    ])
    .await;

    let preview = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "dry_run": true
        }),
    )
    .await
    .expect("dry run");
    assert_eq!(preview.semantic_error(), Some(false));
    let preview_json = edit_json(&preview);
    assert_eq!(preview_json["success"], json!(true), "{preview_json}");
    assert_eq!(
        preview_json["file_path"],
        json!("src/lib.rs"),
        "{preview_json}"
    );
    assert_eq!(
        preview_json["matched_str"],
        json!("old_name"),
        "{preview_json}"
    );
    assert_eq!(preview_json["new_str"], json!("new_name"), "{preview_json}");
    assert_eq!(
        preview_json["replaced_span"],
        json!("old_name"),
        "{preview_json}"
    );
    assert_eq!(preview_json["dry_run"], json!(true), "{preview_json}");
    assert_eq!(preview_json["diff"], json!(LIB_DIFF), "{preview_json}");
    assert_eq!(
        preview_json["message"],
        json!("dry run. Nothing written; preview only (replacement successful)"),
        "{preview_json}"
    );
    assert_eq!(preview_json["replayed"], json!(false), "{preview_json}");
    assert_file(&fixture, "src/lib.rs", LIB_BEFORE);

    let expected_state = preview_json["expected_state"]
        .as_str()
        .expect("preview expected_state")
        .to_owned();
    let applied = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "idempotency_key": "str-replace.behavior.apply",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("apply");
    assert_eq!(applied.semantic_error(), Some(false));
    assert_applied(&edit_json(&applied), "src/lib.rs", "old_name", "new_name");
    assert_file(&fixture, "src/lib.rs", LIB_AFTER);
    assert_file(&fixture, "src/other.rs", "fn other() {}\n");
}

#[tokio::test]
async fn str_replace_exact_retry_replays_and_a_different_input_conflicts() {
    let (fixture, _dir) = open_project(&[("src/lib.rs", LIB_BEFORE)]).await;
    let first = replace_once(
        &fixture,
        "src/lib.rs",
        "old_name",
        "new_name",
        "str-replace.behavior.replay",
    )
    .await;
    assert_applied(&first, "src/lib.rs", "old_name", "new_name");
    assert_file(&fixture, "src/lib.rs", LIB_AFTER);
    let expected_state = first["expected_state"]
        .as_str()
        .expect("apply expected_state")
        .to_owned();
    let effect_id = first["effect"]["effect_id"].clone();

    let retry = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "idempotency_key": "str-replace.behavior.replay",
            "expected_state": expected_state
        }),
    )
    .await
    .expect("exact retry");
    assert_eq!(retry.semantic_error(), Some(false), "{retry:?}");
    let retry_json = edit_json(&retry);
    assert_eq!(retry_json["success"], json!(true), "{retry_json}");
    assert_eq!(retry_json["replayed"], json!(true), "{retry_json}");
    assert_eq!(
        retry_json["message"],
        json!("source edit completed; detailed edit output was not retained"),
        "{retry_json}"
    );
    assert_eq!(retry_json["effect"]["effect_id"], effect_id, "{retry_json}");
    assert_file(&fixture, "src/lib.rs", LIB_AFTER);

    let conflict = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "other_name",
            "idempotency_key": "str-replace.behavior.replay",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(conflict),
        "project route error (source_edit.idempotency_conflict): source edit idempotency key conflicts with a prior input"
    );
    assert_file(&fixture, "src/lib.rs", LIB_AFTER);

    let spent = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "final_name",
            "dry_run": true
        }),
    )
    .await
    .expect("spent match");
    assert_eq!(spent.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&spent),
        "src/lib.rs",
        "old_name",
        "final_name",
        "old_str not found in src/lib.rs",
    );
    assert_file(&fixture, "src/lib.rs", LIB_AFTER);
}

#[tokio::test]
async fn str_replace_refuses_a_string_that_is_not_unique_and_keeps_exact_bytes() {
    let sample = "fn foo() {}\nfn foo() {}\nfn bar() {}\n";
    let (fixture, _dir) = open_project(&[
        ("src/sample.rs", sample),
        ("src/short.rs", "ab\n"),
        ("src/token.rs", "let old_name = 1;\n"),
        ("src/overlap.rs", "aaa\n"),
        ("src/crlf.rs", "fn old() {}\r\n// mark \u{2603}\n"),
        ("src/abs.rs", "value = 1;\n"),
        ("src/empty.rs", ""),
    ])
    .await;

    let missing = call_str_replace(
        &fixture,
        json!({
            "path": "src/sample.rs",
            "old_str": "fn missing() {}",
            "new_str": "fn present() {}",
            "dry_run": true
        }),
    )
    .await
    .expect("missing string");
    assert_eq!(missing.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&missing),
        "src/sample.rs",
        "fn missing() {}",
        "fn present() {}",
        "old_str not found in src/sample.rs",
    );

    let twice = call_str_replace(
        &fixture,
        json!({
            "path": "src/sample.rs",
            "old_str": "fn foo() {}",
            "new_str": "fn baz() {}",
            "dry_run": true
        }),
    )
    .await
    .expect("two matches");
    assert_eq!(twice.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&twice),
        "src/sample.rs",
        "fn foo() {}",
        "fn baz() {}",
        "old_str matches 2 times, must match exactly once",
    );

    let thrice = call_str_replace(
        &fixture,
        json!({
            "path": "src/sample.rs",
            "old_str": "fn ",
            "new_str": "pub fn ",
            "dry_run": true
        }),
    )
    .await
    .expect("three matches");
    assert_eq!(thrice.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&thrice),
        "src/sample.rs",
        "fn ",
        "pub fn ",
        "old_str matches 3 times, must match exactly once",
    );

    let spacing = call_str_replace(
        &fixture,
        json!({
            "path": "src/sample.rs",
            "old_str": "fn  foo() {}",
            "new_str": "fn foo() {}",
            "dry_run": true
        }),
    )
    .await
    .expect("whitespace mismatch");
    assert_eq!(spacing.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&spacing),
        "src/sample.rs",
        "fn  foo() {}",
        "fn foo() {}",
        "old_str not found in src/sample.rs",
    );
    assert_file(&fixture, "src/sample.rs", sample);

    let empty_needle = call_str_replace(
        &fixture,
        json!({
            "path": "src/short.rs",
            "old_str": "",
            "new_str": "x",
            "dry_run": true
        }),
    )
    .await
    .expect("empty needle");
    assert_eq!(empty_needle.semantic_error(), Some(true));
    assert_refused(
        &edit_json(&empty_needle),
        "src/short.rs",
        "",
        "x",
        "old_str matches 4 times, must match exactly once",
    );
    assert_file(&fixture, "src/short.rs", "ab\n");

    let applied = replace_once(
        &fixture,
        "src/token.rs",
        "name",
        "title",
        "str-replace.behavior.substring",
    )
    .await;
    assert_applied(&applied, "src/token.rs", "name", "title");
    assert_file(&fixture, "src/token.rs", "let old_title = 1;\n");

    let overlap = replace_once(
        &fixture,
        "src/overlap.rs",
        "aa",
        "b",
        "str-replace.behavior.overlap",
    )
    .await;
    assert_applied(&overlap, "src/overlap.rs", "aa", "b");
    assert_file(&fixture, "src/overlap.rs", "ba\n");

    let crlf = replace_once(
        &fixture,
        "src/crlf.rs",
        "old",
        "new",
        "str-replace.behavior.crlf",
    )
    .await;
    assert_applied(&crlf, "src/crlf.rs", "old", "new");
    assert_file(&fixture, "src/crlf.rs", "fn new() {}\r\n// mark \u{2603}\n");

    let absolute = project_file(&fixture, "src/abs.rs");
    let absolute = absolute.to_str().expect("utf-8 fixture path");
    let from_absolute = replace_once(
        &fixture,
        absolute,
        "value = 1;",
        "value = 2;",
        "str-replace.behavior.absolute",
    )
    .await;
    assert_applied(&from_absolute, "src/abs.rs", "value = 1;", "value = 2;");
    assert_file(&fixture, "src/abs.rs", "value = 2;\n");

    let empty = replace_once(
        &fixture,
        "src/empty.rs",
        "",
        "inserted",
        "str-replace.behavior.empty",
    )
    .await;
    assert_applied(&empty, "src/empty.rs", "", "inserted");
    assert_file(&fixture, "src/empty.rs", "inserted");
}

#[tokio::test]
async fn str_replace_refuses_paths_that_are_not_a_regular_project_file() {
    let (fixture, dir) = open_project(&[("src/lib.rs", LIB_BEFORE)]).await;
    let outside = dir.path().join("secret.rs");
    fs::write(&outside, "fn secret() {}\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, project_file(&fixture, "src/link.rs")).unwrap();
    }

    let parent = call_str_replace(
        &fixture,
        json!({
            "path": "../secret.rs",
            "old_str": "secret",
            "new_str": "leaked",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(parent),
        "project route error (source_edit.execution_failed): config error: path is not within the project"
    );

    let absolute_outside = call_str_replace(
        &fixture,
        json!({
            "path": outside.to_str().expect("utf-8 outside path"),
            "old_str": "secret",
            "new_str": "leaked",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(absolute_outside),
        "project route error (source_edit.execution_failed): config error: path is not within the project"
    );
    assert_eq!(fs::read(&outside).unwrap(), b"fn secret() {}\n");

    let missing = call_str_replace(
        &fixture,
        json!({
            "path": "src/missing.rs",
            "old_str": "anything",
            "new_str": "else",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(missing),
        "project route error (source_edit.execution_failed): config error: failed to read src/missing.rs: file was not found"
    );

    let directory = call_str_replace(
        &fixture,
        json!({
            "path": "src",
            "old_str": "lib",
            "new_str": "bin",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(directory),
        "project route error (source_edit.execution_failed): config error: source edit path is not a regular file beneath the authorized worktree"
    );

    #[cfg(unix)]
    {
        let link = call_str_replace(
            &fixture,
            json!({
                "path": "src/link.rs",
                "old_str": "secret",
                "new_str": "leaked",
                "dry_run": true
            }),
        )
        .await;
        assert_eq!(
            expect_tool_error(link),
            "project route error (source_edit.execution_failed): config error: source edit path is not a regular file beneath the authorized worktree"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"fn secret() {}\n");
    }

    assert_file(&fixture, "src/lib.rs", LIB_BEFORE);
}

#[tokio::test]
async fn str_replace_apply_requires_the_preview_digest_and_rejects_a_stale_one() {
    let (fixture, _dir) = open_project(&[("src/lib.rs", LIB_BEFORE)]).await;

    let missing_arg = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(missing_arg),
        "config error: missing required parameter: new_str"
    );

    let unpreviewed = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "new_name"
        }),
    )
    .await;
    assert_eq!(
        expect_tool_error(unpreviewed),
        "config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
    );
    assert_file(&fixture, "src/lib.rs", LIB_BEFORE);

    let stale = call_str_replace(
        &fixture,
        json!({
            "path": "src/lib.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "idempotency_key": "str-replace.behavior.stale",
            "expected_state": STALE_STATE
        }),
    )
    .await
    .expect("stale apply returns a failed edit");
    assert_eq!(stale.semantic_error(), Some(true));
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
    assert_file(&fixture, "src/lib.rs", LIB_BEFORE);
}
