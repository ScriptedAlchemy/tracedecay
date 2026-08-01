//! Structural analysis tool handlers.
//!
//! One sibling module per report. This module holds only the shared imports
//! (siblings pick them up through `use super::*`), the two path predicates used
//! by more than one sibling, and the re-exports the handler dispatcher calls.
//!
//! `constructors`, `field_sites`, `imports`, `recursion`, and `unsafe_patterns`
//! read Rust source with hand-rolled byte scanners built on [`lex`]. That
//! scanning is scheduled to move onto ast-grep patterns and graph edges — see
//! the migration notes in `docs/` before extending it.

mod circular;
mod complexity;
mod constructors;
mod dead_code;
mod diagnostics;
mod field_sites;
mod hotspots;
mod imports;
mod lex;
mod metrics;
mod recursion;
mod unsafe_patterns;

pub(super) use circular::handle_circular;
pub(super) use complexity::{handle_complexity, handle_doc_coverage, handle_god_class};
pub(super) use constructors::handle_constructors;
pub(super) use dead_code::handle_dead_code;
pub(super) use diagnostics::handle_diagnostics;
pub(super) use field_sites::handle_field_sites;
pub(super) use hotspots::handle_hotspots;
pub(super) use imports::handle_unused_imports;
pub(super) use metrics::{
    handle_coupling, handle_distribution, handle_inheritance_depth, handle_largest, handle_rank,
};
pub(super) use recursion::handle_recursion;
pub(super) use unsafe_patterns::handle_unsafe_patterns;

use lex::{is_ident_byte, line_number_at, skip_ascii_whitespace};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_lsp::analyzer::activity::{active_languages_for_files, documents_for_adapter};
use tracedecay_lsp::analyzer::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity, NodeSpan,
    enclosing_node_for_line,
};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

use super::super::ToolResult;
use super::super::render;
use super::support::{
    effective_path, filter_by_scope, generic_tool_result, rendered_tool_result,
    require_object_args, require_positive_limit, unique_file_paths,
};

/// True when `path` names a Rust source file (case-insensitive `.rs`). Gates
/// tree-sitter masking, which parses with the Rust grammar and would
/// mis-tokenise other languages.
fn path_is_rust(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

fn path_matches_optional_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    crate::path_scope::path_matches_scope(path, scope_prefix)
}
