//! LCM dashboard API.
//!
//! The dashboard composition root injects a daemon-owned canonical temporal
//! retrieval port. This adapter never queries LCM tables or hydrates payloads
//! directly; a missing authority remains a typed unavailable state.

use std::future::Future;
use std::pin::Pin;

use axum::{Json, extract::State};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DashboardState;
use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::util::{JsonPath, JsonQuery};

#[derive(Clone, Debug)]
pub enum DashboardLcmReadRequestV1 {
    Search {
        query: String,
        limit: i64,
        offset: i64,
        cursor: Option<String>,
        role: Option<String>,
        source: Option<String>,
        session_id: Option<String>,
        since: Option<i64>,
        until: Option<i64>,
    },
    Session {
        session_id: String,
        limit: i64,
        cursor: Option<String>,
    },
}

pub enum DashboardLcmReadOutcomeV1 {
    Ready(DashboardLcmCanonicalPageV1),
    NotReady {
        state: DashboardLcmReadStateV1,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardLcmReadStateV1 {
    Absent,
    Stale,
    Locked,
    Denied,
    Redacted,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct DashboardLcmCanonicalMessageV1 {
    pub session_id: String,
    pub provider: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub content: String,
    pub message_id: String,
    pub metadata_json: Option<String>,
    pub tool_names: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DashboardLcmCanonicalSummaryV1 {
    pub node_id: String,
    pub session_id: String,
    pub depth: i64,
    pub token_count: Option<i64>,
    pub source_token_count: Option<i64>,
    pub latest_at: Option<i64>,
    pub created_at: i64,
    pub expand_hint: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default)]
pub struct DashboardLcmCanonicalStatsV1 {
    pub message_count: i64,
    pub summary_node_count: i64,
    pub summary_token_count: Option<i64>,
    pub source_token_count: Option<i64>,
    pub depth_counts: Vec<(i64, i64)>,
}

#[derive(Clone, Debug)]
pub struct DashboardLcmCanonicalPageV1 {
    pub messages: Vec<DashboardLcmCanonicalMessageV1>,
    pub summary_nodes: Vec<DashboardLcmCanonicalSummaryV1>,
    pub stats: DashboardLcmCanonicalStatsV1,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

pub type DashboardLcmReadFutureV1<'a> =
    Pin<Box<dyn Future<Output = DashboardLcmReadOutcomeV1> + Send + 'a>>;

pub trait DashboardLcmReadPortV1: Send + Sync {
    fn read(
        &self,
        project_id: Option<&str>,
        request: DashboardLcmReadRequestV1,
    ) -> DashboardLcmReadFutureV1<'_>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSessionCountsV1 {
    message_count: i64,
    summary_node_count: i64,
    token_estimate_total: Option<i64>,
    summary_token_count: Option<i64>,
    source_token_count: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmMessageV1 {
    pub(super) store_id: Option<i64>,
    pub(super) session_id: String,
    pub(super) role: Option<String>,
    pub(super) source: Option<String>,
    pub(super) timestamp: Option<i64>,
    pub(super) token_estimate: Option<i64>,
    pub(super) content: Option<String>,
    pub(super) message_id: String,
    pub(super) ordinal: Option<i64>,
    pub(super) storage_kind: Option<String>,
    pub(super) metadata_json: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) pinned: Option<i64>,
    pub(super) summary_node_ids: Vec<String>,
    #[serde(default)]
    pub(super) snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmSummaryNodeV1 {
    pub(super) node_id: String,
    pub(super) session_id: String,
    pub(super) depth: i64,
    pub(super) category: String,
    pub(super) source_type: String,
    pub(super) token_count: Option<i64>,
    pub(super) source_token_count: Option<i64>,
    pub(super) latest_at: Option<i64>,
    pub(super) created_at: i64,
    pub(super) expand_hint: String,
    pub(super) summary: String,
    #[serde(default)]
    pub(super) recency: Option<i64>,
    #[serde(default)]
    pub(super) snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmRoleCountV1 {
    role: Option<String>,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSourceCountV1 {
    source: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmDepthCountV1 {
    depth: i64,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmCompressionSummaryV1 {
    source_token_count: Option<i64>,
    token_count: Option<i64>,
    ratio: Option<f64>,
    node_count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmOverviewStatsV1 {
    messages_total: i64,
    sessions_total: i64,
    summary_nodes_total: i64,
    summary_node_sessions_total: i64,
    max_summary_depth: i64,
    role_counts: Vec<LcmRoleCountV1>,
    source_counts: Vec<LcmSourceCountV1>,
    depth_counts: Vec<LcmDepthCountV1>,
    compression: LcmCompressionSummaryV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmLatestSessionV1 {
    session_id: String,
    message_count: i64,
    last_store_id: Option<i64>,
    last_timestamp: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmMatchesV1 {
    messages: Vec<LcmMessageV1>,
    summary_nodes: Vec<LcmSummaryNodeV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct LcmOverviewPayloadV1 {
    path: String,
    storage_scope: String,
    exists: bool,
    overview: LcmOverviewStatsV1,
    latest_sessions: Vec<LcmLatestSessionV1>,
    latest_summary_nodes: Vec<LcmSummaryNodeV1>,
    matches: LcmMatchesV1,
    query: String,
    limit: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSearchEngineDetailV1 {
    messages: String,
    summary_nodes: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSearchTotalsV1 {
    messages: i64,
    summary_nodes: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSearchFiltersV1 {
    role: Option<String>,
    source: Option<String>,
    session_id: Option<String>,
    since: Option<f64>,
    until: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct LcmSearchPayloadV1 {
    path: String,
    storage_scope: String,
    exists: bool,
    query: String,
    limit: i64,
    offset: i64,
    next_cursor: Option<String>,
    engine: String,
    engine_detail: LcmSearchEngineDetailV1,
    total: LcmSearchTotalsV1,
    filters: LcmSearchFiltersV1,
    matches: LcmMatchesV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmSessionPayloadV1 {
    path: String,
    storage_scope: String,
    exists: bool,
    session_id: String,
    limit: i64,
    counts: LcmSessionCountsV1,
    messages: Vec<LcmMessageV1>,
    summary_nodes: Vec<LcmSummaryNodeV1>,
    has_more: bool,
    has_more_messages: bool,
    has_more_summary_nodes: bool,
    next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmTimelineBucketV1 {
    bucket: String,
    count: i64,
    token_estimate: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmTimelineCoverageV1 {
    limit: i64,
    returned_buckets: i64,
    total_dated_buckets: i64,
    truncated: bool,
    ordering: String,
    next_before_bucket: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmTimelinePayloadV1 {
    path: String,
    storage_scope: String,
    exists: bool,
    bucket: String,
    session_id: Option<String>,
    buckets: Vec<LcmTimelineBucketV1>,
    node_buckets: Vec<LcmTimelineNodeBucketV1>,
    undated: LcmTimelineUndatedV1,
    #[serde(default)]
    coverage: Option<LcmTimelineCoverageV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmTimelineNodeBucketV1 {
    bucket: Option<String>,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmTimelineUndatedV1 {
    count: i64,
    token_estimate: i64,
}

#[derive(Deserialize)]
pub struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

/// GET /api/plugins/hermes-lcm/overview
///
/// Transcript data is exposed only after the mounted temporal retrieval
/// authority hydrates it through the owning store's redaction authority.
pub async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(_params): JsonQuery<OverviewParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmOverviewPayloadV1>>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(&state),
        None,
        "lcm_aggregate_cursor_contract_unavailable",
    ))
}

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
    cursor: Option<String>,
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

/// GET /api/plugins/hermes-lcm/search
pub async fn search(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmSearchPayloadV1>>> {
    let since = parse_optional_i64(&params.since);
    let until = parse_optional_i64(&params.until);
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Search {
            query: params.q,
            limit: params.limit.unwrap_or(50).clamp(1, 500),
            offset: params.offset.unwrap_or(0).max(0),
            cursor: params.cursor,
            role: nonempty(params.role),
            source: nonempty(params.source),
            session_id: nonempty(params.session_id),
            since,
            until,
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct SessionParams {
    limit: Option<i64>,
    cursor: Option<String>,
}

/// GET /api/plugins/hermes-lcm/session/{session_id}
pub async fn session(
    State(state): State<DashboardState>,
    JsonPath(session_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<SessionParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmSessionPayloadV1>>> {
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Session {
            session_id,
            limit: params.limit.unwrap_or(100).clamp(1, 500),
            cursor: params.cursor,
        },
    )
    .await
}
#[derive(Deserialize)]
pub struct TimelineParams {
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    session_id: String,
    limit: Option<i64>,
}

/// GET /api/plugins/hermes-lcm/timeline
pub async fn timeline(
    State(state): State<DashboardState>,
    JsonQuery(_params): JsonQuery<TimelineParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmTimelinePayloadV1>>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(&state),
        None,
        "lcm_aggregate_cursor_contract_unavailable",
    ))
}

async fn lcm_read<T>(
    state: &DashboardState,
    request: DashboardLcmReadRequestV1,
) -> Json<DashboardEnvelopeV1<Option<T>>>
where
    T: serde::de::DeserializeOwned,
{
    let Some(authority) = state.lcm_read_authority.as_ref() else {
        return Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(state),
            None,
            "lcm_daemon_authority_unavailable",
        ));
    };
    match authority
        .read(state.project_id.as_deref(), request.clone())
        .await
    {
        DashboardLcmReadOutcomeV1::Ready(page) => {
            match serde_json::from_value(render_canonical_payload(request, page)) {
                Ok(payload) => Json(DashboardEnvelopeV1::ready(
                    scope_from_state(state),
                    DashboardCoverageV1::unknown(),
                    Some(payload),
                )),
                Err(_) => Json(DashboardEnvelopeV1::unavailable(
                    scope_from_state(state),
                    None,
                    "lcm_daemon_payload_invalid",
                )),
            }
        }
        DashboardLcmReadOutcomeV1::NotReady {
            state: read_state,
            reason,
        } => {
            let scope = scope_from_state(state);
            let envelope = match read_state {
                DashboardLcmReadStateV1::Absent => DashboardEnvelopeV1::complete_zero_findings(
                    scope,
                    DashboardCoverageV1::complete(0, "session records"),
                    None,
                ),
                DashboardLcmReadStateV1::Stale => {
                    let mut coverage = DashboardCoverageV1::unknown();
                    coverage.omission_reasons.push(reason);
                    DashboardEnvelopeV1::stale(scope, coverage, None)
                }
                DashboardLcmReadStateV1::Locked => DashboardEnvelopeV1::locked(scope, None, reason),
                DashboardLcmReadStateV1::Denied => DashboardEnvelopeV1::denied(scope, None),
                DashboardLcmReadStateV1::Redacted => {
                    DashboardEnvelopeV1::redacted(scope, None, reason)
                }
                DashboardLcmReadStateV1::Unavailable => {
                    DashboardEnvelopeV1::unavailable(scope, None, reason)
                }
            };
            Json(envelope)
        }
    }
}

fn render_canonical_payload(
    request: DashboardLcmReadRequestV1,
    page: DashboardLcmCanonicalPageV1,
) -> serde_json::Value {
    match request {
        DashboardLcmReadRequestV1::Search {
            query,
            limit,
            offset,
            cursor: _,
            role,
            source,
            session_id,
            since,
            until,
        } => {
            let messages = page
                .messages
                .into_iter()
                .map(message_json)
                .collect::<Vec<_>>();
            let summary_nodes = page
                .summary_nodes
                .into_iter()
                .map(summary_json)
                .collect::<Vec<_>>();
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": "project",
                "exists": true,
                "query": query,
                "limit": limit,
                "offset": offset,
                "next_cursor": page.next_cursor,
                "engine": "canonical_temporal",
                "engine_detail": {
                    "messages": "canonical_hydration",
                    "summary_nodes": "canonical_temporal_relations"
                },
                "total": {
                    "messages": messages.len(),
                    "summary_nodes": summary_nodes.len()
                },
                "filters": {
                    "role": role,
                    "source": source,
                    "session_id": session_id,
                    "since": since,
                    "until": until
                },
                "matches": {"messages": messages, "summary_nodes": summary_nodes},
            })
        }
        DashboardLcmReadRequestV1::Session {
            session_id,
            limit,
            cursor: _,
        } => {
            let messages = page
                .messages
                .into_iter()
                .map(message_json)
                .collect::<Vec<_>>();
            let summary_nodes = page
                .summary_nodes
                .into_iter()
                .map(summary_json)
                .collect::<Vec<_>>();
            let returned_summary_nodes = i64::try_from(summary_nodes.len()).unwrap_or(i64::MAX);
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": "project",
                "exists": page.stats.message_count > 0 || page.stats.summary_node_count > 0,
                "session_id": session_id,
                "limit": limit,
                "counts": {
                    "message_count": page.stats.message_count,
                    "summary_node_count": page.stats.summary_node_count,
                    "token_estimate_total": page.stats.source_token_count,
                    "summary_token_count": page.stats.summary_token_count,
                    "source_token_count": page.stats.source_token_count
                },
                "messages": messages,
                "summary_nodes": summary_nodes,
                "has_more": page.has_more,
                "has_more_messages": page.has_more,
                "has_more_summary_nodes": page.stats.summary_node_count > returned_summary_nodes,
                "next_cursor": page.next_cursor
            })
        }
    }
}

fn message_json(message: DashboardLcmCanonicalMessageV1) -> serde_json::Value {
    serde_json::json!({
        "store_id": null,
        "session_id": message.session_id,
        "role": message.role,
        "source": message.provider,
        "timestamp": message.timestamp,
        "token_estimate": token_estimate(&message.content),
        "content": message.content,
        "message_id": message.message_id,
        "ordinal": message.ordinal,
        "storage_kind": "canonical_temporal",
        "metadata_json": message.metadata_json,
        "tool_name": message.tool_names,
        "pinned": null,
        "summary_node_ids": [],
        "snippet": null
    })
}

fn summary_json(summary: DashboardLcmCanonicalSummaryV1) -> serde_json::Value {
    serde_json::json!({
        "node_id": summary.node_id,
        "session_id": summary.session_id,
        "depth": summary.depth,
        "category": "summary",
        "source_type": "canonical_temporal",
        "token_count": summary.token_count,
        "source_token_count": summary.source_token_count,
        "latest_at": summary.latest_at,
        "created_at": summary.created_at,
        "expand_hint": summary.expand_hint,
        "summary": summary.summary,
        "recency": summary.latest_at,
        "snippet": null
    })
}

fn token_estimate(content: &str) -> i64 {
    i64::try_from(content.chars().count().div_ceil(4)).unwrap_or(i64::MAX)
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_optional_i64(value: &str) -> Option<i64> {
    (!value.is_empty()).then(|| value.parse().ok()).flatten()
}
