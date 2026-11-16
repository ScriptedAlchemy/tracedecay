//! Request-owned Explorer source stream and session-context HTTP binding.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{self, FuturesUnordered, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod knowledge;
mod lifecycle;

use lifecycle::admitted_deadline_elapsed;

use super::lcm_api::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadOutcomeV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1, LcmMessageV1, LcmSummaryNodeV1, LcmTokenCountProvenanceV1,
    message_tool_use_id,
};
use super::read_model::{
    DashboardCoverageCompletenessV1, DashboardCoverageV1, DashboardEnvelopeV1, now_micros,
    scope_from_state,
};
use super::util::json_error;
use super::{DashboardHttpRequestControlV1, DashboardState, RequestControl, graph_service};
use tracedecay_runtime_core::cancellation::CancellationToken;

const SOURCE_IDS: [ExplorerSourceIdV1; 3] = [
    ExplorerSourceIdV1::CodeGraph,
    ExplorerSourceIdV1::Sessions,
    ExplorerSourceIdV1::Knowledge,
];
const QUERY_LIMIT_DEFAULT: i64 = 25;
const QUERY_LIMIT_MAX: i64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplorerQueryRequestV1 {
    query: String,
    #[serde(default = "default_query_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

const fn default_query_limit() -> i64 {
    QUERY_LIMIT_DEFAULT
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerSourceIdV1 {
    CodeGraph,
    Sessions,
    Knowledge,
}

impl ExplorerSourceIdV1 {
    const fn label(self) -> &'static str {
        match self {
            Self::CodeGraph => "Code graph",
            Self::Sessions => "Sessions",
            Self::Knowledge => "Knowledge",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerRunStateV1 {
    Completed,
    Partial,
    Cancelled,
    TimedOut,
    Error,
}

/// What one source truthfully concluded for this run. Every member has a real
/// producer; none is speculative:
/// - `Partial`: the source answered with rows but its own read reported
///   omitted records (LCM temporal reads).
/// - `Stale`: the source's store exists but does not match the current
///   generation (verified graph stale reads, LCM stale projections).
/// - `TimedOut`: the source's own read exceeded the admitted deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerSourceOutcomeV1 {
    Ready,
    Partial,
    Stale,
    TimedOut,
    Unavailable,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ExplorerResultPageV1 {
    offset: i64,
    limit: i64,
    total: Option<u64>,
    next_offset: Option<i64>,
    rows: Vec<Value>,
    metadata: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ExplorerSourceProgressV1 {
    source_id: ExplorerSourceIdV1,
    source_label: &'static str,
    outcome: ExplorerSourceOutcomeV1,
    completed_units: Option<u64>,
    total_units: Option<u64>,
    coverage: DashboardCoverageV1,
    freshness: &'static str,
    watermark: Option<String>,
    error_code: Option<&'static str>,
    message: Option<String>,
    page: Option<ExplorerResultPageV1>,
}

impl ExplorerSourceProgressV1 {
    fn unavailable(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_id,
            source_label: source_id.label(),
            outcome: ExplorerSourceOutcomeV1::Unavailable,
            completed_units: None,
            total_units: None,
            coverage: DashboardCoverageV1::unknown(),
            freshness: "unknown",
            watermark: None,
            error_code: Some(error_code),
            message: Some(message.into()),
            page: None,
        }
    }

    fn error(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Error,
            ..Self::unavailable(source_id, error_code, message)
        }
    }

    fn stale(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Stale,
            ..Self::unavailable(source_id, error_code, message)
        }
    }

    fn timed_out(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::TimedOut,
            ..Self::unavailable(source_id, error_code, message)
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ExplorerQueryEventV1 {
    Source {
        source: Box<ExplorerSourceProgressV1>,
    },
    Final {
        state: ExplorerRunStateV1,
        elapsed_micros: i64,
    },
}

fn bad_request(message: impl Into<String>) -> Response {
    json_error(StatusCode::BAD_REQUEST, message).into_response()
}

fn validate_query(request: &mut ExplorerQueryRequestV1) -> Result<(), &'static str> {
    request.query = request.query.trim().to_owned();
    if request.query.is_empty() {
        return Err("query must not be empty");
    }
    if !(1..=QUERY_LIMIT_MAX).contains(&request.limit) {
        return Err("limit must be between 1 and 100");
    }
    if request.offset != 0 {
        return Err("offset must be zero until every required source exposes a cursor");
    }
    Ok(())
}

struct ExplorerStream {
    control: DashboardHttpRequestControlV1,
    cancellation: CancellationToken,
    tasks: FuturesUnordered<BoxFuture<'static, ExplorerSourceProgressV1>>,
    sources: Vec<ExplorerSourceProgressV1>,
    frames: VecDeque<ExplorerQueryEventV1>,
    started_at: i64,
    terminal: bool,
}

impl Drop for ExplorerStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let _ = self
            .control
            .cancellation()
            .cancel(tracedecay_session_memory::context::application_observed_at());
    }
}

impl ExplorerStream {
    fn finish(&mut self, state: ExplorerRunStateV1) {
        if self.terminal {
            return;
        }
        if matches!(
            state,
            ExplorerRunStateV1::TimedOut | ExplorerRunStateV1::Cancelled
        ) {
            for source_id in SOURCE_IDS {
                if self
                    .sources
                    .iter()
                    .any(|source| source.source_id == source_id)
                {
                    continue;
                }
                let source = match state {
                    ExplorerRunStateV1::TimedOut => ExplorerSourceProgressV1::timed_out(
                        source_id,
                        "request_deadline_elapsed",
                        "dashboard request deadline elapsed",
                    ),
                    _ => ExplorerSourceProgressV1::cancelled(
                        source_id,
                        "request_cancelled",
                        "dashboard request was cancelled",
                    ),
                };
                self.frames
                    .push_back(ExplorerQueryEventV1::Source {
                        source: Box::new(source),
                    });
            }
        }
        self.frames.push_back(ExplorerQueryEventV1::Final {
            state,
            elapsed_micros: now_micros().saturating_sub(self.started_at),
        });
        self.terminal = true;
    }

    fn finish_from_sources(&mut self) {
        let complete = self.sources.len() == SOURCE_IDS.len()
            && self.sources.iter().all(|source| {
                source.outcome == ExplorerSourceOutcomeV1::Ready
                    && source.coverage.completeness == DashboardCoverageCompletenessV1::Complete
            });
        let state = if complete {
            ExplorerRunStateV1::Completed
        } else if self.sources.iter().any(|source| {
            matches!(
                source.outcome,
                ExplorerSourceOutcomeV1::Ready | ExplorerSourceOutcomeV1::Partial
            )
        }) {
            ExplorerRunStateV1::Partial
        } else {
            ExplorerRunStateV1::Error
        };
        self.finish(state);
    }
}

fn frame(event: ExplorerQueryEventV1) -> Result<Event, std::io::Error> {
    serde_json::to_string(&event)
        .map(|data| Event::default().data(data))
        .map_err(std::io::Error::other)
}

#[tracing::instrument(name = "dashboard_api.query.stream", level = "trace", skip_all)]
pub async fn stream_query(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    Json(mut request): Json<ExplorerQueryRequestV1>,
) -> Response {
    if let Err(message) = validate_query(&mut request) {
        return bad_request(message);
    }
    if state.resolved_scope.is_none() {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "exact registered project scope is unavailable",
        )
        .into_response();
    }
    let cancellation = CancellationToken::new();
    let tasks: FuturesUnordered<BoxFuture<'static, ExplorerSourceProgressV1>> = SOURCE_IDS
        .into_iter()
        .map(|source_id| {
            let read = execute_source(
                state.clone(),
                request.clone(),
                source_id,
                control.clone(),
                cancellation.clone(),
            );
            AssertUnwindSafe(read)
                .catch_unwind()
                .map(move |outcome| {
                    outcome.unwrap_or_else(|_| {
                        ExplorerSourceProgressV1::error(
                            source_id,
                            "source_task_failed",
                            "explorer source read panicked",
                        )
                    })
                })
                .boxed()
        })
        .collect();
    let stream_state = ExplorerStream {
        control,
        cancellation,
        tasks,
        sources: Vec::with_capacity(SOURCE_IDS.len()),
        frames: VecDeque::new(),
        started_at: now_micros(),
        terminal: false,
    };
    Sse::new(stream::unfold(stream_state, |mut stream| async move {
        if let Some(event) = stream.frames.pop_front() {
            return Some((frame(event), stream));
        }
        if stream.terminal {
            return None;
        }
        let admitted = stream.control.cancellation().clone();
        let deadline = stream.control.deadline();
        tokio::select! {
            biased;
            () = admitted_deadline_elapsed(deadline) => stream.finish(ExplorerRunStateV1::TimedOut),
            () = admitted.cancelled() => stream.finish(ExplorerRunStateV1::Cancelled),
            source = stream.tasks.next() => {
                if let Some(source) = source {
                    stream.sources.push(source.clone());
                    stream.frames.push_back(ExplorerQueryEventV1::Source {
                        source: Box::new(source),
                    });
                    if stream.sources.len() == SOURCE_IDS.len() {
                        stream.finish_from_sources();
                    }
                } else {
                    stream.finish_from_sources();
                }
            }
        }
        stream
            .frames
            .pop_front()
            .map(|event| (frame(event), stream))
    }))
    .into_response()
}

async fn execute_source(
    state: DashboardState,
    request: ExplorerQueryRequestV1,
    source_id: ExplorerSourceIdV1,
    control: DashboardHttpRequestControlV1,
    cancellation: CancellationToken,
) -> ExplorerSourceProgressV1 {
    match source_id {
        ExplorerSourceIdV1::CodeGraph => code_source(&state, &request, &control).await,
        ExplorerSourceIdV1::Sessions => session_source(&state, &request, control).await,
        ExplorerSourceIdV1::Knowledge => {
            knowledge::knowledge_source(&state, &request, &control, cancellation).await
        }
    }
}

async fn code_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
    control: &DashboardHttpRequestControlV1,
) -> ExplorerSourceProgressV1 {
    let read = match graph_service::search_payload(
        state,
        control,
        &request.query,
        request.limit,
        request.offset,
    )
    .await
    {
        Ok(read) => read,
        Err(error) => {
            return code_graph_error(error);
        }
    };
    let payload = read.payload;
    let total = payload.total;
    let omission_reasons = if payload.has_more && total.is_none() {
        vec!["query page limit".to_owned()]
    } else {
        Vec::new()
    };
    let rows = match payload
        .results
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(rows) => rows,
        Err(error) => {
            return ExplorerSourceProgressV1::error(
                ExplorerSourceIdV1::CodeGraph,
                "code_graph_contract_invalid",
                error.to_string(),
            );
        }
    };
    let mut source = ready_source(
        ExplorerSourceIdV1::CodeGraph,
        request,
        rows,
        total,
        json!({"query": request.query}),
        "symbols",
        omission_reasons,
    );
    source.freshness = "fresh";
    source.watermark = Some(read.generation);
    source
}

fn code_graph_error(error: crate::graph::CodeGraphReadError) -> ExplorerSourceProgressV1 {
    use crate::graph::CodeGraphReadError;

    match error {
        CodeGraphReadError::MissingRegistry => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "missing_registry",
            "the exact project graph registry is missing",
        ),
        CodeGraphReadError::Unavailable { detail } | CodeGraphReadError::Refused { detail } => {
            ExplorerSourceProgressV1::unavailable(
                ExplorerSourceIdV1::CodeGraph,
                "graph_authority_unavailable",
                detail,
            )
        }
        CodeGraphReadError::Stale { detail } => ExplorerSourceProgressV1::stale(
            ExplorerSourceIdV1::CodeGraph,
            "graph_generation_stale",
            detail,
        ),
        CodeGraphReadError::Cancelled => ExplorerSourceProgressV1::cancelled(
            ExplorerSourceIdV1::CodeGraph,
            "graph_read_cancelled",
            "the graph read was cancelled",
        ),
        CodeGraphReadError::TimedOut => ExplorerSourceProgressV1::timed_out(
            ExplorerSourceIdV1::CodeGraph,
            "graph_read_timed_out",
            "the graph read timed out",
        ),
        CodeGraphReadError::Denied => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "graph_read_denied",
            "the graph read is not authorized",
        ),
        CodeGraphReadError::InvalidRequest { detail } => ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::CodeGraph,
            "graph_request_invalid",
            detail,
        ),
        CodeGraphReadError::Corrupt { detail } => ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::CodeGraph,
            "verified_graph_corrupt",
            detail,
        ),
        CodeGraphReadError::ResetRequired { detail } => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "graph_reset_required",
            detail,
        ),
        CodeGraphReadError::BudgetExhausted { detail } => ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::CodeGraph,
            "graph_budget_exhausted",
            detail,
        ),
        parked @ CodeGraphReadError::Parked { .. } => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "code_index_parked",
            parked.to_string(),
        ),
        rewarming @ CodeGraphReadError::Rewarming { .. } => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "graph_authority_unavailable",
            rewarming.to_string(),
        ),
    }
}

async fn session_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
    control: DashboardHttpRequestControlV1,
) -> ExplorerSourceProgressV1 {
    let Some(authority) = state.lcm_read_authority.as_ref() else {
        return ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::Sessions,
            "lcm_daemon_authority_unavailable",
            "daemon LCM retrieval authority is unavailable",
        );
    };
    if request.offset != 0 {
        // Temporal session reads paginate with the opaque daemon cursor only;
        // there is no offset protocol to honor truthfully.
        return ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::Sessions,
            "lcm_cursor_required",
            "session pagination requires the opaque temporal cursor",
        );
    }
    match authority
        .read(
            control,
            state.project_id.as_deref(),
            DashboardLcmReadRequestV1::Search {
                query: request.query.clone(),
                limit: request.limit,
                cursor: None,
                role: None,
                source: None,
                session_id: None,
                since: None,
                until: None,
            },
        )
        .await
    {
        DashboardLcmReadOutcomeV1::Ready(page) => {
            explorer_session_rows(request, page, Vec::new(), false)
        }
        DashboardLcmReadOutcomeV1::Partial { page, omitted } => explorer_session_rows(
            request,
            page,
            vec![format!("lcm_temporal_read_incomplete:{omitted}")],
            true,
        ),
        DashboardLcmReadOutcomeV1::NotReady {
            state: DashboardLcmReadStateV1::Absent,
            ..
        } => ready_source(
            ExplorerSourceIdV1::Sessions,
            request,
            Vec::new(),
            Some(0),
            json!({"query": request.query, "authority": "canonical_temporal"}),
            "messages",
            Vec::new(),
        ),
        DashboardLcmReadOutcomeV1::NotReady {
            state: state @ DashboardLcmReadStateV1::Stale,
            reason,
        } => ExplorerSourceProgressV1::stale(
            ExplorerSourceIdV1::Sessions,
            explorer_lcm_error_code(state),
            format!("canonical temporal retrieval did not produce a page: {reason}"),
        ),
        DashboardLcmReadOutcomeV1::NotReady { state, reason } => {
            ExplorerSourceProgressV1::unavailable(
                ExplorerSourceIdV1::Sessions,
                explorer_lcm_error_code(state),
                format!("canonical temporal retrieval did not produce a page: {reason}"),
            )
        }
    }
}

fn ready_source(
    source_id: ExplorerSourceIdV1,
    request: &ExplorerQueryRequestV1,
    rows: Vec<Value>,
    total: Option<u64>,
    metadata: Value,
    unit: &'static str,
    omission_reasons: Vec<String>,
) -> ExplorerSourceProgressV1 {
    let completed = rows.len() as u64;
    let coverage = total.map_or_else(
        || {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage.examined = Some(completed);
            coverage.unit = Some(unit.to_owned());
            coverage.omission_reasons = omission_reasons;
            coverage
        },
        |eligible| {
            if completed >= eligible {
                DashboardCoverageV1::complete(eligible, unit)
            } else {
                DashboardCoverageV1::partial(
                    eligible,
                    completed,
                    unit,
                    vec!["query page limit".to_owned()],
                )
            }
        },
    );
    ExplorerSourceProgressV1 {
        source_id,
        source_label: source_id.label(),
        outcome: ExplorerSourceOutcomeV1::Ready,
        completed_units: Some(completed),
        total_units: total,
        coverage,
        freshness: "unknown",
        watermark: None,
        error_code: None,
        message: None,
        page: Some(ExplorerResultPageV1 {
            offset: request.offset,
            limit: request.limit,
            total,
            next_offset: None,
            rows,
            metadata,
        }),
    }
}

#[derive(Debug, Deserialize)]
pub struct ReadContextParams {
    limit: Option<i64>,
    offset: Option<i64>,
    order: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ExplorerSessionCountsV1 {
    message_count: i64,
    unpublished_message_count: i64,
    summary_node_count: i64,
    summary_token_count: Option<i64>,
    source_token_count: Option<i64>,
    /// Complete session token estimate from the LCM size authority, or
    /// typed-absent when the bounded estimate did not cover the session.
    token_estimate_total: Option<i64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ExplorerSessionSizeV1 {
    session_id: String,
    storage_scope: String,
    counts: ExplorerSessionCountsV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ExplorerReadContextV1 {
    session_id: String,
    storage_scope: String,
    limit: i64,
    offset: i64,
    order: String,
    counts: ExplorerSessionCountsV1,
    messages: Vec<LcmMessageV1>,
    summary_nodes: Vec<LcmSummaryNodeV1>,
    has_more: bool,
    has_more_messages: bool,
    has_more_summary_nodes: bool,
}

pub async fn session_size(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    Path(session_id): Path<String>,
) -> Response {
    tracing::Instrument::instrument(
        async move {
            let outcome = read_session_page(&state, control, &session_id, 500, None).await;
            match outcome {
                DashboardLcmReadOutcomeV1::Ready(page) => {
                    let payload = ExplorerSessionSizeV1 {
                        session_id,
                        storage_scope: "project".to_owned(),
                        counts: explorer_session_counts(&page.stats),
                    };
                    Json(DashboardEnvelopeV1::ready(
                        scope_from_state(&state),
                        DashboardCoverageV1::unknown(),
                        Some(payload),
                    ))
                    .into_response()
                }
                DashboardLcmReadOutcomeV1::Partial { page, omitted } => {
                    let examined = u64::try_from(page.messages.len()).unwrap_or(u64::MAX);
                    let payload = ExplorerSessionSizeV1 {
                        session_id,
                        storage_scope: "project".to_owned(),
                        counts: explorer_session_counts(&page.stats),
                    };
                    Json(DashboardEnvelopeV1::partial(
                        scope_from_state(&state),
                        examined.saturating_add(omitted),
                        examined,
                        "canonical hydrated records",
                        vec!["lcm_temporal_read_incomplete".to_owned()],
                        Some(payload),
                    ))
                    .into_response()
                }
                DashboardLcmReadOutcomeV1::NotReady {
                    state: read_state,
                    reason,
                } => {
                    explorer_session_not_ready::<ExplorerSessionSizeV1>(&state, read_state, reason)
                }
            }
        },
        tracing::trace_span!("dashboard_api.query.session_size"),
    )
    .await
}

pub async fn read_context(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    Path(session_id): Path<String>,
    Query(params): Query<ReadContextParams>,
) -> Response {
    tracing::Instrument::instrument(
        async move {
            let limit = params.limit.unwrap_or(100).clamp(1, 500);
            let offset = params.offset.unwrap_or(0).max(0);
            let order = if params.order.as_deref() == Some("desc") {
                "desc"
            } else {
                "asc"
            };
            if offset != 0 || order == "desc" {
                return Json(DashboardEnvelopeV1::unavailable(
                    scope_from_state(&state),
                    None::<ExplorerReadContextV1>,
                    "lcm_cursor_required",
                ))
                .into_response();
            }
            // The kernel serves one ranked stream in which summary nodes ride beside
            // messages, so a windowed read fills its MESSAGE quota by consuming
            // continuation pages: summaries accumulate alongside without starving the
            // caller's limit, and the fill stays bounded so a read never becomes
            // unbounded background work.
            const READ_CONTEXT_FILL_PAGES: usize = 8;
            let mut messages: Vec<DashboardLcmCanonicalMessageV1> = Vec::new();
            let mut summary_nodes = Vec::new();
            let mut stats = None;
            let mut window_partial = false;
            let mut omitted_total = 0_u64;
            let mut cursor: Option<String> = None;
            for _ in 0..READ_CONTEXT_FILL_PAGES {
                let outcome =
                    read_session_page(&state, control.clone(), &session_id, limit, cursor.take())
                        .await;
                let (mut page, page_partial, page_omitted) = match outcome {
                    DashboardLcmReadOutcomeV1::Ready(page) => (page, false, 0),
                    DashboardLcmReadOutcomeV1::Partial { page, omitted } => (page, true, omitted),
                    DashboardLcmReadOutcomeV1::NotReady {
                        state: read_state,
                        reason,
                    } => {
                        return explorer_session_not_ready::<ExplorerReadContextV1>(
                            &state, read_state, reason,
                        );
                    }
                };
                window_partial |= page_partial;
                omitted_total = omitted_total.saturating_add(page_omitted);
                summary_nodes.append(&mut page.summary_nodes);
                let deficit = usize::try_from(limit).unwrap_or(usize::MAX) - messages.len();
                let overflow = page.messages.len() > deficit;
                messages.extend(page.messages.drain(..).take(deficit));
                stats = Some(page.stats);
                cursor = page.next_cursor;
                if overflow || messages.len() >= usize::try_from(limit).unwrap_or(usize::MAX) {
                    break;
                }
                if cursor.is_none() {
                    break;
                }
            }
            let stats = stats.unwrap_or_default();
            let counts = explorer_session_counts(&stats);
            let returned_messages = i64::try_from(messages.len()).unwrap_or(i64::MAX);
            let returned_summary_nodes = i64::try_from(summary_nodes.len()).unwrap_or(i64::MAX);
            let has_more_messages =
                stats.message_count - stats.unpublished_message_count > returned_messages;
            let has_more_summary_nodes = stats.summary_node_count > returned_summary_nodes;
            let examined = u64::try_from(messages.len()).unwrap_or(u64::MAX);
            let has_more = has_more_messages || has_more_summary_nodes;
            let payload = ExplorerReadContextV1 {
                session_id,
                storage_scope: "project".to_owned(),
                limit,
                offset,
                order: order.to_owned(),
                counts,
                messages: messages.into_iter().map(explorer_lcm_message).collect(),
                summary_nodes: summary_nodes
                    .into_iter()
                    .map(explorer_lcm_summary)
                    .collect(),
                has_more,
                has_more_messages,
                has_more_summary_nodes,
            };
            if window_partial || has_more {
                Json(DashboardEnvelopeV1::partial(
                    scope_from_state(&state),
                    examined.saturating_add(omitted_total),
                    examined,
                    "canonical hydrated records",
                    vec!["lcm_temporal_read_incomplete".to_owned()],
                    Some(payload),
                ))
                .into_response()
            } else {
                Json(DashboardEnvelopeV1::ready(
                    scope_from_state(&state),
                    DashboardCoverageV1::unknown(),
                    Some(payload),
                ))
                .into_response()
            }
        },
        tracing::trace_span!("dashboard_api.query.read_context"),
    )
    .await
}

async fn read_session_page(
    state: &DashboardState,
    control: DashboardHttpRequestControlV1,
    session_id: &str,
    limit: i64,
    cursor: Option<String>,
) -> DashboardLcmReadOutcomeV1 {
    let Some(authority) = state.lcm_read_authority.as_ref() else {
        return DashboardLcmReadOutcomeV1::NotReady {
            state: DashboardLcmReadStateV1::Unavailable,
            reason: "lcm_daemon_authority_unavailable".to_owned(),
        };
    };
    authority
        .read(
            control,
            state.project_id.as_deref(),
            DashboardLcmReadRequestV1::Session {
                session_id: session_id.to_owned(),
                limit,
                cursor,
            },
        )
        .await
}

fn explorer_session_rows(
    request: &ExplorerQueryRequestV1,
    page: DashboardLcmCanonicalPageV1,
    omission_reasons: Vec<String>,
    read_incomplete: bool,
) -> ExplorerSourceProgressV1 {
    let has_more = page.has_more;
    let rows = page
        .messages
        .into_iter()
        .map(explorer_lcm_message)
        .map(serde_json::to_value)
        .chain(
            page.summary_nodes
                .into_iter()
                .map(explorer_lcm_summary)
                .map(serde_json::to_value),
        )
        .collect::<Result<Vec<_>, _>>();
    match rows {
        Ok(rows) => {
            let mut source = ready_source(
                ExplorerSourceIdV1::Sessions,
                request,
                rows,
                None,
                json!({"query": request.query, "has_more": has_more}),
                "messages",
                omission_reasons,
            );
            // The temporal read itself reported omitted records: the rows are
            // real but the answer is incomplete, which is a different outcome
            // from a bounded page the source served completely.
            if read_incomplete {
                source.outcome = ExplorerSourceOutcomeV1::Partial;
                source.error_code = Some("lcm_temporal_read_incomplete");
            }
            source
        }
        Err(error) => ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::Sessions,
            "lcm_dashboard_contract_invalid",
            error.to_string(),
        ),
    }
}

fn explorer_lcm_message(message: DashboardLcmCanonicalMessageV1) -> LcmMessageV1 {
    LcmMessageV1 {
        store_id: None,
        session_id: message.session_id,
        role: Some(message.role),
        source: Some(message.provider),
        timestamp: message.timestamp,
        // The canonical temporal page carries no durable token accounting for
        // raw messages; absence stays typed instead of a char-count estimate.
        token_count: None,
        token_count_provenance: Some(LcmTokenCountProvenanceV1::Unavailable),
        content: Some(message.content),
        message_id: message.message_id,
        ordinal: Some(message.ordinal),
        storage_kind: Some("canonical_temporal".to_owned()),
        tool_use_id: message_tool_use_id(message.metadata_json.as_deref()),
        metadata_json: message.metadata_json,
        tool_name: message.tool_names,
        pinned: None,
        summary_node_ids: Vec::new(),
        snippet: None,
    }
}

fn explorer_lcm_summary(summary: DashboardLcmCanonicalSummaryV1) -> LcmSummaryNodeV1 {
    LcmSummaryNodeV1 {
        node_id: summary.node_id,
        session_id: summary.session_id,
        depth: summary.depth,
        category: "summary".to_owned(),
        source_type: "canonical_temporal".to_owned(),
        token_count: summary.token_count,
        source_token_count: summary.source_token_count,
        latest_at: summary.latest_at,
        created_at: summary.created_at,
        expand_hint: summary.expand_hint,
        summary: summary.summary,
        recency: summary.latest_at,
        snippet: None,
    }
}

fn explorer_session_counts(stats: &DashboardLcmCanonicalStatsV1) -> ExplorerSessionCountsV1 {
    ExplorerSessionCountsV1 {
        message_count: stats.message_count,
        unpublished_message_count: stats.unpublished_message_count,
        summary_node_count: stats.summary_node_count,
        summary_token_count: stats.summary_token_count,
        source_token_count: stats.source_token_count,
        token_estimate_total: stats.token_estimate_total,
    }
}

fn explorer_session_not_ready<T>(
    state: &DashboardState,
    read_state: DashboardLcmReadStateV1,
    reason: String,
) -> Response
where
    T: Serialize,
{
    let scope = scope_from_state(state);
    let envelope = match read_state {
        DashboardLcmReadStateV1::Absent => DashboardEnvelopeV1::complete_zero_findings(
            scope,
            DashboardCoverageV1::complete(0, "session records"),
            None::<T>,
        ),
        DashboardLcmReadStateV1::Stale => {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage.omission_reasons.push(reason);
            DashboardEnvelopeV1::stale(scope, coverage, None::<T>)
        }
        DashboardLcmReadStateV1::Locked => DashboardEnvelopeV1::locked(scope, None::<T>, reason),
        DashboardLcmReadStateV1::Denied => DashboardEnvelopeV1::denied(scope, None::<T>),
        DashboardLcmReadStateV1::Redacted => {
            DashboardEnvelopeV1::redacted(scope, None::<T>, reason)
        }
        DashboardLcmReadStateV1::CursorRefused => {
            DashboardEnvelopeV1::error(scope, None::<T>, reason)
        }
        DashboardLcmReadStateV1::Unavailable
        | DashboardLcmReadStateV1::CursorManifestLimitExceeded
        | DashboardLcmReadStateV1::BudgetExhausted
        | DashboardLcmReadStateV1::TimedOut
        | DashboardLcmReadStateV1::Cancelled => {
            DashboardEnvelopeV1::unavailable(scope, None::<T>, reason)
        }
    };
    Json(envelope).into_response()
}

const fn explorer_lcm_error_code(state: DashboardLcmReadStateV1) -> &'static str {
    match state {
        DashboardLcmReadStateV1::Absent => "lcm_session_absent",
        DashboardLcmReadStateV1::Stale => "lcm_temporal_projection_stale",
        DashboardLcmReadStateV1::Locked => "lcm_temporal_read_locked",
        DashboardLcmReadStateV1::Denied => "lcm_temporal_read_denied",
        DashboardLcmReadStateV1::Redacted => "lcm_temporal_read_redacted",
        DashboardLcmReadStateV1::Unavailable => "lcm_temporal_authority_unavailable",
        DashboardLcmReadStateV1::CursorManifestLimitExceeded => {
            "lcm_temporal_cursor_manifest_limit_exceeded"
        }
        DashboardLcmReadStateV1::CursorRefused => "lcm_temporal_cursor_refused",
        DashboardLcmReadStateV1::BudgetExhausted => "lcm_temporal_budget_exhausted",
        DashboardLcmReadStateV1::TimedOut => "lcm_temporal_read_timed_out",
        DashboardLcmReadStateV1::Cancelled => "lcm_temporal_read_cancelled",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod native_tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use axum::body::HttpBody;
    use tokio::sync::Notify;
    use tracedecay_domain::{Confidence, FactCategoryV1, ProvenanceId};
    use tracedecay_privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
    use tracedecay_runtime_core::native_runtime;
    use tracedecay_session_memory::fact_store::DatabaseFactStore;
    use tracedecay_store::{
        FactWriteControl, ProjectMemoryFactAddMaterialV1, ProjectMemoryFactStore,
    };

    use super::*;

    struct HeldSessions {
        started: Arc<Notify>,
        release: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    impl super::super::lcm_api::DashboardLcmReadPortV1 for HeldSessions {
        fn read(
            &self,
            _control: DashboardHttpRequestControlV1,
            _project_id: Option<&str>,
            _request: DashboardLcmReadRequestV1,
        ) -> super::super::lcm_api::DashboardLcmReadFutureV1<'_> {
            struct DropMarker(Arc<AtomicBool>);
            impl Drop for DropMarker {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            Box::pin(async {
                let _marker = DropMarker(Arc::clone(&self.dropped));
                self.started.notify_one();
                self.release.notified().await;
                DashboardLcmReadOutcomeV1::NotReady {
                    state: super::super::lcm_api::DashboardLcmReadStateV1::Unavailable,
                    reason: "held session read unavailable".to_owned(),
                }
            })
        }
    }

    struct StaleSessions;
    impl super::super::lcm_api::DashboardLcmReadPortV1 for StaleSessions {
        fn read(
            &self,
            _control: DashboardHttpRequestControlV1,
            _project_id: Option<&str>,
            _request: DashboardLcmReadRequestV1,
        ) -> super::super::lcm_api::DashboardLcmReadFutureV1<'_> {
            Box::pin(async {
                DashboardLcmReadOutcomeV1::NotReady {
                    state: super::super::lcm_api::DashboardLcmReadStateV1::Stale,
                    reason: "session projection is stale".to_owned(),
                }
            })
        }
    }

    async fn seed_fast_knowledge_row(state: &DashboardState) {
        let payload = json!({
            "content": "fixture memory row",
            "category": "project",
            "tags": ["explorer"],
            "entities": ["Fixture"],
            "metadata": {},
        });
        let receipt =
            match sanitize_memory_fact_payload(payload).expect("sanitize fixture knowledge row") {
                MemoryFactSanitizationV1::Durable { receipt, .. } => receipt,
                MemoryFactSanitizationV1::Quarantined => panic!("fixture fact must be durable"),
            };
        let command = ProjectMemoryFactAddMaterialV1::new(
            state.memory_owner.clone(),
            "fixture memory row".to_owned(),
            FactCategoryV1::Project,
            None,
            vec!["explorer".to_owned()],
            vec!["Fixture".to_owned()],
            json!({}),
            receipt,
            None,
            Confidence::new(0.8).expect("fixture confidence"),
            None,
        )
        .and_then(|material| {
            material.into_command(
                ProvenanceId::new("dashboard.explorer.stream.fixture".to_owned())
                    .expect("fixture provenance"),
            )
        })
        .expect("fixture fact command");
        DatabaseFactStore::new(&state.mem_db)
            .add_project_memory_fact(
                command,
                &FactWriteControl::new(Arc::new(|| false), Arc::new(|| true)),
            )
            .await
            .expect("seed fixture knowledge row");
    }

    async fn next_frame(body: &mut axum::body::Body) -> Value {
        let frame = native_runtime::timeout(Duration::from_secs(15), async {
            std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await
        })
        .await
        .expect("stream frame before deadline")
        .expect("stream has next frame")
        .expect("stream frame reads")
        .into_data()
        .expect("SSE data");
        let text = String::from_utf8(frame.to_vec()).expect("utf8 frame");
        text.lines()
            .find_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).expect("event JSON"))
            .unwrap_or(Value::Null)
    }

    fn assert_knowledge_coverage(source: &Value) {
        let omissions = source["coverage"]["omission_reasons"]
            .as_array()
            .expect("knowledge coverage reasons");
        match source["coverage"]["completeness"].as_str() {
            Some("complete") => assert!(omissions.is_empty(), "complete coverage has no gaps"),
            Some("unknown") => assert!(
                omissions
                    .iter()
                    .any(|reason| reason == "verified memory graph coverage is incomplete"),
                "unknown knowledge coverage names its missing graph: {source}"
            ),
            other => panic!("ready knowledge did not account for its coverage: {other:?}"),
        }
    }

    #[test]
    fn fast_explorer_source_precedes_held_source() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, mut dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-stream",
        ));
        fixture.block_on(seed_fast_knowledge_row(&dash));
        let held = Arc::new(HeldSessions {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
        });
        dash.lcm_read_authority = Some(held.clone());
        let response = native_runtime::block_on(async {
            stream_query(
                State(dash),
                RequestControl(DashboardHttpRequestControlV1::test_fixture(
                    "dashboard-explorer-stream",
                )),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                    offset: 0,
                }),
            )
            .await
        })
        .expect("native block_on");
        native_runtime::block_on(async {
            let mut body = response.into_body();
            let first = next_frame(&mut body).await;
            assert_eq!(
                first["kind"], "source",
                "the first explorer frame was not a source frame"
            );
            assert_ne!(first["source"]["source_id"], "sessions");
            let mut fast = first;
            while fast["source"]["source_id"] != "knowledge" {
                fast = next_frame(&mut body).await;
                assert_ne!(fast["source"]["source_id"], "sessions");
            }
            assert!(
                fast["source"]["page"]["rows"]
                    .as_array()
                    .is_some_and(|rows| rows
                        .iter()
                        .any(|row| { row["content"] == "fixture memory row" })),
                "knowledge rows arrived before the held session read: {fast}"
            );
            assert_knowledge_coverage(&fast["source"]);
            assert_eq!(fast["source"]["outcome"], "ready");
            held.release.notify_one();
            let mut saw_sessions = false;
            loop {
                let event = next_frame(&mut body).await;
                if event["kind"] == "final" {
                    assert!(
                        saw_sessions,
                        "released source emitted before terminal event"
                    );
                    break;
                }
                saw_sessions |= event["source"]["source_id"] == "sessions";
            }
        })
        .expect("native block_on");
    }

    #[test]
    fn disconnect_drops_unfinished_explorer_sources() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, mut dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-disconnect",
        ));
        let held = Arc::new(HeldSessions {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
        });
        dash.lcm_read_authority = Some(held.clone());
        native_runtime::block_on(async {
            let control = DashboardHttpRequestControlV1::test_fixture("explorer-disconnect");
            let cancellation = control.cancellation().clone();
            let response = stream_query(
                State(dash),
                RequestControl(control),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                    offset: 0,
                }),
            )
            .await;
            let mut body = response.into_body();
            let first = next_frame(&mut body).await;
            assert_eq!(first["kind"], "source");
            let second = next_frame(&mut body).await;
            assert_eq!(second["kind"], "source");
            assert_ne!(first["source"]["source_id"], second["source"]["source_id"]);
            assert_ne!(first["source"]["source_id"], "sessions");
            assert_ne!(second["source"]["source_id"], "sessions");
            {
                let next = next_frame(&mut body);
                tokio::pin!(next);
                tokio::select! {
                    () = held.started.notified() => {}
                    event = &mut next => panic!("held source finished before disconnect: {event}"),
                }
            }
            drop(body);
            assert!(
                cancellation.is_cancelled(),
                "body drop cancels admitted request"
            );
            assert!(
                held.dropped.load(Ordering::SeqCst),
                "body drop releases held source"
            );
        })
        .expect("native block_on");
    }

    #[test]
    fn admitted_cancellation_concludes_each_source_once() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, mut dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-cancellation",
        ));
        fixture.block_on(seed_fast_knowledge_row(&dash));
        dash.lcm_read_authority = Some(Arc::new(HeldSessions {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
        }));
        let frames = native_runtime::block_on(async {
            let control = DashboardHttpRequestControlV1::test_fixture("explorer-cancellation");
            let cancellation = control.cancellation().clone();
            let response = stream_query(
                State(dash),
                RequestControl(control),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                    offset: 0,
                }),
            )
            .await;
            let mut body = response.into_body();
            let first = next_frame(&mut body).await;
            let second = next_frame(&mut body).await;
            assert_eq!(first["kind"], "source");
            assert_eq!(second["kind"], "source");
            assert_ne!(first["source"]["source_id"], "sessions");
            assert_ne!(second["source"]["source_id"], "sessions");
            cancellation.cancel(tracedecay_session_memory::context::application_observed_at());
            let third = next_frame(&mut body).await;
            let final_frame = next_frame(&mut body).await;
            let eof = native_runtime::timeout(Duration::from_secs(15), async {
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            })
            .await
            .expect("stream ends after terminal frame");
            assert!(eof.is_none(), "stream emitted more than one terminal frame");
            [first, second, third, final_frame]
        })
        .expect("native block_on");
        assert_eq!(frames[2]["source"]["source_id"], "sessions");
        assert_eq!(frames[2]["source"]["outcome"], "cancelled");
        assert_eq!(frames[3]["kind"], "final");
        assert_eq!(frames[3]["state"], "cancelled");
    }

    #[test]
    fn request_deadline_concludes_each_missing_source_once() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, mut dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-deadline",
        ));
        dash.lcm_read_authority = Some(Arc::new(HeldSessions {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
        }));
        let frames = native_runtime::block_on(async {
            let control = DashboardHttpRequestControlV1::test_fixture_with(
                "explorer-deadline",
                tracedecay_contracts::CancellationSignal::active("cancel.explorer-deadline")
                    .expect("test cancellation"),
                tracedecay_domain::UtcMicros(1),
                tracedecay_domain::UtcMicros(1),
            );
            let response = stream_query(
                State(dash),
                RequestControl(control),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                    offset: 0,
                }),
            )
            .await;
            let mut body = response.into_body();
            let mut frames = Vec::new();
            loop {
                let frame = next_frame(&mut body).await;
                let final_frame = frame["kind"] == "final";
                frames.push(frame);
                if final_frame {
                    break;
                }
            }
            let eof = native_runtime::timeout(Duration::from_secs(15), async {
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            })
            .await
            .expect("stream ends after terminal frame");
            assert!(eof.is_none(), "stream emitted more than one terminal frame");
            frames
        })
        .expect("native block_on");
        assert_eq!(frames.len(), 4, "three sources then one terminal frame");
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["kind"] == "final")
                .count(),
            1
        );
        let sources: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["kind"] == "source")
            .collect();
        assert_eq!(sources.len(), 3);
        for source_id in ["code_graph", "sessions", "knowledge"] {
            let source = sources
                .iter()
                .find(|frame| frame["source"]["source_id"] == source_id)
                .map(|frame| &frame["source"])
                .unwrap_or_else(|| panic!("deadline must conclude {source_id}"));
            assert_eq!(source["outcome"], "timed_out", "{source}");
            assert_eq!(source["error_code"], "request_deadline_elapsed");
            assert!(source["page"].is_null());
        }
        assert_eq!(frames[3]["state"], "timed_out");
    }

    #[test]
    fn explorer_source_outcomes_remain_distinct() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, mut dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-outcomes",
        ));
        dash.lcm_read_authority = Some(Arc::new(StaleSessions));
        let frames = native_runtime::block_on(async {
            let response = stream_query(
                State(dash),
                RequestControl(DashboardHttpRequestControlV1::test_fixture(
                    "explorer-outcomes",
                )),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                    offset: 0,
                }),
            )
            .await;
            let mut body = response.into_body();
            let mut frames = Vec::new();
            loop {
                let frame = next_frame(&mut body).await;
                let final_frame = frame["kind"] == "final";
                frames.push(frame);
                if final_frame {
                    break;
                }
            }
            frames
        })
        .expect("native block_on");
        assert_eq!(frames.len(), 4);
        assert_eq!(
            frames.last().map(|frame| &frame["state"]),
            Some(&json!("partial"))
        );
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["kind"] == "final")
                .count(),
            1
        );
        let sources: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["kind"] == "source")
            .collect();
        assert_eq!(sources.len(), 3);
        let source = |id| {
            sources
                .iter()
                .find(|frame| frame["source"]["source_id"] == id)
                .map(|frame| &frame["source"])
                .expect("required source frame")
        };
        assert_eq!(source("code_graph")["outcome"], "unavailable");
        assert_eq!(source("code_graph")["error_code"], "missing_registry");
        assert!(source("code_graph")["page"].is_null());
        assert_eq!(source("sessions")["outcome"], "stale");
        assert_eq!(
            source("sessions")["error_code"],
            "lcm_temporal_projection_stale"
        );
        assert!(source("sessions")["page"].is_null());
        assert_eq!(source("knowledge")["outcome"], "ready");
        assert_eq!(source("knowledge")["page"]["rows"], json!([]));
        assert_knowledge_coverage(source("knowledge"));
    }
}
