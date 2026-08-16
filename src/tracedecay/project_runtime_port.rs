//! Root-owned adapters for automation and dashboard project runtimes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_agent_hosts::ports::project_runtime::{ProjectRuntime, RuntimeFuture};
use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult, RenameResult,
    RenameSymbolBindingV1,
};
use tracedecay_dashboard_api::DashboardProjectRuntime;
use tracedecay_domain::{FactOwnerV1, ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::{Database, DatabaseStorageTelemetryHandle};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_usecases::configuration::UserSettingsDaemonClient;
use tracedecay_usecases::tracedecay::{
    EditDiagnosticRecord, GraphFuture, PlannedSourceEditFile, SourceEditGraphReadV1,
    SourceEditRuntimePort, SourceReadRuntimePort,
};

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

    fn profile_id(&self) -> &UserProfileId {
        self.store_runtime_registry().profile_id()
    }

    fn profile_database(&self) -> &RegisteredGlobalDbLeaseV1 {
        TraceDecay::profile_database(self)
    }

    fn project_sessions(
        &self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1> {
        Box::pin(async move {
            TraceDecay::store_runtime_registry(self)
                .project_sessions(project_id, roots)
                .await
        })
    }

    fn open_project_store_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(TraceDecay::open_project_store_db(self))
    }
}

impl DashboardProjectRuntime for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn automation_runtime(&self) -> &(dyn ProjectRuntime + 'static) {
        self
    }

    fn dashboard_db_path(&self) -> PathBuf {
        TraceDecay::dashboard_db_path(self)
    }

    fn dashboard_database_guard(&self) -> Arc<Database> {
        TraceDecay::dashboard_database_guard(self)
    }

    fn storage_telemetry_handle(&self) -> Result<DatabaseStorageTelemetryHandle> {
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

impl SourceEditRuntimePort for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn run_diagnostics<'a>(&'a self, _file: &'a str) -> GraphFuture<'a, Vec<EditDiagnosticRecord>> {
        Box::pin(async {
            Err(TraceDecayError::project_route(
                "source_edit_diagnostics_unavailable",
                true,
                "source-edit verification requires the daemon-owned LSP diagnostics authority",
            ))
        })
    }

    fn replace_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        new_source: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, EditResult> {
        Box::pin(TraceDecay::replace_symbol(
            self, graph, symbol, new_source, dry_run,
        ))
    }

    fn str_replace<'a>(
        &'a self,
        path: &'a str,
        old_str: &'a str,
        new_str: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, EditResult> {
        Box::pin(TraceDecay::str_replace(
            self, path, old_str, new_str, dry_run,
        ))
    }

    fn multi_str_replace<'a>(
        &'a self,
        path: &'a str,
        replacements: &'a [(&'a str, &'a str)],
        dry_run: bool,
    ) -> GraphFuture<'a, MultiEditResult> {
        Box::pin(TraceDecay::multi_str_replace(
            self,
            path,
            replacements,
            dry_run,
        ))
    }

    fn insert_at<'a>(
        &'a self,
        path: &'a str,
        anchor: &'a str,
        content: &'a str,
        before: bool,
        dry_run: bool,
    ) -> GraphFuture<'a, InsertResult> {
        Box::pin(TraceDecay::insert_at(
            self, path, anchor, content, before, dry_run,
        ))
    }

    fn insert_at_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        content: &'a str,
        position: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, InsertResult> {
        Box::pin(TraceDecay::insert_at_symbol(
            self, graph, symbol, content, position, dry_run,
        ))
    }

    fn ast_grep_rewrite<'a>(
        &'a self,
        path: &'a str,
        pattern: &'a str,
        rewrite: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, AstGrepResult> {
        Box::pin(TraceDecay::ast_grep_rewrite(
            self, path, pattern, rewrite, dry_run,
        ))
    }

    fn move_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        dest_file: &'a str,
        dry_run: bool,
        update_references: bool,
    ) -> GraphFuture<'a, MoveResult> {
        Box::pin(TraceDecay::move_symbol(
            self,
            graph,
            symbol,
            dest_file,
            dry_run,
            update_references,
        ))
    }

    fn rename_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        binding: &'a RenameSymbolBindingV1,
        new_name: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, RenameResult> {
        Box::pin(TraceDecay::rename_symbol(
            self, graph, binding, new_name, dry_run,
        ))
    }

    fn recover_source_edit_preimages<'a>(
        &'a self,
        files: &'a [PlannedSourceEditFile],
    ) -> GraphFuture<'a, ()> {
        Box::pin(TraceDecay::recover_source_edit_preimages(self, files))
    }

    fn apply_source_edit_rollback<'a>(
        &'a self,
        files: &'a [PlannedSourceEditFile],
    ) -> GraphFuture<'a, ()> {
        Box::pin(TraceDecay::apply_source_edit_rollback(self, files))
    }

    fn commit_source_edit_postimages<'a>(
        &'a self,
        files: &'a [PlannedSourceEditFile],
    ) -> GraphFuture<'a, ()> {
        Box::pin(TraceDecay::commit_source_edit_postimages(self, files))
    }
}

impl SourceReadRuntimePort for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn db(&self) -> &Database {
        TraceDecay::db(self)
    }

    fn is_read_only(&self) -> bool {
        TraceDecay::is_read_only(self)
    }
}
