use crate::support::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;

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

    let stored_payload: Value = serde_json::from_str(
        &fs::read_to_string(
            stored
                .response_handle_root
                .join(format!("{}.json", stored.handle)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(stored_payload.get("handle").is_none());
    assert!(stored_payload.get("original_chars").is_none());
    assert_eq!(stored_payload["content"], original);

    let result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": stored.handle }),
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
        json!({ "retrieve_handle": stored.handle }),
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
        json!({ "handle": "rh_0123456789abcdef01234567" }),
        None,
        None,
    )
    .await
    .unwrap();
    let missing_payload: Value = serde_json::from_str(extract_text(&missing.value)).unwrap();
    assert_eq!(missing_payload["expired"], true);
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
        json!({ "handle": expired.handle }),
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
}

#[tokio::test]
async fn fact_store_large_list_response_uses_retrieve_handle() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let mut last_fact_id = None;
    for index in 0..4 {
        let added = handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "format": "json",
                "content": format!(
                    "LONG_FACT_MARKER_{index:02}: {}",
                    "large fact-store response should remain retrievable ".repeat(180)
                ),
                "category": "project",
                "trust": 0.9
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
        last_fact_id = added["fact"]["fact_id"].as_i64();
    }
    let last_fact_id = last_fact_id.expect("tail fact id");

    let markdown_list = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "project", "min_trust": 0.0, "limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown_text = extract_text(&markdown_list.value);
    assert!(
        markdown_text.starts_with("# Truncated Response"),
        "large default fact-store response should use Markdown truncation: {markdown_text}"
    );
    assert!(markdown_text.contains("## Preview"));
    assert!(markdown_text.contains("## Fact Store"));
    assert!(markdown_text.contains("### Facts"));
    assert!(!markdown_text.contains("| fact_id |"));
    let markdown_handle = markdown_text
        .split_once("using handle `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(handle, _)| handle.to_string())
        .expect("Markdown truncation should expose a retrieve handle");
    let markdown_retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": markdown_handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown_retrieved_payload: Value =
        serde_json::from_str(extract_text(&markdown_retrieved.value)).unwrap();
    let full_markdown = markdown_retrieved_payload["content"]
        .as_str()
        .expect("retrieved Markdown response should contain text");
    assert!(full_markdown.starts_with("## Fact Store"));
    assert!(full_markdown.contains("LONG_FACT_MARKER_00"));
    assert!(!full_markdown.contains("| fact_id |"));

    let listed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "format": "json", "category": "project", "min_trust": 0.0, "limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&listed.value);
    let envelope: Value = serde_json::from_str(text).expect("large response should stay JSON");
    assert_eq!(envelope["truncated"], true);
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

    let removed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({ "action": "remove", "format": "json", "fact_id": last_fact_id }),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
    assert!(
        cg.get_fact(last_fact_id).await.unwrap().is_none(),
        "tail fact should be absent from the live store before handle retrieval"
    );

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain original JSON text");
    let full: Value = serde_json::from_str(full_json).expect("retrieved content should be JSON");
    assert_eq!(full["count"].as_u64(), Some(4));
    assert!(
        full_json.contains("LONG_FACT_MARKER_00"),
        "retrieved response should include the full fact list"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn search_large_response_uses_retrievable_truncation_handle() {
    // Code-index `tracedecay_search` at high limits currently fails closed as
    // typed `search_failed` (Internal) after a generation binds; the MCP
    // truncation/retrieve envelope is shared across discovery tools, so this
    // contract is exercised through grep until that search failure is fixed.
    // Grep caps at 200 results and 20 hits/file — spread markers across files.
    const EXPECTED_MATCH_COUNT: usize = 200;
    const MARKERS_PER_FILE: usize = 20;
    const FILE_COUNT: usize = 12;
    const LINE_PADDING: &str =
        "PAD_FOR_MCP_RESPONSE_TRUNCATION_HANDLE_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_";

    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let (cg, _env) = init_test_project(project).await;
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

    let result = handle_tool_call(
        &cg,
        "tracedecay_grep",
        json!({
            "pattern": "reversible_search_marker_",
            "max_results": EXPECTED_MATCH_COUNT,
            "context_lines": 0,
            "format": "json",
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let envelope: Value =
        serde_json::from_str(text).expect("large grep response envelope");
    assert_eq!(
        envelope["truncated"], true,
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
        original_chars as usize > text.len(),
        "stored original must exceed the truncated wire payload"
    );

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain full discovery JSON");
    assert_eq!(
        full_json.len() as u64,
        original_chars,
        "retrieve must restore the exact stored discovery payload"
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
}

#[tokio::test]
async fn context_memory_large_markdown_uses_reversible_lane_preview() {
    let (cg, _dir) = setup_project().await;
    let tail = "MEMORY_TAIL_MARKER";
    let long_content = format!("Large reversible memory fact {}{tail}", "x".repeat(20_000));
    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": long_content,
            "category": "decision",
            "entity": "large reversible memory fact",
            "tags": ["context-memory-lane-preview"],
            "trust": 0.95,
            "source": "mcp-context-test"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let markdown = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "large reversible memory fact", "memory_limit": 1}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&markdown.value);
    assert!(text.starts_with("# Truncated Response"), "got: {text}");
    assert!(text.contains("lane-budgeted preview"), "got: {text}");
    assert!(text.contains("### Memory Matches"), "got: {text}");
    assert!(text.contains("lane truncated"), "got: {text}");
    assert!(text.contains("tracedecay_retrieve"), "got: {text}");
    assert!(!text.contains(tail), "preview should omit tail: {text}");
}

#[tokio::test]
async fn diff_context_large_response_uses_retrievable_truncation_handle() {
    const LARGE_RESPONSE_MARKER_COUNT: usize = 260;
    const LAST_LARGE_RESPONSE_MARKER: usize = LARGE_RESPONSE_MARKER_COUNT - 1;

    let dir = test_temp_dir();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let (cg, _env) = init_test_project(project).await;
    let mut source = String::new();
    for i in 0..LARGE_RESPONSE_MARKER_COUNT {
        let _ = writeln!(
            source,
            "pub fn reversible_diff_context_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}"
        );
    }
    fs::write(project.join("src/large_diff.rs"), source).unwrap();
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/large_diff.rs"], "depth": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value =
        serde_json::from_str(extract_text(&result.value)).expect("large diff_context envelope");
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let handle = envelope["handle"]
        .as_str()
        .expect("large diff_context response should include a handle");

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
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
}
