//! Anchored source-editing primitives (str-replace, insert, symbol
//! replacement, ast-grep rewrites) plus the single-file re-index they
//! trigger.
//!
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

mod api_migration;
mod ast_grep;
mod file_authority;
mod plan;
mod preview;
mod primitives;
mod symbols;

#[cfg(test)]
mod api_migration_graph_tests;
#[cfg(test)]
mod execute_tests;
#[cfg(test)]
mod reconcile_tests;
#[cfg(test)]
mod test_support;

// `move_symbol.rs` (a sibling of this module) imports these eight names via
// `use super::edits::{...}` — the re-exports below keep that import path
// stable across the split, resolving each name at `crate::tracedecay::edits`
// exactly as it did when they were all defined directly in `edits.rs`.
pub(in crate::tracedecay) use plan::{
    capture_planned_source_edit, publish_planned_source_edit, validate_planned_source_edit,
};
pub(in crate::tracedecay) use preview::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, edit_success_message,
};
pub(in crate::tracedecay) use symbols::resolve_symbol_for_edit;
