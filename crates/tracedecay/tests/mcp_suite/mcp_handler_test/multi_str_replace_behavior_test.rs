//! Observable behavior of `tracedecay_multi_str_replace` through the production
//! MCP source-edit server. Each case sends the tool the arguments a caller
//! sends and checks the text the caller reads plus the bytes left on disk.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, close_production_source_edit_fixture,
    expect_tool_error, extract_first_json_content, handle_production_source_edit_tool_call,
    init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const STALE_STATE: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

async fn open_fixture(files: &[(&str, &str)]) -> (TestTempDir, ProductionSourceEditFixture) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    for (name, contents) in files {
        let path = project.join(name);
        fs::create_dir_all(path.parent().expect("fixture file has a parent")).unwrap();
        fs::write(&path, contents).unwrap();
    }
    let (fixture, ()) = init_production_source_edit_project(&project).await;
    (dir, fixture)
}

fn project_file(dir: &TestTempDir, relative: &str) -> PathBuf {
    dir.path().join("project").join(relative)
}

fn read_file(dir: &TestTempDir, relative: &str) -> String {
    fs::read_to_string(project_file(dir, relative)).unwrap()
}

async fn call_tool(fixture: &ProductionSourceEditFixture, args: Value) -> Value {
    let result = handle_production_source_edit_tool_call(
        fixture,
        "tracedecay_multi_str_replace",
        args,
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("tracedecay_multi_str_replace returned {error}"));
    extract_first_json_content(&result.value)
}

async fn refuse_tool(fixture: &ProductionSourceEditFixture, args: Value) -> String {
    expect_tool_error(
        handle_production_source_edit_tool_call(
            fixture,
            "tracedecay_multi_str_replace",
            args,
            None,
            None,
        )
        .await,
    )
}

#[tokio::test]
async fn preview_apply_and_replay_replace_each_original_span() {
    let original = "old-a\nold-b\n";
    let applied = "new-a\nnew-b\n";
    let (dir, fixture) = open_fixture(&[("src/main.rs", original)]).await;

    let preview = call_tool(
        &fixture,
        json!({
            "path": "src/main.rs",
            "replacements": [["old-a", "new-a"], ["old-b", "new-b"]],
            "dry_run": true
        }),
    )
    .await;

    assert_eq!(preview["success"], true, "{preview}");
    assert_eq!(preview["dry_run"], true, "{preview}");
    assert_eq!(preview["replayed"], false, "{preview}");
    assert_eq!(preview["file_path"], "src/main.rs", "{preview}");
    assert_eq!(preview["applied_count"], 2, "{preview}");
    assert_eq!(
        preview["message"], "dry run. Nothing written; preview only (applied 2 replacements)",
        "{preview}"
    );
    assert_eq!(
        preview["diff"], "@@ -1,2 +1,2 @@\n-old-a\n-old-b\n+new-a\n+new-b",
        "{preview}"
    );
    assert_eq!(
        preview["effect"]["effect_class"], "source_edit",
        "{preview}"
    );
    assert_eq!(
        preview["effect"]["payload"]["operation"],
        "use-case.application.source-edit.multi-str-replace",
        "{preview}"
    );
    assert_eq!(preview["effect"]["payload"]["change_count"], 2, "{preview}");
    assert_eq!(
        preview["effect"]["payload"]["files"],
        json!(["src/main.rs"]),
        "{preview}"
    );
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state")
        .to_owned();
    let predicted_state = preview["predicted_state"]
        .as_str()
        .expect("preview returns predicted_state")
        .to_owned();
    assert_ne!(expected_state, predicted_state, "{preview}");
    assert_eq!(read_file(&dir, "src/main.rs"), original);

    let apply_args = json!({
        "path": "src/main.rs",
        "replacements": [["old-a", "new-a"], ["old-b", "new-b"]],
        "idempotency_key": "mcp-behavior.multi-str.apply",
        "expected_state": expected_state
    });
    let applied_result = call_tool(&fixture, apply_args.clone()).await;
    assert_eq!(applied_result["success"], true, "{applied_result}");
    assert_eq!(applied_result["replayed"], false, "{applied_result}");
    assert_eq!(applied_result["applied_count"], 2, "{applied_result}");
    assert_eq!(
        applied_result["file_path"], "src/main.rs",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["message"], "applied 2 replacements",
        "{applied_result}"
    );
    assert!(
        applied_result.get("dry_run").is_none(),
        "a committed edit must not be marked as a preview: {applied_result}"
    );
    assert!(
        applied_result.get("diff").is_none(),
        "a committed edit must not return a preview diff: {applied_result}"
    );
    assert_eq!(
        applied_result["expected_state"], expected_state,
        "{applied_result}"
    );
    assert_eq!(
        applied_result["predicted_state"], predicted_state,
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["idempotency_key"], "mcp-behavior.multi-str.apply",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["effect_class"], "source_edit",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["receipt"]["outcome"], "completed",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["receipt"]["expected_state"], expected_state,
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["success"], true,
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["operation"],
        "use-case.application.source-edit.multi-str-replace",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["change_count"], 2,
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["files"],
        json!(["src/main.rs"]),
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["message"],
        "source edit completed; detailed edit output was not retained",
        "{applied_result}"
    );
    assert_eq!(
        applied_result["effect"]["payload"]["durable_metadata_only"], true,
        "{applied_result}"
    );
    assert_eq!(read_file(&dir, "src/main.rs"), applied);

    let replay = call_tool(&fixture, apply_args).await;
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(
        replay["message"], "source edit completed; detailed edit output was not retained",
        "{replay}"
    );
    assert_eq!(replay["change_count"], 2, "{replay}");
    assert_eq!(replay["files"], json!(["src/main.rs"]), "{replay}");
    assert_eq!(
        replay["operation"], "use-case.application.source-edit.multi-str-replace",
        "{replay}"
    );
    assert_eq!(replay["durable_metadata_only"], true, "{replay}");
    assert_eq!(
        replay["effect"]["effect_id"], applied_result["effect"]["effect_id"],
        "{replay}"
    );
    assert_eq!(
        replay["effect"]["receipt"], applied_result["effect"]["receipt"],
        "{replay}"
    );
    assert_eq!(read_file(&dir, "src/main.rs"), applied);

    let conflict = refuse_tool(
        &fixture,
        json!({
            "path": "src/main.rs",
            "replacements": [["old-a", "other-a"], ["old-b", "other-b"]],
            "idempotency_key": "mcp-behavior.multi-str.apply",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(
        conflict,
        "project route error (source_edit.idempotency_conflict): source edit idempotency key conflicts with a prior input"
    );
    assert_eq!(read_file(&dir, "src/main.rs"), applied);

    close_production_source_edit_fixture(fixture).await;
}

#[tokio::test]
async fn later_replacement_edits_the_original_span_not_inserted_text() {
    let original = "fn keep() {}\nfn target() {}\n";
    let (dir, fixture) = open_fixture(&[("src/main.rs", original)]).await;

    let preview = call_tool(
        &fixture,
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn keep() {}", "fn keep() {}\nfn target() {}"],
                ["fn target() {}", "fn target_renamed() {}"]
            ],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["success"], true, "{preview}");
    assert_eq!(preview["applied_count"], 2, "{preview}");
    assert_eq!(
        preview["diff"],
        "@@ -1,2 +1,3 @@\n fn keep() {}\n-fn target() {}\n+fn target() {}\n+fn target_renamed() {}",
        "{preview}"
    );
    assert_eq!(read_file(&dir, "src/main.rs"), original);
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview returns expected_state");

    let applied = call_tool(
        &fixture,
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn keep() {}", "fn keep() {}\nfn target() {}"],
                ["fn target() {}", "fn target_renamed() {}"]
            ],
            "idempotency_key": "mcp-behavior.multi-str.insertion",
            "expected_state": expected_state
        }),
    )
    .await;
    assert_eq!(applied["success"], true, "{applied}");
    assert_eq!(applied["applied_count"], 2, "{applied}");
    assert_eq!(applied["message"], "applied 2 replacements", "{applied}");
    assert_eq!(
        read_file(&dir, "src/main.rs"),
        "fn keep() {}\nfn target() {}\nfn target_renamed() {}\n"
    );

    close_production_source_edit_fixture(fixture).await;
}

#[tokio::test]
async fn refused_batches_leave_every_file_byte_unchanged() {
    let miss = "keep this\nchange me\n";
    let duplicated = "once\nonce\n";
    let overlap = "abcdef\n";
    let untouched = "leave me\n";
    let (dir, fixture) = open_fixture(&[
        ("src/miss.rs", miss),
        ("src/duplicated.rs", duplicated),
        ("src/overlap.rs", overlap),
        ("src/untouched.rs", untouched),
    ])
    .await;
    let outside = dir.path().join("outside.txt");
    fs::write(&outside, "secret\n").unwrap();

    let missed = call_tool(
        &fixture,
        json!({
            "path": "src/miss.rs",
            "replacements": [["change me", "changed"], ["missing", "gone"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(missed["success"], false, "{missed}");
    assert_eq!(missed["applied_count"], 0, "{missed}");
    assert_eq!(missed["dry_run"], true, "{missed}");
    assert_eq!(
        missed["message"], "replacement 'missing' matches 0 times, must match exactly once",
        "{missed}"
    );
    assert!(missed.get("diff").is_none(), "{missed}");
    assert_eq!(read_file(&dir, "src/miss.rs"), miss);

    let duplicate = call_tool(
        &fixture,
        json!({
            "path": "src/duplicated.rs",
            "replacements": [["once", "twice"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(duplicate["success"], false, "{duplicate}");
    assert_eq!(duplicate["applied_count"], 0, "{duplicate}");
    assert_eq!(
        duplicate["message"], "replacement 'once' matches 2 times, must match exactly once",
        "{duplicate}"
    );
    assert_eq!(read_file(&dir, "src/duplicated.rs"), duplicated);

    let overlapped = call_tool(
        &fixture,
        json!({
            "path": "src/overlap.rs",
            "replacements": [["cdef", "QRST"], ["abcd", "WXYZ"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(overlapped["success"], false, "{overlapped}");
    assert_eq!(overlapped["applied_count"], 0, "{overlapped}");
    assert_eq!(
        overlapped["message"],
        "replacements 'abcd' and 'cdef' target overlapping ranges; apply them separately",
        "{overlapped}"
    );
    assert_eq!(read_file(&dir, "src/overlap.rs"), overlap);

    let outside_result = call_tool(
        &fixture,
        json!({
            "path": "../outside.txt",
            "replacements": [["secret", "leaked"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(outside_result["success"], false, "{outside_result}");
    assert_eq!(outside_result["failed"], true, "{outside_result}");
    assert_eq!(
        outside_result["message"],
        "source edit failed before the effect: config error: path is not within the project",
        "{outside_result}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "secret\n");
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    let malformed = refuse_tool(
        &fixture,
        json!({
            "path": "src/untouched.rs",
            "replacements": [["old", "new", "extra"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        malformed,
        "config error: each replacement must be an array of exactly 2 strings"
    );
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    let missing_path = refuse_tool(
        &fixture,
        json!({
            "replacements": [["leave me", "changed"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(
        missing_path,
        "config error: missing required parameter: path"
    );
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    let server = fixture
        .harness
        .server(dir.path().join("project"))
        .expect("mounted source-edit server");
    let missing_apply_keys = expect_tool_error(
        server
            .call_tool_for_test(
                "tracedecay_multi_str_replace",
                json!({
                    "path": "src/untouched.rs",
                    "replacements": [["leave me", "changed"]]
                }),
            )
            .await,
    );
    assert_eq!(
        missing_apply_keys,
        "config error: source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
    );
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    let stale = call_tool(
        &fixture,
        json!({
            "path": "src/untouched.rs",
            "replacements": [["leave me", "changed"]],
            "idempotency_key": "mcp-behavior.multi-str.stale",
            "expected_state": STALE_STATE
        }),
    )
    .await;
    assert_eq!(stale["success"], false, "{stale}");
    assert_eq!(stale["failed"], true, "{stale}");
    assert_eq!(stale["replayed"], false, "{stale}");
    assert_eq!(
        stale["message"], "source edit failed before the effect",
        "{stale}"
    );
    assert_eq!(stale["expected_state"], STALE_STATE, "{stale}");
    assert_eq!(stale["effect"]["receipt"]["outcome"], "failed", "{stale}");
    assert_eq!(
        stale["effect"]["receipt"]["expected_state"], STALE_STATE,
        "{stale}"
    );
    assert!(
        stale["effect"]["receipt"]["committed_state"].is_null(),
        "{stale}"
    );
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    let prefix = "a".repeat(19);
    let unicode_miss = call_tool(
        &fixture,
        json!({
            "path": "src/untouched.rs",
            "replacements": [[format!("{prefix}é"), "replacement"]],
            "dry_run": true
        }),
    )
    .await;
    assert_eq!(unicode_miss["success"], false, "{unicode_miss}");
    assert_eq!(
        unicode_miss["message"],
        "replacement 'aaaaaaaaaaaaaaaaaaa' matches 0 times, must match exactly once",
        "{unicode_miss}"
    );
    assert_eq!(read_file(&dir, "src/untouched.rs"), untouched);

    close_production_source_edit_fixture(fixture).await;
}
