//! Explorer planner/coordinator and session-context HTTP binding.
//!
//! The coordinator composes existing graph, session, and memory authorities.
//! It preserves source-local ordering and coverage instead of manufacturing a
//! cross-source rank. Query cancellation is bound to the opaque run identity
//! stored here and reaches the live Tokio work through `CancellationToken`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::task::JoinSet;

mod knowledge;
mod lifecycle;
mod semantic;

use lifecycle::{admitted_deadline_elapsed, mark_cancelled, mark_timed_out};
pub use semantic::{ExplorerSemanticReadFuture, ExplorerSemanticReadV1, ExplorerSemanticReader};

use super::lcm_api::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadOutcomeV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1, LcmMessageV1, LcmSummaryNodeV1, LcmTokenCountProvenanceV1,
};
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, now_micros, scope_from_state,
};
use super::{DashboardHttpRequestControlV1, DashboardState, graph_service};
use crate::application::context::CancellationToken;
use crate::request_identity::{GlobalOpaqueIdentityKind, mint_global_opaque_id};

const SOURCE_IDS: [ExplorerSourceIdV1; 4] = [
    ExplorerSourceIdV1::CodeGraph,
    ExplorerSourceIdV1::Sessions,
    ExplorerSourceIdV1::Knowledge,
    ExplorerSourceIdV1::Semantic,
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
    Semantic,
}

impl ExplorerSourceIdV1 {
    const fn label(self) -> &'static str {
        match self {
            Self::CodeGraph => "Code graph",
            Self::Sessions => "Sessions",
            Self::Knowledge => "Knowledge",
            Self::Semantic => "Semantic",
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
    TimedOut,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerFinalityV1 {
    Pending,
    Complete,
    Partial,
    Cancelled,
    TimedOut,
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

/// What one source truthfully concluded for this run. Every member has a real
/// producer; none is speculative:
/// - `Partial`: the source answered with rows but its own read reported
///   omitted records (LCM temporal reads).
/// - `Indexing`: the provider is acquiring its model or projecting vectors
///   (semantic runtime acquisition/indexing states).
/// - `Stale`: the source's store exists but does not match the current
///   generation (verified graph stale reads, LCM stale projections, semantic
///   generation staleness).
/// - `TimedOut`: the source's own read exceeded the admitted deadline.
/// - `Unsupported`: this dashboard surface cannot consult the source at all
///   (no daemon authority attached, or the provider state cannot be consumed
///   on this surface yet).
/// - `Absent`: the source's store does not exist for this project — a typed
///   absence, not a failure (semantic search not activated).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExplorerSourceOutcomeV1 {
    Pending,
    Ready,
    Partial,
    Indexing,
    Stale,
    TimedOut,
    Unavailable,
    Unsupported,
    Absent,
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

    fn unsupported(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Unsupported,
            ..Self::unavailable(source_id, error_code, message)
        }
    }

    fn indexing(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Indexing,
            ..Self::unavailable(source_id, error_code, message)
        }
    }

    /// A typed absence: the source's store does not exist for this project.
    /// Coverage is the complete accounting of an empty domain, so absence
    /// claims over the run stay checkable instead of being blocked forever by
    /// a source that holds nothing.
    fn absent(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
        unit: &'static str,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Absent,
            completed_units: Some(0),
            total_units: Some(0),
            coverage: DashboardCoverageV1::complete(0, unit),
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
    required_source_ids: [ExplorerSourceIdV1; 4],
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

fn run_owner(state: &DashboardState) -> Option<String> {
    state
        .resolved_scope
        .as_ref()
        .map(|scope| scope.scope_digest.to_string())
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
        explanation: "Search the code graph, active-project session store, and bounded project fact authority in parallel, and consult the semantic provider's typed state; preserve each source's own order and coverage.",
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
        ExplorerRunStateV1::TimedOut => DashboardDomainStateV1::TimedOut,
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
        .filter(|stored| run_owner(state).as_ref() == Some(&stored.owner))
        .cloned()
}

pub async fn create_query(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    Json(mut request): Json<ExplorerQueryRequestV1>,
) -> Response {
    if let Err(message) = validate_query(&mut request) {
        return bad_request(message);
    }
    let Some(owner) = run_owner(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail": "exact registered project scope is unavailable"})),
        )
            .into_response();
    };
    let Some(Extension(control)) = control else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail": "dashboard HTTP request admission is unavailable"})),
        )
            .into_response();
    };
    let Some(run_id) = new_run_id() else {
        return internal_error("could not allocate explorer query run identity");
    };
    let run = Arc::new(RwLock::new(initial_run(run_id.clone(), request.clone())));
    let cancellation = CancellationToken::new();
    if let Err(message) = remember_run(
        StoredExplorerRun {
            owner,
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
        control,
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
    control: DashboardHttpRequestControlV1,
) {
    let request_cancellation = control.cancellation().clone();
    let deadline = control.deadline();
    let deadline_wait = admitted_deadline_elapsed(deadline.clone());
    tokio::pin!(deadline_wait);
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
        let control = control.clone();
        let source_cancellation = cancellation.clone();
        tasks.spawn(async move {
            (
                source_id,
                execute_source(state, request, source_id, control, source_cancellation).await,
            )
        });
    }

    let mut completed = 0;
    while completed < SOURCE_IDS.len() {
        tokio::select! {
            biased;
            () = &mut deadline_wait => {
                tasks.abort_all();
                let mut current = run.write().await;
                if current.state == ExplorerRunStateV1::Pending {
                    mark_timed_out(&mut current);
                }
                return;
            }
            () = cancellation.cancelled() => {
                tasks.abort_all();
                let mut current = run.write().await;
                if current.state == ExplorerRunStateV1::Pending {
                    mark_cancelled(&mut current);
                }
                return;
            }
            () = request_cancellation.cancelled() => {
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
    if deadline.is_elapsed_at(crate::application::context::application_observed_at()) {
        mark_timed_out(&mut current);
        return;
    }
    if cancellation.is_cancelled() || request_cancellation.is_cancelled() {
        mark_cancelled(&mut current);
        return;
    }
    // `Absent` is a truthful terminal answer ("this store does not exist"),
    // so it counts toward completion alongside `Ready` — its constructor
    // carries the complete accounting of an empty domain.
    let every_concluded = current.sources.iter().all(|source| {
        matches!(
            source.outcome,
            ExplorerSourceOutcomeV1::Ready | ExplorerSourceOutcomeV1::Absent
        )
    });
    let complete = every_concluded
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
    } else if current.sources.iter().any(|source| {
        matches!(
            source.outcome,
            ExplorerSourceOutcomeV1::Ready
                | ExplorerSourceOutcomeV1::Partial
                | ExplorerSourceOutcomeV1::Absent
        )
    }) {
        current.state = ExplorerRunStateV1::Partial;
        current.finality = ExplorerFinalityV1::Partial;
    } else {
        current.state = ExplorerRunStateV1::Error;
        current.finality = ExplorerFinalityV1::Error;
    }
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
        ExplorerSourceIdV1::Semantic => semantic::semantic_source(&state).await,
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
    let mut source = ready_source(
        ExplorerSourceIdV1::CodeGraph,
        request,
        rows,
        Some(total),
        json!({"query": request.query}),
        "symbols",
        Vec::new(),
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
        CodeGraphReadError::Unavailable { detail } => ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::CodeGraph,
            "graph_authority_unavailable",
            detail,
        ),
        CodeGraphReadError::Stale { detail } => ExplorerSourceProgressV1::stale(
            ExplorerSourceIdV1::CodeGraph,
            "graph_generation_stale",
            detail,
        ),
        CodeGraphReadError::Cancelled => {
            let mut source = ExplorerSourceProgressV1::unavailable(
                ExplorerSourceIdV1::CodeGraph,
                "graph_read_cancelled",
                "the graph read was cancelled",
            );
            source.phase = ExplorerSourcePhaseV1::Cancelled;
            source.outcome = ExplorerSourceOutcomeV1::Cancelled;
            source
        }
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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    Path(session_id): Path<String>,
) -> Response {
    let Some(Extension(control)) = control else {
        return explorer_session_not_ready::<ExplorerSessionSizeV1>(
            &state,
            DashboardLcmReadStateV1::Unavailable,
            "dashboard_request_admission_unavailable".to_owned(),
        );
    };
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
        } => explorer_session_not_ready::<ExplorerSessionSizeV1>(&state, read_state, reason),
    }
}

pub async fn read_context(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    Path(session_id): Path<String>,
    Query(params): Query<ReadContextParams>,
) -> Response {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let Some(Extension(control)) = control else {
        return explorer_session_not_ready::<ExplorerReadContextV1>(
            &state,
            DashboardLcmReadStateV1::Unavailable,
            "dashboard_request_admission_unavailable".to_owned(),
        );
    };
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
            read_session_page(&state, control.clone(), &session_id, limit, cursor.take()).await;
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
    let has_more_messages = stats.message_count > returned_messages;
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
        DashboardLcmReadStateV1::Unavailable => {
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
    }
}
