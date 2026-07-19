use tempfile::TempDir;
use tracedecay::global_db::{GlobalDb, ParseOffset};
use tracedecay::sessions::lcm::{
    LCM_SCHEMA_VERSION, LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmError,
    LcmExpandQueryRequest, LcmExpandRequest, LcmExpandTarget, LcmGcConfig, LcmGrepRequest,
    LcmGrepSort, LcmLifecycleUpdate, LcmLoadSessionRequest, LcmMaintenanceDebt, LcmScope,
    LcmSessionReplayRequest, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft,
    MAX_DERIVED_SNIPPET_CHARS,
};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};

use crate::common::{
    self, isolated_lcm_db_path as isolated_db_path, lcm_dag_message as raw_message, open_lcm_db,
};

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    common::session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM query test",
        None,
        None,
    )
}

struct RawMessageContext<'a> {
    role: &'a str,
    source: &'a str,
    timestamp: i64,
}

fn raw_message_with_role_source_timestamp(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
    context: RawMessageContext<'_>,
) -> SessionMessageRecord {
    let mut message = raw_message(provider, message_id, session_id, ordinal, text);
    message.role = context.role.to_string();
    message.timestamp = Some(context.timestamp);
    message.metadata_json = Some(serde_json::json!({"source": context.source}).to_string());
    message
}

async fn insert_session(db: &GlobalDb, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

async fn insert_raw_messages(
    db: &GlobalDb,
    db_path: &std::path::Path,
    provider: &str,
    session_id: &str,
    contents: &[String],
) -> Vec<i64> {
    let session = sample_session(provider, session_id);
    let messages: Vec<_> = contents
        .iter()
        .enumerate()
        .map(|(idx, content)| {
            let message_id = format!("{session_id}-message-{:03}", idx + 1);
            raw_message(provider, &message_id, session_id, (idx + 1) as i64, content)
        })
        .collect();
    assert!(
        db.upsert_transcript_batch(
            &session,
            &messages,
            &format!("session-lcm-query-{provider}-{session_id}.jsonl"),
            ParseOffset::default(),
        )
        .await
    );
    let message_ids: Vec<_> = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect();

    if message_ids.is_empty() {
        return Vec::new();
    }

    let mut store_ids_by_message_id = std::collections::BTreeMap::new();
    let raw_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT message_id, store_id
         FROM lcm_raw_messages
         WHERE provider = ?
           AND session_id = ?
           AND message_id IN ({placeholders})
         ORDER BY message_id"
    );
    let mut values = vec![
        libsql::Value::Text(provider.to_string()),
        libsql::Value::Text(session_id.to_string()),
    ];
    values.extend(message_ids.iter().cloned().map(libsql::Value::Text));
    let mut rows = conn
        .query(&sql, libsql::params_from_iter(values))
        .await
        .expect("raw message store ids should query after insert");
    while let Some(row) = rows
        .next()
        .await
        .expect("raw message store id row should read")
    {
        let message_id = row
            .get::<String>(0)
            .expect("raw message message_id should decode");
        let store_id = row
            .get::<i64>(1)
            .expect("raw message store_id should decode");
        store_ids_by_message_id.insert(message_id, store_id);
    }

    assert_eq!(store_ids_by_message_id.len(), message_ids.len());
    message_ids
        .into_iter()
        .map(|message_id| {
            *store_ids_by_message_id
                .get(&message_id)
                .unwrap_or_else(|| panic!("raw message {message_id} should exist"))
        })
        .collect()
}

fn summary_draft(
    provider: &str,
    session_id: &str,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation-1".to_string(),
        session_id: session_id.to_string(),
        depth: 0,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 30,
        summary_token_count: 5,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("query test summary".to_string()),
        metadata_json: None,
    }
}

mod architecture;
mod describe;
mod expand;
mod expand_query;
mod grep;
mod grep_ranking;
mod load_session;
mod sessions;
mod status;
