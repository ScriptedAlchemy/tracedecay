use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceExt;
use tracedecay_api::{HandoffOperation, HttpApplicationControls, handoff_application_router};
use tracedecay_application::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::UtcMicros;

#[test]
fn descriptor_matches_the_typed_handoff_registry_routes() {
    assert_eq!(
        HandoffOperation::ALL
            .into_iter()
            .map(|operation| (
                operation.operation_id_str(),
                operation.application_route_path()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "operation.handoff.issue_task_handoff",
                "/application/handoff/issue-task",
            ),
            (
                "operation.handoff.open_investigation_handoff",
                "/application/handoff/open-investigation",
            ),
            (
                "operation.handoff.open_task_handoff",
                "/application/handoff/open-task",
            ),
        ]
    );
}

#[tokio::test]
async fn router_dispatches_both_operations_to_one_application_owner() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let owner_observed = Arc::clone(&observed);
    let app = handoff_application_router(move |request: tracedecay_api::HandoffHttpRequest| {
        let observed = Arc::clone(&owner_observed);
        async move {
            observed.lock().unwrap().push(request.operation);
            StatusCode::NO_CONTENT.into_response()
        }
    })
    .layer(Extension(
        RequestId::new("request.handoff.route-test").unwrap(),
    ))
    .layer(Extension(HttpApplicationControls {
        deadline: Deadline::new(UtcMicros(10_000)).unwrap(),
        cancellation: CancellationSignal::active("cancel.handoff.route-test").unwrap(),
    }));

    for operation in HandoffOperation::ALL {
        let response = app
            .clone()
            .oneshot(
                Request::post(operation.route_path())
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(*observed.lock().unwrap(), HandoffOperation::ALL);
}
