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

use tracedecay_automation::analytics::{
    ToolUsageObservation, UsageKind, categorize_skill, infer_usage_events,
    underused_tool_family_signals,
};
use tracedecay_automation_runtime::automation::agent_targets::managed_agent_label;
use tracedecay_automation_runtime::automation::host_io::HostIo;
use tracedecay_global_db::{
    AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts, RegisteredGlobalDb,
};
use tracedecay_runtime_core::db::engine::params;

use super::DashboardState;
use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::util::{i64_field, query_i64, query_i64_result, query_rows, str_field};

pub use tracedecay_usecases::analytics_bridge::{
    AnalyticsDiagnosticsPayloadV1, AnalyticsDiagnosticsRatiosV1, AnalyticsEventKindCountV1,
    AnalyticsHintEfficacyCategoryV1, AnalyticsHintEfficacyTotalsV1, AnalyticsHintEfficacyV1,
    AnalyticsHookNameCountV1, AnalyticsHookWindowV1, AnalyticsOutcomeCountV1,
    AnalyticsPromptCategoryCountV1, AnalyticsRecentEventV1, AnalyticsRecentHookV1,
    AnalyticsToolCategoryCountV1, AnalyticsToolCountV1, HOOK_ANALYTICS_WINDOW_ROWS,
    HookAnalyticsRows, HookAnalyticsWindow, diagnostics_payload_from_parts,
    diagnostics_summary_from_parts, durable_analytics_event_row, hint_efficacy_from_events,
    read_hook_analytics_file, read_hook_analytics_rows_at, recent_hook_rows,
    sort_hook_analytics_rows,
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
    hotpath::future!(
        async move {
            // These reads are independent of one another; run them concurrently. The
            // hint/usage/diagnostics summaries then share the one durable-event fetch.
            let (durable_events, observatory, agents, underused) = tokio::join!(
                durable_analytics_rows_for_state(&state),
                observatory_model(&state),
                agent_usage_summary(&state.host_io, state.lcm_db.as_deref()),
                underused_tool_families(state.lcm_db.as_deref()),
            );
            let observatory = Some(observatory);
            let project_id = RegisteredGlobalDb::canonical_project_key(&state.project_root);
            let (hints, usage, diagnostics) = tokio::join!(
                hint_summary(
                    state.savings_db.as_deref().or(state.lcm_db.as_deref()),
                    durable_events.as_deref(),
                    Some(&project_id),
                ),
                typed_usage_summary(state.lcm_db.as_deref(), durable_events.as_deref()),
                typed_diagnostics_summary(&state, durable_events.as_deref()),
            );
            let usage = match usage {
                Ok(usage) => usage,
                Err(error) => {
                    return Json(DashboardEnvelopeV1::error(
                        scope_from_state(&state),
                        None,
                        error,
                    ));
                }
            };
            let agents = match agents {
                Ok(agents) => agents,
                Err(error) => {
                    return Json(DashboardEnvelopeV1::error(
                        scope_from_state(&state),
                        None,
                        error,
                    ));
                }
            };
            let diagnostics = match diagnostics {
                Ok(diagnostics) => diagnostics,
                Err(error) => {
                    return Json(DashboardEnvelopeV1::error(
                        scope_from_state(&state),
                        None,
                        error,
                    ));
                }
            };
            let underused = match underused {
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
        },
        label = "dashboard_api.analytics.overview"
    )
    .await
}

/// Canonical Observatory read model. CLI/MCP call the same application
/// composer instead of re-deriving these values in their adapters.
pub async fn observatory(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<ObservatoryReadModelV1>> {
    hotpath::future!(
        async move {
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
        },
        label = "dashboard_api.analytics.observatory"
    )
    .await
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

async fn agent_usage_summary(
    host_io: &HostIo,
    db: Option<&RegisteredGlobalDb>,
) -> Result<AnalyticsAgentsPayloadV1, String> {
    let Some(db) = db else {
        return Ok(AnalyticsAgentsPayloadV1 {
            available: false,
            source: "session_store_unavailable".to_owned(),
            by_agent: Vec::new(),
        });
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
            managed_agent_label_for_session(host_io, agent_id, str_field(&row, "metadata_json"))
        else {
            continue;
        };
        *by_agent.entry(label.to_string()).or_default() += 1;
    }

    Ok(AnalyticsAgentsPayloadV1 {
        available: true,
        source: "sessions".to_owned(),
        by_agent: by_agent
            .into_iter()
            .map(|(agent, sessions)| AnalyticsAgentUsageV1 { agent, sessions })
            .collect(),
    })
}

fn managed_agent_label_for_session(
    host_io: &HostIo,
    agent_id: &str,
    metadata_json: &str,
) -> Option<&'static str> {
    if let Some(label) = managed_agent_label(host_io, agent_id) {
        return Some(label);
    }
    let metadata: Value = serde_json::from_str(metadata_json).ok()?;
    ["agent_nickname", "agent_role"]
        .into_iter()
        .filter_map(|key| metadata.get(key).and_then(Value::as_str))
        .find_map(|id| managed_agent_label(host_io, id))
}

/// `GET /api/plugins/analytics/agents` — sessions per managed subagent,
/// straight from the session store. The same summary the overview embeds,
/// exposed on its own so the Agents workspace can read subagent context
/// without paying for the full hook-analytics fold.
pub async fn agents(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsAgentsPayloadV1>>> {
    hotpath::future!(
        async move {
            match agent_usage_summary(&state.host_io, state.lcm_db.as_deref()).await {
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
        },
        label = "dashboard_api.analytics.agents"
    )
    .await
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
            row.session_id.as_str(),
            row.provider.as_str(),
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
    host_io: &HostIo,
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
            let agent =
                managed_agent_label_for_session(host_io, agent_id, str_field(row, "metadata_json"))
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
    hotpath::future!(
        async move {
            let project_key = RegisteredGlobalDb::canonical_project_key(&state.project_root);
            match subagent_tree_reading(&state.host_io, state.lcm_db.as_deref(), &project_key).await
            {
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
        },
        label = "dashboard_api.analytics.subagent_tree"
    )
    .await
}

/// `GET /api/plugins/analytics/hints`
pub async fn hints(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsHintsPayloadV1>>> {
    hotpath::future!(
        async move {
            let durable_events = durable_analytics_rows_for_state(&state).await;
            let project_id = RegisteredGlobalDb::canonical_project_key(&state.project_root);
            let payload = hint_summary(
                state.savings_db.as_deref().or(state.lcm_db.as_deref()),
                durable_events.as_deref(),
                Some(&project_id),
            )
            .await;
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
        },
        label = "dashboard_api.analytics.hints"
    )
    .await
}

/// `GET /api/plugins/analytics/usage`
pub async fn usage(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsUsageSummaryV1>>> {
    hotpath::future!(
        async move {
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
                    let examined =
                        payload.event_count.unwrap_or(payload.message_count).max(0) as u64;
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
        },
        label = "dashboard_api.analytics.usage"
    )
    .await
}

/// `GET /api/plugins/analytics/diagnostics`
pub async fn diagnostics(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsDiagnosticsPayloadV1>>> {
    hotpath::future!(
        async move {
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
        },
        label = "dashboard_api.analytics.diagnostics"
    )
    .await
}

/// `GET /api/plugins/analytics/underused`
pub async fn underused(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<AnalyticsUnderusedPayloadV1>>> {
    hotpath::future!(
        async move {
            match underused_tool_families(state.lcm_db.as_deref()).await {
                Ok(Some(families)) => {
                    let payload = AnalyticsUnderusedPayloadV1 {
                        available: true,
                        db: state.lcm_db_path.clone(),
                        families,
                    };
                    Json(DashboardEnvelopeV1::ready(
                        scope_from_state(&state),
                        DashboardCoverageV1::complete(
                            payload.families.len() as u64,
                            "tool_families",
                        ),
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
        },
        label = "dashboard_api.analytics.underused"
    )
    .await
}

fn empty_hint_categories() -> Vec<AnalyticsHintCategoryV1> {
    HINT_CATEGORIES
        .iter()
        .map(|category| AnalyticsHintCategoryV1 {
            category: (*category).to_owned(),
            emitted: 0,
            followed: 0,
            ignored: 0,
            suppressed: 0,
        })
        .collect()
}

async fn durable_analytics_rows_for_state(
    state: &DashboardState,
) -> Option<Vec<AnalyticsEventRecord>> {
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
) -> Option<Vec<AnalyticsEventRecord>> {
    let query = AnalyticsEventQuery {
        provider: None,
        project_id: Some(project_id.to_string()),
        session_id: None,
        event_kind: None,
        since: None,
        until: None,
        before_id: None,
        limit: ANALYTICS_EVENT_LIMIT,
    };
    for db in [global_db, lcm_db].into_iter().flatten() {
        if let Ok(events) = db.query_analytics_events(&query).await
            && !events.is_empty()
        {
            return Some(events);
        }
    }
    None
}

pub fn hint_summary_from_events(events: &[AnalyticsEventRecord]) -> AnalyticsHintsPayloadV1 {
    let mut by_category: BTreeMap<String, HintCounts> = HINT_CATEGORIES
        .iter()
        .map(|category| ((*category).to_string(), HintCounts::default()))
        .collect();

    for event in events {
        let category = event.hint_category.as_deref().unwrap_or("");
        if category.is_empty() {
            continue;
        }
        let counts = by_category.entry(category.to_owned()).or_default();
        let event_kind = normalize(&event.event_kind);
        match event_kind.as_str() {
            "hint_emitted" | "hint_escalated" | "missing_session" => counts.emitted += 1,
            "hint_outcome" => match normalize(event.outcome.as_deref().unwrap_or("")).as_str() {
                "acted" => counts.followed += 1,
                "ignored" => counts.ignored += 1,
                _ => {}
            },
            _ if event_kind.starts_with("suppressed_") => counts.suppressed += 1,
            _ => {}
        }
    }

    AnalyticsHintsPayloadV1 {
        available: true,
        source: "analytics_events".to_owned(),
        error: None,
        by_category: by_category
            .into_iter()
            .map(|(category, counts)| AnalyticsHintCategoryV1 {
                category,
                emitted: counts.emitted,
                followed: counts.followed,
                ignored: counts.ignored,
                suppressed: counts.suppressed,
            })
            .collect(),
    }
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

#[cfg(test)]
fn decode_analytics_contract<T: serde::de::DeserializeOwned>(
    value: Value,
    label: &str,
) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("{label} did not match its response contract: {error}"))
}

fn typed_hint_summary_from_counts(counts: &[AnalyticsHintCounts]) -> AnalyticsHintsPayloadV1 {
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
    AnalyticsHintsPayloadV1 {
        available: true,
        source: "analytics_events".to_owned(),
        error: None,
        by_category: by_category
            .into_iter()
            .map(|(category, counts)| AnalyticsHintCategoryV1 {
                category,
                emitted: counts.emitted,
                followed: counts.followed,
                ignored: counts.ignored,
                suppressed: counts.suppressed,
            })
            .collect(),
    }
}

async fn hint_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[AnalyticsEventRecord]>,
    project_id: Option<&str>,
) -> AnalyticsHintsPayloadV1 {
    // A present savings/profile DB can query successfully with zero hint rows
    // while the project store still has durable events. Empty counts are not a
    // source; fall through so session-only fixtures stay unavailable and
    // project-scoped events remain visible.
    if let (Some(db), Some(project_id)) = (db, project_id)
        && let Ok(counts) = db.query_analytics_hint_counts(Some(project_id), 0).await
        && !counts.is_empty()
    {
        return typed_hint_summary_from_counts(&counts);
    }
    if let Some(events) = durable_events {
        return hint_summary_from_events(events);
    }

    let Some(db) = db else {
        return AnalyticsHintsPayloadV1 {
            available: false,
            source: "session_store_unavailable".to_owned(),
            error: None,
            by_category: empty_hint_categories(),
        };
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
        return AnalyticsHintsPayloadV1 {
            available: false,
            source: "dashboard_hint_events_missing".to_owned(),
            error: None,
            by_category: empty_hint_categories(),
        };
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
            return AnalyticsHintsPayloadV1 {
                available: false,
                source: "dashboard_hint_events_error".to_owned(),
                error: Some(err),
                by_category: empty_hint_categories(),
            };
        }
    };

    let mut by_category: BTreeMap<String, AnalyticsHintCategoryV1> = empty_hint_categories()
        .into_iter()
        .map(|row| (row.category.clone(), row))
        .collect();
    for row in rows {
        let category = str_field(&row, "category");
        by_category.insert(
            category.to_owned(),
            AnalyticsHintCategoryV1 {
                category: category.to_owned(),
                emitted: i64_field(&row, "emitted"),
                followed: i64_field(&row, "followed"),
                ignored: i64_field(&row, "ignored"),
                suppressed: i64_field(&row, "suppressed"),
            },
        );
    }

    AnalyticsHintsPayloadV1 {
        available: true,
        source: "dashboard_hint_events".to_owned(),
        error: None,
        by_category: by_category.into_values().collect(),
    }
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

fn usage_summary_from_events(events: &[AnalyticsEventRecord]) -> AnalyticsUsageSummaryV1 {
    let mut counts: BTreeMap<(String, String), i64> = BTreeMap::new();
    for event in events {
        record_event_usage(
            &mut counts,
            &event.event_kind,
            event.tool_name.as_deref().unwrap_or(""),
            event.skill_name.as_deref().unwrap_or(""),
            event.metadata_json.as_deref().unwrap_or(""),
        );
    }

    AnalyticsUsageSummaryV1 {
        available: true,
        source: Some("analytics_events".to_owned()),
        message_count: events.len() as i64,
        event_count: Some(events.len() as i64),
        by_category: usage_count_rows(counts),
    }
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
/// Absent `source` / `event_count` stay `None` on the struct so serde writes
/// them as explicit nulls. The previous JSON literals omitted those keys and
/// had to round-trip through this type to keep that distinction.
async fn typed_usage_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[AnalyticsEventRecord]>,
) -> Result<AnalyticsUsageSummaryV1, String> {
    usage_summary(db, durable_events).await
}

async fn usage_summary(
    db: Option<&RegisteredGlobalDb>,
    durable_events: Option<&[AnalyticsEventRecord]>,
) -> Result<AnalyticsUsageSummaryV1, String> {
    if let Some(events) = durable_events {
        return Ok(usage_summary_from_events(events));
    }

    let Some(rows) = session_message_rows(db).await? else {
        return Ok(AnalyticsUsageSummaryV1 {
            available: false,
            source: None,
            message_count: 0,
            event_count: None,
            by_category: Vec::new(),
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

    Ok(AnalyticsUsageSummaryV1 {
        available: true,
        source: None,
        message_count: rows.len() as i64,
        event_count: None,
        by_category: usage_count_rows(counts),
    })
}

fn usage_count_rows(counts: BTreeMap<(String, String), i64>) -> Vec<AnalyticsUsageCategoryV1> {
    counts
        .into_iter()
        .map(|((kind, category), events)| AnalyticsUsageCategoryV1 {
            kind,
            category,
            events,
        })
        .collect()
}

/// Bounded scalar count of `session_messages`, capped at the same 10,000-row
/// ceiling as [`session_message_rows`] so the diagnostics `message_count`
/// keeps its meaning without hauling full rows (text and metadata included)
/// through the JSON layer just to be counted.
async fn session_message_count(db: Option<&RegisteredGlobalDb>) -> Result<i64, String> {
    let Some(db) = db else {
        return Ok(0);
    };
    let connection = db.read_connection();
    query_i64_result(
        &connection,
        "SELECT COUNT(*) FROM (SELECT 1 FROM session_messages LIMIT 10000)",
        (),
    )
    .await
    .map_err(|error| format!("session-message count query failed: {error}"))
}

async fn typed_diagnostics_summary(
    state: &DashboardState,
    durable_events: Option<&[AnalyticsEventRecord]>,
) -> Result<AnalyticsDiagnosticsPayloadV1, String> {
    let message_count = session_message_count(state.lcm_db.as_deref()).await?;
    // The hook stream is plain synchronous file IO over up-to-megabyte tails;
    // read it off the async worker instead of blocking a runtime thread.
    let store_root = state.store_root.clone();
    let project_root = state.project_root.clone();
    let hook_analytics = tokio::task::spawn_blocking(move || {
        read_hook_analytics_rows_at(Some(&store_root), Some(&project_root))
    })
    .await
    .map_err(|error| format!("hook analytics read task failed: {error}"))?;
    Ok(diagnostics_payload_from_parts(
        message_count,
        &hook_analytics,
        durable_events,
    ))
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
    use tracedecay_global_db::AnalyticsEventRecord;

    fn analytics_event(
        event_kind: &str,
        hint_category: &str,
        outcome: &str,
    ) -> AnalyticsEventRecord {
        AnalyticsEventRecord {
            id: 0,
            provider: String::new(),
            project_id: String::new(),
            session_id: None,
            timestamp: 0,
            event_kind: event_kind.to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: (!hint_category.is_empty()).then(|| hint_category.to_owned()),
            hint_id: None,
            outcome: (!outcome.is_empty()).then(|| outcome.to_owned()),
            metadata_json: None,
        }
    }

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
            analytics_event("hint_emitted", "search", "observed"),
            analytics_event("hint_outcome", "search", "acted"),
            analytics_event("hint_emitted", "file_lookup", "observed"),
            analytics_event("hint_outcome", "file_lookup", "ignored"),
            analytics_event("hint_escalated", "impact", "observed"),
            analytics_event("suppressed_duplicate", "impact", "observed"),
        ];

        let summary = hint_summary_from_events(&events);
        let row = |category: &str| {
            summary
                .by_category
                .iter()
                .find(|row| row.category == category)
                .unwrap()
        };
        assert_eq!(row("search").emitted, 1);
        assert_eq!(row("search").followed, 1);
        assert_eq!(row("file_lookup").emitted, 1);
        assert_eq!(row("file_lookup").ignored, 1);
        assert_eq!(row("impact").emitted, 1);
        assert_eq!(row("impact").suppressed, 1);
    }

    #[test]
    fn hint_efficacy_counts_emitted_acted_ignored_and_unresolved() {
        let events = vec![
            analytics_event("hint_emitted", "search", ""),
            analytics_event("hint_emitted", "search", ""),
            analytics_event("hint_emitted", "search", ""),
            analytics_event("hint_outcome", "search", "acted"),
            analytics_event("hint_outcome", "search", "ignored"),
            analytics_event("hint_emitted", "impact", ""),
            // Unrelated events must not affect hint efficacy.
            {
                let mut event = analytics_event("mcp_tool_call", "", "");
                event.tool_name = Some("tracedecay_context".to_owned());
                event
            },
        ];

        let summary = hint_efficacy_from_events(&events);
        assert!(summary.available);
        assert_eq!(summary.totals.emitted, 4);
        assert_eq!(summary.totals.acted, 1);
        assert_eq!(summary.totals.ignored, 1);
        // 4 emitted - 1 acted - 1 ignored = 2 still unresolved.
        assert_eq!(summary.totals.unresolved, 2);

        let search = summary
            .by_category
            .iter()
            .find(|row| row.category == "search")
            .unwrap();
        assert_eq!(search.emitted, 3);
        assert_eq!(search.acted, 1);
        assert_eq!(search.ignored, 1);
        assert_eq!(search.unresolved, 1);

        let impact = summary
            .by_category
            .iter()
            .find(|row| row.category == "impact")
            .unwrap();
        assert_eq!(impact.emitted, 1);
        assert_eq!(impact.unresolved, 1);
    }

    #[test]
    fn hint_efficacy_is_unavailable_without_hint_events() {
        let summary = hint_efficacy_from_events(&[analytics_event("mcp_tool_call", "", "")]);
        assert!(!summary.available);
        assert!(summary.by_category.is_empty());
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
        assert_eq!(recent[0].ts_unix_ms, Some(12));
        assert_eq!(recent[0].session_id, "c");
        assert_eq!(recent[1].ts_unix_ms, Some(11));
        assert_eq!(recent[1].session_id, "b");
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
