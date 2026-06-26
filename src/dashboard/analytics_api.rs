//! Read-only durable analytics API for dashboard-level agent behavior.
//!
//! Durable `analytics_events` rows are preferred when available. Older session
//! stores still get session-message usage rollups, and hint lifecycle telemetry
//! falls back to the legacy `dashboard_hint_events` table when present.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::Json;
use serde_json::{json, Value};

use crate::analytics::{
    categorize_skill, infer_usage_events, underused_tool_family_signals, ToolUsageObservation,
    UsageKind,
};
use crate::global_db::{AnalyticsEventQuery, AnalyticsEventRecord, GlobalDb};

use super::util::{i64_field, query_i64, query_rows, str_field};
use super::DashboardState;

const HINT_CATEGORIES: &[&str] = &[
    "search",
    "semantic_search",
    "file_read",
    "broad_read",
    "call_graph",
    "impact",
    "symbol_lookup",
    "file_lookup",
    "explore_subagent",
    "subagent_start_context",
];
const ANALYTICS_EVENT_LIMIT: usize = 10_000;

#[derive(Default)]
struct HintCounts {
    emitted: i64,
    followed: i64,
    ignored: i64,
    suppressed: i64,
}

/// `GET /api/plugins/analytics/overview`
pub(crate) async fn overview(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let hints = hint_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await;
    let usage = usage_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await;
    let diagnostics = diagnostics_summary(&state, durable_events.as_deref()).await;
    let underused = underused_tool_families(state.lcm_conn.as_ref()).await;

    Json(json!({
        "available": state.lcm_conn.is_some() || durable_events.is_some(),
        "db": state.lcm_db_path,
        "scope": state.lcm_scope,
        "hints": hints,
        "usage": usage,
        "diagnostics": diagnostics,
        "underused_tool_families": underused,
    }))
}

/// `GET /api/plugins/analytics/hints`
pub(crate) async fn hints(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(hint_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/usage`
pub(crate) async fn usage(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(usage_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/diagnostics`
pub(crate) async fn diagnostics(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(diagnostics_summary(&state, durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/underused`
pub(crate) async fn underused(State(state): State<DashboardState>) -> Json<Value> {
    Json(json!({
        "available": state.lcm_conn.is_some(),
        "db": state.lcm_db_path,
        "families": underused_tool_families(state.lcm_conn.as_ref()).await,
    }))
}

fn empty_hint_rows() -> Vec<Value> {
    HINT_CATEGORIES
        .iter()
        .map(|category| {
            json!({
                "category": category,
                "emitted": 0,
                "followed": 0,
                "ignored": 0,
                "suppressed": 0,
            })
        })
        .collect()
}

async fn durable_analytics_rows_for_state(state: &DashboardState) -> Option<Vec<Value>> {
    durable_analytics_rows(
        state.savings_db.as_deref(),
        state.lcm_conn.as_ref(),
        &GlobalDb::canonical_project_key(&state.project_root),
    )
    .await
}

async fn durable_analytics_rows(
    global_db: Option<&GlobalDb>,
    lcm_conn: Option<&libsql::Connection>,
    project_id: &str,
) -> Option<Vec<Value>> {
    if let Some(db) = global_db {
        if let Ok(events) = db
            .query_analytics_events(&AnalyticsEventQuery {
                provider: None,
                project_id: Some(project_id.to_string()),
                session_id: None,
                event_kind: None,
                limit: ANALYTICS_EVENT_LIMIT,
            })
            .await
        {
            if !events.is_empty() {
                return Some(events.iter().map(durable_analytics_event_row).collect());
            }
        }
    }

    let rows = query_rows(
        lcm_conn?,
        "SELECT provider, timestamp, event_kind, hook_name, tool_name,
                tool_category, skill_name, hint_category, outcome, metadata_json
         FROM (
             SELECT provider, timestamp, event_kind, hook_name, tool_name,
                    tool_category, skill_name, hint_category, outcome, metadata_json, id
             FROM analytics_events
             WHERE project_id = ?1
             ORDER BY timestamp DESC, id DESC
             LIMIT 10000
         )
         ORDER BY timestamp, id",
        libsql::params![project_id],
    )
    .await
    .ok()?;
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

fn durable_analytics_event_row(event: &AnalyticsEventRecord) -> Value {
    json!({
        "provider": &event.provider,
        "timestamp": event.timestamp,
        "event_kind": &event.event_kind,
        "hook_name": &event.hook_name,
        "tool_name": &event.tool_name,
        "tool_category": &event.tool_category,
        "skill_name": &event.skill_name,
        "hint_category": &event.hint_category,
        "outcome": &event.outcome,
        "metadata_json": &event.metadata_json,
    })
}

fn hint_summary_from_events(events: &[Value]) -> Value {
    let mut by_category: BTreeMap<String, HintCounts> = HINT_CATEGORIES
        .iter()
        .map(|category| ((*category).to_string(), HintCounts::default()))
        .collect();

    for event in events {
        let category = str_field(event, "hint_category");
        if category.is_empty() {
            continue;
        }
        let counts = by_category.entry(category.to_string()).or_default();
        match normalize(str_field(event, "outcome")).as_str() {
            "emitted" | "shown" => counts.emitted += 1,
            "followed" => counts.followed += 1,
            "ignored" => counts.ignored += 1,
            "suppressed" => counts.suppressed += 1,
            _ => {}
        }
    }

    json!({
        "available": true,
        "source": "analytics_events",
        "by_category": by_category.into_iter().map(|(category, counts)| {
            json!({
                "category": category,
                "emitted": counts.emitted,
                "followed": counts.followed,
                "ignored": counts.ignored,
                "suppressed": counts.suppressed,
            })
        }).collect::<Vec<_>>(),
    })
}

async fn hint_summary(
    conn: Option<&libsql::Connection>,
    durable_events: Option<&[Value]>,
) -> Value {
    if let Some(events) = durable_events {
        return hint_summary_from_events(events);
    }

    let Some(conn) = conn else {
        return json!({
            "available": false,
            "source": "session_store_unavailable",
            "by_category": empty_hint_rows(),
        });
    };

    let has_table = query_i64(
        conn,
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type IN ('table', 'view') AND name = 'dashboard_hint_events'",
        (),
    )
    .await
        > 0;
    if !has_table {
        return json!({
            "available": false,
            "source": "dashboard_hint_events_missing",
            "by_category": empty_hint_rows(),
        });
    }

    let rows = match query_rows(
        conn,
        "SELECT category,
                SUM(CASE WHEN event_type = 'emitted' THEN 1 ELSE 0 END) AS emitted,
                SUM(CASE WHEN event_type = 'followed' THEN 1 ELSE 0 END) AS followed,
                SUM(CASE WHEN event_type = 'ignored' THEN 1 ELSE 0 END) AS ignored,
                SUM(CASE WHEN event_type = 'suppressed' THEN 1 ELSE 0 END) AS suppressed
         FROM dashboard_hint_events
         GROUP BY category
         ORDER BY category",
        (),
    )
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            return json!({
                "available": false,
                "source": "dashboard_hint_events_error",
                "error": err,
                "by_category": empty_hint_rows(),
            });
        }
    };

    let mut by_category: BTreeMap<String, Value> = empty_hint_rows()
        .into_iter()
        .map(|row| (str_field(&row, "category").to_string(), row))
        .collect();
    for row in rows {
        let category = str_field(&row, "category");
        by_category.insert(
            category.to_string(),
            json!({
                "category": category,
                "emitted": i64_field(&row, "emitted"),
                "followed": i64_field(&row, "followed"),
                "ignored": i64_field(&row, "ignored"),
                "suppressed": i64_field(&row, "suppressed"),
            }),
        );
    }

    json!({
        "available": true,
        "source": "dashboard_hint_events",
        "by_category": by_category.into_values().collect::<Vec<_>>(),
    })
}

async fn session_message_rows(conn: Option<&libsql::Connection>) -> Option<Vec<Value>> {
    let conn = conn?;
    query_rows(
        conn,
        "SELECT COALESCE(tool_names, '') AS tool_names,
                COALESCE(text, '') AS text,
                COALESCE(metadata_json, '') AS metadata_json
         FROM session_messages
         ORDER BY timestamp, ordinal
         LIMIT 10000",
        (),
    )
    .await
    .ok()
}

fn usage_summary_from_events(events: &[Value]) -> Value {
    let mut counts: BTreeMap<(String, String), i64> = BTreeMap::new();
    for event in events {
        let event_kind = str_field(event, "event_kind");
        let tool_name = str_field(event, "tool_name");
        let skill_name = str_field(event, "skill_name");
        let metadata_json = str_field(event, "metadata_json");
        record_event_usage(
            &mut counts,
            event_kind,
            tool_name,
            skill_name,
            metadata_json,
        );
    }

    json!({
        "available": true,
        "source": "analytics_events",
        "message_count": events.len() as i64,
        "event_count": events.len() as i64,
        "by_category": usage_count_rows(counts),
    })
}

fn record_event_usage(
    counts: &mut BTreeMap<(String, String), i64>,
    event_kind: &str,
    tool_name: &str,
    skill_name: &str,
    metadata_json: &str,
) {
    let inferred = match event_kind {
        "tool" | "mcp_tool_call" => infer_usage_events(Some(tool_name), Some(metadata_json), None),
        "skill" => infer_usage_events(None, Some(metadata_json), Some(skill_name)),
        _ => Vec::new(),
    };

    if inferred.is_empty() {
        record_fallback_usage(counts, event_kind, skill_name);
        return;
    }

    for event in inferred {
        record_usage_count(counts, event.kind, event.category.dashboard_label());
    }
}

fn record_fallback_usage(
    counts: &mut BTreeMap<(String, String), i64>,
    event_kind: &str,
    skill_name: &str,
) {
    match event_kind {
        "tool" | "mcp_tool_call" => increment_usage_count(counts, "tool", "other_tool"),
        "skill" if !skill_name.is_empty() => {
            increment_usage_count(
                counts,
                "skill",
                categorize_skill(skill_name).dashboard_label(),
            );
        }
        _ => {}
    }
}

fn record_usage_count(
    counts: &mut BTreeMap<(String, String), i64>,
    kind: UsageKind,
    category: &str,
) {
    let kind = match kind {
        UsageKind::Tool => "tool",
        UsageKind::Skill => "skill",
    };
    increment_usage_count(counts, kind, category);
}

fn increment_usage_count(counts: &mut BTreeMap<(String, String), i64>, kind: &str, category: &str) {
    *counts
        .entry((kind.to_string(), category.to_string()))
        .or_default() += 1;
}

async fn usage_summary(
    conn: Option<&libsql::Connection>,
    durable_events: Option<&[Value]>,
) -> Value {
    if let Some(events) = durable_events {
        return usage_summary_from_events(events);
    }

    let Some(rows) = session_message_rows(conn).await else {
        return json!({
            "available": false,
            "message_count": 0,
            "by_category": [],
        });
    };

    let mut counts: BTreeMap<(String, String), i64> = BTreeMap::new();
    for row in &rows {
        for event in infer_usage_events(
            Some(str_field(row, "tool_names")),
            Some(str_field(row, "metadata_json")),
            Some(str_field(row, "text")),
        ) {
            record_usage_count(&mut counts, event.kind, event.category.dashboard_label());
        }
    }

    json!({
        "available": true,
        "message_count": rows.len() as i64,
        "by_category": usage_count_rows(counts),
    })
}

fn usage_count_rows(counts: BTreeMap<(String, String), i64>) -> Vec<Value> {
    counts
        .into_iter()
        .map(|((kind, category), events)| {
            json!({
                "kind": kind,
                "category": category,
                "events": events,
            })
        })
        .collect()
}

async fn diagnostics_summary(state: &DashboardState, durable_events: Option<&[Value]>) -> Value {
    let message_count = session_message_rows(state.lcm_conn.as_ref())
        .await
        .map_or(0, |rows| rows.len() as i64);
    let hook_rows = read_hook_analytics_rows(state);
    let hook_call_count = hook_invocation_count(&hook_rows);

    let Some(events) = durable_events else {
        return json!({
            "available": !hook_rows.is_empty() || message_count > 0,
            "source": "session_messages_and_hook_analytics",
            "message_count": message_count,
            "event_count": 0,
            "tool_call_count": 0,
            "mcp_tool_call_count": 0,
            "tracedecay_call_count": 0,
            "hook_call_count": hook_call_count,
            "ratios": diagnostics_ratios(message_count, 0, 0, 0, hook_call_count),
            "by_event_kind": [],
            "by_tool": [],
            "by_mcp_tool": [],
            "by_tool_category": [],
            "by_outcome": [],
            "by_hook": hook_count_rows(&hook_rows),
            "by_prompt_category": hook_prompt_category_rows(&hook_rows),
            "recent_events": [],
            "recent_hooks": recent_hook_rows(&hook_rows, 20),
        });
    };

    let mut by_event_kind = BTreeMap::new();
    let mut by_tool = BTreeMap::new();
    let mut by_mcp_tool = BTreeMap::new();
    let mut by_tool_category = BTreeMap::new();
    let mut by_outcome = BTreeMap::new();
    let mut tool_call_count = 0;
    let mut mcp_tool_call_count = 0;
    let mut tracedecay_call_count = 0;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;

    for event in events {
        let event_kind = str_field(event, "event_kind");
        let tool_name = str_field(event, "tool_name");
        increment_string_count(&mut by_event_kind, event_kind);
        increment_string_count(&mut by_tool_category, str_field(event, "tool_category"));
        increment_string_count(&mut by_outcome, str_field(event, "outcome"));

        if let Some(ts) = event.get("timestamp").and_then(Value::as_i64) {
            first_ts = Some(first_ts.map_or(ts, |current| current.min(ts)));
            last_ts = Some(last_ts.map_or(ts, |current| current.max(ts)));
        }

        if !tool_name.is_empty() {
            tool_call_count += 1;
            increment_string_count(&mut by_tool, tool_name);
            if event_kind == "mcp_tool_call" || tool_name.starts_with("mcp__") {
                mcp_tool_call_count += 1;
                increment_string_count(&mut by_mcp_tool, tool_name);
            }
            if crate::analytics::normalize_tool_name(tool_name).starts_with("tracedecay_") {
                tracedecay_call_count += 1;
            }
        }
    }

    let span_secs = match (first_ts, last_ts) {
        (Some(first), Some(last)) => last.saturating_sub(first).max(1),
        _ => 0,
    };
    let events_per_hour = if span_secs > 0 {
        (events.len() as f64) * 3600.0 / span_secs as f64
    } else {
        0.0
    };

    json!({
        "available": true,
        "source": "analytics_events",
        "message_count": message_count,
        "event_count": events.len() as i64,
        "tool_call_count": tool_call_count,
        "mcp_tool_call_count": mcp_tool_call_count,
        "tracedecay_call_count": tracedecay_call_count,
        "hook_call_count": hook_call_count,
        "events_per_hour": events_per_hour,
        "ratios": diagnostics_ratios(
            message_count,
            events.len() as i64,
            tool_call_count,
            mcp_tool_call_count,
            hook_call_count,
        ),
        "by_event_kind": count_rows("event_kind", by_event_kind),
        "by_tool": count_rows("tool_name", by_tool),
        "by_mcp_tool": count_rows("tool_name", by_mcp_tool),
        "by_tool_category": count_rows("tool_category", by_tool_category),
        "by_outcome": count_rows("outcome", by_outcome),
        "by_hook": hook_count_rows(&hook_rows),
        "by_prompt_category": hook_prompt_category_rows(&hook_rows),
        "recent_events": recent_event_rows(events, 20),
        "recent_hooks": recent_hook_rows(&hook_rows, 20),
    })
}

fn diagnostics_ratios(
    message_count: i64,
    event_count: i64,
    tool_call_count: i64,
    mcp_tool_call_count: i64,
    hook_call_count: i64,
) -> Value {
    json!({
        "events_per_message": per_message(event_count, message_count),
        "tool_calls_per_message": per_message(tool_call_count, message_count),
        "mcp_tool_calls_per_message": per_message(mcp_tool_call_count, message_count),
        "hook_calls_per_message": per_message(hook_call_count, message_count),
    })
}

fn per_message(count: i64, message_count: i64) -> f64 {
    if message_count <= 0 {
        0.0
    } else {
        count as f64 / message_count as f64
    }
}

fn increment_string_count(counts: &mut BTreeMap<String, i64>, key: &str) {
    if !key.is_empty() {
        *counts.entry(key.to_string()).or_default() += 1;
    }
}

fn count_rows(label: &str, counts: BTreeMap<String, i64>) -> Vec<Value> {
    counts
        .into_iter()
        .map(|(key, count)| json!({ label: key, "count": count }))
        .collect()
}

fn read_hook_analytics_rows(state: &DashboardState) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(state.store_root.join("hook_analytics.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn hook_invocation_count(rows: &[Value]) -> i64 {
    rows.iter()
        .filter(|row| str_field(row, "event") == "hook_invoked")
        .count() as i64
}

fn hook_count_rows(rows: &[Value]) -> Vec<Value> {
    let mut counts = BTreeMap::new();
    for row in rows {
        if str_field(row, "event") == "hook_invoked" {
            increment_string_count(&mut counts, str_field(row, "hook_name"));
        }
    }
    count_rows("hook_name", counts)
}

fn hook_prompt_category_rows(rows: &[Value]) -> Vec<Value> {
    let mut counts = BTreeMap::new();
    for row in rows {
        if str_field(row, "event") == "hook_invoked" {
            increment_string_count(&mut counts, str_field(row, "prompt_category"));
        }
    }
    count_rows("prompt_category", counts)
}

fn recent_event_rows(events: &[Value], limit: usize) -> Vec<Value> {
    events
        .iter()
        .rev()
        .take(limit)
        .map(|event| {
            json!({
                "timestamp": event.get("timestamp").cloned().unwrap_or(Value::Null),
                "event_kind": str_field(event, "event_kind"),
                "hook_name": str_field(event, "hook_name"),
                "tool_name": str_field(event, "tool_name"),
                "outcome": str_field(event, "outcome"),
            })
        })
        .collect()
}

fn recent_hook_rows(rows: &[Value], limit: usize) -> Vec<Value> {
    rows.iter()
        .rev()
        .filter(|row| str_field(row, "event") == "hook_invoked")
        .take(limit)
        .map(|row| {
            json!({
                "ts_unix_ms": row.get("ts_unix_ms").cloned().unwrap_or(Value::Null),
                "agent": str_field(row, "agent"),
                "hook_name": str_field(row, "hook_name"),
                "session_id": str_field(row, "session_id"),
                "tool_name": str_field(row, "tool_name"),
                "prompt_category": str_field(row, "prompt_category"),
            })
        })
        .collect()
}

async fn underused_tool_families(conn: Option<&libsql::Connection>) -> Value {
    let Some(rows) = session_message_rows(conn).await else {
        return Value::Array(Vec::new());
    };

    json!(underused_tool_family_signals(rows.iter().map(|row| {
        let text = str_field(row, "text");
        ToolUsageObservation {
            tool_names: Some(str_field(row, "tool_names")),
            metadata_json: Some(str_field(row, "metadata_json")),
            text: Some(text),
        }
    })))
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}
