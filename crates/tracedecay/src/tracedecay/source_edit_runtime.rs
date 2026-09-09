//! Direct graph mutations are crate-internal adapters; external callers must
//! use the canonical source-edit transaction.
//!
//! ```compile_fail
//! async fn direct_str_replace_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph.str_replace("src/lib.rs", "old", "new", true).await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_multi_str_replace_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .multi_str_replace("src/lib.rs", &[("old", "new")], true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_insert_at_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .insert_at("src/lib.rs", "anchor", "content", true, true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_replace_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph.replace_symbol("symbol", "fn symbol() {}", true).await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_insert_at_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .insert_at_symbol("symbol", "content", "before", true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_ast_grep_rewrite_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .ast_grep_rewrite("src/lib.rs", "$A", "$A", true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_move_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .move_symbol("symbol", "src/dest.rs", true, false)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_rename_symbol_is_not_public(
//!     graph: &tracedecay::tracedecay::TraceDecay,
//!     binding: &tracedecay_contracts::RenameSymbolBindingV1,
//! ) {
//!     let _ = graph.rename_symbol(binding, "new_name", true).await;
//! }
//! ```

use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_source_edit::{EditDiagnosticRecord, SourceEditFuture, SourceEditRuntimePort};

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
}
