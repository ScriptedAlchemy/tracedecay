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
    DashboardAutomationWriter, direct_dashboard_automation_writer,
};
mod automation_scheduler_api;
mod automation_skills_api;
mod code_diagnostics_api;
mod code_index_freshness_api;
mod doctor_findings_api;
mod events_api;
mod graph_api;
mod graph_queries;
mod graph_service;
mod lcm_api;
mod lcm_queries;
mod lcm_service;
mod memory_analysis;
mod memory_api;
pub mod memory_curate;
mod memory_service;
mod projects;
mod read_model;
mod savings_api;
mod savings_pricing;
mod settings_api;
mod storage_findings_api;
mod storage_telemetry_api;
mod token_count;
mod util;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, patch, post};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tower::ServiceExt;

use crate::application_surface::http_application_router;
use crate::automation::backend;
use crate::automation::config::{self, AutomationBackend, AutomationHostMode};
use crate::daemon::{DaemonHandshake, daemon_operation_event_authority};
use crate::daemon_client::DaemonInvocationClient;
use crate::db::Database;
use crate::diagnostics::lsp;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{GlobalDb, GlobalDbReadConnection};
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

#[derive(Clone)]
pub(crate) struct DashboardState {
    /// Registered project id for profile-backed stores, when known.
    pub(crate) project_id: Option<String>,
    /// Immutable authoritative owner for every memory operation served here.
    pub(crate) memory_owner: FactOwnerV1,
    /// Active code-graph database. This can be branch-specific.
    pub(crate) graph_conn: libsql::Connection,
    /// Keeps every project-database authority alive as long as cloned raw
    /// connections remain reachable through this state.
    pub(crate) _database_guards: Vec<Arc<Database>>,
    /// Display path of the active code-graph database.
    pub(crate) graph_db_path: String,
    /// Authoritative project-memory handle and process-local writer lane.
    pub(crate) mem_db: Arc<Database>,
    /// Display path of the project memory database.
    pub(crate) mem_db_path: String,
    /// LCM session store for the resolved active project store. Absent when
    /// project authority is unavailable; the accounting DB is never used.
    pub(crate) lcm_conn: Option<GlobalDbReadConnection>,
    /// Authoritative LCM store for serialized dashboard mutations. Retaining
    /// this authority also keeps `lcm_conn` alive.
    pub(crate) lcm_db: Option<Arc<GlobalDb>>,
    /// Display path of the LCM session store actually being served.
    pub(crate) lcm_db_path: String,
    /// Which store `lcm_conn` points at: project storage mode or `"unavailable"`.
    pub(crate) lcm_scope: String,
    /// Standalone CLI dashboards may open their resolved project session DB.
    /// Daemon-started dashboards inherit `false` and fail closed if a retained
    /// authority is unavailable, including for project-picker states.
    pub(crate) allow_direct_session_open: bool,
    /// Global accounting DB (savings ledger, lifetime counters, turns) used
    /// by the Savings & Cost tab, when available.
    pub(crate) savings_db: Option<Arc<GlobalDb>>,
    /// Display path of the global accounting DB.
    pub(crate) savings_db_path: String,
    pub(crate) project_root: PathBuf,
    /// Storage mode resolved for the active project store.
    pub(crate) storage_mode: String,
    /// Resolved active project store root.
    pub(crate) store_root: PathBuf,
    /// Resolved `config.json` path for the active project store.
    pub(crate) config_path: PathBuf,
    /// Resolved dashboard sidecar root inside the active project store.
    pub(crate) dashboard_root: PathBuf,
    /// Recent deterministic curation activity emitted by the standalone dashboard.
    pub(crate) curation_activity: Arc<RwLock<Vec<Value>>>,
    /// In-process BPE token-count cache for the Savings & Cost tab (backed
    /// by the `dashboard_token_counts` sidecar in the global accounting DB).
    pub(crate) token_counts: Arc<token_count::TokenCountCache>,
    /// Dashboard-owned LSP diagnostics broker. This is deliberately not
    /// exposed to hooks or model-context paths in Phase 1.
    pub(crate) code_diagnostics: Arc<RwLock<lsp::broker::DiagnosticBroker>>,
    /// Ensures the dashboard-opened idle backfill pass is scheduled once per
    /// dashboard server lifetime.
    pub(crate) code_diagnostics_backfill_started: Arc<AtomicBool>,
    pub(crate) automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    /// Lifetime-owning capability for complete dashboard automation writes.
    pub(crate) automation_writer: DashboardAutomationWriter,
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
    pub(crate) conn: Option<GlobalDbReadConnection>,
    pub(crate) lcm_db: Option<Arc<GlobalDb>>,
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
    retained_project_session_db: Option<Arc<GlobalDb>>,
    allow_direct_session_open: bool,
) -> LcmStoreSelection {
    let project_root = cg.project_root();
    if let Some(db) = retained_project_session_db {
        let conn = db.owned_read_connection().await;
        return LcmStoreSelection {
            conn,
            path: db.db_path().display().to_string(),
            lcm_db: Some(db),
            scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
        };
    }
    if !allow_direct_session_open {
        let project_db_path = cg.store_layout().sessions_db_path.clone();
        if let Some(db) = GlobalDb::open_read_only_at(&project_db_path).await {
            let conn = db.owned_read_connection().await;
            return LcmStoreSelection {
                conn,
                lcm_db: None,
                path: project_db_path.display().to_string(),
                scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
            };
        }
        return LcmStoreSelection {
            conn: None,
            lcm_db: None,
            path: project_db_path.display().to_string(),
            scope: "unavailable".to_string(),
        };
    }
    let Some(project_db_path) =
        crate::sessions::cursor::resolved_project_session_db_path(project_root).await
    else {
        return LcmStoreSelection {
            conn: None,
            lcm_db: None,
            path: cg.store_layout().sessions_db_path.display().to_string(),
            scope: "unavailable".to_string(),
        };
    };
    if let Some(db) = GlobalDb::open_at(&project_db_path).await {
        let conn = db.owned_read_connection().await;
        let db = Arc::new(db);
        return LcmStoreSelection {
            conn,
            lcm_db: Some(db),
            path: project_db_path.display().to_string(),
            scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
        };
    }
    LcmStoreSelection {
        conn: None,
        lcm_db: None,
        path: project_db_path.display().to_string(),
        scope: "unavailable".to_string(),
    }
}

pub(crate) fn storage_mode_label(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProjectLocal => "project_local",
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

pub(crate) fn code_diagnostics_broker(
    project_root: PathBuf,
    settings: lsp::settings::CodeDiagnosticsSettings,
) -> lsp::broker::DiagnosticBroker {
    let mut adapters = lsp::adapters::builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    lsp::broker::DiagnosticBroker::new(project_root, adapters, settings)
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
    retained_project_session_db: Option<Arc<GlobalDb>>,
    allow_direct_session_open: bool,
    warm_token_counts: bool,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
) -> Result<DashboardState> {
    let (mem_db_path, mem_db) = resolve_project_memory_store(cg);
    let memory_owner = project_memory_owner(cg)?;
    let lcm = resolve_lcm_store(
        cg,
        retained_project_session_db.clone(),
        allow_direct_session_open,
    )
    .await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let store_root = cg.store_layout().data_root.clone();
    let config_path = cg.store_layout().config_path.clone();
    let storage_mode = storage_mode_label(&cg.store_layout().storage_mode).to_string();
    let code_diagnostics_settings = lsp::settings::load_settings(&dashboard_root)
        .await
        .unwrap_or_default();
    let code_diagnostics =
        code_diagnostics_broker(cg.project_root().to_path_buf(), code_diagnostics_settings);
    let global_db_path = crate::global_db::global_db_path();
    let savings_db = if let Some(retained) =
        retained_project_session_db.filter(|db| global_db_path.as_deref() == Some(db.db_path()))
    {
        Some(retained)
    } else if allow_direct_session_open {
        GlobalDb::open().await.map(Arc::new)
    } else if let Some(path) = global_db_path.as_deref() {
        GlobalDb::open_read_only_at(path).await.map(Arc::new)
    } else {
        None
    };
    let savings_db_path = global_db_path
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let state = DashboardState {
        project_id: cg.store_layout().identity.project_id.clone(),
        memory_owner,
        graph_conn: cg.dashboard_connection(),
        _database_guards: vec![mem_db.clone()],
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_db,
        mem_db_path,
        lcm_conn: lcm.conn,
        lcm_db: lcm.lcm_db,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        allow_direct_session_open,
        savings_db,
        savings_db_path,
        project_root: cg.project_root().to_path_buf(),
        storage_mode,
        store_root,
        config_path,
        dashboard_root,
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics: Arc::new(RwLock::new(code_diagnostics)),
        code_diagnostics_backfill_started: Arc::new(AtomicBool::new(false)),
        automation_scheduler_reconciler,
        automation_writer,
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
        true,
        true,
        None,
        direct_dashboard_automation_writer(),
    )
    .await
}

pub(crate) async fn build_state_with_automation_reconciler(
    cg: &TraceDecay,
    retained_project_session_db: Option<Arc<GlobalDb>>,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
) -> Result<DashboardState> {
    build_state_inner(
        cg,
        retained_project_session_db,
        false,
        true,
        automation_scheduler_reconciler,
        automation_writer,
    )
    .await
}

/// Builds a lightweight cached state for a non-active project selected from the
/// dashboard project picker. Automation authority is inherited from the active
/// dashboard state so daemon-selected projects cannot fall back to direct open.
pub(crate) async fn build_selected_project_state(
    cg: &TraceDecay,
    active: &DashboardState,
) -> Result<DashboardState> {
    build_state_inner(
        cg,
        None,
        active.allow_direct_session_open,
        false,
        None,
        Arc::clone(&active.automation_writer),
    )
    .await
}

/// Detached catch-up ingest for transcript sources (Claude, Codex, Vibe,
/// Cline-like, and Cursor's historical backlog), mirroring the MCP serve
/// startup sweep so a standalone `tracedecay dashboard` reflects transcripts
/// written while no MCP server was running. Cursor's live turns still arrive
/// via hooks; the sweep shares their parse offsets so it only picks up
/// transcripts the hooks never saw. Fail-open and incremental
/// (`parse_offsets` makes repeats cheap no-ops).
fn spawn_session_catch_up_ingest(
    db: Arc<GlobalDb>,
    registry_db: Option<Arc<GlobalDb>>,
    project_root: PathBuf,
    project_id: Option<String>,
) {
    tokio::spawn(async move {
        let project_id = project_id
            .as_deref()
            .and_then(|id| tracedecay_domain::ProjectId::new(id).ok());
        let project_outcome = crate::sessions::ingest_project_sources_for_provider(
            db.as_ref(),
            &project_root,
            project_id,
            None,
            true,
        )
        .await;
        let mut stats = project_outcome.stats;
        if !project_outcome.is_success() {
            for failure in &project_outcome.failures {
                tracing::warn!(
                    provider = failure.provider,
                    source = failure.source,
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "dashboard project session catch-up incomplete"
                );
            }
        }
        if let Some(registry_db) = registry_db
            && let Ok(profile_root) = crate::storage::default_profile_root()
        {
            let user_db_path = crate::sessions::user_sessions_db_path(&profile_root);
            if db.db_path() == user_db_path {
                let outcome = crate::sessions::ingest_user_global_sources_for_startup_with_db(
                    db.as_ref(),
                    registry_db.as_ref(),
                    &profile_root,
                )
                .await;
                for failure in &outcome.failures {
                    tracing::warn!(
                        provider = failure.provider,
                        source = failure.source,
                        reason_code = failure.reason_code,
                        retryable = failure.retryable,
                        "dashboard user session catch-up incomplete"
                    );
                }
                stats = stats.merge(outcome.stats);
            } else {
                tracing::warn!(
                    user_db = %user_db_path.display(),
                    "dashboard user session catch-up skipped because retained authority is unavailable"
                );
            }
        }
        if stats.sessions_upserted > 0 || stats.messages_upserted > 0 {
            tracing::info!(
                sessions = stats.sessions_upserted,
                messages = stats.messages_upserted,
                "dashboard session catch-up completed"
            );
        }
    });
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
    run_until_shutdown_inner(cg, host, port, shutdown, DashboardRunOptions::test()).await
}

#[derive(Debug, Clone, Copy)]
struct DashboardRunOptions {
    open: bool,
    warm_token_counts: bool,
    start_session_catch_up: bool,
}

impl DashboardRunOptions {
    fn production(open: bool) -> Self {
        Self {
            open,
            warm_token_counts: true,
            start_session_catch_up: true,
        }
    }

    fn test() -> Self {
        Self {
            open: false,
            warm_token_counts: false,
            start_session_catch_up: false,
        }
    }
}

async fn run_until_shutdown_inner<F>(
    cg: &TraceDecay,
    host: &str,
    port: u16,
    shutdown: F,
    options: DashboardRunOptions,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let state = build_state_inner(
        cg,
        None,
        true,
        options.warm_token_counts,
        None,
        direct_dashboard_automation_writer(),
    )
    .await?;
    if options.start_session_catch_up {
        if let Some(db) = state.lcm_db.as_ref() {
            spawn_session_catch_up_ingest(
                Arc::clone(db),
                state.savings_db.clone(),
                state.project_root.clone(),
                state.project_id.clone(),
            );
        } else {
            tracing::warn!(
                project_root = %state.project_root.display(),
                "dashboard session catch-up skipped because authoritative project session storage is unavailable"
            );
        }
    }
    let app = router(cg, state).await?;
    let (listener, addr) = bind_dashboard(host, port).await?;

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

/// PR12 application routes are intentionally bound to the active project
/// daemon. They are not part of the selected-project dashboard gateway; PR14
/// must add explicit selected-project and Hermes adapters before that scope
/// can change.
struct ActiveProjectApplicationRoutes(Router);

impl ActiveProjectApplicationRoutes {
    fn for_active_project(cg: &TraceDecay) -> Result<Self> {
        let handshake = DaemonHandshake::for_current_client(
            Some(cg.project_root().to_path_buf()),
            None,
            false,
            false,
        )?;
        let client = DaemonInvocationClient::for_current(handshake)?;
        let active_project_id = match project_memory_owner(cg)? {
            FactOwnerV1::Project { project_id } => project_id,
            FactOwnerV1::Profile => {
                return Err(config_error(
                    "active-project application routes require project authority",
                ));
            }
        };
        let router = http_application_router(
            client,
            daemon_operation_event_authority(),
            active_project_id,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not mount application HTTP routes: {error}"),
        })?;
        Ok(Self(router))
    }
}

/// Builds the complete dashboard router shared by direct and daemon-managed
/// startup. The supplied state is the active writable project authority.
pub(crate) async fn router(cg: &TraceDecay, state: DashboardState) -> Result<Router> {
    // Fact writes defer derived memory rebuilds. Invoke the canonical bounded
    // convergence policy exactly once for the active writable project before
    // serving either startup path. Selected-project states are opened later
    // through the read-only gateway and never pass through this function.
    match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => {
            if let Err(error) = application
                .converge_derived_memory("dashboard-startup-repair")
                .await
            {
                tracing::warn!("Derived memory startup repair skipped: {error}");
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
    let application = match ActiveProjectApplicationRoutes::for_active_project(cg) {
        Ok(application) => Some(application),
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
        .route("/", get(assets::index_html))
        .route("/shell/{file}", get(assets::shell_asset))
        .route(
            "/dashboard-plugins/{plugin}/dist/{file}",
            get(assets::plugin_asset),
        )
        .route("/api/dashboard/plugins", get(plugins_list))
        .route("/api/projects", get(projects::list))
        .route("/api/projects/{project_id}", get(projects::context))
        .route(
            "/api/projects/{project_id}/{*tail}",
            any(project_scoped_api_gateway),
        )
        .route("/api/capabilities", any(active_api_gateway))
        .route("/api/plugins/{*tail}", any(active_api_gateway))
        .route("/api/automation/{*tail}", any(active_api_gateway))
        .route("/api/settings", any(active_api_gateway))
        .route("/api/settings/{*tail}", any(active_api_gateway))
        // PR14 V2 read-model surfaces bound through the active-project gateway,
        // mirroring the project-scoped `/api/projects/{id}/…` gateway path.
        .route("/api/doctor/{*tail}", any(active_api_gateway))
        .route("/api/storage/{*tail}", any(active_api_gateway))
        .route("/api/code-index/{*tail}", any(active_api_gateway))
        .route("/api/events", any(active_api_gateway))
        .with_state(runtime);
    match application {
        Some(application) => router.nest("/api/application", application.0),
        None => router,
    }
}

fn project_api_router() -> Router<DashboardState> {
    Router::new()
        .route("/api/capabilities", get(capabilities))
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
        // Durable analytics API (hint lifecycle scaffolds + session usage rollups)
        .route(
            "/api/plugins/analytics/overview",
            get(analytics_api::overview),
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
        // PR14 V2 read-model surfaces (DashboardEnvelope<T>). Doctor finding
        // family, plan-38 storage telemetry/findings, code-index freshness, and
        // the typed SSE stream. See `read_model` for the normative envelope.
        .route("/api/doctor/findings", get(doctor_findings_api::findings))
        .route(
            "/api/storage/telemetry",
            get(storage_telemetry_api::telemetry),
        )
        .route("/api/storage/findings", get(storage_findings_api::findings))
        .route(
            "/api/code-index/freshness",
            get(code_index_freshness_api::freshness),
        )
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
    if runtime.active_project_id() != Some(project_id.as_str())
        && !matches!(req.method(), &Method::GET | &Method::HEAD)
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
    let has_lcm = state.lcm_conn.is_some();
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
    Json(json!({
        "name": "tracedecay-dashboard",
        "version": env!("CARGO_PKG_VERSION"),
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
        "features": {
            "memory": true,
            "lcm": has_lcm,
            "lcm_gc": has_lcm,
            "lcm_payload_health": has_lcm,
            "graph": true,
            "analytics": true,
            "code_diagnostics": true,
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
        },
        "automation": {
            "enabled": automation.enabled,
            "mode": automation_mode,
            "backend": automation_backend,
            "host_mode": automation_host_mode,
            "availability": backend_availability,
        },
        "dashboards": assets::DASHBOARD_PLUGINS
            .iter()
            .map(|plugin| plugin.name)
            .collect::<Vec<_>>(),
    }))
}

/// Plugin manifest list, mirroring the Hermes `/api/dashboard/plugins`
/// endpoint shape closely enough for the standalone shell.
async fn plugins_list() -> Json<Value> {
    Json(json!(
        assets::DASHBOARD_PLUGINS
            .iter()
            .map(|plugin| {
                json!({
                    "name": plugin.name,
                    "label": plugin.label,
                    "description": plugin.description,
                    "icon": plugin.icon,
                    "entry": "dist/index.js",
                    "css": "dist/style.css",
                    "has_api": true,
                    "source": "tracedecay",
                })
            })
            .collect::<Vec<_>>()
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod authority_tests {
    use super::*;

    #[tokio::test]
    async fn project_memory_owner_uses_validated_store_identity() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
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
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");

        let expected_path = cg.dashboard_db_path().display().to_string();
        let state = build_state(&cg).await.expect("dashboard state");

        assert_eq!(state.mem_db_path, expected_path);
        assert_eq!(state._database_guards.len(), 1);
    }

    #[tokio::test]
    async fn retained_project_session_authority_is_reused_exactly() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let retained = Arc::new(
            GlobalDb::open_at(&cg.store_layout().sessions_db_path)
                .await
                .expect("project session DB"),
        );

        let selected = resolve_lcm_store(&cg, Some(Arc::clone(&retained)), false).await;

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
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let db_path = cg.store_layout().sessions_db_path.clone();
        // Configuration authority opens sessions.db during init.
        assert!(
            db_path.exists(),
            "init must open configuration authority sessions.db"
        );

        let selected = resolve_lcm_store(&cg, None, false).await;

        // Without retained authority the dashboard may open read-only, but must
        // not promote a writable LCM handle.
        assert!(selected.conn.is_some());
        assert!(selected.lcm_db.is_none());
        assert_ne!(selected.scope, "unavailable");
    }

    #[tokio::test]
    async fn daemon_dashboard_without_retained_authority_is_read_only() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let db_path = cg.store_layout().sessions_db_path.clone();
        let writable = GlobalDb::open_at(&db_path).await.expect("session DB");
        drop(writable);

        let selected = resolve_lcm_store(&cg, None, false).await;

        assert!(selected.conn.is_some());
        assert!(selected.lcm_db.is_none());
        assert_ne!(selected.scope, "unavailable");
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
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let state = build_state(&cg).await.expect("dashboard state");
        let project_id = state.project_id.clone().expect("active project id");
        let application = ActiveProjectApplicationRoutes(
            Router::new().route("/probe", get(|| async { StatusCode::NO_CONTENT })),
        );
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
        let cg = TraceDecay::init(project.path())
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
}
