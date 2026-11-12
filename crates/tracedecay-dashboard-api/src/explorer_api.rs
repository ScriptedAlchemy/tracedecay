//! Explorer search stream and session-context HTTP binding.
//!
//! One request owns one search. The three source reads are polled in place by
//! the response body, so each source's result and coverage is published as it
//! finishes, one final frame closes the stream, and dropping the body (a
//! closed connection, a superseded search) drops every unfinished read. The
//! coordinator preserves source-local ordering and coverage instead of
//! manufacturing a cross-source rank.

use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures_util::stream::{FuturesUnordered, Stream, StreamExt};
use futures_util::{FutureExt, stream};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_runtime_core::native_runtime;

mod knowledge;

use super::lcm_api::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadOutcomeV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1, LcmMessageV1, LcmSummaryNodeV1, LcmTokenCountProvenanceV1,
    message_tool_use_id,
};
use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, now_micros, scope_from_state};
use super::util::json_error;
use super::{DashboardHttpRequestControlV1, DashboardState, RequestControl, graph_service};

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

/// How the whole search ended, published once as the final frame. A search
/// whose connection closed has no outcome: nothing is left to receive it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerOutcomeV1 {
    Completed,
    Partial,
    TimedOut,
    Error,
}

/// What one source truthfully concluded for this search. Every member has a
/// real producer; none is speculative:
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
    limit: i64,
    total: Option<u64>,
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

    fn with_outcome(self, outcome: ExplorerSourceOutcomeV1) -> Self {
        Self { outcome, ..self }
    }

    fn error(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::unavailable(source_id, error_code, message)
            .with_outcome(ExplorerSourceOutcomeV1::Error)
    }

    fn stale(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::unavailable(source_id, error_code, message)
            .with_outcome(ExplorerSourceOutcomeV1::Stale)
    }

    fn timed_out(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::unavailable(source_id, error_code, message)
            .with_outcome(ExplorerSourceOutcomeV1::TimedOut)
    }

    fn cancelled(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::unavailable(source_id, error_code, message)
            .with_outcome(ExplorerSourceOutcomeV1::Cancelled)
    }
}

/// One frame of the search stream. The SSE event name is the `event` tag.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum ExplorerStreamEventV1 {
    /// One source finished. Sources arrive in completion order, each once.
    Source {
        source: Box<ExplorerSourceProgressV1>,
    },
    /// The last frame: every source has been published.
    Final {
        outcome: ExplorerOutcomeV1,
        elapsed_micros: i64,
    },
}

impl ExplorerStreamEventV1 {
    const fn name(&self) -> &'static str {
        match self {
            Self::Source { .. } => "source",
            Self::Final { .. } => "final",
        }
    }

    fn into_sse(self) -> Result<Event, axum::Error> {
        Event::default().event(self.name()).json_data(&self)
    }
}

type SourceRead = Pin<Box<dyn Future<Output = ExplorerSourceProgressV1> + Send>>;

/// Cancels the request's signal when the stream is dropped, so reads that poll
/// the request control (the graph and fact readers run off-thread) stop with
/// the connection instead of finishing unobserved.
struct CancelOnDrop(CancellationSignal);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self
            .0
            .cancel(tracedecay_session_memory::context::application_observed_at());
    }
}

/// The three-source search as a stream of frames. The unfinished reads live in
/// this state, so dropping the stream drops them.
struct SearchRun {
    reads: FuturesUnordered<SourceRead>,
    unfinished: Vec<ExplorerSourceIdV1>,
    every_source_complete: bool,
    any_source_answered: bool,
    queued: VecDeque<ExplorerStreamEventV1>,
    admitted_deadline: Deadline,
    deadline: Pin<Box<dyn Future<Output = ()> + Send>>,
    cancellation: CancellationSignal,
    started_at_micros: i64,
    finished: bool,
    _cancel_on_drop: CancelOnDrop,
}

impl SearchRun {
    fn new(
        reads: impl IntoIterator<Item = (ExplorerSourceIdV1, SourceRead)>,
        control: &DashboardHttpRequestControlV1,
    ) -> Self {
        let mut unfinished = Vec::new();
        let reads: FuturesUnordered<SourceRead> = reads
            .into_iter()
            .map(|(source_id, read)| {
                unfinished.push(source_id);
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
        Self {
            reads,
            unfinished,
            every_source_complete: true,
            any_source_answered: false,
            queued: VecDeque::new(),
            admitted_deadline: control.deadline(),
            deadline: Box::pin(admitted_deadline_elapsed(control.deadline())),
            cancellation: control.cancellation().clone(),
            started_at_micros: now_micros(),
            finished: false,
            _cancel_on_drop: CancelOnDrop(control.cancellation().clone()),
        }
    }

    fn settle(&mut self, source: &ExplorerSourceProgressV1) {
        self.unfinished.retain(|id| *id != source.source_id);
        self.every_source_complete &=
            source.outcome == ExplorerSourceOutcomeV1::Ready && source.coverage.is_complete();
        self.any_source_answered |= matches!(
            source.outcome,
            ExplorerSourceOutcomeV1::Ready | ExplorerSourceOutcomeV1::Partial
        );
    }

    fn finish(&mut self, outcome: ExplorerOutcomeV1) {
        self.finished = true;
        self.queued.push_back(ExplorerStreamEventV1::Final {
            outcome,
            elapsed_micros: now_micros().saturating_sub(self.started_at_micros),
        });
    }

    fn time_out(&mut self) {
        self.reads.clear();
        let _ = self
            .cancellation
            .cancel(tracedecay_session_memory::context::application_observed_at());
        for source_id in std::mem::take(&mut self.unfinished) {
            self.queued.push_back(ExplorerStreamEventV1::Source {
                source: Box::new(ExplorerSourceProgressV1::timed_out(
                    source_id,
                    "request_deadline_elapsed",
                    "dashboard request deadline elapsed",
                )),
            });
        }
        self.finish(ExplorerOutcomeV1::TimedOut);
    }

    fn stop_if_required(&mut self) -> bool {
        if self.cancellation.is_cancelled() {
            self.reads.clear();
            self.finished = true;
            return true;
        }
        if self
            .admitted_deadline
            .is_elapsed_at(tracedecay_session_memory::context::application_observed_at())
            || self.deadline.as_mut().now_or_never().is_some()
        {
            self.time_out();
            return true;
        }
        false
    }

    async fn next_event(&mut self) -> Option<ExplorerStreamEventV1> {
        loop {
            if !self.finished {
                self.stop_if_required();
            }
            if let Some(event) = self.queued.pop_front() {
                return Some(event);
            }
            if self.finished {
                return None;
            }
            if self.unfinished.is_empty() {
                let outcome = if self.every_source_complete {
                    ExplorerOutcomeV1::Completed
                } else if self.any_source_answered {
                    ExplorerOutcomeV1::Partial
                } else {
                    ExplorerOutcomeV1::Error
                };
                self.finish(outcome);
                continue;
            }
            tokio::select! {
                biased;
                () = self.cancellation.cancelled() => {
                    self.reads.clear();
                    self.finished = true;
                    return None;
                }
                () = &mut self.deadline => self.time_out(),
                Some(source) = self.reads.next() => {
                    if self.stop_if_required() {
                        continue;
                    }
                    self.settle(&source);
                    return Some(ExplorerStreamEventV1::Source { source: Box::new(source) });
                }
            }
        }
    }
}

fn search_events<R: IntoIterator<Item = (ExplorerSourceIdV1, SourceRead)>>(
    reads: R,
    control: &DashboardHttpRequestControlV1,
) -> impl Stream<Item = ExplorerStreamEventV1> + Send + use<R> {
    stream::unfold(SearchRun::new(reads, control), |mut run| async move {
        run.next_event().await.map(|event| (event, run))
    })
}

async fn admitted_deadline_elapsed(deadline: Deadline) {
    let observed_at = tracedecay_session_memory::context::application_observed_at();
    let remaining_micros = deadline.expires_at.0.saturating_sub(observed_at.0).max(0) as u64;
    let due = Instant::now() + Duration::from_micros(remaining_micros);
    // A search without a timer times out at once rather than run unbounded.
    if let Err(error) = native_runtime::sleep_until(due).await {
        tracing::error!(%error, "dashboard explorer deadline timer unavailable");
    }
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
    Ok(())
}

fn source_reads(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
    control: &DashboardHttpRequestControlV1,
) -> Vec<(ExplorerSourceIdV1, SourceRead)> {
    SOURCE_IDS
        .into_iter()
        .map(|source_id| {
            let (state, request, control) = (state.clone(), request.clone(), control.clone());
            let read: SourceRead = Box::pin(async move {
                match source_id {
                    ExplorerSourceIdV1::CodeGraph => code_source(&state, &request, &control).await,
                    ExplorerSourceIdV1::Sessions => session_source(&state, &request, control).await,
                    ExplorerSourceIdV1::Knowledge => {
                        knowledge::knowledge_source(&state, &request, &control).await
                    }
                }
            });
            (source_id, read)
        })
        .collect()
}

/// Streams one search. The response owns the reads: it is the only holder of
/// the search, so there is nothing to look up, poll, or cancel afterwards.
pub async fn search_stream(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    Json(mut request): Json<ExplorerQueryRequestV1>,
) -> Response {
    tracing::Instrument::instrument(
        async move {
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
            let reads = source_reads(&state, &request, &control);
            Sse::new(search_events(reads, &control).map(ExplorerStreamEventV1::into_sse))
                .into_response()
        },
        tracing::trace_span!("dashboard_api.query.stream"),
    )
    .await
}

async fn code_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
    control: &DashboardHttpRequestControlV1,
) -> ExplorerSourceProgressV1 {
    let read = match graph_service::search_payload(state, control, &request.query, request.limit, 0)
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
            limit: request.limit,
            total,
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
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures_util::{FutureExt, StreamExt};
    use tokio::sync::oneshot;
    use tracedecay_contracts::CancellationSignal;
    use tracedecay_domain::UtcMicros;

    use super::*;

    fn control_with_deadline(label: &str, deadline_in: i64) -> DashboardHttpRequestControlV1 {
        let now = tracedecay_session_memory::context::application_observed_at();
        DashboardHttpRequestControlV1::test_fixture_with(
            label,
            CancellationSignal::active(format!("cancel.{label}")).expect("cancellation"),
            now,
            UtcMicros(now.0 + deadline_in),
        )
    }

    fn control(label: &str) -> DashboardHttpRequestControlV1 {
        control_with_deadline(label, 60_000_000)
    }

    fn request() -> ExplorerQueryRequestV1 {
        ExplorerQueryRequestV1 {
            query: "cache".to_owned(),
            limit: 5,
        }
    }

    fn ready(source_id: ExplorerSourceIdV1, rows: usize) -> ExplorerSourceProgressV1 {
        ready_source(
            source_id,
            &request(),
            vec![json!({"id": "row"}); rows],
            Some(rows as u64),
            json!({}),
            "rows",
            Vec::new(),
        )
    }

    fn immediate(source: ExplorerSourceProgressV1) -> (ExplorerSourceIdV1, SourceRead) {
        (source.source_id, Box::pin(async move { source }))
    }

    fn held(
        source: ExplorerSourceProgressV1,
    ) -> (oneshot::Sender<()>, (ExplorerSourceIdV1, SourceRead)) {
        let (release, released) = oneshot::channel();
        let source_id = source.source_id;
        let read: SourceRead = Box::pin(async move {
            let _ = released.await;
            source
        });
        (release, (source_id, read))
    }

    fn source_of(event: Option<ExplorerStreamEventV1>) -> ExplorerSourceProgressV1 {
        match event {
            Some(ExplorerStreamEventV1::Source { source }) => *source,
            other => panic!("expected a source frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_finished_source_is_published_while_another_is_still_reading() {
        let (release, sessions) = held(ready(ExplorerSourceIdV1::Sessions, 1));
        let control = control("explorer-progressive");
        let mut events = Box::pin(search_events(
            [
                immediate(ready(ExplorerSourceIdV1::CodeGraph, 2)),
                sessions,
                immediate(ready(ExplorerSourceIdV1::Knowledge, 3)),
            ],
            &control,
        ));

        let mut early = vec![
            source_of(events.next().now_or_never().expect("first fast source")),
            source_of(events.next().now_or_never().expect("second fast source")),
        ]
        .into_iter()
        .map(|source| {
            (
                source.source_id,
                source.page.map_or(0, |page| page.rows.len()),
            )
        })
        .collect::<Vec<_>>();
        early.sort_by_key(|(id, _)| *id as u8);
        assert_eq!(
            early,
            vec![
                (ExplorerSourceIdV1::CodeGraph, 2),
                (ExplorerSourceIdV1::Knowledge, 3),
            ]
        );
        assert!(
            events.next().now_or_never().is_none(),
            "the held source must not block or fabricate a frame"
        );

        release.send(()).expect("release the held source");
        assert_eq!(
            source_of(events.next().await).source_id,
            ExplorerSourceIdV1::Sessions
        );
        assert!(matches!(
            events.next().await,
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::Completed,
                ..
            })
        ));
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn failed_and_stale_sources_stay_distinct_from_an_empty_answer() {
        let control = control("explorer-distinct");
        let mut events = Box::pin(search_events(
            [
                immediate(ExplorerSourceProgressV1::stale(
                    ExplorerSourceIdV1::CodeGraph,
                    "graph_generation_stale",
                    "stale",
                )),
                immediate(ready(ExplorerSourceIdV1::Sessions, 0)),
                immediate(ExplorerSourceProgressV1::error(
                    ExplorerSourceIdV1::Knowledge,
                    "knowledge_query_failed",
                    "failed",
                )),
            ],
            &control,
        ));
        let mut outcomes = Vec::new();
        for _ in 0..3 {
            let source = source_of(events.next().await);
            outcomes.push((source.source_id, source.outcome, source.page.is_some()));
        }
        outcomes.sort_by_key(|(id, _, _)| *id as u8);
        assert_eq!(
            outcomes,
            vec![
                (
                    ExplorerSourceIdV1::CodeGraph,
                    ExplorerSourceOutcomeV1::Stale,
                    false
                ),
                (
                    ExplorerSourceIdV1::Sessions,
                    ExplorerSourceOutcomeV1::Ready,
                    true
                ),
                (
                    ExplorerSourceIdV1::Knowledge,
                    ExplorerSourceOutcomeV1::Error,
                    false
                ),
            ]
        );
        assert!(matches!(
            events.next().await,
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::Partial,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_search_where_no_source_answers_ends_in_error() {
        let control = control("explorer-no-answer");
        let events = search_events(
            [immediate(ExplorerSourceProgressV1::unavailable(
                ExplorerSourceIdV1::Sessions,
                "lcm_daemon_authority_unavailable",
                "down",
            ))],
            &control,
        )
        .collect::<Vec<_>>()
        .await;
        assert!(matches!(
            events.last(),
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::Error,
                ..
            })
        ));
    }

    struct ReadGuard(Arc<AtomicBool>);

    impl Drop for ReadGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn dropping_the_stream_releases_unfinished_reads_and_cancels_the_request() {
        let released = Arc::new(AtomicBool::new(false));
        let guard = ReadGuard(Arc::clone(&released));
        let pending: SourceRead = Box::pin(async move {
            let _guard = guard;
            std::future::pending::<ExplorerSourceProgressV1>().await
        });
        let control = control("explorer-drop");
        let mut events = Box::pin(search_events(
            [
                immediate(ready(ExplorerSourceIdV1::CodeGraph, 1)),
                (ExplorerSourceIdV1::Sessions, pending),
            ],
            &control,
        ));
        assert_eq!(
            source_of(events.next().await).source_id,
            ExplorerSourceIdV1::CodeGraph
        );
        assert!(events.next().now_or_never().is_none());
        assert!(!released.load(Ordering::SeqCst));
        assert!(!control.cancellation().is_cancelled());

        drop(events);

        assert!(
            released.load(Ordering::SeqCst),
            "the unfinished read must be dropped with the stream"
        );
        assert!(
            control.cancellation().is_cancelled(),
            "off-thread readers observe the request cancellation"
        );
    }

    #[tokio::test]
    async fn an_elapsed_deadline_settles_unfinished_sources_as_timed_out() {
        let control = control("explorer-deadline");
        let released = Arc::new(AtomicBool::new(false));
        let guard = ReadGuard(Arc::clone(&released));
        let pending: SourceRead = Box::pin(async move {
            let _guard = guard;
            std::future::pending().await
        });
        let mut run = SearchRun::new(
            [
                immediate(ready(ExplorerSourceIdV1::CodeGraph, 1)),
                (ExplorerSourceIdV1::Sessions, pending),
            ],
            &control,
        );
        let (expire, expired) = oneshot::channel();
        run.deadline = Box::pin(async move {
            expired.await.expect("release the admitted deadline");
        });
        assert_eq!(
            source_of(run.next_event().await).source_id,
            ExplorerSourceIdV1::CodeGraph
        );
        assert!(run.next_event().now_or_never().is_none());
        assert!(!released.load(Ordering::SeqCst));
        assert!(!control.cancellation().is_cancelled());
        expire.send(()).expect("expire the admitted deadline");
        let timed_out = source_of(run.next_event().await);
        assert_eq!(timed_out.source_id, ExplorerSourceIdV1::Sessions);
        assert_eq!(timed_out.outcome, ExplorerSourceOutcomeV1::TimedOut);
        assert!(released.load(Ordering::SeqCst));
        assert!(control.cancellation().is_cancelled());
        assert!(matches!(
            run.next_event().await,
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::TimedOut,
                ..
            })
        ));
        assert!(run.next_event().await.is_none());
    }

    #[tokio::test]
    async fn a_final_source_crossing_the_deadline_cannot_publish_an_ordinary_terminal() {
        for source in [
            ready(ExplorerSourceIdV1::Sessions, 1),
            ready(ExplorerSourceIdV1::Sessions, 1).with_outcome(ExplorerSourceOutcomeV1::Partial),
            ExplorerSourceProgressV1::unavailable(
                ExplorerSourceIdV1::Sessions,
                "unavailable",
                "unavailable",
            ),
            ExplorerSourceProgressV1::error(ExplorerSourceIdV1::Sessions, "failed", "failed"),
        ] {
            let control = control("explorer-final-source-deadline");
            let (expire, expired) = oneshot::channel();
            let read: SourceRead = Box::pin(async move {
                expire.send(()).expect("expire during the source poll");
                source
            });
            let mut run = SearchRun::new([(ExplorerSourceIdV1::Sessions, read)], &control);
            run.deadline = Box::pin(async move {
                expired.await.expect("deadline released");
            });
            assert_eq!(
                source_of(run.next_event().await).outcome,
                ExplorerSourceOutcomeV1::TimedOut
            );
            assert!(matches!(
                run.next_event().await,
                Some(ExplorerStreamEventV1::Final {
                    outcome: ExplorerOutcomeV1::TimedOut,
                    ..
                })
            ));
            assert!(run.next_event().await.is_none());
        }
    }

    #[tokio::test]
    async fn expiry_between_the_last_source_frame_and_finalization_preserves_the_frame() {
        let control = control("explorer-final-frame-deadline");
        let (expire, expired) = oneshot::channel();
        let mut run = SearchRun::new(
            [immediate(ready(ExplorerSourceIdV1::Sessions, 1))],
            &control,
        );
        run.deadline = Box::pin(async move {
            expired.await.expect("deadline released");
        });
        let source = source_of(run.next_event().await);
        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Ready);
        assert_eq!(source.page.expect("published answer").rows.len(), 1);
        expire.send(()).expect("expire before finalization");
        assert!(matches!(
            run.next_event().await,
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::TimedOut,
                ..
            })
        ));
        assert!(run.next_event().await.is_none());
    }

    #[tokio::test]
    async fn cancellation_during_the_final_source_poll_closes_without_publishing() {
        let control = control("explorer-final-source-cancellation");
        let cancellation = control.cancellation().clone();
        let read: SourceRead = Box::pin(async move {
            assert!(
                cancellation.cancel(tracedecay_session_memory::context::application_observed_at()),
                "cancel during the source poll"
            );
            ready(ExplorerSourceIdV1::Sessions, 1)
        });
        let mut events = Box::pin(search_events(
            [(ExplorerSourceIdV1::Sessions, read)],
            &control,
        ));
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn cancellation_between_the_last_source_frame_and_finalization_closes_the_stream() {
        let control = control("explorer-final-frame-cancellation");
        let mut events = Box::pin(search_events(
            [immediate(ready(ExplorerSourceIdV1::Sessions, 1))],
            &control,
        ));
        assert_eq!(
            source_of(events.next().await).outcome,
            ExplorerSourceOutcomeV1::Ready
        );
        assert!(
            control
                .cancellation()
                .cancel(tracedecay_session_memory::context::application_observed_at()),
            "cancel before finalization"
        );
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn an_expired_admission_times_out_even_when_its_timer_has_not_been_polled_ready() {
        let control = control_with_deadline("explorer-expired-admission", -1);
        let mut run = SearchRun::new(
            [immediate(ready(ExplorerSourceIdV1::Sessions, 1))],
            &control,
        );
        run.deadline = Box::pin(std::future::pending());
        assert_eq!(
            source_of(run.next_event().await).outcome,
            ExplorerSourceOutcomeV1::TimedOut
        );
        assert!(matches!(
            run.next_event().await,
            Some(ExplorerStreamEventV1::Final {
                outcome: ExplorerOutcomeV1::TimedOut,
                ..
            })
        ));
        assert!(run.next_event().await.is_none());
    }

    #[tokio::test]
    async fn a_panicking_source_is_published_as_an_error_not_a_lost_stream() {
        let control = control("explorer-panic");
        let panicking: SourceRead = Box::pin(async { panic!("source blew up") });
        let events = search_events([(ExplorerSourceIdV1::Sessions, panicking)], &control)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            source_of(events.first().cloned()).error_code,
            Some("source_task_failed")
        );
    }

    #[test]
    fn the_request_rejects_an_offset_it_can_no_longer_honor() {
        let parsed: Result<ExplorerQueryRequestV1, _> =
            serde_json::from_value(json!({"query": "cache", "limit": 5, "offset": 1}));
        assert!(parsed.is_err());
    }

    #[test]
    fn explorer_search_settles_without_a_tokio_runtime() {
        let fixture = tokio::runtime::Runtime::new().expect("fixture runtime");
        let (_project, dash) = fixture.block_on(crate::events_api::dashboard_state_fixture(
            "project.dashboard-explorer-native",
        ));
        let writer_waits = native_runtime::native_thread_writer_waits();
        let frames = native_runtime::block_on(async move {
            assert!(tokio::runtime::Handle::try_current().is_err());
            let response = search_stream(
                State(dash.clone()),
                RequestControl(DashboardHttpRequestControlV1::test_fixture(
                    "dashboard-explorer-native",
                )),
                Json(ExplorerQueryRequestV1 {
                    query: "fixture".to_owned(),
                    limit: 1,
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("stream body");
            String::from_utf8(body.to_vec()).expect("utf-8 stream")
        })
        .expect("native block_on");
        assert_eq!(
            frames.matches("event: source").count(),
            3,
            "every source publishes once: {frames}"
        );
        assert!(
            frames.contains("event: final") && frames.contains("\"outcome\":\"partial\""),
            "the stream ends in one final outcome: {frames}"
        );
        assert_eq!(
            native_runtime::native_thread_writer_waits(),
            writer_waits,
            "the explorer search parked no native thread on a writer"
        );
    }
}
