//! Unfinished-workflow evidence listing.
//!
//! A lightweight, text-evidence view over ingested session messages: it scans
//! the LCM raw-message store for phrases that signal a stalled or terminated
//! run (`session limit`, `blocked`, `interrupted`, `runs:0`) and reports the
//! matching rows. This complements the structured `workflow_runs` /
//! `workflow_agents` tables (see [`crate::sessions::workflow_index`]): where
//! those record what the workflow harness wrote, this surfaces in-transcript
//! evidence that a run did not finish cleanly, including for providers/sessions
//! that never produced a `wf_*` run directory.

use serde::Serialize;

use crate::db::engine::{ReadSnapshot, params};

/// Max characters of collapsed evidence text kept per unfinished-run row before
/// a single-character `…` truncation, so one row never dominates the listing.
const EVIDENCE_PREVIEW_CAP: usize = 180;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowStateItem {
    pub status: String,
    pub provider: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub message_id: String,
    pub ordinal: i64,
    pub evidence: String,
}

/// Reads through one caller-pinned snapshot, so every returned row is observed
/// at a single database generation. The store adapter owns opening it.
pub(crate) async fn list_unfinished(
    snapshot: &ReadSnapshot,
    limit: usize,
) -> Result<Vec<WorkflowStateItem>, String> {
    let limit = limit.clamp(1, 250) as i64;
    let mut rows = snapshot
        .query(
            "SELECT raw.provider, raw.session_id, raw.message_id, raw.ordinal, raw.content,
                    COALESCE(raw.snippet_text, ''), COALESCE(raw.metadata_json, '')
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages AS raw
               ON raw.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH ?1
             ORDER BY COALESCE(raw.timestamp, 0) DESC, raw.store_id DESC
             LIMIT ?2",
            params![
                r#""session limit" OR blocked OR interrupted OR "runs 0""#,
                limit
            ],
        )
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
        let content: String = row.get(4).map_err(|e| e.to_string())?;
        let snippet: String = row.get(5).map_err(|e| e.to_string())?;
        if let Some((status, evidence)) = classify_evidence(&content, &snippet) {
            let metadata_json: String = row.get(6).map_err(|e| e.to_string())?;
            out.push(WorkflowStateItem {
                status,
                provider: row.get(0).map_err(|e| e.to_string())?,
                session_id: row.get(1).map_err(|e| e.to_string())?,
                message_id: row.get(2).map_err(|e| e.to_string())?,
                ordinal: row.get(3).map_err(|e| e.to_string())?,
                task_id: task_id_from_metadata(&metadata_json),
                evidence,
            });
        }
    }
    Ok(out)
}

fn classify_evidence(content: &str, snippet: &str) -> Option<(String, String)> {
    let status = classify_status(content)?;
    let evidence_source = if snippet.trim().is_empty() {
        content
    } else {
        snippet
    };
    Some((
        status.to_string(),
        crate::sessions::shared::one_line_truncated(evidence_source, EVIDENCE_PREVIEW_CAP),
    ))
}

fn classify_status(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("session limit") {
        Some("session limit")
    } else if lower.contains("runs:0") || lower.contains("\"runs\":0") {
        Some("runs:0")
    } else if lower.contains("blocked") {
        Some("blocked")
    } else if lower.contains("interrupted") {
        Some("interrupted")
    } else {
        None
    }
}

fn task_id_from_metadata(metadata_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(metadata_json).ok()?;
    ["task_id", "taskId", "task", "id"]
        .into_iter()
        .find_map(|key| value.get(key)?.as_str())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn classify_workflow_states_from_text() {
        for (text, expected) in [
            (
                "Claude hit the session limit while running task",
                "session limit",
            ),
            ("automation blocked on missing credentials", "blocked"),
            ("task interrupted by compaction", "interrupted"),
            ("worker finished with runs:0", "runs:0"),
            (r#"{"runs":0,"status":"queued"}"#, "runs:0"),
        ] {
            let (status, evidence) = classify_evidence(text, "").expect("status");
            assert_eq!(status, expected);
            assert!(!evidence.is_empty());
        }
    }

    #[test]
    fn extracts_task_id_from_metadata() {
        assert_eq!(
            task_id_from_metadata(r#"{"task_id":"task-123"}"#),
            Some("task-123".to_string())
        );
    }
}
