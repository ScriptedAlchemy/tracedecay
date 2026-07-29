//! Read-only durable analytics API for dashboard-level agent behavior.
//!
//! Durable `analytics_events` rows are preferred when available. Older session
//! stores still get session-message usage rollups, and hint lifecycle telemetry
//! falls back to the legacy `dashboard_hint_events` table when present.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::ObservatoryReadModelV1;
use tracedecay_domain::CoverageStateV1;

use crate::analytics::{
    ToolUsageObservation, UsageKind, categorize_skill, infer_usage_events,
    underused_tool_family_signals,
};
use crate::db::engine::params;
use crate::global_db::{
    AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts, RegisteredGlobalDb,
};

use super::DashboardState;
use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::util::{i64_field, query_i64, query_rows, str_field};

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

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct AnalyticsUsageCategoryV1 {
    kind: String,
    category: String,
    events: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct AnalyticsUsageSummaryV1 {
    available: bool,
    #[serde(default)]
    source: Option<String>,
    message_count: i64,
    #[serde(default)]
    event_count: Option<i64>,
    by_category: Vec<AnalyticsUsageCategoryV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AnalyticsOverviewPayloadV1 {
    available: bool,
    db: String,
    scope: String,
    hints: Value,
    usage: AnalyticsUsageSummaryV1,
    agents: Value,
    diagnostics: Value,
    underused_tool_families: Value,
    observatory: Option<ObservatoryReadModelV1>,
}

#[derive(Default)]
struct HintCounts {
    emitted: i64,
    followed: i64,
    ignored: i64,
    suppressed: i64,
}

/// `GET /api/plugins/analytics/overview`
pub(crate) async fn overview(State(state): State<DashboardState>) -> Response {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let observatory = Some(observatory_model(&state).await);
    let hints = hint_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await;
    let usage = match typed_usage_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await
    {
        Ok(usage) => usage,
        Err(response) => return response,
    };
    let agents = agent_usage_summary(state.lcm_db.as_deref()).await;
    let diagnostics = diagnostics_summary(&state, durable_events.as_deref()).await;
    let underused = underused_tool_families(state.lcm_db.as_deref()).await;

    Json(AnalyticsOverviewPayloadV1 {
        available: state.lcm_db.is_some() || durable_events.is_some(),
        db: state.lcm_db_path,
        scope: state.lcm_scope,
        hints,
        usage,
        agents,
        diagnostics,
        underused_tool_families: underused,
        observatory,
    })
    .into_response()
}

/// Canonical Plan 26 Observatory read model. CLI/MCP call the same application
/// composer instead of re-deriving these values in their adapters.
pub(crate) async fn observatory(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<ObservatoryReadModelV1>> {
    let model = observatory_model(&state).await;
    let known = model
        .metrics
        .iter()
        .filter(|metric| metric.coverage.state == CoverageStateV1::Known)
        .count() as u64;
    let eligible = model.metrics.len() as u64;
    let envelope = if model.current && known == eligible {
        DashboardEnvelopeV1::ready(
            scope_from_state(&state),
            DashboardCoverageV1::complete(eligible, "metrics"),
            model,
        )
    } else {
        DashboardEnvelopeV1::partial(
            scope_from_state(&state),
            eligible,
            known,
            "metrics",
            vec!["incomplete_metric_coverage".to_owned()],
            model,
        )
    };
    Json(envelope)
}

/// Transport-neutral HTTP representation over the same application model.
pub(crate) async fn observatory_http(State(state): State<DashboardState>) -> Response {
    let model = observatory_model(&state).await;
    match crate::application::observability::observatory_http_value(&model) {
        Ok(value) => Json(value).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Public JSON export. No dashboard projection or formula is applied.
pub(crate) async fn observatory_export(State(state): State<DashboardState>) -> Response {
    let model = observatory_model(&state).await;
    match crate::application::observability::observatory_export_bytes(&model) {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static(
                        "attachment; filename=\"tracedecay-observatory-v1.json\"",
                    ),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn observatory_model(state: &DashboardState) -> ObservatoryReadModelV1 {
    let scope_ref = RegisteredGlobalDb::canonical_project_key(&state.project_root);
    let since = crate::tracedecay::current_timestamp().saturating_sub(30 * 86_400);
    let mut read_model = match state.savings_db.as_deref() {
        Some(db) => {
            crate::application::observability::observatory_read_model(db, Some(&scope_ref), since)
                .await
        }
        None => crate::application::observability::observatory_unavailable_read_model(
            Some(&scope_ref),
            since,
            "observability_store_unavailable",
        ),
    };
    let feedback = match state.feedback_status_reader.as_ref() {
        Some(reader) => reader(state.project_root.clone()).await.ok(),
        None => None,
    };
    crate::application::observability::attach_feedback_system_quality(
        &mut read_model,
        feedback.as_ref(),
        Some("feedback_observations_unavailable"),
    );
    read_model
}

async fn agent_usage_summary(db: Option<&RegisteredGlobalDb>) -> Value {
    let Some(db) = db else {
        return json!({
            "available": false,
            "source": "session_store_unavailable",
            "by_agent": [],
        });
    };

    let rows = query_rows(
        db.read_connection(),
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
pub(crate) async fn hints(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(hint_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/usage`
pub(crate) async fn usage(State(state): State<DashboardState>) -> Response {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    match typed_usage_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await {
        Ok(usage) => Json(usage).into_response(),
        Err(response) => response,
    }
}

/// `GET /api/plugins/analytics/diagnostics`
pub(crate) async fn diagnostics(State(state): State<DashboardState>) -> Json<Value> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    Json(diagnostics_summary(&state, durable_events.as_deref()).await)
}

/// `GET /api/plugins/analytics/underused`
pub(crate) async fn underused(State(state): State<DashboardState>) -> Json<Value> {
    Json(json!({
        "available": state.lcm_db.is_some(),
        "db": state.lcm_db_path,
        "families": underused_tool_families(state.lcm_db.as_deref()).await,
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
        state.lcm_db.as_deref(),
        &RegisteredGlobalDb::canonical_project_key(&state.project_root),
    )
    .await
}

async fn durable_analytics_rows(
    global_db: Option<&RegisteredGlobalDb>,
    lcm_db: Option<&RegisteredGlobalDb>,
    project_id: &str,
) -> Option<Vec<Value>> {
    if let Some(db) = global_db
        && let Ok(events) = db
            .query_analytics_events(&AnalyticsEventQuery {
                provider: None,
                project_id: Some(project_id.to_string()),
                session_id: None,
                event_kind: None,
                since: None,
                until: None,
                before_id: None,
                limit: ANALYTICS_EVENT_LIMIT,
            })
            .await
        && !events.is_empty()
    {
        return Some(events.iter().map(durable_analytics_event_row).collect());
    }

    let rows = query_rows(
        lcm_db?.read_connection(),
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
        params![project_id],
    )
    .await
    .ok()?;
    if rows.is_empty() { None } else { Some(rows) }
}

pub(crate) fn durable_analytics_event_row(event: &AnalyticsEventRecord) -> Value {
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

pub(crate) fn hint_summary_from_events(events: &[Value]) -> Value {
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

pub(crate) fn hint_summary_from_counts(counts: &[AnalyticsHintCounts]) -> Value {
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

async fn hint_summary(db: Option<&RegisteredGlobalDb>, durable_events: Option<&[Value]>) -> Value {
    if let Some(events) = durable_events {
        return hint_summary_from_events(events);
    }

    let Some(db) = db else {
        return json!({
            "available": false,
            "source": "session_store_unavailable",
            "by_category": empty_hint_rows(),
        });
    };

    let has_table = query_i64(
        db.read_connection(),
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
        db.read_connection(),
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

async fn session_message_rows(db: Option<&RegisteredGlobalDb>) -> Option<Vec<Value>> {
    let db = db?;
    query_rows(
        db.read_connection(),
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

/// The contract form of the usage summary, shared by `GET .../usage` and the
/// `usage` member of the overview payload.
///
/// `usage_summary` builds two different literals — the unavailable branch omits
/// `source` and `event_count` rather than sending them null — so serving that
/// value raw would put a shape on the wire that the declared contract rejects.
/// Round-tripping through the struct is what makes the absent count arrive as an
/// explicit null, which is the distinction the readers depend on.
async fn typed_usage_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[Value]>,
) -> Result<AnalyticsUsageSummaryV1, Response> {
    let usage = usage_summary(db, durable_events).await;
    serde_json::from_value::<AnalyticsUsageSummaryV1>(usage).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "status": "contract_invalid",
                "error": format!("analytics usage summary did not match its contract: {error}"),
            })),
        )
            .into_response()
    })
}

async fn usage_summary(db: Option<&RegisteredGlobalDb>, durable_events: Option<&[Value]>) -> Value {
    if let Some(events) = durable_events {
        return usage_summary_from_events(events);
    }

    let Some(rows) = session_message_rows(db).await else {
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
    let message_count = session_message_rows(state.lcm_db.as_deref())
        .await
        .map_or(0, |rows| rows.len() as i64);
    let hook_analytics = read_hook_analytics_rows(state);
    diagnostics_summary_from_parts(message_count, &hook_analytics, durable_events)
}

pub(crate) fn diagnostics_summary_from_parts(
    message_count: i64,
    hook_analytics: &HookAnalyticsRows,
    durable_events: Option<&[Value]>,
) -> Value {
    let hook_rows = &hook_analytics.rows;
    let hook_call_count = hook_invocation_count(hook_rows);
    let hook_readiness = crate::hooks::aggregate_hook_completed_readiness(hook_rows);

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
            "hook_window": hook_analytics.window_payload(),
            "hook_readiness": hook_readiness,
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
        "hook_window": hook_analytics.window_payload(),
        "hook_readiness": hook_readiness,
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

/// Trailing rows read per `hook_analytics.jsonl` file.
///
/// The hook stream is append-only and unbounded: on an active profile the
/// project-level file reaches hundreds of megabytes and over a million rows,
/// and folding all of it per request cost ~14s. Diagnostics therefore reads a
/// bounded suffix of each file. Every figure derived from hook rows describes
/// that window, not all time, and the payload captions it under `hook_window`.
pub(crate) const HOOK_ANALYTICS_WINDOW_ROWS: usize = 10_000;

/// Suffix chunk size used when walking a hook analytics file backwards.
const HOOK_ANALYTICS_TAIL_CHUNK_BYTES: u64 = 1 << 20;

/// Window provenance for the hook rows folded into a diagnostics payload.
#[derive(Default)]
pub(crate) struct HookAnalyticsWindow {
    /// Per-file cap on trailing rows scanned.
    pub(crate) window_rows: usize,
    /// Rows actually scanned across every file in the window.
    pub(crate) rows_scanned: i64,
    /// True when at least one file was larger than its window, so the
    /// aggregates cover a recent suffix rather than the full history.
    pub(crate) truncated: bool,
}

pub(crate) struct HookAnalyticsRows {
    pub(crate) rows: Vec<Value>,
    pub(crate) sources: Vec<Value>,
    pub(crate) window: HookAnalyticsWindow,
}

impl HookAnalyticsRows {
    fn empty() -> Self {
        Self {
            rows: Vec::new(),
            sources: Vec::new(),
            window: HookAnalyticsWindow {
                window_rows: HOOK_ANALYTICS_WINDOW_ROWS,
                rows_scanned: 0,
                truncated: false,
            },
        }
    }

    /// Caption describing exactly which slice of the hook stream the sibling
    /// hook figures (`hook_call_count`, `by_hook`, `by_prompt_category`,
    /// `hook_readiness`, `recent_hooks`) were computed over.
    fn window_payload(&self) -> Value {
        let timestamps = || {
            self.rows
                .iter()
                .filter_map(|row| row.get("ts_unix_ms").and_then(Value::as_i64))
        };
        json!({
            "window_rows": self.window.window_rows as i64,
            "rows_scanned": self.window.rows_scanned,
            "rows_included": self.rows.len() as i64,
            "truncated": self.window.truncated,
            "total_rows_known": !self.window.truncated,
            "oldest_ts_unix_ms": timestamps().min(),
            "newest_ts_unix_ms": timestamps().max(),
        })
    }
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
///
/// Reads only the trailing [`HOOK_ANALYTICS_WINDOW_ROWS`] rows of each file;
/// see [`HookAnalyticsRows::window_payload`] for the caption callers must
/// surface alongside any derived figure.
pub(crate) fn read_hook_analytics_rows_at(
    store_root: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    let mut out = HookAnalyticsRows::empty();
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
    sort_hook_analytics_rows(&mut out.rows);
    out
}

fn sort_hook_analytics_rows(rows: &mut [Value]) {
    // `sort_by` is stable. Rows sort chronologically, then by durable event fields.
    // Exact-key ties (including rows with all fields missing) retain deterministic
    // ingestion order: project JSONL line order, followed by profile JSONL line order.
    rows.sort_by(|left, right| {
        hook_analytics_row_order_key(left).cmp(&hook_analytics_row_order_key(right))
    });
}

fn hook_analytics_row_order_key(row: &Value) -> (i64, &str, &str, &str) {
    (
        row.get("ts_unix_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        row.get("session_id").and_then(Value::as_str).unwrap_or(""),
        row.get("hook_name").and_then(Value::as_str).unwrap_or(""),
        row.get("agent").and_then(Value::as_str).unwrap_or(""),
    )
}

/// Read the last `window_rows` newline-delimited records of `path`.
///
/// Walks the file backwards in [`HOOK_ANALYTICS_TAIL_CHUNK_BYTES`] chunks so
/// cost tracks the window, not the file. Returns the lines oldest-first
/// alongside `reached_file_start`, which is false when the file held more rows
/// than the window and the result is therefore a suffix.
fn read_hook_analytics_tail(
    path: &std::path::Path,
    window_rows: usize,
) -> std::io::Result<(Vec<String>, bool)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let mut end = file.metadata()?.len();
    let mut buffer: Vec<u8> = Vec::new();
    let mut reached_file_start = true;
    let mut starts_at_line_boundary = true;

    while end > 0 {
        let chunk_len = HOOK_ANALYTICS_TAIL_CHUNK_BYTES.min(end);
        let start = end - chunk_len;
        let mut chunk = vec![0u8; usize::try_from(chunk_len).unwrap_or(usize::MAX)];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        end = start;

        // Every newline in the retained bytes terminates a record we hold in
        // full, so it is a lower bound on complete records available.
        if end > 0 && bytecount(&buffer, b'\n') >= window_rows {
            reached_file_start = false;
            file.seek(SeekFrom::Start(end.saturating_sub(1)))?;
            let mut preceding = [0_u8; 1];
            file.read_exact(&mut preceding)?;
            starts_at_line_boundary = preceding[0] == b'\n';
            break;
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<&str> = text.lines().collect();
    if !reached_file_start && !starts_at_line_boundary && !lines.is_empty() {
        // The first retained line began before the chunk boundary and is
        // truncated; drop it rather than reporting it as malformed.
        lines.remove(0);
    }
    if lines.len() > window_rows {
        reached_file_start = false;
        lines.drain(..lines.len() - window_rows);
    }

    Ok((
        lines.into_iter().map(str::to_string).collect(),
        reached_file_start,
    ))
}

fn bytecount(haystack: &[u8], needle: u8) -> usize {
    haystack.iter().filter(|byte| **byte == needle).count()
}

fn read_hook_analytics_file(
    path: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut HookAnalyticsRows,
) {
    let window_rows = out.window.window_rows;
    let Ok((lines, reached_file_start)) = read_hook_analytics_tail(path, window_rows) else {
        return;
    };
    // `rows_scanned` counts every line in the window; `rows_total` keeps its
    // original meaning of well-formed rows, so malformed lines stay visible as
    // the difference between the two rather than inflating the parsed count.
    let rows_scanned = lines.len() as i64;
    let mut rows_total = 0i64;
    let mut rows_included = 0i64;
    let mut rows_malformed = 0i64;
    let mut first_malformed_offset = None;
    let mut first_malformed_error = None;
    for (index, line) in lines.iter().enumerate() {
        let row = match serde_json::from_str::<Value>(line) {
            Ok(row) => row,
            Err(err) => {
                rows_malformed += 1;
                if first_malformed_offset.is_none() {
                    first_malformed_offset = Some(index + 1);
                    first_malformed_error = Some(err.to_string());
                }
                tracing::warn!(
                    hook_analytics_path = %path.display(),
                    window_line_number = index + 1,
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
    out.window.rows_scanned += rows_scanned;
    out.window.truncated |= !reached_file_start;
    out.sources.push(json!({
        "path": path.display().to_string(),
        // Counts describe the trailing window only. `window_truncated` is true
        // when the file extends past it, so `rows_total` is not the file total.
        "rows_scanned": rows_scanned,
        "rows_total": rows_total,
        "rows_included": rows_included,
        "rows_malformed": rows_malformed,
        "window_rows": window_rows as i64,
        "window_truncated": !reached_file_start,
        // Line numbers are relative to the window, not the file, when truncated.
        "first_malformed_line": first_malformed_offset,
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

async fn underused_tool_families(db: Option<&RegisteredGlobalDb>) -> Value {
    let Some(rows) = session_message_rows(db).await else {
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
    use serde_json::{Value, json};

    use super::{
        HOOK_ANALYTICS_WINDOW_ROWS, HookAnalyticsRows, diagnostics_summary_from_parts,
        hint_efficacy_from_events, hint_summary_from_events, read_hook_analytics_file,
        recent_hook_rows, sort_hook_analytics_rows,
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
    fn hook_analytics_row_order_is_stable_on_timestamp_ties() {
        let mut rows = vec![
            json!({"source_marker": "project-missing"}),
            json!({"source_marker": "profile-missing"}),
            json!({
                "ts_unix_ms": 10,
                "session_id": "b",
                "hook_name": "post",
                "agent": "claude"
            }),
            json!({
                "ts_unix_ms": 10,
                "session_id": "a",
                "hook_name": "post",
                "agent": "claude"
            }),
            json!({
                "ts_unix_ms": 9,
                "session_id": "z",
                "hook_name": "pre",
                "agent": "codex"
            }),
            json!({
                "ts_unix_ms": 10,
                "session_id": "a",
                "hook_name": "pre",
                "agent": "claude"
            }),
            json!({
                "ts_unix_ms": 10,
                "session_id": "a",
                "hook_name": "post",
                "agent": "claude",
                "source_marker": "project-exact-tie"
            }),
            json!({
                "ts_unix_ms": 10,
                "session_id": "a",
                "hook_name": "post",
                "agent": "claude",
                "source_marker": "profile-exact-tie"
            }),
        ];
        sort_hook_analytics_rows(&mut rows);
        assert_eq!(rows[0]["source_marker"], json!("project-missing"));
        assert_eq!(rows[1]["source_marker"], json!("profile-missing"));
        assert_eq!(rows[2]["ts_unix_ms"], json!(9));
        assert_eq!(rows[3]["session_id"], json!("a"));
        assert_eq!(rows[3]["hook_name"], json!("post"));
        assert_eq!(rows[4]["source_marker"], json!("project-exact-tie"));
        assert_eq!(rows[5]["source_marker"], json!("profile-exact-tie"));
        assert_eq!(rows[6]["session_id"], json!("a"));
        assert_eq!(rows[6]["hook_name"], json!("pre"));
        assert_eq!(rows[7]["session_id"], json!("b"));
    }

    #[test]
    fn recent_hook_rows_remain_newest_first_after_global_sort() {
        let mut rows = vec![
            json!({"event": "hook_invoked", "ts_unix_ms": 10, "session_id": "a"}),
            json!({"event": "hook_invoked", "ts_unix_ms": 12, "session_id": "c"}),
            json!({"event": "hook_invoked", "ts_unix_ms": 11, "session_id": "b"}),
        ];
        sort_hook_analytics_rows(&mut rows);

        let recent = recent_hook_rows(&rows, 2);
        assert_eq!(recent[0]["ts_unix_ms"], json!(12));
        assert_eq!(recent[0]["session_id"], json!("c"));
        assert_eq!(recent[1]["ts_unix_ms"], json!(11));
        assert_eq!(recent[1]["session_id"], json!("b"));
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

        let mut rows = HookAnalyticsRows::empty();
        read_hook_analytics_file(&store_root.join("hook_analytics.jsonl"), None, &mut rows);

        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.sources.len(), 1);
        assert_eq!(rows.sources[0]["rows_scanned"], 3);
        assert_eq!(rows.sources[0]["rows_total"], 2);
        assert_eq!(rows.sources[0]["rows_included"], 2);
        assert_eq!(rows.sources[0]["rows_malformed"], 1);
        assert_eq!(rows.sources[0]["first_malformed_line"], 2);
        assert_eq!(rows.sources[0]["window_truncated"], json!(false));
        assert!(
            rows.sources[0]["first_malformed_error"]
                .as_str()
                .is_some_and(|error| error.contains("EOF"))
        );
    }

    /// Writes `count` chronologically ordered hook rows, each padded so the
    /// file spans many tail chunks.
    fn write_hook_analytics_fixture(path: &std::path::Path, count: usize) {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        for index in 0..count {
            let row = json!({
                "event": "hook_invoked",
                "hook_name": "PostToolUse",
                "session_id": format!("session-{index:06}"),
                "ts_unix_ms": 1_000_000 + index as i64,
                "padding": "x".repeat(400),
            });
            writeln!(file, "{row}").unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn hook_analytics_tail_keeps_newest_rows_within_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook_analytics.jsonl");
        write_hook_analytics_fixture(&path, 10_000);

        let mut rows = HookAnalyticsRows::empty();
        rows.window.window_rows = 250;
        read_hook_analytics_file(&path, None, &mut rows);

        assert_eq!(rows.rows.len(), 250);
        // The window is the newest suffix, and no row is truncated mid-line.
        assert_eq!(rows.rows[0]["session_id"], json!("session-009750"));
        assert_eq!(rows.rows[249]["session_id"], json!("session-009999"));
        assert_eq!(rows.sources[0]["rows_malformed"], 0);
        assert_eq!(rows.sources[0]["window_truncated"], json!(true));
        assert_eq!(rows.sources[0]["window_rows"], json!(250));
    }

    #[test]
    fn hook_analytics_tail_reads_whole_file_when_under_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook_analytics.jsonl");
        write_hook_analytics_fixture(&path, 40);

        let mut rows = HookAnalyticsRows::empty();
        read_hook_analytics_file(&path, None, &mut rows);

        assert_eq!(rows.rows.len(), 40);
        assert_eq!(rows.rows[0]["session_id"], json!("session-000000"));
        assert_eq!(rows.sources[0]["window_truncated"], json!(false));
        assert!(!rows.window.truncated);
    }

    #[test]
    fn hook_analytics_tail_preserves_record_at_exact_chunk_boundary() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook_analytics.jsonl");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for index in 0..2_048 {
            let base = json!({
                "event": "hook_invoked",
                "session_id": format!("session-{index:06}"),
                "padding": "",
            })
            .to_string();
            let padding = 1_023_usize.checked_sub(base.len()).unwrap();
            let line = json!({
                "event": "hook_invoked",
                "session_id": format!("session-{index:06}"),
                "padding": "x".repeat(padding),
            })
            .to_string();
            assert_eq!(line.len(), 1_023);
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();

        let mut rows = HookAnalyticsRows::empty();
        rows.window.window_rows = 1_024;
        read_hook_analytics_file(&path, None, &mut rows);

        assert_eq!(rows.rows.len(), 1_024);
        assert_eq!(rows.rows[0]["session_id"], json!("session-001024"));
        assert_eq!(rows.rows[1_023]["session_id"], json!("session-002047"));
    }

    #[test]
    fn diagnostics_summary_captions_the_hook_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook_analytics.jsonl");
        write_hook_analytics_fixture(&path, 5_000);

        let mut hook_analytics = HookAnalyticsRows::empty();
        hook_analytics.window.window_rows = 100;
        read_hook_analytics_file(&path, None, &mut hook_analytics);
        sort_hook_analytics_rows(&mut hook_analytics.rows);

        let summary = diagnostics_summary_from_parts(0, &hook_analytics, None);
        let window = &summary["hook_window"];
        assert_eq!(window["window_rows"], json!(100));
        assert_eq!(window["rows_scanned"], json!(100));
        assert_eq!(window["rows_included"], json!(100));
        assert_eq!(window["truncated"], json!(true));
        // The frontend must not print these as all-time figures.
        assert_eq!(window["total_rows_known"], json!(false));
        assert_eq!(window["oldest_ts_unix_ms"], json!(1_004_900));
        assert_eq!(window["newest_ts_unix_ms"], json!(1_004_999));
        assert_eq!(summary["hook_call_count"], json!(100));
    }

    /// Bounded-fold regression guard against a real, unbounded hook stream.
    ///
    /// Opt in by pointing `TRACEDECAY_BENCH_HOOK_ANALYTICS_STORE` at a store
    /// root holding `hook_analytics.jsonl`; this reproduces the diagnostics
    /// handler's whole read (project store file plus the profile file). The
    /// test is a no-op otherwise so CI stays hermetic.
    #[test]
    fn hook_analytics_read_is_bounded_on_real_stores() {
        let store_root = match std::env::var_os("TRACEDECAY_BENCH_HOOK_ANALYTICS_STORE") {
            Some(path) => std::path::PathBuf::from(path),
            None => return,
        };

        let started = std::time::Instant::now();
        let rows = super::read_hook_analytics_rows_at(Some(&store_root), None);
        let summary = diagnostics_summary_from_parts(0, &rows, None);
        let elapsed = started.elapsed();

        println!(
            "bounded hook analytics read: {} rows in {elapsed:?}\n  window={}\n  sources={}",
            rows.rows.len(),
            summary["hook_window"],
            Value::Array(rows.sources.clone()),
        );
        // One window per file read.
        assert!(rows.rows.len() <= HOOK_ANALYTICS_WINDOW_ROWS * rows.sources.len().max(1));
        assert!(summary["hook_window"]["window_rows"].is_number());
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "bounded read took {elapsed:?}, expected <500ms"
        );
    }

    #[test]
    fn diagnostics_summary_aggregates_real_hook_completed_rows_safely() {
        let hook_analytics = HookAnalyticsRows {
            rows: vec![json!({
                "event": "hook_completed",
                "agent": "untrusted-host",
                "hook_name": "privateHookName",
                "hook_wall_time_us": 0,
                "daemon_rtt_us": null,
                "payload_bytes": 0,
                "daemon_ipc_payload_bytes": null,
                "timeout": {"budget_ms": null, "timed_out": null},
                "disposition": {
                    "class": "untrusted-class",
                    "status": "untrusted-status",
                    "retryable": null,
                    "reason_code": "private-reason"
                }
            })],
            sources: Vec::new(),
            window: HookAnalyticsWindow::default(),
        };

        let summary = diagnostics_summary_from_parts(0, &hook_analytics, None);
        assert_eq!(summary["hook_readiness"]["collection_status"], "measured");
        assert_eq!(summary["hook_readiness"]["events_considered"], 1);
        assert_eq!(
            summary["hook_readiness"]["hook_wall_time_distribution"][0]["host"],
            "other"
        );
        assert_eq!(
            summary["hook_readiness"]["hook_wall_time_distribution"][0]["summary"]["min"],
            0
        );
        assert_eq!(
            summary["hook_readiness"]["host_ipc_rtt_distribution"][0]["summary"]["availability"],
            "no_samples"
        );
        assert_eq!(
            summary["hook_readiness"]["disposition_counts_by_host"][0]["class"],
            "unknown"
        );

        let encoded = serde_json::to_string(&summary["hook_readiness"]).unwrap();
        for forbidden in [
            "untrusted-host",
            "privateHookName",
            "private-reason",
            "hook_name",
            "reason_code",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
