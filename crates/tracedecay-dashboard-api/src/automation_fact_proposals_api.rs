use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonQuery, coerce_limit, http_detail};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_agent_hosts::automation::fact_proposals::{
    FactProposalRecord, FactProposalState, list_fact_proposals, load_fact_proposal,
};

#[derive(Debug, Deserialize)]
pub struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
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
    match list_fact_proposals(&memory, proposal_state, limit).await {
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
    match load_fact_proposal(&memory, &id).await {
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

fn proposal_payload(proposal: &FactProposalRecord) -> Value {
    json!({
        "proposal": proposal,
        "error": "",
    })
}
