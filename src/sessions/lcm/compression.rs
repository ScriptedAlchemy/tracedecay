use std::path::Path;

use libsql::{params, Connection, Value as DbValue};
use serde_json::{json, Value};

use crate::sessions::SessionMessageRecord;

use super::{
    dag, raw, LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmLifecycleState,
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmPreflightRequest, LcmPreflightResponse,
    LcmRawMessage, LcmSourceRef, LcmStorageKind, LcmSummarizerMode, LcmSummaryNode,
    LcmSummaryNodeDraft,
};

const DEFAULT_FRESH_TAIL_COUNT: usize = 2;

struct IngestedActiveMessages {
    replay_messages: Vec<Value>,
    changed_replay: bool,
}

pub(crate) async fn update_lifecycle(
    conn: &Connection,
    update: LcmLifecycleUpdate,
) -> Result<LcmLifecycleState, LcmError> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    if let Err(err) = upsert_lifecycle_state(conn, &update).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }
    if let Err(err) = replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await
    {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }

    match conn.execute("COMMIT", ()).await {
        Ok(_) => lifecycle_state(conn, &update.provider, &update.conversation_id).await,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(LcmError::Db(err.to_string()))
        }
    }
}

pub(crate) async fn lifecycle_state(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
) -> Result<LcmLifecycleState, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, conversation_id, current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND conversation_id = ?2",
            params![provider, conversation_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("lifecycle state not found".to_string()))?;
    let maintenance_debt = load_maintenance_debt(conn, provider, conversation_id).await?;
    Ok(LcmLifecycleState {
        provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        conversation_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        current_session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        current_frontier_store_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_session_id: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_frontier_store_id: row
            .get(5)
            .map_err(|err| LcmError::Db(err.to_string()))?,
        maintenance_debt,
    })
}

pub(crate) async fn preflight(
    conn: &Connection,
    storage_root: &Path,
    request: LcmPreflightRequest,
) -> Result<LcmPreflightResponse, LcmError> {
    ensure_session(conn, &request.provider, &request.session_id).await?;
    let ingested = ingest_active_messages(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
    )
    .await?;
    let reason = if ingested.changed_replay {
        "ingest_protection_changed_replay"
    } else {
        "no_compression_needed"
    };
    Ok(LcmPreflightResponse {
        status: "ok".to_string(),
        should_compress: ingested.changed_replay,
        reason: reason.to_string(),
        replay_messages: ingested.replay_messages,
    })
}

pub(crate) async fn compress(
    conn: &Connection,
    storage_root: &Path,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    ensure_session(conn, &request.provider, &request.session_id).await?;
    let ingested = ingest_active_messages(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
    )
    .await?;

    if matches!(request.summarizer, LcmSummarizerMode::Noop) {
        let frontier = lifecycle_state_or_default(
            conn,
            &request.provider,
            &request.session_id,
            &request.session_id,
        )
        .await?;
        return Ok(LcmCompressionResponse {
            status: "ok".to_string(),
            reason: "noop_summarizer".to_string(),
            summary_nodes_created: 0,
            summary_nodes: Vec::new(),
            replay_messages: ingested.replay_messages,
            frontier,
        });
    }

    let conversation_id = request.session_id.clone();
    let existing_frontier = lifecycle_state_or_default(
        conn,
        &request.provider,
        &conversation_id,
        &request.session_id,
    )
    .await?;
    let raw_messages =
        load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
    let frontier_store_id = existing_frontier.current_frontier_store_id.unwrap_or(0);
    let unsummarized = raw_messages
        .iter()
        .filter(|message| message.store_id > frontier_store_id)
        .cloned()
        .collect::<Vec<_>>();
    let backlog_len = unsummarized.len().saturating_sub(DEFAULT_FRESH_TAIL_COUNT);
    let (backlog, fresh_tail) = unsummarized.split_at(backlog_len);

    if backlog.is_empty() {
        return Ok(LcmCompressionResponse {
            status: "ok".to_string(),
            reason: "no_backlog_to_compress".to_string(),
            summary_nodes_created: 0,
            summary_nodes: Vec::new(),
            replay_messages: fresh_tail.iter().map(raw_replay_message).collect(),
            frontier: existing_frontier,
        });
    }

    if matches!(request.summarizer, LcmSummarizerMode::HermesAuxiliary) {
        return Ok(LcmCompressionResponse {
            status: "needs_summary".to_string(),
            reason: "hermes_auxiliary_not_available_in_task_9".to_string(),
            summary_nodes_created: 0,
            summary_nodes: Vec::new(),
            replay_messages: fresh_tail.iter().map(raw_replay_message).collect(),
            frontier: existing_frontier,
        });
    }

    let (summary_text, route) = match request.summarizer {
        LcmSummarizerMode::Fake { summary_text } => (summary_text, None),
        LcmSummarizerMode::Provided {
            summary_text,
            route,
        } => (summary_text, route),
        LcmSummarizerMode::Noop | LcmSummarizerMode::HermesAuxiliary => unreachable!(),
    };

    let source_refs = backlog
        .iter()
        .map(|message| LcmSourceRef::RawMessage {
            store_id: message.store_id,
        })
        .collect::<Vec<_>>();
    let source_token_count = backlog
        .iter()
        .map(|message| estimate_tokens(&message.content))
        .sum::<i64>();
    let source_time_start = backlog.iter().filter_map(|message| message.timestamp).min();
    let source_time_end = backlog.iter().filter_map(|message| message.timestamp).max();
    let metadata_json = route.map(|route| json!({ "summary_route": route }).to_string());
    let summary = dag::insert_summary_node(
        conn,
        LcmSummaryNodeDraft {
            provider: request.provider.clone(),
            conversation_id: conversation_id.clone(),
            session_id: request.session_id.clone(),
            depth: 0,
            summary_text: summary_text.clone(),
            source_refs,
            source_token_count,
            summary_token_count: estimate_tokens(&summary_text),
            source_time_start,
            source_time_end,
            expand_hint: Some(format!("{} raw messages", backlog.len())),
            metadata_json,
        },
    )
    .await?;
    let new_frontier = backlog
        .last()
        .map(|message| message.store_id)
        .or(existing_frontier.current_frontier_store_id);
    let frontier = update_lifecycle(
        conn,
        LcmLifecycleUpdate {
            provider: request.provider.clone(),
            conversation_id,
            current_session_id: request.session_id.clone(),
            current_frontier_store_id: new_frontier,
            last_finalized_session_id: existing_frontier.last_finalized_session_id.clone(),
            last_finalized_frontier_store_id: existing_frontier.last_finalized_frontier_store_id,
            maintenance_debt: Vec::new(),
        },
    )
    .await?;

    let mut replay_messages = Vec::with_capacity(1 + fresh_tail.len());
    replay_messages.push(summary_replay_message(&summary));
    replay_messages.extend(fresh_tail.iter().map(raw_replay_message));

    Ok(LcmCompressionResponse {
        status: "ok".to_string(),
        reason: "compressed_backlog".to_string(),
        summary_nodes_created: 1,
        summary_nodes: vec![summary],
        replay_messages,
        frontier,
    })
}

pub(crate) async fn maintenance_debt_count(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_maintenance_debt d
             JOIN lcm_lifecycle_state s
               ON s.provider = d.provider AND s.conversation_id = d.conversation_id
             WHERE d.provider = ?1 AND (?2 IS NULL OR s.current_session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("maintenance debt count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn upsert_lifecycle_state(
    conn: &Connection,
    update: &LcmLifecycleUpdate,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_lifecycle_state (
            provider, conversation_id, current_session_id, last_finalized_session_id,
            current_frontier_store_id, last_finalized_frontier_store_id, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
         ON CONFLICT(provider, conversation_id) DO UPDATE SET
            current_session_id = excluded.current_session_id,
            last_finalized_session_id = excluded.last_finalized_session_id,
            current_frontier_store_id = excluded.current_frontier_store_id,
            last_finalized_frontier_store_id = excluded.last_finalized_frontier_store_id,
            updated_at = unixepoch()",
        params![
            update.provider.as_str(),
            update.conversation_id.as_str(),
            update.current_session_id.as_str(),
            opt_text(update.last_finalized_session_id.as_deref()),
            opt_i64(update.current_frontier_store_id),
            opt_i64(update.last_finalized_frontier_store_id),
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn replace_maintenance_debt(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
    debts: &[LcmMaintenanceDebt],
) -> Result<(), LcmError> {
    conn.execute(
        "DELETE FROM lcm_maintenance_debt WHERE provider = ?1 AND conversation_id = ?2",
        params![provider, conversation_id],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;

    for debt in debts {
        let (debt_id, debt_kind, from_store_id, to_store_id) = debt_to_db(debt);
        conn.execute(
            "INSERT INTO lcm_maintenance_debt (
                provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                provider,
                conversation_id,
                debt_id.as_str(),
                debt_kind,
                opt_i64(from_store_id),
                opt_i64(to_store_id),
            ],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    }
    Ok(())
}

async fn load_maintenance_debt(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
) -> Result<Vec<LcmMaintenanceDebt>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT debt_kind, from_store_id, to_store_id
             FROM lcm_maintenance_debt
             WHERE provider = ?1 AND conversation_id = ?2
             ORDER BY created_at, debt_id",
            params![provider, conversation_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut debts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let debt_kind: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        debts.push(debt_from_db(
            &debt_kind,
            row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        )?);
    }
    Ok(debts)
}

async fn lifecycle_state_or_default(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
    session_id: &str,
) -> Result<LcmLifecycleState, LcmError> {
    match lifecycle_state(conn, provider, conversation_id).await {
        Ok(state) => Ok(state),
        Err(LcmError::Db(message)) if message == "lifecycle state not found" => {
            Ok(LcmLifecycleState {
                provider: provider.to_string(),
                conversation_id: conversation_id.to_string(),
                current_session_id: session_id.to_string(),
                current_frontier_store_id: None,
                last_finalized_session_id: None,
                last_finalized_frontier_store_id: None,
                maintenance_debt: Vec::new(),
            })
        }
        Err(err) => Err(err),
    }
}

async fn ingest_active_messages(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    messages: &[Value],
) -> Result<IngestedActiveMessages, LcmError> {
    let mut replay_messages = Vec::with_capacity(messages.len());
    let mut changed_replay = false;
    let mut ordinal = next_ordinal(conn, provider, session_id).await?;

    for (idx, message) in messages.iter().enumerate() {
        ordinal += 1;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let content = message_content(message);
        let message_id = message
            .get("id")
            .or_else(|| message.get("message_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                deterministic_message_id(provider, session_id, idx, &role, &content)
            });
        let kind = message
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some("message".to_string()));
        let record = SessionMessageRecord {
            provider: provider.to_string(),
            message_id: message_id.clone(),
            session_id: session_id.to_string(),
            role,
            timestamp: message.get("timestamp").and_then(Value::as_i64),
            ordinal,
            text: content.clone(),
            kind,
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: serde_json::to_string(message).ok(),
        };
        let upsert = raw::upsert_raw_message_with_payload(conn, storage_root, &record).await?;
        let raw = super::schema::load_raw_message(conn, provider, &message_id)
            .await
            .ok_or_else(|| LcmError::Db("active message did not persist".to_string()))?;
        let replay_content = if raw.storage_kind == LcmStorageKind::External {
            changed_replay = true;
            upsert.projection_text
        } else {
            content
        };
        let mut replay = message.clone();
        replay["role"] = Value::String(record.role.clone());
        replay["content"] = Value::String(replay_content);
        replay_messages.push(replay);
    }

    Ok(IngestedActiveMessages {
        replay_messages,
        changed_replay,
    })
}

async fn ensure_session(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT OR IGNORE INTO sessions (
            provider, session_id, project_key, project_path, title, started_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())",
        params![
            provider,
            session_id,
            "lcm-active-context",
            "lcm-active-context",
            "LCM active context",
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn next_ordinal(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(ordinal), 0)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("ordinal query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn load_raw_messages_for_session(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessage>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY store_id",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut messages = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        messages.push(raw_message_from_row(&row)?);
    }
    Ok(messages)
}

fn raw_message_from_row(row: &libsql::Row) -> Result<LcmRawMessage, LcmError> {
    let storage_kind_text: String = row.get(9).map_err(|err| LcmError::Db(err.to_string()))?;
    let content: Option<String> = row.get(7).map_err(|err| LcmError::Db(err.to_string()))?;
    let snippet_text: String = row.get(11).map_err(|err| LcmError::Db(err.to_string()))?;
    let storage_kind = LcmStorageKind::from_db(&storage_kind_text)
        .ok_or_else(|| LcmError::Db(format!("invalid storage_kind: {storage_kind_text}")))?;
    let content = match storage_kind {
        LcmStorageKind::Inline => content.unwrap_or_default(),
        LcmStorageKind::External => content.unwrap_or(snippet_text),
    };
    Ok(LcmRawMessage {
        provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        message_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        store_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        role: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        ordinal: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        timestamp: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
        content,
        content_hash: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        storage_kind,
        payload_ref: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
        legacy_source: row.get::<i64>(12).unwrap_or(0) != 0,
        legacy_truncated: row.get::<i64>(13).unwrap_or(0) != 0,
        metadata_json: row.get(14).map_err(|err| LcmError::Db(err.to_string()))?,
    })
}

fn message_content(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(items) = content.as_array() {
        let texts = items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !texts.is_empty() {
            return texts.join("\n\n");
        }
    }
    content.to_string()
}

fn deterministic_message_id(
    provider: &str,
    session_id: &str,
    idx: usize,
    role: &str,
    content: &str,
) -> String {
    format!(
        "active_{}",
        raw::sha256_hex(&format!(
            "{provider}\0{session_id}\0{idx}\0{role}\0{content}"
        ))
    )
}

fn raw_replay_message(message: &LcmRawMessage) -> Value {
    json!({
        "role": message.role,
        "content": message.content,
        "store_id": message.store_id,
    })
}

fn summary_replay_message(summary: &LcmSummaryNode) -> Value {
    json!({
        "role": "system",
        "content": summary.summary_text,
        "lcm_summary_node_id": summary.node_id,
    })
}

fn estimate_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

fn debt_to_db(debt: &LcmMaintenanceDebt) -> (String, &'static str, Option<i64>, Option<i64>) {
    match debt {
        LcmMaintenanceDebt::RawBacklog {
            from_store_id,
            to_store_id,
        } => (
            format!("raw_backlog:{from_store_id}:{to_store_id}"),
            "raw_backlog",
            Some(*from_store_id),
            Some(*to_store_id),
        ),
    }
}

fn debt_from_db(
    debt_kind: &str,
    from_store_id: Option<i64>,
    to_store_id: Option<i64>,
) -> Result<LcmMaintenanceDebt, LcmError> {
    match debt_kind {
        "raw_backlog" => Ok(LcmMaintenanceDebt::RawBacklog {
            from_store_id: from_store_id.unwrap_or(0),
            to_store_id: to_store_id.unwrap_or(0),
        }),
        _ => Err(LcmError::Db(format!(
            "invalid maintenance debt kind: {debt_kind}"
        ))),
    }
}

fn opt_text(value: Option<&str>) -> DbValue {
    value.map_or(DbValue::Null, |s| DbValue::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> DbValue {
    value.map_or(DbValue::Null, DbValue::Integer)
}
