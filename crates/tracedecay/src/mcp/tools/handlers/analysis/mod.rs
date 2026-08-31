//! Composition-root analysis handlers that still need source-masking or LSP.
//!
//! Portable census reports live in `tracedecay_mcp::handlers::analysis`.

mod diagnostics;
mod imports;
mod unmounted_files;
mod unsafe_patterns;

pub(super) use diagnostics::handle_diagnostics;
pub(super) use imports::handle_unused_imports;
pub(super) use unmounted_files::handle_unmounted_files;
pub(super) use unsafe_patterns::handle_unsafe_patterns;

use tracedecay_mcp::is_ident_byte;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_domain::SymbolOccurrenceId;
use tracedecay_lsp::analyzer::activity::{active_languages_for_files, documents_for_adapter};
use tracedecay_lsp::analyzer::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity, NodeSpan,
    enclosing_node_for_line,
};

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::support::{
    effective_path, generic_tool_result, rendered_tool_result, require_object_args,
    unique_file_paths,
};
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render;

/// True when `path` names a Rust source file (case-insensitive `.rs`). Gates
/// tree-sitter masking, which parses with the Rust grammar and would
/// mis-tokenise other languages.
fn path_is_rust(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

fn path_matches_optional_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    tracedecay_runtime_core::path_scope::path_matches_scope(path, scope_prefix)
}

const ANALYSIS_SYMBOL_BUDGET: usize = 500_000;

#[derive(Clone)]
struct VerifiedAnalysisSymbol {
    occurrence: SymbolOccurrenceId,
    path: String,
    metadata: LineageSymbolRecordV1,
}

impl VerifiedAnalysisSymbol {
    fn end_line(&self) -> u32 {
        self.metadata
            .start_line
            .saturating_add(self.metadata.line_span.saturating_sub(1))
    }
}

fn verified_analysis_symbols(
    graph: &tracedecay_usecases::graph::VerifiedGraphQuery,
    scope_prefix: Option<&str>,
) -> Result<Vec<VerifiedAnalysisSymbol>> {
    let page = graph.symbols_page(None, ANALYSIS_SYMBOL_BUDGET)?;
    if page.has_more {
        return Err(TraceDecayError::project_route(
            "code-graph-budget-exhausted",
            false,
            "verified analysis symbol census exceeded its declared budget",
        ));
    }
    page.symbols
        .into_iter()
        .map(|symbol| {
            let path = symbol
                .binding
                .and_then(|binding| binding.logical_path)
                .ok_or_else(|| {
                    TraceDecayError::project_route(
                        "code-graph-corrupt",
                        false,
                        "verified analysis symbol is missing its logical file binding",
                    )
                })?;
            let metadata = symbol.metadata.ok_or_else(|| {
                TraceDecayError::project_route(
                    "code-graph-corrupt",
                    false,
                    "verified analysis symbol is missing extraction-attested metadata",
                )
            })?;
            Ok(VerifiedAnalysisSymbol {
                occurrence: symbol.occurrence,
                path,
                metadata,
            })
        })
        .filter_map(|result| match result {
            Ok(symbol) if path_matches_optional_scope(&symbol.path, scope_prefix) => {
                Some(Ok(symbol))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}
