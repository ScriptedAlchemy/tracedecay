//! Holographic-memory dashboard API, backed by tracedecay's memory store.
//!
//! Port of `plugins/memory/holographic_plus/dashboard/plugin_api.py` (Hermes)
//! onto the project database tables `memory_facts`, `memory_entities`,
//! `memory_fact_entities`, and `memory_banks`. Payload shapes mirror the
//! original routes so the ported UI bundle works unchanged.
//!
//! Differences from the Hermes backend, by design:
//! - Curation is implemented as similarity-based deduplication (no LLM).
//!   `POST /curate` proposes hard-DELETING the lower-trust fact in each
//!   `likely_duplicate` pair; `dry_run=false` applies those deletions.
//! - `POST /curate/apply` is a generic curation-ops endpoint (`delete` /
//!   `merge`) that external planners (e.g. an LLM-backed Hermes wrapper)
//!   can call with their own proposed operations.
//! - There is no fact archive: deletion is permanent (the original
//!   `holographic_plus` soft-archived facts; tracedecay does not).
//! - Banks are named after their category directly (no `cat:` prefix).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::automation_run_service;
use super::memory_analysis::{SIMILARITY_DEFAULT_THRESHOLD, SIMILARITY_PAIR_CAP};
use super::memory_service;
use super::util::{coerce_limit, http_detail, query_i64, JsonPath, JsonQuery};
use super::DashboardState;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::store::MemoryStore;
use crate::memory::trust::DEFAULT_MIN_TRUST;
use crate::memory::types::{MemoryRepairStats, MemoryStatus};

#[derive(Deserialize)]
pub(crate) struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    graph_limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectionParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct SimilarityParams {
    min_similarity: Option<f64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct LimitParams {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct FactProposalParams {
    state: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct FactProposalApplyBody {
    reviewer: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct FactProposalRejectBody {
    reviewer: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CurateBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
}

pub(crate) fn default_dry_run() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct CurateApplyBody {
    ops: Vec<Value>,
}

#[derive(Deserialize)]
pub(crate) struct AgentPlanBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    #[serde(default = "default_agent_plan_max_clusters")]
    max_clusters: usize,
    #[serde(default = "default_agent_plan_min_confidence")]
    min_confidence: f64,
}

pub(crate) fn default_agent_plan_max_clusters() -> usize {
    crate::dashboard::memory_curate::CURATION_DEFAULT_MAX_CLUSTERS
}

pub(crate) fn default_agent_plan_min_confidence() -> f64 {
    crate::dashboard::memory_curate::CURATION_DEFAULT_MIN_CONFIDENCE
}

async fn largest_bank_fact_count(state: &DashboardState) -> Result<i64, String> {
    let mut rows = state
        .mem_conn
        .query("SELECT COALESCE(MAX(fact_count), 0) FROM memory_banks", ())
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = rows.next().await.map_err(|e| e.to_string())? else {
        return Ok(0);
    };
    Ok(row.get::<i64>(0).unwrap_or(0).max(0))
}

pub(crate) async fn repair_derived_memory(
    state: &DashboardState,
) -> Result<MemoryRepairStats, String> {
    let store = MemoryStore::new(&state.mem_conn);
    let mut missing_vectors_repaired = 0;
    loop {
        let repaired = store
            .compute_missing_vectors(500)
            .await
            .map_err(|e| e.to_string())?;
        if repaired == 0 {
            break;
        }
        missing_vectors_repaired += repaired;
    }

    let banks_rebuilt = store
        .rebuild_dirty_banks()
        .await
        .map_err(|e| e.to_string())?;

    Ok(MemoryRepairStats {
        missing_vectors_repaired,
        banks_rebuilt,
    })
}

async fn memory_status_payload(state: &DashboardState) -> Result<Value, String> {
    let hrr_dim = HolographicEncoder::DIMENSIONS;
    let repair = repair_derived_memory(state).await?;
    let status = MemoryStatus {
        fact_count: query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await
            as usize,
        entity_count: query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_entities", ()).await
            as usize,
        bank_count: query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_banks", ()).await
            as usize,
        algebra_name: "amari_fhrr".to_string(),
        hrr_dim,
        estimated_capacity: (hrr_dim as f64 / (hrr_dim as f64).ln()).round() as usize,
        trust_0_025_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts WHERE trust_score < 0.25",
            (),
        )
        .await as usize,
        trust_025_050_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts WHERE trust_score >= 0.25 AND trust_score < 0.50",
            (),
        )
        .await as usize,
        trust_050_075_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts WHERE trust_score >= 0.50 AND trust_score < 0.75",
            (),
        )
        .await as usize,
        trust_075_100_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts WHERE trust_score >= 0.75",
            (),
        )
        .await as usize,
        below_default_recall_threshold_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts WHERE trust_score < ?1",
            libsql::params![DEFAULT_MIN_TRUST],
        )
        .await as usize,
        helpful_count: query_i64(
            &state.mem_conn,
            "SELECT COALESCE(SUM(helpful_count), 0) FROM memory_facts",
            (),
        )
        .await as usize,
        unhelpful_count: query_i64(
            &state.mem_conn,
            "SELECT COALESCE(SUM(unhelpful_count), 0) FROM memory_facts",
            (),
        )
        .await as usize,
        missing_vector_count: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts
             WHERE hrr_vector IS NULL OR hrr_algebra != 'amari_fhrr' OR hrr_dim != ?1",
            libsql::params![hrr_dim as i64],
        )
        .await as usize,
        legacy_backfill_complete: query_i64(
            &state.mem_conn,
            "SELECT COUNT(*) FROM memory_facts
             WHERE json_extract(metadata, '$.holographic_memory_backfill_v1') = 1",
            (),
        )
        .await
            > 0,
        repair,
    };
    let largest_bank_fact_count = largest_bank_fact_count(state).await?;
    let largest_bank_utilization_pct = if status.estimated_capacity > 0 {
        largest_bank_fact_count as f64 / status.estimated_capacity as f64 * 100.0
    } else {
        0.0
    };
    Ok(json!({
        "path": state.mem_db_path,
        "exists": true,
        "memory": status,
        "largest_bank_fact_count": largest_bank_fact_count,
        "largest_bank_utilization_pct": largest_bank_utilization_pct,
        "error": "",
    }))
}

async fn fact_trust_history_payload(
    state: &DashboardState,
    fact_id: i64,
) -> Result<Option<Value>, String> {
    let store = MemoryStore::new(&state.mem_conn);
    let Some(_fact) = store.get_fact(fact_id).await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let trust_history = store
        .fact_trust_history(fact_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(json!({
        "fact_id": fact_id,
        "trust_history": trust_history,
        "error": "",
    })))
}

/// `GET /api/plugins/holographic/` — overview + facts + entities + graph.
pub(crate) async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> Json<Value> {
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
    match memory_service::overview_payload(&state).await {
        Ok(payload) => {
            obj.insert("overview".into(), payload);
        }
        Err(e) => {
            obj.insert("error".into(), json!(e));
        }
    }
    if let Ok(facts) = memory_service::fetch_facts(&state, &params.q, limit).await {
        obj.insert("facts".into(), json!(facts));
    }
    if let Ok(entities) = memory_service::fetch_entities(&state, limit).await {
        obj.insert("entities".into(), json!(entities));
    }
    if let Ok(graph) = memory_service::graph_payload(&state, &params.q, graph_limit).await {
        obj.insert("graph".into(), graph);
    }
    let holographic = Value::Object(obj);

    Json(json!({
        "providers": memory_service::providers_payload(),
        "query": params.q,
        "limit": limit,
        "holographic": holographic,
    }))
}

/// `GET /api/plugins/holographic/status` — rich holographic-memory health
/// derived from `TraceDecay::memory_status()` plus the largest-bank utilization
/// that operators need for the dashboard health card.
pub(crate) async fn status(State(state): State<DashboardState>) -> (StatusCode, Json<Value>) {
    match memory_status_payload(&state).await {
        Ok(payload) => (StatusCode::OK, Json(payload)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to compute memory status: {e}"
            ))),
        ),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}` — full fact detail.
///
/// List and projection payloads truncate `content` to 200 chars to keep them
/// light; detail panels (e.g. the Semantic Map's pinned card) fetch the
/// complete row — plus linked entities — from here.
pub(crate) async fn fact_detail(
    State(state): State<DashboardState>,
    JsonPath(fact_id): JsonPath<i64>,
) -> (StatusCode, Json<Value>) {
    match memory_service::fact_detail_payload(&state, fact_id).await {
        Ok(Some(payload)) => (StatusCode::OK, Json(payload)),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("fact not found: {fact_id}"))),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(http_detail(&e))),
    }
}

/// `GET /api/plugins/holographic/fact/{fact_id}/trust-history` — append-only
/// feedback audit rows explaining how a fact's trust changed over time.
pub(crate) async fn fact_trust_history(
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
pub(crate) async fn projection(
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
pub(crate) async fn similarity(
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
pub(crate) async fn curation_status(State(state): State<DashboardState>) -> Json<Value> {
    Json(memory_service::curation_status_payload(&state).await)
}

/// `GET /api/plugins/holographic/curation/activity` — recent deterministic curator events.
pub(crate) async fn curation_activity(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 100, 300);
    Json(memory_service::curation_activity_payload(&state, limit).await)
}

/// `GET /api/plugins/holographic/curation/runs` — recent standalone
/// automation backend runs, loaded from the append-only project sidecar ledger.
pub(crate) async fn curation_runs(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 200) as usize;
    match crate::automation::run_ledger::load_run_records(&state.dashboard_root, limit).await {
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
/// proposals awaiting approval, plus historical applied/rejected decisions.
pub(crate) async fn fact_proposals(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<FactProposalParams>,
) -> (StatusCode, Json<Value>) {
    let proposal_state = match parse_fact_proposal_state(params.state.as_deref()) {
        Ok(state) => state,
        Err(message) => return (StatusCode::BAD_REQUEST, Json(http_detail(&message))),
    };
    let limit = coerce_limit(params.limit, 50, 200) as usize;
    match crate::automation::fact_proposals::list_fact_proposals(
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
/// approval-gated session-reflector fact write.
pub(crate) async fn fact_proposal_apply(
    State(state): State<DashboardState>,
    Path(proposal_id): Path<String>,
    body: Option<axum::extract::Json<FactProposalApplyBody>>,
) -> (StatusCode, Json<Value>) {
    let reviewer = body.and_then(|body| body.0.reviewer);
    match crate::automation::fact_proposals::apply_fact_proposal(
        &state.dashboard_root,
        &state.mem_conn,
        &proposal_id,
        reviewer,
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

/// `POST /api/plugins/holographic/fact-proposals/{proposal_id}/reject` —
/// explicit rejection for a pending session-reflector proposal.
pub(crate) async fn fact_proposal_reject(
    State(state): State<DashboardState>,
    Path(proposal_id): Path<String>,
    body: Option<axum::extract::Json<FactProposalRejectBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    match crate::automation::fact_proposals::reject_fact_proposal(
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
) -> Result<Option<crate::automation::fact_proposals::FactProposalState>, String> {
    use crate::automation::fact_proposals::FactProposalState;

    let Some(state) = state else {
        return Ok(None);
    };
    match state.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "pending" | "pending_approval" => Ok(Some(FactProposalState::PendingApproval)),
        "applied" => Ok(Some(FactProposalState::Applied)),
        "rejected" => Ok(Some(FactProposalState::Rejected)),
        _ => Err(format!(
            "unknown fact proposal state '{state}' (expected pending_approval, applied, rejected)"
        )),
    }
}

fn fact_proposal_error(err: &crate::errors::TraceDecayError) -> (StatusCode, Json<Value>) {
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

/// `GET /api/plugins/holographic/curation/preview` — returns the last saved
/// dry-run preview, or null if none has been run this server session.
pub(crate) async fn curation_preview(State(state): State<DashboardState>) -> Json<Value> {
    Json(memory_service::curation_preview_payload(&state).await)
}

/// `POST /api/plugins/holographic/curation/agent-plan` — standalone backend
/// curation planner. Delegated-host mode skips TraceDecay-owned backend calls.
pub(crate) async fn curation_agent_plan(
    State(state): State<DashboardState>,
    axum::Json(body): axum::Json<AgentPlanBody>,
) -> (StatusCode, Json<Value>) {
    if !body.dry_run {
        return (
            StatusCode::BAD_REQUEST,
            Json(http_detail(
                "agent-plan currently supports dry_run=true only; apply validated ops separately",
            )),
        );
    }
    match Box::pin(automation_run_service::curation_agent_plan_payload(
        &state,
        body.max_clusters,
        body.min_confidence,
    ))
    .await
    {
        Ok(payload) => (StatusCode::OK, Json(payload)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!("Agent curation plan failed: {e}"))),
        ),
    }
}

/// `POST /api/plugins/holographic/curate` — similarity-based deduplication
/// curation. `dry_run=true` (default) returns the proposed plan without
/// mutating; `dry_run=false` applies the plan by hard-DELETING duplicate
/// losers (no archive — deletion is permanent).
pub(crate) async fn curate(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<CurateBody>>,
) -> (StatusCode, Json<Value>) {
    let dry_run = body.is_none_or(|b| b.dry_run);
    match memory_service::curate_payload(&state, dry_run).await {
        Ok(payload) => (StatusCode::OK, Json(payload)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!("Curation analysis failed: {e}"))),
        ),
    }
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
pub(crate) async fn curate_apply(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<CurateApplyBody>>,
) -> (StatusCode, Json<Value>) {
    let Some(axum::extract::Json(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(http_detail("Request body must be JSON: {\"ops\": [...]}")),
        );
    };

    (
        StatusCode::OK,
        Json(memory_service::curate_apply_payload(&state, &body.ops).await),
    )
}

/// `GET /api/plugins/holographic/oplog` — recent memory operations, newest
/// first. Rows come from `memory_oplog`, the append-only audit written by the
/// store mutation paths (add/update/remove/feedback) and curation applies.
/// `detail_json` never carries fact content beyond what the op needs
/// (deletes record a content hash, not the content).
pub(crate) async fn oplog(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<LimitParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 300);
    Json(memory_service::oplog_payload(&state, limit).await)
}
