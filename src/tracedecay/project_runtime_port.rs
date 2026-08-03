//! Root-owned adapters for automation and dashboard project runtimes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_agent_hosts::ports::project_runtime::{
    MemoryCurateOptions, ProjectRuntime, RuntimeFuture,
};
use tracedecay_dashboard_api::DashboardProjectRuntime;
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::Result;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_usecases::configuration::UserSettingsDaemonClient;

use super::TraceDecay;

impl ProjectRuntime for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn db(&self) -> &Database {
        TraceDecay::db(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn project_memory_owner(&self) -> Result<FactOwnerV1> {
        TraceDecay::project_memory_owner(self)
    }

    fn profile_database(&self) -> &Arc<RegisteredGlobalDb> {
        TraceDecay::profile_database(self)
    }

    fn project_sessions(
        &self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>> {
        Box::pin(async move {
            TraceDecay::store_runtime_registry(self)
                .project_sessions(project_id, roots)
                .await
        })
    }

    fn open_project_store_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(TraceDecay::open_project_store_db(self))
    }

    fn curate_memory<'a>(
        &'a self,
        options: &'a MemoryCurateOptions,
    ) -> RuntimeFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: options.apply,
                llm: options.llm,
                llm_ops: options.llm_ops.clone(),
                max_clusters: options.max_clusters,
                min_confidence: options.min_confidence,
            };
            crate::dashboard::memory_curate::run_memory_curate(self, &options).await
        })
    }
}

impl DashboardProjectRuntime for TraceDecay {
    fn automation_runtime(&self) -> &(dyn ProjectRuntime + 'static) {
        self
    }

    fn dashboard_db_path(&self) -> PathBuf {
        TraceDecay::dashboard_db_path(self)
    }

    fn dashboard_database_guard(&self) -> Arc<Database> {
        TraceDecay::dashboard_database_guard(self)
    }

    fn storage_telemetry_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle> {
        TraceDecay::storage_telemetry_handle(self)
    }

    fn retention_config(&self) -> tracedecay_dashboard_api::config::RetentionConfig {
        tracedecay_dashboard_api::config::RetentionConfig {
            store_soft_budgets_bytes: TraceDecay::get_config(self)
                .sync
                .retention
                .store_soft_budgets_bytes
                .clone(),
        }
    }

    fn user_settings_client(&self) -> Arc<dyn UserSettingsDaemonClient> {
        TraceDecay::configuration_runtime(self).user_settings_client()
    }
}
