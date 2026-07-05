use super::*;
use crate::mcp::response_handles::{retrieve_response_handle_from_root, ResponseHandleLookup};
use crate::tracedecay::current_timestamp;
use serde_json::json;

#[test]
fn default_format_is_markdown() {
    assert_eq!(parse_format(&json!({})), OutputFormat::Markdown);
    assert_eq!(
        parse_format(&json!({"format": "markdown"})),
        OutputFormat::Markdown
    );
    assert_eq!(
        parse_format(&json!({"format": "md"})),
        OutputFormat::Markdown
    );
    assert_eq!(parse_format(&json!({"format": "json"})), OutputFormat::Json);
    assert_eq!(parse_format(&json!({"format": "JSON"})), OutputFormat::Json);
    assert_eq!(
        parse_format(&json!({"format": "yaml"})),
        OutputFormat::Markdown
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
fn truncate_short_response() {
    let short = "hello world";
    assert_eq!(truncate_response(short), short);
}

#[test]
fn truncate_long_response() {
    let long = "x".repeat(20_000);
    let result = truncate_response(&long);
    assert!(result.len() < 20_000);
    assert!(result.contains("[... truncated at 15000 chars]"));
}

#[test]
fn truncated_json_envelope_includes_handle() {
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

    let prepared = prepare_truncated_response_handle(Some(dir.path()), &long);
    let record = prepared.record.as_ref().unwrap();
    assert_eq!(record.handle, handle);
    let stored = retrieve_response_handle_from_root(
        &record.response_handle_root,
        handle,
        current_timestamp(),
    )
    .unwrap();
    match stored {
        ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
        other => panic!("stored response should be retrievable, got {other:?}"),
    }
}

#[test]
fn truncated_markdown_includes_readable_handle_guidance() {
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

    let prepared = prepare_truncated_response_handle(Some(dir.path()), &long);
    let record = prepared.record.as_ref().unwrap();
    assert_eq!(record.handle, handle);
    let stored = retrieve_response_handle_from_root(
        &record.response_handle_root,
        handle,
        current_timestamp(),
    )
    .unwrap();
    match stored {
        ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
        other => panic!("stored markdown response should be retrievable, got {other:?}"),
    }
}

#[test]
fn truncate_text_with_handle_returns_short_text_unchanged() {
    let short = "hello world";
    assert_eq!(truncate_text_with_handle(None, short), short);
}

#[test]
fn truncate_text_with_handle_stores_reversible_envelope() {
    let dir = tempfile::TempDir::new().unwrap();
    let long = "- indexed file entry\n".repeat(3_000);

    let result = truncate_text_with_handle(Some(dir.path()), &long);

    assert!(result.len() <= MAX_RESPONSE_CHARS);
    assert!(result.starts_with("# Truncated Response"));
    assert!(result.contains("## Preview"));
    assert!(result.contains("tracedecay_retrieve"));
    let Some(handle) = result
        .split("handle `")
        .nth(1)
        .and_then(|tail| tail.split('`').next())
    else {
        panic!("truncate_text_with_handle envelope should include handle");
    };
    assert!(handle.starts_with("rh_"));
}

#[test]
fn truncated_json_envelope_reports_store_failure() {
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
    assert!(parsed["handle_status"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("could not be cached locally"));
}

#[test]
fn generic_md_renders_array_of_objects_as_table() {
    let v = json!([
        {"id": "function:abc", "name": "foo", "line": 10},
        {"id": "function:def", "name": "bar", "line": 20}
    ]);
    let out = generic_md(&v);
    // Preferred column order follows PREFERRED_COLUMNS
    // (name, kind, file, line, id, ...), so `line` sorts ahead of `id`.
    assert!(out.contains("| name | line | id |"), "got: {out}");
    assert!(out.contains("`function:abc`"), "id should be backticked");
    assert!(out.contains("foo"));
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
    // `file` is a preferred column so it sorts ahead of `count`.
    assert!(out.contains("| file | count |"), "got: {out}");
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
fn table_columns_use_preferred_order() {
    let v = json!([{ "line": 5, "name": "foo", "file": "a.rs", "extra": "z", "kind": "fn" }]);
    let out = generic_md(&v);
    let header = out.lines().find(|l| l.starts_with("| name")).unwrap();
    // name, kind, file, line come first; unknown `extra` sorts after.
    assert_eq!(
        header, "| name | kind | file | line | extra |",
        "got: {out}"
    );
}

#[test]
fn table_drops_all_empty_columns() {
    let v = json!([
        { "name": "foo", "doc": "", "line": 1 },
        { "name": "bar", "doc": "", "line": 2 }
    ]);
    let out = generic_md(&v);
    assert!(
        !out.contains("doc"),
        "empty column should be dropped: {out}"
    );
    assert!(out.contains("| name | line |"), "got: {out}");
}

#[test]
fn table_hoists_constant_columns() {
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
        "constant leaked into table: {out}"
    );
    assert!(out.contains("| name |"), "got: {out}");
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

#[test]
fn table_escapes_pipes() {
    let mut md = Md::new();
    md.table(
        &["name", "sig"],
        &[vec!["foo".to_string(), "fn foo(a|b)".to_string()]],
    );
    let out = md.render();
    assert!(out.contains("fn foo(a\\|b)"));
    assert!(out.contains("| name | sig |"));
}
