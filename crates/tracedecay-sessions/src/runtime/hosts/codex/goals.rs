//! Codex thread-goal events and goal-context blocks: structured models,
//! parsers, and their compact `goal` / `goal_context` / `context` rows.

use std::path::Path;

use serde_json::Value;

use super::PROVIDER;
use super::meta::CodexMeta;
use super::records::{collect_response_item_text, timestamp_from_record};
use crate::runtime::SessionMessageRecord;

/// Codex's structured session goal, parsed from a `thread_goal_updated`
/// `event_msg`. `status` is stored verbatim; the parser deliberately does not
/// map it to a fixed enum so an unrecognized future value survives round-trip.
pub(super) struct CodexGoalEvent {
    pub objective: String,
    pub status: Option<String>,
    pub thread_id: Option<String>,
    pub tokens_used: Option<i64>,
    pub time_used_seconds: Option<i64>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

impl CodexGoalEvent {
    /// Key used to collapse identical consecutive lifecycle states within one
    /// parse pass. Token/time drift on the same `(objective, status)` is
    /// progress within a state, not a transition, so it does not open a new row.
    pub(super) fn dedup_key(&self) -> (String, Option<String>) {
        (self.objective.clone(), self.status.clone())
    }

    pub(super) fn metadata(&self) -> Value {
        let mut goal = serde_json::Map::new();
        goal.insert(
            "source".to_string(),
            Value::String("codex_thread_goal".to_string()),
        );
        goal.insert(
            "source_event".to_string(),
            Value::String("thread_goal_updated".to_string()),
        );
        goal.insert(
            "objective".to_string(),
            Value::String(self.objective.clone()),
        );
        if let Some(status) = &self.status {
            goal.insert("status".to_string(), Value::String(status.clone()));
        }
        if let Some(thread_id) = &self.thread_id {
            goal.insert("thread_id".to_string(), Value::String(thread_id.clone()));
        }
        if let Some(tokens_used) = self.tokens_used {
            goal.insert("tokens_used".to_string(), Value::from(tokens_used));
        }
        if let Some(time_used_seconds) = self.time_used_seconds {
            goal.insert(
                "time_used_seconds".to_string(),
                Value::from(time_used_seconds),
            );
        }
        if let Some(created_at) = self.created_at {
            goal.insert("created_at".to_string(), Value::from(created_at));
        }
        if let Some(updated_at) = self.updated_at {
            goal.insert("updated_at".to_string(), Value::from(updated_at));
        }
        Value::Object(goal)
    }
}

/// Parse a `thread_goal_updated` `event_msg` into a [`CodexGoalEvent`], or
/// `None` for any other line. A goal with an empty/absent objective is skipped
/// (there is nothing to catalog or search).
pub(super) fn codex_goal_event_from_line(record: &Value) -> Option<CodexGoalEvent> {
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("thread_goal_updated") {
        return None;
    }
    let goal = payload.get("goal")?;
    let objective = goal
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())?
        .to_string();
    Some(CodexGoalEvent {
        objective,
        status: goal
            .get("status")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|status| !status.is_empty())
            .map(str::to_string),
        thread_id: goal
            .get("threadId")
            .and_then(Value::as_str)
            .or_else(|| payload.get("threadId").and_then(Value::as_str))
            .filter(|thread_id| !thread_id.is_empty())
            .map(str::to_string),
        tokens_used: goal.get("tokensUsed").and_then(Value::as_i64),
        time_used_seconds: goal.get("timeUsedSeconds").and_then(Value::as_i64),
        created_at: goal.get("createdAt").and_then(Value::as_i64),
        updated_at: goal.get("updatedAt").and_then(Value::as_i64),
    })
}

/// Build the compact `goal` session row: the objective as searchable text, the
/// lifecycle fields in `metadata_json`. Role `system` matches the other
/// non-conversational Codex rows (goal context, compaction summaries).
pub(super) fn goal_event_message(
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
    event: &CodexGoalEvent,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp,
        ordinal: offset,
        text: event.objective.clone(),
        kind: Some("goal".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&event.metadata()).ok(),
    }
}

pub(super) fn goal_context_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let text = collect_response_item_text(payload.get("content").unwrap_or(payload));
    if !is_goal_context_text(&text) {
        return None;
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_goal_context".to_string()),
    );
    metadata.insert(
        "source_event".to_string(),
        Value::String("response_item".to_string()),
    );
    metadata.insert("source_offset".to_string(), Value::from(offset));

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp: timestamp_from_record(record),
        ordinal: offset,
        text,
        kind: Some("context".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

fn is_goal_context_text(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return false;
    };
    let header = header.trim_end_matches(':').to_ascii_lowercase();
    if header != "current goal for this thread" && header != "active goal for this thread" {
        return false;
    }

    let mut has_objective = false;
    let mut has_budget = false;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        has_objective |= lower.starts_with("objective:");
        has_budget |=
            lower.starts_with("remaining token budget:") || lower.starts_with("token budget:");
    }
    has_objective && has_budget
}
