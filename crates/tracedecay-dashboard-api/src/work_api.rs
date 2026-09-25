//! The dashboard's public Work contract.
//!
//! The routes themselves are built by [`tracedecay_api::work_dashboard_router`] from
//! the canonical [`WorkOperation`] descriptor, this module only restates that
//! descriptor as the route document the dashboard contract schema publishes.
//! There is no second route table and no forwarding hop: a dashboard Work
//! request enters the same handler, owner, and dispatch as an application Work
//! request, one segment of path apart.

use std::borrow::Cow;

use tracedecay_api::WorkOperation;

#[derive(Clone, Copy)]
pub(super) struct RegisteredWorkRouteContractV1 {
    pub method: &'static str,
    pub path: &'static str,
    pub request_schema_name: fn() -> Cow<'static, str>,
    pub response_schema_name: fn() -> Cow<'static, str>,
}

/// Names the dashboard-exposed operations; every column of the document is read
/// off the descriptor. This is the dashboard view of `WorkOperation::ALL` with
/// scheduler-owned `StartAttempt` intentionally withheld from the dashboard.
macro_rules! dashboard_work_routes {
    ($($variant:ident),+ $(,)?) => {
        static REGISTERED_ROUTE_CONTRACTS: &[RegisteredWorkRouteContractV1] = &[
            $(
                RegisteredWorkRouteContractV1 {
                    method: "POST",
                    path: WorkOperation::$variant.dashboard_route_path(),
                    request_schema_name: || WorkOperation::$variant.request_schema_name(),
                    response_schema_name: || WorkOperation::$variant.result_schema_name(),
                },
            )+
        ];
    };
}

dashboard_work_routes!(
    GenerateProposal,
    Create,
    ReviewProposal,
    AcceptProposal,
    AdmitExecution,
    Synthesize,
    AttemptStatus,
    CancelAttempt,
    ResumeAttempts,
    RetryAttempt,
    ListAttempts,
    ExecutionHistory,
    HydrateArtifacts,
    RetrieveEvidence,
    Views,
    Experience,
    CompareProposal,
    PrepareGraphMutation,
    MutateGraph,
    Topology,
    TopologyMetrics,
    PrepareDuplicateAdjudication,
    AdjudicateDuplicate,
    AdjudicateLeak,
    PauseRun,
    ResumeRun,
    RunControl,
    PlacementPreflight,
    AdmitPlacement,
    PlacementStatus,
    ReleasePlacement,
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
    use tracedecay_api::WorkHttpRequest;
    use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    fn dashboard_router() -> Router {
        Router::new().nest(
            "/api/work",
            tracedecay_api::work_dashboard_router(|_request: WorkHttpRequest| async {
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
    async fn every_work_route_is_mounted_and_unknown_segments_are_refused() {
        let router = dashboard_router();

        for route in super::registered_route_contracts() {
            let status = post(&router, route.path).await;
            assert_ne!(status, StatusCode::NOT_FOUND, "{}", route.path);
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{}", route.path);
        }

        assert_eq!(
            post(&router, "/api/work/not-an-operation").await,
            StatusCode::NOT_FOUND
        );
        for retired in [
            "/api/work/snapshot",
            "/api/work/delta",
            "/api/work/replan-dependencies",
            "/api/work/accept-task",
        ] {
            assert_eq!(
                post(&router, retired).await,
                StatusCode::NOT_FOUND,
                "{retired}"
            );
        }
    }
}
