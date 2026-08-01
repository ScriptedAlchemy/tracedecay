//! The root engine's implementation of the downward graph-runtime port.
//!
//! `tracedecay-usecases` states what it needs from a graph runtime as
//! [`GraphRuntimePort`]; this module is the one place where the root
//! [`TraceDecay`] engine satisfies that statement. Almost every method is a
//! direct delegation to the inherent counterpart — the calls are written as
//! `TraceDecay::method(self, …)` so an inherent method and its port method
//! can never quietly resolve to each other and recurse.
//!
//! Three methods have no inherent counterpart because the behavior lives in
//! the graph analysis engine or the diagnostics drivers rather than on the
//! engine itself; those are adapted here and marked below. None of them reach
//! up into the MCP handler layer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracedecay_application::retrieval::HealthDeltaResult;
use tracedecay_application::retrieval::grep_analysis::{RedundancyRequestV1, RedundancyResultV1};
use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult,
};
use tracedecay_application::{ApiMigrationApplyResultV1, ApiMigrationPlanV1};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_usecases::tracedecay::{
    BranchDiagnostics, EditDiagnosticRecord, GraphFuture, GraphRuntimePort, GraphValueFuture,
    PlannedSourceEditFile,
};

use crate::db::Database;
use crate::errors::TraceDecayError;
use crate::graph::redundancy_scan::RedundancyOptions;
use crate::storage::StoreLayout;
use crate::types::{Edge, GraphStats, Node, NodeKind, SearchResult, Subgraph};

use super::TraceDecay;

impl GraphRuntimePort for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn db(&self) -> &Database {
        TraceDecay::db(self)
    }

    fn db_path(&self) -> PathBuf {
        TraceDecay::db_path(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn is_read_only(&self) -> bool {
        TraceDecay::is_read_only(self)
    }

    fn branch_diagnostics(&self) -> BranchDiagnostics {
        TraceDecay::branch_diagnostics(self)
    }

    fn get_node<'a>(&'a self, id: &'a str) -> GraphFuture<'a, Option<Node>> {
        Box::pin(TraceDecay::get_node(self, id))
    }

    fn get_nodes_by_file<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<Node>> {
        Box::pin(TraceDecay::get_nodes_by_file(self, file))
    }

    fn get_nodes_by_name<'a>(&'a self, name: &'a str) -> GraphFuture<'a, Vec<Node>> {
        Box::pin(TraceDecay::get_nodes_by_name(self, name))
    }

    fn get_nodes_by_qualified_name<'a>(
        &'a self,
        qualified_name: &'a str,
    ) -> GraphFuture<'a, Vec<Node>> {
        Box::pin(TraceDecay::get_nodes_by_qualified_name(
            self,
            qualified_name,
        ))
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> GraphFuture<'a, Vec<SearchResult>> {
        Box::pin(TraceDecay::search(self, query, limit))
    }

    fn get_stats(&self) -> GraphFuture<'_, GraphStats> {
        Box::pin(TraceDecay::get_stats(self))
    }

    fn get_all_nodes(&self) -> GraphFuture<'_, Vec<Node>> {
        Box::pin(TraceDecay::get_all_nodes(self))
    }

    fn get_all_edges(&self) -> GraphFuture<'_, Vec<Edge>> {
        Box::pin(TraceDecay::get_all_edges(self))
    }

    fn get_incoming_edges<'a>(&'a self, node_id: &'a str) -> GraphFuture<'a, Vec<Edge>> {
        Box::pin(TraceDecay::get_incoming_edges(self, node_id))
    }

    fn get_outgoing_edges<'a>(&'a self, node_id: &'a str) -> GraphFuture<'a, Vec<Edge>> {
        Box::pin(TraceDecay::get_outgoing_edges(self, node_id))
    }

    fn get_callers<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<(Node, Edge)>> {
        Box::pin(TraceDecay::get_callers(self, node_id, max_depth))
    }

    fn get_callees<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<(Node, Edge)>> {
        Box::pin(TraceDecay::get_callees(self, node_id, max_depth))
    }

    fn get_call_chain<'a>(
        &'a self,
        from_id: &'a str,
        to_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Option<Vec<(Node, Option<Edge>)>>> {
        Box::pin(TraceDecay::get_call_chain(self, from_id, to_id, max_depth))
    }

    fn get_impact_radius<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Subgraph> {
        Box::pin(TraceDecay::get_impact_radius(self, node_id, max_depth))
    }

    fn get_impact_radius_multi<'a>(
        &'a self,
        seed_ids: &'a [String],
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<Node>> {
        Box::pin(TraceDecay::get_impact_radius_multi(
            self, seed_ids, max_depth,
        ))
    }

    fn get_trait_dispatch_targets<'a>(&'a self, method: &'a Node) -> GraphFuture<'a, Vec<Node>> {
        Box::pin(TraceDecay::get_trait_dispatch_targets(self, method))
    }

    fn get_test_annotated_node_ids<'a>(
        &'a self,
        candidate_ids: &'a [String],
    ) -> GraphFuture<'a, HashSet<String>> {
        Box::pin(TraceDecay::get_test_annotated_node_ids(self, candidate_ids))
    }

    fn get_files_with_test_annotations(&self) -> GraphFuture<'_, HashSet<String>> {
        Box::pin(TraceDecay::get_files_with_test_annotations(self))
    }

    fn get_file_dependents<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<String>> {
        Box::pin(TraceDecay::get_file_dependents(self, file))
    }

    fn node_at_location<'a>(
        &'a self,
        file: &'a str,
        line_1based: u32,
    ) -> GraphFuture<'a, Option<Node>> {
        Box::pin(TraceDecay::node_at_location(self, file, line_1based))
    }

    fn last_synced_commit(&self) -> GraphValueFuture<'_, Option<String>> {
        Box::pin(TraceDecay::last_synced_commit(self))
    }

    fn storage_page_counts(&self) -> GraphFuture<'_, (u64, u64, u64)> {
        Box::pin(TraceDecay::storage_page_counts(self))
    }

    fn get_complexity_ranked<'a>(
        &'a self,
        node_kind: Option<&'a NodeKind>,
        path_prefix: Option<&'a str>,
        limit: usize,
    ) -> GraphFuture<'a, Vec<(Node, u32, u64, u64, u64)>> {
        Box::pin(TraceDecay::get_complexity_ranked(
            self,
            node_kind,
            path_prefix,
            limit,
        ))
    }

    /// Adapter: diagnostics are produced by the per-language drivers, not by
    /// the engine. The file scope and the `file == requested file` filter the
    /// use case applies afterwards match the pre-split call exactly.
    fn run_diagnostics<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<EditDiagnosticRecord>> {
        Box::pin(async move {
            let scope = crate::diagnostics::Scope::File {
                path: file.to_owned(),
            };
            let diagnostics =
                crate::diagnostics::run_all(TraceDecay::project_root(self), &scope).await?;
            Ok(diagnostics
                .into_iter()
                .map(|diagnostic| EditDiagnosticRecord {
                    file: diagnostic.file,
                    line_start: diagnostic.line_start,
                    level: diagnostic.level,
                    code: Some(diagnostic.code),
                    message: diagnostic.message,
                })
                .collect())
        })
    }

    /// Adapter: the redundancy pipeline lives in the graph analysis engine,
    /// which renders the same structured payload this port returns typed. The
    /// MCP handler renders that payload rather than owning it.
    fn redundancy<'a>(
        &'a self,
        request: &'a RedundancyRequestV1,
        scope_prefix: Option<&'a str>,
    ) -> GraphFuture<'a, RedundancyResultV1> {
        Box::pin(async move {
            let options = RedundancyOptions {
                path_prefix: request.path.as_deref().or(scope_prefix),
                min_lines: request.min_lines,
                max_pairs: usize::try_from(u64::from(request.max_pairs).min(500)).unwrap_or(20),
                // A non-finite threshold never survived the handler's JSON
                // argument round-trip either (it decoded as null and fell back
                // to the default); keep that exact behavior.
                threshold: if request.similarity_threshold.is_finite() {
                    request.similarity_threshold.clamp(0.0, 1.0)
                } else {
                    0.6
                },
                include_naming: request.include_naming_only,
                include_generated: request.include_generated_paths,
            };
            let scan = crate::graph::redundancy_scan::redundancy_scan(self, &options).await?;
            serde_json::from_value(scan.output).map_err(|error| TraceDecayError::Config {
                message: format!("redundancy payload failed typed decode: {error}"),
            })
        })
    }

    /// Adapter: the pinned health delta is computed by the graph health
    /// engine, which already takes the engine and the observation database
    /// directly. The MCP session handlers call the same function.
    fn health_delta<'a>(
        &'a self,
        observation_database: &'a RegisteredGlobalDb,
        before_cursor: Option<&'a str>,
        path_prefix: Option<&'a str>,
    ) -> GraphFuture<'a, HealthDeltaResult> {
        Box::pin(crate::graph::health::delta::compute_health_delta_result(
            self,
            observation_database,
            before_cursor,
            path_prefix,
        ))
    }

    fn replace_symbol<'a>(
        &'a self,
        symbol: &'a str,
        new_source: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, EditResult> {
        Box::pin(TraceDecay::replace_symbol(
            self, symbol, new_source, dry_run,
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
        symbol: &'a str,
        content: &'a str,
        position: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, InsertResult> {
        Box::pin(TraceDecay::insert_at_symbol(
            self, symbol, content, position, dry_run,
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
        symbol: &'a str,
        dest_file: &'a str,
        dry_run: bool,
        update_references: bool,
    ) -> GraphFuture<'a, MoveResult> {
        Box::pin(TraceDecay::move_symbol(
            self,
            symbol,
            dest_file,
            dry_run,
            update_references,
        ))
    }

    fn apply_api_migration_plan<'a>(
        &'a self,
        plan: &'a ApiMigrationPlanV1,
        dry_run: bool,
        is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
    ) -> GraphFuture<'a, ApiMigrationApplyResultV1> {
        Box::pin(TraceDecay::apply_api_migration_plan(
            self,
            plan,
            dry_run,
            is_cancelled,
        ))
    }

    fn rollback_api_migration_plan<'a>(
        &'a self,
        plan: &'a ApiMigrationPlanV1,
    ) -> GraphFuture<'a, ()> {
        Box::pin(TraceDecay::rollback_api_migration_plan(self, plan))
    }

    fn recover_source_edit_preimages<'a>(
        &'a self,
        files: &'a [PlannedSourceEditFile],
    ) -> GraphFuture<'a, ()> {
        Box::pin(TraceDecay::recover_source_edit_preimages(self, files))
    }
}
