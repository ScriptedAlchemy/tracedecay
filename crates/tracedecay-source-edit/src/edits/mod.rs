//! Anchored source-editing primitives (str-replace, insert, symbol
//! replacement, ast-grep rewrites) owned by the source-edit crate.

mod ast_grep;
mod preview;
mod primitives;
mod rename;
mod symbols;

#[cfg(test)]
mod execute_tests;
#[cfg(test)]
mod reconcile_tests;
#[cfg(test)]
mod test_support;

pub(crate) use ast_grep::ast_grep_rewrite;
pub(crate) use preview::{
    LeadingKind, MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff,
    classify_leading_line, edit_success_message,
};
pub(crate) use primitives::{
    insert_at, insert_at_symbol, multi_str_replace, replace_symbol, splice_lines, str_replace,
};
pub(crate) use rename::rename_symbol;
pub(crate) use symbols::{EditSymbolV1, edit_symbol_from_summary, resolve_symbol_for_edit};
