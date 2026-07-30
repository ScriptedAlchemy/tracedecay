use serde_json::{Value, json};
use tracedecay_sessions::lcm::compression_policy::{
    AssemblyCapInput, CondensationCandidateDecision, OverflowRecoveryCapInput,
    bounded_leaf_chunk_len, condensation_candidate_decision, effective_assembly_token_cap,
    effective_leaf_chunk_tokens, forced_overflow_pressure, incremental_max_depth_limit,
    overflow_recovery_assembly_cap, progress_leaf_chunk_len, threshold_pressure,
};
use tracedecay_sessions::lcm::contracts::{LcmRawMessage, LcmStorageKind};

fn raw_message(store_id: i64, role: &str, content: &str) -> LcmRawMessage {
    LcmRawMessage {
        provider: "provider".to_string(),
        message_id: format!("message-{store_id}"),
        session_id: "session".to_string(),
        store_id,
        role: role.to_string(),
        ordinal: store_id,
        timestamp: Some(store_id),
        content: content.to_string(),
        content_hash: format!("hash-{store_id}"),
        storage_kind: LcmStorageKind::Inline,
        payload_ref: None,
        legacy_source: false,
        legacy_truncated: false,
        metadata_json: None,
    }
}

fn active_replay_message(store_id: i64, replay: Value) -> LcmRawMessage {
    let role = replay["role"].as_str().expect("fixture role");
    let content = replay["content"].as_str().unwrap_or_default();
    let mut message = raw_message(store_id, role, content);
    message.metadata_json = Some(
        json!({
            "lcm_active_replay": true,
            "active_replay": replay,
        })
        .to_string(),
    );
    message
}

fn complete_tool_transaction() -> Vec<LcmRawMessage> {
    [
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": "call-a"}, {"id": "call-b"}],
        }),
        json!({"role": "tool", "content": "alpha", "tool_call_id": "call-a"}),
        json!({"role": "tool", "content": "beta", "tool_call_id": "call-b"}),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, replay)| active_replay_message((index + 1) as i64, replay))
    .collect()
}

#[test]
fn assembly_cap_uses_the_smallest_positive_authority() {
    assert_eq!(
        effective_assembly_token_cap(AssemblyCapInput {
            max_assembly_tokens: Some(200),
            context_length: Some(80),
            reserve_tokens_floor: Some(30),
        }),
        Some(50)
    );
    assert_eq!(
        effective_assembly_token_cap(AssemblyCapInput {
            max_assembly_tokens: None,
            context_length: Some(30),
            reserve_tokens_floor: Some(30),
        }),
        None
    );
}

#[test]
fn overflow_recovery_cap_reserves_non_message_overhead() {
    assert_eq!(
        overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
            current_tokens: Some(8),
            max_assembly_tokens: Some(10),
            messages: &[json!({"content": "two tokens"})],
        }),
        Some(4)
    );
    assert_eq!(
        overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
            current_tokens: Some(18),
            max_assembly_tokens: Some(10),
            messages: &[json!({"content": "tiny"})],
        }),
        Some(1)
    );
}

#[test]
fn dynamic_leaf_size_grows_by_powers_of_two_within_the_ceiling() {
    assert_eq!(
        effective_leaf_chunk_tokens(Some(4), Some(true), Some(16), 20),
        Some(16)
    );
    assert_eq!(
        effective_leaf_chunk_tokens(Some(4), Some(false), Some(16), 20),
        Some(4)
    );
}

#[test]
fn leaf_cap_can_select_zero_when_the_first_message_is_too_large() {
    let backlog = [
        raw_message(1, "assistant", "one two three"),
        raw_message(2, "assistant", "four"),
    ];

    assert_eq!(bounded_leaf_chunk_len(&backlog, Some(2), None), 0);
}

#[test]
fn atomic_tool_transactions_are_never_split_by_leaf_caps() {
    let transaction = complete_tool_transaction();

    assert_eq!(bounded_leaf_chunk_len(&transaction, Some(1), None), 0);
    assert_eq!(bounded_leaf_chunk_len(&transaction, None, Some(1)), 0);
    assert_eq!(
        progress_leaf_chunk_len(&transaction, Some(1), Some(1)),
        transaction.len()
    );
}

#[test]
fn pressure_and_condensation_limits_keep_boundary_semantics() {
    assert!(threshold_pressure(Some(10), Some(10)));
    assert!(!threshold_pressure(Some(9), Some(10)));
    assert!(forced_overflow_pressure(Some(10), Some(10)));
    assert!(!forced_overflow_pressure(Some(10), None));
    assert_eq!(incremental_max_depth_limit(None), 1);
    assert_eq!(incremental_max_depth_limit(Some(-1)), i64::MAX);
    assert_eq!(
        condensation_candidate_decision(2, 3),
        CondensationCandidateDecision::SkipNotEnoughCandidates
    );
    assert_eq!(
        condensation_candidate_decision(3, 3),
        CondensationCandidateDecision::Condense
    );
}
