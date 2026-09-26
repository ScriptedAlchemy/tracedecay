//! Root-side dashboard composition: the graph-to-context mapping plus the
//! daemon-coupled integration fixtures.
//!
//! The dashboard API, routes, read models, services and their tests, lives
//! in `crates/tracedecay-dashboard-api`; callers import it directly.
//!
//! The embedded asset bundle is not generated here: the shipping binary crate
//! embeds it and hands it to this library through the registered product
//! runtime ([`mod@tracedecay_project::product_runtime`]). The canonical API crate owns the
//! resulting HTTP router and transport policy.

use tracedecay_dashboard_api::DashboardProjectContext;
#[cfg(feature = "test-transport")]
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

#[cfg(feature = "test-transport")]
use tracedecay_daemon_service::DaemonInvocationService;
#[cfg(feature = "test-transport")]
use tracedecay_dashboard_api::{
    DashboardApplicationRuntime, DashboardAutomationAuthorityV1, DashboardAutomationWriter,
    DashboardGitCorrelationReadPortV1, DashboardHostAdmissionTestAuthorityV1,
    DashboardLcmReadPortV1, DashboardProfileCodeIndexWorkerSettingsPort, DashboardTestEndpointV1,
    DashboardTestProjectGraphsV1, standalone_dashboard_automation_writer,
};
#[cfg(feature = "test-transport")]
use tracedecay_session_runtime::session_retrieval::{
    DaemonSessionRetrievalRoot, DaemonSessionRetrievalService, SessionRetrievalServingIdentityV1,
};

/// Canonical observation-capture seeding for dashboard integration fixtures.
#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub mod observation_seed;

/// Test-only graph fixture. Compiled only under `test-transport`.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
#[path = "dashboard_graph_test_runtime.rs"]
pub mod dashboard_graph_test_runtime;

/// Installs the canonical root-owned registered schema port before dashboard
/// integration fixtures open any database authority.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub fn register_test_schema_installer() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(tracedecay_global_db::register_registered_schema_installer);
}

#[doc(hidden)]
/// The dashboard context of `graph`, owned by `profile` when a daemon serves
/// it, else by the profile the graph was opened in.
pub fn dashboard_project_context(
    graph: &tracedecay_project::project::TraceDecay,
    profile: Option<&tracedecay_runtime_core::config::ProfileRoot>,
) -> tracedecay_domain::errors::Result<DashboardProjectContext> {
    let profile = match profile {
        Some(profile) => profile.clone(),
        None => tracedecay_runtime_core::config::ProfileRoot::new(graph.profile_root()?),
    };
    Ok(DashboardProjectContext {
        profile,
        store_layout: graph.store_layout().clone(),
        dashboard_db_path: graph.dashboard_db_path(),
        dashboard_database: graph.dashboard_database_guard(),
        retention_config: graph.get_config().sync.retention.clone(),
        host_io: tracedecay_agent_hosts::host_io(),
        user_settings_client: graph.configuration_runtime().user_settings_client(),
    })
}

#[cfg(feature = "test-transport")]
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_until_shutdown_for_tests_with_host_admission<F>(
    graph: std::sync::Arc<tracedecay_project::project::TraceDecay>,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    authority: DashboardHostAdmissionTestAuthorityV1,
    project_graphs: DashboardTestProjectGraphsV1,
    endpoint: DashboardTestEndpointV1<'_>,
    build_version: &'static str,
    spa_routes: axum::Router,
    shutdown: F,
) -> tracedecay_domain::errors::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tracedecay_dashboard_api::run_until_shutdown_for_tests_with_host_admission(
        std::sync::Arc::new(dashboard_project_context(&graph, Some(profile))?),
        authority,
        project_graphs,
        endpoint,
        build_version,
        spa_routes,
        shutdown,
    )
    .await
}

/// Composes the production dashboard automation authority over one retained
/// integration-test graph. The returned writer is the same serialization
/// authority captured by managed-skill mutation/materialization and must be
/// mounted into dashboard state with the authority. Runs use canonical runner
/// locking directly, so a model turn never holds this broad writer. The
/// retained invocation service is mounted with the graph's exact project
/// observability identity and retained application runtime before that
/// authority can admit backend execution.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn dashboard_automation_authority_for_test(
    cg: std::sync::Arc<tracedecay_project::project::TraceDecay>,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
) -> tracedecay_domain::errors::Result<(DashboardAutomationAuthorityV1, DashboardAutomationWriter)>
{
    let profile_root = canonical_existing_identity(profile.data_dir())?;
    let project_root = canonical_existing_identity(cg.project_root())?;
    let configuration = hotpath::future!(
        cg.configuration_runtime().client().current(),
        label = "dashboard.automation.configuration"
    )
    .await
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("dashboard automation fixture configuration is unavailable: {error}"),
    })?;
    let configured_project_root =
        canonical_existing_identity(&configuration.target().project_root)?;
    if configured_project_root != project_root {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "dashboard automation fixture configuration resolved a different project root"
                .to_owned(),
        });
    }
    let project_id = configuration.target().project_id.clone();
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&project_root, &project_id)
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("dashboard automation fixture scope is invalid: {error}"),
            })?;
    let project_database = hotpath::future!(
        cg.store_runtime_registry()
            .project_sessions(project_id.clone(), [project_root.clone()]),
        label = "dashboard.automation.project_sessions"
    )
    .await?;
    let configuration_policy_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        &configuration.snapshot().effective_behavior_digest,
        &configuration.snapshot().resolution_provenance_digest,
    ))
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("dashboard automation fixture policy digest failed: {error}"),
    })?;
    let writer = standalone_dashboard_automation_writer();
    let resident_memory = std::sync::Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::new(
            tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1(),
        ),
    );
    let invocation_service = DaemonInvocationService::with_code_index_schedulers(
        tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1::with_resident_memory(
            1,
            resident_memory,
        ),
    )
    .with_owner_home(profile.home().map(std::path::Path::to_path_buf));
    hotpath::future!(
        invocation_service.mount_observability_producer(
            project_root.clone(),
            project_database,
            project_id.clone(),
            configuration.snapshot().effective_behavior_digest.clone(),
            configuration_policy_digest,
        ),
        label = "dashboard.automation.mount"
    )
    .await?;
    hotpath::future!(
        crate::daemon::register_dashboard_test_retained_runtime(
            &invocation_service,
            &cg,
            project_root.clone(),
            project_id,
        ),
        label = "dashboard.automation.runtime"
    )
    .await?;
    let authority =
        crate::daemon::dashboard_automation::compose_dashboard_automation_authority_for_test(
            profile_root,
            cg,
            std::sync::Arc::clone(&writer),
            invocation_service,
        )?;
    Ok((authority, writer))
}

/// Mounts the canonical daemon configuration mutation service and the same
/// ProfileSessions worker-settings adapter used by production dashboards.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn dashboard_configuration_authorities_for_test(
    cg: std::sync::Arc<tracedecay_project::project::TraceDecay>,
    profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
) -> tracedecay_domain::errors::Result<(
    std::sync::Arc<dyn DashboardApplicationRuntime>,
    std::sync::Arc<dyn DashboardProfileCodeIndexWorkerSettingsPort>,
)> {
    crate::daemon::dashboard_configuration_authorities_for_test(cg, profile_database).await
}

/// Composes the daemon-owned LCM read authority over the fixture's
/// registered project-sessions store, the same `DashboardLcmReadAdapter`
/// over the daemon session retrieval service that the MCP dashboard
/// composition mounts in production. Without it every `hermes-lcm` and
/// explorer session read answers `lcm_daemon_authority_unavailable`.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn dashboard_lcm_read_authority_for_test(
    cg: &tracedecay_project::project::TraceDecay,
    registry: &tracedecay_global_db::RegisteredGlobalDb,
    project_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
) -> Option<std::sync::Arc<dyn DashboardLcmReadPortV1>> {
    let serving_db = cg.db_path();
    let project_id = cg.store_layout().identity.project_id.as_deref()?;
    let serving = hotpath::future!(
        SessionRetrievalServingIdentityV1::resolve_project(
            project_id,
            &serving_db,
            cg.serving_branch(),
            cg.project_root(),
            &project_database.binding().shard_id.profile_id,
            &project_database.binding().shard_id,
            registry,
        ),
        label = "dashboard.lcm.root"
    )
    .await?;
    let root = DaemonSessionRetrievalRoot::project(serving, registry).await?;
    let identity = root.identity().clone();
    let service =
        DaemonSessionRetrievalService::new_without_refresh_worker(project_database.clone(), root)?;
    let adapter = tracedecay_mcp::handlers::dashboard_lcm::DashboardLcmReadAdapter::new(
        std::sync::Arc::new(service),
        identity,
    )?;
    Some(std::sync::Arc::new(adapter))
}

/// Composes the daemon-owned git-correlation read authority over the
/// fixture's registered project-sessions store, the same
/// `DashboardGitCorrelationReadAdapter` the MCP dashboard composition mounts
/// in production. Without it Loom's session↔commit and branch/worktree
/// sources answer their typed unavailable states.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub fn dashboard_git_correlation_read_authority_for_test(
    project_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
) -> std::sync::Arc<dyn DashboardGitCorrelationReadPortV1> {
    std::sync::Arc::new(
        tracedecay_mcp::handlers::dashboard_git_correlation::DashboardGitCorrelationReadAdapter::new(
            project_database,
        ),
    )
}

/// Records one git span through a registered ProjectSessions authority.
///
/// Root-owned bridge for the dashboard integration suite: span evidence is
/// published through the same graph-backed correlation store the dashboard
/// APIs read, which is crate-internal.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn record_project_span_for_test(
    project_database: &tracedecay_global_db::RegisteredGlobalDb,
    observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
    merge_gap_secs: i64,
) -> tracedecay_domain::errors::Result<i64> {
    hotpath::future!(
        tracedecay_global_db::GlobalDbGitCorrelationStore::new(project_database)
            .record_span_observation(observation, merge_gap_secs),
        label = "dashboard.span.persist"
    )
    .await
    .map_err(
        |error| tracedecay_domain::errors::TraceDecayError::Database {
            operation: "record dashboard test git span".to_owned(),
            message: error.to_string(),
        },
    )
}

#[cfg(test)]
mod spa_router_tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn unknown_api_paths_never_receive_the_single_page_app() {
        let response = tracedecay_api::static_dashboard_router(std::sync::Arc::new(
            tracedecay_project::product_runtime::FIXTURE_DASHBOARD_ASSETS,
        ))
        .oneshot(
            Request::builder()
                .uri("/api/not-a-real-route")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("SPA router response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
