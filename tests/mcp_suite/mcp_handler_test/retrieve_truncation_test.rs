use crate::support::*;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::fmt::Write as _;
use std::fs;
#[path = "retrieve_truncation_support.rs"]
mod retrieve_truncation_support;
#[cfg(feature = "test-transport")]
use retrieve_truncation_support::call_production_tool;
use retrieve_truncation_support::retrieve_json_arguments;

#[tokio::test]
async fn retrieve_tool_returns_full_stored_response() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let original = "{\"items\":[{\"id\":1,\"name\":\"alpha\"}]}";
    let stored = tracedecay::mcp::response_handles::store_response_handle(
        cg.project_root(),
        original,
        tracedecay::tracedecay::current_timestamp(),
    )
    .unwrap();

    let response_handle_root =
        tracedecay::storage::resolve_response_handle_root(cg.project_root()).unwrap();
    let stored_payload: Value = serde_json::from_str(
        &fs::read_to_string(response_handle_root.join(format!("{}.json", stored.handle))).unwrap(),
    )
    .unwrap();
    assert!(stored_payload.get("handle").is_none());
    assert!(stored_payload.get("original_chars").is_none());
    assert_eq!(stored_payload["content"], original);

    let result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        retrieve_json_arguments(&stored.handle),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["handle"], stored.handle);
    assert_eq!(payload["content"], original);
    assert_eq!(payload["expired"], false);

    let alias_result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({
            "format": "json",
            "handle": stored.handle,
            "retrieve_handle": stored.handle,
        }),
        None,
        None,
    )
    .await;
    assert!(
        alias_result.is_err(),
        "tracedecay_retrieve must accept only the canonical `handle` field"
    );
}

#[tokio::test]
async fn retrieve_tool_reports_missing_and_expired_handles_actionably() {
    let (cg, _env, _dir) = setup_empty_project().await;

    let missing = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        retrieve_json_arguments("rh_0123456789abcdef01234567"),
        None,
        None,
    )
    .await
    .unwrap();
    let missing_payload: Value = serde_json::from_str(extract_text(&missing.value)).unwrap();
    assert_eq!(missing_payload["expired"], Value::Null);
    assert_eq!(missing_payload["content"], Value::Null);
    assert_eq!(missing_payload["reason_code"], "handle_not_found");
    assert_eq!(missing_payload["retryable"], true);
    assert!(
        missing_payload["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not found")
    );
    assert!(
        missing_payload["retry_instruction"]
            .as_str()
            .unwrap_or_default()
            .contains("Re-run the original MCP tool")
    );

    let expired = tracedecay::mcp::response_handles::store_response_handle(
        cg.project_root(),
        "{\"items\":[42]}",
        tracedecay::tracedecay::current_timestamp()
            - tracedecay::mcp::response_handles::RESPONSE_HANDLE_TTL_SECS
            - 5,
    )
    .unwrap();

    let expired_result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        retrieve_json_arguments(&expired.handle),
        None,
        None,
    )
    .await
    .unwrap();
    let expired_payload: Value = serde_json::from_str(extract_text(&expired_result.value)).unwrap();
    assert_eq!(expired_payload["expired"], true);
    assert_eq!(expired_payload["content"], Value::Null);
    assert_eq!(expired_payload["reason_code"], "handle_expired");
    assert_eq!(expired_payload["retryable"], true);
    assert_eq!(expired_payload["expires_at"], expired.expires_at);
    assert!(
        expired_payload["message"]
            .as_str()
            .unwrap_or_default()
            .contains("expired")
    );
    assert!(
        expired_payload["retry_instruction"]
            .as_str()
            .unwrap_or_default()
            .contains("Re-run the original MCP tool")
    );

    let identity_path = tracedecay::storage::repository_identity_path(cg.project_root()).unwrap();
    fs::write(
        &identity_path,
        r#"{"schema_version":1,"project_id":"../operator-private"}"#,
    )
    .unwrap();
    let unavailable = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        retrieve_json_arguments("rh_0123456789abcdef01234567"),
        None,
        None,
    )
    .await
    .expect_err("invalid storage identity must fail closed");
    let public = unavailable.to_string();
    assert!(public.contains("response-handle cache is unavailable"));
    assert!(!public.contains(cg.project_root().to_string_lossy().as_ref()));
    assert!(!public.contains(identity_path.to_string_lossy().as_ref()));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn fact_store_large_json_list_response_uses_retrieve_handle() {
    const FACT_COUNT: usize = 8;

    let fixture = production_composition_fixture().await;
    let mut last_fact_id = None;
    for index in 0..FACT_COUNT {
        let added = call_production_tool(
            &fixture,
            "tracedecay_fact_store_add",
            json!({
                "format": "json",
                "content": format!(
                    "LONG_FACT_MARKER_{index:02}: {}",
                    "large fact-store response should remain retrievable ".repeat(180)
                ),
                "category": "project",
                "trust": 0.9
            }),
        )
        .await;
        let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
        last_fact_id = added["outcome"]["value"]["payload"]["result"]["fact"]["fact"]["fact_id"]
            .as_str()
            .map(str::to_owned);
    }
    let last_fact_id = last_fact_id.expect("tail fact id");

    let markdown_list = call_production_tool(
        &fixture,
        "tracedecay_fact_store_list",
        json!({"category": "project", "min_trust": 0.0, "limit": 200}),
    )
    .await;
    let markdown_text = extract_text(&markdown_list.value);
    assert!(
        markdown_text.starts_with("## fact\\_store\\_list"),
        "default fact-store output should remain the canonical compact human view: {markdown_text}"
    );
    assert!(markdown_text.contains("complete: --json"));
    assert!(!markdown_text.contains("# Truncated Response"));
    assert!(!markdown_text.contains("LONG_FACT_MARKER_00"));

    let listed = call_production_tool(
        &fixture,
        "tracedecay_fact_store_list",
        json!({"format": "json", "category": "project", "min_trust": 0.0, "limit": 200}),
    )
    .await;
    let text = extract_text(&listed.value);
    let envelope: Value = serde_json::from_str(text).expect("large response should stay JSON");
    assert_eq!(envelope["truncated"], true);
    let original_chars = envelope["original_chars"]
        .as_u64()
        .expect("truncation envelope should report original_chars");
    assert!(
        original_chars as usize > text.chars().count(),
        "the stored canonical JSON must be larger than its wire envelope"
    );
    let handle = envelope["handle"]
        .as_str()
        .expect("large fact-store response should include retrieve handle")
        .to_string();
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let instruction = envelope["retrieve_instruction"]
        .as_str()
        .expect("large response envelope should teach retrieval");
    assert!(instruction.contains("This response was truncated"));
    assert!(instruction.contains("original response is stored locally"));
    assert!(instruction.contains("expires"));
    assert!(instruction.contains("tracedecay_retrieve"));
    assert!(instruction.contains("required argument `handle`"));
    assert!(instruction.contains(&handle));
    assert!(instruction.contains("Only call it if the missing details are needed"));

    let removed = call_production_tool(
        &fixture,
        "tracedecay_fact_store_remove",
        json!({ "format": "json", "fact_id": last_fact_id.clone() }),
    )
    .await;
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["outcome"]["value"]["payload"]["outcome"], "removed");
    let deleted = call_production_tool(
        &fixture,
        "tracedecay_fact_store_get",
        json!({ "format": "json", "fact_id": last_fact_id.clone() }),
    )
    .await;
    assert_ne!(deleted.value["isError"], true);
    let deleted: Value = serde_json::from_str(extract_text(&deleted.value)).unwrap();
    let deleted_fact = &deleted["outcome"]["value"]["payload"]["fact"];
    assert_eq!(deleted_fact["kind"], "unavailable");
    assert_eq!(deleted_fact["status"]["fact_id"], last_fact_id);
    assert_eq!(deleted_fact["status"]["payload_access"], "deleted");

    let retrieved = call_production_tool(
        &fixture,
        "tracedecay_retrieve",
        retrieve_json_arguments(&handle),
    )
    .await;
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain original JSON text");
    let full: Value = serde_json::from_str(full_json).expect("retrieved content should be JSON");
    assert_eq!(
        full["outcome"]["value"]["payload"]["facts"]
            .as_array()
            .map(Vec::len),
        Some(FACT_COUNT)
    );
    assert!(
        full_json.contains("LONG_FACT_MARKER_00"),
        "retrieved response should include the full fact list"
    );
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn grep_large_response_uses_retrievable_truncation_handle() {
    // Grep caps at 200 results and 20 hits/file, so spread markers across files.
    const EXPECTED_MATCH_COUNT: usize = 200;
    const MARKERS_PER_FILE: usize = 20;
    const FILE_COUNT: usize = 12;
    const LINE_PADDING: &str =
        "PAD_FOR_MCP_RESPONSE_TRUNCATION_HANDLE_🦀_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_";

    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        let padding = LINE_PADDING.repeat(2);
        for file_idx in 0..FILE_COUNT {
            let mut source = String::new();
            for i in 0..MARKERS_PER_FILE {
                let marker = file_idx * MARKERS_PER_FILE + i;
                let _ = writeln!(
                    source,
                    "pub fn reversible_search_marker_{marker:03}() -> &'static str {{ \"marker-{marker:03}-{padding}\" }}"
                );
            }
            fs::write(
                project.join(format!("src/large_search_{file_idx:02}.rs")),
                source,
            )
            .unwrap();
        }
    })
    .await;

    let result = call_production_tool(
        &fixture,
        "tracedecay_grep",
        json!({
            "pattern": "reversible_search_marker_",
            "max_results": EXPECTED_MATCH_COUNT,
            "context_lines": 0,
            "format": "json",
        }),
    )
    .await;
    let text = extract_text(&result.value);
    let envelope: Value = serde_json::from_str(text).expect("large grep response envelope");
    assert_eq!(
        envelope["truncated"],
        true,
        "expected MCP truncation envelope ({} chars)",
        text.len()
    );
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let handle = envelope["handle"]
        .as_str()
        .expect("large discovery response should include a handle");
    let original_chars = envelope["original_chars"]
        .as_u64()
        .expect("truncation envelope should report original_chars");
    assert!(
        original_chars as usize > text.chars().count(),
        "stored original must exceed the truncated wire payload"
    );

    let retrieved = call_production_tool(
        &fixture,
        "tracedecay_retrieve",
        retrieve_json_arguments(handle),
    )
    .await;
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain full discovery JSON");
    assert_eq!(
        full_json.chars().count() as u64,
        original_chars,
        "retrieve must restore the exact stored discovery character count"
    );
    assert_ne!(
        full_json.len(),
        full_json.chars().count(),
        "fixture must distinguish UTF-8 bytes from characters"
    );
    let full: Value =
        serde_json::from_str(full_json).expect("retrieved discovery content should be JSON");
    assert_eq!(
        full["match_count"].as_u64(),
        Some(EXPECTED_MATCH_COUNT as u64),
        "retrieved discovery response should keep the full capped hit cohort"
    );
    let results = full["results"]
        .as_array()
        .expect("retrieved discovery JSON should include results");
    assert_eq!(results.len(), EXPECTED_MATCH_COUNT);
    assert!(
        results.iter().all(|hit| {
            hit["text"]
                .as_str()
                .is_some_and(|text| text.contains("reversible_search_marker_"))
        }),
        "every retrieved hit should carry a marker line"
    );
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn context_memory_large_markdown_uses_reversible_lane_preview() {
    let fixture = production_composition_fixture().await;
    let tail = "MEMORY_TAIL_MARKER";
    let long_content = format!("Large reversible memory fact {}{tail}", "x".repeat(20_000));
    call_production_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": long_content,
            "category": "decision",
            "source_label": "mcp-context-test",
            "tags": ["context-memory-lane-preview"],
            "entities": ["large reversible memory fact"],
            "trust": 0.95
        }),
    )
    .await;

    let markdown = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({"task": "large reversible memory fact", "memory_limit": 1}),
    )
    .await;

    let text = extract_text(&markdown.value);
    assert!(text.starts_with("# Truncated Response"), "got: {text}");
    assert!(text.contains("lane-budgeted preview"), "got: {text}");
    assert!(text.contains("### Memory Matches"), "got: {text}");
    assert!(text.contains("lane truncated"), "got: {text}");
    assert!(text.contains("tracedecay_retrieve"), "got: {text}");
    assert!(!text.contains(tail), "preview should omit tail: {text}");
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn diff_context_large_response_uses_retrievable_truncation_handle() {
    const LARGE_RESPONSE_MARKER_COUNT: usize = 260;
    const LAST_LARGE_RESPONSE_MARKER: usize = LARGE_RESPONSE_MARKER_COUNT - 1;

    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        let mut source = String::new();
        for i in 0..LARGE_RESPONSE_MARKER_COUNT {
            let _ = writeln!(
                source,
                "pub fn reversible_diff_context_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}"
            );
        }
        fs::write(project.join("src/large_diff.rs"), source).unwrap();
    })
    .await;

    let result = call_production_tool(
        &fixture,
        "tracedecay_diff_context",
        json!({"format": "json", "files": ["src/large_diff.rs"], "depth": 1}),
    )
    .await;
    let envelope: Value =
        serde_json::from_str(extract_text(&result.value)).expect("large diff_context envelope");
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let handle = envelope["handle"]
        .as_str()
        .expect("large diff_context response should include a handle");

    let retrieved = call_production_tool(
        &fixture,
        "tracedecay_retrieve",
        retrieve_json_arguments(handle),
    )
    .await;
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain full diff_context JSON");
    assert!(
        full_json.contains(&format!(
            "reversible_diff_context_marker_{LAST_LARGE_RESPONSE_MARKER:03}"
        )),
        "retrieved diff_context response should include the tail result"
    );
    fixture.harness.shutdown().await;
}
