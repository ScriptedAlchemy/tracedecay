use super::*;
use crate::daemon_client::RequestedOutputFormat;
use crate::mcp::response_handles::{
    ResponseHandleLookup, lock_response_handle_store, retrieve_response_handle,
};
use crate::tracedecay::current_timestamp;
use serde_json::json;

#[test]
fn default_format_is_markdown() {
    assert_eq!(parse_format(&json!({})), RequestedOutputFormat::Markdown);
    assert_eq!(
        parse_format(&json!({"format": "markdown"})),
        RequestedOutputFormat::Markdown
    );
    assert_eq!(
        parse_format(&json!({"format": "md"})),
        RequestedOutputFormat::Markdown
    );
    assert_eq!(
        parse_format(&json!({"format": "json"})),
        RequestedOutputFormat::Json
    );
    assert_eq!(
        parse_format(&json!({"format": "JSON"})),
        RequestedOutputFormat::Json
    );
    assert_eq!(
        parse_format(&json!({"format": "yaml"})),
        RequestedOutputFormat::Markdown
    );
}

#[test]
fn json_format_is_compact() {
    let value = json!({"a": 1, "b": [1, 2]});
    let out = finalize(None, &json!({"format": "json"}), &value, || {
        "unused".to_string()
    });
    assert_eq!(out, "{\"a\":1,\"b\":[1,2]}");
    assert!(
        !out.contains('\n'),
        "compact json must not be pretty-printed"
    );
}

#[test]
fn markdown_format_uses_closure() {
    let value = json!({"a": 1});
    let out = finalize(None, &json!({}), &value, || "## Hi\n".to_string());
    assert_eq!(out, "## Hi\n");
}

#[test]
fn truncated_json_envelope_includes_handle() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    let long = format!(
        "{{\"items\":[{}]}}",
        (0..3_000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );

    let result = truncated_json_envelope_with_handle(Some(dir.path()), &long);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(parsed["truncated"], true);
    assert_eq!(parsed["retrieve_tool"], "tracedecay_retrieve");
    assert!(parsed.get("retrieve_handle").is_none());
    let handle = parsed["handle"].as_str().unwrap();
    assert!(handle.starts_with("rh_"));

    let stored = retrieve_response_handle(dir.path(), handle, current_timestamp()).unwrap();
    match stored {
        ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
        other => panic!("stored response should be retrievable, got {other:?}"),
    }
}

#[test]
fn truncated_json_envelope_reports_character_counts_for_utf8() {
    let long = "🦀".repeat(MAX_RESPONSE_CHARS);

    let result = truncated_json_envelope_with_handle(None, &long);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(parsed["original_chars"], MAX_RESPONSE_CHARS);
    let preview = parsed["preview"].as_str().unwrap();
    assert_eq!(parsed["preview_chars"], preview.chars().count());
}

#[test]
fn truncated_markdown_includes_readable_handle_guidance() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    let long = format!("# Scan\n\n{}", "- repeated finding\n".repeat(3_000));

    let result = truncated_markdown_with_handle(Some(dir.path()), &long);

    assert!(result.starts_with("# Truncated Response"));
    assert!(result.contains("## Preview"));
    assert!(result.contains("Full response stored locally"));
    assert!(result.contains("tracedecay_retrieve"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&result).is_err(),
        "markdown truncation should not render as a JSON envelope"
    );
    let Some(handle) = result
        .split("handle `")
        .nth(1)
        .and_then(|tail| tail.split('`').next())
    else {
        panic!("markdown guidance should include handle");
    };
    assert!(handle.starts_with("rh_"));

    let stored = retrieve_response_handle(dir.path(), handle, current_timestamp()).unwrap();
    match stored {
        ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
        other => panic!("stored markdown response should be retrievable, got {other:?}"),
    }
}

#[test]
fn truncated_markdown_preserves_late_priority_sections() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    let long = format!(
        "## Code Context\n{}\n### Memory Matches\n- fact_id=fact.v1.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb category=project trust=0.90 score=0.500: remembered context\n\n### Entry Points\n- **late_symbol** (function) - src/lib.rs:10\n",
        "padding\n".repeat(5_000)
    );

    let result = truncated_markdown_with_handle(Some(dir.path()), &long);

    assert!(result.len() <= MAX_RESPONSE_CHARS);
    assert!(result.contains("## Preserved Priority Sections"));
    assert!(result.contains("### Memory Matches"));
    assert!(result.contains("fact_id=fact.v1."));
    assert!(result.contains("### Entry Points"));
    assert!(result.contains("late_symbol"));
}

#[test]
fn markdown_truncation_preview_closes_open_code_fence() {
    let markdown = format!("### Code\n```rust\n{}\n", "fn demo() {}\n".repeat(1_000));

    let preview = markdown_truncation_preview(&markdown, 1_024);

    assert!(!has_open_markdown_fence(&preview));
}

#[test]
fn markdown_truncation_preview_closes_prefix_fence_before_preserved_sections() {
    let markdown = format!(
        "## Code Context\n```rust\n{}\n### Memory Matches\n- fact_id=fact.v1.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb category=project trust=0.90 score=0.500: remembered context\n",
        "fn demo() {}\n".repeat(5_000)
    );

    let preview = markdown_truncation_preview(&markdown, 12_000);
    let preserved_start = preview
        .find("## Preserved Priority Sections")
        .unwrap_or(preview.len());
    assert!(
        preserved_start < preview.len(),
        "late priority section should be preserved: {preview}"
    );

    assert!(!has_open_markdown_fence(&preview[..preserved_start]));
}

#[test]
fn markdown_preview_with_handle_stores_full_text_when_preview_differs() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    let full = format!(
        "# Full\n\nsmall visible preview\n\n{}## Details\nfull-only detail",
        "full-only body\n".repeat(MAX_RESPONSE_CHARS)
    );
    let preview = "# Full\n\nsmall visible preview";

    let result = markdown_preview_with_handle(Some(dir.path()), &full, preview);

    assert!(result.starts_with("# Truncated Response"));
    assert!(result.contains("lane-budgeted preview"));
    assert!(result.contains(preview));
    assert!(!result.contains("full-only detail"));
    let Some(handle) = result
        .split("handle `")
        .nth(1)
        .and_then(|tail| tail.split('`').next())
    else {
        panic!("markdown preview envelope should include handle");
    };

    let stored = retrieve_response_handle(dir.path(), handle, current_timestamp()).unwrap();
    match stored {
        ResponseHandleLookup::Found(record) => assert_eq!(record.content, full),
        other => panic!("stored markdown preview should be retrievable, got {other:?}"),
    }
}

#[test]
fn markdown_preview_with_handle_keeps_matching_short_text_plain() {
    let text = "# Full\n\nalready complete";

    assert_eq!(markdown_preview_with_handle(None, text, text), text);
}

#[test]
fn markdown_preview_with_handle_keeps_different_short_full_text_plain() {
    let full = "# Full\n\nsmall visible preview\n\n## Details\nfull-only detail";
    let preview = "# Full\n\nsmall visible preview";

    assert_eq!(markdown_preview_with_handle(None, full, preview), full);
}

#[test]
fn truncated_json_envelope_reports_store_failure() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".tracedecay")).unwrap();
    std::fs::write(
        dir.path().join(".tracedecay/enrollment.json"),
        r#"{"project_id":"../invalid","storage_mode":"profile_sharded"}"#,
    )
    .unwrap();
    let long = format!(
        "{{\"items\":[{}]}}",
        (0..3_000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );

    let result = truncated_json_envelope_with_handle(Some(dir.path()), &long);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(parsed["truncated"], true);
    assert_eq!(parsed["handle_available"], false);
    assert!(parsed.get("handle").is_none());
    assert_eq!(
        parsed["handle_status"]["reason_code"],
        "handle_store_failed"
    );
    let message = parsed["handle_status"]["message"].as_str().unwrap();
    assert_eq!(
        message,
        "The full response could not be cached locally, so no retrieval handle is available."
    );
    assert!(!message.contains(dir.path().to_string_lossy().as_ref()));
}

#[test]
fn generic_md_renders_array_of_objects_as_bullets() {
    let v = json!([
        {"id": "function:abc", "name": "foo", "line": 10},
        {"id": "function:def", "name": "bar", "line": 20}
    ]);
    let out = generic_md(&v);
    assert!(out.contains("- **foo**"), "got: {out}");
    assert!(out.contains("- **bar**"), "got: {out}");
    assert!(out.contains("**line:** 10"), "got: {out}");
    assert!(out.contains("`function:abc`"), "id should be backticked");
    assert!(!out.contains("| name | line | id |"), "got: {out}");
}

#[test]
fn generic_md_renders_object_fields_and_sections() {
    let v = json!({
        "total": 3,
        "name": "demo",
        "items": [{"file": "a.rs", "count": 1}]
    });
    let out = generic_md(&v);
    assert!(out.contains("**total:** 3"));
    assert!(out.contains("**name:** demo"));
    assert!(out.contains("## items"));
    assert!(out.contains("- **a.rs**"), "got: {out}");
    assert!(out.contains("**count:** 1"), "got: {out}");
    assert!(!out.contains("| file | count |"), "got: {out}");
}

#[test]
fn diagnostics_md_renders_bullets_not_tables() {
    let v = json!({
        "scope": "workspace",
        "diagnostic_count": 1,
        "error_count": 1,
        "warning_count": 0,
        "diagnostics": [{
            "level": "error",
            "code": "E0425",
            "message": "cannot find value\nsecond line",
            "file": "src/lib.rs",
            "line_start": 42,
            "driver": "cargo",
            "enclosing": "crate::demo",
            "near_duplicates": [{
                "name": "compute_b",
                "file": "src/other.rs",
                "line": 7,
                "id": "function:abc",
                "ranking_score": 0.85,
                "severity": "definite",
                "overlap_kind": "ast_isomorphic"
            }]
        }]
    });

    let out = diagnostics_md(&v);

    assert!(out.contains("## Diagnostics"), "got: {out}");
    assert!(out.contains("**Diagnostic count:** 1"), "got: {out}");
    assert!(
        out.contains("- **ERROR E0425 at src/lib.rs:42**"),
        "got: {out}"
    );
    assert!(out.contains("  **Message:** cannot find value\n    second line"));
    assert!(
        out.contains("  **Near-duplicates:** compute_b (src/other.rs:7) [ast_isomorphic]"),
        "got: {out}"
    );
    assert!(!out.contains("| file |"), "got: {out}");
    assert!(!out.contains("| --- |"), "got: {out}");
}

#[test]
fn risky_patterns_md_renders_matches() {
    // Regression: this payload uses the `matches` shape, not `diagnostics`.
    // Routing it through `diagnostics_md` printed "No diagnostics." and dropped
    // every finding, which is why `tracedecay_unsafe_patterns` looked empty.
    let v = json!({
        "match_count": 1,
        "by_kind": { "unsafe_block": 1 },
        "matches": [{
            "kind": "unsafe_block",
            "file": "src/audit.rs",
            "line": 28,
            "snippet": "unsafe { *ptr as usize }",
            "enclosing": "src/audit.rs::raw_total_len",
            "in_test": false
        }]
    });

    let out = risky_patterns_md(&v);

    assert!(out.contains("## Risky Patterns"), "got: {out}");
    assert!(out.contains("**Match count:** 1"), "got: {out}");
    assert!(out.contains("**By kind:** unsafe_block: 1"), "got: {out}");
    assert!(
        out.contains("- **UNSAFE_BLOCK at src/audit.rs:28**"),
        "got: {out}"
    );
    assert!(
        out.contains("  **Snippet:** unsafe { *ptr as usize }"),
        "got: {out}"
    );
    assert!(
        out.contains("  **Enclosing:** src/audit.rs::raw_total_len"),
        "got: {out}"
    );
    assert!(!out.contains("No diagnostics"), "got: {out}");
    assert!(!out.contains("No risky patterns"), "got: {out}");
}

#[test]
fn risky_patterns_md_empty_is_noted() {
    let out = risky_patterns_md(&json!({ "match_count": 0, "by_kind": {}, "matches": [] }));
    assert!(out.contains("## Risky Patterns"), "got: {out}");
    assert!(out.contains("**Match count:** 0"), "got: {out}");
    assert!(out.contains("No risky patterns found."), "got: {out}");
}

#[test]
fn unused_imports_md_renders_findings() {
    let v = json!({
        "unused_import_count": 1,
        "imports": [{
            "id": "use:abc",
            "name": "std::collections::BTreeMap",
            "unused": "BTreeMap",
            "file": "src/audit.rs",
            "line": 18
        }]
    });

    let out = unused_imports_md(&v);

    assert!(out.contains("## Unused Imports"), "got: {out}");
    assert!(out.contains("**Unused import count:** 1"), "got: {out}");
    assert!(
        out.contains("- **BTreeMap unused in src/audit.rs:18**"),
        "got: {out}"
    );
    assert!(
        out.contains("  **Import:** std::collections::BTreeMap"),
        "got: {out}"
    );
    assert!(!out.contains("No unused imports"), "got: {out}");
}

#[test]
fn unused_imports_md_empty_is_noted() {
    let out = unused_imports_md(&json!({ "unused_import_count": 0, "imports": [] }));
    assert!(out.contains("## Unused Imports"), "got: {out}");
    assert!(out.contains("**Unused import count:** 0"), "got: {out}");
    assert!(out.contains("No unused imports found."), "got: {out}");
}

#[test]
fn generic_md_empty_is_noted() {
    assert!(generic_md(&json!([])).contains("None."));
    assert!(generic_md(&json!({})).contains("No results."));
}

#[test]
fn generic_md_compacts_scalar_path_arrays() {
    let out = generic_md(&json!({
        "changed_files": [
            "tests/gateway/test_gateway_shutdown.py",
            "tests/gateway/test_goal_verdict_send.py",
            "tests/gateway/test_homeassistant.py"
        ]
    }));

    assert!(out.contains("## changed_files"));
    assert!(out.contains("tests/gateway/"));
    assert!(out.contains("  test_gateway_shutdown.py"));
    assert!(!out.contains("- tests/gateway/test_gateway_shutdown.py"));
}

#[test]
fn generic_md_keeps_non_path_scalar_arrays_as_bullets() {
    let out = generic_md(&json!({
        "warnings": ["first warning", "second warning"]
    }));

    assert!(out.contains("- first warning"));
    assert!(out.contains("- second warning"));
}

#[test]
fn format_score_rounds_and_trims() {
    assert_eq!(format_score(0.123_456_789_012_345), "0.12");
    assert_eq!(format_score(1.0), "1");
    assert_eq!(format_score(1.5), "1.5");
    assert_eq!(format_score(2.50), "2.5");
    assert_eq!(format_score(0.0), "0");
}

#[test]
fn generic_md_rounds_float_scores() {
    // 16-digit similarity scores must not leak into the table.
    let v = json!([{ "name": "foo", "score": 0.876_543_210_987_654_3 }]);
    let out = generic_md(&v);
    assert!(out.contains("0.88"), "got: {out}");
    assert!(!out.contains("0.8765432"), "raw float leaked: {out}");
}

#[test]
fn generic_md_humanizes_timestamp_keys() {
    let v = json!({ "created_at": 100_000_100u64 });
    let out = generic_md(&v);
    assert!(out.contains("ago"), "expected relative age, got: {out}");
    assert!(
        out.contains("100000100"),
        "should keep raw epoch too: {out}"
    );
}

#[test]
fn generic_md_leaves_small_numbers_untouched_for_timestamp_keys() {
    // A tiny value under the epoch threshold is not a real timestamp.
    let v = json!({ "wait_time": 5 });
    let out = generic_md(&v);
    assert!(out.contains("**wait_time:** 5"), "got: {out}");
}

#[test]
fn nested_array_of_objects_becomes_summary_cell() {
    let v = json!([{
        "name": "outer",
        "callers": [
            { "name": "a", "file": "x.rs", "line": 1 },
            { "name": "b", "file": "y.rs", "line": 2 }
        ]
    }]);
    let out = generic_md(&v);
    assert!(out.contains("a (x.rs:1)"), "got: {out}");
    assert!(out.contains("b (y.rs:2)"), "got: {out}");
    assert!(!out.contains("[{"), "raw JSON leaked into cell: {out}");
}

#[test]
fn nested_array_summary_truncates_with_count() {
    let v = json!([{
        "name": "outer",
        "items": [
            {"name": "a"}, {"name": "b"}, {"name": "c"}, {"name": "d"}
        ]
    }]);
    let out = generic_md(&v);
    assert!(out.contains("+1 more"), "got: {out}");
    assert!(out.contains("4 total"), "got: {out}");
}

#[test]
fn nested_object_becomes_kv_cell() {
    let v = json!([{ "name": "row", "meta": { "a": 1, "b": "two" } }]);
    let out = generic_md(&v);
    assert!(out.contains("a=1"), "got: {out}");
    assert!(out.contains("b=two"), "got: {out}");
    assert!(!out.contains("{\"a\""), "raw JSON leaked: {out}");
}

#[test]
fn object_array_fields_use_preferred_order() {
    let v = json!([{ "line": 5, "name": "foo", "file": "a.rs", "extra": "z", "kind": "fn" }]);
    let out = generic_md(&v);
    let kind_idx = out.find("**kind:** fn").unwrap();
    let file_idx = out.find("**file:** a.rs").unwrap();
    let line_idx = out.find("**line:** 5").unwrap();
    let extra_idx = out.find("**extra:** z").unwrap();
    assert!(kind_idx < file_idx, "got: {out}");
    assert!(file_idx < line_idx, "got: {out}");
    assert!(line_idx < extra_idx, "got: {out}");
}

#[test]
fn object_array_drops_all_empty_fields() {
    let v = json!([
        { "name": "foo", "doc": "", "line": 1 },
        { "name": "bar", "doc": "", "line": 2 }
    ]);
    let out = generic_md(&v);
    assert!(
        !out.contains("doc"),
        "empty column should be dropped: {out}"
    );
    assert!(out.contains("- **foo**"), "got: {out}");
    assert!(out.contains("**line:** 1"), "got: {out}");
    assert!(!out.contains("| name | line |"), "got: {out}");
}

#[test]
fn table_with_no_visible_columns_is_not_blank() {
    let out = generic_md(&json!([{ "id": null }, { "id": null }]));
    assert!(
        out.contains("No visible fields across 2 rows; dropped empty keys: id."),
        "got: {out}"
    );
}

#[test]
fn object_array_hoists_constant_fields() {
    let v = json!([
        { "name": "foo", "edge_kind": "calls" },
        { "name": "bar", "edge_kind": "calls" }
    ]);
    let out = generic_md(&v);
    assert!(
        out.contains("**edge_kind:** calls"),
        "constant not hoisted: {out}"
    );
    // It should not also appear as a table column.
    assert!(
        !out.contains("| edge_kind"),
        "constant leaked into table output: {out}"
    );
    assert!(out.contains("- **foo**"), "got: {out}");
    assert!(out.contains("- **bar**"), "got: {out}");
}

#[test]
fn object_array_indents_multiline_field_values() {
    let v = json!([{ "name": "foo", "doc": "first\n# heading\n- item" }]);
    let out = generic_md(&v);
    assert!(
        out.contains("  **doc:** first\n    # heading\n    - item"),
        "got: {out}"
    );
    assert!(!out.contains("\n# heading"), "got: {out}");
    assert!(!out.contains("\n- item"), "got: {out}");
}

#[test]
fn object_omits_empty_scalar_fields() {
    let v = json!({ "name": "x", "docstring": "" });
    let out = generic_md(&v);
    assert!(out.contains("**name:** x"), "got: {out}");
    assert!(
        !out.contains("docstring"),
        "empty scalar should be omitted: {out}"
    );
}

#[test]
fn object_collapses_empty_collections_to_one_line() {
    let v = json!({ "name": "x", "callers": [], "meta": {} });
    let out = generic_md(&v);
    assert!(out.contains("callers: none"), "got: {out}");
    assert!(out.contains("meta: none"), "got: {out}");
    // No bare heading for the empty collection.
    assert!(!out.contains("## callers"), "got: {out}");
}
