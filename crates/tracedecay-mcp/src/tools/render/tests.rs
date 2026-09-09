use super::*;
use crate::response_handles::{
    ResponseHandleLookup, lock_response_handle_store, retrieve_response_handle,
};
use serde_json::json;
use std::ffi::OsString;
use tracedecay_runtime_core::tracedecay::current_timestamp;

/// Restores one environment variable on drop. Callers must already hold
/// `lock_response_handle_store` (the user-data-dir test-env lock) so the
/// mutation cannot race other tests.
struct EnvRestore {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }
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
fn truncated_json_envelope_reports_store_failure() {
    let _store_guard = lock_response_handle_store();
    let dir = tempfile::TempDir::new().unwrap();
    // Identity never lives in the working tree, so the honest failure
    // injection is an unwritable profile root: pin discovery beneath a
    // regular file so every durable handle-store write fails.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let _profile = EnvRestore::set(
        tracedecay_runtime_core::config::USER_DATA_DIR_ENV,
        blocker.join(".tracedecay"),
    );
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
