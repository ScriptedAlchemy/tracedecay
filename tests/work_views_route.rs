//! Mount contract for the Work graph views route.
//!
//! The views route is the one public surface over the durable work-product
//! graph authority, and it is composed from three tables owned by three
//! different crates: the [`WorkOperation`] descriptor that names its path, the
//! executable catalog row that advertises its schemas, and the operation-id
//! table the daemon resolves a capability and use case from. A row missing from
//! any one of them fails differently — an unmounted path, an unadvertised
//! binding, or a request refused as invalid before it reaches the authority —
//! so all three are checked against each other here rather than each against a
//! handwritten expectation.
//!
//! What this file deliberately does not restate: the graph authority's own
//! typed answers (proved against the real store in
//! `tracedecay-rusqlite-runtime/tests/work_product_graph_authority.rs`) and
//! whole-registry route reachability on the live daemon (proved for every
//! public binding, this one included, by `work_route_exposure_conformance`).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceExt;
use tracedecay_api::{WorkHttpRequest, WorkOperation};
use tracedecay_application::{
    CancellationSignal, Deadline, RequestId, WORK_APPLICATION_OPERATION_IDS_V1,
    work_executable_binding_registry,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{OperationId, RouteExposureV1};

#[test]
fn the_views_descriptor_catalog_row_and_dispatch_identity_describe_one_operation() {
    let operation = WorkOperation::Views;
    assert_eq!(operation.operation_key(), "views");
    assert_eq!(operation.route_path(), "/work/views");
    assert_eq!(
        operation.application_route_path(),
        "/application/work/views"
    );
    assert_eq!(operation.dashboard_route_path(), "/api/work/views");
    assert!(
        operation.is_read_only(),
        "serving a recorded graph produces no durable effect"
    );

    let registry = work_executable_binding_registry().expect("canonical Work registry");
    let binding = registry
        .get(&OperationId::new(operation.operation_id_str()).expect("operation id"))
        .and_then(|availability| availability.binding())
        .expect("the views operation must be an available executable binding");
    let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
        panic!("the views binding must carry a public route");
    };
    assert_eq!(route_path, operation.application_route_path());
    assert_eq!(
        binding.request_schema().body()["title"]
            .as_str()
            .expect("a titled request schema"),
        operation.request_schema_name(),
    );
    assert_eq!(
        binding.result_schema().body()["title"]
            .as_str()
            .expect("a titled result schema"),
        operation.result_schema_name(),
    );

    // Without this row the daemon refuses the invocation as invalid before the
    // graph authority is ever consulted, so a mounted route would answer 400 for
    // every well-formed request.
    let dispatch_identity = WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .find(|(key, _, _)| *key == operation.operation_key())
        .expect("the views operation must resolve a capability and use case");
    assert_eq!(
        *dispatch_identity,
        ("views", "capability.work.views", "use-case.work.views")
    );
}

#[tokio::test]
async fn the_views_path_resolves_to_the_views_operation_and_refuses_a_malformed_body() {
    let seen: Arc<Mutex<Vec<WorkOperation>>> = Arc::new(Mutex::new(Vec::new()));
    let router = work_router(Arc::clone(&seen));

    let accepted = post(&router, "/api/work/views", "{}").await;
    assert_ne!(accepted, StatusCode::NOT_FOUND, "the views path is mounted");
    assert_eq!(
        seen.lock().expect("recorded operations").as_slice(),
        [WorkOperation::Views],
        "the views segment must resolve to the views operation, not a sibling read"
    );

    // A body the adapter cannot read is refused by the adapter. Reaching the
    // owner with an unparsed body would push the refusal past the boundary that
    // owns request well-formedness.
    let refused = post(&router, "/api/work/views", "{ not json").await;
    assert_eq!(refused, StatusCode::BAD_REQUEST);
    assert_eq!(
        seen.lock().expect("recorded operations").len(),
        1,
        "a malformed body must not reach the application owner"
    );
}

fn work_router(seen: Arc<Mutex<Vec<WorkOperation>>>) -> Router {
    Router::new().nest(
        "/api/work",
        tracedecay_api::work_core_router(move |request: WorkHttpRequest| {
            let seen = Arc::clone(&seen);
            async move {
                seen.lock()
                    .expect("recorded operations")
                    .push(request.operation);
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }),
    )
}

async fn post(router: &Router, uri: &str, body: &'static str) -> StatusCode {
    let cancellation =
        CancellationSignal::active("cancellation.work-views-route").expect("cancellation");
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("content-type", "application/json")
                .extension(RequestId::new("request.work-views-route").expect("request identity"))
                .extension(tracedecay_api::HttpApplicationControls {
                    deadline: Deadline::new(UtcMicros(9_999_999)).expect("deadline"),
                    cancellation,
                })
                .body(Body::from(body))
                .expect("Work views request"),
        )
        .await
        .expect("Work views response")
        .status()
}
