use axum::extract::{Extension, Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use super::util::{JsonQuery, coerce_limit, http_detail};
use super::{DashboardHttpRequestControlV1, DashboardState};
use crate::memory_api::control::{fact_read_control, request_terminal_state, terminal_read_code};
use crate::read_model::DashboardDomainStateV1;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_agent_hosts::automation::automatic_facts::{
    AutomaticFactReceipt, AutomaticFactState, list_automatic_fact_receipts,
    load_automatic_fact_receipt,
};
use tracedecay_store::MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
}

pub async fn list(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<ListParams>,
) -> (StatusCode, Json<Value>) {
    let Some(Extension(control)) = control else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(http_detail(
                "dashboard HTTP request admission is unavailable",
            )),
        );
    };
    let receipt_state = match params.state.as_deref() {
        Some(value) => match AutomaticFactState::parse(value) {
            Ok(state) => Some(state),
            Err(err) => return (StatusCode::BAD_REQUEST, Json(http_detail(&err.to_string()))),
        },
        None => None,
    };
    let limit = coerce_limit(
        params.limit,
        50,
        MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS as i64,
    ) as usize;
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
    let result =
        list_automatic_fact_receipts(&memory, receipt_state, limit, &fact_read_control(&control))
            .await;
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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(Extension(control)) = control else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(http_detail(
                "dashboard HTTP request admission is unavailable",
            )),
        );
    };
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
    let result = load_automatic_fact_receipt(&memory, &id, &fact_read_control(&control)).await;
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
