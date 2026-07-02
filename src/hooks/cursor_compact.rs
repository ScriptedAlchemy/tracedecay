//! Cursor `preCompact` machinery.
//!
//! Cursor's compaction event exposes pressure metadata but not Cursor's own
//! generated summary text, so at the boundary `TraceDecay` ingests the current
//! transcript tail, asks LCM for the compactable raw-message backlog,
//! generates a summary through `cursor-agent -p`, and stores that summary as
//! a normal LCM summary node.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::cursor::{cursor_project_root_from_parsed_event, ingest_cursor_transcript_for_event};
use super::{event_i64, event_session_id, event_usize};

/// Budget for the transcript catch-up portion of the `preCompact` hook.
const CURSOR_PRE_COMPACT_INGEST_BUDGET: Duration = Duration::from_secs(30);
/// Budget for the auxiliary `cursor-agent` summary call inside the hook. Kept
/// below the registered Cursor hook timeout so the child can be killed/reaped
/// by `TraceDecay` rather than by Cursor killing the hook process. Sized so
/// the ingest budget plus this cap stay below the overall preCompact budget,
/// leaving slack for LCM prepare/persist and process overhead.
pub(super) const CURSOR_PRE_COMPACT_SUMMARY_BUDGET: Duration = Duration::from_secs(75);
/// Overall budget for the `preCompact` hook (registered with a 120s timeout).
const CURSOR_PRE_COMPACT_BUDGET: Duration = Duration::from_secs(115);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPreCompactOutcome {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_node_ids: Vec<String>,
}

impl CursorPreCompactOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }
}

pub async fn cursor_pre_compact_for_event_with_config(
    event_json: &str,
    config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    match tokio::time::timeout(
        CURSOR_PRE_COMPACT_BUDGET,
        cursor_pre_compact_for_event_inner(event_json, config),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => CursorPreCompactOutcome::error("timed out"),
    }
}

async fn cursor_pre_compact_for_event_inner(
    event_json: &str,
    config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    if std::env::var(crate::sessions::cursor_agent::CURSOR_SUMMARY_CHILD_ENV).is_ok() {
        return CursorPreCompactOutcome::skipped("cursor summary child");
    }
    let parsed = match serde_json::from_str::<Value>(event_json) {
        Ok(parsed) => parsed,
        Err(err) => return CursorPreCompactOutcome::error(format!("invalid event JSON: {err}")),
    };
    let Some(project_root) = cursor_project_root_from_parsed_event(&parsed) else {
        return CursorPreCompactOutcome::skipped("no project root");
    };
    if !cursor_event_transcript_path_exists(&parsed) {
        return CursorPreCompactOutcome::skipped("no transcript path");
    }

    let caught_up =
        ingest_cursor_transcript_for_event(event_json, None, CURSOR_PRE_COMPACT_INGEST_BUDGET)
            .await;
    if !caught_up {
        return CursorPreCompactOutcome::skipped("transcript ingest did not complete");
    }

    let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
        return CursorPreCompactOutcome::skipped("session database unavailable");
    };
    let Some(session_id) = event_session_id(&parsed) else {
        return CursorPreCompactOutcome::skipped("no session id");
    };

    let messages_to_compact = event_usize(&parsed, &["messages_to_compact", "compact_count"]);
    if messages_to_compact == Some(0) {
        return CursorPreCompactOutcome::skipped("no messages to compact");
    }
    let fresh_tail_count = cursor_pre_compact_fresh_tail_count(&parsed, messages_to_compact);
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);

    let first = match db
        .lcm_compress(cursor_pre_compact_lcm_request(
            &session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::HermesAuxiliary,
            None,
        ))
        .await
    {
        Ok(response) => response,
        Err(err) => return CursorPreCompactOutcome::error(format!("LCM prepare failed: {err}")),
    };
    let Some(summary_request) = first.summary_request else {
        return CursorPreCompactOutcome::skipped(first.reason);
    };

    let summary = match crate::sessions::cursor_agent::summarize_with_cursor_agent(
        &summary_request,
        config,
    ) {
        Ok(summary) => summary,
        Err(err) => {
            return CursorPreCompactOutcome::error(format!("cursor-agent summary failed: {err}"))
        }
    };

    let second = match db
        .lcm_compress(cursor_pre_compact_lcm_request(
            &session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::Provided {
                summary_text: summary,
                route: Some("cursor_agent".to_string()),
            },
            first.frontier.current_frontier_store_id.or(Some(0)),
        ))
        .await
    {
        Ok(response) => response,
        Err(err) => return CursorPreCompactOutcome::error(format!("LCM persist failed: {err}")),
    };
    CursorPreCompactOutcome {
        status: second.status,
        reason: second.reason,
        summary_nodes_created: second.summary_nodes_created,
        summary_node_ids: second
            .summary_nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect(),
    }
}

fn cursor_pre_compact_lcm_request(
    session_id: &str,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
    summarizer: crate::sessions::lcm::LcmSummarizerMode,
    expected_current_frontier_store_id: Option<i64>,
) -> crate::sessions::lcm::LcmCompressionRequest {
    crate::sessions::lcm::LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens,
        focus_topic: Some("Cursor context compaction".to_string()),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length,
        reserve_tokens_floor: None,
        summarizer,
    }
}

fn cursor_pre_compact_fresh_tail_count(
    parsed: &Value,
    messages_to_compact: Option<usize>,
) -> Option<usize> {
    let message_count = event_usize(parsed, &["message_count", "messages_count"])?;
    let messages_to_compact = messages_to_compact?;
    Some(message_count.saturating_sub(messages_to_compact))
}

fn cursor_event_transcript_path_exists(parsed: &Value) -> bool {
    parsed
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .is_some_and(|path| Path::new(path).exists())
}
