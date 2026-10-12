//! Dashboard HTTP route table and the active-project and selected-project gateways.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, patch, post};
use serde_json::{Value, json};
use tower::ServiceExt;
use tracedecay_api::{WorkOperation, WorkflowOperation};
use tracedecay_automation_runtime::automation::backend;
use tracedecay_automation_runtime::automation::config::{AutomationBackend, AutomationHostMode};
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::application_surface::DashboardApplicationRuntime;
use crate::http::{DashboardHttpRequestControlV1, RequestControl};
use crate::state::{DashboardState, config_error, project_memory_owner};
use crate::tracedecay::DashboardProjectContext;
use crate::{
    analytics_api, automation_config_api, automation_fact_receipts_api, automation_jobs_api,
    automation_outcomes_api, automation_run_api, automation_scheduler_api, automation_skills_api,
    code_diagnostics_api, code_index_freshness_api, code_read_api, delivery_api,
    doctor_findings_api, events_api, events_delivery, explorer_api, feedback_api, graph_api,
    graph_binding, graph_service, graph_structure_api, lcm_api, loom_api, memory_api,
    multi_root_api, native_integration_api, projects, remote_status_api, savings_api, settings_api,
    storage_telemetry_api,
};

pub(crate) fn selected_project_application_runtime(
    active: Option<&Arc<dyn DashboardApplicationRuntime>>,
    project_root: &std::path::Path,
) -> Result<Option<Arc<dyn DashboardApplicationRuntime>>> {
    active
        .map(|runtime| runtime.for_project_root(project_root))
        .transpose()
        .map_err(config_error)
}

/// Canonical application routes bound to one exact project daemon.
///
/// The active project mounts every route below. The selected-project gateway
/// constructs only the dashboard feedback read subset from its retained graph.
pub(crate) struct ActiveProjectApplicationRoutes {
    pub(crate) http_router: Router,
    pub(crate) dashboard_configuration_router: Router,
    pub(crate) dashboard_feedback_router: Router,
    pub(crate) dashboard_work_router: Router,
    pub(crate) executor: Option<Arc<dyn DashboardApplicationRuntime>>,
}

impl ActiveProjectApplicationRoutes {
    fn for_active_project(
        cg: &DashboardProjectContext,
        executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    ) -> Result<Self> {
        let executor = executor
            .ok_or_else(|| config_error("active-project application runtime is not mounted"))?;
        let active_project_id = match project_memory_owner(cg)? {
            FactOwnerV1::Project { project_id } => project_id,
            FactOwnerV1::Profile => {
                return Err(config_error(
                    "active-project application routes require project authority",
                ));
            }
        };
        let routes = executor
            .routers(active_project_id)
            .map_err(|message| TraceDecayError::Config { message })?;
        Ok(Self {
            http_router: routes.http,
            dashboard_configuration_router: routes.configuration,
            dashboard_feedback_router: routes.feedback,
            dashboard_work_router: routes.work,
            executor: Some(executor),
        })
    }
}

/// Builds the complete dashboard router shared by direct and daemon-managed
/// startup. The supplied state is the active writable project authority.
///
/// `spa_routes` carries the embedded single-page-app surface, the app index,
/// `/static/{*tail}`, and the SPA fallback for unmatched non-API client routes
/// (`/brain?scope=…` deep links). It is built by the owning binary because the
/// bundle is generated into `OUT_DIR` by that crate's `build.rs`. It must be a
/// stateless `axum::Router` (it is merged after `.with_state(…)`), it must set
/// its own `.fallback(…)`, and it must not define any `/api/**` route, axum
/// panics on overlapping paths. Pass `Router::new()` to serve the JSON API
/// with no UI.
pub async fn router(
    cg: &DashboardProjectContext,
    mut state: DashboardState,
    spa_routes: Router,
) -> Result<Router> {
    // application primitive routes are bound to the active-project daemon. When the
    // daemon authority record is unavailable (standalone `tracedecay dashboard`
    // or the in-process test server), mounting them would otherwise fail the
    // whole server before it binds. Degrade gracefully instead, serve the core
    // dashboard and skip the `/api/application` surface.
    let application = match ActiveProjectApplicationRoutes::for_active_project(
        cg,
        state.application_invocation_executor.clone(),
    ) {
        Ok(application) => {
            state
                .application_invocation_executor
                .clone_from(&application.executor);
            Some(application)
        }
        Err(error) => {
            tracing::warn!("Active-project application routes skipped: {error}");
            None
        }
    };
    Ok(router_with_active_application(
        state,
        application,
        spa_routes,
    ))
}

pub(crate) fn router_with_active_application(
    state: DashboardState,
    application: Option<ActiveProjectApplicationRoutes>,
    spa_routes: Router,
) -> Router {
    let runtime = projects::DashboardRuntime::new(state, project_api_router());
    let router = Router::new()
        .route("/api/projects", get(projects::list))
        .route("/api/projects/{project_id}", get(projects::context))
        .route(
            "/api/projects/{project_id}/{*tail}",
            any(project_scoped_api_gateway),
        )
        .route("/api/capabilities", any(active_api_gateway))
        .route("/api/plugins/{*tail}", any(active_api_gateway))
        .route("/api/observatory", any(active_api_gateway))
        .route("/api/costs", any(active_api_gateway))
        .route("/api/automation/{*tail}", any(active_api_gateway))
        .route("/api/settings", any(active_api_gateway))
        .route("/api/settings/{*tail}", any(active_api_gateway))
        .route("/api/delivery/{*tail}", any(active_api_gateway))
        .route("/api/explorer/{*tail}", any(active_api_gateway))
        .route("/api/loom/{*tail}", any(active_api_gateway))
        // V2 read-model surfaces bound through the active-project gateway,
        // mirroring the project-scoped `/api/projects/{id}/…` gateway path.
        .route("/api/doctor/{*tail}", any(active_api_gateway))
        .route("/api/storage/{*tail}", any(active_api_gateway))
        .route("/api/code-index/{*tail}", any(active_api_gateway))
        .route("/api/remote/{*tail}", any(active_api_gateway))
        .route("/api/feedback/status", any(active_api_gateway))
        .route("/api/multi-root/{*tail}", any(active_api_gateway))
        .route("/api/native-integration/{*tail}", any(active_api_gateway))
        .route("/api/events", any(active_api_gateway))
        .route("/api/events/delivery-ack", any(active_api_gateway))
        .with_state(runtime)
        // Embedded SPA/static routes are supplied by the owning binary: the
        // asset bundle is generated into `OUT_DIR` by the root crate's
        // `build.rs` and included with `env!("OUT_DIR")`, which only resolves
        // inside the crate that ran that build script. `spa_routes` is merged
        // after `.with_state(…)`, so it is a stateless `axum::Router` and must
        // carry the SPA fallback itself (see [`spa_routes`] on the entry
        // points for the exact contract).
        .merge(spa_routes);
    match application {
        Some(application) => router
            .nest("/api/application", application.http_router)
            .nest("/api/work", application.dashboard_work_router)
            .nest("/api/feedback", application.dashboard_feedback_router)
            .nest(
                "/api/dashboard/application",
                application.dashboard_configuration_router,
            ),
        None => router,
    }
}

fn project_api_router() -> Router<DashboardState> {
    Router::new()
        .route("/api/capabilities", get(capabilities))
        .route(
            "/api/multi-root/collection",
            get(multi_root_api::resolve_collection),
        )
        .route(
            "/api/native-integration/status",
            get(native_integration_api::status),
        )
        .route("/api/feedback/status", get(feedback_api::status))
        // Holographic memory plugin API (mirrors holographic_plus plugin_api.py)
        .route("/api/plugins/holographic", get(memory_api::overview))
        .route("/api/plugins/holographic/status", get(memory_api::status))
        .route(
            "/api/plugins/holographic/fact/{fact_id}",
            get(memory_api::fact_detail),
        )
        .route(
            "/api/plugins/holographic/fact/{fact_id}/trust-history",
            get(memory_api::fact_trust_history),
        )
        .route(
            "/api/plugins/holographic/projection",
            get(memory_api::projection),
        )
        .route(
            "/api/plugins/holographic/similarity",
            get(memory_api::similarity),
        )
        .route(
            "/api/plugins/holographic/curation/config",
            get(automation_config_api::get_config).patch(automation_config_api::patch_config),
        )
        .route(
            "/api/automation/skills",
            get(automation_skills_api::list).post(automation_skills_api::create),
        )
        .route(
            "/api/automation/skills/{id}",
            get(automation_skills_api::view).patch(automation_skills_api::update),
        )
        .route(
            "/api/automation/skills/{id}/disable",
            post(automation_skills_api::disable),
        )
        .route(
            "/api/automation/skills/{id}/archive",
            post(automation_skills_api::archive),
        )
        .route(
            "/api/automation/skills/{id}/restore",
            post(automation_skills_api::restore),
        )
        .route(
            "/api/automation/automatic-fact-receipts",
            get(automation_fact_receipts_api::list),
        )
        .route(
            "/api/automation/automatic-fact-receipts/{id}",
            get(automation_fact_receipts_api::view),
        )
        .route(
            "/api/automation/jobs",
            get(automation_jobs_api::list).post(automation_jobs_api::create),
        )
        .route(
            "/api/automation/jobs/{id}",
            get(automation_jobs_api::view)
                .patch(automation_jobs_api::update)
                .delete(automation_jobs_api::delete),
        )
        .route(
            "/api/automation/jobs/{id}/run",
            post(automation_jobs_api::run),
        )
        .route(
            "/api/automation/scheduler/status",
            get(automation_scheduler_api::status),
        )
        .route(
            "/api/automation/scheduler/pause",
            post(automation_scheduler_api::pause),
        )
        .route(
            "/api/automation/scheduler/resume",
            post(automation_scheduler_api::resume),
        )
        .route(
            "/api/automation/outcomes",
            get(automation_outcomes_api::outcomes),
        )
        .route("/api/automation/runs", get(automation_run_api::run_list))
        .route(
            "/api/automation/runs/{run_id}/artifacts",
            get(automation_run_api::artifact_list),
        )
        .route(
            "/api/automation/runs/{run_id}/artifacts/{kind}",
            get(automation_run_api::artifact_payload),
        )
        .route("/api/plugins/holographic/oplog", get(memory_api::oplog))
        // LCM plugin API (mirrors hermes-lcm dashboard/plugin_api.py)
        .route("/api/plugins/hermes-lcm/overview", get(lcm_api::overview))
        .route("/api/plugins/hermes-lcm/search", get(lcm_api::search))
        .route(
            "/api/plugins/hermes-lcm/session/{session_id}",
            get(lcm_api::session),
        )
        .route("/api/plugins/hermes-lcm/timeline", get(lcm_api::timeline))
        // Code graph explorer API (project-local nodes / edges / files tables)
        .route("/api/plugins/graph/overview", get(graph_api::overview))
        .route("/api/plugins/graph/search", get(graph_api::search))
        .route("/api/plugins/graph/node/{node_id}", get(graph_api::node))
        .route(
            "/api/plugins/graph/node/{node_id}/neighbors",
            get(graph_api::neighbors),
        )
        .route("/api/plugins/graph/subgraph", get(graph_api::subgraph))
        .route("/api/plugins/graph/path", get(graph_api::path))
        .route(
            "/api/plugins/graph/shared-code/family",
            get(code_read_api::shared_family),
        )
        .route(
            "/api/plugins/graph/compare/union-layout",
            get(code_read_api::revision_pair),
        )
        .merge(graph_structure_api::contracted_routes())
        // Durable analytics API (hint lifecycle scaffolds + session usage rollups)
        .route(
            "/api/plugins/analytics/overview",
            get(analytics_api::overview),
        )
        .route("/api/observatory", get(analytics_api::observatory))
        .route("/api/plugins/analytics/agents", get(analytics_api::agents))
        .route(
            "/api/plugins/analytics/subagent-tree",
            get(analytics_api::subagent_tree),
        )
        .route("/api/plugins/analytics/hints", get(analytics_api::hints))
        .route("/api/plugins/analytics/usage", get(analytics_api::usage))
        .route(
            "/api/plugins/analytics/diagnostics",
            get(analytics_api::diagnostics),
        )
        .route(
            "/api/plugins/analytics/underused",
            get(analytics_api::underused),
        )
        // Code Diagnostics API (dashboard-only LSP diagnostics broker)
        .route(
            "/api/plugins/code-diagnostics",
            get(code_diagnostics_api::overview).patch(code_diagnostics_api::patch_settings),
        )
        .route(
            "/api/plugins/code-diagnostics/refresh",
            post(code_diagnostics_api::refresh_all),
        )
        .route(
            "/api/plugins/code-diagnostics/refresh/{language}",
            post(code_diagnostics_api::refresh_language),
        )
        // Savings & Cost API (savings ledger + session cost accounting)
        .route("/api/plugins/savings/overview", get(savings_api::overview))
        .route("/api/costs", get(savings_api::costs))
        .route("/api/plugins/savings/models", get(savings_api::models))
        // Settings API (aggregated project/user config + read-only env gates)
        .route("/api/settings", get(settings_api::get_settings))
        .route(
            "/api/settings/project",
            patch(settings_api::patch_project_settings),
        )
        .route(
            "/api/settings/user",
            patch(settings_api::patch_user_settings),
        )
        .route(
            "/api/settings/user/code-index-workers",
            patch(settings_api::patch_code_index_worker_settings),
        )
        .route("/api/explorer/queries", post(explorer_api::create_query))
        .route(
            "/api/explorer/queries/{run_id}",
            get(explorer_api::query_status).delete(explorer_api::cancel_query),
        )
        .route(
            "/api/explorer/sessions/{session_id}/size",
            get(explorer_api::session_size),
        )
        .route(
            "/api/explorer/sessions/{session_id}/read-context",
            get(explorer_api::read_context),
        )
        .route("/api/loom/temporal", get(loom_api::temporal))
        // V2 read-model surfaces (DashboardEnvelope<T>). Doctor finding
        // family, storage telemetry, code-index freshness, and
        // the typed SSE stream. See `read_model` for the normative envelope.
        // Read-only Doctor/health paths come from the API-owned descriptors in
        // `tracedecay_api::doctor` so the mount cannot drift from them.
        .route(
            tracedecay_api::doctor::DOCTOR_FINDINGS_ROUTE_PATH,
            get(doctor_findings_api::findings),
        )
        .route(
            "/api/storage/telemetry",
            get(storage_telemetry_api::telemetry),
        )
        .route(
            "/api/code-index/freshness",
            get(code_index_freshness_api::freshness),
        )
        .route("/api/remote/status", get(remote_status_api::status))
        .route("/api/delivery/inbox", get(delivery_api::inbox))
        .route("/api/delivery/overview", get(delivery_api::overview))
        .route("/api/events", get(events_api::events))
        .route(
            "/api/events/delivery-ack",
            post(events_delivery::acknowledge),
        )
}

async fn active_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    req: Request<Body>,
) -> Response {
    forward_project_request(runtime.project_api_router(), runtime.active_state(), req).await
}

async fn project_scoped_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    AxumPath((project_id, tail)): AxumPath<(String, String)>,
    mut req: Request<Body>,
) -> Response {
    if is_profile_owned_automation_skills_route(&tail) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "status": "not_project_scoped",
                "detail": "managed automation skills are profile-owned and are not available through project-qualified routes",
                "project_id": project_id,
            })),
        )
            .into_response();
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
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "status": "read_only_project",
                "detail": "project-scoped dashboard APIs are read-only for non-active projects",
                "project_id": project_id,
            })),
        )
            .into_response();
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
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "status": "not_found",
                    "detail": err.to_string(),
                    "project_id": project_id,
                })),
            )
                .into_response();
        }
    };

    let query = req
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    if let Some(read) = application_read {
        let Some(project_graph) = selected.state.project_graph.as_deref() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unavailable",
                    "detail": format!("selected project {read} authority is unavailable"),
                    "project_id": project_id,
                })),
            )
                .into_response();
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
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "status": "unavailable",
                                "detail": format!(
                                    "selected project {read} authority is unavailable: {err}"
                                ),
                                "project_id": project_id,
                            })),
                        )
                            .into_response();
                    }
                }
            };
        let application = match ActiveProjectApplicationRoutes::for_active_project(
            project_graph,
            application_runtime,
        ) {
            Ok(application) => application,
            Err(err) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "status": "unavailable",
                        "detail": format!(
                            "selected project {read} authority is unavailable: {err}"
                        ),
                        "project_id": project_id,
                    })),
                )
                    .into_response();
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
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status": "bad_request",
                    "detail": format!("invalid project-scoped {read} path"),
                })),
            )
                .into_response();
        };
        let rewritten = format!("/{operation}{query}");
        return match rewritten.parse::<Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                match router.oneshot(req).await {
                    Ok(response) => response,
                    Err(err) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({
                            "status": "error",
                            "detail": format!("dashboard {read} route failed: {err}"),
                        })),
                    )
                        .into_response(),
                }
            }
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status": "bad_request",
                    "detail": format!("invalid project-scoped {read} path: {err}"),
                })),
            )
                .into_response(),
        };
    }
    let rewritten = format!("/api/{tail}{query}");
    match rewritten.parse::<Uri>() {
        Ok(uri) => {
            *req.uri_mut() = uri;
            forward_project_request(runtime.project_api_router(), selected.state, req).await
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "bad_request",
                "detail": format!("invalid project-scoped dashboard path: {err}"),
            })),
        )
            .into_response(),
    }
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
pub(crate) enum SelectedProjectApplicationRead {
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

pub(crate) fn selected_project_application_read(
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

pub(crate) fn is_selected_project_event_delivery_ack(method: &Method, tail: &str) -> bool {
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

/// Capability discovery for hosts and future delegated-host extensions. The UI
/// (or a wrapper) can probe this to decide which panels/actions to enable.
#[tracing::instrument(name = "dashboard_api.http.capabilities", level = "trace", skip_all)]
pub(crate) async fn capabilities(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
) -> Json<Value> {
    let has_lcm = state.lcm_read_authority.is_some();
    let automation = automation_config_api::effective_automation_config(&state);
    let (automation_configured, automation_mode, automation_payload) = match automation {
        Ok((configuration_revision_id, config, codex)) => {
            let backend_supported = matches!(config.backend, AutomationBackend::CodexAppServer);
            let configured = config.enabled && backend_supported;
            let mode = if !configured {
                "disabled"
            } else if config.host_mode == AutomationHostMode::DelegatedHost {
                "delegated_host"
            } else {
                "standalone_backend"
            };
            (
                configured,
                mode,
                json!({
                    "available": true,
                    "configuration_revision_id": configuration_revision_id,
                    "enabled": config.enabled,
                    "mode": mode,
                    "backend": config.backend,
                    "host_mode": config.host_mode,
                    "availability": backend::backend_availability(&config, &codex),
                }),
            )
        }
        Err(error) => (
            false,
            "unavailable",
            json!({
                "available": false,
                "reason": error.to_string(),
                "required_authority": "pinned automation configuration",
            }),
        ),
    };
    let standalone_automation = automation_mode == "standalone_backend";
    // Multi-root reads are served by the daemon, never by the dashboard's own
    // stores. Capability discovery resolves through the mounted named-collection
    // resolver: with no configured default collection this reports the typed
    // no-collection state, and `/api/multi-root/collection` resolves explicit
    // targets through the same authority.
    let multi_root_resolver_mounted = state.application_invocation_executor.is_some();
    let multi_root = multi_root_api::resolve_collection_capability(&state, control, None).await;
    Json(json!({
        "name": "tracedecay-dashboard",
        "version": state.build_version,
        "mode": "standalone",
        "project_id": state.project_id,
        "project_root": state.project_root.display().to_string(),
        "storage_mode": state.storage_mode,
        "store_root": state.store_root.display().to_string(),
        "dashboard_root": state.dashboard_root.display().to_string(),
        "memory_db": state.mem_db_path,
        "graph_db": state.graph_db_path,
        "lcm_db": state.lcm_db_path,
        "lcm_scope": state.lcm_scope,
        "session_authority": state.session_authority.as_str(),
        "multi_root": multi_root,
        "features": {
            "memory": true,
            "lcm": has_lcm,
            "graph": true,
            "analytics": true,
            "feedback": state.feedback_status_reader.is_some(),
            "code_diagnostics": state.code_diagnostics_authority.is_some(),
            // Memory curation/refinement is served by the configured
            // standalone automation backend. Explicit operations use the
            // canonical retained application surface.
            "curation": true,
            "automation": automation_configured,
            "llm_curation": standalone_automation,
            "managed_skills": true,
            // Savings & Cost tab: savings-ledger analytics + per-session
            // cost accounting with OpenRouter-backed pricing.
            "savings": true,
            // Settings tab: aggregated project/user config editing plus
            // read-only environment and storage-path display.
            "settings": true,
            // Named-collection resolution is mounted whenever the daemon
            // application transport is admitted for this dashboard.
            "multi_root": multi_root_resolver_mounted,
        },
        "automation": automation_payload,
        "dashboards": ["tracedecay"],
    }))
}
