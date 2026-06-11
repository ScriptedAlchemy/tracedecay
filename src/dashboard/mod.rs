//! `tokensave dashboard` — local HTTP server for the dashboard UIs.
//!
//! Serves two dashboard plugin bundles ported from Hermes (the
//! holographic-memory explorer and the LCM explorer) behind a small
//! standalone shell, plus the JSON API both UIs expect — re-implemented on
//! top of tokensave's own data:
//!
//! - `/api/plugins/holographic/*`  → project memory store
//!   (`memory_facts` / `memory_entities` / `memory_banks` in the project DB)
//! - `/api/plugins/hermes-lcm/*`   → LCM session store
//!   (`lcm_raw_messages` / `lcm_summary_nodes` in the project-local
//!   `.tokensave/sessions.db` where transcript ingest writes; see
//!   [`resolve_lcm_store`] for the `TOKENSAVE_GLOBAL_DB` override and the
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

pub(crate) mod assets;
mod curate_preview_store;
mod graph_api;
mod lcm_api;
mod memory_analysis;
mod memory_api;
mod savings_api;
mod savings_pricing;
mod token_count;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::errors::{Result, TokenSaveError};
use crate::global_db::GlobalDb;
use crate::tokensave::TokenSave;

/// Default port for `tokensave dashboard` (chosen to avoid common dev-server
/// defaults; override with `--port`).
pub const DEFAULT_PORT: u16 = 7341;

/// Cached last curation preview, written by `POST /curate?dry_run=true`.
#[derive(Debug, Clone)]
pub(crate) struct CuratePreviewEntry {
    pub(crate) report: Value,
    /// ISO 8601 UTC timestamp when this preview was saved.
    pub(crate) saved_at: String,
    /// Active fact count at the time the preview was generated (for stale detection).
    pub(crate) active_facts_at_save: i64,
}

#[derive(Clone)]
pub(crate) struct DashboardState {
    /// Project database (code graph + holographic memory store).
    pub(crate) mem_conn: libsql::Connection,
    /// Display path of the project database.
    pub(crate) mem_db_path: String,
    /// LCM session store (project-local `sessions.db`, or the global DB
    /// when overridden/unavailable), when available.
    pub(crate) lcm_conn: Option<libsql::Connection>,
    /// Display path of the LCM session store actually being served.
    pub(crate) lcm_db_path: String,
    /// Which store `lcm_conn` points at: `"project_local"` or `"global"`.
    pub(crate) lcm_scope: &'static str,
    /// Global accounting DB (savings ledger, lifetime counters, turns) used
    /// by the Savings & Cost tab, when available.
    pub(crate) savings_db: Option<Arc<GlobalDb>>,
    /// Display path of the global accounting DB.
    pub(crate) savings_db_path: String,
    pub(crate) project_root: PathBuf,
    /// Last saved dry-run curation preview (shared across all clones of the state).
    pub(crate) curate_preview: Arc<RwLock<Option<CuratePreviewEntry>>>,
    /// In-process BPE token-count cache for the Savings & Cost tab (backed
    /// by the `dashboard_token_counts` sidecar in the global accounting DB).
    pub(crate) token_counts: Arc<token_count::TokenCountCache>,
}

/// The LCM session store the dashboard will serve.
pub(crate) struct LcmStoreSelection {
    pub(crate) conn: Option<libsql::Connection>,
    pub(crate) path: String,
    pub(crate) scope: &'static str,
}

/// Selects the LCM session store for `project_root`.
///
/// Transcript ingest writes per project: Cursor's end-of-turn hooks and the
/// MCP serve startup catch-up sweep (Claude/Codex/Vibe/Cline-like) both
/// upsert into `<project>/.tokensave/sessions.db`, never into
/// `~/.tokensave/global.db`. So the dashboard serves the project-local store
/// by default — opened with the same writable schema-ensuring path the MCP
/// LCM tools use for `storage_scope = "project_local"`, creating it on first
/// run.
///
/// An explicit `TOKENSAVE_GLOBAL_DB` override always wins (scope `"global"`):
/// tests, the smoke harness, and the Hermes wrapper use it to pin the
/// dashboard to a specific store. The legacy global DB is also the fallback
/// if the project store cannot be opened.
pub(crate) async fn resolve_lcm_store(project_root: &std::path::Path) -> LcmStoreSelection {
    if !crate::global_db::global_db_path_is_overridden() {
        let project_db_path = crate::sessions::cursor::project_session_db_path(project_root);
        if let Some(db) = GlobalDb::open_at(&project_db_path).await {
            return LcmStoreSelection {
                conn: Some(db.dashboard_connection()),
                path: project_db_path.display().to_string(),
                scope: "project_local",
            };
        }
    }
    let global = GlobalDb::open().await;
    LcmStoreSelection {
        conn: global.as_ref().map(GlobalDb::dashboard_connection),
        path: crate::global_db::global_db_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        scope: "global",
    }
}

/// Builds the dashboard state shared by the CLI `run` path and the
/// `tokensave_dashboard` MCP tool.
pub(crate) async fn build_state(cg: &TokenSave) -> DashboardState {
    if let Err(err) = cg.memory_status().await {
        eprintln!("Warning: dashboard memory repair failed: {err}");
    }
    let lcm = resolve_lcm_store(cg.project_root()).await;
    // Re-hydrate the last dry-run curation preview from its sidecar so it
    // survives server restarts (staleness is recomputed on read anyway).
    let persisted_preview = curate_preview_store::load(cg.project_root()).await;
    let savings_db = GlobalDb::open().await.map(Arc::new);
    let savings_db_path = crate::global_db::global_db_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let state = DashboardState {
        mem_conn: cg.dashboard_connection(),
        mem_db_path: cg.dashboard_db_path().display().to_string(),
        lcm_conn: lcm.conn,
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        savings_db,
        savings_db_path,
        project_root: cg.project_root().to_path_buf(),
        curate_preview: Arc::new(RwLock::new(persisted_preview)),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
    };
    // Pre-count non-usage messages in the background so the first Savings
    // tab paint doesn't pay the initial BPE pass over the session store.
    token_count::spawn_warm(state.clone());
    state
}

/// Detached catch-up ingest for hookless agents (Claude, Codex, Vibe,
/// Cline-like), mirroring the MCP serve startup sweep so a standalone
/// `tokensave dashboard` reflects transcripts written while no MCP server
/// was running. Cursor needs no sweep — its hooks ingest at end of turn.
/// Fail-open and incremental (`parse_offsets` makes repeats cheap no-ops).
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

fn config_error(message: impl Into<String>) -> TokenSaveError {
    TokenSaveError::Config {
        message: message.into(),
    }
}

/// Builds state and runs the dashboard server until interrupted.
/// Binds `host:port` (`port` 0 lets the OS pick) and prints the URL on
/// stderr; the URL line on stdout is stable output for wrappers to parse.
/// Pass `open: true` to also open the URL in the default browser (CLI --open).
pub async fn run(cg: &TokenSave, host: &str, port: u16, open: bool) -> Result<()> {
    let state = build_state(cg).await;
    if state.lcm_scope == "project_local" {
        spawn_session_catch_up_ingest(state.project_root.clone());
    }

    let app = router(state);
    let (listener, addr) = bind_dashboard(host, port).await?;

    let url = format!("http://{addr}/");
    // Stable, parseable line for wrappers (the Hermes plugin reads this).
    println!("tokensave dashboard listening on {url}");
    eprintln!("Serving project {}", cg.project_root().display());
    eprintln!("Press Ctrl+C to stop.");

    if open {
        match open::that(&url) {
            Ok(()) => eprintln!("Opened dashboard in default browser: {url}"),
            Err(e) => eprintln!("Warning: could not open browser for {url}: {e}"),
        }
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| config_error(format!("dashboard server error: {e}")))
}

/// Shared bind logic for both CLI `run` and the MCP `tokensave_dashboard` tool
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
    Router::new()
        .route("/", get(assets::index_html))
        .route("/shell/{file}", get(assets::shell_asset))
        .route(
            "/dashboard-plugins/{plugin}/dist/{file}",
            get(assets::plugin_asset),
        )
        .route("/api/capabilities", get(capabilities))
        .route("/api/dashboard/plugins", get(plugins_list))
        // Holographic memory plugin API (mirrors holographic_plus plugin_api.py)
        .route("/api/plugins/holographic/", get(memory_api::overview))
        .route("/api/plugins/holographic", get(memory_api::overview))
        .route(
            "/api/plugins/holographic/fact/{fact_id}",
            get(memory_api::fact_detail),
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
            "/api/plugins/holographic/curation/preview",
            get(memory_api::curation_preview),
        )
        .route("/api/plugins/holographic/curate", post(memory_api::curate))
        .route(
            "/api/plugins/holographic/curate/apply",
            post(memory_api::curate_apply),
        )
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
        // Savings & Cost API (savings ledger + session cost accounting)
        .route("/api/plugins/savings/overview", get(savings_api::overview))
        .route("/api/plugins/savings/ledger", get(savings_api::ledger))
        .route("/api/plugins/savings/sessions", get(savings_api::sessions))
        .route("/api/plugins/savings/models", get(savings_api::models))
        .route("/api/plugins/savings/pricing", get(savings_api::pricing))
        .with_state(state)
}

/// Capability discovery for hosts and future Hermes-side extensions. The UI
/// (or a wrapper) can probe this to decide which panels/actions to enable.
async fn capabilities(State(state): State<DashboardState>) -> Json<Value> {
    Json(json!({
        "name": "tokensave-dashboard",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "standalone",
        "project_root": state.project_root.display().to_string(),
        "memory_db": state.mem_db_path,
        "lcm_db": state.lcm_db_path,
        "lcm_scope": state.lcm_scope,
        "features": {
            "memory": true,
            "lcm": state.lcm_conn.is_some(),
            "graph": true,
            // Similarity-based dedup curation (delete/merge ops via /curate
            // and /curate/apply). LLM-proposed curation is a host-side
            // extension (the Hermes wrapper flips llm_curation when it adds
            // an LLM planner that calls /curate/apply).
            "curation": true,
            "llm_curation": false,
            // Savings & Cost tab: savings-ledger analytics + per-session
            // cost accounting with OpenRouter-backed pricing.
            "savings": true,
        },
        "dashboards": ["holographic", "hermes-lcm", "graph", "savings"],
    }))
}

/// Plugin manifest list, mirroring the Hermes `/api/dashboard/plugins`
/// endpoint shape closely enough for the standalone shell.
async fn plugins_list() -> Json<Value> {
    Json(json!([
        {
            "name": "holographic",
            "label": "Holographic Memory",
            "description": "Holographic memory explorer + curation",
            "icon": "BrainCircuit",
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "has_api": true,
            "source": "tokensave",
        },
        {
            "name": "hermes-lcm",
            "label": "LCM",
            "description": "Lossless Context Management dashboard tab.",
            "icon": "Database",
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "has_api": true,
            "source": "tokensave",
        },
        {
            "name": "graph",
            "label": "Code Graph",
            "description": "Search and explore the indexed code graph.",
            "icon": "Network",
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "has_api": true,
            "source": "tokensave",
        },
        {
            "name": "savings",
            "label": "Savings & Cost",
            "description": "Token savings ledger and session cost accounting.",
            "icon": "PiggyBank",
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "has_api": true,
            "source": "tokensave",
        }
    ]))
}
