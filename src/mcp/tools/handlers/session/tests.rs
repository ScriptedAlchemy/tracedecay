use super::*;
use crate::mcp::response_handles::lock_response_handle_store;

fn sample_message_search_payload() -> Value {
    json!({
        "status": "ok",
        "provider": "all",
        "query": "database backup",
        "scope": "all",
        "count": 1,
        "results": [{
            "score": 18.42,
            "session": {
                "provider": "claude",
                "session_id": "sess-abc-123",
                "title": "Investigate backup failure",
                "transcript_path": "/home/zack/.claude/projects/x/sess-abc-123.jsonl",
                "metadata_json": "{\"claude_session_cwd\":\"/home/zack/proj\",\"secret\":\"do-not-leak\"}",
                "project_path": "/home/zack/proj",
            },
            "message": {
                "provider": "claude",
                "session_id": "sess-abc-123",
                "message_id": "msg-1",
                "role": "assistant",
                "timestamp": 1_783_117_588,
                "text": "[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_x\",\"content\":\"the database backup completed successfully at 03:00 UTC\"}]",
                "source_path": "/home/zack/.claude/projects/x/sess-abc-123.jsonl",
                "source_offset": 1_676_581,
                "metadata_json": "{\"raw_type\":\"assistant\"}",
            },
        }],
    })
}

#[test]
fn message_search_markdown_drops_raw_json_blobs() {
    let payload = sample_message_search_payload();
    let md = render_message_search_md(&payload);

    // Human-facing fields are present.
    assert!(md.contains("## Transcript Search"), "{md}");
    assert!(md.contains("**query:** database backup"), "{md}");
    assert!(md.contains("**assistant**"), "{md}");
    assert!(md.contains("session `sess-abc-123`"), "{md}");
    assert!(md.contains("Investigate backup failure"), "{md}");
    assert!(md.contains("score 18.4"), "{md}");
    assert!(md.contains("t=1783117588"), "{md}");
    // The readable content is surfaced without the surrounding JSON block.
    assert!(
        md.contains("the database backup completed successfully"),
        "{md}"
    );

    // None of the raw record blobs leak into the default output.
    for forbidden in [
        "metadata_json",
        "transcript_path",
        "source_path",
        "source_offset",
        "do-not-leak",
        "tool_use_id",
        "claude_session_cwd",
    ] {
        assert!(
            !md.contains(forbidden),
            "default markdown must not embed `{forbidden}`:\n{md}"
        );
    }
    // And it must not be a JSON document.
    assert!(serde_json::from_str::<Value>(&md).is_err(), "{md}");
}

#[test]
fn message_search_markdown_handles_empty_results() {
    let payload = json!({
        "status": "ok",
        "query": "nothing matches",
        "count": 0,
        "results": [],
    });
    let md = render_message_search_md(&payload);
    assert!(md.contains("## Transcript Search"), "{md}");
    assert!(md.contains("**count:** 0"), "{md}");
    assert!(md.contains("No matching messages."), "{md}");
}

#[test]
fn message_text_snippet_extracts_readable_content_from_json() {
    let text =
        "[{\"type\":\"tool_result\",\"content\":\"hello world\",\"tool_use_id\":\"toolu_1\"}]";
    let snippet = message_text_snippet(text, 240);
    assert_eq!(snippet, "hello world");
    assert!(!snippet.contains("tool_use_id"));
}

#[test]
fn message_text_snippet_falls_back_to_raw_and_truncates() {
    let text = "x".repeat(500);
    let snippet = message_text_snippet(&text, 240);
    assert!(snippet.ends_with('…'));
    assert_eq!(snippet.chars().count(), 241); // 240 chars + ellipsis
}

#[test]
fn message_text_snippet_plain_text_is_collapsed() {
    let text = "line one\n\n   line two\ttabbed";
    assert_eq!(message_text_snippet(text, 240), "line one line two tabbed");
}

#[test]
fn lcm_preflight_markdown_truncation_stores_retrieval_handle() {
    let _store_guard = lock_response_handle_store();
    // Regression: the markdown-default preflight path must thread the
    // project root so an oversized payload truncates *with* a recoverable
    // handle rather than an irreversible clip.
    let dir = tempfile::TempDir::new().unwrap();
    // Oversize the payload the way a real preflight does — via a large
    // replay_messages array (what the compaction tiers actually target).
    let replay: Vec<Value> = (0..200)
        .map(|i| json!({"role": "user", "content": format!("message {i} {}", "y".repeat(200))}))
        .collect();
    let payload = json!({
        "status": "ok",
        "provider": "claude",
        "session_id": "s1",
        "should_compress": false,
        "reason": "no_compression_needed",
        "replay_messages": replay,
    });

    // Markdown default (no `format` arg): must produce the readable
    // truncation envelope with a stored handle.
    let result = lcm_preflight_tool_json(Some(dir.path()), &json!({}), &payload);
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("# Truncated Response"), "{text}");
    assert!(text.contains("Full response stored locally"), "{text}");
    assert!(text.contains("tracedecay_retrieve"), "{text}");
    assert!(
        serde_json::from_str::<Value>(text).is_err(),
        "markdown truncation must not be a JSON envelope: {text}"
    );

    // `format:"json"` still yields the compact Hermes bridge contract.
    let json_result =
        lcm_preflight_tool_json(Some(dir.path()), &json!({"format": "json"}), &payload);
    let json_text = json_result.value["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(json_text).unwrap();
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["should_compress"], false);
}

/// Builds an expand-query payload that overflows even the `Minimal`
/// compaction tier: `Minimal` clones `context_pagination` items whole (up
/// to 10) and `matches` metadata fields verbatim, so oversized entries
/// there survive both compaction passes and force the bounded floor.
fn oversized_needs_synthesis_expand_query_payload() -> Value {
    let context_blocks: Vec<Value> = (0..60)
        .map(|i| {
            json!({
                "kind": "raw_message",
                "node_id": format!("node-{i}"),
                "content": "c".repeat(2_000),
            })
        })
        .collect();
    let matches: Vec<Value> = (0..40)
        .map(|i| {
            json!({
                "kind": "match",
                "node_id": format!("match-{i}-{}", "m".repeat(1_500)),
                "snippet": "s".repeat(600),
            })
        })
        .collect();
    let context_pagination: Vec<Value> = (0..10)
        .map(|i| json!({ "cursor": format!("{i}-{}", "p".repeat(4_000)) }))
        .collect();
    json!({
        "status": "ok",
        "provider": "claude",
        "session_id": "s1",
        "storage_scope": "project",
        "needs_synthesis": true,
        "prompt": "What changed in the auth flow?",
        "context_blocks": context_blocks,
        "matches": matches,
        "node_ids": (0..30).map(|i| format!("n{i}")).collect::<Vec<_>>(),
        "context_pagination": context_pagination,
    })
}

#[test]
fn lcm_expand_query_needs_synthesis_floor_is_bounded_valid_json() {
    let _store_guard = lock_response_handle_store();
    // Regression (S3): a needs_synthesis payload that is still over budget
    // after Minimal compaction must NOT be emitted unbounded. The floor
    // must stay within MAX_RESPONSE_CHARS, remain valid JSON, and keep the
    // Hermes synthesis contract keys — with a retrieval handle when a
    // project root is available.
    let dir = tempfile::TempDir::new().unwrap();
    let payload = oversized_needs_synthesis_expand_query_payload();

    // Sanity: this payload really does defeat both compaction tiers.
    let minimal = compact_lcm_expand_query_payload(
        &payload,
        serde_json::to_string(&payload).unwrap().len(),
        CompactTier::Minimal { compact_chars: 0 },
    );
    assert!(
        serde_json::to_string(&minimal).unwrap().len() > MAX_RESPONSE_CHARS,
        "test payload must overflow the Minimal tier to exercise the floor"
    );

    let result = lcm_expand_query_tool_json(Some(dir.path()), &json!({"format": "json"}), &payload);
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert!(
        text.len() <= MAX_RESPONSE_CHARS,
        "floor must bound the response: {} chars",
        text.len()
    );
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["needs_synthesis"], true);
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["mcp_response_truncated"], true);
    assert_eq!(parsed["contract_truncated"], true);
    // The synthesis contract survives: the bridge can still synthesize.
    assert!(parsed["synthesis_prompt"]["user"].as_str().is_some());
    assert!(parsed["synthesis_prompt"]["system"].as_str().is_some());
    // Unbounded arrays are dropped but flagged.
    assert_eq!(parsed["context_blocks"], json!([]));
    assert_eq!(parsed["context_blocks_truncated_for_mcp"], true);
    assert_eq!(parsed["matches_truncated_for_mcp"], true);
    // Nothing is lost: the full payload is stored behind a handle.
    let handle = parsed["response_handle"].as_str().unwrap();
    assert!(handle.starts_with("rh_"), "{handle}");
    assert_eq!(parsed["retrieve_tool"], "tracedecay_retrieve");
}

#[test]
fn lcm_expand_query_needs_synthesis_floor_is_bounded_without_project_root() {
    // Even when no project root is available (no handle storage), the
    // floor must still emit bounded, contract-preserving JSON.
    let payload = oversized_needs_synthesis_expand_query_payload();
    let result = lcm_expand_query_tool_json(None, &json!({"format": "json"}), &payload);
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert!(
        text.len() <= MAX_RESPONSE_CHARS,
        "floor must bound the response: {} chars",
        text.len()
    );
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["needs_synthesis"], true);
    assert_eq!(parsed["mcp_response_truncated"], true);
    assert!(parsed.get("response_handle").is_none());
}

#[test]
fn lcm_expand_query_in_budget_and_synthesis_compaction_paths_unchanged() {
    // In-budget payloads pass through verbatim.
    let small = json!({
        "status": "ok",
        "needs_synthesis": true,
        "prompt": "q",
        "context_blocks": [],
    });
    let result = lcm_expand_query_tool_json(None, &json!({"format": "json"}), &small);
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        small,
        "in-budget payload must be emitted verbatim"
    );

    // Oversized-but-compactable synthesis payloads still use the tiers
    // (no floor markers, no handle keys).
    let blocks: Vec<Value> = (0..40)
            .map(|i| json!({"kind": "raw_message", "node_id": format!("n{i}"), "content": "c".repeat(1_000)}))
            .collect();
    let compactable = json!({
        "status": "ok",
        "needs_synthesis": true,
        "prompt": "q",
        "context_blocks": blocks,
    });
    let result = lcm_expand_query_tool_json(None, &json!({"format": "json"}), &compactable);
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert!(text.len() <= MAX_RESPONSE_CHARS);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["needs_synthesis"], true);
    assert!(
        parsed.get("response_handle").is_none(),
        "tier compaction must not reach the handle-storing floor"
    );
    assert!(
        !parsed["context_blocks"].as_array().unwrap().is_empty(),
        "tier compaction keeps bounded context blocks"
    );
}
