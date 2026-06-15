use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{json, Map, Value};

use super::lcm_queries;
use super::util::{build_fts_match, http_detail, json_error, json_object, like_pattern};
use super::DashboardState;

pub(crate) type LcmErrorResponse = (StatusCode, Json<Value>);
pub(crate) type LcmServiceResult<T> = Result<T, LcmErrorResponse>;

pub(crate) struct SearchPayloadArgs<'a> {
    pub(crate) query: &'a str,
    pub(crate) limit: i64,
    pub(crate) offset: i64,
    pub(crate) role: &'a str,
    pub(crate) source: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) since: Option<f64>,
    pub(crate) until: Option<f64>,
}

/// LCM store paths whose summary metadata has already validated clean this
/// process. Validation is a full `lcm_summary_nodes` scan; the invariant is
/// writer-enforced, so a store that passed once is not re-scanned on every
/// request. Failures are NOT cached — a store with a malformed row keeps
/// returning 422 until it is repaired.
static VALIDATED_METADATA_STORES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn empty_overview() -> Value {
    json!({
        "messages_total": 0,
        "sessions_total": 0,
        "summary_nodes_total": 0,
        "summary_node_sessions_total": 0,
        "max_summary_depth": 0,
        "role_counts": [],
        "source_counts": [],
        "depth_counts": [],
        "compression": {
            "source_token_count": 0,
            "token_count": 0,
            "ratio": 0.0,
            "node_count": 0,
        },
    })
}

fn parse_summary_node_ids(rows: &mut [Value]) {
    for row in rows.iter_mut() {
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        let Some(raw) = obj.get("summary_node_ids").and_then(Value::as_str) else {
            continue;
        };
        let parsed = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!([]));
        obj.insert("summary_node_ids".into(), parsed);
    }
}

fn ratio(src: i64, out: i64) -> f64 {
    if out > 0 {
        (src as f64 / out as f64 * 100.0).round() / 100.0
    } else {
        0.0
    }
}

fn query_error(context: &str, err: &str) -> LcmErrorResponse {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{context} query failed: {err}"),
    )
}

fn map_query_error<T>(context: &str, result: Result<T, String>) -> LcmServiceResult<T> {
    result.map_err(|err| query_error(context, &err))
}

pub(crate) fn parse_epoch(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

pub(crate) async fn ensure_valid_summary_metadata(
    state: &DashboardState,
    conn: &libsql::Connection,
    context: &str,
) -> LcmServiceResult<()> {
    let validated = VALIDATED_METADATA_STORES.get_or_init(|| Mutex::new(HashSet::new()));
    if validated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&state.lcm_db_path)
    {
        return Ok(());
    }
    if let Some(node_id) = map_query_error(
        context,
        lcm_queries::invalid_summary_metadata_node(conn).await,
    )? {
        return Err(json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{context}: malformed metadata_json for summary node {node_id}"),
        ));
    }
    validated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(state.lcm_db_path.clone());
    Ok(())
}

pub(crate) async fn overview_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> LcmServiceResult<Map<String, Value>> {
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "overview": empty_overview(),
        "latest_sessions": [],
        "latest_summary_nodes": [],
        "matches": { "messages": [], "summary_nodes": [] },
        "query": query,
        "limit": limit,
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };
    ensure_valid_summary_metadata(state, conn, "overview").await?;

    let messages_total =
        super::util::query_i64(conn, "SELECT COUNT(*) FROM lcm_raw_messages", ()).await;
    let sessions_total = super::util::query_i64(
        conn,
        "SELECT COUNT(DISTINCT session_id) FROM lcm_raw_messages",
        (),
    )
    .await;
    let role_counts = map_query_error(
        "overview role counts",
        lcm_queries::overview_role_counts(conn).await,
    )?;
    let source_counts = map_query_error(
        "overview source counts",
        lcm_queries::overview_source_counts(conn).await,
    )?;
    let summary_nodes_total =
        super::util::query_i64(conn, "SELECT COUNT(*) FROM lcm_summary_nodes", ()).await;
    let summary_node_sessions_total = super::util::query_i64(
        conn,
        "SELECT COUNT(DISTINCT session_id) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let max_summary_depth = super::util::query_i64(
        conn,
        "SELECT COALESCE(MAX(depth), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let depth_counts = map_query_error(
        "overview depth counts",
        lcm_queries::overview_depth_counts(conn).await,
    )?;
    let source_token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;

    payload.insert(
        "overview".into(),
        json!({
            "messages_total": messages_total,
            "sessions_total": sessions_total,
            "summary_nodes_total": summary_nodes_total,
            "summary_node_sessions_total": summary_node_sessions_total,
            "max_summary_depth": max_summary_depth,
            "role_counts": role_counts,
            "source_counts": source_counts,
            "depth_counts": depth_counts,
            "compression": {
                "source_token_count": source_token_count,
                "token_count": token_count,
                "ratio": ratio(source_token_count, token_count),
                "node_count": summary_nodes_total,
            },
        }),
    );

    let latest_sessions = map_query_error(
        "latest_sessions",
        lcm_queries::latest_sessions(conn, limit).await,
    )?;
    let latest_summary_nodes = map_query_error(
        "latest_summary_nodes",
        lcm_queries::latest_summary_nodes(conn, limit).await,
    )?;
    payload.insert("latest_sessions".into(), json!(latest_sessions));
    payload.insert("latest_summary_nodes".into(), json!(latest_summary_nodes));

    let trimmed_query = query.trim();
    if !trimmed_query.is_empty() {
        let like = like_pattern(trimmed_query);
        let mut message_matches = map_query_error(
            "overview message matches",
            lcm_queries::overview_message_matches(conn, &like, limit).await,
        )?;
        parse_summary_node_ids(&mut message_matches);
        let node_matches = map_query_error(
            "overview summary node matches",
            lcm_queries::overview_summary_node_matches(conn, &like, limit).await,
        )?;
        payload.insert(
            "matches".into(),
            json!({ "messages": message_matches, "summary_nodes": node_matches }),
        );
    }

    Ok(payload)
}

pub(crate) async fn search_payload(
    state: &DashboardState,
    args: SearchPayloadArgs<'_>,
) -> LcmServiceResult<Map<String, Value>> {
    let SearchPayloadArgs {
        query,
        limit,
        offset,
        role,
        source,
        session_id,
        since,
        until,
    } = args;
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "query": query,
        "limit": limit,
        "offset": offset,
        "engine": "none",
        "engine_detail": { "messages": "none", "summary_nodes": "none" },
        "total": { "messages": 0, "summary_nodes": 0 },
        "filters": {
            "role": if role.is_empty() { Value::Null } else { json!(role) },
            "source": if source.is_empty() { Value::Null } else { json!(source) },
            "session_id": if session_id.is_empty() { Value::Null } else { json!(session_id) },
            "since": since,
            "until": until,
        },
        "matches": { "messages": [], "summary_nodes": [] },
    }));
    let trimmed_query = query.trim().to_string();
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };
    if trimmed_query.is_empty() {
        return Ok(payload);
    }
    ensure_valid_summary_metadata(state, conn, "search").await?;

    let mut facet_clauses: Vec<String> = Vec::new();
    let mut facet_params: Vec<libsql::Value> = Vec::new();
    if !role.is_empty() {
        facet_clauses.push("m.role = ?".into());
        facet_params.push(libsql::Value::Text(role.to_string()));
    }
    if !source.is_empty() {
        if source == "unknown" {
            facet_clauses.push("(m.provider IS NULL OR TRIM(m.provider) = '')".into());
        } else {
            facet_clauses.push("m.provider = ?".into());
            facet_params.push(libsql::Value::Text(source.to_string()));
        }
    }
    if !session_id.is_empty() {
        facet_clauses.push("m.session_id = ?".into());
        facet_params.push(libsql::Value::Text(session_id.to_string()));
    }
    if let Some(since) = since {
        facet_clauses.push("m.timestamp >= ?".into());
        facet_params.push(libsql::Value::Real(since));
    }
    if let Some(until) = until {
        facet_clauses.push("m.timestamp <= ?".into());
        facet_params.push(libsql::Value::Real(until));
    }

    let match_expr = build_fts_match(&trimmed_query);
    let like = like_pattern(&trimmed_query);
    let mut message_engine = "like";
    let (mut message_matches, message_total) = if let Some(expr) = &match_expr {
        match lcm_queries::search_message_fts(
            conn,
            expr,
            &facet_clauses,
            &facet_params,
            limit,
            offset,
        )
        .await
        {
            Ok((rows, total)) => {
                message_engine = "fts";
                (rows, total)
            }
            Err(_) => map_query_error(
                "search message LIKE fallback",
                lcm_queries::search_message_like(
                    conn,
                    &like,
                    &facet_clauses,
                    &facet_params,
                    limit,
                    offset,
                )
                .await,
            )?,
        }
    } else {
        map_query_error(
            "search message LIKE fallback",
            lcm_queries::search_message_like(
                conn,
                &like,
                &facet_clauses,
                &facet_params,
                limit,
                offset,
            )
            .await,
        )?
    };
    parse_summary_node_ids(&mut message_matches);

    let mut node_clauses: Vec<String> = Vec::new();
    let mut node_params: Vec<libsql::Value> = Vec::new();
    if !session_id.is_empty() {
        node_clauses.push("n.session_id = ?".into());
        node_params.push(libsql::Value::Text(session_id.to_string()));
    }
    if let Some(since) = since {
        node_clauses.push("COALESCE(n.source_time_end, n.created_at) >= ?".into());
        node_params.push(libsql::Value::Real(since));
    }
    if let Some(until) = until {
        node_clauses.push("COALESCE(n.source_time_end, n.created_at) <= ?".into());
        node_params.push(libsql::Value::Real(until));
    }

    let mut node_engine = "like";
    let (node_matches, node_total) = if let Some(expr) = &match_expr {
        match lcm_queries::search_node_fts(conn, expr, &node_clauses, &node_params, limit, offset)
            .await
        {
            Ok((rows, total)) => {
                node_engine = "fts";
                (rows, total)
            }
            Err(_) => map_query_error(
                "search summary node LIKE fallback",
                lcm_queries::search_node_like(
                    conn,
                    &like,
                    &node_clauses,
                    &node_params,
                    limit,
                    offset,
                )
                .await,
            )?,
        }
    } else {
        map_query_error(
            "search summary node LIKE fallback",
            lcm_queries::search_node_like(conn, &like, &node_clauses, &node_params, limit, offset)
                .await,
        )?
    };

    let engine = if message_engine == "fts" && node_engine == "fts" {
        "fts"
    } else {
        "like"
    };
    payload.insert("engine".into(), json!(engine));
    payload.insert(
        "engine_detail".into(),
        json!({ "messages": message_engine, "summary_nodes": node_engine }),
    );
    payload.insert(
        "total".into(),
        json!({ "messages": message_total, "summary_nodes": node_total }),
    );
    payload.insert(
        "matches".into(),
        json!({ "messages": message_matches, "summary_nodes": node_matches }),
    );
    Ok(payload)
}

pub(crate) async fn session_payload(
    state: &DashboardState,
    session_id: &str,
    limit: i64,
    offset: i64,
    descending: bool,
) -> LcmServiceResult<Map<String, Value>> {
    let order = if descending { "DESC" } else { "ASC" };
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "session_id": session_id,
        "limit": limit,
        "offset": offset,
        "order": order.to_ascii_lowercase(),
        "counts": {
            "message_count": 0,
            "summary_node_count": 0,
            "token_estimate_total": 0,
            "summary_token_count": 0,
            "source_token_count": 0,
        },
        "messages": [],
        "summary_nodes": [],
        "has_more": false,
        "has_more_messages": false,
        "has_more_summary_nodes": false,
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };
    ensure_valid_summary_metadata(state, conn, "session").await?;

    let message_count = super::util::query_i64(
        conn,
        "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
        libsql::params![session_id.to_string()],
    )
    .await;
    let summary_node_count = super::util::query_i64(
        conn,
        "SELECT COUNT(*) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.to_string()],
    )
    .await;
    if message_count == 0 && summary_node_count == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("session not found: {session_id}"))),
        ));
    }

    let token_estimate_total = super::util::query_i64(
        conn,
        &format!(
            "SELECT COALESCE(SUM({}), 0)
             FROM lcm_raw_messages WHERE session_id = ?1",
            lcm_queries::MESSAGE_TOKEN_ESTIMATE_EXPR
        ),
        libsql::params![session_id.to_string()],
    )
    .await;
    let mut messages = map_query_error(
        "session messages",
        lcm_queries::session_messages(conn, session_id, order, limit + 1, offset).await,
    )?;
    let has_more_messages = messages.len() as i64 > limit;
    messages.truncate(limit as usize);
    parse_summary_node_ids(&mut messages);

    let summary_token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.to_string()],
    )
    .await;
    let source_token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.to_string()],
    )
    .await;
    let mut summary_nodes = map_query_error(
        "session summary nodes",
        lcm_queries::session_summary_nodes(conn, session_id, limit + 1, offset).await,
    )?;
    let has_more_summary_nodes = summary_nodes.len() as i64 > limit;
    summary_nodes.truncate(limit as usize);
    let has_more = has_more_messages || has_more_summary_nodes;

    payload.insert(
        "counts".into(),
        json!({
            "message_count": message_count,
            "summary_node_count": summary_node_count,
            "token_estimate_total": token_estimate_total,
            "summary_token_count": summary_token_count,
            "source_token_count": source_token_count,
        }),
    );
    payload.insert("messages".into(), json!(messages));
    payload.insert("summary_nodes".into(), json!(summary_nodes));
    payload.insert("has_more".into(), json!(has_more));
    payload.insert("has_more_messages".into(), json!(has_more_messages));
    payload.insert(
        "has_more_summary_nodes".into(),
        json!(has_more_summary_nodes),
    );
    Ok(payload)
}

pub(crate) async fn node_payload(
    state: &DashboardState,
    node_id: &str,
) -> LcmServiceResult<Map<String, Value>> {
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "node_id": node_id,
        "node": null,
        "sources": { "type": null, "ids": [], "messages": [], "nodes": [] },
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };
    ensure_valid_summary_metadata(state, conn, "node").await?;

    let Some(node_row) =
        map_query_error("node lookup", lcm_queries::node_row(conn, node_id).await)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("summary node not found: {node_id}"))),
        ));
    };
    payload.insert("node".into(), node_row);

    let source_rows = map_query_error(
        "node sources",
        lcm_queries::node_source_rows(conn, node_id).await,
    )?;
    let mut message_ids: Vec<i64> = Vec::new();
    let mut child_node_ids: Vec<String> = Vec::new();
    for row in &source_rows {
        let kind = row.get("source_kind").and_then(Value::as_str).unwrap_or("");
        let id = row.get("source_id").and_then(Value::as_str).unwrap_or("");
        match kind {
            "raw_message" => {
                if let Ok(store_id) = id.parse::<i64>() {
                    message_ids.push(store_id);
                }
            }
            "summary_node" => child_node_ids.push(id.to_string()),
            _ => {}
        }
    }

    let source_type = if child_node_ids.is_empty() {
        "messages"
    } else {
        "nodes"
    };
    let ids: Vec<Value> = if source_type == "nodes" {
        child_node_ids.iter().map(|id| json!(id)).collect()
    } else {
        message_ids.iter().map(|id| json!(id)).collect()
    };

    let mut sources_obj = Map::new();
    sources_obj.insert("type".into(), json!(source_type));
    sources_obj.insert("ids".into(), json!(ids));
    sources_obj.insert("messages".into(), json!([]));
    sources_obj.insert("nodes".into(), json!([]));

    if source_type == "nodes" && !child_node_ids.is_empty() {
        let rows = map_query_error(
            "node child summary nodes",
            lcm_queries::child_summary_nodes(conn, &child_node_ids).await,
        )?;
        sources_obj.insert("nodes".into(), json!(rows));
    } else if !message_ids.is_empty() {
        let mut rows = map_query_error(
            "node source messages",
            lcm_queries::source_messages(conn, &message_ids).await,
        )?;
        parse_summary_node_ids(&mut rows);
        let order: HashMap<i64, usize> = message_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();
        rows.sort_by_key(|row| {
            row.get("store_id")
                .and_then(Value::as_i64)
                .and_then(|id| order.get(&id).copied())
                .unwrap_or(usize::MAX)
        });
        sources_obj.insert("messages".into(), json!(rows));
    }
    payload.insert("sources".into(), Value::Object(sources_obj));
    Ok(payload)
}

pub(crate) async fn timeline_payload(
    state: &DashboardState,
    by_hour: bool,
    session_id: &str,
    limit: i64,
) -> LcmServiceResult<Map<String, Value>> {
    let fmt = if by_hour {
        "%Y-%m-%dT%H:00"
    } else {
        "%Y-%m-%d"
    };
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "bucket": if by_hour { "hour" } else { "day" },
        "session_id": if session_id.is_empty() { Value::Null } else { json!(session_id) },
        "buckets": [],
        "node_buckets": [],
        "undated": {"count": 0, "token_estimate": 0},
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };
    let session_id = if session_id.is_empty() {
        None
    } else {
        Some(session_id)
    };
    let buckets = map_query_error(
        "timeline message buckets",
        lcm_queries::timeline_message_buckets(conn, fmt, session_id, limit).await,
    )?;
    let undated = map_query_error(
        "timeline undated messages",
        lcm_queries::timeline_undated_messages(conn, session_id).await,
    )?;
    let node_buckets = map_query_error(
        "timeline summary buckets",
        lcm_queries::timeline_summary_buckets(conn, fmt, session_id, limit).await,
    )?;
    payload.insert("buckets".into(), json!(buckets));
    payload.insert(
        "undated".into(),
        undated
            .into_iter()
            .next_back()
            .unwrap_or_else(|| json!({"count": 0, "token_estimate": 0})),
    );
    payload.insert("node_buckets".into(), json!(node_buckets));
    Ok(payload)
}

pub(crate) async fn compression_payload(
    state: &DashboardState,
    by_node: bool,
    limit: i64,
) -> LcmServiceResult<Map<String, Value>> {
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "by": if by_node { "node" } else { "session" },
        "limit": limit,
        "overall": {
            "source_token_count": 0,
            "token_count": 0,
            "ratio": 0.0,
            "node_count": 0,
        },
        "groups": [],
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(payload);
    };

    let source_token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let token_count = super::util::query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let node_count =
        super::util::query_i64(conn, "SELECT COUNT(*) FROM lcm_summary_nodes", ()).await;
    payload.insert(
        "overall".into(),
        json!({
            "source_token_count": source_token_count,
            "token_count": token_count,
            "ratio": ratio(source_token_count, token_count),
            "node_count": node_count,
        }),
    );

    let mut groups = map_query_error(
        "compression groups",
        lcm_queries::compression_groups(conn, by_node, limit).await,
    )?;
    for group in &mut groups {
        let source_token_count = group
            .get("source_token_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let token_count = group
            .get("token_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(obj) = group.as_object_mut() {
            obj.insert(
                "ratio".into(),
                json!(ratio(source_token_count, token_count)),
            );
        }
    }
    payload.insert("groups".into(), json!(groups));
    Ok(payload)
}
