//! Read-only durable analytics API for dashboard-level agent behavior.
//!
//! Durable `analytics_events` rows are preferred when available. Older session
//! stores still get session-message usage rollups, and hint lifecycle telemetry
//! falls back to the legacy `dashboard_hint_events` table when present.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::ObservatoryReadModelV1;
use tracedecay_domain::CoverageStateV1;

use crate::analytics::{
    ToolUsageObservation, UsageKind, categorize_skill, infer_usage_events,
    underused_tool_family_signals,
};
use tracedecay_global_db::{
    AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts, RegisteredGlobalDb,
};
use tracedecay_runtime_core::db::engine::params;

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
pub struct AnalyticsUsageCategoryV1 {
    pub kind: String,
    pub category: String,
    pub events: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsUsageSummaryV1 {
    pub available: bool,
    #[serde(default)]
    pub source: Option<String>,
    pub message_count: i64,
    #[serde(default)]
    pub event_count: Option<i64>,
    pub by_category: Vec<AnalyticsUsageCategoryV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintCategoryV1 {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintsPayloadV1 {
    pub available: bool,
    pub source: String,
    #[serde(default)]
    pub error: Option<String>,
    pub by_category: Vec<AnalyticsHintCategoryV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsAgentUsageV1 {
    pub agent: String,
    pub sessions: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsAgentsPayloadV1 {
    pub available: bool,
    pub source: String,
    pub by_agent: Vec<AnalyticsAgentUsageV1>,
}

/// How a session is attached to the delegation tree above it.
///
/// The distinction is load-bearing. A session with no parent and a session
/// whose parent the store does not hold both draw at the left margin, but only
/// the first one is actually a root: the second is a tree whose top was never
/// ingested, and captioning it as a root would assert a delegation boundary
/// that was never observed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsSubagentLinkV1 {
    /// The session records no parent — a genuine top of a delegation tree.
    Root,
    /// The parent named by the session is present in this reading.
    Linked,
    /// The session names a parent the session store does not hold, so its
    /// depth is measured from a cut edge rather than from a real root.
    MissingParent,
    /// The parent chain closes on itself. Never reachable from a root, so it
    /// is surfaced at the margin rather than silently dropped.
    Cycle,
}

/// One session in the subagent delegation tree.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsSubagentNodeV1 {
    pub provider: String,
    pub session_id: String,
    pub parent_session_id: Option<String>,
    /// The managed-agent label when the session carries one, else the raw
    /// `agent_id`. `None` is an unlabeled session, not an unnamed agent.
    pub agent: Option<String>,
    pub title: Option<String>,
    /// Unix SECONDS, as the session store records them — capture parses
    /// provider stamps with `parse_rfc3339_timestamp`, which yields seconds,
    /// and normalizes millisecond inputs down to seconds before storing. This
    /// is not the microsecond convention the Work contracts use, and reading
    /// it as micros would place every session in 1970.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub is_subagent: bool,
    /// The tool invocation that spawned this session, when the provider
    /// recorded one. It is what makes a delegation edge attributable to a
    /// specific call rather than merely to a parent.
    pub parent_tool_use_id: Option<String>,
    /// Distance from this node's tree top. Roots are 0.
    pub depth: i64,
    /// Sessions below this one, transitively, excluding itself.
    pub descendants: i64,
    pub link: AnalyticsSubagentLinkV1,
}

/// The subagent tree: parent/child session edges, not a per-agent rollup.
///
/// `nodes` is a pre-order flattening — every node appears after its own parent
/// and before that parent's later siblings — so a reader can draw the tree from
/// `depth` alone without reassembling edges client-side.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsSubagentTreePayloadV1 {
    pub available: bool,
    pub source: String,
    #[serde(default)]
    pub error: Option<String>,
    pub nodes: Vec<AnalyticsSubagentNodeV1>,
    /// Sessions read for this project before any tree was built. The only
    /// honest denominator for the counts below.
    pub sessions_read: i64,
    /// Nodes whose `link` is `root`.
    pub root_count: i64,
    /// Parent/child edges actually resolved within this reading.
    pub edge_count: i64,
    /// Deepest `depth` present, so a caption can state the tree's reach
    /// instead of implying one from the drawn rows.
    pub max_depth: i64,
    /// Sessions naming a parent this reading does not hold.
    pub missing_parent_count: i64,
    /// Sessions whose parent chain closes on itself.
    pub cycle_count: i64,
    /// True when the scan ceiling was reached, so edges may be cut and the
    /// counts above describe a prefix of the store rather than all of it.
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsUnderusedFamilyV1 {
    pub family: String,
    pub relevant_events: i64,
    pub usage_events: i64,
    pub missed_events: i64,
    pub underused: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsUnderusedPayloadV1 {
    pub available: bool,
    pub db: String,
    pub families: Vec<AnalyticsUnderusedFamilyV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsDiagnosticsRatiosV1 {
    pub events_per_message: f64,
    pub tool_calls_per_message: f64,
    pub mcp_tool_calls_per_message: f64,
    pub hook_calls_per_message: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsEventKindCountV1 {
    pub event_kind: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsToolCountV1 {
    pub tool_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsOutcomeCountV1 {
    pub outcome: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHookWindowV1 {
    pub window_rows: i64,
    pub rows_scanned: i64,
    pub rows_included: i64,
    pub truncated: bool,
    pub total_rows_known: bool,
    pub oldest_ts_unix_ms: Option<i64>,
    pub newest_ts_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsRecentEventV1 {
    pub timestamp: Option<i64>,
    pub event_kind: String,
    pub hook_name: String,
    pub tool_name: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsToolCategoryCountV1 {
    pub tool_category: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHookNameCountV1 {
    pub hook_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsPromptCategoryCountV1 {
    pub prompt_category: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsRecentHookV1 {
    pub ts_unix_ms: Option<i64>,
    pub agent: String,
    pub hook_name: String,
    pub session_id: String,
    pub tool_name: String,
    pub prompt_category: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyTotalsV1 {
    pub emitted: i64,
    pub acted: i64,
    pub ignored: i64,
    pub unresolved: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyCategoryV1 {
    pub category: String,
    pub emitted: i64,
    pub acted: i64,
    pub ignored: i64,
    pub unresolved: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyV1 {
    pub available: bool,
    pub source: String,
    pub totals: AnalyticsHintEfficacyTotalsV1,
    pub by_category: Vec<AnalyticsHintEfficacyCategoryV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsDiagnosticsPayloadV1 {
    pub available: bool,
    pub source: String,
    pub message_count: i64,
    pub event_count: i64,
    pub tool_call_count: i64,
    pub mcp_tool_call_count: i64,
    pub tracedecay_call_count: i64,
    pub hook_call_count: i64,
    /// Provenance rows for the hook-analytics source files backing the
    /// hook rollups. Free-form provenance, not a versioned sub-contract.
    pub hook_sources: Vec<Value>,
    /// Daemon-owned hook readiness projection. The projection stamps its own
    /// `schema_version`; the diagnostics read carries it verbatim.
    pub hook_readiness: Value,
    #[serde(default)]
    pub events_per_hour: Option<f64>,
    pub ratios: AnalyticsDiagnosticsRatiosV1,
    pub by_event_kind: Vec<AnalyticsEventKindCountV1>,
    pub by_tool: Vec<AnalyticsToolCountV1>,
    pub by_mcp_tool: Vec<AnalyticsToolCountV1>,
    pub by_tool_category: Vec<AnalyticsToolCategoryCountV1>,
    pub by_outcome: Vec<AnalyticsOutcomeCountV1>,
    pub by_hook: Vec<AnalyticsHookNameCountV1>,
    pub by_prompt_category: Vec<AnalyticsPromptCategoryCountV1>,
    pub hint_efficacy: AnalyticsHintEfficacyV1,
    pub hook_window: AnalyticsHookWindowV1,
    pub recent_events: Vec<AnalyticsRecentEventV1>,
    pub recent_hooks: Vec<AnalyticsRecentHookV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AnalyticsOverviewPayloadV1 {
    available: bool,
    db: String,
    scope: String,
    hints: AnalyticsHintsPayloadV1,
    usage: AnalyticsUsageSummaryV1,
    agents: AnalyticsAgentsPayloadV1,
    diagnostics: AnalyticsDiagnosticsPayloadV1,
    underused_tool_families: Vec<AnalyticsUnderusedFamilyV1>,
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
pub async fn overview(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsOverviewPayloadV1>>> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let observatory = Some(observatory_model(&state).await);
    let hints = match typed_hint_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await {
        Ok(hints) => hints,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    let usage = match typed_usage_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await
    {
        Ok(usage) => usage,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    let agents = match typed_agent_usage_summary(state.lcm_db.as_deref()).await {
        Ok(agents) => agents,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    let diagnostics = match typed_diagnostics_summary(&state, durable_events.as_deref()).await {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    let underused = match underused_tool_families(state.lcm_db.as_deref()).await {
        Ok(Some(families)) => families,
        Ok(None) => Vec::new(),
        Err(error) => {
            return Json(DashboardEnvelopeV1::unavailable(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };

    let payload = AnalyticsOverviewPayloadV1 {
        available: state.lcm_db.is_some() || durable_events.is_some(),
        db: state.lcm_db_path.clone(),
        scope: state.lcm_scope.clone(),
        hints,
        usage,
        agents,
        diagnostics,
        underused_tool_families: underused,
        observatory,
    };
    if payload.available {
        Json(DashboardEnvelopeV1::ready(
            scope_from_state(&state),
            DashboardCoverageV1::unknown(),
            Some(payload),
        ))
    } else {
        Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_sources_unavailable",
        ))
    }
}

/// Canonical Plan 26 Observatory read model. CLI/MCP call the same application
/// composer instead of re-deriving these values in their adapters.
pub async fn observatory(
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

// `observatory_http` / `observatory_export` are deleted with their last caller.
// They mounted `/api/plugins/analytics/observatory{,/export}`, which served the
// same application model as `/api/observatory` — the route the Observatory
// workspace actually reads (`CanonicalObservations.tsx`) — one without the
// envelope and one with a download disposition. No dashboard, SDK, CLI, or MCP
// caller ever bound to either; the only reader was a parity test asserting the
// aliases agreed with the canonical route, which is a test of the duplication
// rather than of any behavior a consumer depends on.

async fn observatory_model(state: &DashboardState) -> ObservatoryReadModelV1 {
    let scope_ref = RegisteredGlobalDb::canonical_project_key(&state.project_root);
    let since =
        tracedecay_runtime_core::tracedecay::current_timestamp().saturating_sub(30 * 86_400);
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

async fn agent_usage_summary(db: Option<&RegisteredGlobalDb>) -> Result<Value, String> {
    let Some(db) = db else {
        return Ok(json!({
            "available": false,
            "source": "session_store_unavailable",
            "by_agent": [],
        }));
    };

    let connection = db.read_connection();
    let rows = query_rows(
        &connection,
        "SELECT COALESCE(agent_id, '') AS agent_id,
                COALESCE(metadata_json, '') AS metadata_json
         FROM sessions
         WHERE is_subagent = 1
           AND (COALESCE(agent_id, '') <> '' OR COALESCE(metadata_json, '') <> '')
         ORDER BY agent_id",
        (),
    )
    .await
    .map_err(|error| format!("analytics agent usage query failed: {error}"))?;

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

    Ok(json!({
        "available": true,
        "source": "sessions",
        "by_agent": by_agent.into_iter().map(|(agent, sessions)| {
            json!({
                "agent": agent,
                "sessions": sessions,
            })
        }).collect::<Vec<_>>(),
    }))
}

async fn typed_agent_usage_summary(
    db: Option<&RegisteredGlobalDb>,
) -> Result<AnalyticsAgentsPayloadV1, String> {
    decode_analytics_contract(agent_usage_summary(db).await?, "analytics agent usage")
}

fn managed_agent_label_for_session(agent_id: &str, metadata_json: &str) -> Option<&'static str> {
    tracedecay_agent_hosts::automation::agent_targets::managed_agent_label(agent_id).or_else(|| {
        let metadata: Value = serde_json::from_str(metadata_json).ok()?;
        ["agent_nickname", "agent_role"]
            .into_iter()
            .filter_map(|key| metadata.get(key).and_then(Value::as_str))
            .find_map(tracedecay_agent_hosts::automation::agent_targets::managed_agent_label)
    })
}

/// `GET /api/plugins/analytics/agents` — sessions per managed subagent,
/// straight from the session store. The same summary the overview embeds,
/// exposed on its own so the Agents workspace can read subagent context
/// without paying for the full hook-analytics fold.
pub async fn agents(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsAgentsPayloadV1>>> {
    match typed_agent_usage_summary(state.lcm_db.as_deref()).await {
        Ok(payload) if !payload.available => Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_agents_source_unavailable",
        )),
        Ok(payload) => {
            let count = payload.by_agent.len() as u64;
            Json(DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                DashboardCoverageV1::complete(count, "managed_agents"),
                Some(payload),
            ))
        }
        Err(error) => Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            error,
        )),
    }
}

/// Ceiling on sessions read for one subagent-tree answer. A dashboard tree is
/// drawn, not paged, and an unbounded store would be neither drawable nor
/// affordable. Reaching it is reported rather than hidden.
const SUBAGENT_TREE_SESSION_CEILING: i64 = 2_000;

/// One session row as the tree builder needs it, before any edge is resolved.
struct SubagentSessionRow {
    provider: String,
    session_id: String,
    parent_session_id: Option<String>,
    agent: Option<String>,
    title: Option<String>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    is_subagent: bool,
    parent_tool_use_id: Option<String>,
}

fn optional_text(row: &Value, key: &str) -> Option<String> {
    let value = str_field(row, key).trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Assemble parent/child session edges into a pre-order tree.
///
/// Every input row appears in the output exactly once. Sessions reachable from
/// a top are emitted under it; sessions that are not reachable from any top —
/// which can only happen when their parent chain closes on itself — are emitted
/// afterwards as their own tops, marked `Cycle`, because dropping them would
/// silently shrink a delegation count the caller is about to read.
fn build_subagent_tree(rows: Vec<SubagentSessionRow>) -> Vec<AnalyticsSubagentNodeV1> {
    // Sessions are keyed by (provider, session_id); `parent_session_id` carries
    // no provider of its own, so an edge is only resolved inside one provider.
    // Joining across providers would invent delegations between unrelated hosts
    // that happen to have minted the same session id.
    let index: BTreeMap<(&str, &str), usize> = rows
        .iter()
        .enumerate()
        .map(|(position, row)| ((row.provider.as_str(), row.session_id.as_str()), position))
        .collect();

    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut link = vec![AnalyticsSubagentLinkV1::Root; rows.len()];
    for (position, row) in rows.iter().enumerate() {
        let Some(parent_id) = row.parent_session_id.as_deref() else {
            continue;
        };
        match index.get(&(row.provider.as_str(), parent_id)) {
            // A session naming itself as its parent is a one-node cycle; it can
            // never be reached from a top, so it must not be filed as an edge.
            Some(&parent) if parent != position => {
                children.entry(parent).or_default().push(position);
                link[position] = AnalyticsSubagentLinkV1::Linked;
            }
            Some(_) => link[position] = AnalyticsSubagentLinkV1::Cycle,
            None => link[position] = AnalyticsSubagentLinkV1::MissingParent,
        }
    }

    // Stable sibling order: when a session started is the reading's own
    // ordering claim, and the id breaks ties so two reads never disagree.
    let order_key = |position: usize| {
        let row = &rows[position];
        (
            row.started_at.unwrap_or(i64::MAX),
            row.session_id.clone(),
            row.provider.clone(),
        )
    };
    for bucket in children.values_mut() {
        bucket.sort_by_key(|&position| order_key(position));
    }

    let mut tops: Vec<usize> = (0..rows.len())
        .filter(|&position| {
            matches!(
                link[position],
                AnalyticsSubagentLinkV1::Root | AnalyticsSubagentLinkV1::MissingParent
            )
        })
        .collect();
    tops.sort_by_key(|&position| order_key(position));

    let mut visited = vec![false; rows.len()];
    let mut preorder: Vec<(usize, i64)> = Vec::with_capacity(rows.len());
    let walk = |top: usize, visited: &mut Vec<bool>, preorder: &mut Vec<(usize, i64)>| {
        if visited[top] {
            return;
        }
        // Explicit stack, not recursion: a delegation chain is data, and a
        // deep one must not be able to overflow the daemon's stack.
        let mut stack = vec![(top, 0i64)];
        while let Some((position, depth)) = stack.pop() {
            if visited[position] {
                continue;
            }
            visited[position] = true;
            preorder.push((position, depth));
            if let Some(bucket) = children.get(&position) {
                for &child in bucket.iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
    };
    for &top in &tops {
        walk(top, &mut visited, &mut preorder);
    }
    // Anything still unvisited sits on a parent cycle. Surfaced at the margin.
    let stranded: Vec<usize> = (0..rows.len())
        .filter(|&position| !visited[position])
        .collect();
    for position in stranded {
        link[position] = AnalyticsSubagentLinkV1::Cycle;
        walk(position, &mut visited, &mut preorder);
    }

    // Subtree sizes, right to left. Pre-order makes a node's descendants
    // contiguous behind it, so its direct children are exactly the depth+1
    // entries sitting on top of the stack when the node is reached.
    let mut sizes = vec![1i64; preorder.len()];
    let mut pending: Vec<(i64, i64)> = Vec::new();
    for slot in (0..preorder.len()).rev() {
        let depth = preorder[slot].1;
        let mut size = 1i64;
        while let Some(&(child_depth, child_size)) = pending.last() {
            if child_depth != depth + 1 {
                break;
            }
            size += child_size;
            pending.pop();
        }
        sizes[slot] = size;
        pending.push((depth, size));
    }

    preorder
        .into_iter()
        .enumerate()
        .map(|(slot, (position, depth))| {
            let row = &rows[position];
            AnalyticsSubagentNodeV1 {
                provider: row.provider.clone(),
                session_id: row.session_id.clone(),
                parent_session_id: row.parent_session_id.clone(),
                agent: row.agent.clone(),
                title: row.title.clone(),
                started_at: row.started_at,
                ended_at: row.ended_at,
                is_subagent: row.is_subagent,
                parent_tool_use_id: row.parent_tool_use_id.clone(),
                depth,
                descendants: sizes[slot] - 1,
                link: link[position],
            }
        })
        .collect()
}

async fn subagent_tree_reading(
    db: Option<&RegisteredGlobalDb>,
    project_key: &str,
) -> Result<AnalyticsSubagentTreePayloadV1, String> {
    let Some(db) = db else {
        return Ok(AnalyticsSubagentTreePayloadV1 {
            available: false,
            source: "session_store_unavailable".to_owned(),
            error: None,
            nodes: Vec::new(),
            sessions_read: 0,
            root_count: 0,
            edge_count: 0,
            max_depth: 0,
            missing_parent_count: 0,
            cycle_count: 0,
            truncated: false,
        });
    };

    let connection = db.read_connection();
    let rows = query_rows(
        &connection,
        "SELECT provider,
                session_id,
                COALESCE(parent_session_id, '') AS parent_session_id,
                COALESCE(agent_id, '') AS agent_id,
                COALESCE(metadata_json, '') AS metadata_json,
                COALESCE(title, '') AS title,
                started_at,
                ended_at,
                is_subagent,
                COALESCE(parent_tool_use_id, '') AS parent_tool_use_id
         FROM sessions
         -- Either column may carry the project: `project_key` is a provider's
         -- own label and `project_path` the canonical root. Matching both is
         -- the convention every scoped session read in `registered_sessions`
         -- already uses, and matching only one silently empties the tree for
         -- whichever provider labels its sessions the other way.
         WHERE (project_key = ?1 OR project_path = ?1)
         ORDER BY COALESCE(started_at, 0), provider, session_id
         LIMIT ?2",
        params![project_key, SUBAGENT_TREE_SESSION_CEILING],
    )
    .await
    .map_err(|error| format!("analytics subagent tree query failed: {error}"))?;

    let sessions_read = rows.len() as i64;
    let session_rows: Vec<SubagentSessionRow> = rows
        .iter()
        .map(|row| {
            let agent_id = str_field(row, "agent_id");
            let agent = managed_agent_label_for_session(agent_id, str_field(row, "metadata_json"))
                .map(str::to_owned)
                .or_else(|| optional_text(row, "agent_id"));
            SubagentSessionRow {
                provider: str_field(row, "provider").to_owned(),
                session_id: str_field(row, "session_id").to_owned(),
                parent_session_id: optional_text(row, "parent_session_id"),
                agent,
                title: optional_text(row, "title"),
                started_at: row.get("started_at").and_then(Value::as_i64),
                ended_at: row.get("ended_at").and_then(Value::as_i64),
                is_subagent: i64_field(row, "is_subagent") != 0,
                parent_tool_use_id: optional_text(row, "parent_tool_use_id"),
            }
        })
        .collect();

    let nodes = build_subagent_tree(session_rows);
    let count_link = |wanted: AnalyticsSubagentLinkV1| {
        nodes.iter().filter(|node| node.link == wanted).count() as i64
    };
    Ok(AnalyticsSubagentTreePayloadV1 {
        available: true,
        source: "sessions".to_owned(),
        error: None,
        root_count: count_link(AnalyticsSubagentLinkV1::Root),
        edge_count: count_link(AnalyticsSubagentLinkV1::Linked),
        missing_parent_count: count_link(AnalyticsSubagentLinkV1::MissingParent),
        cycle_count: count_link(AnalyticsSubagentLinkV1::Cycle),
        max_depth: nodes.iter().map(|node| node.depth).max().unwrap_or(0),
        sessions_read,
        truncated: sessions_read >= SUBAGENT_TREE_SESSION_CEILING,
        nodes,
    })
}

/// `GET /api/plugins/analytics/subagent-tree` — parent/child session edges for
/// this project, as a pre-order tree.
///
/// The sibling `/agents` route answers a different question: how many sessions
/// each managed agent was delegated, with no edge between any two of them. This
/// route answers who delegated to whom. A rollup cannot be folded into a tree
/// after the fact, which is why both are served.
pub async fn subagent_tree(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsSubagentTreePayloadV1>>> {
    let project_key = RegisteredGlobalDb::canonical_project_key(&state.project_root);
    match subagent_tree_reading(state.lcm_db.as_deref(), &project_key).await {
        Ok(payload) if !payload.available => Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_subagent_tree_source_unavailable",
        )),
        Ok(payload) => {
            let count = payload.nodes.len() as u64;
            // A ceiling read has no denominator: the store holds an unknown
            // number of further sessions, so `partial` — which asserts a known
            // eligible total — would be the wrong claim. Coverage is unknown
            // with the count actually examined and the reason stated.
            let coverage = if payload.truncated {
                let mut coverage = DashboardCoverageV1::unknown();
                coverage.examined = Some(count);
                coverage.unit = Some("subagent_sessions".to_owned());
                coverage
                    .omission_reasons
                    .push("analytics_subagent_tree_scan_ceiling_reached".to_owned());
                coverage
            } else {
                DashboardCoverageV1::complete(count, "subagent_sessions")
            };
            Json(DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                coverage,
                Some(payload),
            ))
        }
        Err(error) => Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            error,
        )),
    }
}

/// `GET /api/plugins/analytics/hints`
pub async fn hints(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsHintsPayloadV1>>> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let payload = match typed_hint_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await
    {
        Ok(payload) => payload,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    if !payload.available {
        return Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_hint_source_unavailable",
        ));
    }
    let count = payload.by_category.len() as u64;
    let envelope = if durable_events
        .as_ref()
        .is_some_and(|events| events.len() == ANALYTICS_EVENT_LIMIT)
    {
        DashboardEnvelopeV1::partial(
            scope_from_state(&state),
            count.saturating_add(1),
            count,
            "hint_categories",
            vec!["analytics_event_limit".to_owned()],
            Some(payload),
        )
    } else {
        DashboardEnvelopeV1::ready(
            scope_from_state(&state),
            DashboardCoverageV1::complete(count, "hint_categories"),
            Some(payload),
        )
    };
    Json(envelope)
}

/// `GET /api/plugins/analytics/usage`
pub async fn usage(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsUsageSummaryV1>>> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    match typed_usage_summary(state.lcm_db.as_deref(), durable_events.as_deref()).await {
        Ok(payload) if !payload.available => Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_usage_source_unavailable",
        )),
        Ok(payload)
            if durable_events
                .as_ref()
                .is_some_and(|events| events.len() == ANALYTICS_EVENT_LIMIT) =>
        {
            let examined = payload.event_count.unwrap_or(payload.message_count).max(0) as u64;
            Json(DashboardEnvelopeV1::partial(
                scope_from_state(&state),
                examined.saturating_add(1),
                examined,
                "analytics_events",
                vec!["analytics_event_limit".to_owned()],
                Some(payload),
            ))
        }
        Ok(payload) => {
            let count = payload.event_count.unwrap_or(payload.message_count).max(0) as u64;
            Json(DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                DashboardCoverageV1::complete(count, "analytics_events"),
                Some(payload),
            ))
        }
        Err(error) => Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            error,
        )),
    }
}

/// `GET /api/plugins/analytics/diagnostics`
pub async fn diagnostics(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsDiagnosticsPayloadV1>>> {
    let durable_events = durable_analytics_rows_for_state(&state).await;
    let payload = match typed_diagnostics_summary(&state, durable_events.as_deref()).await {
        Ok(payload) => payload,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                error,
            ));
        }
    };
    if !payload.available {
        return Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(payload),
            "analytics_diagnostics_sources_unavailable",
        ));
    }
    let truncated_events = durable_events
        .as_ref()
        .is_some_and(|events| events.len() == ANALYTICS_EVENT_LIMIT);
    if truncated_events || payload.hook_window.truncated {
        let mut reasons = Vec::new();
        if truncated_events {
            reasons.push("analytics_event_limit".to_owned());
        }
        if payload.hook_window.truncated {
            reasons.push("hook_analytics_window".to_owned());
        }
        return Json(DashboardEnvelopeV1::partial(
            scope_from_state(&state),
            2,
            2_u64.saturating_sub(reasons.len() as u64),
            "analytics_sources",
            reasons,
            Some(payload),
        ));
    }
    Json(DashboardEnvelopeV1::ready(
        scope_from_state(&state),
        DashboardCoverageV1::complete(2, "analytics_sources"),
        Some(payload),
    ))
}

/// `GET /api/plugins/analytics/underused`
pub async fn underused(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsUnderusedPayloadV1>>> {
    match underused_tool_families(state.lcm_db.as_deref()).await {
        Ok(Some(families)) => {
            let payload = AnalyticsUnderusedPayloadV1 {
                available: true,
                db: state.lcm_db_path.clone(),
                families,
            };
            Json(DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                DashboardCoverageV1::complete(payload.families.len() as u64, "tool_families"),
                Some(payload),
            ))
        }
        Ok(None) => Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(AnalyticsUnderusedPayloadV1 {
                available: false,
                db: state.lcm_db_path.clone(),
                families: Vec::new(),
            }),
            "analytics_underused_source_unavailable",
        )),
        Err(error) => Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&state),
            Some(AnalyticsUnderusedPayloadV1 {
                available: false,
                db: state.lcm_db_path.clone(),
                families: Vec::new(),
            }),
            error,
        )),
    }
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

    let lcm_db = lcm_db?;
    let connection = lcm_db.read_connection();
    let rows = query_rows(
        &connection,
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

pub fn durable_analytics_event_row(event: &AnalyticsEventRecord) -> Value {
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

pub fn hint_summary_from_counts(counts: &[AnalyticsHintCounts]) -> Value {
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

fn decode_analytics_contract<T: serde::de::DeserializeOwned>(
    value: Value,
    label: &str,
) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("{label} did not match its response contract: {error}"))
}

async fn typed_hint_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[Value]>,
) -> Result<AnalyticsHintsPayloadV1, String> {
    decode_analytics_contract(
        hint_summary(db, durable_events).await,
        "analytics hint summary",
    )
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

    let connection = db.read_connection();
    let has_table = query_i64(
        &connection,
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
        &connection,
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

async fn session_message_rows(
    db: Option<&RegisteredGlobalDb>,
) -> Result<Option<Vec<Value>>, String> {
    let Some(db) = db else {
        return Ok(None);
    };
    let connection = db.read_connection();
    query_rows(
        &connection,
        "SELECT COALESCE(tool_names, '') AS tool_names,
                COALESCE(text, '') AS text,
                COALESCE(metadata_json, '') AS metadata_json
         FROM session_messages
         ORDER BY timestamp, ordinal
         LIMIT 10000",
        (),
    )
    .await
    .map(Some)
    .map_err(|error| format!("session-message query failed: {error}"))
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
) -> Result<AnalyticsUsageSummaryV1, String> {
    let usage = usage_summary(db, durable_events).await?;
    serde_json::from_value::<AnalyticsUsageSummaryV1>(usage)
        .map_err(|error| format!("analytics usage summary did not match its contract: {error}"))
}

async fn usage_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[Value]>,
) -> Result<Value, String> {
    if let Some(events) = durable_events {
        return Ok(usage_summary_from_events(events));
    }

    let Some(rows) = session_message_rows(db).await? else {
        return Ok(json!({
            "available": false,
            "message_count": 0,
            "by_category": [],
        }));
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

    Ok(json!({
        "available": true,
        "message_count": rows.len() as i64,
        "by_category": usage_count_rows(counts),
    }))
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

async fn diagnostics_summary(
    state: &DashboardState,
    durable_events: Option<&[Value]>,
) -> Result<Value, String> {
    let message_count = session_message_rows(state.lcm_db.as_deref())
        .await?
        .map_or(0, |rows| rows.len() as i64);
    let hook_analytics = read_hook_analytics_rows(state);
    Ok(diagnostics_summary_from_parts(
        message_count,
        &hook_analytics,
        durable_events,
    ))
}

async fn typed_diagnostics_summary(
    state: &DashboardState,
    durable_events: Option<&[Value]>,
) -> Result<AnalyticsDiagnosticsPayloadV1, String> {
    decode_analytics_contract(
        diagnostics_summary(state, durable_events).await?,
        "analytics diagnostics",
    )
}

pub fn diagnostics_summary_from_parts(
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
pub const HOOK_ANALYTICS_WINDOW_ROWS: usize = 10_000;

/// Suffix chunk size used when walking a hook analytics file backwards.
const HOOK_ANALYTICS_TAIL_CHUNK_BYTES: u64 = 1 << 20;

/// Window provenance for the hook rows folded into a diagnostics payload.
#[derive(Default)]
pub struct HookAnalyticsWindow {
    /// Per-file cap on trailing rows scanned.
    pub window_rows: usize,
    /// Rows actually scanned across every file in the window.
    pub rows_scanned: i64,
    /// True when at least one file was larger than its window, so the
    /// aggregates cover a recent suffix rather than the full history.
    pub truncated: bool,
}

pub struct HookAnalyticsRows {
    pub rows: Vec<Value>,
    pub sources: Vec<Value>,
    pub window: HookAnalyticsWindow,
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
pub fn read_hook_analytics_rows_at(
    store_root: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    let mut out = HookAnalyticsRows::empty();
    let store_path = store_root.map(|root| root.join("hook_analytics.jsonl"));
    if let Some(store_path) = &store_path {
        read_hook_analytics_file(store_path, None, &mut out);
    }
    if let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() {
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
    let mut count = 0;
    let mut remaining = haystack;
    while let Some(index) = remaining.iter().position(|byte| *byte == needle) {
        count += 1;
        remaining = &remaining[index + 1..];
    }
    count
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

async fn underused_tool_families(
    db: Option<&RegisteredGlobalDb>,
) -> Result<Option<Vec<AnalyticsUnderusedFamilyV1>>, String> {
    let Some(rows) = session_message_rows(db).await? else {
        return Ok(None);
    };

    Ok(Some(
        underused_tool_family_signals(rows.iter().map(|row| {
            let text = str_field(row, "text");
            ToolUsageObservation {
                tool_names: Some(str_field(row, "tool_names")),
                metadata_json: Some(str_field(row, "metadata_json")),
                text: Some(text),
            }
        }))
        .into_iter()
        .map(|signal| AnalyticsUnderusedFamilyV1 {
            family: signal.family,
            relevant_events: signal.relevant_events,
            usage_events: signal.usage_events,
            missed_events: signal.missed_events,
            underused: signal.underused,
        })
        .collect(),
    ))
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        AnalyticsDiagnosticsPayloadV1, AnalyticsSubagentLinkV1, HOOK_ANALYTICS_WINDOW_ROWS,
        HookAnalyticsRows, HookAnalyticsWindow, SubagentSessionRow, build_subagent_tree,
        decode_analytics_contract, diagnostics_summary_from_parts, hint_efficacy_from_events,
        hint_summary_from_events, read_hook_analytics_file, recent_hook_rows,
        sort_hook_analytics_rows,
    };

    fn row(session_id: &str, parent: Option<&str>) -> SubagentSessionRow {
        SubagentSessionRow {
            provider: "codex".to_owned(),
            session_id: session_id.to_owned(),
            parent_session_id: parent.map(str::to_owned),
            agent: None,
            title: None,
            started_at: None,
            ended_at: None,
            is_subagent: parent.is_some(),
            parent_tool_use_id: None,
        }
    }

    fn shape(nodes: &[super::AnalyticsSubagentNodeV1]) -> Vec<(&str, i64, i64)> {
        nodes
            .iter()
            .map(|node| (node.session_id.as_str(), node.depth, node.descendants))
            .collect()
    }

    #[test]
    fn tree_nests_children_under_parents_in_preorder_with_subtree_sizes() {
        let nodes = build_subagent_tree(vec![
            row("root", None),
            row("child.a", Some("root")),
            row("grandchild", Some("child.a")),
            row("child.b", Some("root")),
        ]);

        // Pre-order: a node precedes its whole subtree, and `descendants`
        // counts that subtree without counting the node itself.
        assert_eq!(
            shape(&nodes),
            vec![
                ("root", 0, 3),
                ("child.a", 1, 1),
                ("grandchild", 2, 0),
                ("child.b", 1, 0),
            ]
        );
    }

    #[test]
    fn a_session_whose_parent_is_absent_is_a_cut_edge_not_a_root() {
        let nodes = build_subagent_tree(vec![
            row("real.root", None),
            row("orphan", Some("never.ingested")),
        ]);

        let link = |id: &str| {
            nodes
                .iter()
                .find(|node| node.session_id == id)
                .map(|node| node.link)
                .unwrap()
        };
        assert_eq!(link("real.root"), AnalyticsSubagentLinkV1::Root);
        assert_eq!(link("orphan"), AnalyticsSubagentLinkV1::MissingParent);
        // Both draw at the margin, which is exactly why the link kinds, not the
        // depth, are what a caption may be built from.
        assert!(nodes.iter().all(|node| node.depth == 0));
    }

    #[test]
    fn cycles_are_surfaced_rather_than_dropped_from_the_count() {
        let nodes = build_subagent_tree(vec![
            row("a", Some("b")),
            row("b", Some("a")),
            row("self", Some("self")),
        ]);

        // Every input session is still present — a tree walk that silently lost
        // them would under-report delegation.
        assert_eq!(nodes.len(), 3);
        assert!(
            nodes
                .iter()
                .all(|node| node.link == AnalyticsSubagentLinkV1::Cycle)
        );
    }

    #[test]
    fn edges_never_join_two_providers_that_minted_the_same_session_id() {
        let child = SubagentSessionRow {
            provider: "claude".to_owned(),
            ..row("child", Some("shared.id"))
        };
        let nodes = build_subagent_tree(vec![row("shared.id", None), child]);

        // The Claude child names a session id the Codex store also holds. They
        // are different sessions, so no delegation may be invented between them.
        let claude = nodes
            .iter()
            .find(|node| node.provider == "claude")
            .expect("claude session retained");
        assert_eq!(claude.link, AnalyticsSubagentLinkV1::MissingParent);
        assert_eq!(claude.descendants, 0);
        assert_eq!(
            nodes
                .iter()
                .find(|node| node.provider == "codex")
                .map(|node| node.descendants),
            Some(0)
        );
    }

    #[test]
    fn every_input_session_appears_exactly_once() {
        let nodes = build_subagent_tree(vec![
            row("root", None),
            row("child", Some("root")),
            row("orphan", Some("gone")),
            row("cycle.a", Some("cycle.b")),
            row("cycle.b", Some("cycle.a")),
        ]);

        let mut ids: Vec<&str> = nodes.iter().map(|node| node.session_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["child", "cycle.a", "cycle.b", "orphan", "root"]);
    }

    #[test]
    fn an_empty_store_builds_an_empty_tree_without_panicking() {
        assert!(build_subagent_tree(Vec::new()).is_empty());
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        // The walk is iterative on purpose: delegation depth is data, and a
        // recursive walk would let a pathological store crash the daemon.
        let depth = 10_000usize;
        let mut rows = vec![row("session.0", None)];
        for step in 1..depth {
            let parent = format!("session.{}", step - 1);
            rows.push(row(&format!("session.{step}"), Some(&parent)));
        }

        let nodes = build_subagent_tree(rows);
        assert_eq!(nodes.len(), depth);
        assert_eq!(nodes[0].descendants, depth as i64 - 1);
        assert_eq!(nodes[depth - 1].depth, depth as i64 - 1);
    }

    #[test]
    fn unavailable_diagnostics_value_decodes_to_the_canonical_payload() {
        let value = diagnostics_summary_from_parts(0, &HookAnalyticsRows::empty(), None);
        let payload: AnalyticsDiagnosticsPayloadV1 =
            decode_analytics_contract(value, "analytics diagnostics").unwrap();

        assert!(!payload.available);
        assert_eq!(payload.event_count, 0);
        assert!(!payload.hook_window.truncated);
    }

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

    /// This crate owns the diagnostics summary but not the readiness
    /// aggregation: it reads that through the port the composition root
    /// installs. With no projection mounted the summary must fail closed —
    /// naming the blocker and counting rows only — and must never echo an
    /// untrusted row's own values back out.
    ///
    /// The mounted counterpart, which is the only composition that can answer
    /// `measured`, is proven by
    /// `dashboard_diagnostics_summary_aggregates_hook_completed_rows_safely`
    /// in `src/hooks/analytics/tests.rs`.
    #[test]
    fn diagnostics_summary_without_a_readiness_projection_fails_closed_safely() {
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
        let readiness = &summary["hook_readiness"];

        // Fail closed: no projection means no measurement, said plainly.
        assert_eq!(readiness["collection_status"], "unavailable");
        assert_eq!(readiness["events_considered"], 0);
        assert_eq!(readiness["input_rows_received"], 1);
        assert_eq!(readiness["input_rows_processed"], 0);
        assert_eq!(
            readiness["unavailable_metrics"][0]["blocker"],
            "hook readiness projection is not mounted"
        );
        // The row count is still real, so the frontend cannot read this as an
        // empty stream.
        assert_eq!(summary["hook_call_count"], 0);

        let encoded = serde_json::to_string(readiness).expect("readiness encodes");
        for forbidden in [
            "untrusted-host",
            "privateHookName",
            "private-reason",
            "hook_name",
            "reason_code",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "the unmounted envelope must not leak {forbidden}: {encoded}"
            );
        }
    }
}
