//! Exact-project request selection and forwarding with read-only route admission.
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use tower::ServiceExt;
use tracedecay_api::{WorkOperation, WorkflowOperation};

use crate::application_routes::{
    ActiveProjectApplicationRoutes, selected_project_application_runtime,
};
use crate::{
    DashboardHttpRequestControlV1, DashboardState, graph_api, graph_binding, graph_service,
    projects,
};

pub(super) async fn active_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    req: Request<Body>,
) -> Response {
    forward_project_request(runtime.project_api_router(), runtime.active_state(), req).await
}

pub(super) async fn project_scoped_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    AxumPath((project_id, tail)): AxumPath<(String, String)>,
    mut req: Request<Body>,
) -> Response {
    if is_profile_owned_automation_skills_route(&tail) {
        return gateway_problem(
            StatusCode::NOT_FOUND,
            "not_project_scoped",
            "managed automation skills are profile-owned and are not available through project-qualified routes",
            Some(&project_id),
        );
    }
    if is_graph_search_tail(&tail)
        && let Some(refusal) = graph_binding::refuse_graph_search_scope(
            runtime.active_project_id().as_deref(),
            Some(project_id.as_str()),
        )
    {
        return Json(graph_api::graph_search_scope_envelope::<
            graph_service::GraphSearchPayloadV1,
        >(&runtime.active_state(), refusal))
        .into_response();
    }
    let application_read = selected_project_application_read(req.method(), &tail);
    let event_delivery_ack = is_selected_project_event_delivery_ack(req.method(), &tail);
    if runtime.active_project_id().as_deref() != Some(project_id.as_str())
        && !matches!(req.method(), &Method::GET | &Method::HEAD)
        && application_read.is_none()
        && !event_delivery_ack
    {
        return gateway_problem(
            StatusCode::METHOD_NOT_ALLOWED,
            "read_only_project",
            "project-scoped dashboard APIs are read-only for non-active projects",
            Some(&project_id),
        );
    }

    let selected = match runtime.selected_project_state(&project_id).await {
        Ok(selected) => selected,
        Err(projects::SelectedProjectError::RegistryNotMounted) => {
            return projects::registry_unavailable_response(
                &runtime.active_state(),
                "tracedecay project registry is unavailable".to_owned(),
            )
            .into_response();
        }
        Err(projects::SelectedProjectError::Failed(err))
            if projects::is_registry_unavailable_error(&err) =>
        {
            return projects::registry_unavailable_response(
                &runtime.active_state(),
                err.to_string(),
            )
            .into_response();
        }
        Err(projects::SelectedProjectError::Failed(err)) => {
            return gateway_problem(
                StatusCode::NOT_FOUND,
                "not_found",
                err.to_string(),
                Some(&project_id),
            );
        }
    };

    let query = req
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    if let Some(read) = application_read {
        let Some(project_graph) = selected.state.project_graph.as_deref() else {
            return gateway_problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                format!("selected project {read} authority is unavailable"),
                Some(&project_id),
            );
        };
        let application_runtime =
            if runtime.active_project_id().as_deref() == Some(project_id.as_str()) {
                selected.state.application_invocation_executor.clone()
            } else {
                match selected_project_application_runtime(
                    runtime
                        .active_state()
                        .application_invocation_executor
                        .as_ref(),
                    &project_graph.store_layout.project_root,
                ) {
                    Ok(application_runtime) => application_runtime,
                    Err(err) => {
                        return gateway_problem(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "unavailable",
                            format!("selected project {read} authority is unavailable: {err}"),
                            Some(&project_id),
                        );
                    }
                }
            };
        let application = match ActiveProjectApplicationRoutes::for_active_project(
            project_graph,
            application_runtime,
        ) {
            Ok(application) => application,
            Err(err) => {
                return gateway_problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    format!("selected project {read} authority is unavailable: {err}"),
                    Some(&project_id),
                );
            }
        };
        let (router, family) = match read {
            SelectedProjectApplicationRead::Feedback => {
                (application.dashboard_feedback_router, "feedback/")
            }
            SelectedProjectApplicationRead::Work => (application.dashboard_work_router, "work/"),
            // Stripping `application/` leaves the `/workflow/{operation}`
            // path the project's canonical application router mounts.
            SelectedProjectApplicationRead::Workflow => (application.http_router, "application/"),
        };
        let Some(operation) = tail.strip_prefix(family) else {
            return gateway_problem(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("invalid project-scoped {read} path"),
                None,
            );
        };
        let rewritten = format!("/{operation}{query}");
        return match rewritten.parse::<Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                match router.oneshot(req).await {
                    Ok(response) => response,
                    Err(err) => gateway_problem(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "error",
                        format!("dashboard {read} route failed: {err}"),
                        None,
                    ),
                }
            }
            Err(err) => gateway_problem(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("invalid project-scoped {read} path: {err}"),
                None,
            ),
        };
    }
    let rewritten = format!("/api/{tail}{query}");
    match rewritten.parse::<Uri>() {
        Ok(uri) => {
            *req.uri_mut() = uri;
            forward_project_request(runtime.project_api_router(), selected.state, req).await
        }
        Err(err) => gateway_problem(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("invalid project-scoped dashboard path: {err}"),
            None,
        ),
    }
}

/// One gateway refusal body; `project_id` is echoed when the refusal is about
/// that selected project.
fn gateway_problem(
    status: StatusCode,
    label: &str,
    detail: impl Into<String>,
    project_id: Option<&str>,
) -> Response {
    let mut body = json!({ "status": label, "detail": detail.into() });
    if let Some(project_id) = project_id {
        body["project_id"] = json!(project_id);
    }
    (status, Json(body)).into_response()
}

fn is_profile_owned_automation_skills_route(tail: &str) -> bool {
    tail == "automation/skills" || tail.starts_with("automation/skills/")
}

fn is_graph_search_tail(tail: &str) -> bool {
    tail == "plugins/graph/search"
}

/// A canonical application read a selected project answers for itself.
///
/// These are POSTs, so the gateway's read-only rule for non-active projects
/// would otherwise refuse them. They are admitted because they are served from
/// the selected project's own graph: the answer belongs to the project the
/// caller named, and no other project's data can appear under its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectedProjectApplicationRead {
    Feedback,
    Work,
    Workflow,
}

impl std::fmt::Display for SelectedProjectApplicationRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Feedback => "feedback",
            Self::Work => "Work",
            Self::Workflow => "Workflow",
        })
    }
}

pub(super) fn selected_project_application_read(
    method: &Method,
    tail: &str,
) -> Option<SelectedProjectApplicationRead> {
    if method != Method::POST {
        return None;
    }
    match tail {
        "feedback/get" | "feedback/expand" | "feedback/list" | "feedback/proximity" => {
            Some(SelectedProjectApplicationRead::Feedback)
        }
        _ => {
            if let Some(segment) = tail.strip_prefix("application/workflow/") {
                return WorkflowOperation::ALL
                    .into_iter()
                    .filter(|operation| operation.is_read_only())
                    .any(|operation| operation.route_segment() == segment)
                    .then_some(SelectedProjectApplicationRead::Workflow);
            }
            WorkOperation::ALL
                .into_iter()
                .filter(|operation| operation.is_read_only())
                .any(|operation| tail.strip_prefix("work/") == Some(operation.route_segment()))
                .then_some(SelectedProjectApplicationRead::Work)
        }
    }
}

pub(super) fn is_selected_project_event_delivery_ack(method: &Method, tail: &str) -> bool {
    method == Method::POST && tail == "events/delivery-ack"
}

async fn forward_project_request(
    project_api: Router<DashboardState>,
    state: DashboardState,
    req: Request<Body>,
) -> Response {
    tracing::Instrument::instrument(
        async move {
            let (mut parts, body) = req.into_parts();
            let request_control = parts
                .extensions
                .get::<DashboardHttpRequestControlV1>()
                .cloned();
            parts.extensions.clear();
            if let Some(request_control) = request_control {
                parts.extensions.insert(request_control);
            }
            let req = Request::from_parts(parts, body);
            match project_api.with_state(state).oneshot(req).await {
                Ok(response) => response,
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "status": "error",
                        "detail": format!("dashboard project route failed: {err}"),
                    })),
                )
                    .into_response(),
            }
        },
        tracing::trace_span!("dashboard_api.http.forward_project"),
    )
    .await
}
