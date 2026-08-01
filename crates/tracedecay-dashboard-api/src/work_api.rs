//! The dashboard's public Work contract.
//!
//! The routes themselves are built by [`tracedecay_api::work_core_router`] from
//! the canonical [`WorkOperation`] descriptor — this module only restates that
//! descriptor as the route document the dashboard contract schema publishes.
//! There is no second route table and no forwarding hop: a dashboard Work
//! request enters the same handler, owner, and dispatch as an application Work
//! request, one segment of path apart.

use std::borrow::Cow;

use tracedecay_api::WorkOperation;

/// `operation_id` and `application_path` are the identity the contract test
/// checks this document against; schema generation itself needs only the
/// method, path, and schema names.
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct RegisteredWorkRouteContractV1 {
    pub method: &'static str,
    pub operation_id: &'static str,
    pub path: &'static str,
    pub application_path: &'static str,
    pub request_schema_name: fn() -> Cow<'static, str>,
    pub response_schema_name: fn() -> Cow<'static, str>,
}

/// Names the dashboard-exposed operations; every column of the document is read
/// off the descriptor. A test holds this list to `WorkOperation::CORE`.
macro_rules! dashboard_work_routes {
    ($($variant:ident),+ $(,)?) => {
        static REGISTERED_ROUTE_CONTRACTS: &[RegisteredWorkRouteContractV1] = &[
            $(
                RegisteredWorkRouteContractV1 {
                    method: "POST",
                    operation_id: WorkOperation::$variant.operation_id_str(),
                    path: match WorkOperation::$variant.dashboard_route_path() {
                        Some(path) => path,
                        None => panic!("a dashboard Work route must have a public path"),
                    },
                    application_path: WorkOperation::$variant.application_route_path(),
                    request_schema_name: || WorkOperation::$variant.request_schema_name(),
                    response_schema_name: || WorkOperation::$variant.result_schema_name(),
                },
            )+
        ];

        #[cfg(test)]
        static DOCUMENTED_OPERATIONS: &[WorkOperation] = &[$(WorkOperation::$variant),+];
    };
}

dashboard_work_routes!(
    Snapshot,
    Delta,
    Create,
    ReplanDependencies,
    ReviewProposal,
    AcceptProposal,
    AdmitExecution,
    AttachRuntimeEvidence,
    AcceptTask,
);

pub(super) fn registered_route_contracts() -> &'static [RegisteredWorkRouteContractV1] {
    REGISTERED_ROUTE_CONTRACTS
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use tracedecay_api::{WorkHttpRequest, WorkOperation};
    use tracedecay_application::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    fn dashboard_router() -> Router {
        Router::new().nest(
            "/api/work",
            tracedecay_api::work_core_router(|_request: WorkHttpRequest| async {
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }),
        )
    }

    async fn post(router: &Router, uri: &str) -> StatusCode {
        let cancellation =
            CancellationSignal::active("cancellation.dashboard-work-test").expect("cancellation");
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .extension(
                        RequestId::new("request.dashboard-work-test").expect("request identity"),
                    )
                    .extension(tracedecay_api::HttpApplicationControls {
                        deadline: Deadline::new(UtcMicros(9_999_999)).expect("deadline"),
                        cancellation,
                    })
                    .body(Body::from("{}"))
                    .expect("dashboard Work request"),
            )
            .await
            .expect("dashboard Work response")
            .status()
    }

    #[tokio::test]
    async fn every_core_route_is_mounted_and_no_attempt_route_is_reachable() {
        let router = dashboard_router();

        for route in super::registered_route_contracts() {
            let status = post(&router, route.path).await;
            assert_ne!(status, StatusCode::NOT_FOUND, "{}", route.path);
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{}", route.path);
        }

        for operation in WorkOperation::ATTEMPT {
            let uri = format!("/api{}", operation.route_path());
            assert_eq!(
                post(&router, &uri).await,
                StatusCode::NOT_FOUND,
                "the dashboard must not reach {uri}"
            );
        }
        assert_eq!(
            post(&router, "/api/work/not-an-operation").await,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn the_route_document_is_exactly_the_descriptor_core_family() {
        assert_eq!(super::DOCUMENTED_OPERATIONS, WorkOperation::CORE.as_slice());
    }

    #[test]
    fn the_route_document_covers_every_canonical_core_work_binding() {
        use std::collections::BTreeSet;

        let registry = tracedecay_application::work_executable_binding_registry()
            .expect("canonical Work registry");
        let routes = super::registered_route_contracts();
        let actual_ids = routes
            .iter()
            .map(|route| route.operation_id)
            .collect::<BTreeSet<_>>();
        let expected_ids = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .map(|(operation, _, _)| format!("operation.work.{operation}"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            routes.len(),
            expected_ids.len(),
            "dashboard must expose each core Work operation exactly once"
        );
        assert_eq!(
            actual_ids,
            expected_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            "dashboard routes must cover every core Work operation exactly once"
        );
        assert_eq!(
            routes
                .iter()
                .map(|route| route.path)
                .collect::<BTreeSet<_>>()
                .len(),
            routes.len(),
            "dashboard Work paths must be unique"
        );

        for route in routes {
            assert!(!route.path.is_empty(), "{}", route.operation_id);
            let binding = registry
                .get(
                    &tracedecay_tool_catalog::OperationId::new(route.operation_id)
                        .expect("operation id"),
                )
                .and_then(|availability| availability.binding())
                .expect("available canonical Work binding");
            let tracedecay_tool_catalog::RouteExposureV1::Public { route_path, .. } =
                binding.exposure()
            else {
                panic!("canonical Work binding must be public");
            };
            assert_eq!(route.application_path, route_path);
            assert_eq!(
                (route.request_schema_name)(),
                binding.request_schema().body()["title"]
                    .as_str()
                    .expect("a titled request schema")
            );
            assert_eq!(
                (route.response_schema_name)(),
                binding.result_schema().body()["title"]
                    .as_str()
                    .expect("a titled result schema")
            );
            assert!(!route.path.contains("/attempt/"));
            assert!(!route.application_path.contains("/attempt/"));
        }
        for (operation, _, _) in tracedecay_application::WORK_ATTEMPT_OPERATION_IDS_V1 {
            assert!(
                !actual_ids.contains(format!("operation.work.{operation}").as_str()),
                "dashboard must not expose Work attempt operation {operation}"
            );
        }
    }
}
