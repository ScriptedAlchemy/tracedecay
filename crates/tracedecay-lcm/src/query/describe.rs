use std::collections::HashMap;

use super::super::util::{SQLITE_IN_BATCH_SIZE, sql_in_placeholders};
use super::*;

pub(super) async fn raw_message_overviews(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessageOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT message_id, store_id, role, storage_kind, payload_ref, snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY store_id
             LIMIT 20",
            params![provider, session_id],
        )
        .await?;

    let mut overviews = Vec::new();
    while let Some(row) = rows.next().await? {
        let storage_kind_text: String = row.get(3)?;
        let content_preview: String = row.get(5)?;
        let (_, content_range) = slice_content(&content_preview, None);
        overviews.push(LcmRawMessageOverview {
            message_id: row.get(0)?,
            store_id: row.get(1)?,
            role: row.get(2)?,
            storage_kind: LcmStorageKind::from_db(&storage_kind_text).ok_or_else(|| {
                LcmError::Db(format!("invalid storage_kind: {storage_kind_text}"))
            })?,
            payload_ref: row.get(4)?,
            content_preview,
            content_range,
        });
    }
    Ok(overviews)
}

pub(super) async fn summary_overviews(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmSummaryNodeOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT n.node_id, n.conversation_id, n.depth, n.summary_text, n.created_at,
                    COUNT(s.source_id)
             FROM lcm_summary_nodes n
             LEFT JOIN lcm_summary_sources s ON s.node_id = n.node_id
             WHERE n.provider = ?1 AND n.session_id = ?2
             GROUP BY n.node_id, n.conversation_id, n.depth, n.summary_text, n.created_at
             ORDER BY n.depth, n.created_at, n.node_id
             LIMIT 20",
            params![provider, session_id],
        )
        .await?;

    let mut overviews = Vec::new();
    while let Some(row) = rows.next().await? {
        let summary_text: String = row.get(3)?;
        let source_count: i64 = row.get(5)?;
        overviews.push(LcmSummaryNodeOverview {
            node_id: row.get(0)?,
            conversation_id: row.get(1)?,
            depth: row.get(2)?,
            summary_preview: raw::derived_text_for_snippet(&summary_text),
            source_count: source_count.max(0) as usize,
            created_at: row.get(4)?,
        });
    }
    Ok(overviews)
}

pub(super) async fn describe_summary_node(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmDescribeSummaryNode, LcmError> {
    let mut rows = conn
        .query(
            "SELECT node_id, conversation_id, depth, summary_token_count,
                    source_token_count, source_time_start, source_time_end,
                    expand_hint, metadata_json, created_at
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND session_id = ?2 AND node_id = ?3",
            params![provider, session_id, node_id],
        )
        .await?;
    let row = rows.next().await?.ok_or(LcmError::SummaryNodeNotFound)?;
    let children = describe_summary_sources(conn, provider, session_id, node_id).await?;
    Ok(LcmDescribeSummaryNode {
        node_id: row.get(0)?,
        conversation_id: row.get(1)?,
        depth: row.get(2)?,
        summary_token_count: row.get(3)?,
        source_token_count: row.get(4)?,
        source_time_start: row.get(5)?,
        source_time_end: row.get(6)?,
        expand_hint: row.get(7)?,
        metadata_json: row.get(8)?,
        created_at: row.get(9)?,
        source_count: children.len(),
        children,
    })
}

enum DescribeSource {
    Raw { store_id: i64 },
    Summary { node_id: String },
}

async fn describe_summary_sources(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<Vec<LcmDescribeSourceOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT source_kind, source_id
             FROM lcm_summary_sources
             WHERE node_id = ?1
             ORDER BY ordinal",
            params![node_id],
        )
        .await?;
    let mut sources = Vec::new();
    while let Some(row) = rows.next().await? {
        let source_kind: String = row.get(0)?;
        let source_id: String = row.get(1)?;
        match source_kind.as_str() {
            "raw_message" => {
                let store_id = source_id
                    .parse::<i64>()
                    .map_err(|err| LcmError::Db(format!("invalid raw source id: {err}")))?;
                sources.push(DescribeSource::Raw { store_id });
            }
            "summary_node" => sources.push(DescribeSource::Summary { node_id: source_id }),
            _ => {}
        }
    }
    let raw_store_ids = sources
        .iter()
        .filter_map(|source| match source {
            DescribeSource::Raw { store_id } => Some(*store_id),
            DescribeSource::Summary { .. } => None,
        })
        .collect::<Vec<_>>();
    let summary_node_ids = sources
        .iter()
        .filter_map(|source| match source {
            DescribeSource::Raw { .. } => None,
            DescribeSource::Summary { node_id } => Some(node_id.clone()),
        })
        .collect::<Vec<_>>();
    let raw_by_store_id =
        load_describe_raw_messages(conn, provider, session_id, &raw_store_ids).await?;
    let summary_by_node_id =
        load_describe_summary_nodes(conn, provider, session_id, &summary_node_ids).await?;
    let mut out = Vec::new();
    for source in sources {
        match source {
            DescribeSource::Raw { store_id } => {
                let Some((role, storage_kind_text)) = raw_by_store_id.get(&store_id) else {
                    continue;
                };
                out.push(LcmDescribeSourceOverview {
                    source_kind: "raw_message".to_string(),
                    source_ref: LcmSourceRef::RawMessage { store_id },
                    store_id: Some(store_id),
                    node_id: None,
                    role: Some(role.clone()),
                    storage_kind: LcmStorageKind::from_db(storage_kind_text),
                    summary_token_count: None,
                    source_token_count: None,
                    expand_hint: None,
                });
            }
            DescribeSource::Summary { node_id } => {
                let Some((summary_token_count, source_token_count, expand_hint)) =
                    summary_by_node_id.get(&node_id)
                else {
                    continue;
                };
                out.push(LcmDescribeSourceOverview {
                    source_kind: "summary_node".to_string(),
                    source_ref: LcmSourceRef::SummaryNode {
                        node_id: node_id.clone(),
                    },
                    store_id: None,
                    node_id: Some(node_id),
                    role: None,
                    storage_kind: None,
                    summary_token_count: Some(*summary_token_count),
                    source_token_count: Some(*source_token_count),
                    expand_hint: expand_hint.clone(),
                });
            }
        }
    }
    Ok(out)
}

async fn load_describe_raw_messages(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    store_ids: &[i64],
) -> Result<HashMap<i64, (String, String)>, LcmError> {
    let mut out = HashMap::new();
    for chunk in store_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT store_id, role, storage_kind
             FROM lcm_raw_messages
             WHERE provider = ? AND session_id = ? AND store_id IN ({})",
            sql_in_placeholders(chunk.len())
        );
        let mut values = vec![
            Value::Text(provider.to_string()),
            Value::Text(session_id.to_string()),
        ];
        values.extend(chunk.iter().copied().map(Value::Integer));
        let mut rows = conn.query(&sql, values).await?;
        while let Some(row) = rows.next().await? {
            out.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        }
    }
    Ok(out)
}

async fn load_describe_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_ids: &[String],
) -> Result<HashMap<String, (i64, i64, Option<String>)>, LcmError> {
    let mut out = HashMap::new();
    for chunk in node_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT node_id, summary_token_count, source_token_count, expand_hint
             FROM lcm_summary_nodes
             WHERE provider = ? AND session_id = ? AND node_id IN ({})",
            sql_in_placeholders(chunk.len())
        );
        let mut values = vec![
            Value::Text(provider.to_string()),
            Value::Text(session_id.to_string()),
        ];
        values.extend(chunk.iter().cloned().map(Value::Text));
        let mut rows = conn.query(&sql, values).await?;
        while let Some(row) = rows.next().await? {
            out.insert(row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?));
        }
    }
    Ok(out)
}

pub(super) async fn describe_external_payload(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<LcmDescribeExternalPayload, LcmError> {
    payload::validate_payload_ref(payload_ref)?;
    let payload = payload::load_payload_metadata(conn, payload_ref).await?;
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotFound);
    }
    Ok(LcmDescribeExternalPayload {
        payload_ref: payload.payload_ref,
        provider: payload.provider,
        session_id: payload.session_id.clone(),
        message_id: payload.message_id.clone(),
        kind: payload.kind,
        content_hash: payload.content_hash,
        byte_count: payload.byte_count,
        char_count: payload.char_count,
        created_at: payload.created_at,
        metadata_json: payload.metadata_json,
        content_preview: external_payload_placeholder_preview(
            conn,
            provider,
            session_id,
            &payload.message_id,
            payload_ref,
        )
        .await?,
    })
}

async fn external_payload_placeholder_preview(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    message_id: &str,
    payload_ref: &str,
) -> Result<String, LcmError> {
    let mut rows = conn
        .query(
            "SELECT snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND session_id = ?2
               AND message_id = ?3
               AND payload_ref = ?4
             LIMIT 1",
            params![provider, session_id, message_id, payload_ref],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        return Ok(row.get(0)?);
    }
    Ok(format!("[externalized payload ref={payload_ref}]"))
}
