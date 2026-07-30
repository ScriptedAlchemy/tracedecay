use std::borrow::Cow;

use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use schemars::JsonSchema;
use tower::ServiceExt;
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, ReplanDependenciesCommand, ReviewProposalRequestV1,
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
};
use tracedecay_domain::{WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1};

#[derive(Clone, Copy)]
pub(super) struct RegisteredWorkRouteContractV1 {
    pub method: &'static str,
    pub operation_id: &'static str,
    pub path: &'static str,
    pub application_path: &'static str,
    pub request_schema_name: fn() -> Cow<'static, str>,
    pub response_schema_name: fn() -> Cow<'static, str>,
}

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

macro_rules! core_work_routes {
    (
        $(
            (
                $operation_id:literal,
                $relative_path:literal,
                $application_path:literal,
                $request:ty,
                $response:ty
            )
        ),+ $(,)?
    ) => {
        static REGISTERED_ROUTE_CONTRACTS: &[RegisteredWorkRouteContractV1] = &[
            $(
                RegisteredWorkRouteContractV1 {
                    method: "POST",
                    operation_id: $operation_id,
                    path: concat!("/api/work", $relative_path),
                    application_path: $application_path,
                    request_schema_name: schema_name::<$request>,
                    response_schema_name: schema_name::<$response>,
                },
            )+
        ];

        pub(super) fn router(application_router: Router) -> Router {
            let router = Router::new();
            $(
                let router = router.route($relative_path, post(forward_to_application_router));
            )+
            router.with_state(WorkApplicationRouter { application_router })
        }
    };
}

core_work_routes!(
    (
        "operation.work.snapshot",
        "/snapshot",
        "/application/work/snapshot",
        WorkProjectionSnapshotRequestV1,
        WorkProjectionSnapshotV1
    ),
    (
        "operation.work.delta",
        "/delta",
        "/application/work/delta",
        WorkProjectionDeltaRequestV1,
        WorkProjectionDeltaV1
    ),
    (
        "operation.work.create",
        "/create",
        "/application/work/create",
        CreateWorkCommand,
        WorkProjection
    ),
    (
        "operation.work.replan_dependencies",
        "/replan-dependencies",
        "/application/work/replan-dependencies",
        ReplanDependenciesCommand,
        WorkProjection
    ),
    (
        "operation.work.review_proposal",
        "/review-proposal",
        "/application/work/review-proposal",
        ReviewProposalRequestV1,
        WorkProjection
    ),
    (
        "operation.work.accept_proposal",
        "/accept-proposal",
        "/application/work/accept-proposal",
        AcceptProposalCommand,
        WorkProjection
    ),
    (
        "operation.work.admit_execution",
        "/admit-execution",
        "/application/work/admit-execution",
        AdmitExecutionCommand,
        WorkProjection
    ),
    (
        "operation.work.attach_runtime_evidence",
        "/attach-runtime-evidence",
        "/application/work/attach-runtime-evidence",
        AttachRuntimeEvidenceCommand,
        WorkProjection
    ),
    (
        "operation.work.accept_task",
        "/accept-task",
        "/application/work/accept-task",
        AcceptTaskCommand,
        WorkProjection
    ),
);

pub(super) fn registered_route_contracts() -> &'static [RegisteredWorkRouteContractV1] {
    REGISTERED_ROUTE_CONTRACTS
}

#[derive(Clone)]
struct WorkApplicationRouter {
    application_router: Router,
}

async fn forward_to_application_router(
    State(state): State<WorkApplicationRouter>,
    OriginalUri(original_uri): OriginalUri,
    mut request: Request<Body>,
) -> Response {
    let Some(route) = registered_route_contracts()
        .iter()
        .find(|route| route.path == original_uri.path())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let application_path = route
        .application_path
        .strip_prefix("/application")
        .expect("canonical Work application routes use the application prefix");
    let path_and_query = match original_uri.query() {
        Some(query) => format!("{application_path}?{query}"),
        None => application_path.to_owned(),
    };
    let Ok(uri) = path_and_query.parse::<Uri>() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    *request.uri_mut() = uri;
    match state.application_router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use axum::response::Response;
    use axum::routing::post;
    use tower::ServiceExt;
    use tracedecay_application::{
        ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
        ApplicationResponse, InvocationError,
    };
    use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros};

    use crate::daemon_client::{
        DaemonInvocationError, DaemonInvocationExecutor, DaemonInvocationExecutorFuture,
        InvocationCancellationPolicy,
    };

    struct UnavailableExecutor;

    impl ApplicationInvocationExecutor for UnavailableExecutor {
        fn invoke(
            &self,
            _invocation: ApplicationInvocation,
        ) -> ApplicationInvocationFuture<'_, Result<ApplicationResponse, InvocationError>> {
            Box::pin(async { Err(InvocationError::Unavailable) })
        }
    }

    impl DaemonInvocationExecutor for UnavailableExecutor {
        fn invoke_controlled(
            &self,
            _request: crate::daemon::DaemonInvocationRequest,
            _deadline: tracedecay_application::Deadline,
            _cancellation: tracedecay_application::CancellationSignal,
            _policy: InvocationCancellationPolicy,
        ) -> DaemonInvocationExecutorFuture<
            '_,
            Result<crate::daemon::DaemonInvocationResponse, DaemonInvocationError>,
        > {
            Box::pin(async { Err(DaemonInvocationError::Unavailable) })
        }

        fn observe_plan26_feedback(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            _event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
        ) -> DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn dashboard_work_routes_forward_to_the_production_application_router() {
        let executor: Arc<dyn DaemonInvocationExecutor> = Arc::new(UnavailableExecutor);
        let application = crate::application_surface::http_application_router_with_executor(
            executor,
            crate::daemon::daemon_operation_event_authority(),
            ProjectId::new("project.dashboard-work-routes").expect("project id"),
        )
        .expect("production application router");
        let router = Router::new().nest("/api/work", super::router(application));

        for route in super::registered_route_contracts() {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(route.path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .expect("dashboard Work request"),
                )
                .await
                .expect("dashboard Work response");
            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{}", route.path);
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{}",
                route.path
            );
        }
    }

    #[tokio::test]
    async fn dashboard_work_forwarding_preserves_the_http_exchange() {
        let application = Router::new().route(
            "/work/snapshot",
            post(|request: Request<Body>| async move {
                assert_eq!(
                    request.uri().path_and_query().map(|value| value.as_str()),
                    Some("/work/snapshot?page_size=7")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("x-tracedecay-test")
                        .and_then(|value| value.to_str().ok()),
                    Some("preserved")
                );
                assert_eq!(
                    to_bytes(request.into_body(), 1024)
                        .await
                        .expect("forwarded body"),
                    r#"{"cursor":"work:7"}"#
                );
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header("x-tracedecay-response", "preserved")
                    .body(Body::from(r#"{"kind":"problem"}"#))
                    .expect("upstream response")
            }),
        );
        let router = Router::new().nest("/api/work", super::router(application));

        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/work/snapshot?page_size=7")
                    .header("content-type", "application/json")
                    .header("x-tracedecay-test", "preserved")
                    .body(Body::from(r#"{"cursor":"work:7"}"#))
                    .expect("dashboard Work request"),
            )
            .await
            .expect("dashboard Work response");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get("x-tracedecay-response")
                .and_then(|value| value.to_str().ok()),
            Some("preserved")
        );
        assert_eq!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("dashboard response body"),
            r#"{"kind":"problem"}"#
        );
    }

    #[test]
    fn dashboard_public_routes_cover_every_canonical_core_work_binding() {
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
