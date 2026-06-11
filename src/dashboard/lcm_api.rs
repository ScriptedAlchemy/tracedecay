//! LCM dashboard API, backed by tokensave's LCM session store.
//!
//! Port of the hermes-lcm `dashboard/plugin_api.py` onto the session-store
//! tables `lcm_raw_messages`, `lcm_summary_nodes`, and `lcm_summary_sources`.
//! The store served is selected by [`super::resolve_lcm_store`]: the
//! project-local `.tokensave/sessions.db` (where transcript ingest writes)
//! by default, or the global DB under a `TOKENSAVE_GLOBAL_DB` override /
//! fallback. Every payload reports the active store via the additive
//! `path` + `storage_scope` fields. Payload shapes otherwise mirror the
//! original routes so the ported UI bundle works unchanged.
//!
//! Schema mapping (hermes-lcm → tokensave):
//! - `messages`               → `lcm_raw_messages` (`source` ← `provider`,
//!   `token_estimate` ← ~chars/4, `pinned`/`tool_name` not tracked)
//! - `summary_nodes`          → `lcm_summary_nodes` (`summary` ←
//!   `summary_text`, `token_count` ← `summary_token_count`, `latest_at` ←
//!   `source_time_end`; node ids are strings, not ints)
//! - `summary_nodes.source_ids` JSON → `lcm_summary_sources` rows
//! - FTS mirrors → `lcm_raw_messages_fts` / `lcm_summary_nodes_fts`

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::util::{
    build_fts_match, coerce_limit, http_detail, json_error, json_object, like_pattern, qmarks,
    query_i64, query_rows, JsonPath, JsonQuery,
};
use super::DashboardState;

type LcmResponse = (StatusCode, Json<Value>);
/// Handlers return `Result` so query failures propagate with `?`; Axum
/// renders `Ok` and `Err` identically (both are status + JSON body).
type LcmResult = Result<LcmResponse, LcmResponse>;

/// Message SELECT list shared by every route that returns message rows.
///
/// `content` falls back to `snippet_text` because the LCM store sets
/// `content = NULL` for `storage_kind = 'external'` rows; every canonical
/// reader (see `sessions/lcm/raw.rs::raw_message_from_row`) does the same.
/// `tool_name` comes from the `session_messages` projection (unique on
/// `provider` + `message_id`); `pinned` is not tracked by the LCM store and is
/// always 0. `summary_node_ids` is the message→summaries linkage via the
/// indexed `lcm_summary_sources (source_kind, source_id)` lookup; it is a
/// JSON-encoded string here and parsed into an array by
/// `parse_summary_node_ids` before rows are returned.
const MESSAGE_COLUMNS: &str = "m.store_id,
       m.session_id,
       m.role,
       CASE WHEN m.provider IS NULL OR TRIM(m.provider) = '' THEN 'unknown' ELSE m.provider END AS source,
       m.timestamp,
       (LENGTH(COALESCE(m.content, m.snippet_text, '')) + 3) / 4 AS token_estimate,
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
          AND s.source_id = CAST(m.store_id AS TEXT)) AS summary_node_ids";

/// Summary-node SELECT list (alias `n`), matching the hermes-lcm columns.
const NODE_COLUMNS: &str = "n.node_id,
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

/// Replaces the JSON-encoded `summary_node_ids` string emitted by
/// `MESSAGE_COLUMNS` with a real JSON array in each message row.
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

fn ok(payload: Map<String, Value>) -> LcmResponse {
    (StatusCode::OK, Json(Value::Object(payload)))
}

fn query_error(context: &str, err: &str) -> LcmResponse {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{context} query failed: {err}"),
    )
}

async fn query_lcm_rows(
    conn: &libsql::Connection,
    context: &str,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<Vec<Value>, LcmResponse> {
    query_rows(conn, sql, params)
        .await
        .map_err(|err| query_error(context, &err))
}

/// LCM store paths whose summary metadata has already validated clean this
/// process. Validation is a full `lcm_summary_nodes` scan; the invariant is
/// writer-enforced, so a store that passed once is not re-scanned on every
/// request. Failures are NOT cached — a store with a malformed row keeps
/// returning 422 until it is repaired.
static VALIDATED_METADATA_STORES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

async fn ensure_valid_summary_metadata(
    state: &DashboardState,
    conn: &libsql::Connection,
    context: &str,
) -> Result<(), LcmResponse> {
    let validated = VALIDATED_METADATA_STORES.get_or_init(|| Mutex::new(HashSet::new()));
    if validated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&state.lcm_db_path)
    {
        return Ok(());
    }
    let mut rows = conn
        .query(
            "SELECT node_id
             FROM lcm_summary_nodes
             WHERE metadata_json IS NOT NULL
               AND NOT json_valid(metadata_json)
             LIMIT 1",
            (),
        )
        .await
        .map_err(|err| query_error(context, &err.to_string()))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|err| query_error(context, &err.to_string()))?
    {
        let node_id: String = row.get(0).unwrap_or_default();
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

#[derive(Deserialize)]
pub(crate) struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

/// `GET /api/plugins/hermes-lcm/overview`
pub(crate) async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 25, 200);
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "overview": empty_overview(),
        "latest_sessions": [],
        "latest_summary_nodes": [],
        "matches": { "messages": [], "summary_nodes": [] },
        "query": params.q,
        "limit": limit,
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(ok(payload));
    };
    ensure_valid_summary_metadata(&state, conn, "overview").await?;

    let mut overview = Map::new();
    overview.insert(
        "messages_total".into(),
        json!(query_i64(conn, "SELECT COUNT(*) FROM lcm_raw_messages", ()).await),
    );
    overview.insert(
        "sessions_total".into(),
        json!(
            query_i64(
                conn,
                "SELECT COUNT(DISTINCT session_id) FROM lcm_raw_messages",
                ()
            )
            .await
        ),
    );
    overview.insert(
        "role_counts".into(),
        json!(
            query_lcm_rows(
                conn,
                "overview role counts",
                "SELECT role, COUNT(*) AS count
                 FROM lcm_raw_messages
                 GROUP BY role
                 ORDER BY count DESC, role ASC",
                (),
            )
            .await?
        ),
    );
    overview.insert(
        "source_counts".into(),
        json!(
            query_lcm_rows(
                conn,
                "overview source counts",
                "SELECT CASE WHEN provider IS NULL OR TRIM(provider) = '' THEN 'unknown' ELSE provider END AS source,
                        COUNT(*) AS count
                 FROM lcm_raw_messages
                 GROUP BY source
                 ORDER BY count DESC, source ASC",
                (),
            )
            .await?
        ),
    );
    overview.insert(
        "summary_nodes_total".into(),
        json!(query_i64(conn, "SELECT COUNT(*) FROM lcm_summary_nodes", ()).await),
    );
    overview.insert(
        "summary_node_sessions_total".into(),
        json!(
            query_i64(
                conn,
                "SELECT COUNT(DISTINCT session_id) FROM lcm_summary_nodes",
                ()
            )
            .await
        ),
    );
    overview.insert(
        "max_summary_depth".into(),
        json!(
            query_i64(
                conn,
                "SELECT COALESCE(MAX(depth), 0) FROM lcm_summary_nodes",
                ()
            )
            .await
        ),
    );
    overview.insert(
        "depth_counts".into(),
        json!(
            query_lcm_rows(
                conn,
                "overview depth counts",
                "SELECT depth, COUNT(*) AS count
                 FROM lcm_summary_nodes
                 GROUP BY depth
                 ORDER BY depth ASC",
                (),
            )
            .await?
        ),
    );
    let src_tok = query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let out_tok = query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let node_count = query_i64(conn, "SELECT COUNT(*) FROM lcm_summary_nodes", ()).await;
    overview.insert(
        "compression".into(),
        json!({
            "source_token_count": src_tok,
            "token_count": out_tok,
            "ratio": ratio(src_tok, out_tok),
            "node_count": node_count,
        }),
    );
    payload.insert("overview".into(), Value::Object(overview));

    payload.insert(
        "latest_sessions".into(),
        json!(
            query_lcm_rows(
                conn,
                "latest_sessions",
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
            .await?
        ),
    );
    payload.insert(
        "latest_summary_nodes".into(),
        json!(
            query_lcm_rows(
                conn,
                "latest_summary_nodes",
                &format!(
                    "SELECT {NODE_COLUMNS}
                     FROM lcm_summary_nodes n
                     ORDER BY COALESCE(n.source_time_end, n.created_at) DESC, n.rowid DESC
                     LIMIT ?1"
                ),
                libsql::params![limit],
            )
            .await?
        ),
    );

    let query = params.q.trim();
    if !query.is_empty() {
        let like = like_pattern(query);
        // Match against index_text/snippet_text/content like the canonical
        // LIKE fallback in sessions/lcm/query.rs (externalized rows have
        // content = NULL and are only findable via the derived columns).
        let mut message_matches = query_lcm_rows(
            conn,
            "overview message matches",
            &format!(
                "SELECT {MESSAGE_COLUMNS}
                 FROM lcm_raw_messages m
                 WHERE (m.index_text LIKE ?1 ESCAPE '\\'
                        OR m.snippet_text LIKE ?1 ESCAPE '\\'
                        OR COALESCE(m.content, '') LIKE ?1 ESCAPE '\\')
                 ORDER BY m.timestamp DESC, m.store_id DESC
                 LIMIT ?2"
            ),
            libsql::params![like.clone(), limit],
        )
        .await?;
        parse_summary_node_ids(&mut message_matches);
        let node_matches = query_lcm_rows(
            conn,
            "overview summary node matches",
            &format!(
                "SELECT {NODE_COLUMNS},
                        COALESCE(n.source_time_end, n.created_at) AS recency
                 FROM lcm_summary_nodes n
                 WHERE n.summary_text LIKE ?1 ESCAPE '\\'
                    OR COALESCE(n.expand_hint, '') LIKE ?1 ESCAPE '\\'
                 ORDER BY recency DESC, n.rowid DESC
                 LIMIT ?2"
            ),
            libsql::params![like, limit],
        )
        .await?;
        payload.insert(
            "matches".into(),
            json!({ "messages": message_matches, "summary_nodes": node_matches }),
        );
    }

    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct SearchParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
    #[serde(default)]
    role: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    since: String,
    #[serde(default)]
    until: String,
}

fn parse_epoch(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

/// `GET /api/plugins/hermes-lcm/search`
pub(crate) async fn search(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 25, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let since = parse_epoch(&params.since);
    let until = parse_epoch(&params.until);
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "query": params.q,
        "limit": limit,
        "offset": offset,
        "engine": "none",
        "engine_detail": { "messages": "none", "summary_nodes": "none" },
        "total": { "messages": 0, "summary_nodes": 0 },
        "filters": {
            "role": if params.role.is_empty() { Value::Null } else { json!(params.role) },
            "source": if params.source.is_empty() { Value::Null } else { json!(params.source) },
            "session_id": if params.session_id.is_empty() { Value::Null } else { json!(params.session_id) },
            "since": since,
            "until": until,
        },
        "matches": { "messages": [], "summary_nodes": [] },
    }));
    let query = params.q.trim().to_string();
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(ok(payload));
    };
    if query.is_empty() {
        return Ok(ok(payload));
    }
    ensure_valid_summary_metadata(&state, conn, "search").await?;

    // Message facets, shared between the FTS and LIKE paths.
    let mut facet_clauses: Vec<String> = Vec::new();
    let mut facet_params: Vec<libsql::Value> = Vec::new();
    if !params.role.is_empty() {
        facet_clauses.push("m.role = ?".into());
        facet_params.push(libsql::Value::Text(params.role.clone()));
    }
    if !params.source.is_empty() {
        if params.source == "unknown" {
            facet_clauses.push("(m.provider IS NULL OR TRIM(m.provider) = '')".into());
        } else {
            facet_clauses.push("m.provider = ?".into());
            facet_params.push(libsql::Value::Text(params.source.clone()));
        }
    }
    if !params.session_id.is_empty() {
        facet_clauses.push("m.session_id = ?".into());
        facet_params.push(libsql::Value::Text(params.session_id.clone()));
    }
    if let Some(since) = since {
        facet_clauses.push("m.timestamp >= ?".into());
        facet_params.push(libsql::Value::Real(since));
    }
    if let Some(until) = until {
        facet_clauses.push("m.timestamp <= ?".into());
        facet_params.push(libsql::Value::Real(until));
    }

    let match_expr = build_fts_match(&query);
    let like = like_pattern(&query);
    let mut message_engine = "like";

    let mut message_matches: Option<Vec<Value>> = None;
    let mut message_total = 0_i64;
    if let Some(expr) = &match_expr {
        let mut where_clauses = vec!["lcm_raw_messages_fts MATCH ?".to_string()];
        where_clauses.extend(facet_clauses.iter().cloned());
        let mut fts_params = vec![libsql::Value::Text(expr.clone())];
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
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS},
                    snippet(lcm_raw_messages_fts, 0, '[', ']', '…', 12) AS snippet
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages m ON m.store_id = lcm_raw_messages_fts.rowid
             WHERE {}
             ORDER BY rank
             LIMIT ? OFFSET ?",
            where_clauses.join(" AND ")
        );
        if let Ok(rows) = query_rows(conn, &sql, fts_params).await {
            message_matches = Some(rows);
            message_engine = "fts";
            message_total = query_i64(conn, &count_sql, count_params).await;
        }
    }
    let mut message_matches = if let Some(rows) = message_matches {
        rows
    } else {
        // LIKE fallback matches index_text/snippet_text/content, mirroring
        // the canonical raw_like_grep_hits in sessions/lcm/query.rs so
        // externalized (content = NULL) messages stay searchable.
        let mut where_clauses = vec!["(m.index_text LIKE ? ESCAPE '\\'
              OR m.snippet_text LIKE ? ESCAPE '\\'
              OR COALESCE(m.content, '') LIKE ? ESCAPE '\\')"
            .to_string()];
        where_clauses.extend(facet_clauses.iter().cloned());
        let mut like_params = vec![
            libsql::Value::Text(like.clone()),
            libsql::Value::Text(like.clone()),
            libsql::Value::Text(like.clone()),
        ];
        like_params.extend(facet_params.iter().cloned());
        let count_sql = format!(
            "SELECT COUNT(*) FROM lcm_raw_messages m WHERE {}",
            where_clauses.join(" AND ")
        );
        message_total = query_i64(conn, &count_sql, like_params.clone()).await;
        like_params.push(libsql::Value::Integer(limit));
        like_params.push(libsql::Value::Integer(offset));
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS},
                    substr(COALESCE(m.content, m.snippet_text, ''), 1, 280) AS snippet
             FROM lcm_raw_messages m
             WHERE {}
             ORDER BY m.timestamp DESC, m.store_id DESC
             LIMIT ? OFFSET ?",
            where_clauses.join(" AND ")
        );
        query_lcm_rows(conn, "search message LIKE fallback", &sql, like_params).await?
    };
    parse_summary_node_ids(&mut message_matches);

    // Node facets: session + time range only (mirrors the original).
    let mut node_clauses: Vec<String> = Vec::new();
    let mut node_params: Vec<libsql::Value> = Vec::new();
    if !params.session_id.is_empty() {
        node_clauses.push("n.session_id = ?".into());
        node_params.push(libsql::Value::Text(params.session_id.clone()));
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
    let mut node_matches: Option<Vec<Value>> = None;
    let mut node_total = 0_i64;
    if let Some(expr) = &match_expr {
        // Qualify the MATCH to summary_text/expand_hint via the FTS5 column
        // filter so metadata_json text (e.g. "category":"general") cannot
        // over-match (the canonical reader matches summary_text only; see
        // summary_grep query in sessions/lcm/query.rs).
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
        if let Ok(rows) = query_rows(conn, &sql, fts_params).await {
            node_matches = Some(rows);
            node_engine = "fts";
            node_total = query_i64(conn, &count_sql, count_params).await;
        }
    }
    let node_matches = if let Some(rows) = node_matches {
        rows
    } else {
        let mut where_clauses = vec![
            "(n.summary_text LIKE ? ESCAPE '\\' OR COALESCE(n.expand_hint, '') LIKE ? ESCAPE '\\')"
                .to_string(),
        ];
        where_clauses.extend(node_clauses.iter().cloned());
        let mut like_params = vec![libsql::Value::Text(like.clone()), libsql::Value::Text(like)];
        like_params.extend(node_params.iter().cloned());
        let count_sql = format!(
            "SELECT COUNT(*) FROM lcm_summary_nodes n WHERE {}",
            where_clauses.join(" AND ")
        );
        node_total = query_i64(conn, &count_sql, like_params.clone()).await;
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
        query_lcm_rows(conn, "search summary node LIKE fallback", &sql, like_params).await?
    };

    // Worst-case engine: only report "fts" when both sections used FTS, so
    // the flag can no longer lie when one section silently fell back to LIKE.
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
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct SessionParams {
    limit: Option<i64>,
    offset: Option<i64>,
    #[serde(default)]
    order: String,
}

/// `GET /api/plugins/hermes-lcm/session/{session_id}`
pub(crate) async fn session(
    State(state): State<DashboardState>,
    JsonPath(session_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<SessionParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 200, 1000);
    let offset = params.offset.unwrap_or(0).max(0);
    let order = if params.order.eq_ignore_ascii_case("desc") {
        "DESC"
    } else {
        "ASC"
    };
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
        return Ok(ok(payload));
    };
    ensure_valid_summary_metadata(&state, conn, "session").await?;

    let message_count = query_i64(
        conn,
        "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
        libsql::params![session_id.clone()],
    )
    .await;
    let summary_node_count = query_i64(
        conn,
        "SELECT COUNT(*) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.clone()],
    )
    .await;
    if message_count == 0 && summary_node_count == 0 {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("session not found: {session_id}"))),
        ));
    }
    let token_estimate_total = query_i64(
        conn,
        "SELECT COALESCE(SUM((LENGTH(COALESCE(content, snippet_text, '')) + 3) / 4), 0)
         FROM lcm_raw_messages WHERE session_id = ?1",
        libsql::params![session_id.clone()],
    )
    .await;
    // Ordinal is the ingest order (NOT NULL in the schema); timestamp alone
    // can transpose same-second messages, so it is only a tie-breaker here.
    let mut messages = query_lcm_rows(
        conn,
        "session messages",
        &format!(
            "SELECT {MESSAGE_COLUMNS}
             FROM lcm_raw_messages m
             WHERE m.session_id = ?1
             ORDER BY m.ordinal {order}, m.timestamp {order}, m.store_id {order}
             LIMIT ?2 OFFSET ?3"
        ),
        libsql::params![session_id.clone(), limit + 1, offset],
    )
    .await?;
    let has_more_messages = messages.len() as i64 > limit;
    messages.truncate(limit as usize);
    parse_summary_node_ids(&mut messages);

    let summary_token_count = query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.clone()],
    )
    .await;
    let source_token_count = query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes WHERE session_id = ?1",
        libsql::params![session_id.clone()],
    )
    .await;
    let mut summary_nodes = query_lcm_rows(
        conn,
        "session summary nodes",
        &format!(
            "SELECT {NODE_COLUMNS},
                    COALESCE(n.source_time_end, n.created_at) AS recency
             FROM lcm_summary_nodes n
             WHERE n.session_id = ?1
             ORDER BY n.depth ASC, recency ASC, n.rowid ASC
             LIMIT ?2 OFFSET ?3"
        ),
        libsql::params![session_id.clone(), limit + 1, offset],
    )
    .await?;
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
    Ok(ok(payload))
}

/// `GET /api/plugins/hermes-lcm/node/{node_id}` — a summary node plus the
/// exact source items it covers (lossless expand).
pub(crate) async fn node(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> LcmResult {
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "node_id": node_id,
        "node": null,
        "sources": { "type": null, "ids": [], "messages": [], "nodes": [] },
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(ok(payload));
    };
    ensure_valid_summary_metadata(&state, conn, "node").await?;

    let node_rows = query_lcm_rows(
        conn,
        "node lookup",
        &format!(
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
        ),
        libsql::params![node_id.clone()],
    )
    .await?;
    let Some(node_row) = node_rows.into_iter().next() else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("summary node not found: {node_id}"))),
        ));
    };
    payload.insert("node".into(), node_row);

    let source_rows = query_lcm_rows(
        conn,
        "node sources",
        "SELECT source_kind, source_id
         FROM lcm_summary_sources
         WHERE node_id = ?1
         ORDER BY ordinal ASC",
        libsql::params![node_id.clone()],
    )
    .await?;

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
        let placeholders = qmarks(child_node_ids.len());
        let params: Vec<libsql::Value> = child_node_ids
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();
        let rows = query_lcm_rows(
            conn,
            "node child summary nodes",
            &format!(
                "SELECT {NODE_COLUMNS},
                        COALESCE(n.source_time_end, n.created_at) AS recency
                 FROM lcm_summary_nodes n
                 WHERE n.node_id IN ({placeholders})
                 ORDER BY recency ASC, n.rowid ASC"
            ),
            params,
        )
        .await?;
        sources_obj.insert("nodes".into(), json!(rows));
    } else if !message_ids.is_empty() {
        let placeholders = qmarks(message_ids.len());
        let params: Vec<libsql::Value> = message_ids
            .iter()
            .map(|id| libsql::Value::Integer(*id))
            .collect();
        let mut rows = query_lcm_rows(
            conn,
            "node source messages",
            &format!(
                "SELECT {MESSAGE_COLUMNS}
                 FROM lcm_raw_messages m
                 WHERE m.store_id IN ({placeholders})"
            ),
            params,
        )
        .await?;
        parse_summary_node_ids(&mut rows);
        // Preserve the node's recorded source order.
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
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct TimelineParams {
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    session_id: String,
    limit: Option<i64>,
}

/// `GET /api/plugins/hermes-lcm/timeline`
pub(crate) async fn timeline(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<TimelineParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 400, 2000);
    let hour = params.bucket.eq_ignore_ascii_case("hour");
    let fmt = if hour { "%Y-%m-%dT%H:00" } else { "%Y-%m-%d" };
    let mut payload = json_object(json!({
        "path": state.lcm_db_path,
        "storage_scope": state.lcm_scope,
        "exists": state.lcm_conn.is_some(),
        "bucket": if hour { "hour" } else { "day" },
        "session_id": if params.session_id.is_empty() { Value::Null } else { json!(params.session_id) },
        "buckets": [],
        "node_buckets": [],
        "undated": {"count": 0, "token_estimate": 0},
    }));
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Ok(ok(payload));
    };

    // NULL timestamps would otherwise collapse into one NULL group rendered
    // as a single fake bar; exclude them from the dated buckets and report
    // them honestly via the `undated` aggregate instead.
    let (msg_where, undated_where, node_where) = if params.session_id.is_empty() {
        (
            "WHERE timestamp IS NOT NULL".to_string(),
            "WHERE timestamp IS NULL".to_string(),
            String::new(),
        )
    } else {
        (
            "WHERE timestamp IS NOT NULL AND session_id = ?2".to_string(),
            "WHERE timestamp IS NULL AND session_id = ?1".to_string(),
            "WHERE session_id = ?2".to_string(),
        )
    };
    let msg_sql = format!(
        "SELECT strftime('{fmt}', timestamp, 'unixepoch') AS bucket,
                COUNT(*) AS count,
                COALESCE(SUM((LENGTH(COALESCE(content, snippet_text, '')) + 3) / 4), 0) AS token_estimate
         FROM lcm_raw_messages
         {msg_where}
         GROUP BY bucket
         ORDER BY bucket ASC
         LIMIT ?1"
    );
    let undated_sql = format!(
        "SELECT COUNT(*) AS count,
                COALESCE(SUM((LENGTH(COALESCE(content, snippet_text, '')) + 3) / 4), 0) AS token_estimate
         FROM lcm_raw_messages
         {undated_where}"
    );
    let node_sql = format!(
        "SELECT strftime('{fmt}', COALESCE(source_time_end, created_at), 'unixepoch') AS bucket,
                COUNT(*) AS count
         FROM lcm_summary_nodes
         {node_where}
         GROUP BY bucket
         ORDER BY bucket ASC
         LIMIT ?1"
    );
    let (buckets, undated, node_buckets) = if params.session_id.is_empty() {
        (
            query_lcm_rows(
                conn,
                "timeline message buckets",
                &msg_sql,
                libsql::params![limit],
            )
            .await?,
            query_lcm_rows(conn, "timeline undated messages", &undated_sql, ()).await?,
            query_lcm_rows(
                conn,
                "timeline summary buckets",
                &node_sql,
                libsql::params![limit],
            )
            .await?,
        )
    } else {
        (
            query_lcm_rows(
                conn,
                "timeline message buckets",
                &msg_sql,
                libsql::params![limit, params.session_id.clone()],
            )
            .await?,
            query_lcm_rows(
                conn,
                "timeline undated messages",
                &undated_sql,
                libsql::params![params.session_id.clone()],
            )
            .await?,
            query_lcm_rows(
                conn,
                "timeline summary buckets",
                &node_sql,
                libsql::params![limit, params.session_id.clone()],
            )
            .await?,
        )
    };
    payload.insert("buckets".into(), json!(buckets));
    payload.insert(
        "undated".into(),
        undated
            .into_iter()
            .next_back()
            .unwrap_or_else(|| json!({"count": 0, "token_estimate": 0})),
    );
    payload.insert("node_buckets".into(), json!(node_buckets));
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct CompressionParams {
    #[serde(default)]
    by: String,
    limit: Option<i64>,
}

/// `GET /api/plugins/hermes-lcm/compression`
pub(crate) async fn compression(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<CompressionParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 50, 500);
    let by_node = params.by.eq_ignore_ascii_case("node");
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
        return Ok(ok(payload));
    };

    let src = query_i64(
        conn,
        "SELECT COALESCE(SUM(source_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let out = query_i64(
        conn,
        "SELECT COALESCE(SUM(summary_token_count), 0) FROM lcm_summary_nodes",
        (),
    )
    .await;
    let n = query_i64(conn, "SELECT COUNT(*) FROM lcm_summary_nodes", ()).await;
    payload.insert(
        "overall".into(),
        json!({
            "source_token_count": src,
            "token_count": out,
            "ratio": ratio(src, out),
            "node_count": n,
        }),
    );

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
    let mut groups = query_lcm_rows(
        conn,
        "compression groups",
        groups_sql,
        libsql::params![limit],
    )
    .await?;
    for group in &mut groups {
        let src = group
            .get("source_token_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let out = group
            .get("token_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(obj) = group.as_object_mut() {
            obj.insert("ratio".into(), json!(ratio(src, out)));
        }
    }
    payload.insert("groups".into(), json!(groups));
    Ok(ok(payload))
}
