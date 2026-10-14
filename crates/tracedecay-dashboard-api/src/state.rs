//! Retained dashboard authorities and exact-project state composition.
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_automation_runtime::automation::host_io::HostIo;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader;
use tracedecay_contracts::doctor::{DoctorReportV1, LanguageServerReadV1};
use tracedecay_contracts::storage::{SchemaConvergenceFindingV1, TableGrowthDoctorEvidenceV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::{
    Database, DatabaseEngineReadConnection, DatabaseStorageTelemetryHandle,
};
use tracedecay_runtime_core::storage::{StorageMode, StoreLayout};

use crate::{
    DashboardApplicationRuntime, DashboardAutomationAuthorityV1, DashboardAutomationWriter,
    DashboardDeliveryReadPortV1, DashboardGitCorrelationReadPortV1, DashboardLcmReadPortV1,
    DashboardProfileCodeIndexWorkerSettingsPort, DashboardProjectContext,
    DashboardSessionAuthoritiesV1, DashboardSessionAuthorityStateV1, DashboardSessionMountV1,
    DashboardSessionResolutionV1, code_read_api, events_api, events_delivery, feedback_api, scope,
    settings_api, snapshot_cache, token_count,
};

pub type AutomationSchedulerReconcileFuture = Pin<
    Box<
        dyn Future<Output = tracedecay_contracts::retrieval::AutomationSchedulerReconcileOutcome>
            + Send
            + 'static,
    >,
>;
pub type AutomationSchedulerReconciler =
    Arc<dyn Fn() -> AutomationSchedulerReconcileFuture + Send + Sync + 'static>;
pub type DashboardAutomationObservationRecorderV1 = Arc<
    dyn Fn(tracedecay_automation_runtime::automation::run_ledger::AutomationRunLedgerRecord)
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
pub type DoctorReportReadFuture = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    AdmittedDoctorReportV1,
                    tracedecay_contracts::ApplicationContractError,
                >,
            > + Send
            + 'static,
    >,
>;
pub type DoctorReportReader = Arc<dyn Fn() -> DoctorReportReadFuture + Send + Sync + 'static>;
pub type RemoteOperationalStatusReader = tracedecay_contracts::RemoteOperationalStatusReaderV1;

/// Runtime authorities retained by one daemon-managed dashboard state.
///
/// This is deliberately not `Default`: the composition root must explicitly
/// choose every optional authority and the automation writer for each state.
#[derive(Clone)]
pub struct DashboardStateCompositionV1 {
    /// The owning binary's composed build version. The dashboard crate's own
    /// package version is an implementation detail; the composition root
    /// passes this from its registered product runtime so every version
    /// surface reports the shipped product build.
    pub build_version: &'static str,
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
    /// Exact-project shared-family and revision-pair reads composed from the
    /// daemon's verified code-index authorities.
    pub code_read_authority: Option<code_read_api::DashboardCodeReadAuthorityV1>,
    /// The project's session store with its LCM and Git-correlation reads,
    /// or the resolver that mounts them once a still-opening project admits
    /// its session store.
    pub project_sessions: DashboardSessionMountV1,
    /// Exact ProfileSessions read/mutation capability for the daemon-wide
    /// code-index worker preference. This never aliases the project settings
    /// control plane: it has its own profile revision and CAS boundary.
    pub profile_code_index_worker_settings:
        Option<Arc<dyn DashboardProfileCodeIndexWorkerSettingsPort>>,
    /// Daemon-wide Delivery projection over exact registered project targets.
    /// The adapter owns application admission and provider/store access; HTTP
    /// receives only bounded typed source outcomes.
    pub delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    pub registered_savings_db: Option<RegisteredGlobalDbLeaseV1>,
    /// Exact daemon-selected profile plus its canonical automation run and
    /// managed-skill materialization capabilities. Standalone states leave it
    /// absent and automation mutation routes report typed unavailable.
    pub automation_authority: Option<DashboardAutomationAuthorityV1>,
    pub automation_observation: Option<
        Arc<dyn Fn(PathBuf) -> DashboardAutomationObservationFuture + Send + Sync + 'static>,
    >,
    pub automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    pub automation_writer: DashboardAutomationWriter,
    pub doctor_report_reader: Option<DoctorReportReader>,
    /// Daemon-owned Remote Brain operational read. Standalone dashboards leave
    /// it absent and `GET /api/remote/status` reports typed unavailable.
    pub remote_operational_status_reader: Option<RemoteOperationalStatusReader>,
    pub code_index_freshness_reader: Option<CodeIndexFreshnessReader>,
    pub feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    /// Root-addressed read over the daemon-owned PR-autotrack state sidecar.
    /// Selected projects reuse the resolver but resolve their own exact store
    /// root on every call.
    pub pr_autotrack_reader: Option<settings_api::PrAutoTrackManagedSummaryReader>,
    pub code_diagnostics_broker:
        Option<Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>>,
    pub application_invocation_executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    /// Daemon-owned canonical authority for browser-confirmed SSE delivery.
    /// Standalone dashboards leave this absent and emit no receipt token.
    pub delivery_settlement_authority:
        Option<Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>>,
}

#[derive(Clone)]
pub struct AdmittedDoctorReportV1 {
    pub report: DoctorReportV1,
    pub table_growth_evidence: Vec<TableGrowthDoctorEvidenceV1>,
    pub schema_convergences: Vec<SchemaConvergenceFindingV1>,
    /// The daemon owner's per-analyzer read behind the `LanguageServer`
    /// finding, so `lsp servers` lists exactly what Doctor graded.
    pub language_servers: LanguageServerReadV1,
}

impl AdmittedDoctorReportV1 {
    pub fn new(report: DoctorReportV1) -> Self {
        Self {
            report,
            table_growth_evidence: Vec::new(),
            schema_convergences: Vec::new(),
            language_servers: LanguageServerReadV1::Unknown,
        }
    }

    pub fn with_language_servers(mut self, read: LanguageServerReadV1) -> Self {
        self.language_servers = read;
        self
    }

    pub fn with_table_growth_evidence(
        mut self,
        evidence: Vec<TableGrowthDoctorEvidenceV1>,
    ) -> Self {
        self.table_growth_evidence = evidence;
        self
    }

    pub fn with_schema_convergences(mut self, findings: Vec<SchemaConvergenceFindingV1>) -> Self {
        self.schema_convergences = findings;
        self
    }
}

/// Intentionally has no `Default` or defaulting builder. Every state variant
/// must explicitly provide its mode-specific database and settings authorities;
/// silently defaulting either could route dashboard operations to the wrong
/// project or user profile.
#[derive(Clone)]
pub struct DashboardState {
    /// The owning binary's composed build version, from the composition.
    pub build_version: &'static str,
    /// Host-install I/O from the project runtime; analytics reads the embedded
    /// managed-agent bundle through it to label subagent sessions.
    pub host_io: HostIo,
    /// Registered project id for profile-backed stores, when known.
    pub project_id: Option<String>,
    /// Exact application scope resolved ONCE when this state was constructed.
    /// `None` is the explicit fail-closed state (missing registry, invalid
    /// project id, or unresolvable exact root): handlers report their typed
    /// unavailable states from it and never re-resolve scope from paths or
    /// the CWD per request.
    pub resolved_scope: Option<tracedecay_contracts::ResolvedScope>,
    /// Canonical per-request admission for the verified code graph.
    pub code_graph_read_admission: Option<Arc<dyn crate::graph::CodeGraphReadAdmissionPort>>,
    /// Canonical exact-project verified projection resolver.
    pub code_graph_projection_read_port: Option<Arc<dyn crate::graph::CodeGraphProjectionReadPort>>,
    /// Canonical verified shared-family and revision-pair read authority.
    pub code_read_authority: Option<code_read_api::DashboardCodeReadAuthorityV1>,
    /// Exact project graph retained by the daemon for this dashboard state.
    /// Absent for lightweight/profile-only states that cannot run project
    /// automation.
    pub project_graph: Option<Arc<DashboardProjectContext>>,
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
    /// Whether the session authorities above are mounted, still opening, or
    /// unavailable for this state.
    pub session_authority: DashboardSessionAuthorityStateV1,
    /// Present only while [`Self::session_authority`] is opening; the daemon
    /// sends the project's resolution here once its publication finishes.
    pub(crate) session_mount: Option<tokio::sync::watch::Receiver<DashboardSessionResolutionV1>>,
    /// Daemon-owned canonical session retrieval authority used by LCM browse
    /// routes. Those routes never retain or open a session database.
    pub lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    /// Daemon-owned typed read over the session Git evidence rows, serving
    /// Loom's session↔commit and branch/worktree sources.
    pub git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
    /// Daemon-wide Delivery projection over exact registered project targets.
    pub delivery_read_authority: Option<Arc<dyn DashboardDeliveryReadPortV1>>,
    /// Global accounting DB for the savings ledger and lifetime counters used
    /// by the Savings & Cost tab. Provider usage lives in the retained project
    /// session store exposed separately through `lcm_db`.
    pub savings_db: Option<RegisteredGlobalDbLeaseV1>,
    /// Display path of the global accounting DB.
    pub savings_db_path: String,
    pub project_root: PathBuf,
    /// Live read port over the daemon-owned code-index scheduler registry.
    pub code_index_freshness_reader: Option<CodeIndexFreshnessReader>,
    /// Root-addressed read over the daemon-mounted canonical feedback
    /// observation owner. Selected projects reuse the resolver but resolve
    /// their own exact project root on every call.
    pub feedback_status_reader: Option<feedback_api::FeedbackStatusReader>,
    /// Daemon-owned read over PR-autotrack managed branches for settings.
    pub pr_autotrack_reader: Option<settings_api::PrAutoTrackManagedSummaryReader>,
    /// Storage mode resolved for the active project store.
    pub storage_mode: String,
    /// Resolved active project store root.
    pub store_root: PathBuf,
    /// Data directory of the owning profile, home of profile-wide files.
    pub profile_root: PathBuf,
    /// Resolved dashboard sidecar root inside the active project store.
    pub dashboard_root: PathBuf,
    /// Retention policy resolved with the owning runtime configuration.
    /// Dashboard reads must not re-open mutable config input per request.
    pub retention_config: tracedecay_configuration::RetentionConfig,
    /// Daemon-owned user-profile settings authority. Dashboard routes never
    /// load or mutate `config.toml` directly.
    pub user_settings: Arc<dyn tracedecay_configuration::UserSettingsDaemonClient>,
    /// Root-injected ProfileSessions worker preference authority. It remains
    /// separate from `user_settings`, whose revision belongs to the ordinary
    /// profile settings resource.
    pub profile_code_index_worker_settings:
        Option<Arc<dyn DashboardProfileCodeIndexWorkerSettingsPort>>,
    /// Process-local derived BPE token-count cache for the Savings & Cost tab.
    pub token_counts: Arc<token_count::TokenCountCache>,
    /// Derived snapshots (PCA projection, similarity pairs, dependency strata)
    /// computed from this state's own stores. Owned here, shared by clones,
    /// and released with the state instead of accumulating process-wide.
    pub(crate) derived_snapshots: Arc<snapshot_cache::DerivedSnapshotCaches>,
    /// Admitted daemon/application diagnostics authority. `None` keeps all
    /// diagnostics controls typed unavailable; the dashboard never constructs
    /// a broker or analyzer runtime.
    pub code_diagnostics_authority:
        Option<crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1>,
    /// Daemon-selected profile and canonical automation mutation authority.
    /// HTTP handlers never reconstruct this capability from the environment.
    pub automation_authority: Option<DashboardAutomationAuthorityV1>,
    pub automation_observation: Option<
        Arc<dyn Fn(PathBuf) -> DashboardAutomationObservationFuture + Send + Sync + 'static>,
    >,
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
    /// Shared registry/storage source poll for `/api/events` streams. Clones
    /// share one poller; the first stream starts it.
    pub event_source_poll: events_api::SharedSourcePoll,
}

impl DashboardState {
    /// The configuration this dashboard's project store published. The store
    /// is its owning profile's shard, which names whose pin to read.
    pub(crate) fn cached_runtime_configuration(
        &self,
    ) -> Result<crate::config::PinnedRuntimeConfiguration> {
        let profile_root = self
            .project_id
            .as_deref()
            .and_then(|project_id| {
                tracedecay_runtime_core::storage::profile_root_of_sharded_data_root(
                    &self.store_root,
                    project_id,
                )
            })
            .ok_or_else(|| config_error("dashboard project store is not a profile shard"))?;
        crate::config::cached_runtime_configuration(profile_root, &self.project_root)
    }

    pub fn reconcile_automation_scheduler(&self) {
        if let Some(reconcile) = &self.automation_scheduler_reconciler {
            let reconcile = Arc::clone(reconcile);
            // Stays on Tokio: the daemon's reconcile starts the scheduler
            // loop with `tokio::spawn` (`start_automation_scheduler`).
            tokio::spawn(async move {
                let _ = reconcile().await;
            });
        }
    }

    pub(crate) fn mount_session_authorities(&mut self, authorities: DashboardSessionAuthoritiesV1) {
        self.lcm_db_path = authorities.project_sessions.db_path().display().to_string();
        self.lcm_db = Some(authorities.project_sessions);
        self.lcm_scope.clone_from(&self.storage_mode);
        self.lcm_read_authority = authorities.lcm_read_authority;
        self.git_correlation_read_authority = authorities.git_correlation_read_authority;
        self.session_authority = DashboardSessionAuthorityStateV1::Ready;
        self.session_mount = None;
    }
}

/// The retained session store for the dashboard's session-backed routes.
pub(super) struct LcmStoreSelection {
    pub(super) lcm_db: Option<RegisteredGlobalDbLeaseV1>,
    pub(super) path: String,
    pub(super) scope: String,
}

/// Selects the registered project session store, failing closed to an
/// unavailable selection when the daemon retained none.
pub(super) fn resolve_lcm_store_for_layout(
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
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

/// Resolves the immutable fact owner from the validated project layout.
///
/// Dashboard routes must never infer ownership from a path, label, or
/// optional display field after construction.
pub fn project_memory_owner(cg: &DashboardProjectContext) -> Result<FactOwnerV1> {
    project_memory_owner_for_layout(&cg.store_layout)
}

pub(super) fn project_memory_owner_for_layout(layout: &StoreLayout) -> Result<FactOwnerV1> {
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

pub(super) async fn build_state_inner(
    cg: &DashboardProjectContext,
    project_graph: Option<Arc<DashboardProjectContext>>,
    warm_token_counts: bool,
    composition: DashboardStateCompositionV1,
) -> Result<DashboardState> {
    let DashboardStateCompositionV1 {
        build_version,
        project_graph_resolver,
        code_graph_read_admission,
        code_graph_projection_read_port,
        code_read_authority,
        project_sessions,
        profile_code_index_worker_settings,
        delivery_read_authority,
        registered_savings_db,
        automation_authority,
        automation_observation,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader,
        remote_operational_status_reader,
        code_index_freshness_reader,
        feedback_status_reader,
        pr_autotrack_reader,
        code_diagnostics_broker,
        application_invocation_executor,
        delivery_settlement_authority,
    } = composition;
    let mem_db_path = cg.dashboard_db_path.display().to_string();
    let mem_db = Arc::clone(&cg.dashboard_database);
    let memory_owner = project_memory_owner(cg)?;
    let (session_authorities, session_authority, session_mount) = match project_sessions {
        DashboardSessionMountV1::Ready(authorities) => (
            Some(authorities),
            DashboardSessionAuthorityStateV1::Ready,
            None,
        ),
        DashboardSessionMountV1::Opening(publication) => (
            None,
            DashboardSessionAuthorityStateV1::Opening,
            Some(publication),
        ),
        DashboardSessionMountV1::Unavailable => {
            (None, DashboardSessionAuthorityStateV1::Unavailable, None)
        }
    };
    let (lcm_read_authority, git_correlation_read_authority) =
        session_authorities
            .as_ref()
            .map_or((None, None), |authorities| {
                (
                    authorities.lcm_read_authority.clone(),
                    authorities.git_correlation_read_authority.clone(),
                )
            });
    let lcm = resolve_lcm_store_for_layout(
        &cg.store_layout,
        session_authorities.map(|authorities| authorities.project_sessions),
    );
    let dashboard_root = cg.store_layout.dashboard_root.clone();
    let store_root = cg.store_layout.data_root.clone();
    let storage_mode = storage_mode_label(&cg.store_layout.storage_mode).to_string();
    let code_diagnostics_authority = match (
        code_diagnostics_broker,
        code_graph_read_admission.as_ref(),
        code_graph_projection_read_port.as_ref(),
    ) {
        (Some(broker), Some(graph_admission), Some(graph_projection)) => Some(
            crate::application::dashboard_diagnostics::DashboardDiagnosticsAuthorityV1::new(
                cg.store_layout.project_root.clone(),
                dashboard_root.clone(),
                Arc::clone(graph_admission),
                Arc::clone(graph_projection),
                broker,
            ),
        ),
        _ => None,
    };
    let savings_db_path = registered_savings_db.as_ref().map_or_else(
        || cg.profile.global_db_path().display().to_string(),
        |db| db.db_path().display().to_string(),
    );
    let delivery_settlements = Arc::new(
        events_delivery::DashboardDeliverySettlementRegistryV1::new(delivery_settlement_authority),
    );
    events_delivery::DashboardDeliverySettlementRegistryV1::mount_deadline_reaper(
        &delivery_settlements,
    )
    .map_err(|error| {
        config_error(format!(
            "dashboard delivery deadline reaper did not start: {error}"
        ))
    })?;
    let state = DashboardState {
        build_version,
        host_io: cg.host_io,
        project_id: cg.store_layout.identity.project_id.clone(),
        resolved_scope: scope::resolve_dashboard_scope(
            &cg.store_layout.project_root,
            cg.store_layout.identity.project_id.as_deref(),
        ),
        code_graph_read_admission,
        code_graph_projection_read_port,
        code_read_authority,
        project_graph,
        project_graph_resolver,
        memory_owner,
        graph_conn: mem_db.read_connection(),
        _database_guards: vec![mem_db.clone()],
        graph_telemetry_handle: cg.dashboard_database.storage_telemetry_handle().ok(),
        graph_db_path: cg.dashboard_db_path.display().to_string(),
        mem_db,
        mem_db_path,
        lcm_db: lcm.lcm_db,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        session_authority,
        session_mount,
        lcm_read_authority,
        git_correlation_read_authority,
        delivery_read_authority,
        savings_db: registered_savings_db,
        savings_db_path,
        project_root: cg.store_layout.project_root.clone(),
        code_index_freshness_reader,
        feedback_status_reader,
        pr_autotrack_reader,
        storage_mode,
        store_root,
        profile_root: cg.profile.data_dir().to_path_buf(),
        dashboard_root,
        retention_config: cg.retention_config.clone(),
        user_settings: Arc::clone(&cg.user_settings_client),
        profile_code_index_worker_settings,
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        derived_snapshots: Arc::new(snapshot_cache::DerivedSnapshotCaches::new()),
        code_diagnostics_authority,
        automation_authority,
        automation_observation,
        automation_scheduler_reconciler,
        automation_writer,
        doctor_report_reader,
        remote_operational_status_reader,
        application_invocation_executor,
        delivery_settlements,
        event_source_poll: events_api::SharedSourcePoll::default(),
    };
    // Pre-count non-usage messages in the background so the first Savings
    // tab paint doesn't pay the initial BPE pass over the session store.
    if warm_token_counts {
        token_count::spawn_warm(&state);
    }
    Ok(state)
}

pub async fn build_state_with_automation_reconciler(
    cg: Arc<DashboardProjectContext>,
    composition: DashboardStateCompositionV1,
) -> Result<DashboardState> {
    build_state_inner(cg.as_ref(), Some(Arc::clone(&cg)), true, composition).await
}

/// Builds a lightweight cached state for a non-active project selected from the
/// dashboard project picker. Automation authority is inherited from the active
/// dashboard state so daemon-selected projects cannot fall back to direct open.
pub async fn build_selected_project_state(
    cg: Arc<DashboardProjectContext>,
    active: &DashboardState,
) -> Result<DashboardState> {
    build_state_inner(
        cg.as_ref(),
        Some(Arc::clone(&cg)),
        false,
        DashboardStateCompositionV1 {
            build_version: active.build_version,
            project_graph_resolver: active.project_graph_resolver.clone(),
            // Both verified code-graph ports are exact-project authorities;
            // the active project's admission must never be reused for a
            // selected project.
            code_graph_read_admission: None,
            code_graph_projection_read_port: None,
            code_read_authority: None,
            project_sessions: DashboardSessionMountV1::Unavailable,
            // This capability is profile-global and its route is deliberately
            // unscoped, so selected projects reuse the active dashboard's
            // exact ProfileSessions authority. It is not a project write.
            profile_code_index_worker_settings: active.profile_code_index_worker_settings.clone(),
            delivery_read_authority: active.delivery_read_authority.clone(),
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
            feedback_status_reader: active.feedback_status_reader.clone(),
            pr_autotrack_reader: active.pr_autotrack_reader.clone(),
            code_diagnostics_broker: None,
            // Rebinding an application transport is required only for the
            // selected project's application routes. Ordinary read routes
            // must not fail because that independent authority is absent.
            application_invocation_executor: None,
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
