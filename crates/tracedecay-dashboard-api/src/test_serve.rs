//! Test-transport listener. Callers pass the same host-admission authorities the
//! daemon retains, and this module serves the production router on loopback.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use crate::access::DashboardAccessToken;
use crate::application_surface::DashboardApplicationRuntime;
use crate::automation_authority::DashboardAutomationAuthorityV1;
use crate::automation_run_service::{
    DashboardAutomationWriter, standalone_dashboard_automation_writer,
};
use crate::code_read_api;
use crate::delivery_api::DashboardDeliveryReadPortV1;
use crate::http::{bind_dashboard, with_dashboard_http_admission};
use crate::lcm_api::DashboardLcmReadPortV1;
use crate::loom_api::DashboardGitCorrelationReadPortV1;
use crate::routes::router;
use crate::session_authority::{
    DashboardSessionAuthoritiesV1, DashboardSessionMountV1, DashboardSessionResolutionV1,
};
use crate::settings_api::{
    DashboardProfileCodeIndexWorkerSettingsPort, PrAutoTrackManagedSummaryReader,
};
use crate::state::{DashboardStateCompositionV1, build_state_inner, config_error};
use crate::tracedecay::DashboardProjectContext;

/// Test-only lifetime owner for the same registered authorities retained by a
/// daemon dashboard. Integration tests pass the typed host-admission runtime;
/// raw database handles never cross the public test seam.
pub struct DashboardHostAdmissionTestAuthorityV1 {
    _runtime: Arc<dyn Send + Sync>,
    project_sessions: RegisteredGlobalDbLeaseV1,
    profile_database: RegisteredGlobalDbLeaseV1,
    automation_authority: Option<DashboardAutomationAuthorityV1>,
    automation_writer: Option<DashboardAutomationWriter>,
    lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    code_graph_read_admission: Option<Arc<dyn crate::graph::CodeGraphReadAdmissionPort>>,
    code_graph_projection_read_port: Option<Arc<dyn crate::graph::CodeGraphProjectionReadPort>>,
    code_read_authority: Option<code_read_api::DashboardCodeReadAuthorityV1>,
    git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
    delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    code_index_freshness_reader: Option<CodeIndexFreshnessReader>,
    profile_code_index_worker_settings:
        Option<Arc<dyn DashboardProfileCodeIndexWorkerSettingsPort>>,
    application_invocation_executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    pr_autotrack_reader: Option<PrAutoTrackManagedSummaryReader>,
    opening_project_sessions: Option<tokio::sync::watch::Receiver<DashboardSessionResolutionV1>>,
}

#[cfg(feature = "test-transport")]
impl DashboardHostAdmissionTestAuthorityV1 {
    pub fn new<T>(
        runtime: Arc<T>,
        profile_database: RegisteredGlobalDbLeaseV1,
        project_sessions: RegisteredGlobalDbLeaseV1,
    ) -> Self
    where
        T: Send + Sync + 'static,
    {
        Self {
            _runtime: runtime,
            project_sessions,
            profile_database,
            automation_authority: None,
            automation_writer: None,
            lcm_read_authority: None,
            code_graph_read_admission: None,
            code_graph_projection_read_port: None,
            code_read_authority: None,
            git_correlation_read_authority: None,
            delivery_read_authority: None,
            code_index_freshness_reader: None,
            profile_code_index_worker_settings: None,
            application_invocation_executor: None,
            pr_autotrack_reader: None,
            opening_project_sessions: None,
        }
    }

    /// Composes the dashboard as the daemon does for a project that is still
    /// opening: the session authorities mount once `publication` resolves.
    #[must_use]
    pub fn with_opening_project_sessions(
        mut self,
        publication: tokio::sync::watch::Receiver<DashboardSessionResolutionV1>,
    ) -> Self {
        self.opening_project_sessions = Some(publication);
        self
    }

    /// The session authorities this runtime admits, as an opening project's
    /// publication hands them over.
    pub fn project_session_authorities(&self) -> DashboardSessionAuthoritiesV1 {
        DashboardSessionAuthoritiesV1 {
            project_sessions: self.project_sessions.clone(),
            lcm_read_authority: self.lcm_read_authority.clone(),
            git_correlation_read_authority: self.git_correlation_read_authority.clone(),
        }
    }

    pub fn with_pr_autotrack_reader(mut self, reader: PrAutoTrackManagedSummaryReader) -> Self {
        self.pr_autotrack_reader = Some(reader);
        self
    }

    /// Attaches the daemon-owned application runtime used by mutating
    /// dashboard routes in an integration-test transport.
    #[must_use]
    pub fn with_application_invocation_executor(
        mut self,
        executor: Arc<dyn DashboardApplicationRuntime>,
    ) -> Self {
        self.application_invocation_executor = Some(executor);
        self
    }

    /// Attaches the same exact-profile automation authority production retains
    /// from the daemon composition root.
    #[must_use]
    pub fn with_automation_authority(
        mut self,
        automation_authority: DashboardAutomationAuthorityV1,
        automation_writer: DashboardAutomationWriter,
    ) -> Self {
        self.automation_authority = Some(automation_authority);
        self.automation_writer = Some(automation_writer);
        self
    }

    /// Attaches the daemon-owned LCM read authority so the test transport
    /// serves the same `hermes-lcm`/explorer session reads production mounts.
    #[must_use]
    pub fn with_lcm_read_authority(
        mut self,
        lcm_read_authority: Arc<dyn DashboardLcmReadPortV1>,
    ) -> Self {
        self.lcm_read_authority = Some(lcm_read_authority);
        self
    }

    /// Attaches the exact-project admission and verified projection ports used
    /// by production dashboard code reads.
    #[must_use]
    pub fn with_code_graph_authority(
        mut self,
        admission: Arc<dyn crate::graph::CodeGraphReadAdmissionPort>,
        projection: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
    ) -> Self {
        self.code_graph_read_admission = Some(admission);
        self.code_graph_projection_read_port = Some(projection);
        self
    }

    /// Attaches the verified shared-family and revision-pair read authority.
    #[must_use]
    pub fn with_code_read_authority(
        mut self,
        authority: code_read_api::DashboardCodeReadAuthorityV1,
    ) -> Self {
        self.code_read_authority = Some(authority);
        self
    }

    /// Attaches the daemon-owned git-correlation read authority so the test
    /// transport serves the same Loom git-evidence reads production mounts.
    #[must_use]
    pub fn with_git_correlation_read_authority(
        mut self,
        git_correlation_read_authority: Arc<dyn DashboardGitCorrelationReadPortV1>,
    ) -> Self {
        self.git_correlation_read_authority = Some(git_correlation_read_authority);
        self
    }

    /// Attaches the daemon-owned Delivery read authority so the test
    /// transport serves the same `/api/delivery/*` provider reads production
    /// mounts.
    #[must_use]
    pub fn with_delivery_read_authority(
        mut self,
        delivery_read_authority: Arc<dyn DashboardDeliveryReadPortV1>,
    ) -> Self {
        self.delivery_read_authority = Some(delivery_read_authority);
        self
    }

    /// Attaches a code-index freshness reader so the test transport can
    /// join provider reads against an indexed head, the same admission gate
    /// production's Delivery inbox requires before mounting a provider read.
    #[must_use]
    pub fn with_code_index_freshness_reader(
        mut self,
        code_index_freshness_reader: CodeIndexFreshnessReader,
    ) -> Self {
        self.code_index_freshness_reader = Some(code_index_freshness_reader);
        self
    }

    /// Attaches the exact ProfileSessions worker settings port used by the
    /// production dashboard composition. Tests keep the same separate CAS
    /// resource instead of routing this preference through project settings.
    #[must_use]
    pub fn with_profile_code_index_worker_settings(
        mut self,
        profile_code_index_worker_settings: Arc<dyn DashboardProfileCodeIndexWorkerSettingsPort>,
    ) -> Self {
        self.profile_code_index_worker_settings = Some(profile_code_index_worker_settings);
        self
    }
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
#[derive(Clone, Default)]
pub struct DashboardTestProjectGraphsV1 {
    graphs:
        Arc<std::sync::RwLock<std::collections::HashMap<PathBuf, Arc<DashboardProjectContext>>>>,
}

#[cfg(feature = "test-transport")]
impl DashboardTestProjectGraphsV1 {
    pub fn register(&self, graph: Arc<DashboardProjectContext>) {
        self.graphs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(graph.store_layout.project_root.clone(), graph);
    }

    fn resolver(&self) -> crate::project_graph::RetainedProjectGraphResolver {
        let graphs = Arc::clone(&self.graphs);
        Arc::new(move |request| {
            let graph = graphs
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&request.registered_root)
                .cloned();
            Box::pin(async move { Ok(graph) })
        })
    }
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
pub struct DashboardTestEndpointV1<'a> {
    pub host: &'a str,
    pub port: u16,
    pub access: DashboardAccessToken,
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
pub async fn run_until_shutdown_for_tests_with_host_admission<F>(
    cg: Arc<DashboardProjectContext>,
    authority: DashboardHostAdmissionTestAuthorityV1,
    project_graphs: DashboardTestProjectGraphsV1,
    endpoint: DashboardTestEndpointV1<'_>,
    build_version: &'static str,
    spa_routes: Router,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let project_graph_resolver = project_graphs.resolver();
    run_until_shutdown_inner(
        cg.as_ref(),
        DashboardRunRequest {
            host: endpoint.host,
            port: endpoint.port,
            access: endpoint.access,
            build_version,
            spa_routes,
            test_authority: Some(&authority),
            test_project_graph_resolver: Some(project_graph_resolver),
            test_project_graph: Some(Arc::clone(&cg)),
        },
        shutdown,
    )
    .await
}

#[cfg(feature = "test-transport")]
struct DashboardRunRequest<'a> {
    host: &'a str,
    port: u16,
    access: DashboardAccessToken,
    /// The owning composition's build version; served surfaces must report the
    /// caller's product runtime, never a crate-local substitute.
    build_version: &'static str,
    spa_routes: Router,
    test_authority: Option<&'a DashboardHostAdmissionTestAuthorityV1>,
    test_project_graph_resolver: Option<crate::project_graph::RetainedProjectGraphResolver>,
    test_project_graph: Option<Arc<DashboardProjectContext>>,
}

#[cfg(feature = "test-transport")]
async fn run_until_shutdown_inner<F>(
    cg: &DashboardProjectContext,
    request: DashboardRunRequest<'_>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let DashboardRunRequest {
        host,
        port,
        access,
        build_version,
        spa_routes,
        test_authority,
        test_project_graph_resolver,
        test_project_graph,
    } = request;
    // A directly served dashboard is not handed a daemon-owned analyzer broker,
    // so it opens the same one the MCP server mounts. Without it the code
    // diagnostics surface would answer unsupported purely because of which
    // entry point started the dashboard.
    let code_diagnostics_broker =
        crate::application::dashboard_diagnostics::open_diagnostic_broker(
            Some(&cg.profile),
            cg.store_layout.project_root.clone(),
            &cg.store_layout.dashboard_root,
        )
        .await;
    let state = build_state_inner(
        cg,
        test_project_graph,
        // The test harness is the only remaining direct-serve entry; the
        // production dashboard is served by the daemon MCP tool, which warms
        // token counts through its own composition.
        false,
        DashboardStateCompositionV1 {
            build_version,
            project_graph_resolver: test_project_graph_resolver,
            code_graph_read_admission: test_authority
                .and_then(|authority| authority.code_graph_read_admission.clone()),
            code_graph_projection_read_port: test_authority
                .and_then(|authority| authority.code_graph_projection_read_port.clone()),
            code_read_authority: test_authority
                .and_then(|authority| authority.code_read_authority.clone()),
            project_sessions: match test_authority {
                Some(authority) => match &authority.opening_project_sessions {
                    Some(publication) => DashboardSessionMountV1::Opening(publication.clone()),
                    None => DashboardSessionMountV1::Ready(authority.project_session_authorities()),
                },
                None => DashboardSessionMountV1::Unavailable,
            },
            profile_code_index_worker_settings: test_authority
                .and_then(|authority| authority.profile_code_index_worker_settings.clone()),
            delivery_read_authority: test_authority
                .and_then(|authority| authority.delivery_read_authority.clone()),
            registered_savings_db: test_authority
                .map(|authority| authority.profile_database.clone()),
            automation_authority: test_authority
                .and_then(|authority| authority.automation_authority.clone()),
            automation_observation: None,
            automation_scheduler_reconciler: None,
            automation_writer: test_authority
                .and_then(|authority| authority.automation_writer.clone())
                .unwrap_or_else(standalone_dashboard_automation_writer),
            doctor_report_reader: None,
            remote_operational_status_reader: None,
            code_index_freshness_reader: test_authority
                .and_then(|authority| authority.code_index_freshness_reader.clone()),
            feedback_status_reader: None,
            pr_autotrack_reader: test_authority
                .and_then(|authority| authority.pr_autotrack_reader.clone()),
            code_diagnostics_broker: Some(code_diagnostics_broker),
            application_invocation_executor: test_authority
                .and_then(|authority| authority.application_invocation_executor.clone()),
            delivery_settlement_authority: None,
        },
    )
    .await?;
    let app = router(cg, state, spa_routes).await?;
    let (listener, addr) = bind_dashboard(host, port).await?;
    let url = access.launch_url(addr);
    let app = with_dashboard_http_admission(app, addr, access);

    // Stable, parseable line for wrappers (the Hermes plugin reads this).
    println!("tracedecay dashboard listening on {url}");
    eprintln!(
        "Dashboard bound to launch project {}",
        cg.store_layout.project_root.display()
    );
    eprintln!("Code search covers this project only; rebound with --path to serve another.");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| config_error(format!("dashboard server error: {e}")))
}
