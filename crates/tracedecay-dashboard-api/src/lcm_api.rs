//! LCM dashboard API.
//!
//! Search and session reads use a daemon-owned temporal retrieval port. This
//! crate never opens or queries the session store, and therefore cannot bypass
//! canonical owning-store hydration or redaction.

use std::future::Future;
use std::pin::Pin;

use axum::{Json, extract::State};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    scope_from_state,
};
use super::util::{JsonPath, JsonQuery};

#[derive(Clone, Debug)]
pub enum DashboardLcmReadRequestV1 {
    Search {
        query: String,
        limit: i64,
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
    Partial {
        page: DashboardLcmCanonicalPageV1,
        omitted: u64,
    },
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
    store_id: Option<i64>,
    session_id: String,
    role: Option<String>,
    source: Option<String>,
    timestamp: Option<i64>,
    token_estimate: Option<i64>,
    content: Option<String>,
    message_id: String,
    ordinal: Option<i64>,
    storage_kind: Option<String>,
    metadata_json: Option<String>,
    tool_name: Option<String>,
    pinned: Option<i64>,
    summary_node_ids: Vec<String>,
    #[serde(default)]
    snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmSummaryNodeV1 {
    node_id: String,
    session_id: String,
    depth: i64,
    category: String,
    source_type: String,
    token_count: Option<i64>,
    source_token_count: Option<i64>,
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
/// Transcript data is only safe to expose after temporal retrieval hydrates it
/// through the owning store's redaction authority. That service is not mounted
/// on this dashboard floor, so this route stays explicitly unavailable rather
/// than reading raw LCM tables directly.
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
    let since = match parse_optional_i64(&params.since) {
        Ok(since) => since,
        Err(()) => return invalid_lcm_request(&state),
    };
    let until = match parse_optional_i64(&params.until) {
        Ok(until) => until,
        Err(()) => return invalid_lcm_request(&state),
    };
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Search {
            query: params.q,
            limit: match params.limit {
                Some(limit) => limit,
                None => 50,
            }
            .clamp(1, 500),
            cursor: params.cursor,
            role: trimmed_nonempty(params.role),
            source: trimmed_nonempty(params.source),
            session_id: trimmed_nonempty(params.session_id),
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
            limit: match params.limit {
                Some(limit) => limit,
                None => 100,
            }
            .clamp(1, 500),
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
    let outcome = authority
        .read(state.project_id.as_deref(), request.clone())
        .await;
    let scope = scope_from_state(state);
    match outcome {
        DashboardLcmReadOutcomeV1::Ready(page) => match render_canonical_payload(request, page) {
            Ok(payload) => Json(DashboardEnvelopeV1::ready(
                scope,
                DashboardCoverageV1::unknown(),
                Some(payload),
            )),
            Err(()) => Json(DashboardEnvelopeV1::unavailable(
                scope,
                None,
                "lcm_daemon_payload_invalid",
            )),
        },
        DashboardLcmReadOutcomeV1::Partial { page, omitted } => {
            let examined = returned_count(&page);
            let eligible = examined.saturating_add(omitted);
            match render_canonical_payload(request, page) {
                Ok(payload) => Json(DashboardEnvelopeV1::partial(
                    scope,
                    eligible,
                    examined,
                    "canonical hydrated records",
                    vec!["lcm_temporal_read_incomplete".to_owned()],
                    Some(payload),
                )),
                Err(()) => Json(DashboardEnvelopeV1::unavailable(
                    scope,
                    None,
                    "lcm_daemon_payload_invalid",
                )),
            }
        }
        DashboardLcmReadOutcomeV1::NotReady {
            state: read_state,
            reason,
        } => {
            let envelope = match read_state {
                DashboardLcmReadStateV1::Absent => DashboardEnvelopeV1::complete_zero_findings(
                    scope,
                    DashboardCoverageV1::complete(0, "canonical hydrated records"),
                    None,
                ),
                DashboardLcmReadStateV1::Stale => {
                    let mut coverage = DashboardCoverageV1::unknown();
                    coverage.omission_reasons.push(reason);
                    DashboardEnvelopeV1::stale(scope, coverage, None)
                }
                DashboardLcmReadStateV1::Locked => {
                    typed_not_ready_envelope(scope, DashboardDomainStateV1::Locked, reason)
                }
                DashboardLcmReadStateV1::Denied => DashboardEnvelopeV1::denied(scope, None),
                DashboardLcmReadStateV1::Redacted => {
                    typed_not_ready_envelope(scope, DashboardDomainStateV1::Redacted, reason)
                }
                DashboardLcmReadStateV1::Unavailable => {
                    DashboardEnvelopeV1::unavailable(scope, None, reason)
                }
            };
            Json(envelope)
        }
    }
}

fn typed_not_ready_envelope<T>(
    scope: super::read_model::DashboardScopeV1,
    state: DashboardDomainStateV1,
    reason: String,
) -> DashboardEnvelopeV1<Option<T>> {
    let mut coverage = DashboardCoverageV1::unknown();
    coverage.omission_reasons.push(reason);
    DashboardEnvelopeV1::new(
        scope,
        state,
        coverage,
        DashboardFreshnessV1::unknown(),
        None,
    )
}

fn render_canonical_payload<T>(
    request: DashboardLcmReadRequestV1,
    page: DashboardLcmCanonicalPageV1,
) -> Result<T, ()>
where
    T: serde::de::DeserializeOwned,
{
    let value = match request {
        DashboardLcmReadRequestV1::Search {
            query,
            limit,
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
            let returned_summary_nodes = saturating_usize_to_i64(summary_nodes.len());
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
    };
    serde_json::from_value(value).map_err(|_| ())
}

fn returned_count(page: &DashboardLcmCanonicalPageV1) -> u64 {
    match u64::try_from(page.messages.len().saturating_add(page.summary_nodes.len())) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

fn message_json(message: DashboardLcmCanonicalMessageV1) -> serde_json::Value {
    serde_json::json!({
        "store_id": null,
        "session_id": message.session_id,
        "role": message.role,
        "source": message.provider,
        "timestamp": message.timestamp,
        "token_estimate": null,
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

fn saturating_usize_to_i64(value: usize) -> i64 {
    match i64::try_from(value) {
        Ok(value) => value,
        Err(_) => i64::MAX,
    }
}

fn invalid_lcm_request<T>(state: &DashboardState) -> Json<DashboardEnvelopeV1<Option<T>>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(state),
        None,
        "lcm_dashboard_request_invalid",
    ))
}

fn trimmed_nonempty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_optional_i64(value: &str) -> Result<Option<i64>, ()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse().map(Some).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_message_does_not_fabricate_a_token_estimate() {
        let message = message_json(DashboardLcmCanonicalMessageV1 {
            session_id: "session.message".to_owned(),
            provider: "codex".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1),
            ordinal: 1,
            content: "content whose tokenizer is unknown".to_owned(),
            message_id: "message.one".to_owned(),
            metadata_json: None,
            tool_names: None,
        });

        assert!(message["token_estimate"].is_null());
    }

    #[test]
    fn optional_search_filters_are_trimmed_and_invalid_times_are_rejected() {
        assert_eq!(
            trimmed_nonempty("  assistant  ".to_owned()).as_deref(),
            Some("assistant")
        );
        assert_eq!(trimmed_nonempty(" \t ".to_owned()), None);
        assert_eq!(parse_optional_i64(" 42 "), Ok(Some(42)));
        assert_eq!(parse_optional_i64(" \t "), Ok(None));
        assert_eq!(parse_optional_i64("tomorrow"), Err(()));
    }
}
