//! Read-only durable analytics API for dashboard-level agent behavior.
//!
//! Durable `analytics_events` rows are preferred when available. Older session
//! stores still get session-message usage rollups, and hint lifecycle telemetry
//! falls back to the legacy `dashboard_hint_events` table when present.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::Json;
use serde_json::{json, Value};

use crate::analytics::{categorize_skill, infer_usage_events, UsageKind};
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
struct FamilyCounts {
    relevant_events: i64,
    usage_events: i64,
}

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
    let underused = underused_tool_families(state.lcm_conn.as_ref()).await;

    Json(json!({
        "available": state.lcm_conn.is_some() || durable_events.is_some(),
        "db": state.lcm_db_path,
        "scope": state.lcm_scope,
        "hints": hints,
        "usage": usage,
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
        "SELECT event_kind, tool_name, tool_category, skill_name,
                hint_category, outcome, metadata_json
         FROM (
             SELECT event_kind, tool_name, tool_category, skill_name,
                    hint_category, outcome, metadata_json, timestamp, id
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
        "event_kind": &event.event_kind,
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

async fn underused_tool_families(conn: Option<&libsql::Connection>) -> Value {
    let Some(rows) = session_message_rows(conn).await else {
        return Value::Array(Vec::new());
    };

    let mut families: BTreeMap<String, FamilyCounts> = [
        ("code_context".to_string(), FamilyCounts::default()),
        ("code_search".to_string(), FamilyCounts::default()),
        ("call_graph".to_string(), FamilyCounts::default()),
        ("impact_analysis".to_string(), FamilyCounts::default()),
    ]
    .into_iter()
    .collect();

    for row in &rows {
        let text = str_field(row, "text");
        for event in infer_usage_events(
            Some(str_field(row, "tool_names")),
            Some(str_field(row, "metadata_json")),
            Some(text),
        ) {
            if event.kind == UsageKind::Tool {
                record_tool_family(&mut families, &event.name, text);
            }
        }
    }

    Value::Array(
        families
            .into_iter()
            .map(|(family, counts)| {
                let missed = counts.relevant_events.saturating_sub(counts.usage_events);
                json!({
                    "family": family,
                    "relevant_events": counts.relevant_events,
                    "usage_events": counts.usage_events,
                    "missed_events": missed,
                    "underused": missed > 0,
                })
            })
            .collect(),
    )
}

fn record_tool_family(families: &mut BTreeMap<String, FamilyCounts>, tool: &str, text: &str) {
    let normalized = normalize(tool);
    let text = text.to_ascii_lowercase();
    if normalized.contains("tracedecay_context")
        || normalized.contains("tracedecay_node")
        || normalized.contains("tracedecay_files")
    {
        increment_family_usage(families, "code_context");
    }
    if normalized.contains("tracedecay_search") || normalized.contains("find_exact_symbol") {
        increment_family_usage(families, "code_search");
    }
    if normalized.contains("tracedecay_call") || normalized.contains("tracedecay_graph") {
        increment_family_usage(families, "call_graph");
    }
    if normalized.contains("tracedecay_impact") || normalized.contains("tracedecay_affected") {
        increment_family_usage(families, "impact_analysis");
    }

    if normalized == "read" || normalized == "cat" || normalized == "sed" {
        increment_family_relevance(families, "code_context");
    }
    if matches!(normalized.as_str(), "grep" | "rg" | "glob" | "search")
        || (matches!(normalized.as_str(), "bash" | "shell" | "exec_command")
            && (text.contains(" rg ") || text.contains("grep") || text.contains("find ")))
    {
        increment_family_relevance(families, "code_search");
    }
}

fn increment_family_usage(families: &mut BTreeMap<String, FamilyCounts>, family: &str) {
    families.entry(family.to_string()).or_default().usage_events += 1;
}

fn increment_family_relevance(families: &mut BTreeMap<String, FamilyCounts>, family: &str) {
    families
        .entry(family.to_string())
        .or_default()
        .relevant_events += 1;
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}
