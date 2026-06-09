use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use libsql::{params, Connection, Value};

use super::types::{LcmLifecycleStatus, LcmPayloadStatus, LcmRedactionStatus};
use super::{
    compression, dag, payload, raw, schema, LcmContentRange, LcmContentSlice, LcmDescribeResponse,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandTarget, LcmExpandedSummarySource,
    LcmGrepHit, LcmGrepRequest, LcmLoadSessionMessage, LcmLoadSessionPage, LcmLoadSessionRequest,
    LcmRawMessage, LcmRawMessageOverview, LcmScope, LcmStatus, LcmStorageKind, LcmSummaryNode,
    LcmSummaryNodeOverview, LCM_SCHEMA_VERSION,
};

const MAX_PAGE_LIMIT: usize = 100;

struct LcmLifecycleMetadata {
    current_session_id: Option<String>,
    current_frontier_store_id: Option<i64>,
    last_finalized_session_id: Option<String>,
    last_finalized_frontier_store_id: Option<i64>,
}

pub(crate) async fn load_session(
    conn: &Connection,
    request: LcmLoadSessionRequest,
) -> Result<LcmLoadSessionPage, LcmError> {
    let limit = clamp_limit(request.limit);
    let fetch_limit = limit.saturating_add(1);
    let role = opt_text(request.role.as_deref());
    let start_time = opt_i64(request.start_time);
    let end_time = opt_i64(request.end_time);
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND session_id = ?2
               AND store_id > ?3
               AND (?4 IS NULL OR role = ?4)
               AND (?5 IS NULL OR timestamp >= ?5)
               AND (?6 IS NULL OR timestamp <= ?6)
             ORDER BY store_id
             LIMIT ?7",
            params![
                request.provider.as_str(),
                request.session_id.as_str(),
                request.after_store_id.unwrap_or(0),
                role,
                start_time,
                end_time,
                fetch_limit as i64,
            ],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut messages = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let raw = raw_message_from_row(&row)?;
        messages.push(load_message_from_raw(raw, request.content_slice));
    }

    let has_more = messages.len() > limit;
    if has_more {
        messages.truncate(limit);
    }
    let next_cursor = if has_more {
        messages.last().map(|message| message.store_id.to_string())
    } else {
        None
    };

    Ok(LcmLoadSessionPage {
        request,
        messages,
        next_cursor,
    })
}

pub(crate) async fn grep(
    conn: &Connection,
    request: LcmGrepRequest,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let query = fts_query(&request.query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let limit = clamp_limit(request.limit);
    let session_filter = scoped_session_filter(request.scope, request.session_id.as_deref());
    if matches!(request.scope, LcmScope::Current | LcmScope::Session) && session_filter.is_none() {
        return Ok(Vec::new());
    }

    let mut hits = raw_grep_hits(conn, &request.provider, session_filter, &query, limit).await?;
    if request.include_summaries && hits.len() < limit {
        let remaining = limit - hits.len();
        hits.extend(
            summary_grep_hits(conn, &request.provider, session_filter, &query, remaining).await?,
        );
    }
    Ok(hits)
}

pub(crate) async fn expand(
    conn: &Connection,
    storage_root: &Path,
    request: LcmExpandRequest,
) -> Result<LcmExpandResponse, LcmError> {
    match request.target {
        LcmExpandTarget::RawMessage { store_id } => {
            let raw = load_raw_message_by_store_id(conn, store_id).await?;
            if raw.provider != request.provider || raw.session_id != request.session_id {
                return Err(LcmError::SummarySourceNotOwnedBySession);
            }
            let (raw, range) = raw_message_with_sliced_content(raw, request.content_slice);
            let content = raw.content.clone();
            Ok(LcmExpandResponse {
                kind: "raw_message".to_string(),
                content,
                content_range: range,
                raw_message: Some(raw),
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: None,
            })
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            let expansion =
                dag::expand_summary_node(conn, &request.provider, &request.session_id, &node_id)
                    .await?;
            let (summary, range) =
                summary_node_with_sliced_text(expansion.summary, request.content_slice);
            let content = summary.summary_text.clone();
            let summary_sources = slice_summary_sources(expansion.sources, request.content_slice);
            Ok(LcmExpandResponse {
                kind: "summary_node".to_string(),
                content,
                content_range: range,
                raw_message: None,
                summary_node: Some(summary),
                summary_sources,
                payload_ref: None,
            })
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            let slice = request.content_slice.unwrap_or(LcmContentSlice {
                offset: 0,
                limit: usize::MAX,
            });
            let store = payload::LcmStore::new(conn, storage_root.to_path_buf());
            let expansion = store
                .lcm_expand_payload(
                    &request.provider,
                    &request.session_id,
                    &payload_ref,
                    slice.offset,
                    slice.limit,
                )
                .await?;
            let range = LcmContentRange {
                offset: expansion.offset,
                limit: slice.limit as u64,
                returned_chars: expansion.char_count,
                total_chars: expansion.total_char_count,
                truncated: expansion.offset > 0
                    || expansion.offset.saturating_add(expansion.char_count)
                        < expansion.total_char_count,
            };
            Ok(LcmExpandResponse {
                kind: "external_payload".to_string(),
                content: expansion.content,
                content_range: range,
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(expansion.payload_ref),
            })
        }
    }
}

pub(crate) async fn describe(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<LcmDescribeResponse, LcmError> {
    let raw_message_count = count_raw_messages(conn, provider, Some(session_id)).await?;
    let summary_node_count = count_summary_nodes(conn, provider, Some(session_id)).await?;
    let external_payload_count = count_external_payloads(conn, provider, Some(session_id)).await?;
    let (first_store_id, last_store_id) = raw_store_bounds(conn, provider, session_id).await?;
    let raw_messages = raw_message_overviews(conn, provider, session_id).await?;
    let summary_nodes = summary_overviews(conn, provider, session_id).await?;

    Ok(LcmDescribeResponse {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        raw_message_count,
        summary_node_count,
        external_payload_count,
        first_store_id,
        last_store_id,
        raw_messages,
        summary_nodes,
    })
}

pub(crate) async fn status(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmStatus, LcmError> {
    let external_payload_count = count_external_payloads(conn, provider, session_id).await?;
    let missing_payload_count =
        count_missing_payloads(conn, storage_root, provider, session_id).await?;
    let unreferenced_payload_count = count_unreferenced_payloads(conn, storage_root).await?;
    let maintenance_debt_count =
        compression::maintenance_debt_count(conn, provider, session_id).await?;
    let lifecycle_state_count = count_lifecycle_states(conn, provider, session_id).await?;
    let frontier_count = count_frontier_rows(conn, provider, session_id).await?;
    let lifecycle_metadata = load_lifecycle_metadata(conn, provider, session_id).await?;
    let legacy_truncated_count = count_legacy_truncated(conn, provider, session_id).await?;

    Ok(LcmStatus {
        schema_version: schema::schema_version(conn)
            .await
            .unwrap_or(LCM_SCHEMA_VERSION),
        storage_scope: Some("project_local".to_string()),
        raw_message_count: count_raw_messages(conn, provider, session_id).await?,
        summary_node_count: count_summary_nodes(conn, provider, session_id).await?,
        external_payload_count,
        missing_payload_count,
        unreferenced_payload_count,
        maintenance_debt_count,
        payload: LcmPayloadStatus {
            externalized_count: external_payload_count,
            missing_count: missing_payload_count,
            unreferenced_count: unreferenced_payload_count,
            root_contained: payload_root_contained(storage_root),
        },
        lifecycle: LcmLifecycleStatus {
            lifecycle_state_count,
            frontier_count,
            maintenance_debt_count,
            current_session_id: lifecycle_metadata.current_session_id,
            current_frontier_store_id: lifecycle_metadata.current_frontier_store_id,
            last_finalized_session_id: lifecycle_metadata.last_finalized_session_id,
            last_finalized_frontier_store_id: lifecycle_metadata.last_finalized_frontier_store_id,
        },
        redaction: LcmRedactionStatus {
            enabled: false,
            lossy_records: legacy_truncated_count,
            legacy_truncated_count,
        },
    })
}

fn load_message_from_raw(
    raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> LcmLoadSessionMessage {
    let (content, content_range) = slice_content(&raw.content, slice);
    LcmLoadSessionMessage {
        provider: raw.provider,
        message_id: raw.message_id,
        session_id: raw.session_id,
        store_id: raw.store_id,
        role: raw.role,
        ordinal: raw.ordinal,
        timestamp: raw.timestamp,
        content,
        content_range,
        content_hash: raw.content_hash,
        storage_kind: raw.storage_kind,
        payload_ref: raw.payload_ref,
        legacy_source: raw.legacy_source,
        legacy_truncated: raw.legacy_truncated,
        metadata_json: raw.metadata_json,
    }
}

fn slice_content(content: &str, slice: Option<LcmContentSlice>) -> (String, LcmContentRange) {
    let total_chars = content.chars().count();
    let offset = slice.map_or(0, |slice| slice.offset).min(total_chars);
    let limit = slice.map_or(total_chars.saturating_sub(offset), |slice| slice.limit);
    let sliced = content.chars().skip(offset).take(limit).collect::<String>();
    let returned_chars = sliced.chars().count();
    let truncated = offset > 0 || offset.saturating_add(returned_chars) < total_chars;
    (
        sliced,
        LcmContentRange {
            offset: offset as u64,
            limit: limit as u64,
            returned_chars: returned_chars as u64,
            total_chars: total_chars as u64,
            truncated,
        },
    )
}

fn raw_message_with_sliced_content(
    mut raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> (LcmRawMessage, LcmContentRange) {
    let (content, range) = slice_content(&raw.content, slice);
    raw.content = content;
    (raw, range)
}

fn summary_node_with_sliced_text(
    mut summary: LcmSummaryNode,
    slice: Option<LcmContentSlice>,
) -> (LcmSummaryNode, LcmContentRange) {
    let (summary_text, range) = slice_content(&summary.summary_text, slice);
    summary.summary_text = summary_text;
    (summary, range)
}

fn slice_summary_sources(
    sources: Vec<LcmExpandedSummarySource>,
    slice: Option<LcmContentSlice>,
) -> Vec<LcmExpandedSummarySource> {
    sources
        .into_iter()
        .map(|mut source| {
            let (content, range) = slice_content(&source.content, slice);
            source.content = content.clone();
            source.content_range = Some(range);
            if let Some(raw_message) = source.raw_message.as_mut() {
                raw_message.content = content.clone();
            }
            if let Some(summary_node) = source.summary_node.as_mut() {
                summary_node.summary_text = content;
            }
            source
        })
        .collect()
}

async fn raw_grep_hits(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages r ON r.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH ?1
               AND r.provider = ?2
               AND (?3 IS NULL OR r.session_id = ?3)
             ORDER BY r.store_id
             LIMIT ?4",
            params![query, provider, session_value, limit as i64],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut hits = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let snippet: String = row.get(4).map_err(|err| LcmError::Db(err.to_string()))?;
        hits.push(LcmGrepHit {
            kind: "raw_message".to_string(),
            provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            session_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            message_id: Some(row.get(2).map_err(|err| LcmError::Db(err.to_string()))?),
            node_id: None,
            store_id: Some(row.get(3).map_err(|err| LcmError::Db(err.to_string()))?),
            snippet: raw::derived_text_for_snippet(&snippet),
        });
    }
    Ok(hits)
}

async fn summary_grep_hits(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT n.provider, n.session_id, n.node_id, n.summary_text
             FROM lcm_summary_nodes_fts
             JOIN lcm_summary_nodes n ON n.rowid = lcm_summary_nodes_fts.rowid
             WHERE lcm_summary_nodes_fts MATCH ?1
               AND n.provider = ?2
               AND (?3 IS NULL OR n.session_id = ?3)
             ORDER BY n.created_at, n.node_id
             LIMIT ?4",
            params![query, provider, session_value, limit as i64],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut hits = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let summary_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        hits.push(LcmGrepHit {
            kind: "summary_node".to_string(),
            provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            session_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            message_id: None,
            node_id: Some(row.get(2).map_err(|err| LcmError::Db(err.to_string()))?),
            store_id: None,
            snippet: raw::derived_text_for_snippet(&summary_text),
        });
    }
    Ok(hits)
}

async fn raw_message_overviews(
    conn: &Connection,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut overviews = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let storage_kind_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        let content_preview: String = row.get(5).map_err(|err| LcmError::Db(err.to_string()))?;
        let (_, content_range) = slice_content(&content_preview, None);
        overviews.push(LcmRawMessageOverview {
            message_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            store_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            role: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
            storage_kind: LcmStorageKind::from_db(&storage_kind_text).ok_or_else(|| {
                LcmError::Db(format!("invalid storage_kind: {storage_kind_text}"))
            })?,
            payload_ref: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
            content_preview,
            content_range,
        });
    }
    Ok(overviews)
}

async fn summary_overviews(
    conn: &Connection,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut overviews = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let summary_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_count: i64 = row.get(5).map_err(|err| LcmError::Db(err.to_string()))?;
        overviews.push(LcmSummaryNodeOverview {
            node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            conversation_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            depth: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
            summary_preview: raw::derived_text_for_snippet(&summary_text),
            source_count: source_count.max(0) as usize,
            created_at: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        });
    }
    Ok(overviews)
}

async fn raw_store_bounds(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<(Option<i64>, Option<i64>), LcmError> {
    let mut rows = conn
        .query(
            "SELECT MIN(store_id), MAX(store_id)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    else {
        return Ok((None, None));
    };
    Ok((
        row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
    ))
}

async fn count_raw_messages(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await
}

async fn count_summary_nodes(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await
}

async fn count_external_payloads(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_external_payloads", provider, session_id).await
}

async fn count_by_provider_session(
    conn: &Connection,
    table: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let sql = format!(
        "SELECT COUNT(*) FROM {table} WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)"
    );
    let mut rows = conn
        .query(&sql, params![provider, session_value])
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_missing_payloads(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let dir = payload::payload_dir(storage_root);
    let mut missing = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        if payload::validate_payload_ref(&payload_ref).is_err() || !dir.join(&payload_ref).is_file()
        {
            missing += 1;
        }
    }
    Ok(missing)
}

async fn count_unreferenced_payloads(
    conn: &Connection,
    storage_root: &Path,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut referenced = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        referenced.insert(payload_ref);
    }

    let dir = payload::payload_dir(storage_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(0);
    };

    let mut unreferenced = 0_i64;
    for entry in entries {
        let entry = entry.map_err(|err| LcmError::Io(err.to_string()))?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|err| LcmError::Io(err.to_string()))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            unreferenced += 1;
            continue;
        };
        if payload::validate_payload_ref(&file_name).is_err() || !referenced.contains(&file_name) {
            unreferenced += 1;
        }
    }
    Ok(unreferenced)
}

fn payload_root_contained(storage_root: &Path) -> bool {
    let dir = payload::payload_dir(storage_root);
    if !dir.exists() {
        return true;
    }
    let Ok(root) = storage_root.canonicalize() else {
        return false;
    };
    let Ok(canonical_dir) = dir.canonicalize() else {
        return false;
    };
    canonical_dir.parent() == Some(root.as_path())
}

async fn count_lifecycle_states(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("lifecycle count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn load_lifecycle_metadata(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmLifecycleMetadata, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)
             ORDER BY updated_at DESC, conversation_id DESC
             LIMIT 1",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    else {
        return Ok(LcmLifecycleMetadata {
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        });
    };
    Ok(LcmLifecycleMetadata {
        current_session_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        current_frontier_store_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_frontier_store_id: row
            .get(3)
            .map_err(|err| LcmError::Db(err.to_string()))?,
    })
}

async fn count_frontier_rows(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2)
               AND current_frontier_store_id IS NOT NULL",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("frontier count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_legacy_truncated(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND legacy_truncated != 0",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("legacy truncated count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
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
    raw_message_from_row(&row)
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

fn scoped_session_filter(scope: LcmScope, session_id: Option<&str>) -> Option<&str> {
    match scope {
        LcmScope::All => None,
        LcmScope::Current | LcmScope::Session => session_id,
    }
}

fn fts_query(query: &str) -> String {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }

    terms
        .into_iter()
        .map(|term| quote_fts_term(&term))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_fts_term(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_PAGE_LIMIT)
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}
