//! `tracedecay dashboard` — local HTTP server for the dashboard UIs.
//!
//! Serves TraceDecay's embedded dashboard and its project-scoped JSON APIs:
//!
//! - `/api/plugins/holographic/*`  → canonical project facts, verified Grafeo
//!   topology, and deterministic FHRR projections derived on read
//! - `/api/plugins/hermes-lcm/*`   → LCM session store
//!   (`lcm_raw_messages` / `lcm_summary_nodes` in the resolved active project
//!   store where transcript ingest writes; see [`resolve_lcm_store`] for the
//!   fail-closed authority selection)
//!
//! `/api/capabilities` advertises which current TraceDecay authorities are
//! mounted for the selected project.

pub use tracedecay_agent_hosts::analytics;
pub use tracedecay_usecases as application;
pub use tracedecay_usecases::{git_query, graph, request_identity, user_config};
pub mod tracedecay;
// Crate-root re-exports the composition root reaches through its
// `crate::dashboard::*` shim: the application-surface injection contract and
// the dashboard-facing project runtime trait.
pub use application_surface::{
    DashboardApplicationRouters, DashboardApplicationRuntime, DashboardConfigurationApplyError,
    DashboardConfigurationApplyFuture,
};
pub use tracedecay::DashboardProjectRuntime;

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
///
/// `Database::publish_test_runtime` materialises a profile-scoped sidecar shard
/// that the kernel initialises through
/// `tracedecay_runtime_core::ports::registered_schema`. That port fails closed
/// until the real schema — owned by `tracedecay-global-db` — is registered.
/// Production wires it from the daemon composition root; this crate's test
/// target reuses the identical installer through its `test-helpers`
/// dev-dependency. Idempotent: the port keeps the first registration, so every
/// fixture entry point can call it unconditionally.
///
/// Fixtures built on `tracedecay_global_db::tests::harness` register the
/// installer themselves; only fixtures that reach `publish_test_runtime`
/// directly need this call.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}

pub mod analytics_api;
pub mod application_surface;
mod automation_authority;
pub use automation_authority::{
    DashboardAutomationAuthorityErrorV1, DashboardAutomationAuthorityV1,
    DashboardAutomationRunFutureV1, DashboardAutomationRunInvocationV1,
    DashboardAutomationRunOutcomeV1, DashboardAutomationRunPortV1, DashboardAutomationRunRequestV1,
    DashboardManagedSkillCommandFutureV1, DashboardManagedSkillCommandInvocationV1,
    DashboardManagedSkillCommandOutcomeV1, DashboardManagedSkillCommandPortV1,
    DashboardManagedSkillCommandV1,
};
pub(crate) use automation_authority::{
    automation_authority_error_response, exact_automation_authority,
};
mod automation_config_api;
mod automation_fact_receipts_api;
mod automation_jobs_api;
mod automation_outcomes_api;
mod automation_run_api;
mod automation_run_service;
pub use automation_run_service::{
    DashboardAutomationWriter, standalone_dashboard_automation_writer,
};
mod automation_scheduler_api;
mod automation_skills_api;
mod cloud;
mod code_diagnostics_api;
pub mod code_index_freshness_api;
pub mod config;
#[doc(hidden)]
pub mod contract_schema;
mod delivery_api;
pub use delivery_api::{DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1};
mod doctor_findings_api;
mod events_api;
mod events_delivery;
mod explorer_api;
mod remote_status_api;
pub use explorer_api::{
    ExplorerSemanticReadFuture, ExplorerSemanticReadV1, ExplorerSemanticReader,
};
pub mod feedback_api;
mod graph_api;
mod graph_service;
mod graph_structure_api;
pub mod hooks;
mod lcm_api;
pub use lcm_api::{
    DashboardLcmCanonicalMatchesV1, DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1,
    DashboardLcmCanonicalStatsV1, DashboardLcmCanonicalSummaryV1, DashboardLcmReadFutureV1,
    DashboardLcmReadOutcomeV1, DashboardLcmReadPortV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1, DashboardLcmTimelineBucketV1,
};
mod loom_api;
pub use loom_api::{
    DashboardGitCorrelationReadErrorV1, DashboardGitCorrelationReadFutureV1,
    DashboardGitCorrelationReadPortV1, DashboardGitCorrelationReadV1,
};
mod memory_analysis;
mod memory_api;
mod memory_service;
pub mod project_graph;
pub mod project_registry;
mod projects;
mod read_model;
mod request_deadline;
mod savings_api;
use tracedecay_usecases::provider_pricing as savings_pricing;
pub mod scope;
mod settings_api;
pub use settings_api::{
    DashboardPrAutoTrackEntryV1, DashboardPrAutoTrackReadPort,
    install_dashboard_pr_autotrack_read_port,
};
mod storage_findings_api;
mod storage_telemetry_api;
#[cfg(test)]
mod test_support;
mod token_count;
mod util;
mod version;
pub use version::install_build_version;
mod work_api;

use request_deadline::dashboard_http_request_deadline_micros;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, patch, post};
use serde_json::{Value, json};
use tower::ServiceExt;

use tracedecay_api::{WorkOperation, WorkflowOperation};

use crate::tracedecay::TraceDecay;
use tracedecay_agent_hosts::automation::backend;
use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationHostMode};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::{
    Database, DatabaseEngineReadConnection, DatabaseStorageTelemetryHandle,
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::{StorageMode, StoreLayout};

/// Default port for `tracedecay dashboard` (chosen to avoid common dev-server
/// defaults; override with `--port`).
pub const DEFAULT_PORT: u16 = 7341;
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSchedulerReconcileOutcome {
    Started,
    RunningNotified,
    Exiting,
    Finished,
    Retiring,
    NotConfigured,
    LifecycleInactive,
    OwnerUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationReconcileScope {
    Project,
    Profile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UncachedProjectReconcileOutcome {
    DeferredUntilProjectStartup,
}

#[derive(Debug, serde::Serialize)]
pub struct AutomationSchedulerOwnerReconcileOutcome {
    pub project_id: Option<String>,
    pub store_root: PathBuf,
    pub graph_db_path: PathBuf,
    pub scope_prefix: Option<String>,
    pub outcome: AutomationSchedulerReconcileOutcome,
}

#[derive(Debug, serde::Serialize)]
pub struct ProfileAutomationReconcileReport {
    pub scope: AutomationReconcileScope,
    pub cached_owners: usize,
    pub outcomes: Vec<AutomationSchedulerOwnerReconcileOutcome>,
    pub uncached_projects: UncachedProjectReconcileOutcome,
}

pub type AutomationSchedulerReconcileFuture =
    Pin<Box<dyn Future<Output = AutomationSchedulerReconcileOutcome> + Send + 'static>>;
pub type AutomationSchedulerReconciler =
    Arc<dyn Fn() -> AutomationSchedulerReconcileFuture + Send + Sync + 'static>;
pub type DashboardAutomationObservationRecorderV1 = Arc<
    dyn Fn(tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord)
        + Send
        + Sync
        + 'static,
>;
pub type DashboardAutomationObservationFuture = Pin<
    Box<
        dyn Future<Output = std::result::Result<DashboardAutomationObservationRecorderV1, String>>
            + Send
            + 'static,
    >,
>;
pub type DashboardAutomationObservationPortV1 =
    Arc<dyn Fn(PathBuf) -> DashboardAutomationObservationFuture + Send + Sync + 'static>;
pub type DoctorReportReadFuture = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    AdmittedDoctorReportV1,
                    tracedecay_application::ApplicationContractError,
                >,
            > + Send
            + 'static,
    >,
>;
pub type DoctorReportReader = Arc<dyn Fn() -> DoctorReportReadFuture + Send + Sync + 'static>;
pub type RemoteOperationalStatusReader = Arc<
    dyn Fn() -> tracedecay_application::remote::status::RemoteOperationalStatusReadV1
        + Send
        + Sync
        + 'static,
>;

/// Runtime authorities retained by one daemon-managed dashboard state.
///
/// This is deliberately not `Default`: the composition root must explicitly
/// choose every optional authority and the automation writer for each state.
#[derive(Clone)]
pub struct DashboardStateCompositionV1 {
    pub project_graph_resolver: Option<crate::project_graph::RetainedProjectGraphResolver>,
    /// Exact-project admission for generation-pinned code-graph reads. This
    /// stays separate from the projection port so the HTTP boundary must
    /// present real request identity, deadline, and live cancellation on each
    /// read instead of retaining an already-open reader.
    pub code_graph_read_admission: Option<Arc<dyn crate::graph::CodeGraphReadAdmissionPort>>,
    /// Daemon-owned resolver of the latest complete verified projection for
    /// this exact project. Standalone dashboards leave it absent and graph
    /// structure routes report typed unavailable.
    pub code_graph_projection_read_port: Option<Arc<dyn crate::graph::CodeGraphProjectionReadPort>>,
    pub registered_project_session_db: Option<RegisteredGlobalDbLeaseV1>,
    pub lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    /// Daemon-owned typed read over the verified session-git-evidence graph
    /// projection. Loom's git sources report unavailable without it.
    pub git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
    /// Daemon-owned exact-project Delivery projection. The adapter owns
    /// application admission and provider/store access; HTTP receives only
    /// bounded typed source outcomes.
    pub delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    pub registered_savings_db: Option<RegisteredGlobalDbLeaseV1>,
    /// Exact daemon-selected profile plus its canonical automation run and
    /// managed-skill materialization capabilities. Standalone states leave it
    /// absent and automation mutation routes report typed unavailable.
    pub automation_authority: Option<DashboardAutomationAuthorityV1>,
    pub automation_observation: Option<DashboardAutomationObservationPortV1>,
    pub automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    pub automation_writer: DashboardAutomationWriter,
    pub doctor_report_reader: Option<DoctorReportReader>,
    /// Daemon-owned Remote Brain operational read. Standalone dashboards leave
    /// it absent and `GET /api/remote/status` reports typed unavailable.
    pub remote_operational_status_reader: Option<RemoteOperationalStatusReader>,
    pub code_index_freshness_reader: Option<code_index_freshness_api::CodeIndexFreshnessReader>,
    /// Daemon-owned read over the semantic activation gate and runtime
    /// status for the Explorer semantic source. Standalone dashboards leave
    /// it absent and the source reports typed `unsupported`.
    pub explorer_semantic_reader: Option<ExplorerSemanticReader>,
    pub feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    pub code_diagnostics_broker:
        Option<Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>>,
    pub application_invocation_executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    /// Daemon-owned canonical authority for browser-confirmed SSE delivery.
    /// Standalone dashboards leave this absent and emit no receipt token.
    pub delivery_settlement_authority:
        Option<Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>>,
}

#[derive(Clone)]
pub struct AdmittedDoctorReportV1 {
    pub report: tracedecay_application::doctor::DoctorReportV1,
    pub table_growth_evidence: Vec<tracedecay_application::storage::TableGrowthDoctorEvidenceV1>,
}

impl AdmittedDoctorReportV1 {
    pub fn new(report: tracedecay_application::doctor::DoctorReportV1) -> Self {
        Self {
            report,
            table_growth_evidence: Vec::new(),
        }
    }

    pub fn with_table_growth_evidence(
        mut self,
        evidence: Vec<tracedecay_application::storage::TableGrowthDoctorEvidenceV1>,
    ) -> Self {
        self.table_growth_evidence = evidence;
        self
    }
}

/// Intentionally has no `Default` or defaulting builder. Every state variant
/// must explicitly provide its mode-specific database and settings authorities;
/// silently defaulting either could route dashboard operations to the wrong
/// project or user profile.
#[derive(Clone)]
pub struct DashboardState {
    /// Registered project id for profile-backed stores, when known.
    pub project_id: Option<String>,
    /// Exact application scope resolved ONCE when this state was constructed.
    /// `None` is the explicit fail-closed state (missing registry, invalid
    /// project id, or unresolvable exact root): handlers report their typed
    /// unavailable states from it and never re-resolve scope from paths or
    /// the CWD per request.
    pub resolved_scope: Option<tracedecay_application::ResolvedScope>,
    /// Canonical per-request admission for the verified code graph.
    pub code_graph_read_admission: Option<Arc<dyn crate::graph::CodeGraphReadAdmissionPort>>,
    /// Canonical exact-project verified projection resolver.
    pub code_graph_projection_read_port: Option<Arc<dyn crate::graph::CodeGraphProjectionReadPort>>,
    /// Exact project graph retained by the daemon for this dashboard state.
    /// Absent for lightweight/profile-only states that cannot run project
    /// automation.
    pub project_graph: Option<Arc<TraceDecay>>,
    /// Resolves other registered projects only when their graph is already
    /// mounted by the daemon.
    pub project_graph_resolver: Option<crate::project_graph::RetainedProjectGraphResolver>,
    /// Immutable authoritative owner for every memory operation served here.
    pub memory_owner: FactOwnerV1,
    /// Active code-graph database. This can be branch-specific.
    pub graph_conn: DatabaseEngineReadConnection,
    /// Keeps every project-database authority alive as long as guarded
    /// capabilities remain reachable through this state.
    pub _database_guards: Vec<Arc<Database>>,
    /// Read-only telemetry handle attached to the retained active graph runtime.
    /// This remains distinct from project memory when those stores use
    /// different files.
    pub graph_telemetry_handle: Option<DatabaseStorageTelemetryHandle>,
    /// Display path of the active code-graph database.
    pub graph_db_path: String,
    /// Authoritative project-memory handle and process-local writer lane.
    pub mem_db: Arc<Database>,
    /// Display path of the project memory database.
    pub mem_db_path: String,
    /// Registered LCM session store retained for legacy analytics and
    /// accounting routes that have not yet moved to application read models.
    pub lcm_db: Option<RegisteredGlobalDbLeaseV1>,
    /// Display path of the retained legacy session store.
    pub lcm_db_path: String,
    /// Storage scope of the retained legacy session store.
    pub lcm_scope: String,
    /// Daemon-owned canonical session retrieval authority used by LCM browse
    /// routes. Those routes never retain or open a session database.
    pub lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    /// Daemon-owned typed read over the verified session-git-evidence graph
    /// projection, serving Loom's session↔commit and branch/worktree sources.
    pub git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
    /// Daemon-owned exact-project Delivery projection.
    pub delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    /// Global accounting DB for the savings ledger and lifetime counters used
    /// by the Savings & Cost tab. Provider usage lives in the retained project
    /// session store exposed separately through `lcm_db`.
    pub savings_db: Option<RegisteredGlobalDbLeaseV1>,
    /// Display path of the global accounting DB.
    pub savings_db_path: String,
    pub project_root: PathBuf,
    /// Live read port over the daemon-owned code-index scheduler registry.
    pub code_index_freshness_reader: Option<code_index_freshness_api::CodeIndexFreshnessReader>,
    /// Root-addressed read over the daemon-owned semantic activation gate and
    /// runtime status. Absent for standalone dashboards, whose Explorer
    /// semantic source reports typed `unsupported` instead of guessing from
    /// process-local state.
    pub explorer_semantic_reader: Option<ExplorerSemanticReader>,
    /// Root-addressed read over the daemon-mounted canonical feedback
    /// observation owner. Selected projects reuse the resolver but resolve
    /// their own exact project root on every call.
    pub feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    /// Storage mode resolved for the active project store.
    pub storage_mode: String,
    /// Resolved active project store root.
    pub store_root: PathBuf,
    /// Resolved `config.json` path for the active project store.
    pub config_path: PathBuf,
    /// Resolved dashboard sidecar root inside the active project store.
    pub dashboard_root: PathBuf,
    /// Retention policy resolved with the owning runtime configuration.
    /// Dashboard reads must not re-open mutable config input per request.
    pub retention_config: crate::config::RetentionConfig,
    /// Daemon-owned user-profile settings authority. Dashboard routes never
    /// load or mutate `config.toml` directly.
    pub user_settings: Arc<dyn crate::application::configuration::UserSettingsDaemonClient>,
    /// Process-local derived BPE token-count cache for the Savings & Cost tab.
    pub token_counts: Arc<token_count::TokenCountCache>,
    /// Admitted daemon/application diagnostics authority. `None` keeps all
    /// diagnostics controls typed unavailable; the dashboard never constructs
    /// a broker or analyzer runtime.
    pub code_diagnostics_authority:
        Option<crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1>,
    /// Daemon-selected profile and canonical automation mutation authority.
    /// HTTP handlers never reconstruct this capability from the environment.
    pub automation_authority: Option<DashboardAutomationAuthorityV1>,
    pub automation_observation: Option<DashboardAutomationObservationPortV1>,
    pub automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    /// Lifetime-owning capability for complete dashboard automation writes.
    pub automation_writer: DashboardAutomationWriter,
    /// Admitted canonical Doctor report source. Absent when the dashboard was
    /// not opened by an owner holding an exact application request context.
    pub doctor_report_reader: Option<DoctorReportReader>,
    /// Admitted Remote Brain operational read. Absent for standalone
    /// dashboards that were not opened by a daemon holding the live provider.
    pub remote_operational_status_reader: Option<RemoteOperationalStatusReader>,
    /// Active-project daemon application transport. Mutating dashboard routes
    /// use this catalog-bound executor instead of opening stores or applying
    /// configuration inside HTTP adapters.
    pub application_invocation_executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    pub(crate) delivery_settlements: Arc<events_delivery::DashboardDeliverySettlementRegistryV1>,
}

/// Test-only lifetime owner for the same registered authorities retained by a
/// daemon dashboard. Integration tests pass the typed host-admission runtime;
/// raw database handles never cross the public test seam.
#[cfg(feature = "test-transport")]
pub struct DashboardHostAdmissionTestAuthorityV1 {
    _runtime: Arc<dyn Send + Sync>,
    project_sessions: RegisteredGlobalDbLeaseV1,
    profile_database: RegisteredGlobalDbLeaseV1,
    automation_authority: Option<DashboardAutomationAuthorityV1>,
    automation_writer: Option<DashboardAutomationWriter>,
    lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    code_graph_read_admission: Option<Arc<dyn crate::graph::CodeGraphReadAdmissionPort>>,
    code_graph_projection_read_port: Option<Arc<dyn crate::graph::CodeGraphProjectionReadPort>>,
    git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
    delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    application_invocation_executor: Option<Arc<dyn DashboardApplicationRuntime>>,
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
            git_correlation_read_authority: None,
            delivery_read_authority: None,
            application_invocation_executor: None,
        }
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

    /// Attaches the daemon-owned Delivery read adapter used by the production
    /// route. Tests retain the same request-control and admission boundary.
    #[must_use]
    pub fn with_delivery_read_authority(
        mut self,
        delivery_read_authority: Arc<dyn DashboardDeliveryReadPortV1>,
    ) -> Self {
        self.delivery_read_authority = Some(delivery_read_authority);
        self
    }
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
#[derive(Clone, Default)]
pub struct DashboardTestProjectGraphsV1 {
    graphs: Arc<std::sync::RwLock<std::collections::HashMap<PathBuf, Arc<TraceDecay>>>>,
}

#[cfg(feature = "test-transport")]
impl DashboardTestProjectGraphsV1 {
    pub fn register(&self, graph: Arc<TraceDecay>) {
        self.graphs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(graph.project_root().to_path_buf(), graph);
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

impl DashboardState {
    pub fn reconcile_automation_scheduler(&self) {
        if let Some(reconcile) = &self.automation_scheduler_reconciler {
            let reconcile = Arc::clone(reconcile);
            tokio::spawn(async move {
                let _ = reconcile().await;
            });
        }
    }

    fn retain_admitted_authorities(
        &mut self,
        code_diagnostics_authority: Option<
            crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1,
        >,
        doctor_report_reader: Option<DoctorReportReader>,
        remote_operational_status_reader: Option<RemoteOperationalStatusReader>,
    ) {
        self.code_diagnostics_authority = code_diagnostics_authority;
        self.doctor_report_reader = doctor_report_reader;
        self.remote_operational_status_reader = remote_operational_status_reader;
    }
}

/// The retained session store for legacy dashboard routes.
pub struct LcmStoreSelection {
    pub lcm_db: Option<RegisteredGlobalDbLeaseV1>,
    pub path: String,
    pub scope: String,
}

pub async fn resolve_lcm_store(
    cg: &TraceDecay,
    registered_project_session_db: Option<RegisteredGlobalDbLeaseV1>,
) -> LcmStoreSelection {
    resolve_lcm_store_for_layout(cg.store_layout(), registered_project_session_db)
}

fn resolve_lcm_store_for_layout(
    layout: &StoreLayout,
    registered_project_session_db: Option<RegisteredGlobalDbLeaseV1>,
) -> LcmStoreSelection {
    if let Some(db) = registered_project_session_db {
        return LcmStoreSelection {
            path: db.db_path().display().to_string(),
            lcm_db: Some(db),
            scope: storage_mode_label(&layout.storage_mode).to_string(),
        };
    }
    LcmStoreSelection {
        lcm_db: None,
        path: layout.sessions_db_path.display().to_string(),
        scope: "unavailable".to_string(),
    }
}

pub fn storage_mode_label(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProjectLocal => "project_local",
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

pub fn resolve_project_memory_store(cg: &TraceDecay) -> (String, Arc<Database>) {
    (
        cg.dashboard_db_path().display().to_string(),
        cg.dashboard_database_guard(),
    )
}

/// Resolves the immutable fact owner from the validated project layout.
///
/// Dashboard routes must never infer ownership from a path, label, or
/// optional display field after construction.
pub fn project_memory_owner(cg: &TraceDecay) -> Result<FactOwnerV1> {
    project_memory_owner_for_layout(cg.store_layout())
}

fn project_memory_owner_for_layout(layout: &StoreLayout) -> Result<FactOwnerV1> {
    let raw = layout
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| config_error("project dashboard has no registered project id"))?;
    let project_id = ProjectId::new(raw).map_err(|error| {
        config_error(format!("project dashboard has invalid project id: {error}"))
    })?;
    Ok(FactOwnerV1::Project { project_id })
}

async fn build_state_inner(
    cg: &TraceDecay,
    project_graph: Option<Arc<TraceDecay>>,
    warm_token_counts: bool,
    composition: DashboardStateCompositionV1,
) -> Result<DashboardState> {
    let DashboardStateCompositionV1 {
        project_graph_resolver,
        code_graph_read_admission,
        code_graph_projection_read_port,
        registered_project_session_db,
        lcm_read_authority,
        git_correlation_read_authority,
        delivery_read_authority,
        registered_savings_db,
        automation_authority,
        automation_observation,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader,
        remote_operational_status_reader,
        code_index_freshness_reader,
        explorer_semantic_reader,
        feedback_status_reader,
        code_diagnostics_broker,
        application_invocation_executor,
        delivery_settlement_authority,
    } = composition;
    let (mem_db_path, mem_db) = resolve_project_memory_store(cg);
    let memory_owner = project_memory_owner(cg)?;
    let lcm = resolve_lcm_store(cg, registered_project_session_db).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let store_root = cg.store_layout().data_root.clone();
    let config_path = cg.store_layout().config_path.clone();
    let storage_mode = storage_mode_label(&cg.store_layout().storage_mode).to_string();
    let code_diagnostics_authority = match (
        code_diagnostics_broker,
        code_graph_read_admission.as_ref(),
        code_graph_projection_read_port.as_ref(),
    ) {
        (Some(broker), Some(graph_admission), Some(graph_projection)) => Some(
            crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1::new(
                cg.project_root().to_path_buf(),
                dashboard_root.clone(),
                Arc::clone(graph_admission),
                Arc::clone(graph_projection),
                broker,
            ),
        ),
        _ => None,
    };
    let savings_db_path = registered_savings_db
        .as_ref()
        .map(|db| db.db_path().display().to_string())
        .or_else(|| tracedecay_global_db::global_db_path().map(|path| path.display().to_string()))
        .unwrap_or_default();
    let delivery_settlements = Arc::new(
        events_delivery::DashboardDeliverySettlementRegistryV1::new(delivery_settlement_authority),
    );
    events_delivery::DashboardDeliverySettlementRegistryV1::mount_deadline_reaper(
        &delivery_settlements,
    );
    let mut state = DashboardState {
        project_id: cg.store_layout().identity.project_id.clone(),
        resolved_scope: scope::resolve_dashboard_scope(
            cg.project_root(),
            cg.store_layout().identity.project_id.as_deref(),
        ),
        code_graph_read_admission,
        code_graph_projection_read_port,
        project_graph,
        project_graph_resolver,
        memory_owner,
        graph_conn: mem_db.read_connection(),
        _database_guards: vec![mem_db.clone()],
        graph_telemetry_handle: cg
            .dashboard_database_guard()
            .storage_telemetry_handle()
            .ok(),
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_db,
        mem_db_path,
        lcm_db: lcm.lcm_db,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        lcm_read_authority,
        git_correlation_read_authority,
        delivery_read_authority,
        savings_db: registered_savings_db,
        savings_db_path,
        project_root: cg.project_root().to_path_buf(),
        code_index_freshness_reader,
        explorer_semantic_reader,
        feedback_status_reader,
        storage_mode,
        store_root,
        config_path,
        dashboard_root,
        retention_config: cg.retention_config(),
        user_settings: cg.user_settings_client(),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics_authority: None,
        automation_authority,
        automation_observation,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader: None,
        remote_operational_status_reader: None,
        application_invocation_executor,
        delivery_settlements,
    };
    state.retain_admitted_authorities(
        code_diagnostics_authority,
        doctor_report_reader,
        remote_operational_status_reader,
    );
    // Pre-count non-usage messages in the background so the first Savings
    // tab paint doesn't pay the initial BPE pass over the session store.
    if warm_token_counts {
        token_count::spawn_warm(state.clone());
    }
    Ok(state)
}

pub async fn build_state_with_automation_reconciler(
    cg: Arc<TraceDecay>,
    composition: DashboardStateCompositionV1,
) -> Result<DashboardState> {
    build_state_inner(cg.as_ref(), Some(Arc::clone(&cg)), true, composition).await
}

/// Builds a lightweight cached state for a non-active project selected from the
/// dashboard project picker. Automation authority is inherited from the active
/// dashboard state so daemon-selected projects cannot fall back to direct open.
pub async fn build_selected_project_state(
    cg: Arc<TraceDecay>,
    active: &DashboardState,
) -> Result<DashboardState> {
    build_state_inner(
        cg.as_ref(),
        Some(Arc::clone(&cg)),
        false,
        DashboardStateCompositionV1 {
            project_graph_resolver: active.project_graph_resolver.clone(),
            // Both verified code-graph ports are exact-project authorities;
            // the active project's admission must never be reused for a
            // selected project.
            code_graph_read_admission: None,
            code_graph_projection_read_port: None,
            registered_project_session_db: None,
            lcm_read_authority: None,
            git_correlation_read_authority: None,
            delivery_read_authority: None,
            registered_savings_db: active.savings_db.clone(),
            automation_authority: active.automation_authority.clone(),
            automation_observation: active.automation_observation.clone(),
            automation_scheduler_reconciler: None,
            automation_writer: Arc::clone(&active.automation_writer),
            // Doctor authority is bound to the active project's exact scope.
            // Freshness is different: its daemon registry reader resolves the
            // selected state's exact canonical root and returns only a mounted
            // scheduler, so the root-addressed read port is safe to reuse.
            doctor_report_reader: None,
            // Remote Brain operational status is daemon-wide, not bound to the
            // active project's Doctor scope, so the selected project reuses
            // the same admitted reader.
            remote_operational_status_reader: active.remote_operational_status_reader.clone(),
            code_index_freshness_reader: active.code_index_freshness_reader.clone(),
            // Like freshness, the semantic reader is root-addressed and
            // resolves the selected state's exact root on every call.
            explorer_semantic_reader: active.explorer_semantic_reader.clone(),
            feedback_status_reader: active.feedback_status_reader.clone(),
            code_diagnostics_broker: None,
            application_invocation_executor: active.application_invocation_executor.clone(),
            delivery_settlement_authority: None,
        },
    )
    .await
}

pub fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
pub async fn run_until_shutdown_for_tests_with_host_admission<F>(
    cg: Arc<TraceDecay>,
    authority: DashboardHostAdmissionTestAuthorityV1,
    project_graphs: DashboardTestProjectGraphsV1,
    host: &str,
    port: u16,
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
            host,
            port,
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
    spa_routes: Router,
    test_authority: Option<&'a DashboardHostAdmissionTestAuthorityV1>,
    test_project_graph_resolver: Option<crate::project_graph::RetainedProjectGraphResolver>,
    test_project_graph: Option<Arc<TraceDecay>>,
}

#[cfg(feature = "test-transport")]
async fn run_until_shutdown_inner<F>(
    cg: &TraceDecay,
    request: DashboardRunRequest<'_>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let DashboardRunRequest {
        host,
        port,
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
            cg.project_root().to_path_buf(),
            &cg.store_layout().dashboard_root,
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
            project_graph_resolver: test_project_graph_resolver,
            code_graph_read_admission: test_authority
                .and_then(|authority| authority.code_graph_read_admission.clone()),
            code_graph_projection_read_port: test_authority
                .and_then(|authority| authority.code_graph_projection_read_port.clone()),
            registered_project_session_db: test_authority
                .map(|authority| authority.project_sessions.clone()),
            lcm_read_authority: test_authority
                .and_then(|authority| authority.lcm_read_authority.clone()),
            git_correlation_read_authority: test_authority
                .and_then(|authority| authority.git_correlation_read_authority.clone()),
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
            code_index_freshness_reader: None,
            explorer_semantic_reader: None,
            feedback_status_reader: None,
            code_diagnostics_broker: Some(code_diagnostics_broker),
            application_invocation_executor: test_authority
                .and_then(|authority| authority.application_invocation_executor.clone()),
            delivery_settlement_authority: None,
        },
    )
    .await?;
    let app = router(cg, state, spa_routes).await?;
    let (listener, addr) = bind_dashboard(host, port).await?;
    let app = with_dashboard_http_admission(app, addr);

    let url = format!("http://{addr}/");
    // Stable, parseable line for wrappers (the Hermes plugin reads this).
    println!("tracedecay dashboard listening on {url}");
    eprintln!("Serving project {}", cg.project_root().display());

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| config_error(format!("dashboard server error: {e}")))
}

/// Shared bind logic for the MCP `tracedecay_dashboard` tool and the test
/// harness (so port 0 allocation and URL formatting are consistent).
pub async fn bind_dashboard(
    host: &str,
    port: u16,
) -> Result<(tokio::net::TcpListener, std::net::SocketAddr)> {
    let host = validate_dashboard_host(host)?;
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .map_err(|e| config_error(format!("failed to bind {host}:{port}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| config_error(format!("failed to read local address: {e}")))?;
    Ok((listener, addr))
}

pub fn validate_dashboard_host(host: &str) -> Result<&str> {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1") {
        return Ok(host);
    }

    Err(config_error(format!(
        "dashboard host is loopback-only; use 127.0.0.1, localhost, or ::1 (got {host:?})"
    )))
}

#[derive(Clone)]
struct DashboardHttpAdmission {
    port: u16,
}

const DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS: i64 = 30_000_000;
const DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS: i64 = 300_000_000;
static DASHBOARD_HTTP_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Transport-owned controls attached to one admitted dashboard HTTP request.
/// Graph adapters pass these values intact to canonical application admission;
/// they never manufacture an actor, grant, scope, or projection generation.
#[derive(Clone, Debug)]
pub struct DashboardHttpRequestControlV1 {
    request_id: tracedecay_application::RequestId,
    deadline: tracedecay_application::Deadline,
    cancellation: tracedecay_application::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
}

impl DashboardHttpRequestControlV1 {
    pub fn request_id(&self) -> tracedecay_application::RequestId {
        self.request_id.clone()
    }

    pub fn deadline(&self) -> tracedecay_application::Deadline {
        self.deadline.clone()
    }

    pub fn cancellation(&self) -> &tracedecay_application::CancellationSignal {
        &self.cancellation
    }

    pub const fn observed_at(&self) -> tracedecay_domain::UtcMicros {
        self.observed_at
    }
}

struct DashboardHttpCancellationGuard {
    cancellation: tracedecay_application::CancellationSignal,
    completed: bool,
}

impl Drop for DashboardHttpCancellationGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self
                .cancellation
                .cancel(crate::application::context::application_observed_at());
        }
    }
}

pub fn with_dashboard_http_admission(app: Router, addr: std::net::SocketAddr) -> Router {
    app.layer(middleware::from_fn_with_state(
        DashboardHttpAdmission { port: addr.port() },
        admit_dashboard_http_request,
    ))
}

async fn admit_dashboard_http_request(
    State(admission): State<DashboardHttpAdmission>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return dashboard_request_forbidden("missing Host header");
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return dashboard_request_forbidden("invalid Host header");
    };
    let authority_host = authority.host().trim_matches(['[', ']']);
    let loopback_host = authority_host.eq_ignore_ascii_case("localhost")
        || matches!(authority_host, "127.0.0.1" | "::1");
    if !loopback_host || authority.port_u16() != Some(admission.port) {
        return dashboard_request_forbidden("Host must name the bound loopback dashboard");
    }

    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let Ok(origin_uri) = origin.parse::<Uri>() else {
            return dashboard_request_forbidden("invalid Origin header");
        };
        let same_origin = origin_uri.scheme_str() == Some("http")
            && origin_uri.authority().is_some_and(|origin_authority| {
                origin_authority
                    .as_str()
                    .eq_ignore_ascii_case(authority.as_str())
            });
        if !same_origin {
            return dashboard_request_forbidden("Origin must match the dashboard");
        }
    }

    let observed_at = crate::application::context::application_observed_at();
    let sequence = DASHBOARD_HTTP_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let identity = format!("dashboard.http.{}.{}", observed_at.0, sequence);
    let request_id = match tracedecay_application::RequestId::new(format!("request.{identity}")) {
        Ok(request_id) => request_id,
        Err(error) => return internal_error_response(error),
    };
    let cancellation =
        match tracedecay_application::CancellationSignal::active(format!("cancel.{identity}")) {
            Ok(cancellation) => cancellation,
            Err(error) => return internal_error_response(error),
        };
    let request_deadline_micros = dashboard_http_request_deadline_micros(request.uri().path());
    let deadline_at =
        tracedecay_domain::UtcMicros(observed_at.0.saturating_add(request_deadline_micros));
    let deadline = match tracedecay_application::Deadline::new(deadline_at) {
        Ok(deadline) => deadline,
        Err(error) => return internal_error_response(error),
    };
    request
        .extensions_mut()
        .insert(DashboardHttpRequestControlV1 {
            request_id,
            deadline,
            cancellation: cancellation.clone(),
            observed_at,
        });
    let mut cancellation_guard = DashboardHttpCancellationGuard {
        cancellation,
        completed: false,
    };
    let response = next.run(request).await;
    cancellation_guard.completed = true;
    response
}

fn internal_error_response(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "dashboard_request_admission_failed",
            "detail": error.to_string(),
        })),
    )
        .into_response()
}

fn dashboard_request_forbidden(detail: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "dashboard_request_forbidden",
            "detail": detail,
        })),
    )
        .into_response()
}

/// Canonical application routes bound to one exact project daemon.
///
/// The active project mounts every route below. The selected-project gateway
/// constructs only the dashboard feedback read subset from its retained graph.
struct ActiveProjectApplicationRoutes {
    http_router: Router,
    dashboard_configuration_router: Router,
    dashboard_feedback_router: Router,
    dashboard_work_router: Router,
    executor: Option<Arc<dyn DashboardApplicationRuntime>>,
}

impl ActiveProjectApplicationRoutes {
    fn for_active_project(
        cg: &TraceDecay,
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
/// `spa_routes` carries the embedded single-page-app surface — the app index,
/// `/static/{*tail}`, and the SPA fallback for unmatched non-API client routes
/// (`/brain?scope=…` deep links). It is built by the owning binary because the
/// bundle is generated into `OUT_DIR` by that crate's `build.rs`. It must be a
/// stateless `axum::Router` (it is merged after `.with_state(…)`), it must set
/// its own `.fallback(…)`, and it must not define any `/api/**` route — axum
/// panics on overlapping paths. Pass `Router::new()` to serve the JSON API
/// with no UI.
pub async fn router(
    cg: &TraceDecay,
    mut state: DashboardState,
    spa_routes: Router,
) -> Result<Router> {
    // application primitive routes are bound to the active-project daemon. When the
    // daemon authority record is unavailable (standalone `tracedecay dashboard`
    // or the in-process test server), mounting them would otherwise fail the
    // whole server before it binds. Degrade gracefully instead — serve the core
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

fn router_with_active_application(
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
        .route("/api/feedback/status", get(feedback_api::status))
        // Holographic memory plugin API (mirrors holographic_plus plugin_api.py)
        .route("/api/plugins/holographic/", get(memory_api::overview))
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
        .route("/api/plugins/savings/ledger", get(savings_api::ledger))
        .route("/api/plugins/savings/sessions", get(savings_api::sessions))
        .route("/api/plugins/savings/models", get(savings_api::models))
        .route("/api/plugins/savings/pricing", get(savings_api::pricing))
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
        // family, plan-38 storage telemetry/findings, code-index freshness, and
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
            tracedecay_api::doctor::STORAGE_FINDINGS_ROUTE_PATH,
            get(storage_findings_api::findings),
        )
        .route(
            "/api/code-index/freshness",
            get(code_index_freshness_api::freshness),
        )
        .route("/api/remote/status", get(remote_status_api::status))
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
    let application_read = selected_project_application_read(req.method(), &tail);
    if runtime.active_project_id() != Some(project_id.as_str())
        && !matches!(req.method(), &Method::GET | &Method::HEAD)
        && application_read.is_none()
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
        Err(err) if projects::is_registry_unavailable_error(&err) => {
            return projects::registry_unavailable_response(&runtime.active_state(), &err)
                .into_response();
        }
        Err(err) => {
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
        let application = match ActiveProjectApplicationRoutes::for_active_project(
            project_graph,
            selected.state.application_invocation_executor.clone(),
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

/// A canonical application read a selected project answers for itself.
///
/// These are POSTs, so the gateway's read-only rule for non-active projects
/// would otherwise refuse them. They are admitted because they are served from
/// the selected project's own graph: the answer belongs to the project the
/// caller named, and no other project's data can appear under its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedProjectApplicationRead {
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

fn selected_project_application_read(
    method: &Method,
    tail: &str,
) -> Option<SelectedProjectApplicationRead> {
    if method != Method::POST {
        return None;
    }
    match tail {
        "feedback/get" | "feedback/expand" | "feedback/list" => {
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

async fn forward_project_request(
    project_api: Router<DashboardState>,
    state: DashboardState,
    req: Request<Body>,
) -> Response {
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
}

/// Capability discovery for hosts and future delegated-host extensions. The UI
/// (or a wrapper) can probe this to decide which panels/actions to enable.
async fn capabilities(State(state): State<DashboardState>) -> Json<Value> {
    let has_lcm = state.lcm_read_authority.is_some();
    let automation = automation_config_api::effective_automation_config(&state);
    let (automation_configured, automation_mode, automation_payload) = match automation {
        Ok((configuration_revision_id, config)) => {
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
                    "availability": backend::backend_availability(&config),
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
    // stores. Report the transport the UI would actually have to use rather
    // than a fixed string: without an admitted application executor there is
    // no way to reach a scope set at all.
    let multi_root = if state.application_invocation_executor.is_some() {
        tracedecay_api::read_model::multi_root::MultiRootCapabilityV1::unavailable(
            "no multi-root scope set is mounted for this project",
        )
    } else {
        tracedecay_api::read_model::multi_root::MultiRootCapabilityV1::unavailable(
            "the daemon application transport is not admitted for this dashboard",
        )
    };
    Json(json!({
        "name": "tracedecay-dashboard",
        "version": crate::version::build_version(),
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
            "multi_root": false,
        },
        "automation": automation_payload,
        "dashboards": ["tracedecay"],
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod authority_tests {
    use super::*;

    #[test]
    fn automation_run_routes_receive_the_backend_sized_deadline_budget() {
        assert_eq!(
            dashboard_http_request_deadline_micros("/api/application/retained/fact_store_curate"),
            DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
        );
        assert_eq!(
            dashboard_http_request_deadline_micros("/api/automation/jobs/nightly-review/run"),
            DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
        );
        assert_eq!(
            dashboard_http_request_deadline_micros(
                "/api/projects/project-7/application/retained/fact_store_curate"
            ),
            DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
        );
        assert_eq!(
            dashboard_http_request_deadline_micros("/api/automation/runs"),
            DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
        );
        assert_eq!(
            dashboard_http_request_deadline_micros("/api/plugins/holographic/status"),
            DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
        );
        for near_match in [
            "/api/application/retained/fact_store_curate/extra",
            "/api/application/retained/fact_store_curateish",
            "/api/projects//application/retained/fact_store_curate",
            "/api/projects/project-7/application/retained/fact_store_curate/extra",
            "/api/automation/run/session-reflection",
            "/api/automation/run/skill-writing",
            "/api/projects/project-7/automation/run/session-reflection",
            "/api/projects/project-7/automation/run/skill-writing",
            "/api/projects/project-7/automation/run/skill-writer",
            "/api/projects/project-7/automation/runs",
        ] {
            assert_eq!(
                dashboard_http_request_deadline_micros(near_match),
                DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
                "near-match route must retain the ordinary deadline: {near_match}",
            );
        }
    }

    #[tokio::test]
    async fn admitted_automation_route_carries_the_backend_sized_deadline() {
        async fn deadline_budget(
            axum::Extension(control): axum::Extension<DashboardHttpRequestControlV1>,
        ) -> Json<Value> {
            Json(json!({
                "budget_micros": control.deadline().expires_at.0 - control.observed_at().0,
            }))
        }

        let port = 47_123;
        let app = with_dashboard_http_admission(
            Router::new()
                .route(
                    "/api/application/retained/fact_store_curate",
                    post(deadline_budget),
                )
                .route("/api/automation/runs", get(deadline_budget)),
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        );
        for (method, path, expected) in [
            (
                Method::POST,
                "/api/application/retained/fact_store_curate",
                DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
            ),
            (
                Method::GET,
                "/api/automation/runs",
                DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(header::HOST, format!("127.0.0.1:{port}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("admitted request");
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("response body");
            let payload: Value = serde_json::from_slice(&body).expect("deadline response");
            assert_eq!(payload["budget_micros"], json!(expected));
        }
    }

    mod automatic_fact_receipts_routes;

    /// The loopback authority every admitted-router fixture binds. Requests
    /// must carry it as their `Host` header for the admission layer to admit
    /// them.
    const TEST_DASHBOARD_AUTHORITY: &str = "127.0.0.1:43127";

    /// A GET that the dashboard HTTP admission layer admits.
    fn admitted_request(uri: impl AsRef<str>) -> Request<Body> {
        Request::builder()
            .uri(uri.as_ref())
            .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
            .body(Body::empty())
            .expect("admitted dashboard request")
    }

    /// The router as production serves it.
    ///
    /// [`run_until_shutdown_inner`] wraps every served route in
    /// [`with_dashboard_http_admission`], which is what supplies the
    /// [`DashboardHttpRequestControlV1`] extension every canonical read
    /// requires. A bare [`router_with_active_application`] is not a stack that
    /// exists in production: its handlers can only fail closed on the missing
    /// control, so route behaviour must be asserted through this one.
    fn admitted_router(state: DashboardState) -> Router {
        with_dashboard_http_admission(
            router_with_active_application(state, None, Router::new()),
            TEST_DASHBOARD_AUTHORITY
                .parse()
                .expect("loopback dashboard authority"),
        )
    }

    struct UnavailableCodeGraphPort;

    impl crate::graph::CodeGraphReadAdmissionPort for UnavailableCodeGraphPort {
        fn admit<'a>(
            &'a self,
            _request: crate::graph::CodeGraphReadAdmissionRequest<'a>,
        ) -> crate::graph::CodeGraphReadAdmissionFuture<'a> {
            Box::pin(async { Err(crate::graph::CodeGraphReadError::MissingRegistry) })
        }
    }

    impl crate::graph::CodeGraphProjectionReadPort for UnavailableCodeGraphPort {
        fn open<'a>(
            &'a self,
            _request: crate::graph::CodeGraphReadRequest<'a>,
        ) -> crate::graph::CodeGraphReadFuture<'a> {
            Box::pin(async { Err(crate::graph::CodeGraphReadError::MissingRegistry) })
        }
    }

    struct FakeDashboardLcmRead;

    fn dashboard_lcm_test_control() -> DashboardHttpRequestControlV1 {
        DashboardHttpRequestControlV1 {
            request_id: tracedecay_application::RequestId::new("request.dashboard-lcm-test")
                .expect("dashboard LCM test request"),
            deadline: tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(i64::MAX))
                .expect("dashboard LCM test deadline"),
            cancellation: tracedecay_application::CancellationSignal::active(
                "cancel.dashboard-lcm-test",
            )
            .expect("dashboard LCM test cancellation"),
            observed_at: tracedecay_domain::UtcMicros(1),
        }
    }

    impl DashboardLcmReadPortV1 for FakeDashboardLcmRead {
        fn read(
            &self,
            _control: DashboardHttpRequestControlV1,
            _project_id: Option<&str>,
            request: DashboardLcmReadRequestV1,
        ) -> DashboardLcmReadFutureV1<'_> {
            Box::pin(async move {
                let next_cursor = match request {
                    DashboardLcmReadRequestV1::Overview { .. }
                    | DashboardLcmReadRequestV1::Timeline { .. } => None,
                    DashboardLcmReadRequestV1::Search { .. } => {
                        Some("opaque-search-cursor".to_owned())
                    }
                    DashboardLcmReadRequestV1::Session { .. } => {
                        Some("opaque-session-cursor".to_owned())
                    }
                };
                DashboardLcmReadOutcomeV1::Ready(DashboardLcmCanonicalPageV1 {
                    messages: vec![DashboardLcmCanonicalMessageV1 {
                        session_id: "session.dashboard".to_owned(),
                        provider: "claude".to_owned(),
                        role: "assistant".to_owned(),
                        timestamp: Some(1),
                        ordinal: 1,
                        content: "canonically hydrated".to_owned(),
                        message_id: "message.dashboard".to_owned(),
                        metadata_json: None,
                        tool_names: None,
                    }],
                    summary_nodes: Vec::new(),
                    overview_matches: None,
                    stats: DashboardLcmCanonicalStatsV1 {
                        message_count: 1,
                        ..DashboardLcmCanonicalStatsV1::default()
                    },
                    has_more: true,
                    next_cursor,
                })
            })
        }
    }

    struct DashboardStateFixture {
        state: DashboardState,
        layout: StoreLayout,
        _database_authority: tracedecay_runtime_core::db::DatabaseAuthority,
        _temporary: tempfile::TempDir,
    }

    impl DashboardStateFixture {
        async fn open(project_id: &str) -> Self {
            let temporary = tempfile::tempdir().expect("dashboard fixture");
            let project_root = temporary.path().join("project");
            let profile_root = temporary.path().join("profile");
            std::fs::create_dir_all(&project_root).expect("project root");
            let git_init = std::process::Command::new("git")
                .current_dir(&project_root)
                .args(["init", "-q"])
                .status()
                .expect("git init starts");
            assert!(git_init.success(), "git init failed for dashboard fixture");
            assert!(
                tracedecay_runtime_core::storage::write_repository_identity_marker(
                    &project_root,
                    project_id,
                )
                .expect("dashboard fixture enrollment marker"),
                "dashboard fixture project root must accept its enrollment marker"
            );
            let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
                &project_root,
                &profile_root,
                &tracedecay_runtime_core::storage::EnrollmentMarker {
                    project_id: project_id.to_owned(),
                    storage_mode: StorageMode::ProfileSharded,
                },
            )
            .expect("dashboard store layout");
            std::fs::create_dir_all(
                layout
                    .graph_db_path
                    .parent()
                    .expect("dashboard database parent"),
            )
            .expect("dashboard database parent");
            crate::register_test_schema_installer();
            let database_authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
                &layout.graph_db_path,
                "dashboard state fixture",
            )
            .expect("dashboard database authority");
            let (database, _) = Database::publish_test_runtime(
                &layout.graph_db_path,
                &database_authority,
                tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
            )
            .await
            .expect("dashboard database");
            let database = Arc::new(database);
            let memory_owner =
                project_memory_owner_for_layout(&layout).expect("dashboard project memory owner");
            let state = DashboardState {
                project_id: layout.identity.project_id.clone(),
                resolved_scope: scope::resolve_dashboard_scope(
                    &project_root,
                    layout.identity.project_id.as_deref(),
                ),
                code_graph_read_admission: None,
                code_graph_projection_read_port: None,
                project_graph: None,
                project_graph_resolver: None,
                memory_owner,
                graph_conn: database.read_connection(),
                _database_guards: vec![Arc::clone(&database)],
                graph_telemetry_handle: database.storage_telemetry_handle().ok(),
                graph_db_path: layout.graph_db_path.display().to_string(),
                mem_db: Arc::clone(&database),
                mem_db_path: layout.graph_db_path.display().to_string(),
                lcm_db: None,
                lcm_db_path: layout.sessions_db_path.display().to_string(),
                lcm_scope: "unavailable".to_owned(),
                lcm_read_authority: None,
                git_correlation_read_authority: None,
                delivery_read_authority: None,
                savings_db: None,
                savings_db_path: String::new(),
                project_root: project_root.clone(),
                code_index_freshness_reader: None,
                explorer_semantic_reader: None,
                feedback_status_reader: None,
                storage_mode: storage_mode_label(&layout.storage_mode).to_owned(),
                store_root: layout.data_root.clone(),
                config_path: layout.config_path.clone(),
                dashboard_root: layout.dashboard_root.clone(),
                retention_config: crate::config::RetentionConfig::default(),
                user_settings: Arc::new(
                    crate::application::configuration::ProductionUserSettingsDaemonClient::default(
                    ),
                ),
                token_counts: Arc::new(token_count::TokenCountCache::new()),
                code_diagnostics_authority: None,
                automation_authority: None,
                automation_observation: None,
                automation_scheduler_reconciler: None,
                automation_writer: standalone_dashboard_automation_writer(),
                doctor_report_reader: None,
                remote_operational_status_reader: None,
                application_invocation_executor: None,
                delivery_settlements: Arc::new(
                    events_delivery::DashboardDeliverySettlementRegistryV1::new(None),
                ),
            };
            Self {
                state,
                layout,
                _database_authority: database_authority,
                _temporary: temporary,
            }
        }
    }

    #[tokio::test]
    async fn dashboard_state_resolves_exact_application_scope_once() {
        let fixture = DashboardStateFixture::open("project.dashboard-resolved-scope").await;
        let project_id = ProjectId::new(
            fixture
                .layout
                .identity
                .project_id
                .clone()
                .expect("registered project id"),
        )
        .expect("valid project id");
        let expected = crate::application::context::RegisteredScopeResolver::resolve(
            &fixture.layout.project_root,
            &fixture.layout.project_root,
            &project_id,
        )
        .expect("application exact-root scope");

        // The exact-root HTTP surface resolves the same project and scope
        // through the application type, once, at state construction.
        let scope = fixture
            .state
            .resolved_scope
            .clone()
            .expect("exact resolved application scope");
        assert_eq!(scope, expected);
        scope.validate().expect("resolved scope validates");
    }

    #[tokio::test]
    async fn project_memory_owner_uses_validated_store_identity() {
        let fixture = DashboardStateFixture::open("project.dashboard-project-memory").await;
        let raw = fixture
            .layout
            .identity
            .project_id
            .as_deref()
            .expect("registered project id");
        let expected = ProjectId::new(raw).expect("validated project id");

        assert_eq!(
            project_memory_owner_for_layout(&fixture.layout).expect("project memory owner"),
            FactOwnerV1::Project {
                project_id: expected,
            }
        );
    }

    #[tokio::test]
    async fn dashboard_state_reuses_its_active_database_as_memory_authority() {
        let fixture = DashboardStateFixture::open("project.dashboard-state").await;
        let expected_path = fixture.layout.graph_db_path.display().to_string();
        let state = fixture.state;
        let Json(capabilities) = capabilities(State(state.clone())).await;

        assert_eq!(state.mem_db_path, expected_path);
        assert_eq!(state._database_guards.len(), 1);
        assert_eq!(capabilities["multi_root"]["status"], "unavailable");
        assert_eq!(
            capabilities["multi_root"]["reason"],
            "the daemon application transport is not admitted for this dashboard"
        );
        assert_eq!(capabilities["features"]["multi_root"], false);
        assert!(
            state.code_diagnostics_authority.is_none(),
            "direct dashboard must not construct an analyzer authority"
        );
    }

    #[tokio::test]
    async fn daemon_dashboard_retains_the_admitted_authorities() {
        let mut fixture = DashboardStateFixture::open("project.daemon-dashboard").await;
        let doctor_reader: DoctorReportReader = Arc::new(|| {
            Box::pin(async {
                Err(
                    tracedecay_application::ApplicationContractError::Inconsistent {
                        field: "dashboard authority test reader",
                    },
                )
            })
        });
        let diagnostic_broker = Arc::new(tokio::sync::Mutex::new(
            crate::application::dashboard_diagnostics::diagnostic_broker(
                fixture.layout.project_root.clone(),
                tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings::default(),
            ),
        ));
        let code_graph = Arc::new(UnavailableCodeGraphPort);
        fixture.state.retain_admitted_authorities(
            Some(
                crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1::new(
                    fixture.layout.project_root.clone(),
                    fixture.layout.dashboard_root.clone(),
                    Arc::clone(&code_graph) as Arc<dyn crate::graph::CodeGraphReadAdmissionPort>,
                    code_graph as Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
                    diagnostic_broker,
                ),
            ),
            Some(Arc::clone(&doctor_reader)),
            None,
        );
        let state = fixture.state;

        assert!(Arc::ptr_eq(
            state
                .doctor_report_reader
                .as_ref()
                .expect("admitted Doctor reader"),
            &doctor_reader,
        ));
        assert!(
            state.code_diagnostics_authority.is_some(),
            "daemon dashboard must retain the admitted diagnostics authority"
        );
    }

    #[tokio::test]
    async fn doctor_routes_expose_diagnostics_without_mutation_endpoints() {
        let fixture = DashboardStateFixture::open("project.dashboard-doctor-read-only").await;
        let app = router_with_active_application(fixture.state, None, Router::new());

        for (method, path) in [
            (Method::POST, "/api/doctor/remediations/preview"),
            (Method::POST, "/api/doctor/remediations/apply"),
            (Method::GET, "/api/doctor/remediations/operation"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .expect("legacy Doctor mutation request"),
                )
                .await
                .expect("legacy Doctor mutation response");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} is not mounted"
            );
        }
    }

    #[tokio::test]
    async fn dashboard_user_job_run_without_automation_authority_fails_closed() {
        let fixture = DashboardStateFixture::open("project.dashboard-job-observation").await;
        let app = admitted_router(fixture.state);
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/automation/jobs")
                    .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "id": "observation-required",
                            "name": "Observation required",
                            "prompt": "Review the current project"
                        })
                        .to_string(),
                    ))
                    .expect("create automation job request"),
            )
            .await
            .expect("create automation job response");
        assert_eq!(create.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/automation/jobs/observation-required/run")
                    .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
                    .body(Body::empty())
                    .expect("run automation job request"),
            )
            .await
            .expect("run automation job response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("automation unavailable body");
        let payload: Value = serde_json::from_slice(&body).expect("automation unavailable json");
        assert_eq!(
            payload["detail"],
            json!("dashboard automation authority is not mounted")
        );
    }

    #[tokio::test]
    async fn retained_project_session_authority_is_reused_exactly() {
        let fixture = DashboardStateFixture::open("project.retained-session").await;
        let profile_root = fixture._temporary.path().join("sessions-profile");
        let project_id =
            ProjectId::new("project.retained-session").expect("valid project identity");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            profile_root,
            &fixture.layout.project_root,
            project_id,
        )
        .await
        .expect("registered project sessions");
        let retained = runtime
            .project_database_arc()
            .expect("project session authority");

        let selected = resolve_lcm_store_for_layout(&fixture.layout, Some(retained.clone()));

        assert!(
            selected
                .lcm_db
                .as_ref()
                .expect("retained LCM authority")
                .shares_client_with(&retained)
        );
        assert_eq!(selected.path, retained.db_path().display().to_string());
        assert_ne!(selected.scope, "global");
    }

    #[tokio::test]
    async fn daemon_dashboard_without_retained_authority_fails_closed() {
        let fixture = DashboardStateFixture::open("project.dashboard-session-unavailable").await;
        let selected = resolve_lcm_store_for_layout(&fixture.layout, None);

        assert!(selected.lcm_db.is_none());
        assert_eq!(selected.scope, "unavailable");
        assert_eq!(
            selected.path,
            fixture.layout.sessions_db_path.display().to_string()
        );
    }

    #[tokio::test]
    async fn daemon_dashboard_without_retained_authority_is_read_only() {
        let fixture = DashboardStateFixture::open("project.dashboard-session-read-only").await;
        let selected = resolve_lcm_store_for_layout(&fixture.layout, None);

        assert!(selected.lcm_db.is_none());
        assert_eq!(selected.scope, "unavailable");
    }

    #[test]
    fn dashboard_bindings_are_loopback_only() {
        assert_eq!(
            validate_dashboard_host("127.0.0.1").expect("IPv4 loopback"),
            "127.0.0.1"
        );
        assert_eq!(
            validate_dashboard_host("localhost").expect("localhost"),
            "localhost"
        );
        assert_eq!(
            validate_dashboard_host("::1").expect("IPv6 loopback"),
            "::1"
        );
        assert!(validate_dashboard_host("0.0.0.0").is_err());
    }

    #[tokio::test]
    async fn application_routes_are_active_project_only() {
        let fixture = DashboardStateFixture::open("project.dashboard-application-route").await;
        let state = fixture.state;
        let project_id = state.project_id.clone().expect("active project id");
        let application = ActiveProjectApplicationRoutes {
            http_router: Router::new()
                .route("/probe", get(|| async { StatusCode::NO_CONTENT }))
                .route("/work/views", post(|| async { StatusCode::NO_CONTENT })),
            dashboard_configuration_router: Router::new()
                .route("/probe", get(|| async { StatusCode::ACCEPTED })),
            dashboard_feedback_router: Router::new()
                .route("/probe", get(|| async { StatusCode::OK })),
            dashboard_work_router: Router::new()
                .route("/views", post(|| async { StatusCode::NO_CONTENT })),
            executor: None,
        };
        let app = router_with_active_application(state, Some(application), Router::new());

        let active = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/application/probe")
                    .body(Body::empty())
                    .expect("active application request"),
            )
            .await
            .expect("active application response");
        assert_eq!(active.status(), StatusCode::NO_CONTENT);

        let feedback = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/feedback/probe")
                    .body(Body::empty())
                    .expect("active dashboard feedback request"),
            )
            .await
            .expect("active dashboard feedback response");
        assert_eq!(feedback.status(), StatusCode::OK);

        let dashboard_configuration = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/dashboard/application/probe")
                    .body(Body::empty())
                    .expect("dashboard application request"),
            )
            .await
            .expect("dashboard application response");
        assert_eq!(dashboard_configuration.status(), StatusCode::ACCEPTED);

        let work = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/work/views")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("dashboard Work request"),
            )
            .await
            .expect("dashboard Work response");
        assert_eq!(work.status(), StatusCode::NO_CONTENT);

        let selected = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/application/probe"))
                    .body(Body::empty())
                    .expect("selected-project application request"),
            )
            .await
            .expect("selected-project application response");
        assert_eq!(selected.status(), StatusCode::NOT_FOUND);
    }

    /// Deliverable 4: the V2 read-model routes must be reachable through both
    /// router construction paths — the active-project gateway (`/api/…`) and the
    /// project-scoped gateway (`/api/projects/{id}/…`) — mirroring how the
    /// existing families are exposed.
    #[tokio::test]
    async fn v2_read_models_are_reachable_through_both_gateways() {
        let fixture = DashboardStateFixture::open("project.dashboard-read-model-route").await;
        let state = fixture.state;
        let project_id = state.project_id.clone().expect("active project id");
        let app = router_with_active_application(state, None, Router::new());

        // Every V2 read-model route resolves both through the active gateway and
        // through the project-scoped gateway for the active project.
        for tail in [
            "doctor/findings",
            "storage/telemetry",
            "storage/findings",
            "code-index/freshness",
            "feedback/status",
        ] {
            let active = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/{tail}"))
                        .body(Body::empty())
                        .expect("active v2 request"),
                )
                .await
                .expect("active v2 response");
            assert_eq!(
                active.status(),
                StatusCode::OK,
                "active gateway route /api/{tail} should resolve"
            );
            let body = axum::body::to_bytes(active.into_body(), 1 << 20)
                .await
                .expect("active body");
            let value: Value = serde_json::from_slice(&body).expect("envelope json");
            assert_eq!(value["schema_revision"], 1, "/api/{tail} envelope revision");
            assert!(
                value.get("domain_state").is_some(),
                "/api/{tail} carries a domain_state"
            );

            let scoped = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/projects/{project_id}/{tail}"))
                        .body(Body::empty())
                        .expect("scoped v2 request"),
                )
                .await
                .expect("scoped v2 response");
            assert_eq!(
                scoped.status(),
                StatusCode::OK,
                "project-scoped gateway route for /api/{tail} should resolve"
            );
        }
    }

    /// Graph reads are served only from an admitted, verified projection, so a
    /// dashboard that never mounted one cannot answer `ready` — it must answer
    /// an enveloped `unknown` naming the missing registry, and must never
    /// fabricate totals from the raw store connection the state still holds.
    ///
    /// The `ready` side of this contract needs a real verified generation, so
    /// it is proven end-to-end against a mounted graph authority by
    /// `graph_api_returns_seeded_overview_search_detail_and_subgraph` in
    /// `tests/dashboard_api_test/graph.rs`.
    #[tokio::test]
    async fn graph_overview_without_a_verified_projection_is_an_enveloped_unknown_read() {
        let fixture = DashboardStateFixture::open("project.dashboard-graph-envelope").await;
        assert!(
            fixture.state.code_graph_read_admission.is_none()
                && fixture.state.code_graph_projection_read_port.is_none(),
            "this fixture must not mount a graph authority",
        );
        let app = admitted_router(fixture.state);

        let response = app
            .oneshot(admitted_request("/api/plugins/graph/overview"))
            .await
            .expect("graph overview response");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the daemon answered; source unavailability belongs in the envelope"
        );
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("graph overview body");
        let value: Value = serde_json::from_slice(&body).expect("graph overview json");

        assert_eq!(
            value["schema_revision"],
            crate::read_model::DASHBOARD_SCHEMA_REVISION_V1
        );
        assert_eq!(value["domain_state"], "unknown");
        assert_eq!(value["authorization"]["outcome"], "authorized");
        assert_eq!(value["coverage"]["completeness"], "unknown");
        assert_eq!(
            value["coverage"]["omission_reasons"],
            json!(["missing_registry"])
        );
        assert!(
            value["payload"].is_null(),
            "an unmounted graph authority must not fabricate a payload: {value}"
        );
        assert!(
            value["version"]["graph_version"].is_null(),
            "an unmounted graph authority has no verified generation: {value}"
        );
    }

    #[tokio::test]
    async fn missing_project_registry_is_an_enveloped_unknown_read() {
        let fixture = DashboardStateFixture::open("project.dashboard-registry-envelope").await;
        let app = router_with_active_application(fixture.state, None, Router::new());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects")
                    .body(Body::empty())
                    .expect("project registry request"),
            )
            .await
            .expect("project registry response");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the daemon answered; source unavailability belongs in the envelope"
        );
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("project registry body");
        let value: Value = serde_json::from_slice(&body).expect("project registry json");

        assert_eq!(value["schema_revision"], 1);
        assert_eq!(value["domain_state"], "unknown");
        assert_eq!(value["payload"]["status"], "missing_registry");
        assert_eq!(value["coverage"]["completeness"], "unknown");
    }

    #[tokio::test]
    async fn memory_status_returns_the_canonical_dashboard_envelope() {
        let fixture = DashboardStateFixture::open("project.dashboard-memory-envelope").await;
        let app = admitted_router(fixture.state);

        let response = app
            .oneshot(admitted_request("/api/plugins/holographic/status"))
            .await
            .expect("memory status response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("memory status body");
        let value: Value = serde_json::from_slice(&body).expect("memory status json");

        assert_eq!(value["schema_revision"], 1);
        assert_eq!(value["domain_state"], "ready");
        assert_eq!(value["coverage"]["completeness"], "complete");
        assert!(value["payload"]["memory"].is_object());
    }

    #[tokio::test]
    async fn lcm_search_and_session_use_canonical_daemon_authority_and_opaque_cursors() {
        let mut fixture = DashboardStateFixture::open("project.dashboard-lcm-envelope").await;
        fixture.state.lcm_read_authority = Some(Arc::new(FakeDashboardLcmRead));
        let app = router_with_active_application(fixture.state, None, Router::new());

        for (uri, cursor) in [
            (
                "/api/plugins/hermes-lcm/search?q=needle",
                "opaque-search-cursor",
            ),
            (
                "/api/plugins/hermes-lcm/session/session.dashboard",
                "opaque-session-cursor",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .extension(dashboard_lcm_test_control())
                        .body(Body::empty())
                        .expect("LCM browse request"),
                )
                .await
                .expect("LCM browse response");
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("LCM browse body");
            let value: Value = serde_json::from_slice(&body).expect("LCM browse json");

            assert_eq!(value["schema_revision"], 1, "{uri}");
            assert_eq!(value["domain_state"], "ready", "{uri}");
            assert_eq!(
                value["payload"]["messages"]
                    .as_array()
                    .or_else(|| value["payload"]["matches"]["messages"].as_array())
                    .and_then(|messages| messages.first())
                    .and_then(|message| message["content"].as_str()),
                Some("canonically hydrated"),
                "{uri}",
            );
            assert_eq!(value["payload"]["next_cursor"], cursor, "{uri}");
        }
    }

    #[tokio::test]
    async fn lcm_aggregate_reads_use_the_mounted_daemon_authority() {
        let mut fixture = DashboardStateFixture::open("project.dashboard-lcm-aggregate").await;
        fixture.state.lcm_read_authority = Some(Arc::new(FakeDashboardLcmRead));
        let app = router_with_active_application(fixture.state, None, Router::new());

        for uri in [
            "/api/plugins/hermes-lcm/overview",
            "/api/plugins/hermes-lcm/timeline",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .extension(dashboard_lcm_test_control())
                        .body(Body::empty())
                        .expect("LCM aggregate request"),
                )
                .await
                .expect("LCM aggregate response");
            let body = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("LCM aggregate body");
            let value: Value = serde_json::from_slice(&body).expect("LCM aggregate json");

            assert_eq!(value["domain_state"], "ready", "{uri}");
            assert_eq!(value["coverage"]["completeness"], "complete", "{uri}");
            assert_eq!(value["coverage"]["eligible"], 1, "{uri}");
            assert_eq!(value["coverage"]["examined"], 1, "{uri}");
            assert_eq!(value["payload"]["exists"], true, "{uri}");
        }
    }

    #[tokio::test]
    async fn unavailable_analytics_is_an_enveloped_unknown_read() {
        let fixture = DashboardStateFixture::open("project.dashboard-analytics-envelope").await;
        let app = router_with_active_application(fixture.state, None, Router::new());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/plugins/analytics/overview")
                    .body(Body::empty())
                    .expect("analytics overview request"),
            )
            .await
            .expect("analytics overview response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("analytics overview body");
        let value: Value = serde_json::from_slice(&body).expect("analytics overview json");

        assert_eq!(value["schema_revision"], 1);
        assert_eq!(value["domain_state"], "unknown");
        assert_eq!(value["payload"]["available"], false);
        assert_eq!(
            value["coverage"]["omission_reasons"],
            serde_json::json!(["analytics_sources_unavailable"])
        );
    }

    #[tokio::test]
    async fn unavailable_analytics_detail_reads_are_enveloped_unknown_states() {
        let fixture = DashboardStateFixture::open("project.dashboard-analytics-detail").await;
        let app = router_with_active_application(fixture.state, None, Router::new());

        for uri in [
            "/api/plugins/analytics/agents",
            "/api/plugins/analytics/hints",
            "/api/plugins/analytics/usage",
            "/api/plugins/analytics/underused",
            "/api/plugins/analytics/diagnostics",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("analytics detail request"),
                )
                .await
                .expect("analytics detail response");
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("analytics detail body");
            let value: Value = serde_json::from_slice(&body).expect("analytics detail json");

            assert_eq!(value["schema_revision"], 1, "{uri}");
            assert_eq!(value["domain_state"], "unknown", "{uri}");
            assert_eq!(value["payload"]["available"], false, "{uri}");
        }
    }

    #[tokio::test]
    async fn unavailable_savings_is_an_enveloped_unknown_read() {
        let fixture = DashboardStateFixture::open("project.dashboard-savings-envelope").await;
        let app = router_with_active_application(fixture.state, None, Router::new());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/plugins/savings/overview")
                    .body(Body::empty())
                    .expect("savings overview request"),
            )
            .await
            .expect("savings overview response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("savings overview body");
        let value: Value = serde_json::from_slice(&body).expect("savings overview json");

        assert_eq!(value["schema_revision"], 1);
        assert_eq!(value["domain_state"], "unknown");
        assert_eq!(value["payload"]["savings"]["available"], false);
        assert_eq!(value["payload"]["sessions"]["available"], false);
        assert_eq!(value["payload"]["provider_usage"]["available"], false);
    }

    #[test]
    fn a_selected_project_answers_feedback_and_work_reads_and_nothing_else_by_post() {
        for tail in ["feedback/get", "feedback/expand", "feedback/list"] {
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                Some(SelectedProjectApplicationRead::Feedback)
            );
            assert_eq!(selected_project_application_read(&Method::GET, tail), None);
        }
        // `views` is the canonical Work projection read a selected project may
        // ask for; naming it keeps the intent legible even as the inventory
        // moves underneath the derived loop below.
        assert_eq!(
            selected_project_application_read(&Method::POST, "work/views"),
            Some(SelectedProjectApplicationRead::Work)
        );
        for operation in WorkOperation::ALL {
            if !operation.is_read_only() {
                continue;
            }
            let tail = operation
                .route_path()
                .strip_prefix("/")
                .expect("a rooted route path");
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                Some(SelectedProjectApplicationRead::Work),
                "{tail} is a read-only Work operation a selected project may read"
            );
            assert_eq!(selected_project_application_read(&Method::GET, tail), None);
        }

        // Every Work command stays refused: a selected project is read-only
        // through this gateway.
        for operation in WorkOperation::ALL {
            if operation.is_read_only() {
                continue;
            }
            let tail = operation
                .route_path()
                .strip_prefix("/")
                .expect("a rooted route path");
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                None,
                "{tail} must not be answerable for a selected project"
            );
        }
        assert_eq!(
            selected_project_application_read(&Method::POST, "feedback/status"),
            None
        );

        assert_eq!(
            selected_project_application_read(
                &Method::POST,
                "application/workflow/list-definitions"
            ),
            Some(SelectedProjectApplicationRead::Workflow)
        );
        for operation in WorkflowOperation::ALL {
            let tail = operation
                .application_route_path()
                .strip_prefix("/")
                .expect("a rooted application route path");
            if operation.is_read_only() {
                assert_eq!(
                    selected_project_application_read(&Method::POST, tail),
                    Some(SelectedProjectApplicationRead::Workflow),
                    "{tail} is a read-only Workflow operation a selected project may read"
                );
                assert_eq!(selected_project_application_read(&Method::GET, tail), None);
            } else {
                // Mutations stay refused: the gateway is read-only.
                assert_eq!(
                    selected_project_application_read(&Method::POST, tail),
                    None,
                    "{tail} must not be answerable for a selected project"
                );
            }
        }
    }
}
