use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonQuery, coerce_limit, http_detail};
use crate::automation::fact_proposals::{
    FactProposalRecord, FactProposalState, apply_fact_proposal_with_result, list_fact_proposals,
    load_fact_proposal, reject_fact_proposal,
};
use crate::tracedecay::facts::memory_application_for_db;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RejectBody {
    reason: Option<String>,
}

pub async fn list(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<ListParams>,
) -> (StatusCode, Json<Value>) {
    let proposal_state = match params.state.as_deref() {
        Some(value) => match FactProposalState::parse(value) {
            Ok(state) => Some(state),
            Err(err) => return (StatusCode::BAD_REQUEST, Json(http_detail(&err.to_string()))),
        },
        None => None,
    };
    let limit = coerce_limit(params.limit, 50, 200) as usize;
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!(
                    "Failed to initialize fact proposal authority: {err}"
                ))),
            );
        }
    };
    match list_fact_proposals(&memory, &state.dashboard_root, proposal_state, limit).await {
        Ok(proposals) => {
            let count = proposals.len();
            (
                StatusCode::OK,
                Json(json!({
                    "proposals": proposals,
                    "count": count,
                    "limit": limit,
                    "error": "",
                })),
            )
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to load fact proposals: {err}"
            ))),
        ),
    }
}

pub async fn view(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!(
                    "Failed to initialize fact proposal authority: {err}"
                ))),
            );
        }
    };
    match load_fact_proposal(&memory, &state.dashboard_root, &id).await {
        Ok(Some(proposal)) => (StatusCode::OK, Json(proposal_payload(&proposal))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("fact proposal not found: {id}"))),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!("Failed to load fact proposal: {err}"))),
        ),
    }
}

pub async fn apply(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!(
                    "Failed to initialize fact proposal authority: {err}"
                ))),
            );
        }
    };
    match apply_fact_proposal_with_result(
        &memory,
        &state.dashboard_root,
        &id,
        Some("dashboard".to_string()),
    )
    .await
    {
        Ok(result) => {
            if result.newly_promoted {
                crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                    &memory,
                    &state.project_root,
                )
                .await;
            }
            (StatusCode::OK, Json(proposal_payload(&result.record)))
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(http_detail(&format!(
                "Failed to apply fact proposal: {err}"
            ))),
        ),
    }
}

pub async fn reject(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
    body: Option<axum::extract::Json<RejectBody>>,
) -> (StatusCode, Json<Value>) {
    let reason = body.and_then(|body| body.0.reason);
    let memory = match memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref())
    {
        Ok(memory) => memory,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!(
                    "Failed to initialize fact proposal authority: {err}"
                ))),
            );
        }
    };
    match reject_fact_proposal(
        &memory,
        &state.dashboard_root,
        &id,
        Some("dashboard".to_string()),
        reason,
    )
    .await
    {
        Ok(proposal) => (StatusCode::OK, Json(proposal_payload(&proposal))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(http_detail(&format!(
                "Failed to reject fact proposal: {err}"
            ))),
        ),
    }
}

fn proposal_payload(proposal: &FactProposalRecord) -> Value {
    json!({
        "proposal": proposal,
        "error": "",
    })
}
