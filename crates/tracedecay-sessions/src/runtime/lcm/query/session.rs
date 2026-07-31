use super::*;

pub async fn load_session(
    conn: &(impl QueryExecutor + ?Sized),
    request: LcmLoadSessionRequest,
) -> Result<LcmLoadSessionPage, LcmError> {
    let limit = clamp_limit(request.limit);
    let fetch_limit = limit.saturating_add(1);
    let mut values = vec![
        Value::Text(request.provider.clone()),
        Value::Text(request.provider.clone()),
        Value::Text(request.session_id.clone()),
        Value::Integer(request.after_store_id.unwrap_or(0)),
    ];
    let mut role_clause = String::new();
    let roles = normalized_strings(&request.roles);
    if !roles.is_empty() {
        let placeholders = std::iter::repeat_n("?", roles.len())
            .collect::<Vec<_>>()
            .join(", ");
        role_clause = format!(" AND role IN ({placeholders})");
        values.extend(roles.into_iter().map(Value::Text));
    }
    values.push(request.start_time.map_or(Value::Null, Value::Integer));
    values.push(request.start_time.map_or(Value::Null, Value::Integer));
    values.push(request.end_time.map_or(Value::Null, Value::Integer));
    values.push(request.end_time.map_or(Value::Null, Value::Integer));
    values.push(Value::Integer(fetch_limit as i64));
    let sql = format!(
        "SELECT provider, message_id, session_id, store_id, role, ordinal,
                timestamp, content, content_hash, storage_kind, payload_ref,
                snippet_text, legacy_source, legacy_truncated, metadata_json
         FROM lcm_raw_messages
         WHERE (? = 'all' OR provider = ?)
           AND session_id = ?
           AND store_id > ?
           {role_clause}
           AND (? IS NULL OR timestamp >= ?)
           AND (? IS NULL OR timestamp <= ?)
         ORDER BY store_id
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;

    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        let raw = raw::raw_message_from_row(&row)?;
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
        messages,
        next_cursor,
    })
}

/// Lists sessions in the raw LCM store ordered by most recent ingested activity.
///
/// `timestamp` is provider-supplied and may be absent or use a clock domain that
/// cannot be compared with `store_id`, so recency ordering uses the raw store's
/// insertion order.
pub async fn recent_sessions(
    conn: &(impl QueryExecutor + ?Sized),
    provider: Option<&str>,
    limit: usize,
) -> Result<Vec<LcmRecentSession>, LcmError> {
    let limit = clamp_limit(limit);
    let mut values = Vec::new();
    let provider_clause = match provider {
        Some(provider) => {
            values.push(Value::Text(provider.to_string()));
            "WHERE provider = ?"
        }
        None => "",
    };
    values.push(Value::Integer(limit as i64));
    let sql = format!(
        "SELECT provider, session_id, COUNT(*), MIN(timestamp), MAX(timestamp), MAX(store_id)
         FROM lcm_raw_messages
         {provider_clause}
         GROUP BY provider, session_id
         ORDER BY MAX(store_id) DESC
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(LcmRecentSession {
            provider: row.get(0)?,
            session_id: row.get(1)?,
            message_count: row.get(2)?,
            first_timestamp: row.get(3)?,
            last_timestamp: row.get(4)?,
            last_store_id: row.get(5)?,
        });
    }
    Ok(sessions)
}

/// Lists providers that contain raw messages for an explicit session id,
/// ordered by most recent ingested activity.
pub async fn session_providers(
    conn: &(impl QueryExecutor + ?Sized),
    session_id: &str,
) -> Result<Vec<String>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider
             FROM lcm_raw_messages
             WHERE session_id = ?1
             GROUP BY provider
             ORDER BY MAX(store_id) DESC",
            params![session_id],
        )
        .await?;
    let mut providers = Vec::new();
    while let Some(row) = rows.next().await? {
        providers.push(row.get(0)?);
    }
    Ok(providers)
}

/// Loads a bounded turn-ordered replay slice for one session: head turns,
/// tail turns (deduplicated against the head), and top summary-DAG nodes.
pub async fn session_replay_slice(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmSessionReplayRequest,
) -> Result<LcmSessionReplaySlice, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE provider = ?1 AND session_id = ?2",
            params![request.provider.as_str(), request.session_id.as_str()],
        )
        .await?;
    let total_messages: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };

    let head = replay_slice_messages(conn, request, ReplayDirection::Head, None).await?;
    let last_head_store_id = head.last().map(|message| message.store_id);
    let mut tail =
        replay_slice_messages(conn, request, ReplayDirection::Tail, last_head_store_id).await?;
    tail.reverse();
    let included = (head.len() + tail.len()) as i64;
    let summary_nodes = replay_slice_summary_nodes(conn, request).await?;

    Ok(LcmSessionReplaySlice {
        provider: request.provider.clone(),
        session_id: request.session_id.clone(),
        total_messages,
        omitted_messages: (total_messages - included).max(0),
        head,
        tail,
        summary_nodes,
    })
}

#[derive(Clone, Copy)]
enum ReplayDirection {
    Head,
    Tail,
}

async fn replay_slice_messages(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmSessionReplayRequest,
    direction: ReplayDirection,
    after_store_id: Option<i64>,
) -> Result<Vec<LcmReplayMessage>, LcmError> {
    let limit = match direction {
        ReplayDirection::Head => request.head_limit,
        ReplayDirection::Tail => request.tail_limit,
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let order = match direction {
        ReplayDirection::Head => "ASC",
        ReplayDirection::Tail => "DESC",
    };
    let values = vec![
        Value::Text(request.provider.clone()),
        Value::Text(request.session_id.clone()),
        Value::Integer(after_store_id.unwrap_or(0)),
        Value::Integer(clamp_limit(limit) as i64),
    ];
    let sql = format!(
        "SELECT message_id, store_id, role, ordinal, timestamp, snippet_text
         FROM lcm_raw_messages
         WHERE provider = ? AND session_id = ? AND store_id > ?
         ORDER BY store_id {order}
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;
    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        let snippet_text: String = row.get(5)?;
        let (snippet, truncated) = bounded_replay_snippet(&snippet_text, request.max_snippet_chars);
        messages.push(LcmReplayMessage {
            message_id: row.get(0)?,
            store_id: row.get(1)?,
            role: row.get(2)?,
            ordinal: row.get(3)?,
            timestamp: row.get(4)?,
            snippet,
            truncated,
        });
    }
    Ok(messages)
}

async fn replay_slice_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmSessionReplayRequest,
) -> Result<Vec<LcmReplaySummaryNode>, LcmError> {
    if request.summary_limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT node_id, depth, created_at, summary_text
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY depth DESC, created_at DESC, node_id
             LIMIT ?3",
            params![
                request.provider.as_str(),
                request.session_id.as_str(),
                clamp_limit(request.summary_limit) as i64,
            ],
        )
        .await?;
    let mut nodes = Vec::new();
    while let Some(row) = rows.next().await? {
        let summary_text: String = row.get(3)?;
        let (snippet, truncated) = bounded_replay_snippet(&summary_text, request.max_summary_chars);
        nodes.push(LcmReplaySummaryNode {
            node_id: row.get(0)?,
            depth: row.get(1)?,
            created_at: row.get(2)?,
            snippet,
            truncated,
        });
    }
    Ok(nodes)
}

fn bounded_replay_snippet(text: &str, max_chars: usize) -> (String, bool) {
    let text = text.trim();
    if text.chars().nth(max_chars).is_none() {
        (text.to_string(), false)
    } else {
        (text.chars().take(max_chars).collect(), true)
    }
}

fn load_message_from_raw(
    raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> LcmLoadSessionMessage {
    let LcmRawMessage {
        provider,
        message_id,
        session_id,
        store_id,
        role,
        ordinal,
        timestamp,
        content,
        content_hash,
        storage_kind,
        payload_ref,
        legacy_source,
        legacy_truncated,
        metadata_json,
    } = raw;
    let (content, content_range) = slice_content_owned(content, slice);
    LcmLoadSessionMessage {
        provider,
        message_id,
        session_id,
        store_id,
        role,
        ordinal,
        timestamp,
        content,
        content_range,
        content_hash,
        storage_kind,
        payload_ref,
        legacy_source,
        legacy_truncated,
        metadata_json,
    }
}
