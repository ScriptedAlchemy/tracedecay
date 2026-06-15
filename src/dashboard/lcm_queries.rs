use serde_json::Value;

use super::util::{qmarks, query_i64, query_rows};

pub(crate) const MESSAGE_TOKEN_ESTIMATE_EXPR: &str =
    "(LENGTH(COALESCE(content, snippet_text, '')) + 3) / 4";

pub(crate) const NODE_COLUMNS: &str = "n.node_id,
       n.session_id,
       n.depth,
       COALESCE(
           CASE
               WHEN n.metadata_json IS NOT NULL AND json_valid(n.metadata_json)
               THEN json_extract(n.metadata_json, '$.category')
           END,
           'general'
       ) AS category,
       CASE WHEN EXISTS (
           SELECT 1 FROM lcm_summary_sources s
           WHERE s.node_id = n.node_id AND s.source_kind = 'summary_node'
       ) THEN 'nodes' ELSE 'messages' END AS source_type,
       n.summary_token_count AS token_count,
       n.source_token_count,
       n.source_time_end AS latest_at,
       n.created_at,
       COALESCE(n.expand_hint, '') AS expand_hint,
       n.summary_text AS summary";

pub(crate) fn message_columns() -> String {
    format!(
        "m.store_id,
       m.session_id,
       m.role,
       CASE WHEN m.provider IS NULL OR TRIM(m.provider) = '' THEN 'unknown' ELSE m.provider END AS source,
       m.timestamp,
       ({MESSAGE_TOKEN_ESTIMATE_EXPR}) AS token_estimate,
       COALESCE(m.content, m.snippet_text) AS content,
       m.message_id,
       m.ordinal,
       m.storage_kind,
       m.metadata_json,
       (SELECT sm.tool_names FROM session_messages sm
        WHERE sm.provider = m.provider AND sm.message_id = m.message_id) AS tool_name,
       0 AS pinned,
       (SELECT json_group_array(s.node_id) FROM lcm_summary_sources s
        WHERE s.source_kind = 'raw_message'
          AND s.source_id = CAST(m.store_id AS TEXT)) AS summary_node_ids"
    )
}

pub(crate) async fn invalid_summary_metadata_node(
    conn: &libsql::Connection,
) -> Result<Option<String>, String> {
    let rows = query_rows(
        conn,
        "SELECT node_id
         FROM lcm_summary_nodes
         WHERE metadata_json IS NOT NULL
           AND NOT json_valid(metadata_json)
         LIMIT 1",
        (),
    )
    .await?;
    Ok(rows.into_iter().next().and_then(|row| {
        row.get("node_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    }))
}

pub(crate) async fn overview_role_counts(conn: &libsql::Connection) -> Result<Vec<Value>, String> {
    query_rows(
        conn,
        "SELECT role, COUNT(*) AS count
         FROM lcm_raw_messages
         GROUP BY role
         ORDER BY count DESC, role ASC",
        (),
    )
    .await
}

pub(crate) async fn overview_source_counts(
    conn: &libsql::Connection,
) -> Result<Vec<Value>, String> {
    query_rows(
        conn,
        "SELECT CASE WHEN provider IS NULL OR TRIM(provider) = '' THEN 'unknown' ELSE provider END AS source,
                COUNT(*) AS count
         FROM lcm_raw_messages
         GROUP BY source
         ORDER BY count DESC, source ASC",
        (),
    )
    .await
}

pub(crate) async fn overview_depth_counts(conn: &libsql::Connection) -> Result<Vec<Value>, String> {
    query_rows(
        conn,
        "SELECT depth, COUNT(*) AS count
         FROM lcm_summary_nodes
         GROUP BY depth
         ORDER BY depth ASC",
        (),
    )
    .await
}

pub(crate) async fn latest_sessions(
    conn: &libsql::Connection,
    limit: i64,
) -> Result<Vec<Value>, String> {
    query_rows(
        conn,
        "SELECT session_id,
                COUNT(*) AS message_count,
                MAX(store_id) AS last_store_id,
                MAX(timestamp) AS last_timestamp
         FROM lcm_raw_messages
         GROUP BY session_id
         ORDER BY last_timestamp DESC
         LIMIT ?1",
        libsql::params![limit],
    )
    .await
}

pub(crate) async fn latest_summary_nodes(
    conn: &libsql::Connection,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLUMNS}
         FROM lcm_summary_nodes n
         ORDER BY COALESCE(n.source_time_end, n.created_at) DESC, n.rowid DESC
         LIMIT ?1"
    );
    query_rows(conn, &sql, libsql::params![limit]).await
}

pub(crate) async fn overview_message_matches(
    conn: &libsql::Connection,
    like: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let message_columns = message_columns();
    let sql = format!(
        "SELECT {message_columns}
         FROM lcm_raw_messages m
         WHERE (m.index_text LIKE ?1 ESCAPE '\\'
                OR m.snippet_text LIKE ?1 ESCAPE '\\'
                OR COALESCE(m.content, '') LIKE ?1 ESCAPE '\\')
         ORDER BY m.timestamp DESC, m.store_id DESC
         LIMIT ?2"
    );
    query_rows(conn, &sql, libsql::params![like.to_string(), limit]).await
}

pub(crate) async fn overview_summary_node_matches(
    conn: &libsql::Connection,
    like: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                COALESCE(n.source_time_end, n.created_at) AS recency
         FROM lcm_summary_nodes n
         WHERE n.summary_text LIKE ?1 ESCAPE '\\'
            OR COALESCE(n.expand_hint, '') LIKE ?1 ESCAPE '\\'
         ORDER BY recency DESC, n.rowid DESC
         LIMIT ?2"
    );
    query_rows(conn, &sql, libsql::params![like.to_string(), limit]).await
}

pub(crate) async fn search_message_fts(
    conn: &libsql::Connection,
    expr: &str,
    facet_clauses: &[String],
    facet_params: &[libsql::Value],
    limit: i64,
    offset: i64,
) -> Result<(Vec<Value>, i64), String> {
    let mut where_clauses = vec!["lcm_raw_messages_fts MATCH ?".to_string()];
    where_clauses.extend(facet_clauses.iter().cloned());
    let mut fts_params = vec![libsql::Value::Text(expr.to_string())];
    fts_params.extend(facet_params.iter().cloned());
    let count_sql = format!(
        "SELECT COUNT(*)
         FROM lcm_raw_messages_fts
         JOIN lcm_raw_messages m ON m.store_id = lcm_raw_messages_fts.rowid
         WHERE {}",
        where_clauses.join(" AND ")
    );
    let count_params = fts_params.clone();
    fts_params.push(libsql::Value::Integer(limit));
    fts_params.push(libsql::Value::Integer(offset));
    let message_columns = message_columns();
    let sql = format!(
        "SELECT {message_columns},
                snippet(lcm_raw_messages_fts, 0, '[', ']', '…', 12) AS snippet
         FROM lcm_raw_messages_fts
         JOIN lcm_raw_messages m ON m.store_id = lcm_raw_messages_fts.rowid
         WHERE {}
         ORDER BY rank
         LIMIT ? OFFSET ?",
        where_clauses.join(" AND ")
    );
    let rows = query_rows(conn, &sql, fts_params).await?;
    let total = query_i64(conn, &count_sql, count_params).await;
    Ok((rows, total))
}

pub(crate) async fn search_message_like(
    conn: &libsql::Connection,
    like: &str,
    facet_clauses: &[String],
    facet_params: &[libsql::Value],
    limit: i64,
    offset: i64,
) -> Result<(Vec<Value>, i64), String> {
    let mut where_clauses = vec!["(m.index_text LIKE ? ESCAPE '\\'
              OR m.snippet_text LIKE ? ESCAPE '\\'
              OR COALESCE(m.content, '') LIKE ? ESCAPE '\\')"
        .to_string()];
    where_clauses.extend(facet_clauses.iter().cloned());
    let mut like_params = vec![
        libsql::Value::Text(like.to_string()),
        libsql::Value::Text(like.to_string()),
        libsql::Value::Text(like.to_string()),
    ];
    like_params.extend(facet_params.iter().cloned());
    let count_sql = format!(
        "SELECT COUNT(*) FROM lcm_raw_messages m WHERE {}",
        where_clauses.join(" AND ")
    );
    let total = query_i64(conn, &count_sql, like_params.clone()).await;
    like_params.push(libsql::Value::Integer(limit));
    like_params.push(libsql::Value::Integer(offset));
    let message_columns = message_columns();
    let sql = format!(
        "SELECT {message_columns},
                substr(COALESCE(m.content, m.snippet_text, ''), 1, 280) AS snippet
         FROM lcm_raw_messages m
         WHERE {}
         ORDER BY m.timestamp DESC, m.store_id DESC
         LIMIT ? OFFSET ?",
        where_clauses.join(" AND ")
    );
    let rows = query_rows(conn, &sql, like_params).await?;
    Ok((rows, total))
}

pub(crate) async fn search_node_fts(
    conn: &libsql::Connection,
    expr: &str,
    node_clauses: &[String],
    node_params: &[libsql::Value],
    limit: i64,
    offset: i64,
) -> Result<(Vec<Value>, i64), String> {
    let node_match_expr = format!("{{summary_text expand_hint}} : ({expr})");
    let mut where_clauses = vec!["lcm_summary_nodes_fts MATCH ?".to_string()];
    where_clauses.extend(node_clauses.iter().cloned());
    let mut fts_params = vec![libsql::Value::Text(node_match_expr)];
    fts_params.extend(node_params.iter().cloned());
    let count_sql = format!(
        "SELECT COUNT(*)
         FROM lcm_summary_nodes_fts
         JOIN lcm_summary_nodes n ON n.rowid = lcm_summary_nodes_fts.rowid
         WHERE {}",
        where_clauses.join(" AND ")
    );
    let count_params = fts_params.clone();
    fts_params.push(libsql::Value::Integer(limit));
    fts_params.push(libsql::Value::Integer(offset));
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                COALESCE(n.source_time_end, n.created_at) AS recency,
                snippet(lcm_summary_nodes_fts, 0, '[', ']', '…', 14) AS snippet
         FROM lcm_summary_nodes_fts
         JOIN lcm_summary_nodes n ON n.rowid = lcm_summary_nodes_fts.rowid
         WHERE {}
         ORDER BY rank
         LIMIT ? OFFSET ?",
        where_clauses.join(" AND ")
    );
    let rows = query_rows(conn, &sql, fts_params).await?;
    let total = query_i64(conn, &count_sql, count_params).await;
    Ok((rows, total))
}

pub(crate) async fn search_node_like(
    conn: &libsql::Connection,
    like: &str,
    node_clauses: &[String],
    node_params: &[libsql::Value],
    limit: i64,
    offset: i64,
) -> Result<(Vec<Value>, i64), String> {
    let mut where_clauses = vec![
        "(n.summary_text LIKE ? ESCAPE '\\' OR COALESCE(n.expand_hint, '') LIKE ? ESCAPE '\\')"
            .to_string(),
    ];
    where_clauses.extend(node_clauses.iter().cloned());
    let mut like_params = vec![
        libsql::Value::Text(like.to_string()),
        libsql::Value::Text(like.to_string()),
    ];
    like_params.extend(node_params.iter().cloned());
    let count_sql = format!(
        "SELECT COUNT(*) FROM lcm_summary_nodes n WHERE {}",
        where_clauses.join(" AND ")
    );
    let total = query_i64(conn, &count_sql, like_params.clone()).await;
    like_params.push(libsql::Value::Integer(limit));
    like_params.push(libsql::Value::Integer(offset));
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                COALESCE(n.source_time_end, n.created_at) AS recency,
                substr(n.summary_text, 1, 280) AS snippet
         FROM lcm_summary_nodes n
         WHERE {}
         ORDER BY recency DESC, n.rowid DESC
         LIMIT ? OFFSET ?",
        where_clauses.join(" AND ")
    );
    let rows = query_rows(conn, &sql, like_params).await?;
    Ok((rows, total))
}

pub(crate) async fn session_messages(
    conn: &libsql::Connection,
    session_id: &str,
    order: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Value>, String> {
    let message_columns = message_columns();
    let sql = format!(
        "SELECT {message_columns}
         FROM lcm_raw_messages m
         WHERE m.session_id = ?1
         ORDER BY m.ordinal {order}, m.timestamp {order}, m.store_id {order}
         LIMIT ?2 OFFSET ?3"
    );
    query_rows(
        conn,
        &sql,
        libsql::params![session_id.to_string(), limit, offset],
    )
    .await
}

pub(crate) async fn session_summary_nodes(
    conn: &libsql::Connection,
    session_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                COALESCE(n.source_time_end, n.created_at) AS recency
         FROM lcm_summary_nodes n
         WHERE n.session_id = ?1
         ORDER BY n.depth ASC, recency ASC, n.rowid ASC
         LIMIT ?2 OFFSET ?3"
    );
    query_rows(
        conn,
        &sql,
        libsql::params![session_id.to_string(), limit, offset],
    )
    .await
}

pub(crate) async fn node_row(
    conn: &libsql::Connection,
    node_id: &str,
) -> Result<Option<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                n.source_time_start AS earliest_at,
                CASE
                    WHEN n.metadata_json IS NOT NULL AND json_valid(n.metadata_json)
                    THEN json_extract(n.metadata_json, '$.tags')
                END AS tags,
                CASE
                    WHEN n.metadata_json IS NOT NULL AND json_valid(n.metadata_json)
                    THEN json_extract(n.metadata_json, '$.entities')
                END AS entities
         FROM lcm_summary_nodes n
         WHERE n.node_id = ?1"
    );
    let rows = query_rows(conn, &sql, libsql::params![node_id.to_string()]).await?;
    Ok(rows.into_iter().next())
}

pub(crate) async fn node_source_rows(
    conn: &libsql::Connection,
    node_id: &str,
) -> Result<Vec<Value>, String> {
    query_rows(
        conn,
        "SELECT source_kind, source_id
         FROM lcm_summary_sources
         WHERE node_id = ?1
         ORDER BY ordinal ASC",
        libsql::params![node_id.to_string()],
    )
    .await
}

pub(crate) async fn child_summary_nodes(
    conn: &libsql::Connection,
    child_node_ids: &[String],
) -> Result<Vec<Value>, String> {
    if child_node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = qmarks(child_node_ids.len());
    let params: Vec<libsql::Value> = child_node_ids
        .iter()
        .cloned()
        .map(libsql::Value::Text)
        .collect();
    let sql = format!(
        "SELECT {NODE_COLUMNS},
                COALESCE(n.source_time_end, n.created_at) AS recency
         FROM lcm_summary_nodes n
         WHERE n.node_id IN ({placeholders})
         ORDER BY recency ASC, n.rowid ASC"
    );
    query_rows(conn, &sql, params).await
}

pub(crate) async fn source_messages(
    conn: &libsql::Connection,
    message_ids: &[i64],
) -> Result<Vec<Value>, String> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = qmarks(message_ids.len());
    let params: Vec<libsql::Value> = message_ids
        .iter()
        .copied()
        .map(libsql::Value::Integer)
        .collect();
    let message_columns = message_columns();
    let sql = format!(
        "SELECT {message_columns}
         FROM lcm_raw_messages m
         WHERE m.store_id IN ({placeholders})"
    );
    query_rows(conn, &sql, params).await
}

pub(crate) async fn timeline_message_buckets(
    conn: &libsql::Connection,
    fmt: &str,
    session_id: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let msg_where = if session_id.is_some() {
        "WHERE timestamp IS NOT NULL AND session_id = ?2"
    } else {
        "WHERE timestamp IS NOT NULL"
    };
    let sql = format!(
        "SELECT strftime('{fmt}', timestamp, 'unixepoch') AS bucket,
                COUNT(*) AS count,
                COALESCE(SUM({MESSAGE_TOKEN_ESTIMATE_EXPR}), 0) AS token_estimate
         FROM lcm_raw_messages
         {msg_where}
         GROUP BY bucket
         ORDER BY bucket ASC
         LIMIT ?1"
    );
    if let Some(session_id) = session_id {
        query_rows(conn, &sql, libsql::params![limit, session_id.to_string()]).await
    } else {
        query_rows(conn, &sql, libsql::params![limit]).await
    }
}

pub(crate) async fn timeline_undated_messages(
    conn: &libsql::Connection,
    session_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let undated_where = if session_id.is_some() {
        "WHERE timestamp IS NULL AND session_id = ?1"
    } else {
        "WHERE timestamp IS NULL"
    };
    let sql = format!(
        "SELECT COUNT(*) AS count,
                COALESCE(SUM({MESSAGE_TOKEN_ESTIMATE_EXPR}), 0) AS token_estimate
         FROM lcm_raw_messages
         {undated_where}"
    );
    if let Some(session_id) = session_id {
        query_rows(conn, &sql, libsql::params![session_id.to_string()]).await
    } else {
        query_rows(conn, &sql, ()).await
    }
}

pub(crate) async fn timeline_summary_buckets(
    conn: &libsql::Connection,
    fmt: &str,
    session_id: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let node_where = if session_id.is_some() {
        "WHERE session_id = ?2"
    } else {
        ""
    };
    let sql = format!(
        "SELECT strftime('{fmt}', COALESCE(source_time_end, created_at), 'unixepoch') AS bucket,
                COUNT(*) AS count
         FROM lcm_summary_nodes
         {node_where}
         GROUP BY bucket
         ORDER BY bucket ASC
         LIMIT ?1"
    );
    if let Some(session_id) = session_id {
        query_rows(conn, &sql, libsql::params![limit, session_id.to_string()]).await
    } else {
        query_rows(conn, &sql, libsql::params![limit]).await
    }
}

pub(crate) async fn compression_groups(
    conn: &libsql::Connection,
    by_node: bool,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let groups_sql = if by_node {
        "SELECT node_id AS key,
                session_id,
                depth,
                source_token_count,
                summary_token_count AS token_count
         FROM lcm_summary_nodes
         ORDER BY source_token_count DESC, node_id ASC
         LIMIT ?1"
    } else {
        "SELECT session_id AS key,
                COUNT(*) AS node_count,
                COALESCE(SUM(source_token_count), 0) AS source_token_count,
                COALESCE(SUM(summary_token_count), 0) AS token_count
         FROM lcm_summary_nodes
         GROUP BY session_id
         ORDER BY source_token_count DESC, session_id ASC
         LIMIT ?1"
    };
    query_rows(conn, groups_sql, libsql::params![limit]).await
}
