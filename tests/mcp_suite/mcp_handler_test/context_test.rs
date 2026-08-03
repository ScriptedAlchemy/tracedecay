use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use tracedecay::tracedecay::TraceDecay;

#[tokio::test]
async fn test_context_appends_index_coverage_hint_for_skipped_generated_dirs() {
    let (cg, _env, _dir) = setup_generated_dir_project(false).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "generatedOnly", "max_nodes": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("### Index Coverage Hint"),
        "context miss should include coverage hint, got: {text}"
    );
    assert!(
        text.contains("tracedecay sync --include-folder dist"),
        "hint should include opt-in command, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// 2. tracedecay_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "understand the helper function"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

#[tokio::test]
async fn context_includes_matching_memory_facts() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Helper function reviews should check durable memory before broad file search.",
            "category": "decision",
            "entity": "helper function",
            "tags": ["context", "memory"],
            "trust": 0.91,
            "source": "mcp-context-test"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    let before_context = cg.get_fact(fact_id).await.unwrap().unwrap();

    let markdown_result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "helper function durable memory review"}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown = extract_text(&markdown_result.value);
    assert!(markdown.contains("### Memory Matches"));
    assert!(markdown.contains(&format!("fact_id={fact_id}")));
    assert!(markdown.contains("Helper function reviews should check durable memory"));

    let json_result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "helper function durable memory review", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert!(
        payload.get("context_memory_analytics").is_none(),
        "internal context analytics must not be serialized in direct tool payloads"
    );
    assert!(
        json_result.internal_analytics().is_some(),
        "direct tool results should carry context analytics on the internal side channel"
    );
    assert!(payload["memory_matches"].as_array().is_some_and(|matches| {
        matches
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_i64() == Some(fact_id))
    }));

    let after_context = cg.get_fact(fact_id).await.unwrap().unwrap();
    assert_eq!(
        after_context.retrieval_count, before_context.retrieval_count,
        "context memory enrichment should not count as an explicit memory retrieval"
    );
    assert_eq!(
        after_context.access_count, before_context.access_count,
        "context memory enrichment should not count as an explicit memory recall"
    );
}

#[tokio::test]
async fn context_memory_controls_filter_disable_and_preserve_markdown() {
    let (cg, _dir) = setup_project().await;
    let long_content = format!("Long memory control fact {}", "x".repeat(320));
    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": long_content,
            "category": "decision",
            "entity": "long memory control",
            "tags": ["context-memory-controls"],
            "trust": 0.92,
            "source": "mcp-context-test"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Low trust memory control fact should stay filtered.",
            "category": "decision",
            "entity": "low trust memory control",
            "tags": ["context-memory-controls"],
            "trust": 0.2,
            "source": "mcp-context-test"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let disabled = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({
            "task": "long memory control fact",
            "format": "json",
            "include_memory": false
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let disabled_payload: Value = serde_json::from_str(extract_text(&disabled.value)).unwrap();
    assert_eq!(
        disabled_payload["memory_matches"].as_array().map(Vec::len),
        Some(0)
    );

    let filtered = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({
            "task": "low trust memory control fact",
            "format": "json",
            "memory_min_trust": 0.9
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let filtered_payload: Value = serde_json::from_str(extract_text(&filtered.value)).unwrap();
    assert!(
        !filtered_payload["memory_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["content"]
                .as_str()
                .is_some_and(|content| content.contains("Low trust memory control")))
    );

    let markdown = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "long memory control fact", "memory_limit": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&markdown.value);
    assert!(text.contains("Long memory control fact"));
    assert!(text.contains(&"x".repeat(300)));
    assert!(!text.contains("..."));
}

// ---------------------------------------------------------------------------
// Extra: missing required params for other handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context_missing_task() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_context", json!({}), None, None).await;
    assert!(result.is_err(), "context without task should error");
}

#[tokio::test]
async fn test_context_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // Context scoped to "tests" should return results (even if limited to test files)
    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "understand helper"}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.is_empty(),
        "context should return results even when scoped"
    );
}
