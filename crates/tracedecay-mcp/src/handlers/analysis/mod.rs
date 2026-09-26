//! Portable structural-analysis reports, computed as their typed catalog
//! results for the project's graph-tool owner.

mod circular;
mod complexity;
mod constructors;
mod dead_code;
mod field_sites;
mod hotspots;
mod metrics;
mod recursion;
mod unmounted_files;
mod unsafe_patterns;

pub use circular::render_circular_md;

use circular::compute_circular;
use complexity::{compute_complexity, compute_doc_coverage, compute_god_class};
use constructors::compute_constructors;
use dead_code::compute_dead_code;
use field_sites::compute_field_sites;
use hotspots::compute_hotspots;
use metrics::{
    compute_coupling, compute_distribution, compute_inheritance_depth, compute_largest,
    compute_rank,
};
use recursion::compute_recursion;
use unmounted_files::compute_unmounted_files;
use unsafe_patterns::compute_unsafe_patterns;

use crate::handlers::graph::{graph_tool_completion, user_line};
use crate::handlers::support::{decode_primitive_request, unknown_tool_error};
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::{is_ident_byte, line_number_at};
use crate::{require_positive_limit, unique_file_paths};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, RelationEdgeKindV1, SourceSpan, SymbolOccurrenceId,
};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// Computes one structural-analysis report over the `health_read` verified
/// graph opened through `open`.
pub async fn compute_analysis_report(
    project_root: &Path,
    open: &VerifiedGraphOpen<'_>,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    // The one analysis report that opens no graph query: its whole finding is
    // that the graph and the compiler disagree, so taking the graph's file set
    // as input would answer the question with the very source under suspicion.
    if operation == ApplicationSurfaceOperation::UnmountedFiles {
        return compute_unmounted_files(project_root, args, scope_prefix).await;
    }
    let graph = open(read("health_read")?).await?;
    match operation {
        ApplicationSurfaceOperation::DeadCode => {
            compute_dead_code(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Circular => compute_circular(&graph, args).await,
        ApplicationSurfaceOperation::Hotspots => compute_hotspots(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::Rank => compute_rank(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::Largest => compute_largest(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::Coupling => compute_coupling(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::InheritanceDepth => {
            compute_inheritance_depth(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Distribution => {
            compute_distribution(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Recursion => {
            compute_recursion(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Complexity => {
            compute_complexity(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::DocCoverage => {
            compute_doc_coverage(project_root, &graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::GodClass => {
            compute_god_class(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::UnsafePatterns => {
            compute_unsafe_patterns(project_root, &graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Constructors => {
            compute_constructors(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::FieldSites => {
            compute_field_sites(&graph, args, scope_prefix).await
        }
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}

/// A requested node kind that names no indexed kind is refused: it could
/// never match a symbol, so filtering by it would silently answer nothing.
fn requested_node_kind(tool_name: &str, kind: &str) -> Result<NodeKind> {
    NodeKind::from_str(kind).ok_or_else(|| TraceDecayError::Config {
        message: format!("invalid arguments for {tool_name}: unknown node kind `{kind}`"),
    })
}

fn path_is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn path_matches_optional_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    tracedecay_domain::path_matches_scope(path, scope_prefix)
}

const ANALYSIS_SYMBOL_BUDGET: usize = 500_000;
const ANALYSIS_RELATION_BUDGET: usize = 2_000_000;

#[derive(Clone)]
struct VerifiedAnalysisSymbol {
    occurrence: SymbolOccurrenceId,
    path: String,
    /// Byte range the declaration occupies in its file. Line numbers cannot
    /// separate two declarations that share one line; this can.
    source_span: Option<SourceSpan>,
    metadata: LineageSymbolRecordV1,
}

impl VerifiedAnalysisSymbol {
    fn end_line(&self) -> u32 {
        self.metadata
            .start_line
            .saturating_add(self.metadata.line_span.saturating_sub(1))
    }
}

/// Innermost declaration whose source range covers `match_byte`.
///
/// Byte containment, not line containment: an attribute such as `#[test]` and
/// the two functions on `#[test] fn a() {…} fn b() {…}` all sit on one line,
/// and only the byte range says which of them the site is inside. Selecting by
/// line also had no stable order to break ties with, since symbols arrive in
/// occurrence order and occurrence ids are per-project digests.
fn enclosing_declaration(
    nodes: &[VerifiedAnalysisSymbol],
    match_byte: u64,
) -> Option<&VerifiedAnalysisSymbol> {
    nodes
        .iter()
        .filter_map(|node| node.source_span.map(|span| (node, span)))
        .filter(|(_, span)| span.start_byte <= match_byte && match_byte < span.end_byte)
        .min_by_key(|(_, span)| span.end_byte.saturating_sub(span.start_byte))
        .map(|(node, _)| node)
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
            let source_span = symbol
                .binding
                .as_ref()
                .and_then(|binding| binding.source_span);
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
                source_span,
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
) -> Result<Vec<CanonicalRelationEdgeV1>> {
    let occurrences = symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    graph.edges_among(&occurrences, kinds, ANALYSIS_RELATION_BUDGET)
}

fn verified_analysis_unavailable(capability: &str, detail: &str) -> TraceDecayError {
    TraceDecayError::project_route(format!("verified-{capability}-unavailable"), false, detail)
}
