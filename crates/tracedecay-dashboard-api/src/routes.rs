//! Dashboard route composition over active and selected project gateways.
use axum::Router;
use axum::routing::{any, get, patch, post};
use tracedecay_domain::errors::Result;

use crate::application_routes::ActiveProjectApplicationRoutes;
use crate::capabilities::capabilities;
use crate::project_gateway::{active_api_gateway, project_scoped_api_gateway};
use crate::{
    DashboardProjectContext, DashboardState, analytics_api, automation_config_api,
    automation_fact_receipts_api, automation_jobs_api, automation_outcomes_api, automation_run_api,
    automation_scheduler_api, automation_skills_api, code_diagnostics_api,
    code_index_freshness_api, code_read_api, delivery_api, doctor_findings_api, events_api,
    events_delivery, explorer_api, feedback_api, graph_api, graph_structure_api, lcm_api, loom_api,
    memory_api, multi_root_api, native_integration_api, projects, remote_status_api, savings_api,
    settings_api, storage_telemetry_api,
};

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

pub(super) fn router_with_active_application(
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

pub(super) fn project_api_router() -> Router<DashboardState> {
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
