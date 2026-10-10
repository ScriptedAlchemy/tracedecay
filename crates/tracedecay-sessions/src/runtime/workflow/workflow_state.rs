//! Unfinished-workflow evidence listing.
//!
//! A lightweight, text-evidence view over ingested session messages: it scans
//! the LCM raw-message store for phrases that signal a stalled or terminated
//! run (`session limit`, `blocked`, `interrupted`, `runs:0`) and reports the
//! matching rows. This complements the structured `workflow_runs` /
//! `workflow_agents` tables (see [`crate::runtime::workflow_index`]): where
//! those record what the workflow harness wrote, this surfaces in-transcript
//! evidence that a run did not finish cleanly, including for providers/sessions
//! that never produced a `wf_*` run directory.

use serde::Serialize;

use tracedecay_runtime_core::db::engine::{BackendKind, QueryExecutor, params};

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
pub async fn list_unfinished(
    snapshot: &impl QueryExecutor,
    limit: usize,
) -> Result<Vec<WorkflowStateItem>, String> {
    let limit = limit.clamp(1, 250) as i64;
    let backend = snapshot.backend_kind();
    let source = match backend {
        BackendKind::Sqlite => {
            "lcm_raw_messages_fts JOIN lcm_raw_messages AS raw ON raw.store_id = lcm_raw_messages_fts.rowid"
        }
        BackendKind::NativeTurso => {
            "indexed_hits JOIN lcm_raw_messages AS raw ON raw.store_id = indexed_hits.store_id"
        }
    };
    let predicate = tracedecay_runtime_core::db::native_search::predicate(
        backend,
        "lcm_raw_messages_fts",
        &[
            "raw.index_text",
            "raw.role",
            "raw.kind",
            "raw.model",
            "raw.tool_names",
        ],
        "?1",
    );
    let predicate = if backend == BackendKind::NativeTurso {
        "1 = 1".to_owned()
    } else {
        predicate
    };
    let prefix = if backend == BackendKind::NativeTurso {
        "WITH indexed_hits AS MATERIALIZED (SELECT store_id FROM lcm_raw_messages
            WHERE fts_match(index_text, role, kind, model, tool_names, ?1)) "
    } else {
        ""
    };
    let sql = format!(
        "{prefix}SELECT raw.provider, raw.session_id, raw.message_id, raw.ordinal, raw.content,
                    COALESCE(raw.snippet_text, ''), COALESCE(raw.metadata_json, '')
             FROM {source} WHERE {predicate}
             ORDER BY COALESCE(raw.timestamp, 0) DESC, raw.store_id DESC LIMIT ?2"
    );
    let query = match backend {
        BackendKind::Sqlite => {
            r#"index_text : ("session limit" OR blocked OR interrupted OR "runs 0")"#
        }
        BackendKind::NativeTurso => {
            r#"index_text:("session limit" OR "blocked" OR "interrupted" OR "runs 0")"#
        }
    };
    let mut rows = snapshot
        .query(&sql, params![query, limit])
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
        crate::runtime::shared::one_line_truncated(evidence_source, EVIDENCE_PREVIEW_CAP),
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

    #[tokio::test]
    async fn native_unfinished_search_uses_body_and_pinned_snapshot() {
        use tracedecay_runtime_core::db::engine::{Executor, NativeTestConnection};
        let temp = tempfile::tempdir().expect("native workflow directory");
        let conn = NativeTestConnection::open(&temp.path().join("workflow.db"))
            .expect("native workflow store");
        conn.execute_batch(
            "CREATE TABLE sessions (
            provider TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY(provider,session_id));
            INSERT INTO sessions VALUES('cursor','session-a');",
        )
        .await
        .expect("session identity for convergence foreign keys");
        let schema_transaction = conn.transaction().await.expect("schema transaction");
        tracedecay_lcm::schema::ensure_lcm_schema_in_transaction(&schema_transaction)
            .await
            .expect("canonical LCM schema");
        schema_transaction
            .commit()
            .await
            .expect("publish LCM schema");
        for (id, text, model, ordinal) in [
            (
                "blocked",
                "automation blocked on missing credentials",
                None,
                1,
            ),
            (
                "session-limit",
                "Claude hit the session limit while running task",
                None,
                2,
            ),
            (
                "model-only",
                "complete successful execution",
                Some("blocked"),
                3,
            ),
        ] {
            let record = tracedecay_store::SessionMessageRecord {
                provider: "cursor".to_owned(),
                message_id: id.to_owned(),
                session_id: "session-a".to_owned(),
                role: "assistant".to_owned(),
                timestamp: Some(ordinal),
                ordinal,
                text: text.to_owned(),
                kind: Some("message".to_owned()),
                model: model.map(str::to_owned),
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            };
            let mut rollback =
                tracedecay_lcm::payload::PayloadFileRollback::begin_cancellation_safe(temp.path());
            tracedecay_lcm::raw::upsert_raw_message_with_payload_tracked(
                &conn,
                temp.path(),
                &record,
                &mut rollback,
            )
            .await
            .expect("canonical workflow ingest");
            rollback.disarm();
        }
        let snapshot = conn.read_snapshot().await.expect("native pinned snapshot");
        let rows = list_unfinished(&snapshot, 1)
            .await
            .expect("native production workflow search");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "session-limit");
        assert_eq!(rows[0].status, "session limit");
        let all = list_unfinished(&snapshot, 10)
            .await
            .expect("native body-only search");
        assert_eq!(
            all.iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            ["session-limit", "blocked"]
        );
        drop(snapshot);
        conn.execute(
            "DELETE FROM lcm_raw_messages WHERE message_id = ?1",
            params!["blocked"],
        )
        .await
        .expect("canonical purge");
        let snapshot = conn.read_snapshot().await.expect("new native snapshot");
        assert_eq!(
            list_unfinished(&snapshot, 10)
                .await
                .expect("search after purge")
                .len(),
            1
        );
    }

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
}
