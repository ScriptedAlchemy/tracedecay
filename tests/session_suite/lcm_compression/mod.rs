use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_sessions::runtime::lcm::compression_decision::{
    AssemblyCapInput, CompressionPlanInput, OverflowRecoveryCapInput, PreflightDecisionInput,
    compression_plan, effective_assembly_token_cap, overflow_recovery_assembly_cap,
    preflight_decision,
};
use tracedecay_sessions::runtime::lcm::{
    LcmCompressionRequest, LcmGrepRequest, LcmGrepSort, LcmLifecycleState, LcmLifecycleUpdate,
    LcmLoadSessionRequest, LcmMaintenanceDebt, LcmPreflightRequest, LcmRawMessage, LcmScope,
    LcmSessionBoundaryRequest, LcmSourceRef, LcmStorageKind, LcmSummarizerMode,
    LcmSummaryNodeDraft, MAX_DERIVED_SNIPPET_CHARS,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

use crate::common::{self, LcmTestRuntime, open_lcm_db};

async fn open_registered_lcm_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered LCM test runtime")
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    common::session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM compression test",
        None,
        None,
    )
}

fn raw_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    raw_message_with_role(provider, message_id, session_id, "assistant", ordinal, text)
}

fn raw_message_with_role(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    common::MessageRecordBuilder::new(
        provider, message_id, session_id, role, ordinal, text, "message",
    )
    .with_timestamp(Some(1_715_000_000 + ordinal))
    .build()
}

async fn insert_session(db: &LcmTestRuntime, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

fn externalized_ref_from_placeholder(text: &str) -> String {
    let marker = "ref=";
    let start = text.find(marker).expect("placeholder ref") + marker.len();
    let tail = &text[start..];
    let end = tail.find([']', ',', ';']).unwrap_or(tail.len());
    tail[..end].trim().to_string()
}

async fn insert_raw_messages(
    db: &LcmTestRuntime,
    provider: &str,
    session_id: &str,
    contents: &[&str],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, content) in contents.iter().enumerate() {
        let message_slug = content.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
        let message_id = format!("{session_id}-message-{}-{message_slug}", idx + 1);
        let message = raw_message(provider, &message_id, session_id, (idx + 1) as i64, content);
        assert!(db.upsert_session_message(&message).await);
        let raw = db
            .lcm_load_raw_message(provider, &message_id)
            .await
            .expect("raw message should exist");
        store_ids.push(raw.store_id);
    }
    store_ids
}

async fn insert_registered_raw_messages(
    runtime: &HostAdmissionTestRuntimeV1,
    provider: &str,
    session_id: &str,
    contents: &[&str],
) -> Vec<i64> {
    let session = sample_session(provider, session_id);
    let messages = contents
        .iter()
        .enumerate()
        .map(|(idx, content)| {
            let message_slug = content.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
            let message_id = format!("{session_id}-message-{}-{message_slug}", idx + 1);
            raw_message(provider, &message_id, session_id, (idx + 1) as i64, content)
        })
        .collect::<Vec<_>>();
    runtime
        .upsert_transcript_batch_for_test(
            HostAdmissionScope::Profile,
            &session,
            &messages,
            "lcm-compression-test-fixture",
            tracedecay::global_db::ParseOffset::default(),
        )
        .await
        .expect("registered raw message fixture")
}

async fn insert_raw_messages_with_roles(
    db: &LcmTestRuntime,
    provider: &str,
    session_id: &str,
    messages: &[(&str, &str)],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, (role, content)) in messages.iter().enumerate() {
        let message_slug = content.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
        let message_id = format!("{session_id}-message-{}-{message_slug}", idx + 1);
        let message = raw_message_with_role(
            provider,
            &message_id,
            session_id,
            role,
            (idx + 1) as i64,
            content,
        );
        assert!(db.upsert_session_message(&message).await);
        let raw = db
            .lcm_load_raw_message(provider, &message_id)
            .await
            .expect("raw message should exist");
        store_ids.push(raw.store_id);
    }
    store_ids
}

fn preflight_request(
    provider: &str,
    session_id: &str,
    messages: Vec<Value>,
    current_tokens: Option<i64>,
) -> LcmPreflightRequest {
    LcmPreflightRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages,
        current_tokens,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
    }
}

/// Ingests host-active messages through the daemon compression path without
/// creating summary nodes — the production replacement for the retired
/// ingesting preflight (preflight is a read-only decision surface now).
async fn ingest_active_messages(
    db: &LcmTestRuntime,
    provider: &str,
    session_id: &str,
    messages: Vec<Value>,
) -> tracedecay_sessions::runtime::lcm::LcmCompressionResponse {
    let message_count = messages.len();
    let mut request = compress_request(provider, session_id, LcmSummarizerMode::Noop);
    request.messages = messages;
    request.fresh_tail_count = Some(message_count.max(1));
    let response = db
        .lcm_compress(request)
        .await
        .expect("ingest-only compress");
    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 0);
    response
}

fn compress_request(
    provider: &str,
    session_id: &str,
    summarizer: LcmSummarizerMode,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens: Some(1_000),
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        summarizer,
    }
}

fn limited_compress_request(
    provider: &str,
    session_id: &str,
    summarizer: LcmSummarizerMode,
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
    max_assembly_tokens: Option<i64>,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens: Some(1_000),
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        threshold_tokens: None,
        max_assembly_tokens,
        leaf_chunk_tokens,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        summarizer,
    }
}

fn lcm_raw_message(store_id: i64, role: &str, content: &str) -> LcmRawMessage {
    LcmRawMessage {
        provider: "cursor".into(),
        message_id: format!("message-{store_id}"),
        session_id: "session-1".into(),
        store_id,
        role: role.into(),
        ordinal: store_id,
        timestamp: Some(1_715_000_000 + store_id),
        content: content.into(),
        content_hash: format!("hash-{store_id}"),
        storage_kind: LcmStorageKind::Inline,
        payload_ref: None,
        legacy_source: false,
        legacy_truncated: false,
        metadata_json: None,
    }
}

fn active_multi_tool_transaction() -> Vec<Value> {
    vec![
        json!({
            "id": "assistant-tools",
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {
                    "id": "call-a",
                    "type": "function",
                    "function": {"name": "lookup_a", "arguments": "{\"query\":\"alpha\"}"},
                },
                {
                    "id": "call-b",
                    "type": "function",
                    "function": {"name": "lookup_b", "arguments": "{\"query\":\"beta\"}"},
                },
            ],
        }),
        json!({
            "id": "tool-a",
            "role": "tool",
            "tool_call_id": "call-a",
            "content": "alpha result",
        }),
        json!({
            "id": "tool-b",
            "role": "tool",
            "tool_call_id": "call-b",
            "content": "beta result",
        }),
    ]
}

fn lifecycle_state_with_debt(maintenance_debt: Vec<LcmMaintenanceDebt>) -> LcmLifecycleState {
    LcmLifecycleState {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: None,
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt,
    }
}

// Characterization fixture for `compress_in_transaction` seam extractions.
// Keep this intentionally table-like: follow-on refactors can move internals
// behind smaller seams and re-run this single fixture to prove the externally
// visible decisions, response reasons, replay assembly, and DB writes stayed
// stable across the main branches.
#[derive(Clone, Copy)]
enum CompressBaselineCase {
    FrontierChanged,
    BelowLeafThreshold,
    AuxiliarySummaryRequest,
    FakeSummaryWrite,
}

impl CompressBaselineCase {
    fn name(self) -> &'static str {
        match self {
            CompressBaselineCase::FrontierChanged => "frontier_changed",
            CompressBaselineCase::BelowLeafThreshold => "below_leaf_threshold",
            CompressBaselineCase::AuxiliarySummaryRequest => "auxiliary_summary_request",
            CompressBaselineCase::FakeSummaryWrite => "fake_summary_write",
        }
    }
}

fn boundary_request(
    session_id: &str,
    old_session_id: &str,
    bound_session_id: Option<&str>,
) -> LcmSessionBoundaryRequest {
    LcmSessionBoundaryRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        old_session_id: Some(old_session_id.to_string()),
        boundary_reason: Some("compression".to_string()),
        bound_session_id: bound_session_id.map(str::to_string),
        boundary_skip_at: None,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
}

fn summary_draft(
    provider: &str,
    session_id: &str,
    depth: i64,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: session_id.to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 20,
        summary_token_count: 3,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("test summary lineage".to_string()),
        metadata_json: None,
    }
}

fn summary_draft_with_times(
    provider: &str,
    session_id: &str,
    depth: i64,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
    source_time_start: i64,
    source_time_end: i64,
) -> LcmSummaryNodeDraft {
    let mut draft = summary_draft(provider, session_id, depth, summary_text, source_refs);
    draft.source_time_start = Some(source_time_start);
    draft.source_time_end = Some(source_time_end);
    draft
}

mod boundary;
mod compaction;
mod condensation;
mod decision_baseline;
mod frontier;
mod overflow;
mod patterns;
mod preflight;
mod replay;
mod summarizer_modes;
mod tool_transactions;
