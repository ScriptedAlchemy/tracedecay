use std::path::Path;

use tracedecay_contracts::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult, RenameResult,
    RenameSymbolBindingV1,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_source_edit::{
    EditDiagnosticRecord, SourceEditFuture, SourceEditGraphReadV1, SourceEditRuntimePort,
};

use super::TraceDecay;

impl SourceEditRuntimePort for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn run_diagnostics<'a>(
        &'a self,
        _file: &'a str,
    ) -> SourceEditFuture<'a, Vec<EditDiagnosticRecord>> {
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
    ) -> SourceEditFuture<'a, EditResult> {
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
    ) -> SourceEditFuture<'a, EditResult> {
        Box::pin(TraceDecay::str_replace(
            self, path, old_str, new_str, dry_run,
        ))
    }

    fn multi_str_replace<'a>(
        &'a self,
        path: &'a str,
        replacements: &'a [(&'a str, &'a str)],
        dry_run: bool,
    ) -> SourceEditFuture<'a, MultiEditResult> {
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
    ) -> SourceEditFuture<'a, InsertResult> {
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
    ) -> SourceEditFuture<'a, InsertResult> {
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
    ) -> SourceEditFuture<'a, AstGrepResult> {
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
    ) -> SourceEditFuture<'a, MoveResult> {
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
    ) -> SourceEditFuture<'a, RenameResult> {
        Box::pin(TraceDecay::rename_symbol(
            self, graph, binding, new_name, dry_run,
        ))
    }
}
