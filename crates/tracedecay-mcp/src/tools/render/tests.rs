use super::*;
use crate::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use serde_json::json;
use tracedecay_runtime_core::tracedecay::current_timestamp;

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
    let instruction = parsed["retrieve_instruction"].as_str().unwrap();
    assert!(instruction.contains("Use the preview"));
    assert!(instruction.contains("Only call it if the missing details are needed"));
    assert!(
        !instruction.contains("join the pages"),
        "truncation must not instruct a full reassembly: {instruction}"
    );
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
    assert_eq!(parsed["truncated"], true);
    let preview = parsed["preview"].as_str().unwrap();
    assert_eq!(parsed["preview_chars"], preview.chars().count());
}

#[test]
fn truncated_markdown_includes_readable_handle_guidance() {
    let dir = tempfile::TempDir::new().unwrap();
    let long = format!("# Scan\n\n{}", "- repeated finding\n".repeat(3_000));

    let result = truncated_markdown_with_handle(Some(dir.path()), &long);

    assert!(result.starts_with("# Truncated Response"));
    assert!(result.contains("## Preview"));
    assert!(result.contains("Preview is the context to use"));
    assert!(result.contains("Do not join stored pages"));
    assert!(!result.contains("until has_more is false"));
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
fn markdown_truncation_preview_omits_partial_code_fence() {
    let markdown = format!("### Code\n```rust\n{}\n", "fn demo() {}\n".repeat(1_000));

    let preview = markdown_truncation_preview(&markdown, 1_024);

    assert_eq!(preview, "### Code\n");
}

#[test]
fn markdown_truncation_preview_omits_partial_code_before_preserved_sections() {
    let markdown = format!(
        "## Code Context\n```rust\n{}\n```\n### Memory Matches\n- fact_id=fact.v1.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb category=project trust=0.90 score=0.500: remembered context\n",
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

    assert_eq!(&preview[..preserved_start], "## Code Context\n\n\n");
    assert!(!preview.contains("fn demo"));
    assert!(preview.contains("remembered context"));
}

#[test]
fn markdown_preview_keeps_complete_blocks_and_omits_the_next_body() {
    let first = "Intro\n```rust\nfn café() {}\n```\n";
    let second = "```rust\nfn next() { work(); }\n```\n";
    let markdown = format!("{first}{second}");

    for budget in first.len()..markdown.len() - 4 {
        assert_eq!(whole_markdown_prefix(&markdown, budget), first);
    }
    assert_eq!(whole_markdown_prefix(&markdown, markdown.len()), markdown);
}

#[test]
fn code_renderer_encloses_nested_fences_without_changing_the_body() {
    let body = "```rust\nfn café() {}\n```\n### Memory Matches\ninside code";
    let mut md = Md::new();
    md.code("markdown", body);
    let markdown = md.render();
    assert_eq!(markdown, format!("````markdown\n{body}\n````\n"));
    assert_eq!(whole_markdown_prefix(&markdown, markdown.len() - 4), "");
    assert_eq!(whole_markdown_prefix(&markdown, markdown.len()), markdown);
    assert_eq!(markdown_heading_at(&markdown, "### Memory Matches"), None);
}

#[test]
fn priority_sections_keep_nested_heading_lines_inside_complete_code_blocks() {
    let markdown = "### Memory Matches\n````markdown\n```\n### Entry Points\ncode\n```\n````\n### Extension Points\nactual section\n";
    let start = markdown_heading_at(markdown, "### Memory Matches").unwrap();
    let section = markdown_section_at(markdown, start);
    assert!(section.ends_with("````\n"));
    assert!(section.contains("### Entry Points"));
    assert!(!section.contains("actual section"));
    assert_eq!(markdown_heading_at(markdown, "### Entry Points"), None);
}

#[test]
fn markdown_preview_with_handle_stores_full_text_when_preview_differs() {
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
fn truncated_json_envelope_reports_store_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    // A handle root beneath a regular file makes every durable write fail.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let handle_root = blocker.join("response-handles");
    let long = format!(
        "{{\"items\":[{}]}}",
        (0..3_000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );

    let result = truncated_json_envelope_with_handle(Some(&handle_root), &long);
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
fn handle_store_busy_omits_paths() {
    let busy = handle_store_failure_status(&TraceDecayError::LockDeadline {
        resource: "response-handle writer lock",
        deadline_ms: 10_000,
    });
    assert_eq!(busy["reason_code"], "handle_store_busy");
    assert_eq!(busy["retryable"], true);
}
