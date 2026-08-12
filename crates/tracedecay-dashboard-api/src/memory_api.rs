//! Holographic-memory dashboard API, backed by tracedecay's memory store.
//!
//! Canonical fact payloads come from the relational fact authority, verified
//! memory topology comes from Grafeo, and FHRR vectors are derived on read.

use std::collections::BTreeMap;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::memory_analysis::{
    SIMILARITY_DEFAULT_THRESHOLD, SIMILARITY_PAIR_CAP, empty_score_distribution,
};
use super::memory_service;
use super::read_model::{
    DashboardCoverageCompletenessV1, DashboardCoverageV1, DashboardDomainStateV1,
    DashboardEnvelopeV1, DashboardFreshnessV1, scope_from_state,
};
use super::util::{JsonPath, JsonQuery, coerce_limit, http_detail};
use super::{DashboardHttpRequestControlV1, DashboardState};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_domain::FactId;
use tracedecay_store::FactReadControl;

pub(crate) mod control;
mod overview_contract;

use control::{
    fact_read_control, read_error_envelope, request_deadline_elapsed, request_terminal_state,
    terminal_read_code,
};
pub(super) use overview_contract::MemoryOverviewPayloadV1;
use overview_contract::{
    MemoryEntityRowV1, MemoryFactRowV1, MemoryHolographicPayloadV1, MemoryOverviewSummaryV1,
    MemoryReadStatusV1,
};

#[derive(Deserialize)]
pub struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    graph_limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct ProjectionParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SimilarityParams {
    min_similarity: Option<f64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct LimitParams {
    limit: Option<i64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryAlgebraStatusV1 {
    name: String,
    hrr_dim: u64,
    estimated_capacity: u64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryFeedbackFunnelV1 {
    retrieval_count_total: u64,
    access_count_total: u64,
    retrieved_fact_count: u64,
    rated_fact_count: u64,
    feedback_total: u64,
    seen_to_feedback_ratio: Option<u64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryStatusV1 {
    fact_count: u64,
    entity_count: u64,
    algebra: MemoryAlgebraStatusV1,
    trust_0_025_count: u64,
    trust_025_050_count: u64,
    trust_050_075_count: u64,
    trust_075_100_count: u64,
    below_default_recall_threshold_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    feedback_funnel: MemoryFeedbackFunnelV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryStatusPayloadV1 {
    path: String,
    exists: bool,
    memory: MemoryStatusV1,
    error: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryFactDetailPayloadV1 {
    fact: Option<MemoryFactRowV1>,
    error: String,
}

fn owned_fact_id(state: &DashboardState, raw: String) -> Result<FactId, String> {
    let fact_id = FactId::new(raw).map_err(|error| error.to_string())?;
    fact_id
        .validate_owner(&state.memory_owner)
        .map_err(|error| error.to_string())?;
    Ok(fact_id)
}

fn facts_read_status(coverage: &memory_service::MemoryFactsCoverageV1) -> MemoryReadStatusV1 {
    let graph_complete = matches!(
        coverage.graph,
        None | Some(
            tracedecay_application::memory::FactSearchGraphCoverageV1::Complete { .. }
                | tracedecay_application::memory::FactSearchGraphCoverageV1::NotApplicable
        )
    );
    if coverage.completeness == DashboardCoverageCompletenessV1::Complete && graph_complete {
        MemoryReadStatusV1::new(DashboardDomainStateV1::Ready)
    } else {
        MemoryReadStatusV1 {
            state: DashboardDomainStateV1::Partial,
            code: Some("fact_coverage_incomplete".to_owned()),
            error: None,
        }
    }
}

fn graph_read_status(coverage: &DashboardCoverageV1) -> MemoryReadStatusV1 {
    if coverage.is_complete() {
        MemoryReadStatusV1::new(DashboardDomainStateV1::Ready)
    } else {
        MemoryReadStatusV1 {
            state: DashboardDomainStateV1::Partial,
            code: Some("graph_coverage_incomplete".to_owned()),
            error: None,
        }
    }
}

fn read_failure_status(
    control: &DashboardHttpRequestControlV1,
    code: Option<&str>,
    error: impl Into<String>,
) -> MemoryReadStatusV1 {
    let error = error.into();
    if let Some(state) = request_terminal_state(control) {
        return MemoryReadStatusV1::failed(state, Some(terminal_read_code(state).0), error);
    }
    MemoryReadStatusV1::failed(DashboardDomainStateV1::Error, code, error)
}

async fn memory_status_payload(
    state: &DashboardState,
    read_control: &FactReadControl,
) -> Result<MemoryStatusPayloadV1, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let typed_status = application
        .dashboard_memory_status(read_control)
        .await
        .map_err(|error| error.to_string())?;
    let funnel = typed_status.feedback_funnel();
    let status = MemoryStatusV1 {
        fact_count: typed_status.fact_count(),
        entity_count: typed_status.entity_count(),
        algebra: MemoryAlgebraStatusV1 {
            name: typed_status.algebra().name().to_owned(),
            hrr_dim: typed_status.algebra().hrr_dim(),
            estimated_capacity: typed_status.algebra().estimated_capacity(),
        },
        trust_0_025_count: typed_status.trust_0_025_count(),
        trust_025_050_count: typed_status.trust_025_050_count(),
        trust_050_075_count: typed_status.trust_050_075_count(),
        trust_075_100_count: typed_status.trust_075_100_count(),
        below_default_recall_threshold_count: typed_status.below_default_recall_threshold_count(),
        helpful_count: typed_status.helpful_count(),
        unhelpful_count: typed_status.unhelpful_count(),
        feedback_funnel: MemoryFeedbackFunnelV1 {
            retrieval_count_total: funnel.retrieval_count_total(),
            access_count_total: funnel.access_count_total(),
            retrieved_fact_count: funnel.retrieved_fact_count(),
            rated_fact_count: funnel.rated_fact_count(),
            feedback_total: funnel.feedback_total(),
            seen_to_feedback_ratio: funnel.seen_to_feedback_ratio(),
        },
    };
    Ok(MemoryStatusPayloadV1 {
        path: state.mem_db_path.clone(),
        exists: true,
        memory: status,
        error: String::new(),
    })
}

async fn fact_trust_history_payload(
    state: &DashboardState,
    fact_id: FactId,
    read_control: &FactReadControl,
) -> Result<Option<Value>, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let Some(_detail) = application
        .dashboard_fact_detail(fact_id.clone(), read_control)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    const HISTORY_LIMIT: usize = 300;
    let history = application
        .dashboard_feedback_history(fact_id.clone(), HISTORY_LIMIT, read_control)
        .await
        .map_err(|error| error.to_string())?;
    let trust_history: Vec<Value> = history
        .events()
        .iter()
        .map(|event| {
            let action = match event.action() {
                tracedecay_store::ProjectMemoryFactFeedbackActionV1::Helpful => "helpful",
                tracedecay_store::ProjectMemoryFactFeedbackActionV1::Unhelpful => "unhelpful",
            };
            let availability = match event.details_availability() {
                tracedecay_store::ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available => {
                    "available"
                }
                tracedecay_store::ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted => {
                    "redacted"
                }
                tracedecay_store::ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown => {
                    "unknown"
                }
            };
            let mut row = Map::new();
            row.insert("event_id".into(), json!(event.event_id().as_str()));
            row.insert("timestamp".into(), json!(event.occurred_at().0));
            row.insert("action".into(), json!(action));
            row.insert("old_trust".into(), json!(event.old_trust().as_f64()));
            row.insert("new_trust".into(), json!(event.new_trust().as_f64()));
            row.insert(
                "delta".into(),
                json!(event.new_trust().as_f64() - event.old_trust().as_f64()),
            );
            row.insert("details_availability".into(), json!(availability));
            if let Some(source) = event.source() {
                row.insert("source".into(), json!(source));
            }
            if let Some(note) = event.note() {
                row.insert("note".into(), json!(note));
            }
            Value::Object(row)
        })
        .collect();
    let next_after = history.next_after().map(|cursor| {
        json!({
            "occurred_at": cursor.occurred_at().0,
            "event_id": cursor.event_id().as_str(),
        })
    });
    Ok(Some(json!({
        "fact_id": fact_id.as_str(),
        "trust_history": trust_history,
        "limit": HISTORY_LIMIT,
        "completeness": if next_after.is_some() { "partial" } else { "complete" },
        "next_after": next_after,
        "error": "",
    })))
}

/// `GET /api/plugins/holographic/` — overview + facts + entities + graph.
pub async fn overview(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> Json<DashboardEnvelopeV1<Option<MemoryOverviewPayloadV1>>> {
    let Some(Extension(control)) = control else {
        return Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            "dashboard HTTP request admission is unavailable",
        ));
    };
    let read_control = fact_read_control(&control);
    let limit = coerce_limit(params.limit, 25, memory_service::MEMORY_FACT_LIMIT_MAXIMUM);
    let graph_limit = coerce_limit(params.graph_limit, limit, 1000);
    let Ok(fact_limit) = usize::try_from(limit) else {
        return Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            "memory fact limit is outside the platform range",
        ));
    };
    let Ok(initial_relation_limit) = usize::try_from(graph_limit) else {
        return Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            "memory graph limit is outside the platform range",
        ));
    };

    let mut reads = BTreeMap::from([
        (
            "facts".to_owned(),
            MemoryReadStatusV1::new(DashboardDomainStateV1::Loading),
        ),
        (
            "entities".to_owned(),
            MemoryReadStatusV1::new(DashboardDomainStateV1::Loading),
        ),
        (
            "graph".to_owned(),
            MemoryReadStatusV1::new(DashboardDomainStateV1::Loading),
        ),
    ]);
    let mut holographic = MemoryHolographicPayloadV1 {
        path: state.mem_db_path.clone(),
        exists: true,
        overview: None,
        facts: Vec::new(),
        entities: Vec::new(),
        graph: memory_service::MemoryGraphPayloadV1 {
            nodes: Vec::new(),
            edges: Vec::new(),
            coverage: DashboardCoverageV1::unknown(),
            fact_universe_count: 0,
            fact_candidates_examined: 0,
            unavailable_fact_candidates: 0,
            root_count: 0,
            relation_limit: initial_relation_limit,
            relation_count: 0,
        },
        error: String::new(),
        reads: BTreeMap::new(),
        facts_coverage: memory_service::MemoryFactsCoverageV1 {
            completeness: DashboardCoverageCompletenessV1::Partial,
            limit: fact_limit,
            graph: None,
            examined: None,
            eligible: None,
        },
    };
    let mut overview_ready = false;
    match memory_service::overview_payload(&state, &read_control).await {
        Ok(payload) => match serde_json::from_value::<MemoryOverviewSummaryV1>(payload) {
            Ok(payload) => {
                holographic.overview = Some(payload);
                overview_ready = true;
            }
            Err(error) => {
                holographic.error = format!("Failed to decode memory summary: {error}");
            }
        },
        Err(error) => {
            holographic.error = error;
        }
    }
    if let Some(state) = request_terminal_state(&control) {
        reads.insert(
            "facts".to_owned(),
            MemoryReadStatusV1::failed(state, None, "request lifecycle ended"),
        );
    } else {
        match memory_service::fetch_facts(&state, &params.q, limit, &read_control).await {
            Ok(facts) => {
                let rows = facts
                    .rows
                    .into_iter()
                    .map(serde_json::from_value::<MemoryFactRowV1>)
                    .collect::<Result<Vec<_>, _>>();
                match rows {
                    Ok(rows) => {
                        holographic.facts = rows;
                        let read_status = facts_read_status(&facts.coverage);
                        holographic.facts_coverage = facts.coverage;
                        reads.insert("facts".to_owned(), read_status);
                    }
                    Err(error) => {
                        reads.insert(
                            "facts".to_owned(),
                            read_failure_status(
                                &control,
                                Some("fact_contract_decode_failed"),
                                error.to_string(),
                            ),
                        );
                    }
                }
            }
            Err(error) => {
                reads.insert(
                    "facts".to_owned(),
                    read_failure_status(&control, None, error),
                );
            }
        }
    }
    if let Some(state) = request_terminal_state(&control) {
        reads.insert(
            "entities".to_owned(),
            MemoryReadStatusV1::failed(state, None, "request lifecycle ended"),
        );
    } else {
        match memory_service::fetch_entities(&state, limit, &read_control).await {
            Ok(entities) => {
                let rows = entities
                    .rows
                    .into_iter()
                    .map(serde_json::from_value::<MemoryEntityRowV1>)
                    .collect::<Result<Vec<_>, _>>();
                match rows {
                    Ok(rows) => {
                        holographic.entities = rows;
                        reads.insert(
                            "entities".to_owned(),
                            if entities.bounded {
                                MemoryReadStatusV1 {
                                    state: DashboardDomainStateV1::Partial,
                                    code: Some("entity_limit_reached".to_owned()),
                                    error: None,
                                }
                            } else {
                                MemoryReadStatusV1::new(DashboardDomainStateV1::Ready)
                            },
                        );
                    }
                    Err(error) => {
                        reads.insert(
                            "entities".to_owned(),
                            read_failure_status(
                                &control,
                                Some("entity_contract_decode_failed"),
                                error.to_string(),
                            ),
                        );
                    }
                }
            }
            Err(error) => {
                reads.insert(
                    "entities".to_owned(),
                    read_failure_status(&control, None, error),
                );
            }
        }
    }
    if let Some(state) = request_terminal_state(&control) {
        reads.insert(
            "graph".to_owned(),
            MemoryReadStatusV1::failed(state, None, "request lifecycle ended"),
        );
    } else {
        match memory_service::graph_payload(&state, &params.q, graph_limit, &control, &read_control)
            .await
        {
            Ok(graph) => {
                let read_status = graph_read_status(&graph.coverage);
                holographic.graph = graph;
                reads.insert("graph".to_owned(), read_status);
            }
            Err(error) => {
                reads.insert(
                    "graph".to_owned(),
                    MemoryReadStatusV1::failed(error.state(), Some(error.code()), error.message()),
                );
            }
        }
    }
    holographic.reads = reads;
    let request_timed_out = request_deadline_elapsed(&control);
    let request_cancelled = control.cancellation().is_cancelled();
    let ready_read_count = holographic
        .reads
        .values()
        .filter(|read| read.state == DashboardDomainStateV1::Ready)
        .count();
    let facts_complete = matches!(
        holographic.facts_coverage.completeness,
        DashboardCoverageCompletenessV1::Complete
    );
    let graph_complete = holographic.graph.coverage.is_complete();
    let exact_complete = overview_ready && ready_read_count == 3;
    let domain_state = if request_timed_out {
        DashboardDomainStateV1::TimedOut
    } else if request_cancelled {
        DashboardDomainStateV1::Cancelled
    } else if exact_complete {
        DashboardDomainStateV1::Ready
    } else if overview_ready || ready_read_count != 0 {
        DashboardDomainStateV1::Partial
    } else {
        holographic
            .reads
            .get("graph")
            .map_or(DashboardDomainStateV1::Error, |read| read.state)
    };
    let coverage = if exact_complete && !request_timed_out && !request_cancelled {
        DashboardCoverageV1::complete(4, "applicable_memory_read_sources")
    } else {
        let mut coverage = DashboardCoverageV1::unknown();
        if request_timed_out {
            coverage
                .omission_reasons
                .push("request_deadline_elapsed".into());
        } else if request_cancelled {
            coverage.omission_reasons.push("request_cancelled".into());
        }
        if !overview_ready {
            coverage
                .omission_reasons
                .push("overview_read_failed".into());
        }
        for read in ["facts", "entities", "graph"] {
            if holographic
                .reads
                .get(read)
                .is_none_or(|status| status.state != DashboardDomainStateV1::Ready)
            {
                coverage
                    .omission_reasons
                    .push(format!("{read}_read_incomplete"));
            }
        }
        if !facts_complete {
            coverage.omission_reasons.push("fact_rows_bounded".into());
        }
        if !graph_complete
            && holographic
                .reads
                .get("graph")
                .is_some_and(|status| status.state == DashboardDomainStateV1::Ready)
        {
            coverage.omission_reasons.push("graph_rows_bounded".into());
        }
        coverage
    };
    let freshness =
        if !request_timed_out && !request_cancelled && overview_ready && ready_read_count == 3 {
            DashboardFreshnessV1::fresh_now()
        } else {
            DashboardFreshnessV1::unknown()
        };
    let providers = match serde_json::from_value(memory_service::providers_payload()) {
        Ok(providers) => providers,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                format!("Failed to encode memory provider contract: {error}"),
            ));
        }
    };
    let payload = MemoryOverviewPayloadV1 {
        providers,
        query: params.q,
        limit,
        holographic,
    };
    Json(DashboardEnvelopeV1::new(
        scope_from_state(&state),
        domain_state,
        coverage,
        freshness,
        Some(payload),
    ))
}

/// `GET /api/plugins/holographic/status` — canonical facts and derived-algebra health.
pub async fn status(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
) -> Json<DashboardEnvelopeV1<Option<MemoryStatusPayloadV1>>> {
    let Some(Extension(control)) = control else {
        return Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            "dashboard HTTP request admission is unavailable",
        ));
    };
    let result = memory_status_payload(&state, &fact_read_control(&control)).await;
    if let Some(state_label) = request_terminal_state(&control) {
        return Json(read_error_envelope(
            scope_from_state(&state),
            &control,
            None,
            terminal_read_code(state_label).1,
        ));
    }
    match result {
        Ok(payload) => Json(DashboardEnvelopeV1::ready(
            scope_from_state(&state),
            DashboardCoverageV1::complete(1, "memory_stores"),
            Some(payload),
        )),
        Err(error) => Json(read_error_envelope(
            scope_from_state(&state),
            &control,
            None,
            format!("Failed to compute memory status: {error}"),
        )),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}` — full fact detail.
///
/// List and projection payloads truncate `content` to 200 chars to keep them
/// light; detail panels (e.g. the Semantic Map's pinned card) fetch the
/// complete row — plus linked entities — from here.
pub async fn fact_detail(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(fact_id): JsonPath<String>,
) -> Json<DashboardEnvelopeV1<Option<MemoryFactDetailPayloadV1>>> {
    let Some(Extension(control)) = control else {
        return Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            "dashboard HTTP request admission is unavailable",
        ));
    };
    let fact_id = match owned_fact_id(&state, fact_id) {
        Ok(fact_id) => fact_id,
        Err(error) => {
            return Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                format!("invalid canonical fact id: {error}"),
            ));
        }
    };
    let result =
        memory_service::fact_detail_payload(&state, fact_id, &fact_read_control(&control)).await;
    if let Some(state_label) = request_terminal_state(&control) {
        return Json(read_error_envelope(
            scope_from_state(&state),
            &control,
            None,
            terminal_read_code(state_label).1,
        ));
    }
    match result {
        Ok(Some(payload)) => match serde_json::from_value::<MemoryFactDetailPayloadV1>(payload) {
            Ok(payload) => Json(DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                DashboardCoverageV1::complete(1, "facts"),
                Some(payload),
            )),
            Err(error) => Json(DashboardEnvelopeV1::error(
                scope_from_state(&state),
                None,
                format!("Failed to encode memory fact detail contract: {error}"),
            )),
        },
        Ok(None) => Json(DashboardEnvelopeV1::complete_zero_findings(
            scope_from_state(&state),
            DashboardCoverageV1::complete(1, "facts"),
            None,
        )),
        Err(error) => Json(read_error_envelope(
            scope_from_state(&state),
            &control,
            None,
            error,
        )),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}/trust-history` — append-only
/// feedback audit rows explaining how a fact's trust changed over time.
pub async fn fact_trust_history(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(fact_id): JsonPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(Extension(control)) = control else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(http_detail(
                "dashboard HTTP request admission is unavailable",
            )),
        );
    };
    let fact_id = match owned_fact_id(&state, fact_id) {
        Ok(fact_id) => fact_id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(http_detail(&format!("invalid canonical fact id: {error}"))),
            );
        }
    };
    let fact_id_label = fact_id.as_str().to_owned();
    let result = fact_trust_history_payload(&state, fact_id, &fact_read_control(&control)).await;
    if let Some(state) = request_terminal_state(&control) {
        let (code, detail) = terminal_read_code(state);
        return (
            if state == DashboardDomainStateV1::TimedOut {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::REQUEST_TIMEOUT
            },
            Json(json!({"detail": detail, "code": code})),
        );
    }
    match result {
        Ok(Some(payload)) => (StatusCode::OK, Json(payload)),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("fact not found: {fact_id_label}"))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to load trust history for fact {fact_id_label}: {e}"
            ))),
        ),
    }
}

/// `GET /api/plugins/holographic/projection` — 2D PCA of phase vectors,
/// embedded as `[cos(p), sin(p)]` so wrapped phases compare correctly.
pub async fn projection(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<ProjectionParams>,
) -> Json<Value> {
    let Some(Extension(control)) = control else {
        return Json(json!({
            "exists": true,
            "dim": 0,
            "limit": 0,
            "method": "none",
            "points": [],
            "error": "dashboard HTTP request admission is unavailable",
        }));
    };
    let limit = coerce_limit(params.limit, 25, memory_service::projection_point_cap());
    let payload =
        memory_service::projection_payload(&state, &params.q, limit, &fact_read_control(&control))
            .await;
    if let Some(state) = request_terminal_state(&control) {
        let (code, error) = terminal_read_code(state);
        return Json(json!({
            "exists": true,
            "dim": 0,
            "limit": limit,
            "method": "none",
            "points": [],
            "state": state,
            "code": code,
            "error": error,
        }));
    }
    Json(payload)
}

/// `GET /api/plugins/holographic/similarity` — pairwise phase-cosine
/// similarity (`mean(cos(p_i − p_j))`) over query-time derived vectors.
pub async fn similarity(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<SimilarityParams>,
) -> Json<Value> {
    let Some(Extension(control)) = control else {
        return Json(json!({
            "exists": true,
            "dim": 0,
            "count": 0,
            "limit": 0,
            "min_similarity": null,
            "total_pairs": 0,
            "score_distribution": empty_score_distribution(),
            "pairs": [],
            "error": "dashboard HTTP request admission is unavailable",
        }));
    };
    let min_similarity = memory_service::coerce_similarity_score(
        params.min_similarity,
        SIMILARITY_DEFAULT_THRESHOLD,
    );
    let pair_cap = coerce_limit(params.limit, 25, SIMILARITY_PAIR_CAP) as usize;
    let payload = memory_service::similarity_payload(
        &state,
        min_similarity,
        pair_cap,
        &fact_read_control(&control),
    )
    .await;
    if let Some(state) = request_terminal_state(&control) {
        let (code, error) = terminal_read_code(state);
        return Json(json!({
            "exists": true,
            "dim": 0,
            "count": 0,
            "limit": pair_cap,
            "min_similarity": min_similarity,
            "total_pairs": 0,
            "score_distribution": empty_score_distribution(),
            "pairs": [],
            "state": state,
            "code": code,
            "error": error,
        }));
    }
    Json(payload)
}

/// `GET /api/plugins/holographic/oplog` — recent canonical memory operations,
/// newest first, with optional canonical fact identity.
pub async fn oplog(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let Some(Extension(control)) = control else {
        return Json(json!({
            "events": [],
            "count": 0,
            "limit": 0,
            "error": "dashboard HTTP request admission is unavailable",
        }));
    };
    let limit = coerce_limit(params.limit, 50, 300);
    let payload = memory_service::oplog_payload(&state, limit, &fact_read_control(&control)).await;
    if let Some(state) = request_terminal_state(&control) {
        let (code, error) = terminal_read_code(state);
        return Json(json!({
            "events": [],
            "count": 0,
            "limit": limit,
            "state": state,
            "code": code,
            "error": error,
        }));
    }
    Json(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::memory::{FactSearchGraphCoverageV1, FactSearchGraphDegradationV1};

    fn fact_coverage(
        completeness: DashboardCoverageCompletenessV1,
        graph: Option<FactSearchGraphCoverageV1>,
    ) -> memory_service::MemoryFactsCoverageV1 {
        memory_service::MemoryFactsCoverageV1 {
            completeness,
            limit: 10,
            graph,
            examined: None,
            eligible: None,
        }
    }

    fn request_control(
        cancellation: tracedecay_application::CancellationSignal,
        deadline: i64,
    ) -> DashboardHttpRequestControlV1 {
        DashboardHttpRequestControlV1 {
            request_id: tracedecay_application::RequestId::new(
                "request.dashboard-memory-status-test",
            )
            .expect("request identity"),
            deadline: tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(deadline))
                .expect("request deadline"),
            cancellation,
            observed_at: tracedecay_domain::UtcMicros(1),
        }
    }

    #[test]
    fn bounded_or_degraded_memory_reads_are_partial_not_ready() {
        let graph_complete = FactSearchGraphCoverageV1::Complete {
            root_count: 1,
            relation_count: 1,
            expanded_fact_count: 1,
        };
        assert_eq!(
            facts_read_status(&fact_coverage(
                DashboardCoverageCompletenessV1::Complete,
                Some(graph_complete),
            ))
            .state,
            DashboardDomainStateV1::Ready,
        );
        assert_eq!(
            facts_read_status(&fact_coverage(
                DashboardCoverageCompletenessV1::Partial,
                None,
            ))
            .state,
            DashboardDomainStateV1::Partial,
        );
        assert_eq!(
            facts_read_status(&fact_coverage(
                DashboardCoverageCompletenessV1::Complete,
                Some(FactSearchGraphCoverageV1::Degraded {
                    reason: FactSearchGraphDegradationV1::Unavailable,
                }),
            ))
            .state,
            DashboardDomainStateV1::Partial,
        );
        assert_eq!(
            graph_read_status(&DashboardCoverageV1::unknown()).state,
            DashboardDomainStateV1::Partial,
        );
        assert_eq!(
            graph_read_status(&DashboardCoverageV1::complete(1, "memory_graph_roots")).state,
            DashboardDomainStateV1::Ready,
        );
    }

    #[test]
    fn read_failures_preserve_live_request_terminal_state() {
        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-status-test",
        )
        .expect("cancellation signal");
        let control = request_control(cancellation.clone(), i64::MAX);
        assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
        assert_eq!(
            read_failure_status(&control, None, "cancelled fixture").state,
            DashboardDomainStateV1::Cancelled,
        );

        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-timeout-status-test",
        )
        .expect("cancellation signal");
        let control = request_control(cancellation, 0);
        assert_eq!(
            read_failure_status(&control, None, "timed out fixture").state,
            DashboardDomainStateV1::TimedOut,
        );
    }
}
