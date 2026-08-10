//! `tracedecay_complexity`, `tracedecay_doc_coverage`, and `tracedecay_god_class`.

use super::*;

/// Handles `tracedecay_complexity` tool calls.
pub(crate) async fn handle_complexity(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let node_kind = args
        .get("node_kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let mut symbols = verified_analysis_symbols(graph, path_prefix)?;
    let edges = verified_analysis_edges(graph, &symbols, &[])?;
    let mut fan_in = HashMap::<SymbolOccurrenceId, u64>::new();
    let mut fan_out = HashMap::<SymbolOccurrenceId, u64>::new();
    for edge in edges {
        *fan_out.entry(edge.edge.from_occurrence).or_default() += 1;
        *fan_in.entry(edge.edge.to_occurrence).or_default() += 1;
    }
    if let Some(kind) = node_kind {
        symbols.retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
    }
    symbols.sort_by(|left, right| {
        analysis_score(right, &fan_in, &fan_out)
            .cmp(&analysis_score(left, &fan_in, &fan_out))
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);

    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let items: Vec<Value> = symbols
        .iter()
        .map(|symbol| {
            let metadata = &symbol.metadata;
            let incoming = fan_in.get(&symbol.occurrence).copied().unwrap_or(0);
            let outgoing = fan_out.get(&symbol.occurrence).copied().unwrap_or(0);
            json!({
                "id": symbol.occurrence.as_str(),
                "name": metadata.simple_name,
                "kind": metadata.kind,
                "file": symbol.path,
                "line": metadata.start_line,
                "lines": metadata.line_span,
                "cyclomatic_complexity": metadata.branches.saturating_add(1),
                "branches": metadata.branches,
                "loops": metadata.loops,
                "max_nesting": metadata.max_nesting,
                "fan_out": outgoing,
                "fan_in": incoming,
                "score": analysis_score(symbol, &fan_in, &fan_out),
            })
        })
        .collect();

    let output = json!({
        "formula": "lines + (fan_out × 3) + fan_in",
        "note": "cyclomatic_complexity = branches + 1 (computed from AST during extraction)",
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
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

/// Handles `tracedecay_doc_coverage` tool calls.
pub(crate) async fn handle_doc_coverage(
    _cg: &TraceDecay,
    _graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    _args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    Err(verified_analysis_unavailable(
        "doc-coverage",
        "the admitted graph generation does not publish documentation evidence",
    ))
}

/// Handles `tracedecay_god_class` tool calls.
pub(crate) async fn handle_god_class(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let mut symbols = verified_analysis_symbols(graph, path_prefix)?;
    let by_occurrence = symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol))
        .collect::<HashMap<_, _>>();
    let edges = verified_analysis_edges(graph, &symbols, &[RelationEdgeKindV1::Contains])?;
    let mut counts = HashMap::<SymbolOccurrenceId, (u64, u64)>::new();
    for edge in edges {
        let Some(child) = by_occurrence.get(&edge.edge.to_occurrence) else {
            return Err(verified_analysis_unavailable(
                "god-class",
                "a containment edge endpoint is absent from the admitted symbol census",
            ));
        };
        let count = counts.entry(edge.edge.from_occurrence).or_default();
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
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let items: Vec<Value> = symbols
        .iter()
        .map(|symbol| {
            let (methods, fields) = counts.get(&symbol.occurrence).copied().unwrap_or_default();
            json!({
                "id": symbol.occurrence.as_str(),
                "name": symbol.metadata.simple_name,
                "kind": symbol.metadata.kind,
                "file": symbol.path,
                "line": symbol.metadata.start_line,
                "methods": methods,
                "fields": fields,
                "total_members": methods.saturating_add(fields),
            })
        })
        .collect();

    let output = json!({
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

// ---------------------------------------------------------------------------
// tracedecay_unsafe_patterns
// ---------------------------------------------------------------------------
