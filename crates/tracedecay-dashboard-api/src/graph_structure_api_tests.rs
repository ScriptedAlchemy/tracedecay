use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Value, json};
use tracedecay_domain::errors::TraceDecayError;

use super::{graph_error_response, graph_query_error_response};
use crate::authority_tests::DashboardStateFixture;
use crate::graph::{CodeGraphReadError, GraphQueryError};

async fn response_body(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("response body");
    (
        status,
        serde_json::from_slice(&body).expect("JSON response"),
    )
}

#[tokio::test]
async fn query_refusal_preserves_the_open_response_and_original_detail() {
    let fixture = DashboardStateFixture::open("project.dashboard-graph-query-error").await;
    let error = CodeGraphReadError::Stale {
        detail: "the served generation changed during the read".to_owned(),
    };
    let (open_status, open) =
        response_body(graph_error_response::<Value>(&fixture.state, error.clone())).await;
    let (query_status, query) = response_body(graph_query_error_response::<Value>(
        &fixture.state,
        GraphQueryError::Read(error),
    ))
    .await;

    assert_eq!(open_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(query_status, open_status);
    assert_eq!(query["payload"], open["payload"]);
    assert_eq!(query["domain_state"], open["domain_state"]);
    assert_eq!(query["coverage"], open["coverage"]);
    assert_eq!(query["scope"], open["scope"]);
    assert_eq!(
        query["payload"],
        json!({
            "status": "unmeasured",
            "reason": "graph_generation_stale",
            "detail": "the served generation changed during the read",
        })
    );
}

#[tokio::test]
async fn unrelated_query_route_is_failed_instead_of_graph_unavailability() {
    let fixture = DashboardStateFixture::open("project.dashboard-graph-runtime-error").await;
    let error = TraceDecayError::project_route(
        "unrelated-query-failure",
        false,
        "the query could not read its runtime state",
    );
    let detail = error.to_string();
    let (status, response) = response_body(graph_query_error_response::<Value>(
        &fixture.state,
        GraphQueryError::Runtime(error),
    ))
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response["domain_state"], "error");
    assert_eq!(
        response["payload"],
        json!({
            "status": "failed",
            "code": "strata_read_failed",
            "detail": detail,
            "retryable": false,
        })
    );
}

#[tokio::test]
async fn incomplete_test_evidence_is_unmeasured_instead_of_failed() {
    let fixture = DashboardStateFixture::open("project.dashboard-incomplete-evidence").await;
    let symbol = tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1 {
        occurrence: tracedecay_domain::SymbolOccurrenceId::new("symbol.incomplete")
            .expect("symbol occurrence"),
        binding: None,
        metadata: None,
    };
    let error = tracedecay_graph_query::test_risk::verified_test_symbol_parts(&symbol)
        .expect_err("test evidence requires extraction metadata");
    let (status, response) =
        response_body(graph_query_error_response::<Value>(&fixture.state, error)).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response["domain_state"], "unknown");
    assert_eq!(
        response["payload"],
        json!({
            "status": "unmeasured",
            "reason": "graph_authority_unavailable",
            "detail": "verified test evidence is missing extraction metadata",
        })
    );
}
