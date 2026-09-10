//! `tracedecay_complexity`, `tracedecay_doc_coverage`, and `tracedecay_god_class`.

use super::*;

#[hotpath::measure(future = true, label = "mcp.analysis.complexity.total")]
pub async fn handle_complexity(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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

    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.complexity.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[])?;
        (symbols, edges)
    });
    let (symbols, fan_in, fan_out) = hotpath::measure_block!("mcp.analysis.complexity.compute", {
        let mut fan_in = HashMap::<SymbolOccurrenceId, u64>::new();
        let mut fan_out = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            *fan_out.entry(edge.edge.from_occurrence).or_default() += 1;
            *fan_in.entry(edge.edge.to_occurrence).or_default() += 1;
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
    let output = hotpath::measure_block!("mcp.analysis.complexity.assemble", {
        let items: Vec<Value> = symbols
            .iter()
            .map(|symbol| {
                let metadata = &symbol.metadata;
                let incoming = fan_in.get(&symbol.occurrence).copied().unwrap_or(0);
                let outgoing = fan_out.get(&symbol.occurrence).copied().unwrap_or(0);
                // Counters are published only when the bounded walk covered
                // the body; otherwise the analysis state stands in for them.
                let complexity = metadata.exact_complexity();
                json!({
                    "id": symbol.occurrence.as_str(),
                    "name": metadata.simple_name,
                    "kind": metadata.kind,
                    "file": symbol.path,
                    "line": metadata.start_line,
                    "lines": metadata.line_span,
                    "cyclomatic_complexity": complexity.map(|complexity| complexity.branches.saturating_add(1)),
                    "branches": complexity.map(|complexity| complexity.branches),
                    "loops": complexity.map(|complexity| complexity.loops),
                    "max_nesting": complexity.map(|complexity| complexity.max_nesting),
                    "complexity_analysis": metadata.complexity_analysis,
                    "fan_out": outgoing,
                    "fan_in": incoming,
                    "score": analysis_score(symbol, &fan_in, &fan_out),
                })
            })
            .collect();
        json!({
            "formula": "lines + (fan_out × 3) + fan_in",
            "note": "cyclomatic_complexity = branches + 1 (computed from AST during extraction); counters are null when complexity_analysis is not complete",
            "result_count": items.len(),
            "ranking": items,
        })
    });

    Ok(generic_tool_result(
        Some(graph.project_root()?),
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

pub fn is_documentable_kind(kind: &str) -> bool {
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

#[hotpath::measure(future = true, label = "mcp.analysis.doc_coverage.total")]
pub async fn handle_doc_coverage(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(50, |value| value.min(500) as usize);
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
    let mut by_file = HashMap::<String, Vec<Value>>::new();
    for symbol in &symbols {
        by_file.entry(symbol.path.clone()).or_default().push(json!({
            "id": symbol.occurrence.as_str(),
            "name": symbol.metadata.simple_name,
            "kind": symbol.metadata.kind,
            "line": symbol.metadata.start_line.saturating_add(1),
            "signature": symbol.metadata.signature,
        }));
    }
    let mut files = by_file
        .into_iter()
        .map(|(file, symbols)| {
            json!({
                "file": file,
                "count": symbols.len(),
                "symbols": symbols,
            })
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        right["count"]
            .as_u64()
            .cmp(&left["count"].as_u64())
            .then_with(|| left["file"].as_str().cmp(&right["file"].as_str()))
    });
    let output = json!({
        "path_filter": path_prefix,
        "total_undocumented": total_undocumented,
        "returned_count": returned_count,
        "omitted_count": total_undocumented.saturating_sub(returned_count),
        "complete": returned_count == total_undocumented,
        "limit": limit,
        "file_count": files.len(),
        "files": files,
    });
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}

#[hotpath::measure(future = true, label = "mcp.analysis.god_class.total")]
pub async fn handle_god_class(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

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
        (symbols, counts)
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let output = hotpath::measure_block!("mcp.analysis.god_class.assemble", {
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
        json!({
            "result_count": items.len(),
            "ranking": items,
        })
    });

    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}
