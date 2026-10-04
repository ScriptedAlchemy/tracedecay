//! File-operation spans for language parse and extract.
//!
//! Spans stay at one file per measurement. Individual AST nodes are never
//! timed.

/// Time one file parse.
#[inline]
pub(crate) fn measure_parse_file<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.parse_file").entered();
    f()
}

/// Time one file extract.
#[inline]
pub(crate) fn measure_extract_file<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.traverse_file").entered();
    f()
}

/// Time the Markdown composite-grammar fallback without recursively recording
/// another full-file traversal.
#[inline]
pub(crate) fn measure_markdown_composite_fallback<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.markdown_composite_fallback").entered();
    f()
}

/// Time grammar acquisition and language-specific source prep (masking).
#[inline]
pub(crate) fn measure_language<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.language").entered();
    f()
}

/// Time one file-level AST walk. Per-node visitors stay unmeasured.
#[inline]
pub(crate) fn measure_query<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.query").entered();
    f()
}

/// Time file-level graph emit / canonicalize. Not a per-token emit.
#[inline]
pub(crate) fn measure_emit<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.emit").entered();
    f()
}

/// Time the one-time grammar table construction (every enabled tier's
/// tree-sitter `Language` conversion). This serial cost is paid once per
/// process by whichever worker first touches the table; without its own
/// span it hides inside one outlier `code_extraction.language` sample.
#[inline]
pub(crate) fn measure_grammar_table_init<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.grammar_table_init").entered();
    f()
}

/// Time the post-parse changed-range collection and extraction-range
/// expansion (bounded tree walks that scope incremental re-extraction).
/// Runs once per edit batch, never per node.
#[inline]
pub(crate) fn measure_change_ranges<T>(f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!("code_extraction.change_ranges").entered();
    f()
}
