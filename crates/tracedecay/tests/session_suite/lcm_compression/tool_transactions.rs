use super::*;

#[tokio::test]
async fn fresh_tail_boundary_keeps_multi_tool_transaction_atomic_and_shrinking() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-atomic-tail").await;

    let mut messages = vec![json!({
        "id": "old-user",
        "role": "user",
        "content": "old backlog words ".repeat(60),
    })];
    messages.extend(active_multi_tool_transaction());
    messages.push(json!({"id": "fresh-user", "role": "user", "content": "fresh prompt"}));
    let mut request = limited_compress_request(
        "cursor",
        "session-atomic-tail",
        LcmSummarizerMode::Fake {
            summary_text: "compact summary".into(),
        },
        None,
        None,
        Some(400),
    );
    request.messages = with_authoritative_timestamps(messages);
    request.current_tokens = Some(107);
    request.threshold_tokens = Some(80);
    request.fresh_tail_count = Some(3);

    let response = db.lcm_compress(request).await.unwrap();
    let assistant = response
        .replay_messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap_or_else(|| {
            panic!(
                "assistant tool call missing: {:?}",
                response.replay_messages
            )
        });
    assert_eq!(assistant["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| message["tool_call_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["call-a", "call-b"]
    );
    assert!(response.replay_token_estimate < 107);
}

#[tokio::test]
async fn bounded_leaf_chunk_backs_off_before_multi_tool_transaction() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-atomic-leaf").await;

    let mut messages = vec![json!({
        "id": "old-user",
        "role": "user",
        "content": "old backlog words ".repeat(30),
    })];
    messages.extend(active_multi_tool_transaction());
    messages.push(json!({"id": "fresh-user", "role": "user", "content": "fresh prompt"}));
    let mut request = limited_compress_request(
        "cursor",
        "session-atomic-leaf",
        LcmSummarizerMode::Fake {
            summary_text: "transaction summary".into(),
        },
        None,
        Some(2),
        None,
    );
    request.messages = with_authoritative_timestamps(messages);
    request.fresh_tail_count = Some(1);

    let response = db.lcm_compress(request).await.unwrap();
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.summary_nodes[0].source_refs.len(), 1);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        2
    );
}

#[tokio::test]
async fn budget_and_overflow_replay_never_split_tool_transaction() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-atomic-budget").await;

    let mut messages = vec![json!({
        "id": "old-user",
        "role": "user",
        "content": "old backlog words ".repeat(30),
    })];
    messages.extend(active_multi_tool_transaction());
    let mut request = limited_compress_request(
        "cursor",
        "session-atomic-budget",
        LcmSummarizerMode::Fake {
            summary_text: "summary".into(),
        },
        None,
        None,
        Some(8),
    );
    request.messages = with_authoritative_timestamps(messages);
    request.current_tokens = Some(107);
    request.fresh_tail_count = Some(3);

    let response = db.lcm_compress(request).await.unwrap();
    let assistant_count = response
        .replay_messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .count();
    let tool_count = response
        .replay_messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .count();
    assert!(
        (assistant_count == 0 && tool_count == 0) || (assistant_count == 1 && tool_count == 2),
        "budget assembly must keep or drop the transaction as a unit: {:?}",
        response.replay_messages
    );
}

#[tokio::test]
async fn legacy_partial_tool_transaction_drops_invalid_group_and_repairs_orphans() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-legacy-tools").await;

    let mut transaction = active_multi_tool_transaction();
    transaction.remove(2);
    transaction.push(json!({
        "id": "separator",
        "role": "user",
        "content": "new prompt",
    }));
    transaction.push(json!({
        "id": "orphan-b",
        "role": "tool",
        "tool_call_id": "call-b",
        "content": "late orphan",
    }));
    transaction.push(json!({
        "id": "unmatched-assistant",
        "role": "assistant",
        "content": "visible assistant text",
        "tool_calls": [{
            "id": "never-returned",
            "type": "function",
            "function": {"name": "missing", "arguments": "{}"},
        }],
    }));
    transaction.push(json!({
        "id": "after-unmatched-call",
        "role": "user",
        "content": "the unmatched call is now a closed legacy group",
    }));
    let mut request = compress_request(
        "cursor",
        "session-legacy-tools",
        LcmSummarizerMode::Fake {
            summary_text: "unused".into(),
        },
    );
    request.messages = transaction;
    request.fresh_tail_count = Some(10);

    let response = db.lcm_compress(request).await.unwrap();
    assert!(
        response
            .replay_messages
            .iter()
            .all(|message| message["id"] != "assistant-tools"),
        "an incomplete multi-call assistant group must not replay"
    );
    assert!(
        response
            .replay_messages
            .iter()
            .all(|message| message["role"] != "tool"),
        "orphan tool results must not replay"
    );
    let repaired_assistant = response
        .replay_messages
        .iter()
        .find(|message| message["id"] == "unmatched-assistant")
        .expect("visible unmatched assistant text should remain");
    assert!(repaired_assistant.get("tool_calls").is_none());
}
