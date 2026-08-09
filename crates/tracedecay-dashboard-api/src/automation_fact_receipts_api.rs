use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonQuery, coerce_limit, http_detail};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_agent_hosts::automation::automatic_facts::{
    AutomaticFactReceipt, AutomaticFactState, list_automatic_fact_receipts,
    load_automatic_fact_receipt,
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
    let receipt_state = match params.state.as_deref() {
        Some(value) => match AutomaticFactState::parse(value) {
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
                    "Failed to initialize automatic fact receipt authority: {err}"
                ))),
            );
        }
    };
    match list_automatic_fact_receipts(&memory, receipt_state, limit).await {
        Ok(receipts) => {
            let count = receipts.len();
            (
                StatusCode::OK,
                Json(json!({
                    "receipts": receipts,
                    "count": count,
                    "limit": limit,
                    "error": "",
                })),
            )
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to load automatic fact receipts: {err}"
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
                    "Failed to initialize automatic fact receipt authority: {err}"
                ))),
            );
        }
    };
    match load_automatic_fact_receipt(&memory, &id).await {
        Ok(Some(receipt)) => (StatusCode::OK, Json(receipt_payload(&receipt))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!(
                "automatic fact receipt not found: {id}"
            ))),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to load automatic fact receipt: {err}"
            ))),
        ),
    }
}

fn receipt_payload(receipt: &AutomaticFactReceipt) -> Value {
    json!({
        "receipt": receipt,
        "error": "",
    })
}
