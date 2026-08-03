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

pub(crate) mod assets;
pub use tracedecay_dashboard_api::memory_curate;
pub(crate) use tracedecay_dashboard_api::util;
pub(crate) use tracedecay_dashboard_api::{
    AutomationSchedulerReconciler, DashboardAccountingStore, DashboardAccountingStoreHandle,
    DashboardAutomationExecutor, DashboardAutomationTask, DashboardAutomationWriter,
    DashboardFuture, DashboardManagedSkillExporter, DashboardPrAutotrackReader,
    DashboardProfileRootResolver, DashboardProjectContext, DashboardProjectList,
    DashboardProjectRegistry, DashboardProjectStateBuilder, DashboardSavingsDay,
    DashboardSavingsTotal, DashboardState, DashboardTokenCount, direct_dashboard_automation_writer,
};
pub(crate) use tracedecay_dashboard_api::{
    analytics_api, automation_config_api, automation_fact_proposals_api, automation_jobs_api,
    automation_outcomes_api, automation_run_api, automation_scheduler_api, automation_skills_api,
    code_diagnostics_api, code_diagnostics_broker, graph_api, graph_queries, graph_service,
    lcm_api, lcm_queries, lcm_service, memory_analysis, memory_api, memory_queries, memory_service,
    projects, savings_api, savings_pricing, settings_api, token_count,
};

use std::path::{Path, PathBuf};
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

use crate::automation::backend;
use crate::automation::config::{self, AutomationBackend, AutomationHostMode};
use crate::db::Database;
use crate::diagnostics::lsp;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::storage::StorageMode;
use crate::tracedecay::TraceDecay;

struct RootDashboardAccountingStore {
    db: Arc<GlobalDb>,
}

impl DashboardAccountingStore for RootDashboardAccountingStore {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.db.dashboard_connection()
    }

    fn analytics_events(
        &self,
        project_root: PathBuf,
    ) -> DashboardFuture<std::result::Result<Vec<Value>, String>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move {
            let project_id = GlobalDb::canonical_project_key(&project_root);
            let events = db
                .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
                    provider: None,
                    project_id: Some(project_id),
                    session_id: None,
                    event_kind: None,
                    since: None,
                    limit: 10_000,
                })
                .await
                .map_err(|error| error.to_string())?;
            Ok(events
                .into_iter()
                .map(|event| {
                    json!({
                        "provider": event.provider,
                        "timestamp": event.timestamp,
                        "event_kind": event.event_kind,
                        "hook_name": event.hook_name,
                        "tool_name": event.tool_name,
                        "tool_category": event.tool_category,
                        "skill_name": event.skill_name,
                        "hint_category": event.hint_category,
                        "outcome": event.outcome,
                        "metadata_json": event.metadata_json,
                    })
                })
                .collect())
        })
    }

    fn sum_savings(&self, since: i64) -> DashboardFuture<DashboardSavingsTotal> {
        let db = Arc::clone(&self.db);
        Box::pin(async move {
            let total = db.sum_savings(None, since).await;
            DashboardSavingsTotal {
                saved_tokens: total.saved_tokens.min(i64::MAX as u64) as i64,
                calls: total.calls.min(i64::MAX as u64) as i64,
            }
        })
    }

    fn savings_history(&self, since: i64) -> DashboardFuture<Vec<DashboardSavingsDay>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move {
            db.savings_history(None, since)
                .await
                .into_iter()
                .map(|day| DashboardSavingsDay {
                    day: day.day,
                    saved_tokens: day.saved_tokens.min(i64::MAX as u64) as i64,
                    calls: day.calls.min(i64::MAX as u64) as i64,
                })
                .collect()
        })
    }

    fn ensure_token_count_cache(&self) -> DashboardFuture<bool> {
        let db = Arc::clone(&self.db);
        Box::pin(async move { db.ensure_token_count_cache().await })
    }

    fn load_token_counts(&self, store: String) -> DashboardFuture<Vec<DashboardTokenCount>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move {
            db.load_token_counts(&store)
                .await
                .into_iter()
                .map(
                    |(provider, message_id, text_len, token_count)| DashboardTokenCount {
                        provider,
                        message_id,
                        text_len,
                        token_count,
                        encoder: String::new(),
                    },
                )
                .collect()
        })
    }

    fn save_token_counts(
        &self,
        store: String,
        rows: Vec<DashboardTokenCount>,
    ) -> DashboardFuture<()> {
        let db = Arc::clone(&self.db);
        Box::pin(async move {
            let rows = rows
                .into_iter()
                .map(|row| crate::global_db::TokenCountUpsert {
                    provider: row.provider,
                    message_id: row.message_id,
                    text_len: row.text_len,
                    token_count: row.token_count,
                    encoder: match row.encoder.as_str() {
                        "cl100k_base" => "cl100k_base",
                        _ => "o200k_base",
                    },
                })
                .collect::<Vec<_>>();
            db.save_token_counts(&store, &rows).await;
        })
    }

    fn total_cost_since(&self, since: u64) -> DashboardFuture<Option<f64>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move { db.total_cost_since(since).await })
    }

    fn total_tokens_since(&self, since: u64) -> DashboardFuture<Option<u64>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move { db.total_tokens_since(since).await })
    }

    fn cost_by_model_since(&self, since: u64) -> DashboardFuture<Vec<(String, f64, u64)>> {
        let db = Arc::clone(&self.db);
        Box::pin(async move { db.cost_by_model_since(since).await })
    }
}

struct RootDashboardProjectRegistry;

impl DashboardProjectRegistry for RootDashboardProjectRegistry {
    fn list(
        &self,
        limit: usize,
        active_project_id: Option<String>,
    ) -> DashboardFuture<DashboardProjectList> {
        Box::pin(async move {
            let Some(db) = GlobalDb::open().await else {
                return DashboardProjectList::default();
            };
            let mut projects = db.list_code_projects(limit + 1).await;
            let truncated = projects.len() > limit;
            projects.truncate(limit);
            let contexts = db.project_registry_contexts_for_projects(&projects).await;
            let view = crate::project_registry::build_project_registry_view(
                &contexts,
                active_project_id.as_deref(),
                truncated,
            );
            let projects = projects
                .iter()
                .map(|project| {
                    serde_json::to_value(crate::project_registry::PublicCodeProject::from_record(
                        project,
                        active_project_id.as_deref(),
                    ))
                    .unwrap_or(Value::Null)
                })
                .collect();
            DashboardProjectList {
                truncated,
                projects,
                summary: serde_json::to_value(view.summary).unwrap_or(Value::Null),
                project_tree: serde_json::to_value(view.project_tree).unwrap_or(Value::Null),
            }
        })
    }

    fn context(
        &self,
        project_id: String,
        active_project_id: Option<String>,
    ) -> DashboardFuture<Option<DashboardProjectContext>> {
        Box::pin(async move {
            let db = GlobalDb::open().await?;
            let context = db.project_registry_context_by_id(&project_id).await?;
            let public = crate::project_registry::PublicProjectRegistryContext::new(
                &context,
                active_project_id.as_deref(),
            );
            let payload = json!({
                "project": public.project,
                "aliases": public.aliases,
                "stores": public.stores,
            });
            Some(DashboardProjectContext {
                cache_key: format!("{context:?}"),
                project_root: PathBuf::from(&context.project.canonical_root),
                payload,
            })
        })
    }
}

fn dashboard_project_state_builder() -> DashboardProjectStateBuilder {
    Arc::new(|project_id, project_root, active| {
        Box::pin(async move {
            let cg = TraceDecay::open_read_only(&project_root)
                .await
                .map_err(|error| tracedecay_dashboard_api::config_error(error.to_string()))?;
            if cg.store_layout().identity.project_id.as_deref() != Some(project_id.as_str()) {
                return Err(tracedecay_dashboard_api::config_error(format!(
                    "registered project id mismatch for {project_id}: {}",
                    project_root.display()
                )));
            }
            Ok(build_selected_project_state(&cg, &active).await)
        })
    })
}

fn dashboard_automation_executor(
    project_root: PathBuf,
    dashboard_root: PathBuf,
) -> DashboardAutomationExecutor {
    Arc::new(move |task| {
        let project_root = project_root.clone();
        let dashboard_root = dashboard_root.clone();
        Box::pin(async move {
            use crate::automation::backend::CodexAppServerBackend;
            use crate::automation::config::{
                AutomationBackend, effective_config, load_project_config,
            };
            use crate::automation::run_ledger::AutomationTrigger;
            use crate::automation::runner::{
                MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
                SkillWriterAutomationOptions, run_memory_curator_with_backend,
                run_session_reflector_with_backend, run_skill_writer_with_backend,
            };

            let cg = TraceDecay::open(&project_root)
                .await
                .map_err(|error| error.to_string())?;
            let global = crate::user_config::UserConfig::load().automation;
            let project = load_project_config(&dashboard_root)
                .await
                .map_err(|error| error.to_string())?;
            let config =
                effective_config(&global, project.as_ref()).map_err(|error| error.to_string())?;
            if config.enabled && config.backend == AutomationBackend::ExternalCommand {
                return Err(
                    "automation backend external_command is not implemented yet".to_string()
                );
            }
            let backend = CodexAppServerBackend::from_automation_config(&config);

            match task {
                DashboardAutomationTask::MemoryCurator {
                    max_clusters,
                    min_confidence,
                    run_id,
                } => {
                    let run = run_memory_curator_with_backend(
                        &cg,
                        &config,
                        &backend,
                        MemoryCuratorAutomationOptions {
                            trigger: AutomationTrigger::Dashboard,
                            run_id,
                            max_clusters,
                            min_confidence,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    Ok(
                        tracedecay_dashboard_api::automation_run_service::automation_run_payload(
                            &run.run_id,
                            &run.report,
                            &run.ledger_record,
                            run.backend_response.as_ref(),
                        ),
                    )
                }
                DashboardAutomationTask::SessionReflection {
                    provider,
                    query,
                    evidence_limit,
                    scope,
                    session_id,
                    include_summaries,
                    sort,
                    source,
                    role,
                    start_time,
                    end_time,
                    run_id,
                } => {
                    let mut options = SessionReflectorAutomationOptions {
                        trigger: AutomationTrigger::Dashboard,
                        run_id,
                        ..SessionReflectorAutomationOptions::default()
                    };
                    if let Some(provider) = provider {
                        options.provider = provider;
                    }
                    if let Some(query) = query {
                        options.query = query;
                    }
                    if let Some(evidence_limit) = evidence_limit {
                        options.evidence_limit = evidence_limit;
                    }
                    if let Some(scope) = scope {
                        options.scope = scope;
                    }
                    if let Some(session_id) = session_id {
                        options.session_id = Some(session_id);
                    }
                    if let Some(include_summaries) = include_summaries {
                        options.include_summaries = include_summaries;
                    }
                    if let Some(sort) = sort {
                        options.sort = sort;
                    }
                    if let Some(source) = source {
                        options.source = Some(source);
                    }
                    if let Some(role) = role {
                        options.role = Some(role);
                    }
                    options.start_time = start_time;
                    options.end_time = end_time;
                    let run = run_session_reflector_with_backend(&cg, &config, &backend, options)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(
                        tracedecay_dashboard_api::automation_run_service::automation_run_payload(
                            &run.run_id,
                            &run.report,
                            &run.ledger_record,
                            run.backend_response.as_ref(),
                        ),
                    )
                }
                DashboardAutomationTask::SkillWriting {
                    provider,
                    query,
                    evidence_limit,
                    run_id,
                } => {
                    let mut options = SkillWriterAutomationOptions {
                        trigger: AutomationTrigger::Dashboard,
                        run_id,
                        profile_root: None,
                        ..SkillWriterAutomationOptions::default()
                    };
                    if let Some(provider) = provider {
                        options.provider = provider;
                    }
                    if let Some(query) = query {
                        options.query = query;
                    }
                    if let Some(evidence_limit) = evidence_limit {
                        options.evidence_limit = evidence_limit;
                    }
                    let run = run_skill_writer_with_backend(&cg, &config, &backend, options)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(
                        tracedecay_dashboard_api::automation_run_service::automation_run_payload(
                            &run.run_id,
                            &run.report,
                            &run.ledger_record,
                            run.backend_response.as_ref(),
                        ),
                    )
                }
            }
        })
    })
}

fn dashboard_profile_root_resolver() -> DashboardProfileRootResolver {
    Arc::new(|| crate::storage::default_profile_root().map_err(|error| error.to_string()))
}

fn dashboard_managed_skill_exporter() -> DashboardManagedSkillExporter {
    Arc::new(|profile_root, project_root| {
        Box::pin(async move {
            let Some(home) = crate::agents::home_dir() else {
                return Vec::new();
            };
            tokio::task::spawn_blocking(move || {
                let reports = crate::agents::export_managed_skills_to_agent_hosts(
                    &home,
                    &project_root,
                    &profile_root,
                );
                crate::automation::skill_materialization::reconcile_after_activation(
                    &profile_root,
                    &project_root,
                );
                reports
                    .into_iter()
                    .map(|report| serde_json::to_value(report).unwrap_or(Value::Null))
                    .collect()
            })
            .await
            .unwrap_or_else(|error| {
                vec![json!({
                    "agent": "export-task",
                    "exports": [],
                    "error": format!("managed skill export task failed: {error}"),
                })]
            })
        })
    })
}

/// Default port for `tracedecay dashboard` (chosen to avoid common dev-server
/// defaults; override with `--port`).
pub use tracedecay_dashboard_api::DEFAULT_PORT;

/// The LCM session store the dashboard will serve.
pub(crate) struct LcmStoreSelection {
    pub(crate) conn: Option<libsql::Connection>,
    pub(crate) guard: Option<Arc<GlobalDb>>,
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
            let conn = db.dashboard_connection();
            return LcmStoreSelection {
                conn: Some(conn),
                guard: Some(Arc::new(db)),
                path: project_db_path.display().to_string(),
                scope: storage_mode_label(&cg.store_layout().storage_mode).to_string(),
            };
        }
    }
    let global = GlobalDb::open().await;
    let conn = global.as_ref().map(GlobalDb::dashboard_connection);
    LcmStoreSelection {
        conn,
        guard: global.map(Arc::new),
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

async fn open_dashboard_connection(path: &Path) -> Option<(libsql::Connection, Arc<Database>)> {
    let authority = crate::db::DatabaseAuthority::for_runtime(path, "dashboard").ok()?;
    let (db, _) = Database::open(path, &authority).await.ok()?;
    let conn = db.conn().clone();
    Some((conn, Arc::new(db)))
}

async fn memory_fact_count(conn: &libsql::Connection) -> Option<i64> {
    let mut rows = conn
        .query("SELECT COUNT(*) FROM memory_facts", ())
        .await
        .ok()?;
    rows.next().await.ok()??.get::<i64>(0).ok()
}

pub(crate) async fn resolve_project_memory_store(
    cg: &TraceDecay,
) -> (libsql::Connection, String, Option<Arc<Database>>) {
    let graph_path = cg.dashboard_db_path();
    let mut first_open: Option<(libsql::Connection, String, Option<Arc<Database>>)> = None;
    let mut seen = std::collections::BTreeSet::new();

    for path in [cg.store_layout().graph_db_path.clone()] {
        if !seen.insert(path.clone()) || !path.is_file() {
            continue;
        }
        let opened = if path == graph_path {
            Some((cg.dashboard_connection(), None))
        } else {
            open_dashboard_connection(&path)
                .await
                .map(|(conn, guard)| (conn, Some(guard)))
        };
        let Some((conn, guard)) = opened else {
            continue;
        };
        let display_path = path.display().to_string();
        if first_open.is_none() {
            first_open = Some((conn.clone(), display_path.clone(), guard.clone()));
        }
        if memory_fact_count(&conn).await.unwrap_or(0) > 0 {
            return (conn, display_path, guard);
        }
    }

    first_open.unwrap_or_else(|| {
        (
            cg.dashboard_connection(),
            cg.dashboard_db_path().display().to_string(),
            None,
        )
    })
}

async fn build_state_inner(
    cg: &TraceDecay,
    repair_memory_on_startup: bool,
    warm_token_counts: bool,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
) -> DashboardState {
    let (mem_conn, mem_db_path, mem_guard) = resolve_project_memory_store(cg).await;
    let lcm = resolve_lcm_store(cg).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let store_root = cg.store_layout().data_root.clone();
    let config_path = cg.store_layout().config_path.clone();
    let storage_mode = storage_mode_label(&cg.store_layout().storage_mode).to_string();
    let code_diagnostics_settings = lsp::settings::load_settings(&dashboard_root)
        .await
        .unwrap_or_default();
    let code_diagnostics =
        code_diagnostics_broker(cg.project_root().to_path_buf(), code_diagnostics_settings);
    let accounting_store = GlobalDb::open().await.map(|db| {
        Arc::new(RootDashboardAccountingStore { db: Arc::new(db) })
            as DashboardAccountingStoreHandle
    });
    let accounting_mode = crate::global_db::global_accounting_mode();
    let savings_db_path = crate::global_db::global_db_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let state = DashboardState {
        project_id: cg.store_layout().identity.project_id.clone(),
        graph_conn: cg.dashboard_connection(),
        database_guards: std::iter::once(cg.dashboard_database_guard())
            .chain(mem_guard)
            .collect(),
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_conn,
        mem_db_path,
        lcm_conn: lcm.conn,
        global_database_guards: lcm
            .guard
            .into_iter()
            .map(|guard| guard as Arc<dyn std::any::Any + Send + Sync>)
            .collect(),
        lcm_db_path: lcm.path,
        lcm_scope: lcm.scope,
        accounting_store,
        accounting_mode: tracedecay_dashboard_api::DashboardAccountingMode {
            enabled: accounting_mode.enabled(),
            source: accounting_mode.as_str(),
        },
        release_channel: if crate::cloud::is_beta() {
            "beta"
        } else {
            "stable"
        },
        pr_autotrack_reader: Some(Arc::new(|store_root| {
            #[cfg(unix)]
            {
                crate::daemon::pr_autotrack::managed_summary(&store_root)
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "branch": entry.branch,
                            "pr": entry.pr,
                            "head_branch": entry.head_branch,
                        })
                    })
                    .collect()
            }
            #[cfg(not(unix))]
            {
                let _ = store_root;
                Vec::new()
            }
        }) as DashboardPrAutotrackReader),
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
        automation_executor: Some(dashboard_automation_executor(
            cg.project_root().to_path_buf(),
            dashboard_root.clone(),
        )),
        skill_analytics_sync: Some(dashboard_skill_analytics_sync()),
        profile_root_resolver: dashboard_profile_root_resolver(),
        managed_skill_exporter: dashboard_managed_skill_exporter(),
        project_registry: Some(Arc::new(RootDashboardProjectRegistry)),
        project_state_builder: Some(dashboard_project_state_builder()),
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
#[allow(dead_code)]
pub(crate) async fn build_state(cg: &TraceDecay) -> DashboardState {
    build_state_inner(cg, true, true, None, direct_dashboard_automation_writer()).await
}

pub(crate) async fn build_state_with_automation_reconciler(
    cg: &TraceDecay,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
) -> DashboardState {
    build_state_inner(
        cg,
        true,
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
) -> DashboardState {
    build_state_inner(
        cg,
        false,
        false,
        None,
        Arc::clone(&active.automation_writer),
    )
    .await
}

/// Root composition façade for `tracedecay memory curate`.
pub async fn run_memory_curate(
    cg: &TraceDecay,
    options: &memory_curate::MemoryCurateOptions,
) -> Result<Value> {
    let (mem_conn, mem_db_path, mem_guard) = resolve_project_memory_store(cg).await;
    let layout = cg.store_layout();
    let state = DashboardState {
        project_id: layout.identity.project_id.clone(),
        graph_conn: cg.dashboard_connection(),
        database_guards: std::iter::once(cg.dashboard_database_guard())
            .chain(mem_guard)
            .collect(),
        graph_db_path: cg.dashboard_db_path().display().to_string(),
        mem_conn,
        mem_db_path,
        lcm_conn: None,
        global_database_guards: Vec::new(),
        lcm_db_path: String::new(),
        lcm_scope: storage_mode_label(&layout.storage_mode).to_string(),
        accounting_store: None,
        accounting_mode: tracedecay_dashboard_api::DashboardAccountingMode::default(),
        release_channel: if crate::cloud::is_beta() {
            "beta"
        } else {
            "stable"
        },
        pr_autotrack_reader: None,
        savings_db_path: String::new(),
        project_root: cg.project_root().to_path_buf(),
        storage_mode: storage_mode_label(&layout.storage_mode).to_string(),
        store_root: layout.data_root.clone(),
        config_path: layout.config_path.clone(),
        dashboard_root: layout.dashboard_root.clone(),
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics: Arc::new(RwLock::new(code_diagnostics_broker(
            cg.project_root().to_path_buf(),
            lsp::settings::CodeDiagnosticsSettings::default(),
        ))),
        code_diagnostics_backfill_started: Arc::new(AtomicBool::new(false)),
        automation_scheduler_reconciler: None,
        automation_writer: direct_dashboard_automation_writer(),
        automation_executor: None,
        skill_analytics_sync: None,
        profile_root_resolver: dashboard_profile_root_resolver(),
        managed_skill_exporter: dashboard_managed_skill_exporter(),
        project_registry: None,
        project_state_builder: None,
    };
    memory_curate::run_memory_curate_with_state(&state, options).await
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
    repair_memory_on_startup: bool,
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
        DashboardRunOptions::test(repair_memory_on_startup),
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct DashboardRunOptions {
    open: bool,
    repair_memory_on_startup: bool,
    warm_token_counts: bool,
    start_session_catch_up: bool,
}

impl DashboardRunOptions {
    fn production(open: bool) -> Self {
        Self {
            open,
            repair_memory_on_startup: true,
            warm_token_counts: true,
            start_session_catch_up: true,
        }
    }

    fn test(repair_memory_on_startup: bool) -> Self {
        Self {
            open: false,
            repair_memory_on_startup,
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
        options.repair_memory_on_startup,
        options.warm_token_counts,
        None,
        direct_dashboard_automation_writer(),
    )
    .await;
    if options.start_session_catch_up && state.lcm_scope != "global" {
        spawn_session_catch_up_ingest(state.project_root.clone());
    }
    let app = router(state);
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
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .map_err(|e| config_error(format!("failed to bind {host}:{port}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| config_error(format!("failed to read local address: {e}")))?;
    Ok((listener, addr))
}

pub(crate) fn router(state: DashboardState) -> Router {
    let runtime = projects::DashboardRuntime::new(state, project_api_router());
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
        .route("/api/settings", any(active_api_gateway))
        .route("/api/settings/{*tail}", any(active_api_gateway))
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
