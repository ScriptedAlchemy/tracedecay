//! `tracedecay dashboard` — local HTTP server for the dashboard UIs.
//!
//! Serves two dashboard plugin bundles ported from Hermes (the
//! holographic-memory explorer and the LCM explorer) behind a small
//! standalone shell, plus the JSON API both UIs expect — re-implemented on
//! top of tracedecay's own data:
//!
//! - `/api/plugins/holographic/*`  → project memory store
//!   (`memory_facts` / `memory_entities` / `memory_banks` in the project DB)
//! - `/api/plugins/hermes-lcm/*`   → LCM session store
//!   (`lcm_raw_messages` / `lcm_summary_nodes` in the resolved active project
//!   store where transcript ingest writes; see [`resolve_lcm_store`] for the
//!   fail-closed authority selection)
//!
//! The endpoint paths and JSON payload shapes intentionally mirror the
//! original Hermes plugin APIs (`plugins/memory/holographic_plus/dashboard/
//! plugin_api.py` and the hermes-lcm `dashboard/plugin_api.py`) so the plugin
//! bundles run unmodified under both hosts. The Hermes-side wrapper plugin
//! reverse-proxies to this server, making this the canonical implementation.
//!
//! `/api/capabilities` advertises which features are live so hosts (or a
//! richer Hermes wrapper) can extend the surface without forking the UI.

pub(crate) mod analytics_api;
pub(crate) mod assets;
mod automation_config_api;
mod automation_fact_proposals_api;
mod automation_jobs_api;
mod automation_outcomes_api;
mod automation_run_api;
mod automation_run_service;
pub(crate) use automation_run_service::{
    DashboardAutomationWriter, standalone_dashboard_automation_writer,
};
mod automation_scheduler_api;
mod automation_skills_api;
mod code_diagnostics_api;
pub(crate) mod code_index_freshness_api;
#[doc(hidden)]
pub mod contract_schema;
mod delivery_api;
mod doctor_findings_api;
pub(crate) mod doctor_remediation_api;
pub(crate) use doctor_remediation_api::{
    DoctorRemediationDispatchCommandV1, DoctorRemediationDispatchErrorV1,
    DoctorRemediationDispatcherV1, DoctorRemediationLegalActionV1,
    DoctorRemediationOperationPhaseV1, DoctorRemediationOperationV1, DoctorRemediationTargetV1,
    DoctorRemediationVerificationV1,
};
mod events_api;
mod explorer_api;
pub(crate) mod feedback_api;
mod graph_api;
mod graph_queries;
mod graph_service;
mod graph_structure_api;
mod lcm_api;
#[cfg(test)]
#[path = "../sessions/lcm/dashboard_fixes_tests.rs"]
mod lcm_dashboard_fixes_tests;
mod lcm_queries;
mod lcm_service;
mod loom_api;
mod memory_analysis;
mod memory_api;
pub mod memory_curate;
mod memory_service;
mod projects;
mod read_model;
mod savings_api;
mod savings_pricing;
pub(crate) mod scope;
mod settings_api;
mod storage_findings_api;
mod storage_telemetry_api;
mod token_count;
mod util;
mod work_api;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, patch, post};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tower::ServiceExt;

use tracedecay_api::WorkOperation;

use crate::application_surface::{
    dashboard_configuration_application_router_with_executor,
    dashboard_feedback_application_router_with_executor,
    dashboard_work_application_router_with_executor, http_application_router_with_executor,
};
use crate::automation::backend;
use crate::automation::config::{self, AutomationBackend, AutomationHostMode};
use crate::daemon::{DaemonHandshake, daemon_operation_event_authority};
use crate::daemon_client::{DaemonInvocationClient, DaemonInvocationExecutor};
use crate::db::{Database, DatabaseEngineConnection};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::StorageMode;
use crate::tracedecay::TraceDecay;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_domain::{FactOwnerV1, ProjectId};

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
pub(crate) enum AutomationReconcileScope {
    Project,
    Profile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UncachedProjectReconcileOutcome {
    DeferredUntilProjectStartup,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct AutomationSchedulerOwnerReconcileOutcome {
    pub(crate) project_id: Option<String>,
    pub(crate) store_root: PathBuf,
    pub(crate) graph_db_path: PathBuf,
    pub(crate) scope_prefix: Option<String>,
    pub(crate) outcome: AutomationSchedulerReconcileOutcome,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ProfileAutomationReconcileReport {
    pub(crate) scope: AutomationReconcileScope,
    pub(crate) cached_owners: usize,
    pub(crate) outcomes: Vec<AutomationSchedulerOwnerReconcileOutcome>,
    pub(crate) uncached_projects: UncachedProjectReconcileOutcome,
}

pub(crate) type AutomationSchedulerReconcileFuture =
    Pin<Box<dyn Future<Output = AutomationSchedulerReconcileOutcome> + Send + 'static>>;
pub(crate) type AutomationSchedulerReconciler =
    Arc<dyn Fn() -> AutomationSchedulerReconcileFuture + Send + Sync + 'static>;
pub(crate) type DoctorReportReadFuture = Pin<
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
pub(crate) type DoctorReportReader =
    Arc<dyn Fn() -> DoctorReportReadFuture + Send + Sync + 'static>;

#[derive(Clone)]
pub(crate) struct AdmittedDoctorReportV1 {
    pub(crate) report: tracedecay_application::doctor::DoctorReportV1,
    pub(crate) table_growth_evidence:
        Vec<tracedecay_application::storage::TableGrowthDoctorEvidenceV1>,
}

impl AdmittedDoctorReportV1 {
    pub(crate) fn new(report: tracedecay_application::doctor::DoctorReportV1) -> Self {
        Self {
            report,
            table_growth_evidence: Vec::new(),
        }
    }

    pub(crate) fn with_table_growth_evidence(
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
pub(crate) struct DashboardState {
    /// Registered project id for profile-backed stores, when known.
    pub(crate) project_id: Option<String>,
    /// Exact application scope resolved ONCE when this state was constructed.
    /// `None` is the explicit fail-closed state (missing registry, invalid
    /// project id, or unresolvable exact root): handlers report their typed
    /// unavailable states from it and never re-resolve scope from paths or
    /// the CWD per request.
    pub(crate) resolved_scope: Option<tracedecay_application::ResolvedScope>,
    /// Exact project graph retained by the daemon for this dashboard state.
    /// Absent for lightweight/profile-only states that cannot run project
    /// automation.
    pub(crate) project_graph: Option<Arc<TraceDecay>>,
    /// Resolves other registered projects only when their graph is already
    /// mounted by the daemon.
    pub(crate) project_graph_resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    /// Immutable authoritative owner for every memory operation served here.
    pub(crate) memory_owner: FactOwnerV1,
    /// Active code-graph database. This can be branch-specific.
    pub(crate) graph_conn: DatabaseEngineConnection,
    /// Keeps every project-database authority alive as long as cloned raw
    /// connections remain reachable through this state.
    pub(crate) _database_guards: Vec<Arc<Database>>,
    /// Read-only telemetry handle attached to the retained active graph runtime.
    /// This remains distinct from project memory when those stores use
    /// different files.
    pub(crate) graph_telemetry_handle:
        Option<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle>,
    /// Display path of the active code-graph database.
    pub(crate) graph_db_path: String,
    /// Authoritative project-memory handle and process-local writer lane.
    pub(crate) mem_db: Arc<Database>,
    /// Display path of the project memory database.
    pub(crate) mem_db_path: String,
    /// Registered LCM session store for the resolved active project store.
    /// Absent when exact project-session authority is unavailable.
    pub(crate) lcm_db: Option<Arc<RegisteredGlobalDb>>,
    /// Display path of the LCM session store actually being served.
    pub(crate) lcm_db_path: String,
    /// Which store `lcm_db` points at: project storage mode or `"unavailable"`.
    pub(crate) lcm_scope: String,
    /// Global accounting DB (savings ledger, lifetime counters, turns) used
    /// by the Savings & Cost tab, when available.
    pub(crate) savings_db: Option<Arc<RegisteredGlobalDb>>,
    /// Display path of the global accounting DB.
    pub(crate) savings_db_path: String,
    pub(crate) project_root: PathBuf,
    /// Live read port over the daemon-owned code-index scheduler registry.
    pub(crate) code_index_freshness_reader:
        Option<code_index_freshness_api::CodeIndexFreshnessReader>,
    /// Root-addressed read over the daemon-mounted canonical feedback
    /// observation owner. Selected projects reuse the resolver but resolve
    /// their own exact project root on every call.
    pub(crate) feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    /// Storage mode resolved for the active project store.
    pub(crate) storage_mode: String,
    /// Resolved active project store root.
    pub(crate) store_root: PathBuf,
    /// Resolved `config.json` path for the active project store.
    pub(crate) config_path: PathBuf,
    /// Resolved dashboard sidecar root inside the active project store.
    pub(crate) dashboard_root: PathBuf,
    /// Retention policy resolved with the owning runtime configuration.
    /// Dashboard reads must not re-open mutable config input per request.
    pub(crate) retention_config: crate::config::RetentionConfig,
    /// Daemon-owned user-profile settings authority. Dashboard routes never
    /// load or mutate `config.toml` directly.
    pub(crate) user_settings: Arc<dyn crate::application::configuration::UserSettingsDaemonClient>,
    /// Recent deterministic curation activity emitted by the standalone dashboard.
    pub(crate) curation_activity: Arc<RwLock<Vec<Value>>>,
    /// Process-local derived BPE token-count cache for the Savings & Cost tab.
    pub(crate) token_counts: Arc<token_count::TokenCountCache>,
    /// Admitted daemon/application diagnostics authority. `None` keeps all
    /// diagnostics controls typed unavailable; the dashboard never constructs
    /// a broker or analyzer runtime.
    pub(crate) code_diagnostics_authority:
        Option<crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1>,
    pub(crate) automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    /// Lifetime-owning capability for complete dashboard automation writes.
    pub(crate) automation_writer: DashboardAutomationWriter,
    /// Admitted canonical Doctor report source. Absent when the dashboard was
    /// not opened by an owner holding an exact application request context.
    pub(crate) doctor_report_reader: Option<DoctorReportReader>,
    /// Optional admitted owner-operation router. Its absence keeps remediation
    /// references descriptive and non-actionable.
    pub(crate) doctor_remediation_dispatcher:
        Option<doctor_remediation_api::DoctorRemediationDispatcherV1>,
    /// Active-project daemon application transport. Mutating dashboard routes
    /// use this catalog-bound executor instead of opening stores or applying
    /// configuration inside HTTP adapters.
    pub(crate) application_invocation_executor: Option<Arc<dyn DaemonInvocationExecutor>>,
}

/// Test-only lifetime owner for the same registered authorities retained by a
/// daemon dashboard. Integration tests pass the typed host-admission runtime;
/// raw database handles never cross the public test seam.
pub(crate) struct DashboardHostAdmissionTestAuthorityV1 {
    _runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
    project_sessions: Arc<RegisteredGlobalDb>,
    profile_database: Arc<RegisteredGlobalDb>,
}

impl DashboardHostAdmissionTestAuthorityV1 {
    pub(crate) fn new(
        runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
        profile_database: Arc<RegisteredGlobalDb>,
        project_sessions: Arc<RegisteredGlobalDb>,
    ) -> Self {
        Self {
            _runtime: runtime,
            project_sessions,
            profile_database,
        }
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

    fn resolver(&self) -> crate::mcp::server::RetainedProjectGraphResolver {
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
    pub(crate) fn reconcile_automation_scheduler(&self) {
        if let Some(reconcile) = &self.automation_scheduler_reconciler {
            let reconcile = Arc::clone(reconcile);
            tokio::spawn(async move {
                let _ = reconcile().await;
            });
        }
    }
}

/// The LCM session store the dashboard will serve.
pub(crate) struct LcmStoreSelection {
    pub(crate) lcm_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) path: String,
    pub(crate) scope: String,
}

/// Selects the LCM session store for the resolved active project store.
///
/// Transcript ingest writes to the active code-project store selected by the
/// storage resolver. For profile-backed projects, that is the user-level shard
/// under `~/.tracedecay/projects/<project_id>/`, not a repo-local DB.
///
/// Session storage fails closed when the project authority is unavailable;
/// the global accounting DB is never a fallback LCM destination.
pub(crate) async fn resolve_lcm_store(
    cg: &TraceDecay,
    registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
) -> LcmStoreSelection {
    if let Some(db) = registered_project_session_db {
        return LcmStoreSelection {
            path: db.db_path().display().to_string(),
            lcm_db: Some(db),
            scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
        };
    }
    LcmStoreSelection {
        lcm_db: None,
        path: cg.store_layout().sessions_db_path.display().to_string(),
        scope: "unavailable".to_string(),
    }
}

pub(crate) fn storage_mode_label(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProjectLocal => "project_local",
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

pub(crate) fn resolve_project_memory_store(cg: &TraceDecay) -> (String, Arc<Database>) {
    (
        cg.dashboard_db_path().display().to_string(),
        cg.dashboard_database_guard(),
    )
}

/// Resolves the immutable fact owner from the validated project layout.
///
/// Dashboard routes must never infer ownership from a path, label, or
/// optional display field after construction.
pub(crate) fn project_memory_owner(cg: &TraceDecay) -> Result<FactOwnerV1> {
    let raw = cg
        .store_layout()
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
    project_graph_resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
    registered_savings_db: Option<Arc<RegisteredGlobalDb>>,
    warm_token_counts: bool,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
    doctor_report_reader: Option<DoctorReportReader>,
    doctor_remediation_dispatcher: Option<doctor_remediation_api::DoctorRemediationDispatcherV1>,
    code_index_freshness_reader: Option<code_index_freshness_api::CodeIndexFreshnessReader>,
    feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    code_diagnostics_broker: Option<
        Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    >,
    application_invocation_executor: Option<Arc<dyn DaemonInvocationExecutor>>,
) -> Result<DashboardState> {
    let (mem_db_path, mem_db) = resolve_project_memory_store(cg);
    let memory_owner = project_memory_owner(cg)?;
    let lcm = resolve_lcm_store(cg, registered_project_session_db).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let store_root = cg.store_layout().data_root.clone();
    let config_path = cg.store_layout().config_path.clone();
    let storage_mode = storage_mode_label(&cg.store_layout().storage_mode).to_string();
    let code_diagnostics_authority = code_diagnostics_broker.map(|broker| {
        crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1::new(
            cg.project_root().to_path_buf(),
            dashboard_root.clone(),
            Arc::clone(&mem_db),
            broker,
        )
    });
    let savings_db_path = registered_savings_db
        .as_ref()
        .map(|db| db.db_path().display().to_string())
        .or_else(|| crate::global_db::global_db_path().map(|path| path.display().to_string()))
        .unwrap_or_default();
    let state = DashboardState {
        project_id: cg.store_layout().identity.project_id.clone(),
        resolved_scope: scope::resolve_dashboard_scope(
            cg.project_root(),
            cg.store_layout().identity.project_id.as_deref(),
        ),
        project_graph,
        project_graph_resolver,
        memory_owner,
        graph_conn: mem_db.engine_conn(),
        _database_guards: vec![mem_db.clone()],
        graph_telemetry_handle: cg.storage_telemetry_handle().ok(),
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_db,
        mem_db_path,
        lcm_db: lcm.lcm_db,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        savings_db: registered_savings_db,
        savings_db_path,
        project_root: cg.project_root().to_path_buf(),
        code_index_freshness_reader,
        feedback_status_reader,
        storage_mode,
        store_root,
        config_path,
        dashboard_root,
        retention_config: cg.get_config().sync.retention.clone(),
        user_settings: cg.configuration_runtime().user_settings_client(),
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics_authority,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader,
        doctor_remediation_dispatcher,
        application_invocation_executor,
    };
    // Pre-count non-usage messages in the background so the first Savings
    // tab paint doesn't pay the initial BPE pass over the session store.
    if warm_token_counts {
        token_count::spawn_warm(state.clone());
    }
    Ok(state)
}

/// Builds the dashboard state shared by the CLI `run` path and the
/// `tracedecay_dashboard` MCP tool.
#[allow(dead_code)]
pub(crate) async fn build_state(cg: &TraceDecay) -> Result<DashboardState> {
    build_state_inner(
        cg,
        None,
        None,
        None,
        None,
        true,
        None,
        standalone_dashboard_automation_writer(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
}

pub(crate) async fn build_state_with_automation_reconciler(
    cg: Arc<TraceDecay>,
    project_graph_resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
    registered_savings_db: Option<Arc<RegisteredGlobalDb>>,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
    doctor_report_reader: Option<DoctorReportReader>,
    doctor_remediation_dispatcher: Option<doctor_remediation_api::DoctorRemediationDispatcherV1>,
    code_index_freshness_reader: Option<code_index_freshness_api::CodeIndexFreshnessReader>,
    feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    code_diagnostics_broker: Option<
        Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    >,
    application_invocation_executor: Option<Arc<dyn DaemonInvocationExecutor>>,
) -> Result<DashboardState> {
    build_state_inner(
        cg.as_ref(),
        Some(Arc::clone(&cg)),
        project_graph_resolver,
        registered_project_session_db,
        registered_savings_db,
        true,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader,
        doctor_remediation_dispatcher,
        code_index_freshness_reader,
        feedback_status_reader,
        code_diagnostics_broker,
        application_invocation_executor,
    )
    .await
}

/// Builds a lightweight cached state for a non-active project selected from the
/// dashboard project picker. Automation authority is inherited from the active
/// dashboard state so daemon-selected projects cannot fall back to direct open.
pub(crate) async fn build_selected_project_state(
    cg: Arc<TraceDecay>,
    active: &DashboardState,
) -> Result<DashboardState> {
    build_state_inner(
        cg.as_ref(),
        Some(Arc::clone(&cg)),
        active.project_graph_resolver.clone(),
        None,
        active.savings_db.clone(),
        false,
        None,
        Arc::clone(&active.automation_writer),
        // Doctor authority is bound to the active project's exact scope.
        // Freshness is different: its daemon registry reader resolves the
        // selected state's exact canonical root and returns only a mounted
        // scheduler, so the root-addressed read port is safe to reuse.
        None,
        None,
        active.code_index_freshness_reader.clone(),
        active.feedback_status_reader.clone(),
        None,
        active.application_invocation_executor.clone(),
    )
    .await
}

pub(crate) fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Builds state and runs the dashboard server until `shutdown` resolves.
/// Binds `host:port` (`port` 0 lets the OS pick) and prints the URL on
/// stderr; the URL line on stdout is stable output for wrappers to parse.
/// Pass `open: true` to also open the URL in the default browser (CLI --open).
pub async fn run_until_shutdown<F>(
    cg: &TraceDecay,
    host: &str,
    port: u16,
    open: bool,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    run_until_shutdown_inner(
        cg,
        host,
        port,
        shutdown,
        DashboardRunOptions::production(open),
        None,
        None,
        None,
    )
    .await
}

#[doc(hidden)]
pub async fn run_until_shutdown_for_tests<F>(
    cg: &TraceDecay,
    host: &str,
    port: u16,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    run_until_shutdown_inner(
        cg,
        host,
        port,
        shutdown,
        DashboardRunOptions::test(),
        None,
        None,
        None,
    )
    .await
}

#[doc(hidden)]
#[cfg(feature = "test-transport")]
pub async fn run_until_shutdown_for_tests_with_host_admission<F>(
    cg: Arc<TraceDecay>,
    runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
    project_graphs: DashboardTestProjectGraphsV1,
    host: &str,
    port: u16,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let authority = runtime.dashboard_test_authority()?;
    let project_graph_resolver = project_graphs.resolver();
    run_until_shutdown_inner(
        cg.as_ref(),
        host,
        port,
        shutdown,
        DashboardRunOptions::test(),
        Some(&authority),
        Some(project_graph_resolver),
        Some(Arc::clone(&cg)),
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct DashboardRunOptions {
    open: bool,
    warm_token_counts: bool,
}

impl DashboardRunOptions {
    fn production(open: bool) -> Self {
        Self {
            open,
            warm_token_counts: true,
        }
    }

    fn test() -> Self {
        Self {
            open: false,
            warm_token_counts: false,
        }
    }
}

async fn run_until_shutdown_inner<F>(
    cg: &TraceDecay,
    host: &str,
    port: u16,
    shutdown: F,
    options: DashboardRunOptions,
    test_authority: Option<&DashboardHostAdmissionTestAuthorityV1>,
    test_project_graph_resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    test_project_graph: Option<Arc<TraceDecay>>,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
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
        test_project_graph_resolver,
        test_authority.map(|authority| Arc::clone(&authority.project_sessions)),
        test_authority.map(|authority| Arc::clone(&authority.profile_database)),
        options.warm_token_counts,
        None,
        standalone_dashboard_automation_writer(),
        None,
        None,
        None,
        None,
        Some(code_diagnostics_broker),
        None,
    )
    .await?;
    let app = router(cg, state).await?;
    let (listener, addr) = bind_dashboard(host, port).await?;
    let app = with_dashboard_http_admission(app, addr);

    let url = format!("http://{addr}/");
    // Stable, parseable line for wrappers (the Hermes plugin reads this).
    println!("tracedecay dashboard listening on {url}");
    eprintln!("Serving project {}", cg.project_root().display());
    eprintln!("Press Ctrl+C to stop.");

    if options.open {
        match open::that(&url) {
            Ok(()) => eprintln!("Opened dashboard in default browser: {url}"),
            Err(e) => eprintln!("Warning: could not open browser for {url}: {e}"),
        }
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| config_error(format!("dashboard server error: {e}")))
}

/// Runs the dashboard server until interrupted by Ctrl-C.
pub async fn run(cg: &TraceDecay, host: &str, port: u16, open: bool) -> Result<()> {
    run_until_shutdown(cg, host, port, open, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// Shared bind logic for both CLI `run` and the MCP `tracedecay_dashboard` tool
/// (so port 0 allocation and URL formatting are consistent, no duplication).
pub(crate) async fn bind_dashboard(
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

pub(crate) fn validate_dashboard_host(host: &str) -> Result<&str> {
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

pub(crate) fn with_dashboard_http_admission(app: Router, addr: std::net::SocketAddr) -> Router {
    app.layer(middleware::from_fn_with_state(
        DashboardHttpAdmission { port: addr.port() },
        admit_dashboard_http_request,
    ))
}

async fn admit_dashboard_http_request(
    State(admission): State<DashboardHttpAdmission>,
    headers: HeaderMap,
    request: Request<Body>,
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

    next.run(request).await
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
/// constructs only the PR14 feedback read subset from its retained graph.
struct ActiveProjectApplicationRoutes {
    http_router: Router,
    dashboard_configuration_router: Router,
    dashboard_feedback_router: Router,
    dashboard_work_router: Router,
    executor: Option<Arc<dyn DaemonInvocationExecutor>>,
}

impl ActiveProjectApplicationRoutes {
    fn for_active_project(
        cg: &TraceDecay,
        executor: Option<Arc<dyn DaemonInvocationExecutor>>,
    ) -> Result<Self> {
        let executor = match executor {
            Some(executor) => executor,
            None => {
                let handshake = DaemonHandshake::for_current_client(
                    Some(cg.project_root().to_path_buf()),
                    None,
                    false,
                    false,
                )?;
                Arc::new(DaemonInvocationClient::for_current(handshake)?)
            }
        };
        let active_project_id = match project_memory_owner(cg)? {
            FactOwnerV1::Project { project_id } => project_id,
            FactOwnerV1::Profile => {
                return Err(config_error(
                    "active-project application routes require project authority",
                ));
            }
        };
        let http_router = http_application_router_with_executor(
            Arc::clone(&executor),
            daemon_operation_event_authority(),
            active_project_id,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not mount application HTTP routes: {error}"),
        })?;
        let dashboard_configuration_router =
            dashboard_configuration_application_router_with_executor(Arc::clone(&executor))
                .map_err(|error| TraceDecayError::Config {
                    message: format!("could not mount dashboard configuration routes: {error}"),
                })?;
        let dashboard_feedback_router = dashboard_feedback_application_router_with_executor(
            Arc::clone(&executor),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not mount dashboard feedback routes: {error}"),
        })?;
        let dashboard_work_router = dashboard_work_application_router_with_executor(Arc::clone(
            &executor,
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not mount dashboard Work routes: {error}"),
        })?;
        Ok(Self {
            http_router,
            dashboard_configuration_router,
            dashboard_feedback_router,
            dashboard_work_router,
            executor: Some(executor),
        })
    }
}

/// Builds the complete dashboard router shared by direct and daemon-managed
/// startup. The supplied state is the active writable project authority.
pub(crate) async fn router(cg: &TraceDecay, mut state: DashboardState) -> Result<Router> {
    // Fact writes defer derived memory rebuilds. Invoke the canonical bounded
    // convergence policy exactly once for the active writable project before
    // serving either startup path. Selected-project states are opened later
    // through the read-only gateway and never pass through this function.
    match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => {
            match application
                .converge_derived_memory("dashboard-startup-repair")
                .await
            {
                Ok(report) if report.is_pending() => {
                    tracing::warn!(
                        "Derived memory startup convergence is pending; dashboard startup is \
                         proceeding while remaining repair is deferred to the daemon scheduler"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("Derived memory startup repair skipped: {error}");
                }
            }
        }
        Err(error) => {
            tracing::warn!("Derived memory startup repair skipped: {error}");
        }
    }

    // PR12 application routes are bound to the active-project daemon. When the
    // daemon authority record is unavailable (standalone `tracedecay dashboard`
    // or the in-process test server), mounting them would otherwise fail the
    // whole server before it binds. Degrade gracefully instead — serve the core
    // dashboard and skip the `/api/application` surface, mirroring the
    // best-effort derived-memory repair above.
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
    Ok(router_with_active_application(state, application))
}

fn router_with_active_application(
    state: DashboardState,
    application: Option<ActiveProjectApplicationRoutes>,
) -> Router {
    let runtime = projects::DashboardRuntime::new(state, project_api_router());
    let router = Router::new()
        .route("/", get(assets::app_index))
        .route("/static/{*tail}", get(assets::app_static))
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
        // PR14 V2 read-model surfaces bound through the active-project gateway,
        // mirroring the project-scoped `/api/projects/{id}/…` gateway path.
        .route("/api/doctor/{*tail}", any(active_api_gateway))
        .route("/api/storage/{*tail}", any(active_api_gateway))
        .route("/api/code-index/{*tail}", any(active_api_gateway))
        .route("/api/feedback/status", any(active_api_gateway))
        .route("/api/events", any(active_api_gateway))
        // SPA fallback: unmatched non-API paths are client routes
        // (/brain?scope=… deep links) and receive the embedded app index.
        .fallback(get(assets::app_spa_fallback))
        .with_state(runtime);
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
            "/api/plugins/holographic/curation/status",
            get(memory_api::curation_status),
        )
        .route(
            "/api/plugins/holographic/curation/activity",
            get(memory_api::curation_activity),
        )
        .route(
            "/api/plugins/holographic/curation/runs",
            get(memory_api::curation_runs),
        )
        .route(
            "/api/plugins/holographic/fact-proposals",
            get(memory_api::fact_proposals),
        )
        .route(
            "/api/plugins/holographic/fact-proposals/{proposal_id}/apply",
            post(memory_api::fact_proposal_apply),
        )
        .route(
            "/api/plugins/holographic/fact-proposals/{proposal_id}/reject",
            post(memory_api::fact_proposal_reject),
        )
        .route(
            "/api/plugins/holographic/curation/config",
            get(automation_config_api::get_config)
                .patch(automation_config_api::patch_config)
                .delete(automation_config_api::reset_config),
        )
        .route(
            "/api/automation/skills",
            get(automation_skills_api::list).post(automation_skills_api::draft),
        )
        .route(
            "/api/automation/skills/draft",
            post(automation_skills_api::draft),
        )
        .route(
            "/api/automation/skills/{id}",
            get(automation_skills_api::view).patch(automation_skills_api::update),
        )
        .route(
            "/api/automation/skills/{id}/approve",
            post(automation_skills_api::approve),
        )
        .route(
            "/api/automation/skills/{id}/discard-update",
            post(automation_skills_api::discard_update),
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
            "/api/automation/fact-proposals",
            get(automation_fact_proposals_api::list),
        )
        .route(
            "/api/automation/fact-proposals/{id}",
            get(automation_fact_proposals_api::view),
        )
        .route(
            "/api/automation/fact-proposals/{id}/apply",
            post(automation_fact_proposals_api::apply),
        )
        .route(
            "/api/automation/fact-proposals/{id}/reject",
            post(automation_fact_proposals_api::reject),
        )
        .route(
            "/api/automation/run/memory-curator",
            post(automation_run_api::memory_curator),
        )
        .route(
            "/api/automation/run/session-reflection",
            post(automation_run_api::session_reflection),
        )
        .route(
            "/api/automation/run/skill-writing",
            post(automation_run_api::skill_writing),
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
        .route(
            "/api/automation/runs/{run_id}/artifacts",
            get(automation_run_api::artifact_list),
        )
        .route(
            "/api/automation/runs/{run_id}/artifacts/{kind}",
            get(automation_run_api::artifact_payload),
        )
        .route(
            "/api/plugins/holographic/curate/apply",
            post(memory_api::curate_apply),
        )
        .route("/api/plugins/holographic/oplog", get(memory_api::oplog))
        // LCM plugin API (mirrors hermes-lcm dashboard/plugin_api.py)
        .route("/api/plugins/hermes-lcm/overview", get(lcm_api::overview))
        .route("/api/plugins/hermes-lcm/search", get(lcm_api::search))
        .route(
            "/api/plugins/hermes-lcm/session/{session_id}",
            get(lcm_api::session),
        )
        .route("/api/plugins/hermes-lcm/node/{node_id}", get(lcm_api::node))
        .route("/api/plugins/hermes-lcm/timeline", get(lcm_api::timeline))
        .route(
            "/api/plugins/hermes-lcm/compression",
            get(lcm_api::compression),
        )
        .route(
            "/api/plugins/hermes-lcm/payloads/health",
            get(lcm_api::payloads_health),
        )
        .route(
            "/api/plugins/hermes-lcm/payloads/gc",
            get(lcm_api::payloads_gc_preview).post(lcm_api::payloads_gc_apply),
        )
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
        .route(
            "/api/plugins/analytics/observatory",
            get(analytics_api::observatory_http),
        )
        .route(
            "/api/plugins/analytics/observatory/export",
            get(analytics_api::observatory_export),
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
        .route("/api/plugins/savings/costs", get(savings_api::costs_http))
        .route(
            "/api/plugins/savings/costs/export",
            get(savings_api::costs_export),
        )
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
        // PR14 V2 read-model surfaces (DashboardEnvelope<T>). Doctor finding
        // family, plan-38 storage telemetry/findings, code-index freshness, and
        // the typed SSE stream. See `read_model` for the normative envelope.
        // Read-only Doctor/health paths come from the API-owned descriptors in
        // `tracedecay_api::doctor` so the mount cannot drift from them.
        .route(
            tracedecay_api::doctor::DOCTOR_FINDINGS_ROUTE_PATH,
            get(doctor_findings_api::findings),
        )
        .route(
            "/api/doctor/remediations/preview",
            post(doctor_remediation_api::preview),
        )
        .route(
            "/api/doctor/remediations/apply",
            post(doctor_remediation_api::apply),
        )
        .route(
            "/api/doctor/remediations/{operation_id}",
            get(doctor_remediation_api::status),
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
        .route("/api/delivery/overview", get(delivery_api::overview))
        .route("/api/events", get(events_api::events))
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
            return projects::registry_unavailable_response(&err).into_response();
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
}

impl std::fmt::Display for SelectedProjectApplicationRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Feedback => "feedback",
            Self::Work => "Work",
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
        _ => WorkOperation::CORE
            .into_iter()
            .filter(|operation| operation.is_read_only())
            .any(|operation| tail.strip_prefix("work/") == Some(operation.route_segment()))
            .then_some(SelectedProjectApplicationRead::Work),
    }
}

async fn forward_project_request(
    project_api: Router<DashboardState>,
    state: DashboardState,
    req: Request<Body>,
) -> Response {
    let (mut parts, body) = req.into_parts();
    parts.extensions.clear();
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
    let has_lcm = state.lcm_db.is_some();
    let global_automation = crate::user_config::UserConfig::load().automation;
    let project_automation = config::load_project_config(&state.dashboard_root)
        .await
        .ok()
        .flatten();
    let automation = config::effective_config(&global_automation, project_automation.as_ref())
        .unwrap_or(global_automation);
    let automation_backend = automation.backend;
    let automation_host_mode = automation.host_mode;
    let backend_availability = backend::backend_availability(&automation);
    let automation_backend_supported =
        matches!(automation_backend, AutomationBackend::CodexAppServer);
    let automation_configured = automation.enabled && automation_backend_supported;
    let automation_mode = if !automation_configured {
        "disabled"
    } else if automation_host_mode == AutomationHostMode::DelegatedHost {
        "delegated_host"
    } else {
        "standalone_backend"
    };
    let standalone_automation = automation_mode == "standalone_backend";
    let multi_root = tracedecay_api::read_model::multi_root::MultiRootCapabilityV1::unavailable(
        "multi-root capability is quarantined",
    );
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
            "lcm_gc": has_lcm,
            "lcm_payload_health": has_lcm,
            "graph": true,
            "analytics": true,
            "feedback": state.feedback_status_reader.is_some(),
            "code_diagnostics": state.code_diagnostics_authority.is_some(),
            // Memory curation/refinement is served by the configured
            // standalone automation backend. Explicit agent ops apply through
            // /curate/apply.
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
        "automation": {
            "enabled": automation.enabled,
            "mode": automation_mode,
            "backend": automation_backend,
            "host_mode": automation_host_mode,
            "availability": backend_availability,
        },
        "dashboards": ["tracedecay"],
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod authority_tests {
    use super::*;

    #[tokio::test]
    async fn dashboard_state_resolves_exact_application_scope_once() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-resolved-scope",
        )
        .await
        .expect("project init");
        let project_id = ProjectId::new(
            cg.store_layout()
                .identity
                .project_id
                .clone()
                .expect("registered project id"),
        )
        .expect("valid project id");
        let expected = crate::daemon::project_open_owners::resolved_scope_for_project(
            cg.project_root(),
            &project_id,
        )
        .expect("daemon exact-root scope");

        let state = build_state(&cg).await.expect("dashboard state");

        // The exact-root HTTP surface resolves the same project and scope
        // through the application type, once, at state construction.
        let scope = state
            .resolved_scope
            .clone()
            .expect("exact resolved application scope");
        assert_eq!(scope, expected);
        scope.validate().expect("resolved scope validates");
    }

    #[tokio::test]
    async fn project_memory_owner_uses_validated_store_identity() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-project-memory",
        )
        .await
        .expect("project init");
        let raw = cg
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("registered project id");
        let expected = ProjectId::new(raw).expect("validated project id");

        assert_eq!(
            project_memory_owner(&cg).expect("project memory owner"),
            FactOwnerV1::Project {
                project_id: expected,
            }
        );
    }

    #[tokio::test]
    async fn dashboard_state_reuses_its_active_database_as_memory_authority() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-state",
        )
        .await
        .expect("project init");

        let expected_path = cg.dashboard_db_path().display().to_string();
        let state = build_state(&cg).await.expect("dashboard state");
        let Json(capabilities) = capabilities(State(state.clone())).await;

        assert_eq!(state.mem_db_path, expected_path);
        assert_eq!(state._database_guards.len(), 1);
        assert_eq!(capabilities["multi_root"]["status"], "unavailable");
        assert_eq!(
            capabilities["multi_root"]["reason"],
            "multi-root capability is quarantined"
        );
        assert_eq!(capabilities["features"]["multi_root"], false);
        assert!(
            state.code_diagnostics_authority.is_none(),
            "direct dashboard must not construct an analyzer authority"
        );
    }

    #[tokio::test]
    async fn daemon_dashboard_retains_the_exact_mounted_project_graph() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.daemon-dashboard",
        )
        .await
        .expect("project init");
        let cg = Arc::new(cg);
        let doctor_reader: DoctorReportReader = Arc::new(|| {
            Box::pin(async {
                Err(
                    tracedecay_application::ApplicationContractError::Inconsistent {
                        field: "dashboard authority test reader",
                    },
                )
            })
        });
        let doctor_dispatcher = DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { Vec::new() })),
            Arc::new(|_| {
                Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })
            }),
            Arc::new(|_| panic!("dashboard construction does not observe remediation")),
        );
        let diagnostic_broker = Arc::new(tokio::sync::Mutex::new(
            crate::application::dashboard_diagnostics::diagnostic_broker(
                cg.project_root().to_path_buf(),
                tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings::default(),
            ),
        ));

        let state = build_state_with_automation_reconciler(
            Arc::clone(&cg),
            None,
            None,
            None,
            None,
            standalone_dashboard_automation_writer(),
            Some(Arc::clone(&doctor_reader)),
            Some(doctor_dispatcher),
            None,
            None,
            Some(diagnostic_broker),
            None,
        )
        .await
        .expect("dashboard state");

        assert!(Arc::ptr_eq(
            state.project_graph.as_ref().expect("retained graph"),
            &cg,
        ));
        assert!(Arc::ptr_eq(
            state
                .doctor_report_reader
                .as_ref()
                .expect("admitted Doctor reader"),
            &doctor_reader,
        ));
        assert!(state.doctor_remediation_dispatcher.is_some());
        assert!(
            state.code_diagnostics_authority.is_some(),
            "daemon dashboard must retain the admitted diagnostics authority"
        );
    }

    #[tokio::test]
    async fn retained_project_session_authority_is_reused_exactly() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _graph_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.retained-session",
        )
        .await
        .expect("project init");
        let project_id = ProjectId::new(
            cg.store_layout()
                .identity
                .project_id
                .clone()
                .expect("project identity"),
        )
        .expect("valid project identity");
        let runtime = Arc::new(
            crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                project.path().join(".profile"),
                project.path(),
                project_id,
            )
            .await
            .expect("registered project sessions"),
        );
        let authority = runtime
            .dashboard_test_authority()
            .expect("dashboard test authority");
        let retained = Arc::clone(&authority.project_sessions);

        let selected = resolve_lcm_store(&cg, Some(Arc::clone(&retained))).await;

        assert!(Arc::ptr_eq(
            selected.lcm_db.as_ref().expect("retained LCM authority"),
            &retained,
        ));
        assert_eq!(selected.path, retained.db_path().display().to_string());
        assert_ne!(selected.scope, "global");
    }

    #[tokio::test]
    async fn daemon_dashboard_without_retained_authority_fails_closed() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-session-unavailable",
        )
        .await
        .expect("project init");
        let selected = resolve_lcm_store(&cg, None).await;

        // A display path is not read authority.
        assert!(selected.lcm_db.is_none());
        assert_eq!(selected.scope, "unavailable");
        assert_eq!(
            selected.path,
            cg.store_layout().sessions_db_path.display().to_string()
        );
    }

    #[tokio::test]
    async fn daemon_dashboard_without_retained_authority_is_read_only() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-session-read-only",
        )
        .await
        .expect("project init");
        let selected = resolve_lcm_store(&cg, None).await;

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
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-application-route",
        )
        .await
        .expect("project init");
        let state = build_state(&cg).await.expect("dashboard state");
        let project_id = state.project_id.clone().expect("active project id");
        let application = ActiveProjectApplicationRoutes {
            http_router: Router::new()
                .route("/probe", get(|| async { StatusCode::NO_CONTENT }))
                .route("/work/snapshot", post(|| async { StatusCode::NO_CONTENT }))
                .route(
                    "/work/attempt/start",
                    post(|| async { StatusCode::ACCEPTED }),
                ),
            dashboard_configuration_router: Router::new()
                .route("/probe", get(|| async { StatusCode::ACCEPTED })),
            dashboard_feedback_router: Router::new()
                .route("/probe", get(|| async { StatusCode::OK })),
            dashboard_work_router: Router::new()
                .route("/snapshot", post(|| async { StatusCode::NO_CONTENT })),
            executor: None,
        };
        let app = router_with_active_application(state, Some(application));

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
                    .uri("/api/work/snapshot")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("dashboard Work request"),
            )
            .await
            .expect("dashboard Work response");
        assert_eq!(work.status(), StatusCode::NO_CONTENT);

        let attempt = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/work/attempt/start")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("dashboard Work attempt request"),
            )
            .await
            .expect("dashboard Work attempt response");
        assert_eq!(attempt.status(), StatusCode::METHOD_NOT_ALLOWED);

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
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-read-model-route",
        )
        .await
        .expect("project init");
        let state = build_state(&cg).await.expect("dashboard state");
        let project_id = state.project_id.clone().expect("active project id");
        let app = router_with_active_application(state, None);

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

    #[test]
    fn a_selected_project_answers_feedback_and_work_reads_and_nothing_else_by_post() {
        for tail in ["feedback/get", "feedback/expand", "feedback/list"] {
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                Some(SelectedProjectApplicationRead::Feedback)
            );
            assert_eq!(selected_project_application_read(&Method::GET, tail), None);
        }
        for tail in ["work/snapshot", "work/delta"] {
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                Some(SelectedProjectApplicationRead::Work)
            );
            assert_eq!(selected_project_application_read(&Method::GET, tail), None);
        }

        // Every Work command, and every attempt operation, stays refused: a
        // selected project is read-only through this gateway.
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
            selected_project_application_read(&Method::POST, "doctor/remediations/apply"),
            None
        );
        assert_eq!(
            selected_project_application_read(&Method::POST, "feedback/status"),
            None
        );
    }
}
