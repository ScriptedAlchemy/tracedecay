//! Explorer planner/coordinator and session-context HTTP binding.
//!
//! The coordinator composes existing graph, session, and memory authorities.
//! It preserves source-local ordering and coverage instead of manufacturing a
//! cross-source rank. Query cancellation is bound to the opaque run identity
//! stored here and reaches the live Tokio work through `CancellationToken`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::RwLock;
use tokio::task::JoinSet;

use super::lcm_api::{LcmMessageV1, LcmSummaryNodeV1};
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, now_micros, scope_from_state,
};
use super::{DashboardState, graph_service, memory_service};
use crate::application::context::CancellationToken;
use crate::request_identity::{GlobalOpaqueIdentityKind, mint_global_opaque_id};

const SOURCE_IDS: [ExplorerSourceIdV1; 3] = [
    ExplorerSourceIdV1::CodeGraph,
    ExplorerSourceIdV1::Sessions,
    ExplorerSourceIdV1::Knowledge,
];
const MAX_QUERY_RUNS: usize = 256;
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
    Pending,
    Completed,
    Partial,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerFinalityV1 {
    Pending,
    Complete,
    Partial,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerSourcePhaseV1 {
    Queued,
    Reading,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerSourceOutcomeV1 {
    Pending,
    Ready,
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
    phase: ExplorerSourcePhaseV1,
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
    fn pending(source_id: ExplorerSourceIdV1) -> Self {
        Self {
            source_id,
            source_label: source_id.label(),
            phase: ExplorerSourcePhaseV1::Queued,
            outcome: ExplorerSourceOutcomeV1::Pending,
            completed_units: None,
            total_units: None,
            coverage: DashboardCoverageV1::unknown(),
            freshness: "unknown",
            watermark: None,
            error_code: None,
            message: None,
            page: None,
        }
    }

    fn unavailable(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_id,
            source_label: source_id.label(),
            phase: ExplorerSourcePhaseV1::Completed,
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
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ExplorerQueryRunV1 {
    run_id: String,
    request: ExplorerQueryRequestV1,
    request_revision: &'static str,
    plan_revision: &'static str,
    merge_revision: &'static str,
    required_source_ids: [ExplorerSourceIdV1; 3],
    ordering_policy: &'static str,
    explanation: &'static str,
    submitted_at_micros: i64,
    completed_at_micros: Option<i64>,
    elapsed_micros: i64,
    state: ExplorerRunStateV1,
    finality: ExplorerFinalityV1,
    sources: Vec<ExplorerSourceProgressV1>,
}

#[derive(Clone)]
struct StoredExplorerRun {
    owner: String,
    cancellation: CancellationToken,
    run: Arc<RwLock<ExplorerQueryRunV1>>,
}

static QUERY_RUNS: OnceLock<Mutex<HashMap<String, StoredExplorerRun>>> = OnceLock::new();

fn query_runs() -> &'static Mutex<HashMap<String, StoredExplorerRun>> {
    QUERY_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_owner(state: &DashboardState) -> String {
    state
        .project_id
        .clone()
        .unwrap_or_else(|| state.graph_db_path.clone())
}

fn new_run_id() -> Option<String> {
    mint_global_opaque_id(GlobalOpaqueIdentityKind::ExplorerRun).ok()
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"detail": message.into()})),
    )
        .into_response()
}

fn not_found(run_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"detail": format!("explorer query run not found: {run_id}")})),
    )
        .into_response()
}

fn internal_error(message: impl Into<String>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"detail": message.into()})),
    )
        .into_response()
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

fn initial_run(run_id: String, request: ExplorerQueryRequestV1) -> ExplorerQueryRunV1 {
    ExplorerQueryRunV1 {
        run_id,
        request,
        request_revision: "explorer-query-request-v1",
        plan_revision: "explorer-query-plan-v1",
        merge_revision: "source-local-no-merge-v1",
        required_source_ids: SOURCE_IDS,
        ordering_policy: "source_local_no_cross_source_merge",
        explanation: "Search the code graph, active-project session store, and bounded project fact authority in parallel; preserve each source's own order and coverage.",
        submitted_at_micros: now_micros(),
        completed_at_micros: None,
        elapsed_micros: 0,
        state: ExplorerRunStateV1::Pending,
        finality: ExplorerFinalityV1::Pending,
        sources: SOURCE_IDS
            .into_iter()
            .map(ExplorerSourceProgressV1::pending)
            .collect(),
    }
}

fn envelope_for_run(
    state: &DashboardState,
    run: ExplorerQueryRunV1,
) -> DashboardEnvelopeV1<ExplorerQueryRunV1> {
    let complete_sources = run
        .sources
        .iter()
        .filter(|source| source.coverage.is_complete())
        .count() as u64;
    let coverage = if complete_sources == SOURCE_IDS.len() as u64 {
        DashboardCoverageV1::complete(SOURCE_IDS.len() as u64, "sources")
    } else {
        DashboardCoverageV1::partial(
            SOURCE_IDS.len() as u64,
            complete_sources,
            "sources",
            vec!["one or more required sources has unknown or partial coverage".to_owned()],
        )
    };
    let domain_state = match run.state {
        ExplorerRunStateV1::Pending => DashboardDomainStateV1::Loading,
        ExplorerRunStateV1::Completed => DashboardDomainStateV1::Ready,
        ExplorerRunStateV1::Partial => DashboardDomainStateV1::Partial,
        ExplorerRunStateV1::Cancelled => DashboardDomainStateV1::Cancelled,
        ExplorerRunStateV1::Error => DashboardDomainStateV1::Error,
    };
    let mut envelope = DashboardEnvelopeV1::new(
        scope_from_state(state),
        domain_state,
        coverage,
        DashboardFreshnessV1::unknown(),
        run,
    );
    if envelope.payload.state == ExplorerRunStateV1::Pending {
        envelope = envelope.with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::RequestCancel,
            "dashboard.explorer.query.cancel",
        )]);
    }
    envelope
}

fn remember_run(run: StoredExplorerRun, run_id: String) -> Result<(), &'static str> {
    let mut runs = query_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runs.contains_key(&run_id) {
        return Err("explorer query run identity collision");
    }
    if runs.len() >= MAX_QUERY_RUNS {
        let oldest_terminal = runs
            .iter()
            .filter(|(_, stored)| {
                stored
                    .run
                    .try_read()
                    .is_ok_and(|run| run.state != ExplorerRunStateV1::Pending)
            })
            .min_by_key(|(_, stored)| {
                stored
                    .run
                    .try_read()
                    .map_or(i64::MIN, |run| run.submitted_at_micros)
            })
            .map(|(run_id, _)| run_id.clone());
        let Some(oldest_terminal) = oldest_terminal else {
            return Err("explorer query run capacity is exhausted");
        };
        runs.remove(&oldest_terminal);
    }
    runs.insert(run_id, run);
    Ok(())
}

fn find_run(state: &DashboardState, run_id: &str) -> Option<StoredExplorerRun> {
    let runs = query_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.get(run_id)
        .filter(|stored| stored.owner == run_owner(state))
        .cloned()
}

pub async fn create_query(
    State(state): State<DashboardState>,
    Json(mut request): Json<ExplorerQueryRequestV1>,
) -> Response {
    if let Err(message) = validate_query(&mut request) {
        return bad_request(message);
    }
    let Some(run_id) = new_run_id() else {
        return internal_error("could not allocate explorer query run identity");
    };
    let run = Arc::new(RwLock::new(initial_run(run_id.clone(), request.clone())));
    let cancellation = CancellationToken::new();
    if let Err(message) = remember_run(
        StoredExplorerRun {
            owner: run_owner(&state),
            cancellation: cancellation.clone(),
            run: Arc::clone(&run),
        },
        run_id,
    ) {
        return internal_error(message);
    }
    let initial = run.read().await.clone();
    let response = envelope_for_run(&state, initial);
    tokio::spawn(execute_query(
        state,
        request,
        Arc::clone(&run),
        cancellation,
    ));
    (StatusCode::ACCEPTED, Json(response)).into_response()
}

pub async fn query_status(
    State(state): State<DashboardState>,
    Path(run_id): Path<String>,
) -> Response {
    let Some(stored) = find_run(&state, &run_id) else {
        return not_found(&run_id);
    };
    let mut run = stored.run.read().await.clone();
    run.elapsed_micros = run
        .completed_at_micros
        .unwrap_or_else(now_micros)
        .saturating_sub(run.submitted_at_micros);
    Json(envelope_for_run(&state, run)).into_response()
}

pub async fn cancel_query(
    State(state): State<DashboardState>,
    Path(run_id): Path<String>,
) -> Response {
    let Some(stored) = find_run(&state, &run_id) else {
        return not_found(&run_id);
    };
    let mut run = stored.run.write().await;
    if run.state != ExplorerRunStateV1::Pending {
        return (
            StatusCode::CONFLICT,
            Json(json!({"detail": format!("explorer query run is already terminal: {run_id}")})),
        )
            .into_response();
    }
    stored.cancellation.cancel();
    mark_cancelled(&mut run);
    Json(envelope_for_run(&state, run.clone())).into_response()
}

async fn execute_query(
    state: DashboardState,
    request: ExplorerQueryRequestV1,
    run: Arc<RwLock<ExplorerQueryRunV1>>,
    cancellation: CancellationToken,
) {
    {
        let mut current = run.write().await;
        for source in &mut current.sources {
            source.phase = ExplorerSourcePhaseV1::Reading;
        }
    }

    let mut tasks = JoinSet::new();
    for source_id in SOURCE_IDS {
        let state = state.clone();
        let request = request.clone();
        tasks.spawn(async move { (source_id, execute_source(state, request, source_id).await) });
    }

    let mut completed = 0;
    while completed < SOURCE_IDS.len() {
        tokio::select! {
            () = cancellation.cancelled() => {
                tasks.abort_all();
                let mut current = run.write().await;
                if current.state == ExplorerRunStateV1::Pending {
                    mark_cancelled(&mut current);
                }
                return;
            }
            result = tasks.join_next() => {
                let source = match result {
                    Some(Ok((_, source))) => source,
                    Some(Err(error)) => {
                        let current = run.read().await;
                        let source_id = current
                            .sources
                            .iter()
                            .find(|source| source.outcome == ExplorerSourceOutcomeV1::Pending)
                            .map_or(ExplorerSourceIdV1::CodeGraph, |source| source.source_id);
                        ExplorerSourceProgressV1::error(
                            source_id,
                            "source_task_failed",
                            error.to_string(),
                        )
                    }
                    None => break,
                };
                completed += 1;
                let mut current = run.write().await;
                if current.state != ExplorerRunStateV1::Pending {
                    tasks.abort_all();
                    return;
                }
                if let Some(slot) = current
                    .sources
                    .iter_mut()
                    .find(|slot| slot.source_id == source.source_id)
                {
                    *slot = source;
                }
                current.elapsed_micros = now_micros().saturating_sub(current.submitted_at_micros);
            }
        }
    }

    let mut current = run.write().await;
    if current.state != ExplorerRunStateV1::Pending {
        return;
    }
    let every_ready = current
        .sources
        .iter()
        .all(|source| source.outcome == ExplorerSourceOutcomeV1::Ready);
    let complete = every_ready
        && current
            .sources
            .iter()
            .all(|source| source.coverage.is_complete());
    current.completed_at_micros = Some(now_micros());
    current.elapsed_micros = current
        .completed_at_micros
        .unwrap_or(current.submitted_at_micros)
        .saturating_sub(current.submitted_at_micros);
    if complete {
        current.state = ExplorerRunStateV1::Completed;
        current.finality = ExplorerFinalityV1::Complete;
    } else if current
        .sources
        .iter()
        .any(|source| source.outcome == ExplorerSourceOutcomeV1::Ready)
    {
        current.state = ExplorerRunStateV1::Partial;
        current.finality = ExplorerFinalityV1::Partial;
    } else {
        current.state = ExplorerRunStateV1::Error;
        current.finality = ExplorerFinalityV1::Error;
    }
}

fn mark_cancelled(run: &mut ExplorerQueryRunV1) {
    let completed_at = now_micros();
    run.state = ExplorerRunStateV1::Cancelled;
    run.finality = ExplorerFinalityV1::Cancelled;
    run.completed_at_micros = Some(completed_at);
    run.elapsed_micros = completed_at.saturating_sub(run.submitted_at_micros);
    for source in &mut run.sources {
        if source.outcome == ExplorerSourceOutcomeV1::Pending {
            source.phase = ExplorerSourcePhaseV1::Cancelled;
            source.outcome = ExplorerSourceOutcomeV1::Cancelled;
        }
    }
}

async fn execute_source(
    state: DashboardState,
    request: ExplorerQueryRequestV1,
    source_id: ExplorerSourceIdV1,
) -> ExplorerSourceProgressV1 {
    match source_id {
        ExplorerSourceIdV1::CodeGraph => code_source(&state, &request).await,
        ExplorerSourceIdV1::Sessions => session_source(&state, &request).await,
        ExplorerSourceIdV1::Knowledge => knowledge_source(&state, &request).await,
    }
}

async fn code_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
) -> ExplorerSourceProgressV1 {
    let payload =
        match graph_service::search_payload(state, &request.query, request.limit, request.offset)
            .await
        {
            Ok(payload) => payload,
            Err(error) => {
                return ExplorerSourceProgressV1::error(
                    ExplorerSourceIdV1::CodeGraph,
                    "code_graph_read_failed",
                    error,
                );
            }
        };
    let Ok(total) = u64::try_from(payload.total) else {
        return ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::CodeGraph,
            "code_graph_contract_invalid",
            "code graph search returned a negative total",
        );
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
    ready_source(
        ExplorerSourceIdV1::CodeGraph,
        request,
        rows,
        Some(total),
        json!({"query": request.query}),
        "symbols",
        Vec::new(),
    )
}

async fn session_source(
    _state: &DashboardState,
    _request: &ExplorerQueryRequestV1,
) -> ExplorerSourceProgressV1 {
    ExplorerSourceProgressV1::unavailable(
        ExplorerSourceIdV1::Sessions,
        "lcm_temporal_retrieval_not_mounted",
        "canonical temporal retrieval and redaction hydration are not mounted",
    )
}

async fn knowledge_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
) -> ExplorerSourceProgressV1 {
    let rows = match memory_service::fetch_facts(state, &request.query, request.limit).await {
        Ok(rows) => rows,
        Err(_error) => {
            return ExplorerSourceProgressV1::error(
                ExplorerSourceIdV1::Knowledge,
                "knowledge_query_failed",
                "knowledge query unavailable",
            );
        }
    };
    ready_source(
        ExplorerSourceIdV1::Knowledge,
        request,
        rows,
        None,
        json!({"authority": "bounded project fact overview"}),
        "facts",
        vec![
            "matching fact total is not exposed by the bounded compatibility authority".to_owned(),
        ],
    )
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
    let next_offset = total
        .filter(|total| request.offset as u64 + completed < *total)
        .map(|_| request.offset.saturating_add(request.limit));
    ExplorerSourceProgressV1 {
        source_id,
        source_label: source_id.label(),
        phase: ExplorerSourcePhaseV1::Completed,
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
            next_offset,
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
    summary_node_count: i64,
    token_estimate_total: i64,
    summary_token_count: i64,
    source_token_count: i64,
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
    Path(_session_id): Path<String>,
) -> Response {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(&state),
        None::<ExplorerSessionSizeV1>,
        "lcm_temporal_retrieval_not_mounted",
    ))
    .into_response()
}

pub async fn read_context(
    State(state): State<DashboardState>,
    Path(_session_id): Path<String>,
    Query(_params): Query<ReadContextParams>,
) -> Response {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(&state),
        None::<ExplorerReadContextV1>,
        "lcm_temporal_retrieval_not_mounted",
    ))
    .into_response()
}
