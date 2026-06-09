use libsql::{params, Connection, Value};

use super::{
    raw, LcmError, LcmExpandedSummarySource, LcmRawMessage, LcmSourceRef, LcmStorageKind,
    LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft,
};

pub(crate) async fn insert_summary_node(
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

    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    if let Err(err) = validate_summary_sources(conn, &draft, &node_id).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }
    if let Err(err) = upsert_summary_node(conn, &node_id, &summary_hash, &draft).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }
    if let Err(err) = replace_summary_sources(conn, &node_id, &draft.source_refs).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }

    match conn.execute("COMMIT", ()).await {
        Ok(_) => load_summary_node(conn, &draft.provider, &draft.session_id, &node_id).await,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(LcmError::Db(err.to_string()))
        }
    }
}

pub(crate) async fn expand_summary_node(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmSummaryExpansion, LcmError> {
    let summary = load_summary_node(conn, provider, session_id, node_id).await?;
    let mut sources = Vec::with_capacity(summary.source_refs.len());

    for source_ref in &summary.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                let raw = load_raw_message_by_store_id(conn, *store_id).await?;
                if raw.provider != provider || raw.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    content: raw.content.clone(),
                    raw_message: Some(raw),
                    summary_node: None,
                });
            }
            LcmSourceRef::SummaryNode {
                node_id: child_node_id,
            } => {
                let child = load_summary_node_by_id(conn, child_node_id).await?;
                if child.provider != provider || child.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    content: child.summary_text.clone(),
                    raw_message: None,
                    summary_node: Some(Box::new(child)),
                });
            }
        }
    }

    Ok(LcmSummaryExpansion { summary, sources })
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
    for source_ref in &draft.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                let (provider, session_id) = load_raw_message_owner_by_store_id(conn, *store_id)
                    .await?
                    .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
                if provider != draft.provider || session_id != draft.session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
            }
            LcmSourceRef::SummaryNode {
                node_id: child_node_id,
            } => {
                if child_node_id == node_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                let child = load_summary_node_by_id(conn, child_node_id).await?;
                if child.provider != draft.provider || child.session_id != draft.session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
            }
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

    for (ordinal, source_ref) in source_refs.iter().enumerate() {
        let (source_kind, source_id) = source_ref_to_db(source_ref);
        conn.execute(
            "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, source_kind, source_id.as_str(), ordinal as i64],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    }
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

async fn load_raw_message_by_store_id(
    conn: &Connection,
    store_id: i64,
) -> Result<LcmRawMessage, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE store_id = ?1",
            params![store_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
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

async fn load_raw_message_owner_by_store_id(
    conn: &Connection,
    store_id: i64,
) -> Result<Option<(String, String)>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, session_id
             FROM lcm_raw_messages
             WHERE store_id = ?1",
            params![store_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    rows.next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .map(|row| {
            Ok((
                row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
                row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            ))
        })
        .transpose()
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
