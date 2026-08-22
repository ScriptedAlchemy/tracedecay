use super::*;
use crate::mcp::response_handles::lock_response_handle_store;
use crate::sessions::{SessionMessageRecord, SessionRecord};

#[test]
fn message_search_time_range_ignores_null_optional_bounds() {
    let range = message_search_time_range(&serde_json::json!({
        "since": "2026-08-20",
        "until": null,
    }))
    .expect("null optional bounds should be treated as absent");

    assert!(range.start_time.is_some());
    assert_eq!(range.end_time, None);
}

#[tokio::test]
async fn identical_message_catch_ups_share_one_leader() {
    let key = format!("test-singleflight-{}", std::process::id());
    let leader = match claim_message_catch_up(key.clone()) {
        MessageCatchUpClaim::Leader(leader) => leader,
        MessageCatchUpClaim::Wait(_) => panic!("fresh key unexpectedly had a leader"),
    };
    let waiter = match claim_message_catch_up(key.clone()) {
        MessageCatchUpClaim::Wait(waiter) => waiter,
        MessageCatchUpClaim::Leader(_) => panic!("identical catch-up did not coalesce"),
    };
    drop(leader);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        wait_for_message_catch_up(waiter),
    )
    .await
    .expect("waiter was not released when leader completed");
    assert!(matches!(
        claim_message_catch_up(key),
        MessageCatchUpClaim::Leader(_)
    ));
}

/// Build a search hit with a given relevance score, message timestamp, and
/// stable ids — enough to exercise the cross-project merge ordering contract.
fn merge_result(
    score: f64,
    timestamp: i64,
    session_id: &str,
    message_id: &str,
) -> SessionMessageSearchResult {
    SessionMessageSearchResult {
        session: SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: "proj".to_string(),
            project_path: "proj".to_string(),
            title: None,
            started_at: None,
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        },
        message: SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(timestamp),
            ordinal: 0,
            text: String::new(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        score,
    }
}

/// Regression: the cross-project merge must produce an *exact* distributed
/// top-K. Each shard is truncated in SQL by BM25 relevance, so a shard returns
/// its own top rows by score. A recency merge (the old behavior) would keep a
/// low-relevance-but-newer row and drop a genuinely top-relevance row a shard
/// returned — corrupting the top-K. The merge must order by score DESC.
#[test]
fn cross_project_merge_keeps_top_relevance_not_newest() {
    // Two shards, each already truncated by relevance:
    //   shard A: score 10 @ t=100, score 3 @ t=90
    //   shard B: score 9  @ t=50,  score 8 @ t=40
    let mut results = vec![
        merge_result(10.0, 100, "a", "a-hi"),
        merge_result(3.0, 90, "a", "a-lo"),
        merge_result(9.0, 50, "b", "b-hi"),
        merge_result(8.0, 40, "b", "b-mid"),
    ];
    sort_and_truncate_message_results_by_relevance(&mut results, 3);

    let ids: Vec<&str> = results
        .iter()
        .map(|r| r.message.message_id.as_str())
        .collect();
    // Exact relevance top-3: 10, 9, 8. The newer-but-low-relevance row
    // (score 3 @ t=90) must be dropped, and the top-relevance row that a
    // recency merge would have discarded (score 8) must survive.
    assert_eq!(ids, ["a-hi", "b-hi", "b-mid"], "top-K must be by relevance");
    assert!(
        !ids.contains(&"a-lo"),
        "recency must not resurrect a low-relevance row: {ids:?}"
    );
}

/// Equal-score rows must order deterministically: timestamp DESC, then a
/// stable session-id / message-id tie-break, matching the per-shard key.
#[test]
fn cross_project_merge_tie_break_is_stable() {
    let mut results = vec![
        merge_result(5.0, 100, "z", "z-1"),
        merge_result(5.0, 100, "a", "a-2"),
        merge_result(5.0, 100, "a", "a-1"),
        merge_result(5.0, 200, "m", "m-1"),
    ];
    sort_and_truncate_message_results_by_relevance(&mut results, 10);
    let ids: Vec<&str> = results
        .iter()
        .map(|r| r.message.message_id.as_str())
        .collect();
    // Newest timestamp first (m-1 @ 200), then equal ts=100 ordered by
    // session_id then message_id: (a,a-1), (a,a-2), (z,z-1).
    assert_eq!(ids, ["m-1", "a-1", "a-2", "z-1"], "{ids:?}");
}

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
fn message_search_markdown_surfaces_project_scope_counts() {
    let mut payload = sample_message_search_payload();
    if let Some(map) = payload.as_object_mut() {
        map.insert("project_scope".to_string(), json!("all_registered"));
        map.insert("searched_project_count".to_string(), json!(3));
        map.insert("skipped_project_count".to_string(), json!(2));
    }
    let md = render_message_search_md(&payload);
    assert!(
        md.contains("**project scope:** all_registered (searched 3, skipped 2)"),
        "cross-project scope/counts must appear in default markdown:\n{md}"
    );

    // Single-project searches (no project_scope) must not grow the line.
    let plain = render_message_search_md(&sample_message_search_payload());
    assert!(
        !plain.contains("project scope"),
        "project scope line must be omitted when unscoped:\n{plain}"
    );
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
fn message_search_markdown_renders_goals_view_with_status() {
    let payload = json!({
        "status": "ok",
        "goals": true,
        "count": 1,
        "results": [{
            "score": 0.0,
            "session": {"provider": "codex", "session_id": "sess-goal-1", "title": "Goal work"},
            "message": {
                "provider": "codex",
                "role": "system",
                "kind": "goal",
                "timestamp": 1_783_500_100,
                "text": "phlogiston pipeline overhaul",
                "metadata_json": "{\"source\":\"codex_thread_goal\",\"status\":\"paused\"}",
            },
        }],
    });
    let md = render_message_search_md(&payload);
    assert!(md.contains("## Session Goals"), "{md}");
    assert!(
        md.contains("**mode:** goals (latest goal per session)"),
        "{md}"
    );
    assert!(md.contains("session `sess-goal-1`"), "{md}");
    assert!(md.contains("goal [paused]"), "{md}");
    assert!(md.contains("phlogiston pipeline overhaul"), "{md}");
    // Raw metadata blob must not leak.
    assert!(!md.contains("metadata_json"), "{md}");
}

#[test]
fn message_search_markdown_goals_view_handles_empty() {
    let payload = json!({
        "status": "ok",
        "goals": true,
        "count": 0,
        "results": [],
    });
    let md = render_message_search_md(&payload);
    assert!(md.contains("## Session Goals"), "{md}");
    assert!(md.contains("No goals recorded for this project."), "{md}");
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

/// Deterministic backend stand-in for the expand-query synthesis path. Returns
/// canned output, or a permanent (non-retryable) failure to exercise fallback.
struct FakeSynthesisBackend {
    output: String,
    fail: bool,
}

impl crate::automation::backend::AgentTaskBackend for FakeSynthesisBackend {
    fn run_task(
        &self,
        request: &crate::automation::backend::AgentTaskRequest,
    ) -> crate::errors::Result<crate::automation::backend::AgentTaskResponse> {
        if self.fail {
            return Err(crate::errors::TraceDecayError::Config {
                message: "synthesis backend permanently unavailable".to_string(),
            });
        }
        Ok(crate::automation::backend::AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.clone(),
            output_json: None,
            model: None,
            input_tokens: None,
            output_tokens: None,
        })
    }
}

fn expand_query_response_needing_synthesis() -> crate::sessions::lcm::LcmExpandQueryResponse {
    use crate::sessions::lcm::{
        LcmContentRange, LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryResponse,
        LcmExpandQuerySynthesisPrompt,
    };
    LcmExpandQueryResponse {
        answer: None,
        needs_synthesis: true,
        prompt: "What did we decide?".to_string(),
        query: Some("decision".to_string()),
        synthesis_prompt: Some(LcmExpandQuerySynthesisPrompt {
            system: "You synthesize answers from LCM context.".to_string(),
            user: "QUESTION:\nWhat did we decide?\n\nEXPANDED CONTEXT:\n[block]".to_string(),
        }),
        max_tokens: 512,
        context_max_tokens: 4096,
        context_budget: LcmExpandQueryBudget {
            requested_max_chars: 4096,
            used_chars: 18,
        },
        context_truncated: false,
        context_pagination: Vec::new(),
        node_ids: vec!["node-1".to_string()],
        matches: Vec::new(),
        context_blocks: vec![LcmExpandQueryContextBlock {
            kind: "raw_message".to_string(),
            node_id: Some("node-1".to_string()),
            source_ref: None,
            content: "we decided to ship".to_string(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 100,
                returned_chars: 18,
                total_chars: 18,
                truncated: false,
            },
            raw_message: None,
            summary_node: None,
        }],
    }
}

#[tokio::test]
async fn synthesize_expand_query_answer_populates_answer_with_backend() {
    let mut response = expand_query_response_needing_synthesis();
    let backend = FakeSynthesisBackend {
        output: "  We decided to ship the ranker change.  ".to_string(),
        fail: false,
    };
    let policy = crate::automation::backend::BackendRetryPolicy::new(
        1,
        Vec::new(),
        std::time::Duration::from_secs(30),
    );

    let synthesized = synthesize_expand_query_answer(&mut response, &backend, &policy).await;

    assert!(
        synthesized,
        "backend output should be synthesized into an answer"
    );
    assert_eq!(
        response.answer.as_deref(),
        Some("We decided to ship the ranker change.")
    );
    assert!(
        !response.needs_synthesis,
        "needs_synthesis must be cleared once an answer is synthesized"
    );
}

#[tokio::test]
async fn synthesize_expand_query_answer_falls_back_when_backend_fails() {
    let mut response = expand_query_response_needing_synthesis();
    let backend = FakeSynthesisBackend {
        output: String::new(),
        fail: true,
    };
    let policy = crate::automation::backend::BackendRetryPolicy::new(
        1,
        Vec::new(),
        std::time::Duration::from_secs(30),
    );

    let synthesized = synthesize_expand_query_answer(&mut response, &backend, &policy).await;

    assert!(
        !synthesized,
        "a failed backend must not fabricate an answer"
    );
    assert!(response.answer.is_none());
    assert!(
        response.needs_synthesis,
        "needs_synthesis stays true so the host can synthesize from raw context"
    );
}
