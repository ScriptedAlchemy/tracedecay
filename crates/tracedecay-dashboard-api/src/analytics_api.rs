//! Read-only durable analytics API for dashboard-level agent behavior.
//!
//! Durable `analytics_events` rows are preferred when available. Older session
//! stores still get session-message usage rollups, and hint lifecycle telemetry
//! falls back to the legacy `dashboard_hint_events` table when present.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::Json;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{i64_field, query_i64, query_rows, str_field};
use crate::analytics::{
    ToolUsageObservation, UsageKind, categorize_skill, infer_usage_events,
    underused_tool_family_signals,
};

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
#[derive(Default)]
struct HintCounts {
    emitted: i64,
    followed: i64,
    ignored: i64,
    suppressed: i64,
}

/// `GET /api/plugins/analytics/overview`
pub async fn overview(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let hints = hint_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await;
    let usage = usage_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await;
    let agents = agent_usage_summary(state.lcm_conn.as_ref()).await;
    let diagnostics = diagnostics_summary(&state, durable_events.as_deref()).await;
    let underused = underused_tool_families(state.lcm_conn.as_ref()).await;

    Json(json!({
        "available": state.lcm_conn.is_some() || durable_events.is_some(),
        "db": state.lcm_db_path,
        "scope": state.lcm_scope,
        "hints": hints,
        "usage": usage,
        "agents": agents,
        "diagnostics": diagnostics,
        "underused_tool_families": underused,
    }))
}

async fn agent_usage_summary(conn: Option<&libsql::Connection>) -> Value {
    let Some(conn) = conn else {
        return json!({
            "available": false,
            "source": "session_store_unavailable",
            "by_agent": [],
        });
    };

    let rows = query_rows(
        conn,
        "SELECT COALESCE(agent_id, '') AS agent_id,
                COALESCE(metadata_json, '') AS metadata_json
         FROM sessions
         WHERE is_subagent = 1
           AND (COALESCE(agent_id, '') <> '' OR COALESCE(metadata_json, '') <> '')
         ORDER BY agent_id",
        (),
    )
    .await
    .unwrap_or_default();

    let mut by_agent: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let agent_id = str_field(&row, "agent_id");
        let Some(label) =
            managed_agent_label_for_session(agent_id, str_field(&row, "metadata_json"))
        else {
            continue;
        };
        *by_agent.entry(label.to_string()).or_default() += 1;
    }

    json!({
        "available": true,
        "source": "sessions",
        "by_agent": by_agent.into_iter().map(|(agent, sessions)| {
            json!({
                "agent": agent,
                "sessions": sessions,
            })
        }).collect::<Vec<_>>(),
    })
}

fn managed_agent_label_for_session(agent_id: &str, metadata_json: &str) -> Option<&'static str> {
    crate::automation::agent_targets::managed_agent_label(agent_id).or_else(|| {
        let metadata: Value = serde_json::from_str(metadata_json).ok()?;
        ["agent_nickname", "agent_role"]
            .into_iter()
            .filter_map(|key| metadata.get(key).and_then(Value::as_str))
            .find_map(crate::automation::agent_targets::managed_agent_label)
    })
}

/// `GET /api/plugins/analytics/hints`
pub async fn hints(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(hint_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/usage`
pub async fn usage(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(usage_summary(state.lcm_conn.as_ref(), durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/diagnostics`
pub async fn diagnostics(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(diagnostics_summary(&state, durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/underused`
pub async fn underused(State(state): State<DashboardState>) -> Json<Value> {
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
    if let Some(store) = state.accounting_store.as_ref() {
        if let Ok(events) = store.analytics_events(state.project_root.clone()).await {
            if !events.is_empty() {
                return Some(events);
            }
        }
    }

    let project_id = std::fs::canonicalize(&state.project_root)
        .unwrap_or_else(|_| state.project_root.clone())
        .to_string_lossy()
        .to_string();
    let rows = query_rows(
        state.lcm_conn.as_ref()?,
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
    if rows.is_empty() { None } else { Some(rows) }
}

pub fn hint_summary_from_events(events: &[Value]) -> Value {
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
        let event_kind = normalize(str_field(event, "event_kind"));
        match event_kind.as_str() {
            "hint_emitted" | "hint_escalated" | "missing_session" => counts.emitted += 1,
            "hint_outcome" => match normalize(str_field(event, "outcome")).as_str() {
                "acted" => counts.followed += 1,
                "ignored" => counts.ignored += 1,
                _ => {}
            },
            _ if event_kind.starts_with("suppressed_") => counts.suppressed += 1,
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

pub struct DashboardHintCount {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

pub fn hint_summary_from_counts(counts: &[DashboardHintCount]) -> Value {
    let mut by_category: BTreeMap<String, HintCounts> = HINT_CATEGORIES
        .iter()
        .map(|category| ((*category).to_string(), HintCounts::default()))
        .collect();
    for row in counts {
        by_category.insert(
            row.category.clone(),
            HintCounts {
                emitted: row.emitted,
                followed: row.followed,
                ignored: row.ignored,
                suppressed: row.suppressed,
            },
        );
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

#[derive(Default)]
struct HintEfficacyCounts {
    emitted: i64,
    acted: i64,
    ignored: i64,
}

/// Per-category hint efficacy from durable `hint_emitted` + `hint_outcome`
/// events: how many hints were emitted, how many the model then acted on, how
/// many it ignored, and how many remain unresolved (emitted with no outcome yet
/// — the correlator's later-pass backlog). `unresolved` is derived so it stays
/// non-negative even if the event sample is truncated mid-pair.
fn hint_efficacy_from_events(events: &[Value]) -> Value {
    let mut by_category: BTreeMap<String, HintEfficacyCounts> = BTreeMap::new();
    let mut totals = HintEfficacyCounts::default();

    for event in events {
        let category = str_field(event, "hint_category");
        if category.is_empty() {
            continue;
        }
        let entry = by_category.entry(category.to_string()).or_default();
        match str_field(event, "event_kind") {
            "hint_emitted" => {
                entry.emitted += 1;
                totals.emitted += 1;
            }
            "hint_outcome" => match normalize(str_field(event, "outcome")).as_str() {
                "acted" => {
                    entry.acted += 1;
                    totals.acted += 1;
                }
                "ignored" => {
                    entry.ignored += 1;
                    totals.ignored += 1;
                }
                _ => {}
            },
            _ => {}
        }
    }

    let rows = by_category
        .into_iter()
        .map(|(category, counts)| {
            let unresolved = (counts.emitted - counts.acted - counts.ignored).max(0);
            json!({
                "category": category,
                "emitted": counts.emitted,
                "acted": counts.acted,
                "ignored": counts.ignored,
                "unresolved": unresolved,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "available": !rows.is_empty(),
        "source": "analytics_events",
        "totals": {
            "emitted": totals.emitted,
            "acted": totals.acted,
            "ignored": totals.ignored,
            "unresolved": (totals.emitted - totals.acted - totals.ignored).max(0),
        },
        "by_category": rows,
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
    let hook_analytics = read_hook_analytics_rows(state);
    diagnostics_summary_from_parts(message_count, &hook_analytics, durable_events)
}

pub fn diagnostics_summary_from_parts(
    message_count: i64,
    hook_analytics: &HookAnalyticsRows,
    durable_events: Option<&[Value]>,
) -> Value {
    let hook_rows = &hook_analytics.rows;
    let hook_call_count = hook_invocation_count(hook_rows);

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
            "hook_sources": hook_analytics.sources.clone(),
            "ratios": diagnostics_ratios(message_count, 0, 0, 0, hook_call_count),
            "by_event_kind": [],
            "by_tool": [],
            "by_mcp_tool": [],
            "by_tool_category": [],
            "by_outcome": [],
            "by_hook": hook_count_rows(hook_rows),
            "by_prompt_category": hook_prompt_category_rows(hook_rows),
            "hint_efficacy": json!({
                "available": false,
                "source": "analytics_events_unavailable",
                "totals": {"emitted": 0, "acted": 0, "ignored": 0, "unresolved": 0},
                "by_category": [],
            }),
            "recent_events": [],
            "recent_hooks": recent_hook_rows(hook_rows, 20),
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
        "hook_sources": hook_analytics.sources.clone(),
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
        "by_hook": hook_count_rows(hook_rows),
        "by_prompt_category": hook_prompt_category_rows(hook_rows),
        "hint_efficacy": hint_efficacy_from_events(events),
        "recent_events": recent_event_rows(events, 20),
        "recent_hooks": recent_hook_rows(hook_rows, 20),
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

pub struct HookAnalyticsRows {
    pub rows: Vec<Value>,
    pub sources: Vec<Value>,
}

/// Hooks write `hook_analytics.jsonl` into the project store when they can
/// resolve a project root and into the user-level profile root otherwise, so
/// a project's hook stream is split across both files. Read both, keeping
/// only user-level rows whose attribution places them inside this project.
fn read_hook_analytics_rows(state: &DashboardState) -> HookAnalyticsRows {
    read_hook_analytics_rows_at(Some(&state.store_root), Some(&state.project_root))
}

/// Path-based variant shared with the `tracedecay analytics` CLI. Passing no
/// `project_root` includes every user-level row instead of filtering.
pub fn read_hook_analytics_rows_at(
    store_root: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    let mut out = HookAnalyticsRows {
        rows: Vec::new(),
        sources: Vec::new(),
    };
    let store_path = store_root.map(|root| root.join("hook_analytics.jsonl"));
    if let Some(store_path) = &store_path {
        read_hook_analytics_file(store_path, None, &mut out);
    }
    if let Ok(profile_root) = crate::storage::default_profile_root() {
        let global_path = profile_root.join("hook_analytics.jsonl");
        if store_path.as_deref() != Some(global_path.as_path()) {
            read_hook_analytics_file(&global_path, project_root, &mut out);
        }
    }
    out.rows.sort_by_key(|row| {
        row.get("ts_unix_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    });
    out
}

fn read_hook_analytics_file(
    path: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut HookAnalyticsRows,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut rows_total = 0i64;
    let mut rows_included = 0i64;
    let mut rows_malformed = 0i64;
    let mut first_malformed_line = None;
    let mut first_malformed_error = None;
    for (index, line) in text.lines().enumerate() {
        let row = match serde_json::from_str::<Value>(line) {
            Ok(row) => row,
            Err(err) => {
                rows_malformed += 1;
                if first_malformed_line.is_none() {
                    first_malformed_line = Some(index + 1);
                    first_malformed_error = Some(err.to_string());
                }
                tracing::warn!(
                    hook_analytics_path = %path.display(),
                    line_number = index + 1,
                    error = %err,
                    "skipping malformed hook analytics jsonl row"
                );
                continue;
            }
        };
        rows_total += 1;
        let included = match project_filter {
            None => true,
            Some(root) => hook_row_matches_project(&row, root),
        };
        if included {
            rows_included += 1;
            out.rows.push(row);
        }
    }
    out.sources.push(json!({
        "path": path.display().to_string(),
        "rows_total": rows_total,
        "rows_included": rows_included,
        "rows_malformed": rows_malformed,
        "first_malformed_line": first_malformed_line,
        "first_malformed_error": first_malformed_error,
    }));
}

/// Rows written since project attribution landed carry `project_root` and/or
/// `event_cwd`; earlier user-level rows carry neither and stay unattributed.
fn hook_row_matches_project(row: &Value, project_root: &std::path::Path) -> bool {
    ["project_root", "event_cwd"].iter().any(|key| {
        row.get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| std::path::Path::new(value).starts_with(project_root))
    })
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        HookAnalyticsRows, hint_efficacy_from_events, hint_summary_from_events,
        read_hook_analytics_file,
    };

    #[test]
    fn hint_summary_counts_current_event_kinds_without_impossible_outcomes() {
        let events = vec![
            json!({"event_kind": "hint_emitted", "hint_category": "search", "outcome": "observed"}),
            json!({"event_kind": "hint_outcome", "hint_category": "search", "outcome": "acted"}),
            json!({"event_kind": "hint_emitted", "hint_category": "file_lookup", "outcome": "observed"}),
            json!({"event_kind": "hint_outcome", "hint_category": "file_lookup", "outcome": "ignored"}),
            json!({"event_kind": "hint_escalated", "hint_category": "impact", "outcome": "observed"}),
            json!({"event_kind": "suppressed_duplicate", "hint_category": "impact", "outcome": "observed"}),
        ];

        let summary = hint_summary_from_events(&events);
        let rows = summary["by_category"].as_array().unwrap();
        let row = |category: &str| {
            rows.iter()
                .find(|row| row["category"] == json!(category))
                .unwrap()
        };
        assert_eq!(row("search")["emitted"], json!(1));
        assert_eq!(row("search")["followed"], json!(1));
        assert_eq!(row("file_lookup")["emitted"], json!(1));
        assert_eq!(row("file_lookup")["ignored"], json!(1));
        assert_eq!(row("impact")["emitted"], json!(1));
        assert_eq!(row("impact")["suppressed"], json!(1));
    }

    #[test]
    fn hint_efficacy_counts_emitted_acted_ignored_and_unresolved() {
        let events = vec![
            json!({"event_kind": "hint_emitted", "hint_category": "search"}),
            json!({"event_kind": "hint_emitted", "hint_category": "search"}),
            json!({"event_kind": "hint_emitted", "hint_category": "search"}),
            json!({"event_kind": "hint_outcome", "hint_category": "search", "outcome": "acted"}),
            json!({"event_kind": "hint_outcome", "hint_category": "search", "outcome": "ignored"}),
            json!({"event_kind": "hint_emitted", "hint_category": "impact"}),
            // Unrelated events must not affect hint efficacy.
            json!({"event_kind": "mcp_tool_call", "tool_name": "tracedecay_context"}),
        ];

        let summary = hint_efficacy_from_events(&events);
        assert_eq!(summary["available"], json!(true));
        assert_eq!(summary["totals"]["emitted"], json!(4));
        assert_eq!(summary["totals"]["acted"], json!(1));
        assert_eq!(summary["totals"]["ignored"], json!(1));
        // 4 emitted - 1 acted - 1 ignored = 2 still unresolved.
        assert_eq!(summary["totals"]["unresolved"], json!(2));

        let by_category = summary["by_category"].as_array().unwrap();
        let search = by_category
            .iter()
            .find(|row| row["category"] == json!("search"))
            .unwrap();
        assert_eq!(search["emitted"], json!(3));
        assert_eq!(search["acted"], json!(1));
        assert_eq!(search["ignored"], json!(1));
        assert_eq!(search["unresolved"], json!(1));

        let impact = by_category
            .iter()
            .find(|row| row["category"] == json!("impact"))
            .unwrap();
        assert_eq!(impact["emitted"], json!(1));
        assert_eq!(impact["unresolved"], json!(1));
    }

    #[test]
    fn hint_efficacy_is_unavailable_without_hint_events() {
        let summary = hint_efficacy_from_events(&[json!({"event_kind": "mcp_tool_call"})]);
        assert_eq!(summary["available"], json!(false));
        assert!(summary["by_category"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hook_analytics_sources_report_malformed_jsonl_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        std::fs::write(
            store_root.join("hook_analytics.jsonl"),
            concat!(
                "{\"event\":\"hook_invoked\",\"ts_unix_ms\":1}\n",
                "{\"event\":\"hook_invoked\"\n",
                "{\"event\":\"hook_completed\",\"ts_unix_ms\":2}\n",
            ),
        )
        .unwrap();

        let mut rows = HookAnalyticsRows {
            rows: Vec::new(),
            sources: Vec::new(),
        };
        read_hook_analytics_file(&store_root.join("hook_analytics.jsonl"), None, &mut rows);

        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.sources.len(), 1);
        assert_eq!(rows.sources[0]["rows_total"], 2);
        assert_eq!(rows.sources[0]["rows_included"], 2);
        assert_eq!(rows.sources[0]["rows_malformed"], 1);
        assert_eq!(rows.sources[0]["first_malformed_line"], 2);
        assert!(
            rows.sources[0]["first_malformed_error"]
                .as_str()
                .is_some_and(|error| error.contains("EOF"))
        );
    }
}
