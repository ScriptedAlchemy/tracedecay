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
//!   global-DB fallback)
//!
//! The endpoint paths and JSON payload shapes intentionally mirror the
//! original Hermes plugin APIs (`plugins/memory/holographic_plus/dashboard/
//! plugin_api.py` and the hermes-lcm `dashboard/plugin_api.py`) so the plugin
//! bundles run unmodified under both hosts. The Hermes-side wrapper plugin
//! reverse-proxies to this server, making this the canonical implementation.
//!
//! `/api/capabilities` advertises which features are live so hosts (or a
//! richer Hermes wrapper) can extend the surface without forking the UI.

mod analytics_api;
pub(crate) mod assets;
mod automation_config_api;
mod automation_fact_proposals_api;
mod automation_run_api;
mod automation_run_service;
mod automation_scheduler_api;
mod automation_skills_api;
mod code_diagnostics_api;
mod curate_preview_store;
mod graph_api;
mod graph_queries;
mod graph_service;
mod lcm_api;
mod lcm_queries;
mod lcm_service;
mod memory_analysis;
mod memory_api;
pub mod memory_curate;
mod memory_queries;
mod memory_service;
mod projects;
mod savings_api;
mod savings_pricing;
mod token_count;
mod util;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::ServiceExt;

use crate::automation::backend;
use crate::automation::config::{self, AutomationBackend, AutomationHostMode};
use crate::db::Database;
use crate::diagnostics::lsp;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::storage::StorageMode;
use crate::tracedecay::TraceDecay;

/// Default port for `tracedecay dashboard` (chosen to avoid common dev-server
/// defaults; override with `--port`).
pub const DEFAULT_PORT: u16 = 7341;

pub(crate) type CuratePreviewFingerprint = (i64, i64, i64, i64);

/// Cached last curation preview, written by `POST /curate?dry_run=true`.
#[derive(Debug, Clone)]
pub(crate) struct CuratePreviewEntry {
    pub(crate) report: Value,
    /// ISO 8601 UTC timestamp when this preview was saved.
    pub(crate) saved_at: String,
    /// Active fact count at the time the preview was generated (for stale detection).
    pub(crate) active_facts_at_save: i64,
    /// `(active count, max updated_at, sum fact_id, sum updated_at)` at preview generation.
    pub(crate) memory_fingerprint_at_save: CuratePreviewFingerprint,
}

#[derive(Clone)]
pub(crate) struct DashboardState {
    /// Registered project id for profile-backed stores, when known.
    pub(crate) project_id: Option<String>,
    /// Active code-graph database. This can be branch-specific.
    pub(crate) graph_conn: libsql::Connection,
    /// Display path of the active code-graph database.
    pub(crate) graph_db_path: String,
    /// Project memory database. This is shared across branches.
    pub(crate) mem_conn: libsql::Connection,
    /// Display path of the project memory database.
    pub(crate) mem_db_path: String,
    /// LCM session store for the resolved active project store, or the global
    /// fallback when no project store is available.
    pub(crate) lcm_conn: Option<libsql::Connection>,
    /// Display path of the LCM session store actually being served.
    pub(crate) lcm_db_path: String,
    /// Which store `lcm_conn` points at, e.g. `"profile_sharded"` or `"global"`.
    pub(crate) lcm_scope: String,
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
    /// Resolved dashboard sidecar root inside the active project store.
    pub(crate) dashboard_root: PathBuf,
    /// Last saved dry-run curation preview (shared across all clones of the state).
    pub(crate) curate_preview: Arc<RwLock<Option<CuratePreviewEntry>>>,
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
}

/// The LCM session store the dashboard will serve.
pub(crate) struct LcmStoreSelection {
    pub(crate) conn: Option<libsql::Connection>,
    pub(crate) path: String,
    pub(crate) scope: String,
}

/// Selects the LCM session store for the resolved active project store.
///
/// Transcript ingest writes to the active code-project store selected by the
/// storage resolver. For profile-backed projects, that is the user-level shard
/// under `~/.tracedecay/projects/<project_id>/`, not a repo-local DB.
///
/// The global DB is only a fallback for sessions. `TRACEDECAY_GLOBAL_DB`
/// still controls the savings/accounting ledger, but it must not pull the
/// dashboard away from the resolved active project store transcript ingest uses.
pub(crate) async fn resolve_lcm_store(cg: &TraceDecay) -> LcmStoreSelection {
    let project_root = cg.project_root();
    if let Some(project_db_path) =
        crate::sessions::cursor::resolved_project_session_db_path(project_root).await
    {
        if let Some(db) = GlobalDb::open_at(&project_db_path).await {
            return LcmStoreSelection {
                conn: Some(db.dashboard_connection()),
                path: project_db_path.display().to_string(),
                scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
            };
        }
    }
    let global = GlobalDb::open().await;
    LcmStoreSelection {
        conn: global.as_ref().map(GlobalDb::dashboard_connection),
        path: crate::global_db::global_db_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        scope: "global".to_string(),
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

async fn open_dashboard_connection(path: &Path) -> Option<libsql::Connection> {
    let (db, _) = Database::open(path).await.ok()?;
    Some(db.conn().clone())
}

async fn memory_fact_count(conn: &libsql::Connection) -> Option<i64> {
    let mut rows = conn
        .query("SELECT COUNT(*) FROM memory_facts", ())
        .await
        .ok()?;
    rows.next().await.ok()??.get::<i64>(0).ok()
}

pub(crate) async fn resolve_project_memory_store(cg: &TraceDecay) -> (libsql::Connection, String) {
    let graph_path = cg.dashboard_db_path();
    let mut first_open: Option<(libsql::Connection, String)> = None;
    let mut seen = std::collections::BTreeSet::new();

    for path in [cg.store_layout().graph_db_path.clone()] {
        if !seen.insert(path.clone()) || !path.is_file() {
            continue;
        }
        let conn = if path == graph_path {
            Some(cg.dashboard_connection())
        } else {
            open_dashboard_connection(&path).await
        };
        let Some(conn) = conn else {
            continue;
        };
        let display_path = path.display().to_string();
        if first_open.is_none() {
            first_open = Some((conn.clone(), display_path.clone()));
        }
        if memory_fact_count(&conn).await.unwrap_or(0) > 0 {
            return (conn, display_path);
        }
    }

    first_open.unwrap_or_else(|| {
        (
            cg.dashboard_connection(),
            cg.dashboard_db_path().display().to_string(),
        )
    })
}

async fn build_state_inner(
    cg: &TraceDecay,
    repair_memory_on_startup: bool,
    warm_token_counts: bool,
) -> DashboardState {
    let (mem_conn, mem_db_path) = resolve_project_memory_store(cg).await;
    let lcm = resolve_lcm_store(cg).await;
    // Re-hydrate the last dry-run curation preview from its sidecar so it
    // survives server restarts (staleness is recomputed on read anyway).
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let store_root = cg.store_layout().data_root.clone();
    let storage_mode = storage_mode_label(&cg.store_layout().storage_mode).to_string();
    let persisted_preview = curate_preview_store::load(&dashboard_root).await;
    let code_diagnostics_settings = lsp::settings::load_settings(&dashboard_root)
        .await
        .unwrap_or_default();
    let code_diagnostics =
        code_diagnostics_broker(cg.project_root().to_path_buf(), code_diagnostics_settings);
    let savings_db = GlobalDb::open().await.map(Arc::new);
    let savings_db_path = crate::global_db::global_db_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let state = DashboardState {
        project_id: cg.store_layout().identity.project_id.clone(),
        graph_conn: cg.dashboard_connection(),
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_conn,
        mem_db_path,
        lcm_conn: lcm.conn,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        savings_db,
        savings_db_path,
        project_root: cg.project_root().to_path_buf(),
        storage_mode,
        store_root,
        dashboard_root,
        curate_preview: Arc::new(RwLock::new(persisted_preview)),
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics: Arc::new(RwLock::new(code_diagnostics)),
        code_diagnostics_backfill_started: Arc::new(AtomicBool::new(false)),
    };
    if repair_memory_on_startup {
        if let Err(err) = memory_api::repair_derived_memory(&state).await {
            eprintln!("Dashboard memory repair skipped: {err}");
        }
    }
    // Pre-count non-usage messages in the background so the first Savings
    // tab paint doesn't pay the initial BPE pass over the session store.
    if warm_token_counts {
        token_count::spawn_warm(state.clone());
    }
    state
}

/// Builds the dashboard state shared by the CLI `run` path and the
/// `tracedecay_dashboard` MCP tool.
pub(crate) async fn build_state(cg: &TraceDecay) -> DashboardState {
    build_state_inner(cg, true, true).await
}

/// Builds a lightweight cached state for a non-active project selected from the
/// dashboard project picker.
pub(crate) async fn build_selected_project_state(cg: &TraceDecay) -> DashboardState {
    build_state_inner(cg, false, false).await
}

/// Detached catch-up ingest for transcript sources (Claude, Codex, Vibe,
/// Cline-like, and Cursor's historical backlog), mirroring the MCP serve
/// startup sweep so a standalone `tracedecay dashboard` reflects transcripts
/// written while no MCP server was running. Cursor's live turns still arrive
/// via hooks; the sweep shares their parse offsets so it only picks up
/// transcripts the hooks never saw. Fail-open and incremental
/// (`parse_offsets` makes repeats cheap no-ops).
fn spawn_session_catch_up_ingest(project_root: PathBuf) {
    tokio::spawn(async move {
        if let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await {
            let stats = crate::sessions::ingest_global_sources(&db, &project_root).await;
            if stats.sessions_upserted > 0 || stats.messages_upserted > 0 {
                eprintln!(
                    "Session catch-up ingest: {} session(s), {} message(s) updated.",
                    stats.sessions_upserted, stats.messages_upserted
                );
            }
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
    let state = build_state(cg).await;
    if state.lcm_scope != "global" {
        spawn_session_catch_up_ingest(state.project_root.clone());
    }

    let app = router(state);
    let (listener, addr) = bind_dashboard(host, port).await?;

    let url = format!("http://{addr}/");
    // Stable, parseable line for wrappers (the Hermes plugin reads this).
    println!("tracedecay dashboard listening on {url}");
    eprintln!("Serving project {}", cg.project_root().display());
    eprintln!("Press Ctrl+C to stop.");

    if open {
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
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .map_err(|e| config_error(format!("failed to bind {host}:{port}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| config_error(format!("failed to read local address: {e}")))?;
    Ok((listener, addr))
}

pub(crate) fn router(state: DashboardState) -> Router {
    let runtime = projects::DashboardRuntime::new(state);
    Router::new()
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
        .with_state(runtime)
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
            "/api/plugins/holographic/curation/preview",
            get(memory_api::curation_preview),
        )
        .route(
            "/api/plugins/holographic/curation/config",
            get(automation_config_api::get_config)
                .patch(automation_config_api::patch_config)
                .delete(automation_config_api::reset_config),
        )
        .route(
            "/api/plugins/holographic/curation/agent-plan",
            post(memory_api::curation_agent_plan),
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
            "/api/automation/runs/{run_id}/artifacts",
            get(automation_run_api::artifact_list),
        )
        .route(
            "/api/automation/runs/{run_id}/artifacts/{kind}",
            get(automation_run_api::artifact_payload),
        )
        .route("/api/plugins/holographic/curate", post(memory_api::curate))
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
}

async fn active_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    req: Request<Body>,
) -> Response {
    forward_project_request(runtime.active_state(), req).await
}

async fn project_scoped_api_gateway(
    State(runtime): State<projects::DashboardRuntime>,
    AxumPath((project_id, tail)): AxumPath<(String, String)>,
    mut req: Request<Body>,
) -> Response {
    let state_result = if tail.starts_with("plugins/holographic") {
        runtime.memory_state_for_project(&project_id).await
    } else {
        runtime.state_for_project(&project_id).await
    };
    let state = match state_result {
        Ok(state) => state,
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
            forward_project_request(state, req).await
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

async fn forward_project_request(state: DashboardState, req: Request<Body>) -> Response {
    let (mut parts, body) = req.into_parts();
    parts.extensions.clear();
    let req = Request::from_parts(parts, body);
    match project_api_router().with_state(state).oneshot(req).await {
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
            // Similarity-based dedup curation (delete/merge ops via /curate
            // and /curate/apply). LLM-proposed curation is served by the
            // configured standalone automation backend when enabled.
            "curation": true,
            "automation": automation_configured,
            "llm_curation": standalone_automation,
            "managed_skills": true,
            // Savings & Cost tab: savings-ledger analytics + per-session
            // cost accounting with OpenRouter-backed pricing.
            "savings": true,
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
    Json(json!(assets::DASHBOARD_PLUGINS
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
        .collect::<Vec<_>>()))
}
