//! Portable structural-analysis report handlers.

mod circular;
mod complexity;
mod constructors;
mod dead_code;
mod field_sites;
mod hotspots;
mod imports;
mod metrics;
mod recursion;
#[cfg(feature = "source-analysis")]
mod unmounted_files;
mod unsafe_patterns;

pub use circular::handle_circular;
pub use complexity::{handle_complexity, handle_doc_coverage, handle_god_class};
pub use constructors::handle_constructors;
pub use dead_code::handle_dead_code;
pub use field_sites::handle_field_sites;
pub use hotspots::handle_hotspots;
pub use imports::handle_unused_imports;
pub use metrics::{
    handle_coupling, handle_distribution, handle_inheritance_depth, handle_largest, handle_rank,
};
pub use recursion::handle_recursion;
#[cfg(feature = "source-analysis")]
pub use unmounted_files::handle_unmounted_files;
pub use unsafe_patterns::handle_unsafe_patterns;

use crate::{ToolResult, is_ident_byte, line_number_at, skip_ascii_whitespace};
use crate::{
    effective_path, generic_tool_result, rendered_tool_result, require_object_args,
    require_positive_limit, unique_file_paths,
};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_query::{CodeGraphSemanticEdgeV1, LineageSymbolRecordV1, VerifiedGraphQuery};

fn path_is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn path_matches_optional_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    tracedecay_runtime_core::path_scope::path_matches_scope(path, scope_prefix)
}

const ANALYSIS_SYMBOL_BUDGET: usize = 500_000;
const ANALYSIS_RELATION_BUDGET: usize = 2_000_000;

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
    graph: &VerifiedGraphQuery,
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

fn verified_analysis_edges(
    graph: &VerifiedGraphQuery,
    symbols: &[VerifiedAnalysisSymbol],
    kinds: &[RelationEdgeKindV1],
) -> Result<Vec<CodeGraphSemanticEdgeV1>> {
    let occurrences = symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    graph.edges_among(&occurrences, kinds, ANALYSIS_RELATION_BUDGET)
}

fn verified_analysis_unavailable(capability: &str, detail: &str) -> TraceDecayError {
    TraceDecayError::project_route(format!("verified-{capability}-unavailable"), false, detail)
}
