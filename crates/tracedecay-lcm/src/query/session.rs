use super::super::util;
use super::scope::LcmScopeSql;
use super::*;

// Future lifetime so suspension inside the query executor (writer lease /
// queue wait) is charged here rather than vanishing between poll times. The
// decode share of this span is the nested `sessions.lcm.raw.verify_row`.
#[hotpath::measure(label = "sessions.lcm.load_session", future = true)]
pub async fn load_session(
    conn: &(impl QueryExecutor + ?Sized),
    request: LcmLoadSessionRequest,
) -> Result<LcmLoadSessionPage, LcmError> {
    let limit = clamp_limit(request.limit);
    let fetch_limit = limit.saturating_add(1);
    let (sql, values) = load_session_query(&request, fetch_limit);
    let fetched = hotpath::future!(
        async {
            let mut rows = conn.query(&sql, values).await?;
            let mut fetched = Vec::new();
            while let Some(row) = rows.next().await? {
                fetched.push(row);
            }
            Ok::<_, LcmError>(fetched)
        },
        label = "sessions.lcm.hydrate.fetch"
    )
    .await?;
    let raws = hotpath::measure_block!("sessions.lcm.hydrate.redact", {
        fetched
            .iter()
            .map(raw::verified_raw_message_from_row)
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut messages = raws
        .into_iter()
        .map(|raw| load_message_from_raw(raw, request.content_slice))
        .collect::<Vec<_>>();

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

fn load_session_query(request: &LcmLoadSessionRequest, fetch_limit: usize) -> (String, Vec<Value>) {
    let scope = LcmScopeSql::new(
        "provider",
        "session_id",
        &request.provider,
        Some(&request.session_id),
    );
    let scope_clause = scope.where_clause();
    let mut values = scope.into_values();
    values.push(Value::Integer(request.after_store_id.unwrap_or(0)));
    let mut role_clause = String::new();
    let roles = normalized_strings(&request.roles);
    if !roles.is_empty() {
        let placeholders = util::sql_in_placeholders(roles.len());
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
         {scope}
           AND store_id > ?
           {role_clause}
           AND (? IS NULL OR timestamp >= ?)
           AND (? IS NULL OR timestamp <= ?)
         ORDER BY store_id
         LIMIT ?",
        scope = scope_clause
    );
    (sql, values)
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
#[hotpath::measure(label = "sessions.lcm.replay_slice", future = true)]
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
            "SELECT node_id, depth, created_at, summary_text, summary_hash
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
        let summary_hash: String = row.get(4)?;
        dag::verify_summary_content(&summary_text, &summary_hash)?;
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
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => (text[..end].to_string(), true),
        None => (text.to_string(), false),
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::{TestConnection, params_from_iter};

    use super::*;

    async fn test_lcm_connection() -> (TempDir, TestConnection) {
        let directory = TempDir::new().expect("session database tempdir");
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
            );",
        )
        .await
        .expect("create session table");
        schema::ensure_lcm_schema(&conn)
            .await
            .expect("create LCM schema");
        (directory, conn)
    }

    #[tokio::test]
    async fn load_session_provider_scope_uses_composite_session_order_index() {
        let (_directory, conn) = test_lcm_connection().await;
        let request = LcmLoadSessionRequest {
            provider: "cursor".to_owned(),
            session_id: "session-a".to_owned(),
            after_store_id: None,
            limit: 20,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        };
        let (sql, values) = load_session_query(&request, request.limit.saturating_add(1));
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {sql}"),
                params_from_iter(values),
            )
            .await
            .expect("explain load-session query");
        let mut plan = Vec::new();
        while let Some(row) = rows.next().await.expect("read plan row") {
            plan.push(row.get::<String>(3).expect("plan detail"));
        }

        assert!(
            plan.iter()
                .any(|line| line.contains("idx_lcm_raw_session_order")),
            "provider-scoped restore must seek the composite session-order index:\n{}",
            plan.join("\n")
        );
    }
}
