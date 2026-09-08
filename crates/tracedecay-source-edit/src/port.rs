//! The narrow ports the source-edit slice consumes from its composition root.
//!
//! The root injects one implementation of [`SourceEditRuntimePort`] (the edit
//! primitives over the authorized worktree plus optional post-edit
//! diagnostics) and supplies graph evidence per request as one admitted,
//! generation-pinned [`SourceEditGraphReadV1`]. Planning, preview capture,
//! journaling, rollback, recovery, and reconciliation are owned here and never
//! delegated back through the port.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::CodeGraphInteractiveReader;
use tracedecay_contracts::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult, RenameResult,
    RenameSymbolBindingV1,
};
use tracedecay_domain::errors::Result;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::storage::StoreLayout;

pub type SourceEditFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// One application-admitted immutable graph generation used for a source-edit
/// plan and its exact preview/apply identity.
#[derive(Clone)]
pub struct SourceEditGraphReadV1 {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

impl SourceEditGraphReadV1 {
    pub fn new(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            reader,
            cancellation,
        }
    }

    pub fn reader(&self) -> &CodeGraphInteractiveReader {
        &self.reader
    }

    pub fn cancellation(&self) -> Arc<dyn GraphCancellation> {
        Arc::clone(&self.cancellation)
    }
}

#[derive(Debug, Clone)]
pub struct EditDiagnosticRecord {
    pub file: String,
    pub line_start: u32,
    pub level: String,
    pub code: Option<String>,
    pub message: String,
}

/// Narrow root-owned mutation authority used by the source-edit application.
///
/// Graph evidence is supplied separately as one admitted, generation-pinned
/// [`SourceEditGraphReadV1`]. This port owns only the edit primitives and
/// optional post-edit diagnostics; it must not grow graph-query, journal, or
/// recovery methods.
pub trait SourceEditRuntimePort: Send + Sync {
    fn project_root(&self) -> &Path;
    fn store_layout(&self) -> &StoreLayout;
    fn run_diagnostics<'a>(
        &'a self,
        file: &'a str,
    ) -> SourceEditFuture<'a, Vec<EditDiagnosticRecord>>;
    fn replace_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        new_source: &'a str,
        dry_run: bool,
    ) -> SourceEditFuture<'a, EditResult>;
    fn str_replace<'a>(
        &'a self,
        path: &'a str,
        old_str: &'a str,
        new_str: &'a str,
        dry_run: bool,
    ) -> SourceEditFuture<'a, EditResult>;
    fn multi_str_replace<'a>(
        &'a self,
        path: &'a str,
        replacements: &'a [(&'a str, &'a str)],
        dry_run: bool,
    ) -> SourceEditFuture<'a, MultiEditResult>;
    fn insert_at<'a>(
        &'a self,
        path: &'a str,
        anchor: &'a str,
        content: &'a str,
        before: bool,
        dry_run: bool,
    ) -> SourceEditFuture<'a, InsertResult>;
    fn insert_at_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        content: &'a str,
        position: &'a str,
        dry_run: bool,
    ) -> SourceEditFuture<'a, InsertResult>;
    fn ast_grep_rewrite<'a>(
        &'a self,
        path: &'a str,
        pattern: &'a str,
        rewrite: &'a str,
        dry_run: bool,
    ) -> SourceEditFuture<'a, AstGrepResult>;
    fn move_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        dest_file: &'a str,
        dry_run: bool,
        update_references: bool,
    ) -> SourceEditFuture<'a, MoveResult>;
    fn rename_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        binding: &'a RenameSymbolBindingV1,
        new_name: &'a str,
        dry_run: bool,
    ) -> SourceEditFuture<'a, RenameResult>;
}

pub type SourceEditRuntime = dyn SourceEditRuntimePort;
