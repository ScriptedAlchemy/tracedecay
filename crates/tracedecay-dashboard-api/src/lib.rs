//! Dashboard HTTP routes, read models, and services.
//!
//! The root crate retains embedded assets plus CLI/daemon server composition.

pub mod analytics_api;
pub mod automation_config_api;
pub mod automation_fact_proposals_api;
pub mod automation_jobs_api;
pub mod automation_outcomes_api;
pub mod automation_run_api;
pub mod automation_run_service;
pub mod automation_scheduler_api;
pub mod automation_skills_api;
pub mod code_diagnostics_api;
pub mod graph_api;
pub mod graph_queries;
pub mod graph_service;
pub mod lcm_api;
pub mod lcm_queries;
pub mod lcm_service;
pub mod memory_analysis;
pub mod memory_api;
pub mod memory_curate;
pub mod memory_queries;
pub mod memory_service;
pub mod projects;
pub mod savings_api;
pub mod savings_pricing;
pub mod settings_api;
pub mod token_count;
pub mod tracedecay;
pub mod util;

// These are concrete lower-layer crates. Keeping the compatibility names
// local lets the moved live-source bodies retain their exact route logic
// without a dependency back to the root composition crate.
pub use tracedecay_agent_hosts::{agents, analytics};
pub use tracedecay_automation as automation;
pub use tracedecay_runtime_core::{config, memory, project_registry, timeutil};
pub use tracedecay_sessions as sessions;
pub use tracedecay_usecases::user_config;

pub mod db {
    pub use tracedecay_runtime_core::db::*;
}

pub mod errors {
    pub use tracedecay_runtime_core::errors::*;
}

pub mod storage {
    pub use tracedecay_runtime_core::storage::*;
}

pub mod diagnostics {
    pub use tracedecay_lsp as lsp;
}

use std::any::Any;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tokio::sync::RwLock;
use tracedecay_lsp as lsp;
use tracedecay_runtime_core::db::Database;

pub use automation_run_service::{DashboardAutomationWriter, direct_dashboard_automation_writer};

/// Default port for `tracedecay dashboard`.
pub const DEFAULT_PORT: u16 = 7341;

pub type AutomationSchedulerReconciler = Arc<dyn Fn() + Send + Sync + 'static>;

pub type DashboardFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardAccountingMode {
    pub enabled: bool,
    pub source: &'static str,
}

impl Default for DashboardAccountingMode {
    fn default() -> Self {
        Self {
            enabled: true,
            source: "default",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DashboardSavingsTotal {
    pub saved_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Default)]
pub struct DashboardSavingsDay {
    pub day: i64,
    pub saved_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug)]
pub struct DashboardTokenCount {
    pub provider: String,
    pub message_id: String,
    pub text_len: i64,
    pub token_count: i64,
    pub encoder: String,
}

/// Narrow root adapter for the process-global accounting database. Route
/// modules only need dashboard reads and token-count persistence; construction
/// and database authority stay in the composition root.
pub trait DashboardAccountingStore: Send + Sync {
    fn dashboard_connection(&self) -> libsql::Connection;
    fn analytics_events(
        &self,
        project_root: PathBuf,
    ) -> DashboardFuture<Result<Vec<Value>, String>>;
    fn sum_savings(&self, since: i64) -> DashboardFuture<DashboardSavingsTotal>;
    fn savings_history(&self, since: i64) -> DashboardFuture<Vec<DashboardSavingsDay>>;
    fn ensure_token_count_cache(&self) -> DashboardFuture<bool>;
    fn load_token_counts(&self, store: String) -> DashboardFuture<Vec<DashboardTokenCount>>;
    fn save_token_counts(
        &self,
        store: String,
        rows: Vec<DashboardTokenCount>,
    ) -> DashboardFuture<()>;
    fn total_cost_since(&self, since: u64) -> DashboardFuture<Option<f64>>;
    fn total_tokens_since(&self, since: u64) -> DashboardFuture<Option<u64>>;
    fn cost_by_model_since(&self, since: u64) -> DashboardFuture<Vec<(String, f64, u64)>>;
}

pub type DashboardAccountingStoreHandle = Arc<dyn DashboardAccountingStore>;

#[derive(Clone, Debug)]
pub struct DashboardProjectContext {
    pub cache_key: String,
    pub project_root: PathBuf,
    pub payload: Value,
}

#[derive(Clone, Debug, Default)]
pub struct DashboardProjectList {
    pub truncated: bool,
    pub projects: Vec<Value>,
    pub summary: Value,
    pub project_tree: Value,
}

/// Narrow root adapter for the project registry. The route crate owns HTTP
/// shaping and cache policy; root retains registry and project-open authority.
pub trait DashboardProjectRegistry: Send + Sync {
    fn list(
        &self,
        limit: usize,
        active_project_id: Option<String>,
    ) -> DashboardFuture<DashboardProjectList>;
    fn context(
        &self,
        project_id: String,
        active_project_id: Option<String>,
    ) -> DashboardFuture<Option<DashboardProjectContext>>;
}

pub type DashboardProjectRegistryHandle = Arc<dyn DashboardProjectRegistry>;
pub type DashboardProjectStateFuture = DashboardFuture<crate::errors::Result<DashboardState>>;
pub type DashboardProjectStateBuilder = Arc<
    dyn Fn(String, PathBuf, DashboardState) -> DashboardProjectStateFuture + Send + Sync + 'static,
>;
pub type DashboardPrAutotrackReader = Arc<dyn Fn(PathBuf) -> Vec<Value> + Send + Sync + 'static>;
pub enum DashboardAutomationTask {
    MemoryCurator {
        max_clusters: usize,
        min_confidence: f64,
        run_id: Option<String>,
    },
    SessionReflection {
        provider: Option<String>,
        query: Option<String>,
        evidence_limit: Option<usize>,
        scope: Option<sessions::lcm::LcmScope>,
        session_id: Option<String>,
        include_summaries: Option<bool>,
        sort: Option<sessions::lcm::LcmGrepSort>,
        source: Option<String>,
        role: Option<String>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        run_id: Option<String>,
    },
    SkillWriting {
        provider: Option<String>,
        query: Option<String>,
        evidence_limit: Option<usize>,
        run_id: Option<String>,
    },
}
pub type DashboardAutomationExecutor = Arc<
    dyn Fn(DashboardAutomationTask) -> DashboardFuture<Result<Value, String>>
        + Send
        + Sync
        + 'static,
>;
pub type DashboardSkillAnalyticsSync =
    Arc<dyn Fn(PathBuf, PathBuf) -> DashboardFuture<Result<(), String>> + Send + Sync + 'static>;
/// Root-owned profile resolution. The dashboard never assumes a process-global
/// profile location; composition supplies the current policy at request time.
pub type DashboardProfileRootResolver =
    Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync + 'static>;
/// Root-owned host export/materialization. The route crate owns the HTTP
/// response; host discovery and filesystem authority remain in composition.
pub type DashboardManagedSkillExporter =
    Arc<dyn Fn(PathBuf, PathBuf) -> DashboardFuture<Vec<Value>> + Send + Sync + 'static>;

pub fn config_error(message: impl Into<String>) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

pub fn code_diagnostics_broker(
    project_root: PathBuf,
    settings: lsp::settings::CodeDiagnosticsSettings,
) -> lsp::broker::DiagnosticBroker {
    let mut adapters = lsp::adapters::builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    lsp::broker::DiagnosticBroker::new(project_root, adapters, settings)
}

/// State consumed by every extracted dashboard route and service. Root-only
/// server composition supplies its lower-layer database authorities.
#[derive(Clone)]
pub struct DashboardState {
    pub project_id: Option<String>,
    pub graph_conn: libsql::Connection,
    pub database_guards: Vec<Arc<Database>>,
    pub graph_db_path: String,
    pub mem_conn: libsql::Connection,
    pub mem_db_path: String,
    pub lcm_conn: Option<libsql::Connection>,
    pub global_database_guards: Vec<Arc<dyn Any + Send + Sync>>,
    pub lcm_db_path: String,
    pub lcm_scope: String,
    pub accounting_store: Option<DashboardAccountingStoreHandle>,
    pub accounting_mode: DashboardAccountingMode,
    pub release_channel: &'static str,
    pub pr_autotrack_reader: Option<DashboardPrAutotrackReader>,
    pub savings_db_path: String,
    pub project_root: PathBuf,
    pub storage_mode: String,
    pub store_root: PathBuf,
    pub config_path: PathBuf,
    pub dashboard_root: PathBuf,
    pub curation_activity: Arc<RwLock<Vec<Value>>>,
    pub token_counts: Arc<token_count::TokenCountCache>,
    pub code_diagnostics: Arc<RwLock<lsp::broker::DiagnosticBroker>>,
    pub code_diagnostics_backfill_started: Arc<AtomicBool>,
    pub automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    pub automation_writer: DashboardAutomationWriter,
    pub automation_executor: Option<DashboardAutomationExecutor>,
    pub skill_analytics_sync: Option<DashboardSkillAnalyticsSync>,
    pub profile_root_resolver: DashboardProfileRootResolver,
    pub managed_skill_exporter: DashboardManagedSkillExporter,
    pub project_registry: Option<DashboardProjectRegistryHandle>,
    pub project_state_builder: Option<DashboardProjectStateBuilder>,
}

impl DashboardState {
    pub fn reconcile_automation_scheduler(&self) {
        if let Some(reconcile) = &self.automation_scheduler_reconciler {
            reconcile();
        }
    }
}
