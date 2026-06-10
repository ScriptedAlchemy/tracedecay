//! Savings & Cost dashboard API (`/api/plugins/savings/*`).
//!
//! Two data stores feed this tab:
//!
//! - **Global accounting DB** (`~/.tokensave/global.db`, the store behind
//!   `tokensave gain` / `tokensave cost` / `tokensave monitor`): the
//!   `savings_ledger` event log, the legacy per-project `projects.tokens_saved`
//!   lifetime counters, and the `turns` cost table (Claude Code transcripts,
//!   cost computed from real usage data at ingest — labeled `actual`).
//!   Ledger aggregation reuses [`GlobalDb::sum_savings`] /
//!   [`GlobalDb::savings_history`], the same queries `tokensave gain` runs.
//! - **Session store** (the LCM store the dashboard already serves —
//!   project-local `sessions.db` by default): `sessions` +
//!   `session_messages`, whose `model` and `metadata_json` columns drive
//!   per-session cost accounting.
//!
//! Token counts carry an explicit provenance label everywhere:
//! `cost_basis: "actual"` when the transcript recorded usage data
//! (`metadata_json.usage.*`), `"estimated"` when counts come from the same
//! chars/4 heuristic the LCM views use (`(LENGTH(text)+3)/4`), `"mixed"` for
//! sessions containing both. Dollar costs are computed client-side from the
//! `/pricing` table (see `savings_pricing`); unknown models keep their token
//! counts but get no invented price.

use axum::extract::State;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::util::{coerce_limit, query_i64, query_rows, JsonQuery};
use super::{savings_pricing, DashboardState};
use crate::accounting::metrics::parse_range;
use crate::global_db::GlobalDb;

/// Per-message provenance + token columns, derived once and reused by every
/// aggregate below. Usage fields accept both the Anthropic
/// (`input_tokens`/`output_tokens`) and `OpenAI` (`prompt_tokens`/
/// `completion_tokens`) transcript shapes; `json_valid` guards keep one
/// malformed `metadata_json` row from failing the whole query.
const MESSAGE_TOKENS_CTE: &str = "
    SELECT provider,
           session_id,
           role,
           timestamp,
           COALESCE(NULLIF(TRIM(COALESCE(model, '')), ''), '') AS model,
           (LENGTH(COALESCE(text, '')) + 3) / 4 AS est_tokens,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(COALESCE(json_extract(metadata_json, '$.usage.input_tokens'),
                             json_extract(metadata_json, '$.usage.prompt_tokens')) AS INTEGER)
           END AS usage_in,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(COALESCE(json_extract(metadata_json, '$.usage.output_tokens'),
                             json_extract(metadata_json, '$.usage.completion_tokens')) AS INTEGER)
           END AS usage_out,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(json_extract(metadata_json, '$.usage.cache_read_input_tokens') AS INTEGER)
           END AS usage_cache_read,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(json_extract(metadata_json, '$.usage.cache_creation_input_tokens') AS INTEGER)
           END AS usage_cache_write
    FROM session_messages";

/// Aggregate SELECT list shared by the per-session and per-model rollups.
/// "Actual" sums only count usage-bearing messages; estimated sums only count
/// the rest, attributing non-assistant text to input and assistant text to
/// output (a deliberate lower bound — resent context is not modeled).
const TOKEN_AGG_COLUMNS: &str = "
    COUNT(*) AS messages,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN 1 ELSE 0 END) AS usage_messages,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN COALESCE(usage_in, 0) ELSE 0 END) AS actual_input_tokens,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN COALESCE(usage_out, 0) ELSE 0 END) AS actual_output_tokens,
    SUM(COALESCE(usage_cache_read, 0)) AS cache_read_tokens,
    SUM(COALESCE(usage_cache_write, 0)) AS cache_write_tokens,
    SUM(CASE WHEN usage_in IS NULL AND usage_out IS NULL AND role <> 'assistant' THEN est_tokens ELSE 0 END) AS estimated_input_tokens,
    SUM(CASE WHEN usage_in IS NULL AND usage_out IS NULL AND role = 'assistant' THEN est_tokens ELSE 0 END) AS estimated_output_tokens";

#[derive(Deserialize)]
pub(crate) struct RangeParams {
    range: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SessionsParams {
    range: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn range_since(range: Option<&str>) -> (String, i64) {
    let range = range.unwrap_or("all").to_string();
    let since = parse_range(&range) as i64;
    (range, since)
}

fn i64_of(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn str_of<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `""` (no model recorded) → JSON null so the UI can render an explicit
/// "unknown model" row instead of an empty label.
fn model_value(model: &str) -> Value {
    if model.is_empty() {
        Value::Null
    } else {
        Value::String(model.to_string())
    }
}

fn basis_label(usage_messages: i64, messages: i64) -> &'static str {
    if messages > 0 && usage_messages >= messages {
        "actual"
    } else if usage_messages > 0 {
        "mixed"
    } else {
        "estimated"
    }
}

/// Token-aggregate JSON shared by session-model and model rows.
fn token_block(row: &Value) -> Value {
    let messages = i64_of(row, "messages");
    let usage_messages = i64_of(row, "usage_messages");
    json!({
        "messages": messages,
        "usage_messages": usage_messages,
        "estimated_messages": messages - usage_messages,
        "cost_basis": basis_label(usage_messages, messages),
        "actual": {
            "input_tokens": i64_of(row, "actual_input_tokens"),
            "output_tokens": i64_of(row, "actual_output_tokens"),
            "cache_read_tokens": i64_of(row, "cache_read_tokens"),
            "cache_write_tokens": i64_of(row, "cache_write_tokens"),
        },
        "estimated": {
            "input_tokens": i64_of(row, "estimated_input_tokens"),
            "output_tokens": i64_of(row, "estimated_output_tokens"),
        },
    })
}

fn merge(base: Value, extra: Value) -> Value {
    let (Value::Object(mut base_map), Value::Object(extra_map)) = (base, extra) else {
        return Value::Null;
    };
    base_map.extend(extra_map);
    Value::Object(base_map)
}

/// GET `/api/plugins/savings/overview`
pub(crate) async fn overview(State(state): State<DashboardState>) -> Json<Value> {
    savings_pricing::ensure_background_refresh();

    let savings = match state.savings_db.as_deref() {
        Some(gdb) => savings_overview(gdb, &state.savings_db_path).await,
        None => json!({ "available": false, "db": state.savings_db_path }),
    };
    let sessions = match state.lcm_conn.as_ref() {
        Some(conn) => sessions_overview(conn, &state).await,
        None => json!({ "available": false, "db": state.lcm_db_path }),
    };
    let turns = match state.savings_db.as_deref() {
        Some(gdb) => turns_overview(gdb).await,
        None => json!({ "available": false }),
    };
    let pricing_full = savings_pricing::pricing_payload();
    let pricing = json!({
        "source": pricing_full.get("source"),
        "fetched_at": pricing_full.get("fetched_at"),
        "offline": pricing_full.get("offline"),
        "model_count": pricing_full.get("model_count"),
    });

    Json(json!({
        "savings": savings,
        "sessions": sessions,
        "turns": turns,
        "pricing": pricing,
    }))
}

async fn savings_overview(gdb: &GlobalDb, db_path: &str) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let today = gdb.sum_savings(None, now - (now % 86_400)).await;
    let week = gdb.sum_savings(None, now - 7 * 86_400).await;
    let month = gdb.sum_savings(None, now - 30 * 86_400).await;
    let all_time = gdb.sum_savings(None, 0).await;

    // Legacy lifetime counters (`projects.tokens_saved`) predate the ledger
    // and often carry history the event log does not — surface both.
    let conn = gdb.dashboard_connection();
    let lifetime_projects = query_rows(
        &conn,
        "SELECT path, tokens_saved FROM projects
         WHERE tokens_saved > 0 ORDER BY tokens_saved DESC LIMIT 25",
        (),
    )
    .await
    .unwrap_or_default();
    let lifetime_total = query_i64(
        &conn,
        "SELECT COALESCE(SUM(tokens_saved), 0) FROM projects",
        (),
    )
    .await;

    let sum_json = |total: &crate::global_db::SavingsTotal| {
        json!({ "saved_tokens": total.saved_tokens, "calls": total.calls })
    };
    json!({
        "available": true,
        "db": db_path,
        "ledger": {
            "today": sum_json(&today),
            "last_7d": sum_json(&week),
            "last_30d": sum_json(&month),
            "all_time": sum_json(&all_time),
        },
        "lifetime_counters": {
            "total_tokens_saved": lifetime_total,
            "projects": lifetime_projects.iter().map(|row| json!({
                "path": str_of(row, "path"),
                "tokens_saved": i64_of(row, "tokens_saved"),
            })).collect::<Vec<_>>(),
        },
    })
}

async fn sessions_overview(conn: &libsql::Connection, state: &DashboardState) -> Value {
    let sql = format!(
        "SELECT {TOKEN_AGG_COLUMNS},
                COUNT(DISTINCT session_id) AS session_count,
                COUNT(DISTINCT CASE WHEN model <> '' THEN model END) AS model_count,
                SUM(CASE WHEN model = '' THEN 1 ELSE 0 END) AS unknown_model_messages
         FROM ({MESSAGE_TOKENS_CTE})"
    );
    let rows = query_rows(conn, &sql, ()).await.unwrap_or_default();
    let agg = rows.first().cloned().unwrap_or_else(|| json!({}));
    let session_count = query_i64(conn, "SELECT COUNT(*) FROM sessions", ()).await;

    merge(
        token_block(&agg),
        json!({
            "available": true,
            "db": state.lcm_db_path,
            "scope": state.lcm_scope,
            "session_count": session_count,
            "model_count": i64_of(&agg, "model_count"),
            "unknown_model_messages": i64_of(&agg, "unknown_model_messages"),
        }),
    )
}

async fn turns_overview(gdb: &GlobalDb) -> Value {
    let conn = gdb.dashboard_connection();
    let turn_count = query_i64(&conn, "SELECT COUNT(*) FROM turns", ()).await;
    let total_cost = gdb.total_cost_since(0).await.unwrap_or(0.0);
    let total_tokens = gdb.total_tokens_since(0).await.unwrap_or(0);
    json!({
        "available": true,
        "turn_count": turn_count,
        "total_cost_usd": total_cost,
        "total_tokens": total_tokens,
        "cost_basis": "actual",
    })
}

/// GET `/api/plugins/savings/ledger?range=today|7d|30d|all`
pub(crate) async fn ledger(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = range_since(params.range.as_deref());
    let Some(gdb) = state.savings_db.as_deref() else {
        return Json(json!({
            "available": false,
            "db": state.savings_db_path,
            "range": range,
        }));
    };

    let total = gdb.sum_savings(None, since).await;
    let history = gdb.savings_history(None, since).await;
    let conn = gdb.dashboard_connection();
    let by_tool = query_rows(
        &conn,
        "SELECT tool_name,
                COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0) AS saved_tokens,
                COUNT(*) AS calls
         FROM savings_ledger WHERE ts >= ?1
         GROUP BY tool_name ORDER BY saved_tokens DESC LIMIT 50",
        libsql::params![since],
    )
    .await
    .unwrap_or_default();
    let by_project = query_rows(
        &conn,
        "SELECT project_path,
                COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0) AS saved_tokens,
                COUNT(*) AS calls
         FROM savings_ledger WHERE ts >= ?1
         GROUP BY project_path ORDER BY saved_tokens DESC LIMIT 50",
        libsql::params![since],
    )
    .await
    .unwrap_or_default();

    Json(json!({
        "available": true,
        "db": state.savings_db_path,
        "range": range,
        "since": since,
        "total": { "saved_tokens": total.saved_tokens, "calls": total.calls },
        "by_day": history.iter().map(|day| json!({
            "day": day.day,
            "saved_tokens": day.saved_tokens,
            "calls": day.calls,
        })).collect::<Vec<_>>(),
        "by_tool": by_tool.iter().map(|row| json!({
            "tool": str_of(row, "tool_name"),
            "saved_tokens": i64_of(row, "saved_tokens"),
            "calls": i64_of(row, "calls"),
        })).collect::<Vec<_>>(),
        "by_project": by_project.iter().map(|row| json!({
            "project": str_of(row, "project_path"),
            "saved_tokens": i64_of(row, "saved_tokens"),
            "calls": i64_of(row, "calls"),
        })).collect::<Vec<_>>(),
    }))
}

/// GET `/api/plugins/savings/sessions?range=&limit=&offset=`
///
/// Sessions without any timestamp (neither `started_at` nor message
/// timestamps — true for Cursor hook ingests today) are only included in the
/// default `all` range, since they cannot be placed on a timeline.
pub(crate) async fn sessions(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SessionsParams>,
) -> Json<Value> {
    let (range, since) = range_since(params.range.as_deref());
    let limit = coerce_limit(params.limit, 25, 100);
    let offset = params.offset.unwrap_or(0).max(0);
    let Some(conn) = state.lcm_conn.as_ref() else {
        return Json(json!({
            "available": false,
            "db": state.lcm_db_path,
            "range": range,
            "sessions": [],
            "total": 0,
        }));
    };

    let page_sql = "
        SELECT s.provider, s.session_id, s.title, s.started_at, s.ended_at,
               s.is_subagent,
               (SELECT MAX(m.timestamp) FROM session_messages m
                 WHERE m.provider = s.provider AND m.session_id = s.session_id) AS last_message_at
        FROM sessions s
        WHERE ?1 = 0 OR COALESCE(s.started_at,
              (SELECT MAX(m.timestamp) FROM session_messages m
                WHERE m.provider = s.provider AND m.session_id = s.session_id), 0) >= ?1
        ORDER BY (s.started_at IS NULL), s.started_at DESC, s.rowid DESC
        LIMIT ?2 OFFSET ?3";
    let page = query_rows(conn, page_sql, libsql::params![since, limit, offset])
        .await
        .unwrap_or_default();
    let total = query_i64(
        conn,
        "SELECT COUNT(*) FROM sessions s
         WHERE ?1 = 0 OR COALESCE(s.started_at,
               (SELECT MAX(m.timestamp) FROM session_messages m
                 WHERE m.provider = s.provider AND m.session_id = s.session_id), 0) >= ?1",
        libsql::params![since],
    )
    .await;

    let mut sessions_json = Vec::with_capacity(page.len());
    for row in &page {
        let provider = str_of(row, "provider");
        let session_id = str_of(row, "session_id");
        let agg_sql = format!(
            "SELECT model, {TOKEN_AGG_COLUMNS}
             FROM ({MESSAGE_TOKENS_CTE})
             WHERE provider = ?1 AND session_id = ?2
             GROUP BY model ORDER BY messages DESC"
        );
        let model_rows = query_rows(conn, &agg_sql, libsql::params![provider, session_id])
            .await
            .unwrap_or_default();

        let mut messages = 0;
        let mut usage_messages = 0;
        let models: Vec<Value> = model_rows
            .iter()
            .map(|model_row| {
                messages += i64_of(model_row, "messages");
                usage_messages += i64_of(model_row, "usage_messages");
                merge(
                    token_block(model_row),
                    json!({ "model": model_value(str_of(model_row, "model")) }),
                )
            })
            .collect();

        sessions_json.push(json!({
            "provider": provider,
            "session_id": session_id,
            "title": row.get("title").cloned().unwrap_or(Value::Null),
            "started_at": row.get("started_at").cloned().unwrap_or(Value::Null),
            "last_message_at": row.get("last_message_at").cloned().unwrap_or(Value::Null),
            "is_subagent": i64_of(row, "is_subagent") != 0,
            "messages": messages,
            "usage_messages": usage_messages,
            "estimated_messages": messages - usage_messages,
            "cost_basis": basis_label(usage_messages, messages),
            "models": models,
        }));
    }

    Json(json!({
        "available": true,
        "db": state.lcm_db_path,
        "scope": state.lcm_scope,
        "range": range,
        "since": since,
        "total": total,
        "sessions": sessions_json,
    }))
}

/// GET `/api/plugins/savings/models?range=`
///
/// Per-model token aggregates from the session store, per-day series for
/// timestamped messages, plus the `turns` accounting (per-model cost and
/// per-day cost — `actual`, computed from transcript usage at ingest by
/// `tokensave cost`, reusing [`GlobalDb::cost_by_model_since`]).
pub(crate) async fn models(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = range_since(params.range.as_deref());

    let mut payload = json!({
        "available": state.lcm_conn.is_some(),
        "range": range,
        "since": since,
        "models": [],
        "daily": [],
        "turns": { "available": state.savings_db.is_some(), "by_model": [], "by_day": [] },
    });

    if let Some(conn) = state.lcm_conn.as_ref() {
        let model_sql = format!(
            "SELECT model, COUNT(DISTINCT session_id) AS session_count, {TOKEN_AGG_COLUMNS}
             FROM ({MESSAGE_TOKENS_CTE})
             WHERE ?1 = 0 OR COALESCE(timestamp, 0) >= ?1
             GROUP BY model ORDER BY messages DESC LIMIT 100"
        );
        let model_rows = query_rows(conn, &model_sql, libsql::params![since])
            .await
            .unwrap_or_default();
        payload["models"] = Value::Array(
            model_rows
                .iter()
                .map(|row| {
                    merge(
                        token_block(row),
                        json!({
                            "model": model_value(str_of(row, "model")),
                            "sessions": i64_of(row, "session_count"),
                        }),
                    )
                })
                .collect(),
        );

        let daily_sql = format!(
            "SELECT (timestamp / 86400) * 86400 AS day, {TOKEN_AGG_COLUMNS}
             FROM ({MESSAGE_TOKENS_CTE})
             WHERE timestamp IS NOT NULL AND timestamp > 0 AND (?1 = 0 OR timestamp >= ?1)
             GROUP BY day ORDER BY day ASC LIMIT 366"
        );
        let daily_rows = query_rows(conn, &daily_sql, libsql::params![since])
            .await
            .unwrap_or_default();
        payload["daily"] = Value::Array(
            daily_rows
                .iter()
                .map(|row| merge(token_block(row), json!({ "day": i64_of(row, "day") })))
                .collect(),
        );
    }

    if let Some(gdb) = state.savings_db.as_deref() {
        let by_model = gdb.cost_by_model_since(since.max(0) as u64).await;
        payload["turns"]["by_model"] = Value::Array(
            by_model
                .iter()
                .map(|(model, cost, tokens)| {
                    json!({
                        "model": model,
                        "cost_usd": cost,
                        "total_tokens": tokens,
                        "cost_basis": "actual",
                    })
                })
                .collect(),
        );
        let conn = gdb.dashboard_connection();
        let by_day = query_rows(
            &conn,
            "SELECT (timestamp / 86400) * 86400 AS day,
                    SUM(cost_usd) AS cost_usd,
                    SUM(input_tokens + output_tokens) AS total_tokens
             FROM turns WHERE timestamp >= ?1
             GROUP BY day ORDER BY day ASC LIMIT 366",
            libsql::params![since],
        )
        .await
        .unwrap_or_default();
        payload["turns"]["by_day"] = Value::Array(
            by_day
                .iter()
                .map(|row| {
                    json!({
                        "day": i64_of(row, "day"),
                        "cost_usd": row.get("cost_usd").cloned().unwrap_or(Value::Null),
                        "total_tokens": i64_of(row, "total_tokens"),
                    })
                })
                .collect(),
        );
    }

    Json(payload)
}

/// GET `/api/plugins/savings/pricing` — the merged model price table with
/// provenance (`live` data is always served from its disk cache, so `source`
/// is `"cache"` or `"fallback"`).
pub(crate) async fn pricing() -> Json<Value> {
    savings_pricing::ensure_background_refresh();
    Json(savings_pricing::pricing_payload())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_labels() {
        assert_eq!(basis_label(0, 0), "estimated");
        assert_eq!(basis_label(0, 4), "estimated");
        assert_eq!(basis_label(2, 4), "mixed");
        assert_eq!(basis_label(4, 4), "actual");
    }

    #[test]
    fn unknown_model_serializes_as_null() {
        assert_eq!(model_value(""), Value::Null);
        assert_eq!(model_value("gpt-5.5"), Value::String("gpt-5.5".into()));
    }
}
