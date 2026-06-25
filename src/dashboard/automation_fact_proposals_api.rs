use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::util::{coerce_limit, http_detail, JsonQuery};
use super::DashboardState;
use crate::automation::fact_proposals::{
    apply_fact_proposal, list_fact_proposals, load_fact_proposal, reject_fact_proposal,
    FactProposalRecord, FactProposalState,
};

#[derive(Debug, Deserialize)]
pub(crate) struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RejectBody {
    reason: Option<String>,
}

pub(crate) async fn list(
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
    match list_fact_proposals(&state.dashboard_root, proposal_state, limit).await {
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

pub(crate) async fn view(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match load_fact_proposal(&state.dashboard_root, &id).await {
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

pub(crate) async fn apply(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match apply_fact_proposal(
        &state.dashboard_root,
        &state.mem_conn,
        &id,
        Some("dashboard".to_string()),
    )
    .await
    {
        Ok(proposal) => (StatusCode::OK, Json(proposal_payload(&proposal))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(http_detail(&format!(
                "Failed to apply fact proposal: {err}"
            ))),
        ),
    }
}

pub(crate) async fn reject(
    State(state): State<DashboardState>,
    AxumPath(id): AxumPath<String>,
    body: Option<axum::extract::Json<RejectBody>>,
) -> (StatusCode, Json<Value>) {
    let reason = body.and_then(|body| body.0.reason);
    match reject_fact_proposal(
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
