//! `tracedecay_complexity`, `tracedecay_doc_coverage`, and `tracedecay_god_class`.

use super::*;
use tracedecay_code_index::intake::content_digest;
use tracedecay_contracts::retrieval::{
    ComplexityReportEntryV1, ComplexityReportV1, ComplexitySurfaceRequestV1, DocCoverageFileV1,
    DocCoverageResultV1, DocCoverageSurfaceRequestV1, DocCoverageSymbolV1, GodClassEntryV1,
    GodClassResultV1, GodClassSurfaceRequestV1,
};
use tracedecay_privacy::{CodeSourceShapeV1, sanitize_code_source_bytes};

#[hotpath::measure(future = true, label = "mcp.analysis.complexity.total")]
pub(super) async fn compute_complexity(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: ComplexitySurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_complexity")?;
    let node_kind = request
        .node_kind
        .as_deref()
        .map(|kind| requested_node_kind("tracedecay_complexity", kind))
        .transpose()?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.complexity.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[])?;
        (symbols, edges)
    });
    let (symbols, fan_in, fan_out) = hotpath::measure_block!("mcp.analysis.complexity.compute", {
        let mut fan_in = HashMap::<SymbolOccurrenceId, u64>::new();
        let mut fan_out = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            *fan_out.entry(edge.from_occurrence).or_default() += 1;
            *fan_in.entry(edge.to_occurrence).or_default() += 1;
        }
        if let Some(kind) = node_kind {
            symbols
                .retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
        }
        symbols.sort_by(|left, right| {
            analysis_score(right, &fan_in, &fan_out)
                .cmp(&analysis_score(left, &fan_in, &fan_out))
                .then_with(|| left.occurrence.cmp(&right.occurrence))
        });
        symbols.truncate(limit);
        (symbols, fan_in, fan_out)
    });

    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let ranking: Vec<ComplexityReportEntryV1> = symbols
        .iter()
        .map(|symbol| {
            let metadata = &symbol.metadata;
            // Counters are published only when the bounded walk covered the
            // body; otherwise the analysis state stands in for them.
            let complexity = metadata.exact_complexity();
            ComplexityReportEntryV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: symbol.path.clone(),
                line: user_line(metadata.start_line),
                lines: metadata.line_span,
                cyclomatic_complexity: complexity
                    .map(|complexity| complexity.branches.saturating_add(1)),
                branches: complexity.map(|complexity| complexity.branches),
                loops: complexity.map(|complexity| complexity.loops),
                max_nesting: complexity.map(|complexity| complexity.max_nesting),
                complexity_analysis: metadata.complexity_analysis,
                fan_out: fan_out.get(&symbol.occurrence).copied().unwrap_or(0),
                fan_in: fan_in.get(&symbol.occurrence).copied().unwrap_or(0),
                score: analysis_score(symbol, &fan_in, &fan_out),
            }
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::Complexity(ComplexityReportV1 {
            formula: "lines + (fan_out × 3) + fan_in".to_owned(),
            note: "cyclomatic_complexity = branches + 1 (computed from AST during extraction); counters are null when complexity_analysis is not complete".to_owned(),
            result_count: ranking.len() as u64,
            ranking,
        }),
        touched_files,
    ))
}

fn analysis_score(
    symbol: &VerifiedAnalysisSymbol,
    fan_in: &HashMap<SymbolOccurrenceId, u64>,
    fan_out: &HashMap<SymbolOccurrenceId, u64>,
) -> u64 {
    u64::from(symbol.metadata.line_span)
        .saturating_add(
            fan_out
                .get(&symbol.occurrence)
                .copied()
                .unwrap_or(0)
                .saturating_mul(3),
        )
        .saturating_add(fan_in.get(&symbol.occurrence).copied().unwrap_or(0))
}

fn is_documentable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function"
            | "method"
            | "class"
            | "interface"
            | "trait"
            | "struct"
            | "enum"
            | "module"
            | "field"
            | "enum_variant"
            | "const"
            | "static"
            | "type_alias"
            | "property"
            | "csharp_property"
            | "record"
            | "data_class"
            | "sealed_class"
            | "object"
            | "case_class"
            | "kotlin_object"
            | "inner_class"
            | "abstract_method"
            | "constructor"
            | "struct_method"
            | "val"
            | "var"
            | "mixin"
            | "extension"
            | "union"
            | "typedef"
    )
}

const DOC_COVERAGE_SYMBOL_BUDGET: usize = 500_000;

fn doc_coverage_unavailable(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("verified-doc-coverage-unavailable", false, detail.into())
}

fn admitted_doc_source(project_root: &Path, path: &str) -> Result<Vec<u8>> {
    let raw = std::fs::read(project_root.join(path)).map_err(|error| {
        doc_coverage_unavailable(format!(
            "verified documentation source `{path}` could not be read: {error}"
        ))
    })?;
    let shape = match path.rsplit('.').next() {
        Some("json" | "toml" | "yaml" | "yml") => CodeSourceShapeV1::StructuredData,
        _ => CodeSourceShapeV1::CodeOrProse,
    };
    let sanitized = sanitize_code_source_bytes(&raw, shape).map_err(|error| {
        doc_coverage_unavailable(format!(
            "verified documentation source `{path}` could not be admitted through the code sanitizer: {error}"
        ))
    })?;
    Ok(sanitized.into_parts().0)
}

/// Refuses a documentation census whose public symbols no longer match the
/// source on disk: the admitted generation's content digest must agree with
/// every candidate's current bytes, or the report would describe a tree the
/// caller is not looking at.
fn verify_doc_coverage_sources_current(
    project_root: &Path,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    path_prefix: Option<&str>,
) -> Result<()> {
    let page = graph.symbols_page(None, DOC_COVERAGE_SYMBOL_BUDGET)?;
    if page.has_more {
        return Err(doc_coverage_unavailable(
            "verified documentation census exceeded its declared symbol budget",
        ));
    }
    let mut candidates = Vec::new();
    for symbol in page.symbols {
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "symbol {} has no admitted documentation metadata",
                symbol.occurrence.as_str()
            ))
        })?;
        let path = symbol
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref())
            .ok_or_else(|| {
                doc_coverage_unavailable(format!(
                    "symbol {} has no admitted logical file binding",
                    symbol.occurrence.as_str()
                ))
            })?;
        if metadata.visibility == "public"
            && is_documentable_kind(&metadata.kind)
            && path_matches_optional_scope(path, path_prefix)
        {
            candidates.push(symbol);
        }
    }
    candidates.sort_by(|left, right| {
        left.binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref())
            .cmp(
                &right
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_deref()),
            )
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });

    let mut admitted_path = None::<String>;
    let mut admitted_bytes = Vec::new();
    for symbol in candidates {
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate metadata disappeared")
        })?;
        let binding = symbol.binding.as_ref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate file binding disappeared")
        })?;
        let path = binding.logical_path.as_deref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate logical path disappeared")
        })?;
        if admitted_path.as_deref() != Some(path) {
            admitted_bytes = admitted_doc_source(project_root, path)?;
            admitted_path = Some(path.to_owned());
        }
        let source_span = binding.source_span.ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "public symbol {} has no admitted source span",
                symbol.occurrence.as_str()
            ))
        })?;
        let start = usize::try_from(source_span.start_byte).map_err(|error| {
            doc_coverage_unavailable(format!(
                "public symbol {} source start does not fit this host: {error}",
                symbol.occurrence.as_str()
            ))
        })?;
        let end = usize::try_from(source_span.end_byte).map_err(|error| {
            doc_coverage_unavailable(format!(
                "public symbol {} source end does not fit this host: {error}",
                symbol.occurrence.as_str()
            ))
        })?;
        let source = admitted_bytes.get(start..end).ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "public symbol {} source span is outside `{path}`",
                symbol.occurrence.as_str()
            ))
        })?;
        if content_digest(source) != metadata.content_digest {
            return Err(doc_coverage_unavailable(format!(
                "documentation source for symbol {} no longer matches the admitted graph generation",
                symbol.occurrence.as_str()
            )));
        }
    }
    Ok(())
}

#[hotpath::measure(future = true, label = "mcp.analysis.doc_coverage.total")]
pub(super) async fn compute_doc_coverage(
    project_root: &Path,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: DocCoverageSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_doc_coverage")?;
    let path_prefix = request.path.as_deref().or(scope_prefix);
    verify_doc_coverage_sources_current(project_root, graph, path_prefix)?;
    let limit = request.limit.map_or(50, |value| value.min(500) as usize);
    let mut symbols = verified_analysis_symbols(graph, path_prefix)?
        .into_iter()
        .filter(|symbol| {
            symbol.metadata.visibility == "public"
                && symbol
                    .metadata
                    .docstring
                    .as_deref()
                    .is_none_or(|docstring| docstring.trim().is_empty())
                && is_documentable_kind(&symbol.metadata.kind)
        })
        .collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.metadata.start_line.cmp(&right.metadata.start_line))
            .then(left.occurrence.cmp(&right.occurrence))
    });
    let total_undocumented = symbols.len();
    symbols.truncate(limit);
    let returned_count = symbols.len();

    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let mut by_file = HashMap::<String, Vec<DocCoverageSymbolV1>>::new();
    for symbol in symbols {
        by_file
            .entry(symbol.path)
            .or_default()
            .push(DocCoverageSymbolV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: symbol.metadata.simple_name,
                kind: symbol.metadata.kind,
                line: user_line(symbol.metadata.start_line),
                signature: symbol.metadata.signature,
            });
    }
    let mut files = by_file
        .into_iter()
        .map(|(file, symbols)| DocCoverageFileV1 {
            file,
            count: symbols.len() as u64,
            symbols,
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.file.cmp(&right.file))
    });
    Ok(graph_tool_completion(
        GraphToolResultV1::DocCoverage(DocCoverageResultV1 {
            path_filter: path_prefix.map(str::to_owned),
            total_undocumented: total_undocumented as u64,
            returned_count: returned_count as u64,
            omitted_count: total_undocumented.saturating_sub(returned_count) as u64,
            complete: returned_count == total_undocumented,
            limit: limit as u64,
            file_count: files.len() as u64,
            files,
        }),
        touched_files,
    ))
}

#[hotpath::measure(future = true, label = "mcp.analysis.god_class.total")]
pub(super) async fn compute_god_class(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: GodClassSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_god_class")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.god_class.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[RelationEdgeKindV1::Contains])?;
        (symbols, edges)
    });
    let (symbols, counts) = hotpath::measure_block!("mcp.analysis.god_class.compute", {
        let by_occurrence = symbols
            .iter()
            .map(|symbol| (symbol.occurrence.clone(), symbol))
            .collect::<HashMap<_, _>>();
        let mut counts = HashMap::<SymbolOccurrenceId, (u64, u64)>::new();
        for edge in edges {
            let Some(child) = by_occurrence.get(&edge.to_occurrence) else {
                return Err(verified_analysis_unavailable(
                    "god-class",
                    "a containment edge endpoint is absent from the admitted symbol census",
                ));
            };
            let count = counts.entry(edge.from_occurrence).or_default();
            match child.metadata.kind.as_str() {
                "function" | "method" | "arrow_function" => count.0 += 1,
                "field" | "val_field" | "var_field" => count.1 += 1,
                _ => {}
            }
        }
        symbols.retain(|symbol| matches!(symbol.metadata.kind.as_str(), "class" | "struct"));
        symbols.sort_by(|left, right| {
            let left_counts = counts.get(&left.occurrence).copied().unwrap_or_default();
            let right_counts = counts.get(&right.occurrence).copied().unwrap_or_default();
            right_counts
                .0
                .saturating_add(right_counts.1)
                .cmp(&left_counts.0.saturating_add(left_counts.1))
                .then_with(|| left.occurrence.cmp(&right.occurrence))
        });
        symbols.truncate(limit);
        (symbols, counts)
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let ranking: Vec<GodClassEntryV1> = symbols
        .into_iter()
        .map(|symbol| {
            let (methods, fields) = counts.get(&symbol.occurrence).copied().unwrap_or_default();
            GodClassEntryV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: symbol.metadata.simple_name,
                kind: symbol.metadata.kind,
                file: symbol.path,
                line: user_line(symbol.metadata.start_line),
                methods,
                fields,
                total_members: methods.saturating_add(fields),
            }
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::GodClass(GodClassResultV1 {
            result_count: ranking.len() as u64,
            ranking,
        }),
        touched_files,
    ))
}
