use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::util::{JsonQuery, coerce_limit, json_error};
use super::{DashboardState, RequestControl};
use crate::memory_api::control::{
    fact_read_control, request_terminal_state, terminal_read_response,
};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_automation_runtime::automation::automatic_facts::{
    AutomaticFactReceipt, AutomaticFactState, list_automatic_fact_receipts,
    load_automatic_fact_receipt,
};
use tracedecay_store::MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
}

/// `GET /api/automation/automatic-fact-receipts`, newest first under `limit`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomaticFactReceiptsPayloadV1 {
    receipts: Vec<AutomaticFactReceipt>,
    count: usize,
    limit: usize,
}

#[hotpath::measure(label = "dashboard_api.receipts.list", future = true)]
pub async fn list(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    JsonQuery(params): JsonQuery<ListParams>,
) -> Response {
    let receipt_state = match params.state.as_deref() {
        Some(value) => match AutomaticFactState::parse(value) {
            Ok(state) => Some(state),
            Err(err) => return json_error(StatusCode::BAD_REQUEST, err.to_string()).into_response(),
        },
        None => None,
    };
    let limit = coerce_limit(
        params.limit,
        50,
        MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS as i64,
    ) as usize;
    let memory = match open_receipt_memory(&state) {
        Ok(memory) => memory,
        Err(error) => return error.into_response(),
    };
    let result =
        list_automatic_fact_receipts(&memory, receipt_state, limit, &fact_read_control(&control))
            .await;
    if let Some(state) = request_terminal_state(&control) {
        return terminal_read_response(state).into_response();
    }
    match result {
        Ok(receipts) => Json(AutomaticFactReceiptsPayloadV1 {
            count: receipts.len(),
            receipts,
            limit,
        })
        .into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load automatic fact receipts: {err}"),
        )
        .into_response(),
    }
}

#[hotpath::measure(label = "dashboard_api.receipts.view", future = true)]
pub async fn view(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let memory = match open_receipt_memory(&state) {
        Ok(memory) => memory,
        Err(error) => return error,
    };
    let result = load_automatic_fact_receipt(&memory, &id, &fact_read_control(&control)).await;
    if let Some(state) = request_terminal_state(&control) {
        return terminal_read_response(state);
    }
    match result {
        Ok(Some(receipt)) => (StatusCode::OK, Json(receipt_payload(&receipt))),
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            format!("automatic fact receipt not found: {id}"),
        ),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load automatic fact receipt: {err}"),
        ),
    }
}

fn open_receipt_memory(
    state: &DashboardState,
) -> std::result::Result<
    tracedecay_session_memory::memory::MemoryApplication<
        tracedecay_session_memory::fact_store::DatabaseFactStore<'_>,
    >,
    (StatusCode, Json<Value>),
> {
    memory_application_for_db(state.memory_owner.clone(), state.mem_db.as_ref()).map_err(|err| {
        json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to initialize automatic fact receipt authority: {err}"),
        )
    })
}

fn receipt_payload(receipt: &AutomaticFactReceipt) -> Value {
    json!({
        "receipt": receipt,
        "error": "",
    })
}
