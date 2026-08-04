use crate::support::*;
use serde_json::{Value, json};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;

fn extract_edit_json(value: &Value) -> Value {
    value["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                serde_json::from_str(text).ok()
            })
        })
        .unwrap_or_else(|| panic!("missing JSON content item in {value}"))
}

// ---------------------------------------------------------------------------
// Edit tools: tracedecay_str_replace, tracedecay_multi_str_replace, tracedecay_insert_at
// ---------------------------------------------------------------------------

#[tokio::test]
async fn source_edit_preview_apply_and_retry_use_daemon_owned_cas_authority() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let initial = b"fn old_name() {}\r\n// exact \xE2\x98\x83\n";
    let applied = b"fn new_name() {}\r\n// exact \xE2\x98\x83\n";
    fs::write(project.join("src/main.rs"), initial).unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let preview = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "dry_run": true
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preview: Value = serde_json::from_str(extract_text(&preview.value)).unwrap();
    let expected_state = preview["expected_state"].as_str().unwrap();
    assert_eq!(fs::read(project.join("src/main.rs")).unwrap(), initial);

    let apply_args = json!({
        "path": "src/main.rs",
        "old_str": "old_name",
        "new_str": "new_name",
        "idempotency_key": "mcp-test.source-edit.exact-retry",
        "expected_state": expected_state
    });
    let first = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        apply_args.clone(),
        None,
        None,
    )
    .await
    .unwrap();
    let first: Value = serde_json::from_str(extract_text(&first.value)).unwrap();
    assert_eq!(first["success"], true);
    assert_eq!(first["replayed"], false);
    assert_eq!(first["effect"]["effect_class"], "source_edit");
    assert_eq!(
        first["effect"]["idempotency_key"],
        "mcp-test.source-edit.exact-retry"
    );
    assert_eq!(first["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(first["effect"]["receipt"]["effect_class"], "source_edit");
    assert_eq!(first["effect"]["receipt"]["expected_state"], expected_state);
    assert!(first["effect"]["receipt"]["committed_state"].is_string());
    assert_eq!(fs::read(project.join("src/main.rs")).unwrap(), applied);

    let retry = handle_tool_call(&cg, "tracedecay_str_replace", apply_args, None, None)
        .await
        .unwrap();
    let retry: Value = serde_json::from_str(extract_text(&retry.value)).unwrap();
    assert_eq!(retry["success"], true);
    assert_eq!(retry["replayed"], true);
    assert_eq!(retry["effect"]["effect_id"], first["effect"]["effect_id"]);
    assert_eq!(retry["effect"]["receipt"], first["effect"]["receipt"]);
    assert_eq!(fs::read(project.join("src/main.rs")).unwrap(), applied);

    let stale_preview = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "new_name",
            "new_str": "final_name",
            "dry_run": true
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let stale_preview: Value = serde_json::from_str(extract_text(&stale_preview.value)).unwrap();
    let stale_expected_state = stale_preview["expected_state"].as_str().unwrap();
    let concurrent = b"fn new_name() {}\r\n// concurrent bytes\n";
    fs::write(project.join("src/main.rs"), concurrent).unwrap();
    let stale_apply = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "new_name",
            "new_str": "final_name",
            "idempotency_key": "mcp-test.source-edit.stale-cas",
            "expected_state": stale_expected_state
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let stale_apply: Value = serde_json::from_str(extract_text(&stale_apply.value)).unwrap();
    assert_eq!(stale_apply["success"], false);
    assert_eq!(stale_apply["replayed"], false);
    assert_eq!(stale_apply["effect"]["effect_class"], "source_edit");
    assert_eq!(stale_apply["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(
        stale_apply["effect"]["receipt"]["expected_state"],
        stale_expected_state
    );
    assert!(stale_apply["effect"]["receipt"]["committed_state"].is_null());
    assert_eq!(fs::read(project.join("src/main.rs")).unwrap(), concurrent);
}

#[tokio::test]
async fn test_str_replace_success() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn hello() {}\nfn world() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn hello() {}",
            "new_str": "fn hello_updated() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["matched_str"], "fn hello() {}");
    assert_eq!(parsed["new_str"], "fn hello_updated() {}");

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn hello_updated() {}"));
    assert!(!content.contains("fn hello() {}"));
}

#[tokio::test]
async fn path_containment_config_rejects_parent_traversal_before_serving_config() {
    let dir = test_temp_dir();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.path().join("outside.toml"),
        "token = \"OUTSIDE_SECRET\"\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_config",
        json!({"path": "../outside.toml", "key": "token"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "config read should reject parent traversal, got {result:?}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn path_containment_read_rejects_parent_traversal_before_serving_file() {
    let dir = test_temp_dir();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("outside.rs"), "fn leaked() {}\n").unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_read",
        json!({"file": "../outside.rs", "mode": "full"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "read should reject parent traversal before serving outside files, got {result:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_and_outline_preserve_symlink_indexed_file_key() {
    let dir = test_temp_dir();
    let project = dir.path().join("repo");
    let external = dir.path().join("external-src");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("lib.rs"), "pub fn through_symlink() {}\n").unwrap();
    unix_fs::symlink(&external, project.join("src")).unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let read = handle_tool_call(
        &cg,
        "tracedecay_read",
        json!({"file": "src/lib.rs", "mode": "full", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let read_text = extract_text(&read.value);
    let read_payload: serde_json::Value = serde_json::from_str(read_text).unwrap();
    assert_eq!(read_payload["file"], "src/lib.rs");
    assert!(
        read_payload["body"]
            .as_str()
            .unwrap_or_default()
            .contains("through_symlink"),
        "read should serve indexed source behind symlink: {read_payload:?}"
    );

    if !tracedecay::mcp::tools::ast_grep_outline_available() {
        return;
    }

    let outline = handle_tool_call(
        &cg,
        "tracedecay_outline",
        json!({"file": "src/lib.rs"}),
        None,
        None,
    )
    .await
    .unwrap();
    let outline_text = extract_text(&outline.value);
    let outline_payload: serde_json::Value = serde_json::from_str(outline_text).unwrap();
    assert_eq!(outline_payload["file"], "src/lib.rs");
    assert!(
        outline_payload["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol["name"] == "through_symlink"),
        "outline should query graph by indexed symlink path: {outline_payload:?}"
    );
}

#[tokio::test]
async fn outline_preserves_db_payload_and_adds_ast_grep_outline_when_available() {
    if !tracedecay::mcp::tools::ast_grep_outline_available() {
        return;
    }

    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_outline",
        json!({"file": "src/utils.rs"}),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.touched_files, vec!["src/utils.rs"]);
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["file"], "src/utils.rs");
    assert!(payload["symbol_count"].as_u64().is_some());
    assert!(
        payload["symbols"]
            .as_array()
            .is_some_and(|symbols| symbols.iter().any(|symbol| symbol["name"] == "helper")),
        "DB-backed symbols should still be present: {payload}"
    );
    assert!(
        payload["ast_grep_outline"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["name"] == "helper")))),
        "ast-grep outline should be attached under ast_grep_outline: {payload}"
    );
}

#[tokio::test]
async fn outline_markdown_uses_context_style_bullets_not_table() {
    if !tracedecay::mcp::tools::ast_grep_outline_available() {
        return;
    }

    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_outline",
        json!({"file": "src/utils.rs", "format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    assert!(text.contains("## Outline"));
    assert!(text.contains("- **helper**"));
    assert!(!text.contains("| symbol | kind |"));
}

#[cfg(unix)]
#[tokio::test]
async fn path_containment_config_rejects_symlink_escape_before_serving_config() {
    let dir = test_temp_dir();
    let project = dir.path().join("repo");
    let outside_dir = dir.path().join("outside");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        outside_dir.join("secret.toml"),
        "token = \"SYMLINK_SECRET\"\n",
    )
    .unwrap();
    unix_fs::symlink(&outside_dir, project.join("escape")).unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_config",
        json!({"path": "escape/secret.toml", "key": "token"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "config read should reject symlink escape, got {result:?}"
    );
}

#[tokio::test]
async fn project_selector_is_rejected_before_write_tool_parsing() {
    let (cg, _env, _dir) = setup_empty_project().await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({"project_selector": {"include_all_registered": true}}),
        None,
        None,
    )
    .await;
    let message = expect_tool_error(result);

    assert!(
        message.contains("does not accept project selectors"),
        "write tool should reject project_selector before parser errors, got {message}"
    );
}

#[tokio::test]
async fn test_str_replace_not_found() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn hello() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn not_exists() {}",
            "new_str": "fn replaced() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_str_replace_multiple_matches_fails() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn foo() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn foo() {}",
            "new_str": "fn bar() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("matches 2 times")
    );
}

#[tokio::test]
async fn test_multi_str_replace_success() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn foo() {}\nfn bar() {}\nfn baz() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn foo() {}", "fn foo_replaced() {}"],
                ["fn bar() {}", "fn bar_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo_replaced()"));
    assert!(content.contains("fn bar_replaced()"));
    assert!(content.contains("fn baz() {}"));
}

#[tokio::test]
async fn test_multi_str_replace_atomic_failure() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn baz() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn not_exists() {}", "fn replaced() {}"],
                ["fn baz() {}", "fn baz_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("must match exactly once")
    );

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo() {}"));
    assert!(content.contains("fn baz() {}"));
    assert!(!content.contains("fn replaced()"));
}

#[tokio::test]
async fn test_multi_str_replace_unicode_preview_does_not_panic() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn main() {}\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let missing_old = format!("{}é", "a".repeat(19));
    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                [missing_old, "replacement"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    let message = parsed["message"].as_str().unwrap();
    assert!(message.contains("matches 0 times"));
    assert!(message.contains("must match exactly once"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_multi_str_replace_earlier_insertion_collision_lands_correctly() {
    // Regression: match counts were validated against the ORIGINAL source but
    // replacements were then applied sequentially against progressively-edited
    // text. When an earlier replacement introduced a duplicate of a later
    // `old_str`, `replacen` clobbered the freshly-inserted copy instead of the
    // caller's intended original site. Resolving all ranges up-front against
    // the original and splicing once makes each replacement land where the
    // caller meant.
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn keep() {}\nfn target() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn keep() {}", "fn keep() {}\nfn target() {}"],
                ["fn target() {}", "fn target_renamed() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    // The inserted `fn target() {}` (from the first replacement) is preserved,
    // and the ORIGINAL second-line `fn target()` is the one that gets renamed.
    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(
        content,
        "fn keep() {}\nfn target() {}\nfn target_renamed() {}\n"
    );
}

#[tokio::test]
async fn test_multi_str_replace_overlapping_ranges_error() {
    // Two replacements whose matched ranges overlap cannot both be applied
    // coherently; the edit must be refused rather than silently dropping one.
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "abcdef\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["abcd", "WXYZ"],
                ["cdef", "QRST"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("overlapping ranges")
    );

    // The file must be untouched when the edit is refused.
    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_replace_symbol_documented_fn_keeps_single_doc_comment() {
    // The replaced span must cover the leading doc-comment block, so replacing
    // a documented fn with new_source that carries its own doc yields exactly
    // one doc comment — not the old one orphaned above the new one.
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // A leading item keeps `foo` off row 0 so its extracted attrs_start_line is
    // non-zero and survives the DB round-trip (a stored 0 is treated as a
    // pre-v7 sentinel and falls back to start_line).
    fs::write(
        project.join("src/main.rs"),
        "pub const N: u32 = 0;\n/// Doc for foo.\nfn foo() {\n    let _ = 1;\n}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_replace_symbol",
        json!({
            "symbol": "foo",
            "new_source": "/// Doc for foo.\nfn foo() {\n    let _ = 2;\n}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    // The returned replaced_span carries the old doc comment so the caller can
    // recover it if needed.
    assert!(
        parsed["replaced_span"]
            .as_str()
            .unwrap()
            .contains("/// Doc for foo.")
    );

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content.matches("/// Doc for foo.").count(), 1);
    assert!(content.contains("let _ = 2;"));
    assert!(!content.contains("let _ = 1;"));
}

#[tokio::test]
async fn test_insert_at_symbol_before_lands_above_attribute() {
    // `position=before` must insert above the item's leading doc/attribute
    // block, not between the docs and the item.
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // A leading item keeps `foo` off row 0 so its extracted attrs_start_line is
    // non-zero and survives the DB round-trip.
    fs::write(
        project.join("src/main.rs"),
        "pub const N: u32 = 0;\n/// Doc for foo.\nfn foo() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at_symbol",
        json!({
            "symbol": "foo",
            "content": "// INSERTED",
            "position": "before"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    let inserted_at = content.find("// INSERTED").unwrap();
    let doc_at = content.find("/// Doc for foo.").unwrap();
    assert!(
        inserted_at < doc_at,
        "inserted content should land above the doc comment, got: {content:?}"
    );
}

#[tokio::test]
async fn test_str_replace_unsupported_file_type_succeeds() {
    // Regression: editing unsupported types (e.g. .css) previously wrote the
    // file then returned a reindex error, silently mutating the file.
    let dir = test_temp_dir();
    let project = dir.path();

    fs::write(project.join("style.css"), ".foo {\n\tfont-size: 14px;\n}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "style.css",
            "old_str": "\tfont-size: 14px;",
            "new_str": "\tfont-size: 0.85rem;"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let warning = extract_text(&result.value);
    assert!(warning.contains("style.css"), "{warning}");
    assert!(warning.contains("edited after the last sync"), "{warning}");
    let parsed = extract_edit_json(&result.value);
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(!content.contains("14px"));
}

#[tokio::test]
async fn ast_grep_rewrite_has_literal_fallback_when_binary_missing() {
    if tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn old_name() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name", "rewrite": "new_name"}),
        None,
        None,
    )
    .await
    .unwrap();

    let output = extract_json(&result.value);
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    assert!(
        fs::read_to_string(project.join("src/lib.rs"))
            .unwrap()
            .contains("new_name"),
        "literal fallback should update the file"
    );
}

#[tokio::test]
async fn ast_grep_rewrite_uses_current_cli_update_flag() {
    if !tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn caller() { old_name(); }\npub fn old_name() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name()", "rewrite": "new_name()"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.contains("new_name();"),
        "ast-grep rewrite should apply with the installed CLI: {content}"
    );
    assert!(
        !output["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unexpected argument '-d'"),
        "rewrite must not use the removed -d flag: {output}"
    );
}

/// Regression: when ast-grep exits non-zero with empty stderr (no language
/// inferred from the file extension, or pattern matches nothing), the tool
/// used to surface `"ast-grep failed: "` — a useless empty trailer. The
/// message must instead explain the likely cause so the caller can act on it.
#[tokio::test]
async fn ast_grep_rewrite_surfaces_useful_error_on_empty_stderr() {
    if !tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn foo() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({
            "path": "src/lib.rs",
            "pattern": "__NONEXISTENT_PATTERN__",
            "rewrite": "whatever"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(false), "{output}");
    let message = output["message"].as_str().unwrap_or_default();
    assert!(
        !message.trim_end_matches(':').trim().eq("ast-grep failed"),
        "message must not end as an empty 'ast-grep failed:' — got: {message:?}"
    );
    assert!(
        message.contains("exit") || message.contains("0 nodes") || message.contains("no language"),
        "message must explain the likely cause (exit code / no language / 0 matches), got: {message:?}"
    );
}

#[tokio::test]
async fn test_multi_str_replace_unsupported_file_type_succeeds() {
    let dir = test_temp_dir();
    let project = dir.path();

    fs::write(
        project.join("style.css"),
        ".foo {\n\tfont-size: 14px;\n}\n.bar {\n\tfont-size: 16px;\n}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "style.css",
            "replacements": [
                ["\tfont-size: 14px;", "\tfont-size: 0.85rem;"],
                ["\tfont-size: 16px;", "\tfont-size: 1rem;"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let warning = extract_text(&result.value);
    assert!(warning.contains("style.css"), "{warning}");
    assert!(warning.contains("edited after the last sync"), "{warning}");
    let parsed = extract_edit_json(&result.value);
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(content.contains("1rem"));
    assert!(!content.contains("14px"));
    assert!(!content.contains("16px"));

    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_insert_at_string_anchor_before() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "line two",
            "content": "inserted line",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "inserted line");
    assert_eq!(lines[2], "line two");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_line_number() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "2",
            "content": "inserted at line 2",
            "before": false
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["anchor_line"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "line two");
    assert_eq!(lines[2], "inserted at line 2");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_anchor_not_found() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "line one\nline two\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "nonexistent",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_insert_at_unicode_anchor_prefix_does_not_panic() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "line one\nline two\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let long_anchor = format!("{}é", "a".repeat(99));
    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": long_anchor,
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_insert_at_ambiguous_anchor() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line foo\nline foo\nline bar\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "foo",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("matches 2 lines")
    );
}

// Regression: insert_at must not strip trailing newline (#57)
#[tokio::test]
async fn test_insert_at_preserves_trailing_newline() {
    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn hello() {}\n\nfn world() {}\n";
    fs::write(project.join("src/lib.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/lib.rs",
            "anchor": "fn world",
            "content": "fn extra() {}",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "file must end with newline after insert_at, got: {:?}",
        &content[content.len().saturating_sub(20)..]
    );
    assert_eq!(content, "fn hello() {}\n\nfn extra() {}\nfn world() {}\n");
}
