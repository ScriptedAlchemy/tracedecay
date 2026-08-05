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
    Overview {
        query: String,
        limit: i64,
    },
    Search {
        query: String,
        limit: i64,
        offset: i64,
        role: Option<String>,
        source: Option<String>,
        session_id: Option<String>,
        since: Option<i64>,
        until: Option<i64>,
    },
    Session {
        session_id: String,
        limit: i64,
        offset: i64,
        order: String,
    },
    Timeline {
        bucket: String,
        session_id: Option<String>,
        limit: i64,
    },
}

pub enum DashboardLcmReadOutcomeV1 {
    Ready(DashboardLcmCanonicalPageV1),
    Unavailable { reason: String },
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
pub struct DashboardLcmCanonicalPageV1 {
    pub messages: Vec<DashboardLcmCanonicalMessageV1>,
    pub has_more: bool,
}

pub type DashboardLcmReadFutureV1<'a> =
    Pin<Box<dyn Future<Output = DashboardLcmReadOutcomeV1> + Send + 'a>>;

pub trait DashboardLcmReadPortV1: Send + Sync {
    fn read(&self, request: DashboardLcmReadRequestV1) -> DashboardLcmReadFutureV1<'_>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LcmSessionCountsV1 {
    message_count: i64,
    summary_node_count: i64,
    token_estimate_total: i64,
    summary_token_count: i64,
    source_token_count: i64,
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
    node_id: String,
    session_id: String,
    depth: i64,
    category: String,
    source_type: String,
    token_count: i64,
    source_token_count: i64,
    latest_at: Option<i64>,
    created_at: i64,
    expand_hint: String,
    summary: String,
    #[serde(default)]
    recency: Option<i64>,
    #[serde(default)]
    snippet: Option<String>,
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
    source_token_count: i64,
    token_count: i64,
    ratio: f64,
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
    offset: i64,
    order: String,
    counts: LcmSessionCountsV1,
    messages: Vec<LcmMessageV1>,
    summary_nodes: Vec<LcmSummaryNodeV1>,
    has_more: bool,
    has_more_messages: bool,
    has_more_summary_nodes: bool,
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
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmOverviewPayloadV1>>> {
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Overview {
            query: params.q,
            limit: params.limit.unwrap_or(50).clamp(1, 500),
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct SearchParams {
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
    offset: Option<i64>,
    #[serde(default)]
    order: String,
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
            offset: params.offset.unwrap_or(0).max(0),
            order: if params.order == "desc" {
                "desc".to_owned()
            } else {
                "asc".to_owned()
            },
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
    JsonQuery(params): JsonQuery<TimelineParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmTimelinePayloadV1>>> {
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Timeline {
            bucket: if params.bucket.is_empty() {
                "day".to_owned()
            } else {
                params.bucket
            },
            session_id: nonempty(params.session_id),
            limit: params.limit.unwrap_or(90).clamp(1, 500),
        },
    )
    .await
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
    match authority.read(request.clone()).await {
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
        DashboardLcmReadOutcomeV1::Unavailable { reason } => Json(
            DashboardEnvelopeV1::unavailable(scope_from_state(state), None, reason),
        ),
    }
}

fn render_canonical_payload(
    request: DashboardLcmReadRequestV1,
    page: DashboardLcmCanonicalPageV1,
) -> serde_json::Value {
    match request {
        DashboardLcmReadRequestV1::Overview { query, limit } => {
            overview_payload(page.messages, query, limit)
        }
        DashboardLcmReadRequestV1::Search {
            query,
            limit,
            offset,
            role,
            source,
            session_id,
            since,
            until,
        } => {
            let messages = page
                .messages
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .map(message_json)
                .collect::<Vec<_>>();
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": "project",
                "exists": true,
                "query": query,
                "limit": limit,
                "offset": offset,
                "engine": "canonical_temporal",
                "engine_detail": {
                    "messages": "canonical_hydration",
                    "summary_nodes": "canonical_temporal_relations"
                },
                "total": {
                    "messages": messages.len(),
                    "summary_nodes": 0
                },
                "filters": {
                    "role": role,
                    "source": source,
                    "session_id": session_id,
                    "since": since,
                    "until": until
                },
                "matches": {"messages": messages, "summary_nodes": []},
            })
        }
        DashboardLcmReadRequestV1::Session {
            session_id,
            limit,
            offset,
            order,
        } => {
            let mut messages = page
                .messages
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .collect::<Vec<_>>();
            if order == "desc" {
                messages.reverse();
            }
            let token_estimate_total = messages
                .iter()
                .map(|message| token_estimate(&message.content))
                .sum::<i64>();
            let messages = messages.into_iter().map(message_json).collect::<Vec<_>>();
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": "project",
                "exists": !messages.is_empty(),
                "session_id": session_id,
                "limit": limit,
                "offset": offset,
                "order": order,
                "counts": {
                    "message_count": messages.len(),
                    "summary_node_count": 0,
                    "token_estimate_total": token_estimate_total,
                    "summary_token_count": 0,
                    "source_token_count": token_estimate_total
                },
                "messages": messages,
                "summary_nodes": [],
                "has_more": page.has_more,
                "has_more_messages": page.has_more,
                "has_more_summary_nodes": false
            })
        }
        DashboardLcmReadRequestV1::Timeline {
            bucket,
            session_id,
            limit,
        } => timeline_payload(page.messages, bucket, session_id, limit),
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

fn token_estimate(content: &str) -> i64 {
    i64::try_from(content.chars().count().div_ceil(4)).unwrap_or(i64::MAX)
}

fn overview_payload(
    messages: Vec<DashboardLcmCanonicalMessageV1>,
    query: String,
    limit: i64,
) -> serde_json::Value {
    let mut sessions = std::collections::BTreeMap::<String, (i64, Option<i64>)>::new();
    let mut roles = std::collections::BTreeMap::<String, i64>::new();
    let mut sources = std::collections::BTreeMap::<String, i64>::new();
    let mut token_total = 0i64;
    for message in &messages {
        let session = sessions
            .entry(message.session_id.clone())
            .or_insert((0, None));
        session.0 = session.0.saturating_add(1);
        session.1 = session.1.max(message.timestamp);
        *roles.entry(message.role.clone()).or_default() += 1;
        *sources.entry(message.provider.clone()).or_default() += 1;
        token_total = token_total.saturating_add(token_estimate(&message.content));
    }
    let latest_sessions = sessions
        .into_iter()
        .map(|(session_id, (message_count, last_timestamp))| {
            serde_json::json!({
                "session_id": session_id,
                "message_count": message_count,
                "last_store_id": null,
                "last_timestamp": last_timestamp
            })
        })
        .collect::<Vec<_>>();
    let messages = messages.into_iter().map(message_json).collect::<Vec<_>>();
    serde_json::json!({
        "path": "daemon://session-temporal",
        "storage_scope": "project",
        "exists": true,
        "overview": {
            "messages_total": messages.len(),
            "sessions_total": latest_sessions.len(),
            "summary_nodes_total": 0,
            "summary_node_sessions_total": 0,
            "max_summary_depth": 0,
            "role_counts": roles.into_iter().map(|(role, count)| serde_json::json!({"role": role, "count": count})).collect::<Vec<_>>(),
            "source_counts": sources.into_iter().map(|(source, count)| serde_json::json!({"source": source, "count": count})).collect::<Vec<_>>(),
            "depth_counts": [],
            "compression": {
                "source_token_count": token_total,
                "token_count": 0,
                "ratio": 0.0,
                "node_count": 0
            }
        },
        "latest_sessions": latest_sessions,
        "latest_summary_nodes": [],
        "matches": {"messages": messages, "summary_nodes": []},
        "query": query,
        "limit": limit
    })
}

fn timeline_payload(
    messages: Vec<DashboardLcmCanonicalMessageV1>,
    bucket: String,
    session_id: Option<String>,
    limit: i64,
) -> serde_json::Value {
    let mut buckets = std::collections::BTreeMap::<String, (i64, i64)>::new();
    let mut undated = (0i64, 0i64);
    for message in messages {
        let tokens = token_estimate(&message.content);
        if let Some(timestamp) = message.timestamp {
            let key = timestamp_bucket(timestamp, &bucket);
            let entry = buckets.entry(key).or_default();
            entry.0 = entry.0.saturating_add(1);
            entry.1 = entry.1.saturating_add(tokens);
        } else {
            undated.0 = undated.0.saturating_add(1);
            undated.1 = undated.1.saturating_add(tokens);
        }
    }
    let total_dated_buckets = i64::try_from(buckets.len()).unwrap_or(i64::MAX);
    let buckets = buckets
        .into_iter()
        .rev()
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|(bucket, (count, token_estimate))| {
            serde_json::json!({
                "bucket": bucket,
                "count": count,
                "token_estimate": token_estimate
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "path": "daemon://session-temporal",
        "storage_scope": "project",
        "exists": true,
        "bucket": bucket,
        "session_id": session_id,
        "buckets": buckets,
        "node_buckets": [],
        "undated": {"count": undated.0, "token_estimate": undated.1},
        "coverage": {
            "limit": limit,
            "returned_buckets": buckets.len(),
            "total_dated_buckets": total_dated_buckets,
            "truncated": total_dated_buckets > limit,
            "ordering": "descending",
            "next_before_bucket": null
        }
    })
}

fn timestamp_bucket(timestamp: i64, grain: &str) -> String {
    let seconds = if timestamp.abs() > 10_000_000_000_000 {
        timestamp / 1_000_000
    } else if timestamp.abs() > 10_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    };
    let bucket_seconds = match grain {
        "hour" => 60 * 60,
        "month" => 30 * 24 * 60 * 60,
        _ => 24 * 60 * 60,
    };
    seconds
        .div_euclid(bucket_seconds)
        .saturating_mul(bucket_seconds)
        .to_string()
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_optional_i64(value: &str) -> Option<i64> {
    (!value.is_empty()).then(|| value.parse().ok()).flatten()
}
