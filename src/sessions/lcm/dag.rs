use std::collections::{BTreeMap, BTreeSet};

use libsql::{params, Connection, Value};

use super::{
    raw, LcmError, LcmExpandedSummarySource, LcmRawMessage, LcmSourceRef, LcmStorageKind,
    LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft,
};

pub(crate) async fn insert_summary_node(
    conn: &Connection,
    draft: LcmSummaryNodeDraft,
) -> Result<LcmSummaryNode, LcmError> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let summary = match insert_summary_node_in_transaction(conn, draft).await {
        Ok(summary) => summary,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
    };

    match conn.execute("COMMIT", ()).await {
        Ok(_) => Ok(summary),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(LcmError::Db(err.to_string()))
        }
    }
}

pub(crate) async fn insert_summary_node_in_transaction(
    conn: &Connection,
    draft: LcmSummaryNodeDraft,
) -> Result<LcmSummaryNode, LcmError> {
    let summary_hash = raw::sha256_hex(&draft.summary_text);
    let node_id = summary_node_id(
        &draft.provider,
        &draft.session_id,
        draft.depth,
        &draft.source_refs,
        &summary_hash,
    );

    validate_summary_sources(conn, &draft, &node_id).await?;
    upsert_summary_node(conn, &node_id, &summary_hash, &draft).await?;
    replace_summary_sources(conn, &node_id, &draft.source_refs).await?;
    load_summary_node(conn, &draft.provider, &draft.session_id, &node_id).await
}

pub(crate) async fn expand_summary_node(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmSummaryExpansion, LcmError> {
    let summary = load_summary_node(conn, provider, session_id, node_id).await?;
    let mut raw_store_ids = Vec::new();
    let mut child_node_ids = Vec::new();
    for source_ref in &summary.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => raw_store_ids.push(*store_id),
            LcmSourceRef::SummaryNode { node_id } => child_node_ids.push(node_id.clone()),
        }
    }
    let raw_sources = load_raw_messages_by_store_ids(conn, &raw_store_ids).await?;
    let child_sources = load_summary_nodes_by_ids(conn, &child_node_ids).await?;

    let mut sources = Vec::with_capacity(summary.source_refs.len());

    for source_ref in &summary.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                let raw = raw_sources
                    .get(store_id)
                    .cloned()
                    .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
                if raw.provider != provider || raw.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    content: raw.content.clone(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: Some(raw),
                    summary_node: None,
                });
            }
            LcmSourceRef::SummaryNode {
                node_id: child_node_id,
            } => {
                let child = child_sources
                    .get(child_node_id)
                    .cloned()
                    .ok_or(LcmError::SummaryNodeNotFound)?;
                if child.provider != provider || child.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    content: child.summary_text.clone(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: None,
                    summary_node: Some(Box::new(child)),
                });
            }
        }
    }

    Ok(LcmSummaryExpansion { summary, sources })
}

/// One uncondensed summary node plus the earliest raw-message store id in its
/// descendant lineage, used to position the node inside interleaved replay.
#[derive(Debug, Clone)]
pub(crate) struct LcmUncondensedSummaryNode {
    pub(crate) node: LcmSummaryNode,
    pub(crate) first_source_store_id: Option<i64>,
}

/// Loads every summary node for the session that has not been condensed into
/// a higher-depth node. Mirrors hermes-lcm `SummaryDAG.get_uncondensed_at_depth`
/// collapsed across all depths in one query; replay assembly consumes the
/// result ordered by lineage position (then depth, highest first).
pub(crate) async fn load_uncondensed_summary_nodes(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmUncondensedSummaryNode>, LcmError> {
    let mut rows = conn
        .query(
            "WITH RECURSIVE unparented AS (
               SELECT n.node_id, n.provider, n.conversation_id, n.session_id, n.depth,
                      n.summary_text, n.summary_hash, n.summary_token_count,
                      n.source_token_count, n.source_time_start, n.source_time_end,
                      n.expand_hint, n.metadata_json, n.created_at
               FROM lcm_summary_nodes n
               WHERE n.provider = ?1 AND n.session_id = ?2
                 AND NOT EXISTS (
                   SELECT 1
                   FROM lcm_summary_sources s
                   WHERE s.source_kind = 'summary_node'
                     AND s.source_id = n.node_id
                 )
             ),
             lineage(root_id, source_kind, source_id) AS (
               SELECT s.node_id, s.source_kind, s.source_id
               FROM lcm_summary_sources s
               JOIN unparented u ON u.node_id = s.node_id
               UNION ALL
               SELECT l.root_id, s.source_kind, s.source_id
               FROM lineage l
               JOIN lcm_summary_sources s
                 ON l.source_kind = 'summary_node' AND s.node_id = l.source_id
             ),
             first_raw AS (
               SELECT root_id, MIN(CAST(source_id AS INTEGER)) AS first_source_store_id
               FROM lineage
               WHERE source_kind = 'raw_message'
               GROUP BY root_id
             )
             SELECT u.node_id, u.provider, u.conversation_id, u.session_id, u.depth,
                    u.summary_text, u.summary_hash, u.summary_token_count,
                    u.source_token_count, u.source_time_start, u.source_time_end,
                    u.expand_hint, u.metadata_json, u.created_at,
                    first_raw.first_source_store_id
             FROM unparented u
             LEFT JOIN first_raw ON first_raw.root_id = u.node_id
             ORDER BY first_raw.first_source_store_id IS NULL,
                      first_raw.first_source_store_id,
                      u.depth DESC,
                      u.source_time_start IS NULL, u.source_time_start,
                      u.created_at, u.node_id",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        nodes.push(LcmUncondensedSummaryNode {
            node: LcmSummaryNode {
                node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
                provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
                conversation_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
                session_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
                depth: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_text: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_hash: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_token_count: row.get(7).map_err(|err| LcmError::Db(err.to_string()))?,
                source_token_count: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
                source_time_start: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
                source_time_end: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
                expand_hint: row.get(11).map_err(|err| LcmError::Db(err.to_string()))?,
                metadata_json: row.get(12).map_err(|err| LcmError::Db(err.to_string()))?,
                created_at: row.get(13).map_err(|err| LcmError::Db(err.to_string()))?,
                source_refs: Vec::new(),
            },
            first_source_store_id: row.get(14).map_err(|err| LcmError::Db(err.to_string()))?,
        });
    }
    Ok(nodes)
}

/// Moves all summary nodes from one session id to another inside the caller's
/// transaction, preserving node ids and node-to-node lineage. Mirrors
/// hermes-lcm `SummaryDAG.reassign_session_nodes`.
pub(crate) async fn reassign_session_nodes(
    conn: &Connection,
    provider: &str,
    old_session_id: &str,
    new_session_id: &str,
) -> Result<u64, LcmError> {
    if old_session_id.is_empty() || new_session_id.is_empty() || old_session_id == new_session_id {
        return Ok(0);
    }
    conn.execute(
        "UPDATE lcm_summary_nodes
         SET session_id = ?3, conversation_id = ?3
         WHERE provider = ?1 AND session_id = ?2",
        params![provider, old_session_id, new_session_id],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))
}

pub fn summary_node_id(
    provider: &str,
    session_id: &str,
    depth: i64,
    source_refs: &[LcmSourceRef],
    summary_hash: &str,
) -> String {
    let input = serde_json::json!({
        "provider": provider,
        "session_id": session_id,
        "depth": depth,
        "source_refs": source_refs,
        "summary_hash": summary_hash,
    });
    format!("sum_{}", raw::sha256_hex(&input.to_string()))
}

async fn upsert_summary_node(
    conn: &Connection,
    node_id: &str,
    summary_hash: &str,
    draft: &LcmSummaryNodeDraft,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_summary_nodes (
            node_id, provider, conversation_id, session_id, depth, summary_text,
            summary_hash, summary_token_count, source_token_count, source_time_start,
            source_time_end, expand_hint, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(node_id) DO UPDATE SET
            provider = excluded.provider,
            conversation_id = excluded.conversation_id,
            session_id = excluded.session_id,
            depth = excluded.depth,
            summary_text = excluded.summary_text,
            summary_hash = excluded.summary_hash,
            summary_token_count = excluded.summary_token_count,
            source_token_count = excluded.source_token_count,
            source_time_start = excluded.source_time_start,
            source_time_end = excluded.source_time_end,
            expand_hint = excluded.expand_hint,
            metadata_json = excluded.metadata_json",
        params![
            node_id,
            draft.provider.as_str(),
            draft.conversation_id.as_str(),
            draft.session_id.as_str(),
            draft.depth,
            draft.summary_text.as_str(),
            summary_hash,
            draft.summary_token_count,
            draft.source_token_count,
            opt_i64(draft.source_time_start),
            opt_i64(draft.source_time_end),
            opt_text(draft.expand_hint.as_deref()),
            opt_text(draft.metadata_json.as_deref()),
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn validate_summary_sources(
    conn: &Connection,
    draft: &LcmSummaryNodeDraft,
    node_id: &str,
) -> Result<(), LcmError> {
    let mut raw_store_ids = Vec::new();
    let mut child_node_ids = Vec::new();
    for source_ref in &draft.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => raw_store_ids.push(*store_id),
            LcmSourceRef::SummaryNode {
                node_id: child_node_id,
            } => {
                if child_node_id == node_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                child_node_ids.push(child_node_id.clone());
            }
        }
    }

    let raw_owners = load_raw_message_owners_by_store_ids(conn, &raw_store_ids).await?;
    for store_id in raw_store_ids {
        let Some((provider, session_id)) = raw_owners.get(&store_id) else {
            return Err(LcmError::SummarySourceNotOwnedBySession);
        };
        if provider != &draft.provider || session_id != &draft.session_id {
            return Err(LcmError::SummarySourceNotOwnedBySession);
        }
    }

    let child_owners = load_summary_node_owners_by_ids(conn, &child_node_ids).await?;
    for child_node_id in child_node_ids {
        let Some((provider, session_id)) = child_owners.get(child_node_id.as_str()) else {
            return Err(LcmError::SummaryNodeNotFound);
        };
        if provider != &draft.provider || session_id != &draft.session_id {
            return Err(LcmError::SummarySourceNotOwnedBySession);
        }
    }
    Ok(())
}

async fn replace_summary_sources(
    conn: &Connection,
    node_id: &str,
    source_refs: &[LcmSourceRef],
) -> Result<(), LcmError> {
    conn.execute(
        "DELETE FROM lcm_summary_sources WHERE node_id = ?1",
        params![node_id],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;

    if source_refs.is_empty() {
        return Ok(());
    }

    let placeholders = std::iter::repeat_n("(?, ?, ?, ?)", source_refs.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
         VALUES {placeholders}"
    );
    let mut values = Vec::with_capacity(source_refs.len() * 4);
    for (ordinal, source_ref) in source_refs.iter().enumerate() {
        let (source_kind, source_id) = source_ref_to_db(source_ref);
        values.push(Value::Text(node_id.to_string()));
        values.push(Value::Text(source_kind.to_string()));
        values.push(Value::Text(source_id));
        values.push(Value::Integer(ordinal as i64));
    }
    conn.execute(&sql, values)
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn load_summary_node(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmSummaryNode, LcmError> {
    let node = load_summary_node_by_id(conn, node_id).await?;
    if node.provider == provider && node.session_id == session_id {
        Ok(node)
    } else {
        Err(LcmError::SummaryNodeNotFound)
    }
}

async fn load_summary_node_by_id(
    conn: &Connection,
    node_id: &str,
) -> Result<LcmSummaryNode, LcmError> {
    let mut rows = conn
        .query(
            "SELECT node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count, source_time_start,
                    source_time_end, expand_hint, metadata_json, created_at
             FROM lcm_summary_nodes
             WHERE node_id = ?1",
            params![node_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::SummaryNodeNotFound)?;
    let source_refs = load_summary_source_refs(conn, node_id).await?;
    Ok(LcmSummaryNode {
        node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        conversation_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        session_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        depth: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        summary_text: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        summary_hash: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
        summary_token_count: row.get(7).map_err(|err| LcmError::Db(err.to_string()))?,
        source_token_count: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        source_time_start: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
        source_time_end: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
        expand_hint: row.get(11).map_err(|err| LcmError::Db(err.to_string()))?,
        metadata_json: row.get(12).map_err(|err| LcmError::Db(err.to_string()))?,
        created_at: row.get(13).map_err(|err| LcmError::Db(err.to_string()))?,
        source_refs,
    })
}

async fn load_summary_source_refs(
    conn: &Connection,
    node_id: &str,
) -> Result<Vec<LcmSourceRef>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT source_kind, source_id
             FROM lcm_summary_sources
             WHERE node_id = ?1
             ORDER BY ordinal",
            params![node_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut source_refs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let source_kind: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_id: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        source_refs.push(source_ref_from_db(&source_kind, &source_id)?);
    }
    Ok(source_refs)
}

async fn load_raw_messages_by_store_ids(
    conn: &Connection,
    store_ids: &[i64],
) -> Result<BTreeMap<i64, LcmRawMessage>, LcmError> {
    let unique_store_ids = store_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_store_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = std::iter::repeat_n("?", unique_store_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT provider, message_id, session_id, store_id, role, ordinal,
                timestamp, content, content_hash, storage_kind, payload_ref,
                snippet_text, legacy_source, legacy_truncated, metadata_json
         FROM lcm_raw_messages
         WHERE store_id IN ({placeholders})"
    );
    let mut rows = conn
        .query(
            &sql,
            unique_store_ids
                .iter()
                .map(|store_id| Value::Integer(*store_id))
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let raw = raw_message_from_row(&row)?;
        out.insert(raw.store_id, raw);
    }
    Ok(out)
}

async fn load_summary_nodes_by_ids(
    conn: &Connection,
    node_ids: &[String],
) -> Result<BTreeMap<String, LcmSummaryNode>, LcmError> {
    let unique_node_ids = node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_node_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = std::iter::repeat_n("?", unique_node_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let node_sql = format!(
        "SELECT node_id, provider, conversation_id, session_id, depth, summary_text,
                summary_hash, summary_token_count, source_token_count, source_time_start,
                source_time_end, expand_hint, metadata_json, created_at
         FROM lcm_summary_nodes
         WHERE node_id IN ({placeholders})"
    );
    let values = unique_node_ids
        .iter()
        .map(|node_id| Value::Text(node_id.clone()))
        .collect::<Vec<_>>();
    let mut node_rows = conn
        .query(&node_sql, values.clone())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut nodes = BTreeMap::new();
    while let Some(row) = node_rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let node_id: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        nodes.insert(
            node_id.clone(),
            LcmSummaryNode {
                node_id,
                provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
                conversation_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
                session_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
                depth: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_text: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_hash: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
                summary_token_count: row.get(7).map_err(|err| LcmError::Db(err.to_string()))?,
                source_token_count: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
                source_time_start: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
                source_time_end: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
                expand_hint: row.get(11).map_err(|err| LcmError::Db(err.to_string()))?,
                metadata_json: row.get(12).map_err(|err| LcmError::Db(err.to_string()))?,
                created_at: row.get(13).map_err(|err| LcmError::Db(err.to_string()))?,
                source_refs: Vec::new(),
            },
        );
    }
    let source_sql = format!(
        "SELECT node_id, source_kind, source_id
         FROM lcm_summary_sources
         WHERE node_id IN ({placeholders})
         ORDER BY node_id, ordinal"
    );
    let mut source_rows = conn
        .query(&source_sql, values)
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = source_rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let node_id: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_kind: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_id: String = row.get(2).map_err(|err| LcmError::Db(err.to_string()))?;
        if let Some(node) = nodes.get_mut(&node_id) {
            node.source_refs
                .push(source_ref_from_db(&source_kind, &source_id)?);
        }
    }
    Ok(nodes)
}

async fn load_raw_message_owners_by_store_ids(
    conn: &Connection,
    store_ids: &[i64],
) -> Result<BTreeMap<i64, (String, String)>, LcmError> {
    let unique_store_ids = store_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_store_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = std::iter::repeat_n("?", unique_store_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT store_id, provider, session_id
         FROM lcm_raw_messages
         WHERE store_id IN ({placeholders})"
    );
    let mut rows = conn
        .query(
            &sql,
            unique_store_ids
                .iter()
                .map(|store_id| Value::Integer(*store_id))
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let store_id: i64 = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let provider: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        let session_id: String = row.get(2).map_err(|err| LcmError::Db(err.to_string()))?;
        out.insert(store_id, (provider, session_id));
    }
    Ok(out)
}

async fn load_summary_node_owners_by_ids(
    conn: &Connection,
    node_ids: &[String],
) -> Result<BTreeMap<String, (String, String)>, LcmError> {
    let unique_node_ids = node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_node_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = std::iter::repeat_n("?", unique_node_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT node_id, provider, session_id
         FROM lcm_summary_nodes
         WHERE node_id IN ({placeholders})"
    );
    let mut rows = conn
        .query(
            &sql,
            unique_node_ids
                .iter()
                .map(|node_id| Value::Text(node_id.clone()))
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let node_id: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let provider: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        let session_id: String = row.get(2).map_err(|err| LcmError::Db(err.to_string()))?;
        out.insert(node_id, (provider, session_id));
    }
    Ok(out)
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

fn source_ref_to_db(source_ref: &LcmSourceRef) -> (&'static str, String) {
    match source_ref {
        LcmSourceRef::RawMessage { store_id } => ("raw_message", store_id.to_string()),
        LcmSourceRef::SummaryNode { node_id } => ("summary_node", node_id.clone()),
    }
}

fn source_ref_from_db(source_kind: &str, source_id: &str) -> Result<LcmSourceRef, LcmError> {
    match source_kind {
        "raw_message" => source_id
            .parse::<i64>()
            .map(|store_id| LcmSourceRef::RawMessage { store_id })
            .map_err(|err| LcmError::Db(err.to_string())),
        "summary_node" => Ok(LcmSourceRef::SummaryNode {
            node_id: source_id.to_string(),
        }),
        _ => Err(LcmError::Db(format!(
            "invalid summary source_kind: {source_kind}"
        ))),
    }
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}
