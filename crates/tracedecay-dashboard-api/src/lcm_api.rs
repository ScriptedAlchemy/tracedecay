//! LCM dashboard API.
//!
//! All LCM reads use a daemon-owned temporal retrieval port. This crate never
//! opens or queries the session store, and therefore cannot bypass canonical
//! owning-store hydration or redaction.

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

mod aggregates;

#[derive(Clone, Debug)]
pub enum DashboardLcmReadRequestV1 {
    Overview {
        query: String,
        limit: i64,
    },
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
    Timeline {
        bucket: DashboardLcmTimelineBucketV1,
        session_id: Option<String>,
        limit: i64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardLcmTimelineBucketV1 {
    Hour,
    Day,
}

impl DashboardLcmTimelineBucketV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }
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
    pub overview_matches: Option<DashboardLcmCanonicalMatchesV1>,
    pub stats: DashboardLcmCanonicalStatsV1,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DashboardLcmCanonicalMatchesV1 {
    pub messages: Vec<DashboardLcmCanonicalMessageV1>,
    pub summary_nodes: Vec<DashboardLcmCanonicalSummaryV1>,
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
    summary_token_count: Option<i64>,
    source_token_count: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LcmTokenCountProvenanceV1 {
    O200kApproximate,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct LcmMessageV1 {
    pub(super) store_id: Option<i64>,
    pub(super) session_id: String,
    pub(super) role: Option<String>,
    pub(super) source: Option<String>,
    pub(super) timestamp: Option<i64>,
    pub(super) token_count: Option<i64>,
    pub(super) token_count_provenance: Option<LcmTokenCountProvenanceV1>,
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
    token_count: Option<i64>,
    token_count_provenance: LcmTokenCountProvenanceV1,
    known_message_count: i64,
    unknown_message_count: i64,
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
    token_count: Option<i64>,
    token_count_provenance: LcmTokenCountProvenanceV1,
    known_message_count: i64,
    unknown_message_count: i64,
}

#[derive(Deserialize)]
pub struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

/// GET /api/plugins/hermes-lcm/overview
///
/// The daemon authority freezes and traverses the canonical temporal result
/// manifest before this boundary reduces the hydrated records.
pub async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmOverviewPayloadV1>>> {
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Overview {
            query: params.q,
            limit: params.limit.unwrap_or(25).clamp(1, 200),
        },
    )
    .await
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
    JsonQuery(params): JsonQuery<TimelineParams>,
) -> Json<DashboardEnvelopeV1<Option<LcmTimelinePayloadV1>>> {
    let bucket = match params.bucket.trim().to_ascii_lowercase().as_str() {
        "" | "day" => DashboardLcmTimelineBucketV1::Day,
        "hour" => DashboardLcmTimelineBucketV1::Hour,
        _ => return invalid_lcm_request(&state),
    };
    lcm_read(
        &state,
        DashboardLcmReadRequestV1::Timeline {
            bucket,
            session_id: trimmed_nonempty(params.session_id),
            limit: params.limit.unwrap_or(400).clamp(1, 2_000),
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
    let outcome = authority
        .read(state.project_id.as_deref(), request.clone())
        .await;
    let scope = scope_from_state(state);
    match outcome {
        DashboardLcmReadOutcomeV1::Ready(page) => {
            let timeline_coverage = aggregates::timeline_view_coverage(&request, &page);
            let coverage = if aggregates::is_aggregate_request(&request) {
                DashboardCoverageV1::complete(
                    aggregates::returned_count(&page),
                    "canonical hydrated records",
                )
            } else {
                DashboardCoverageV1::unknown()
            };
            match aggregates::render_canonical_payload(request, page) {
                Ok(payload) => {
                    if let Some((eligible, examined, true)) = timeline_coverage {
                        Json(DashboardEnvelopeV1::partial(
                            scope,
                            eligible,
                            examined,
                            "timeline_buckets",
                            vec!["page_limit".to_owned()],
                            Some(payload),
                        ))
                    } else {
                        Json(DashboardEnvelopeV1::ready(scope, coverage, Some(payload)))
                    }
                }
                Err(()) => Json(DashboardEnvelopeV1::unavailable(
                    scope,
                    None,
                    "lcm_daemon_payload_invalid",
                )),
            }
        }
        DashboardLcmReadOutcomeV1::Partial { page, omitted } => {
            let examined = aggregates::returned_count(&page);
            let eligible = examined.saturating_add(omitted);
            match aggregates::render_canonical_payload(request, page) {
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
