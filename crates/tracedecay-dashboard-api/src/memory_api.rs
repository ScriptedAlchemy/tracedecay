//! Holographic-memory dashboard API, backed by tracedecay's memory store.
//!
//! Port of `plugins/memory/holographic_plus/dashboard/plugin_api.py` (Hermes)
//! through the memory application authority. Payload shapes mirror the
//! original routes so the ported UI bundle works unchanged.
//!
//! Differences from the Hermes backend, by design:
//! - `POST /curate/apply` is a generic curation-ops endpoint (`delete` /
//!   `merge`) for validated agent operations.
//! - There is no fact archive: deletion is permanent (the original
//!   `holographic_plus` soft-archived facts; tracedecay does not).
//! - Banks are named after their category directly (no `cat:` prefix).

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::DashboardState;
use super::memory_analysis::{SIMILARITY_DEFAULT_THRESHOLD, SIMILARITY_PAIR_CAP};
use super::memory_service;
use super::util::{JsonPath, JsonQuery, coerce_limit, http_detail};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_runtime_core::memory::types::{
    MemoryFeedbackFunnel, MemoryRepairStats, MemoryStatus,
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

#[derive(Deserialize)]
pub struct FactProposalParams {
    state: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct FactProposalApplyBody {
    reviewer: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FactProposalRejectBody {
    reviewer: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
pub struct CurateApplyBody {
    ops: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryCategoryCountV1 {
    category: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryEntityTypeCountV1 {
    entity_type: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryHrrCoverageV1 {
    category: String,
    facts: i64,
    hrr_vectors: i64,
    coverage: f64,
    bank_name: Option<String>,
    bank_fact_count: Option<i64>,
    dim: Option<i64>,
    updated_at: Option<i64>,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryBankV1 {
    bank_name: String,
    dim: i64,
    fact_count: i64,
    bundled_fact_count: i64,
    updated_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryTrustBucketV1 {
    bucket: i64,
    label: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryGrowthPointV1 {
    date: String,
    facts: i64,
    cumulative_facts: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryOverviewSummaryV1 {
    facts: i64,
    entities: i64,
    banks: i64,
    categories: Vec<MemoryCategoryCountV1>,
    entity_types: Vec<MemoryEntityTypeCountV1>,
    hrr_coverage: Vec<MemoryHrrCoverageV1>,
    memory_banks: Vec<MemoryBankV1>,
    trust_histogram: Vec<MemoryTrustBucketV1>,
    growth: Vec<MemoryGrowthPointV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryReadStatusV1 {
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryFactsCoverageV1 {
    completeness: String,
    limit: i64,
    query_applied_after_limit: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryFactRowV1 {
    fact_id: i64,
    trust_score: f64,
    retrieval_count: i64,
    access_count: i64,
    helpful_count: i64,
    unhelpful_count: i64,
    created_at: i64,
    updated_at: i64,
    last_recalled_at: Option<i64>,
    has_hrr: Option<i64>,
    content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    metadata: Option<Value>,
    entities: Option<Vec<MemoryEntityRowV1>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryEntityRowV1 {
    entity_id: Option<i64>,
    name: String,
    entity_type: Option<String>,
    aliases: Vec<String>,
    created_at: i64,
    fact_count: u64,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct MemoryHolographicPayloadV1 {
    path: String,
    exists: bool,
    overview: Option<MemoryOverviewSummaryV1>,
    facts: Vec<MemoryFactRowV1>,
    entities: Vec<MemoryEntityRowV1>,
    graph: BTreeMap<String, Value>,
    error: String,
    reads: BTreeMap<String, MemoryReadStatusV1>,
    facts_coverage: MemoryFactsCoverageV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryOverviewPayloadV1 {
    providers: BTreeMap<String, Value>,
    query: String,
    limit: i64,
    holographic: MemoryHolographicPayloadV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
struct MemoryFeedbackHistoryRepairV1 {
    state: String,
    processed: u64,
    remaining: Option<u64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct MemoryStatusPayloadV1 {
    path: String,
    exists: bool,
    memory: MemoryStatus,
    largest_bank_fact_count: i64,
    largest_bank_utilization_pct: f64,
    feedback_history_repair: MemoryFeedbackHistoryRepairV1,
    error: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryFactDetailPayloadV1 {
    fact: Option<MemoryFactRowV1>,
    error: String,
}

pub fn default_agent_plan_max_clusters() -> usize {
    crate::memory_curate::CURATION_DEFAULT_MAX_CLUSTERS
}

pub fn default_agent_plan_min_confidence() -> f64 {
    crate::memory_curate::CURATION_DEFAULT_MIN_CONFIDENCE
}

async fn largest_bank_fact_count(state: &DashboardState) -> Result<i64, String> {
    let overview = memory_service::overview_payload(state).await?;
    Ok(overview["memory_banks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|bank| bank.get("fact_count").and_then(Value::as_i64))
        .max()
        .unwrap_or_default())
}

async fn memory_status_payload(state: &DashboardState) -> Result<MemoryStatusPayloadV1, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let typed_status = application
        .dashboard_memory_status_v1()
        .await
        .map_err(|error| error.to_string())?;
    let as_usize = |value: u64| usize::try_from(value).map_err(|error| error.to_string());
    let as_i64 = |value: u64| i64::try_from(value).map_err(|error| error.to_string());
    let funnel = typed_status.feedback_funnel();
    let status = MemoryStatus {
        fact_count: as_usize(typed_status.fact_count())?,
        entity_count: as_usize(typed_status.entity_count())?,
        bank_count: as_usize(typed_status.bank_count())?,
        algebra_name: typed_status.algebra().name().to_owned(),
        hrr_dim: as_usize(typed_status.algebra().hrr_dim())?,
        estimated_capacity: as_usize(typed_status.algebra().estimated_capacity())?,
        trust_0_025_count: as_usize(typed_status.trust_0_025_count())?,
        trust_025_050_count: as_usize(typed_status.trust_025_050_count())?,
        trust_050_075_count: as_usize(typed_status.trust_050_075_count())?,
        trust_075_100_count: as_usize(typed_status.trust_075_100_count())?,
        below_default_recall_threshold_count: as_usize(
            typed_status.below_default_recall_threshold_count(),
        )?,
        helpful_count: as_usize(typed_status.helpful_count())?,
        unhelpful_count: as_usize(typed_status.unhelpful_count())?,
        missing_vector_count: as_usize(typed_status.missing_vector_count())?,
        repair: MemoryRepairStats {
            missing_vectors_repaired: as_usize(typed_status.repair().missing_vectors_repaired())?,
            banks_rebuilt: as_usize(typed_status.repair().banks_rebuilt())?,
        },
        feedback_funnel: MemoryFeedbackFunnel {
            retrieval_count_total: as_i64(funnel.retrieval_count_total())?,
            access_count_total: as_i64(funnel.access_count_total())?,
            retrieved_fact_count: as_usize(funnel.retrieved_fact_count())?,
            rated_fact_count: as_usize(funnel.rated_fact_count())?,
            feedback_total: as_usize(funnel.feedback_total())?,
            seen_to_feedback_ratio: funnel.seen_to_feedback_ratio().map(as_i64).transpose()?,
        },
    };
    let largest_bank_fact_count = largest_bank_fact_count(state).await?;
    let largest_bank_utilization_pct = if status.estimated_capacity > 0 {
        largest_bank_fact_count as f64 / status.estimated_capacity as f64 * 100.0
    } else {
        0.0
    };
    Ok(MemoryStatusPayloadV1 {
        path: state.mem_db_path.clone(),
        exists: true,
        memory: status,
        largest_bank_fact_count,
        largest_bank_utilization_pct,
        feedback_history_repair: MemoryFeedbackHistoryRepairV1 {
            state: match typed_status.feedback_history_repair() {
                tracedecay_store::CompatibilityFeedbackRepairProgressV1::Unknown => "unknown",
                tracedecay_store::CompatibilityFeedbackRepairProgressV1::NotRequired => {
                    "not_required"
                }
                tracedecay_store::CompatibilityFeedbackRepairProgressV1::Complete { .. } => {
                    "complete"
                }
                tracedecay_store::CompatibilityFeedbackRepairProgressV1::Incomplete { .. } => {
                    "incomplete"
                }
            }
            .to_owned(),
            processed: typed_status.feedback_history_repair().processed(),
            remaining: typed_status.feedback_history_repair().remaining(),
        },
        error: String::new(),
    })
}

async fn fact_trust_history_payload(
    state: &DashboardState,
    fact_id: i64,
) -> Result<Option<Value>, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let Some(_detail) = application
        .dashboard_fact_detail_v1(fact_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let history = application
        .dashboard_feedback_history_v1(fact_id, 300)
        .await
        .map_err(|error| error.to_string())?;
    let trust_history: Vec<Value> = history
        .events()
        .iter()
        .map(|event| {
            let action = match event.action() {
                tracedecay_store::CompatibilityFactFeedbackActionV1::Helpful => "helpful",
                tracedecay_store::CompatibilityFactFeedbackActionV1::Unhelpful => "unhelpful",
            };
            let availability = match event.details_availability() {
                tracedecay_store::CompatibilityFactFeedbackDetailsAvailabilityV1::Available => {
                    "available"
                }
                tracedecay_store::CompatibilityFactFeedbackDetailsAvailabilityV1::LegacyRedacted => {
                    "legacy_redacted"
                }
                tracedecay_store::CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown => {
                    "unknown"
                }
            };
            let mut row = Map::new();
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
    let repair_progress = history.repair_progress();
    let repair_state = match repair_progress {
        tracedecay_store::CompatibilityFeedbackRepairProgressV1::Unknown => "unknown",
        tracedecay_store::CompatibilityFeedbackRepairProgressV1::NotRequired => "not_required",
        tracedecay_store::CompatibilityFeedbackRepairProgressV1::Complete { .. } => "complete",
        tracedecay_store::CompatibilityFeedbackRepairProgressV1::Incomplete { .. } => "incomplete",
    };
    Ok(Some(json!({
        "fact_id": fact_id,
        "trust_history": trust_history,
        "repair": {
            "state": repair_state,
            "processed": repair_progress.processed(),
            "remaining": repair_progress.remaining(),
        },
        "error": "",
    })))
}

/// `GET /api/plugins/holographic/` — overview + facts + entities + graph.
pub async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> Response {
    let limit = coerce_limit(params.limit, 25, 100);
    let graph_limit = coerce_limit(params.graph_limit, limit, 1000);

    let mut obj = Map::new();
    obj.insert("path".into(), json!(state.mem_db_path));
    obj.insert("exists".into(), json!(true));
    obj.insert("overview".into(), Value::Null);
    obj.insert("facts".into(), json!([]));
    obj.insert("entities".into(), json!([]));
    obj.insert("graph".into(), json!({ "nodes": [], "edges": [] }));
    obj.insert("error".into(), json!(""));
    obj.insert(
        "reads".into(),
        json!({
            "facts": {"state": "pending"},
            "entities": {"state": "pending"},
            "graph": {"state": "pending"},
        }),
    );
    obj.insert(
        "facts_coverage".into(),
        json!({
            "completeness": "bounded",
            "limit": limit,
            "query_applied_after_limit": !params.q.is_empty(),
        }),
    );
    match memory_service::overview_payload(&state).await {
        Ok(payload) => {
            obj.insert("overview".into(), payload);
        }
        Err(e) => {
            obj.insert("error".into(), json!(e));
        }
    }
    match memory_service::fetch_facts(&state, &params.q, limit).await {
        Ok(facts) => {
            obj.insert("facts".into(), json!(facts));
            obj["reads"]["facts"] = json!({"state": "ready"});
        }
        Err(error) => {
            obj["reads"]["facts"] = json!({"state": "error", "error": error});
        }
    }
    match memory_service::fetch_entities(&state, limit).await {
        Ok(entities) => {
            obj.insert("entities".into(), json!(entities));
            obj["reads"]["entities"] = json!({"state": "ready"});
        }
        Err(error) => {
            obj["reads"]["entities"] = json!({"state": "error", "error": error});
        }
    }
    match memory_service::graph_payload(&state, &params.q, graph_limit).await {
        Ok(graph) => {
            obj.insert("graph".into(), graph);
            obj["reads"]["graph"] = json!({"state": "ready"});
        }
        Err(error) => {
            obj["reads"]["graph"] = json!({"state": "error", "error": error});
        }
    }
    let holographic = Value::Object(obj);

    let payload = json!({
        "providers": memory_service::providers_payload(),
        "query": params.q,
        "limit": limit,
        "holographic": holographic,
    });
    match serde_json::from_value::<MemoryOverviewPayloadV1>(payload) {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to encode memory overview contract: {error}"
            ))),
        )
            .into_response(),
    }
}

/// `GET /api/plugins/holographic/status` — rich holographic-memory health
/// derived from the memory application authority plus the largest-bank
/// utilization that operators need for the dashboard health card.
pub async fn status(State(state): State<DashboardState>) -> Response {
    match memory_status_payload(&state).await {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to compute memory status: {e}"
            ))),
        )
            .into_response(),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}` — full fact detail.
///
/// List and projection payloads truncate `content` to 200 chars to keep them
/// light; detail panels (e.g. the Semantic Map's pinned card) fetch the
/// complete row — plus linked entities — from here.
pub async fn fact_detail(
    State(state): State<DashboardState>,
    JsonPath(fact_id): JsonPath<i64>,
) -> Response {
    match memory_service::fact_detail_payload(&state, fact_id).await {
        Ok(Some(payload)) => match serde_json::from_value::<MemoryFactDetailPayloadV1>(payload) {
            Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!(
                    "Failed to encode memory fact detail contract: {error}"
                ))),
            )
                .into_response(),
        },
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("fact not found: {fact_id}"))),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(http_detail(&e))).into_response(),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}/trust-history` — append-only
/// feedback audit rows explaining how a fact's trust changed over time.
pub async fn fact_trust_history(
    State(state): State<DashboardState>,
    JsonPath(fact_id): JsonPath<i64>,
) -> (StatusCode, Json<Value>) {
    match fact_trust_history_payload(&state, fact_id).await {
        Ok(Some(payload)) => (StatusCode::OK, Json(payload)),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("fact not found: {fact_id}"))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to load trust history for fact {fact_id}: {e}"
            ))),
        ),
    }
}

/// `GET /api/plugins/holographic/projection` — 2D PCA of phase vectors,
/// embedded as `[cos(p), sin(p)]` so wrapped phases compare correctly.
pub async fn projection(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<ProjectionParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 25, memory_service::projection_point_cap());
    Json(memory_service::projection_payload(&state, &params.q, limit).await)
}

/// `GET /api/plugins/holographic/similarity` — pairwise phase-cosine
/// similarity (`mean(cos(p_i − p_j))`) over all vectored facts.
///
/// `min_similarity` is the single floor parameter; the response still emits
/// the same value under both the `min_similarity` and legacy `threshold`
/// keys so the payload shape is unchanged.
pub async fn similarity(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SimilarityParams>,
) -> Json<Value> {
    let min_similarity = memory_service::coerce_similarity_score(
        params.min_similarity,
        SIMILARITY_DEFAULT_THRESHOLD,
    );
    let pair_cap = coerce_limit(params.limit, 25, SIMILARITY_PAIR_CAP) as usize;
    Json(memory_service::similarity_payload(&state, min_similarity, pair_cap).await)
}

/// `GET /api/plugins/holographic/curation/status` — similarity-dedup curator status.
pub async fn curation_status(State(state): State<DashboardState>) -> Json<Value> {
    Json(memory_service::curation_status_payload(&state).await)
}

/// `GET /api/plugins/holographic/curation/activity` — recent deterministic curator events.
pub async fn curation_activity(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 100, 300);
    Json(memory_service::curation_activity_payload(&state, limit).await)
}

/// `GET /api/plugins/holographic/curation/runs` — recent standalone
/// automation backend runs, loaded from the append-only project sidecar ledger.
pub async fn curation_runs(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 200) as usize;
    match tracedecay_agent_hosts::automation::run_ledger::load_run_records(
        &state.dashboard_root,
        limit,
    )
    .await
    {
        Ok(records) => {
            let count = records.len();
            Json(json!({
                "records": records,
                "count": count,
                "limit": limit,
                "error": "",
            }))
        }
        Err(err) => Json(json!({
            "records": [],
            "count": 0,
            "limit": limit,
            "error": err.to_string(),
        })),
    }
}

/// `GET /api/plugins/holographic/fact-proposals` — session-reflector fact
/// proposal telemetry, plus historical applied/rejected decisions.
pub async fn fact_proposals(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<FactProposalParams>,
) -> (StatusCode, Json<Value>) {
    let proposal_state = match parse_fact_proposal_state(params.state.as_deref()) {
        Ok(state) => state,
        Err(message) => return (StatusCode::BAD_REQUEST, Json(http_detail(&message))),
    };
    let limit = coerce_limit(params.limit, 50, 200) as usize;
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => return fact_proposal_error(&err),
    };
    match tracedecay_agent_hosts::automation::fact_proposals::list_fact_proposals(
        &memory,
        &state.dashboard_root,
        proposal_state,
        limit,
    )
    .await
    {
        Ok(proposals) => (
            StatusCode::OK,
            Json(json!({
                "proposals": proposals,
                "count": proposals.len(),
                "limit": limit,
                "error": "",
            })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&err.to_string())),
        ),
    }
}

/// `POST /api/plugins/holographic/fact-proposals/{proposal_id}/apply` —
/// applies a stored session-reflector fact proposal.
pub async fn fact_proposal_apply(
    State(state): State<DashboardState>,
    Path(proposal_id): Path<String>,
    body: Option<axum::extract::Json<FactProposalApplyBody>>,
) -> (StatusCode, Json<Value>) {
    let reviewer = body.and_then(|body| body.0.reviewer);
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => return fact_proposal_error(&err),
    };
    match tracedecay_agent_hosts::automation::fact_proposals::apply_fact_proposal_with_result(
        &memory,
        &state.dashboard_root,
        &proposal_id,
        reviewer,
    )
    .await
    {
        Ok(result) => {
            if result.newly_promoted {
                tracedecay_agent_hosts::automation::memory_digest::refresh_memory_digest_after_memory_change(
                    &memory,
                    &state.project_root,
                )
                .await;
            }
            (
                StatusCode::OK,
                Json(json!({
                    "proposal": result.record,
                    "error": "",
                })),
            )
        }
        Err(err) => fact_proposal_error(&err),
    }
}

/// `POST /api/plugins/holographic/fact-proposals/{proposal_id}/reject` —
/// explicit rejection for a pending session-reflector proposal.
pub async fn fact_proposal_reject(
    State(state): State<DashboardState>,
    Path(proposal_id): Path<String>,
    body: Option<axum::extract::Json<FactProposalRejectBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => return fact_proposal_error(&err),
    };
    match tracedecay_agent_hosts::automation::fact_proposals::reject_fact_proposal(
        &memory,
        &state.dashboard_root,
        &proposal_id,
        body.reviewer,
        body.reason,
    )
    .await
    {
        Ok(proposal) => (
            StatusCode::OK,
            Json(json!({
                "proposal": proposal,
                "error": "",
            })),
        ),
        Err(err) => fact_proposal_error(&err),
    }
}

fn parse_fact_proposal_state(
    state: Option<&str>,
) -> Result<Option<tracedecay_agent_hosts::automation::fact_proposals::FactProposalState>, String> {
    use tracedecay_agent_hosts::automation::fact_proposals::FactProposalState;

    let Some(state) = state else {
        return Ok(None);
    };
    let state = state.trim().to_ascii_lowercase();
    if state.is_empty() {
        return Ok(None);
    }
    FactProposalState::parse(&state)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn fact_proposal_error(
    err: &tracedecay_runtime_core::errors::TraceDecayError,
) -> (StatusCode, Json<Value>) {
    let message = err.to_string();
    let status = if message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.contains("not pending") || message.contains("no add_fact_request") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(http_detail(&message)))
}

/// `POST /api/plugins/holographic/curate/apply` — generic curation-ops apply
/// endpoint. Body: `{"ops": [...]}` where each op is one of:
///
/// - `{"op": "delete", "fact_id": <id>, "reason": <string?>}` — hard-deletes
///   the fact (entity links cascade, FTS rows drop via trigger).
/// - `{"op": "merge", "winner_id": <id>, "loser_ids": [<id>...],
///   "merged_content": <string?>}` — optionally rewrites the winner's content
///   with `merged_content`, then hard-deletes the losers.
///
/// Per-op failures are reported in `results` (status stays 200); the request
/// only fails wholesale on a malformed body. External planners (e.g. the
/// LLM-backed Hermes wrapper) build against this contract.
pub async fn curate_apply(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<CurateApplyBody>>,
) -> (StatusCode, Json<Value>) {
    let Some(axum::extract::Json(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(http_detail("Request body must be JSON: {\"ops\": [...]}")),
        );
    };

    let payload = memory_service::curate_apply_payload(&state, &body.ops).await;
    if let Ok(application) = memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        tracedecay_agent_hosts::automation::memory_digest::refresh_memory_digest_after_memory_change(
            &application,
            &state.project_root,
        )
        .await;
    }
    (StatusCode::OK, Json(payload))
}

/// `GET /api/plugins/holographic/oplog` — recent memory operations, newest
/// first. Rows come from the authoritative compatibility audit. Details are
/// privacy-gated and may be explicitly redacted rather than reconstructed
/// from legacy JSON.
pub async fn oplog(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 300);
    Json(memory_service::oplog_payload(&state, limit).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_proposal_state_filter_accepts_applying() {
        assert_eq!(
            parse_fact_proposal_state(Some("applying")).unwrap(),
            Some(tracedecay_agent_hosts::automation::fact_proposals::FactProposalState::Applying)
        );
    }
}
