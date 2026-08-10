//! Ranking and distribution reports: `tracedecay_rank`, `tracedecay_largest`, `tracedecay_coupling`, `tracedecay_inheritance_depth`, `tracedecay_distribution`.

use super::*;

/// Handles `tracedecay_rank` tool calls.
pub(crate) async fn handle_rank(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    use crate::types::EdgeKind;
    require_object_args(&args, "tracedecay_rank")?;

    let edge_kind_str = args
        .get("edge_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: edge_kind".to_string(),
        })?;

    let edge_kind = EdgeKind::from_str(edge_kind_str).ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "invalid edge_kind '{edge_kind_str}'. Valid values: implements, extends, calls, uses, contains, annotates, derives_macro"
        ),
    })?;

    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("incoming");

    let incoming = match direction {
        "incoming" => true,
        "outgoing" => false,
        _ => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "invalid direction '{direction}'. Valid values: incoming, outgoing"
                ),
            });
        }
    };

    let node_kind = args
        .get("node_kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let relation_kind = match edge_kind {
        EdgeKind::Contains => RelationEdgeKindV1::Contains,
        EdgeKind::Calls => RelationEdgeKindV1::Calls,
        EdgeKind::Uses => RelationEdgeKindV1::Uses,
        EdgeKind::Implements => RelationEdgeKindV1::Implements,
        EdgeKind::TypeOf => RelationEdgeKindV1::TypeOf,
        EdgeKind::Returns => RelationEdgeKindV1::Returns,
        EdgeKind::Extends => RelationEdgeKindV1::Extends,
        EdgeKind::Annotates => RelationEdgeKindV1::Annotates,
        EdgeKind::Receives => RelationEdgeKindV1::Receives,
        EdgeKind::DerivesMacro => {
            return Err(verified_analysis_unavailable(
                "rank",
                "the admitted graph generation does not publish derives_macro relations",
            ));
        }
    };
    let mut symbols = verified_analysis_symbols(graph, path_prefix)?;
    let edges = verified_analysis_edges(graph, &symbols, &[relation_kind])?;
    let mut counts = HashMap::<SymbolOccurrenceId, u64>::new();
    for edge in edges {
        let occurrence = if incoming {
            edge.edge.to_occurrence
        } else {
            edge.edge.from_occurrence
        };
        *counts.entry(occurrence).or_default() += 1;
    }
    if let Some(kind) = node_kind {
        symbols.retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
    }
    symbols.sort_by(|left, right| {
        counts
            .get(&right.occurrence)
            .copied()
            .unwrap_or(0)
            .cmp(&counts.get(&left.occurrence).copied().unwrap_or(0))
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let items: Vec<Value> = symbols
        .iter()
        .map(|symbol| {
            json!({
                "id": symbol.occurrence.as_str(),
                "name": symbol.metadata.simple_name,
                "kind": symbol.metadata.kind,
                "file": symbol.path,
                "line": symbol.metadata.start_line,
                "count": counts.get(&symbol.occurrence).copied().unwrap_or(0),
            })
        })
        .collect();

    let output = json!({
        "edge_kind": edge_kind_str,
        "direction": direction,
        "node_kind_filter": args.get("node_kind").and_then(|v| v.as_str()),
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

/// Handles `tracedecay_largest` tool calls.
pub(crate) async fn handle_largest(
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
    if let Some(kind) = node_kind {
        symbols.retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
    }
    symbols.sort_by(|left, right| {
        right
            .metadata
            .line_span
            .cmp(&left.metadata.line_span)
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let items: Vec<Value> = symbols
        .iter()
        .map(|symbol| {
            json!({
                "id": symbol.occurrence.as_str(),
                "name": symbol.metadata.simple_name,
                "kind": symbol.metadata.kind,
                "file": symbol.path,
                "start_line": symbol.metadata.start_line,
                "end_line": symbol.end_line(),
                "lines": symbol.metadata.line_span,
            })
        })
        .collect();

    let output = json!({
        "node_kind_filter": args.get("node_kind").and_then(|v| v.as_str()),
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

/// Handles `tracedecay_coupling` tool calls.
pub(crate) async fn handle_coupling(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("fan_in");

    let fan_in = match direction {
        "fan_in" => true,
        "fan_out" => false,
        _ => {
            return Err(TraceDecayError::Config {
                message: format!("invalid direction '{direction}'. Valid values: fan_in, fan_out"),
            });
        }
    };

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let symbols = verified_analysis_symbols(graph, path_prefix)?;
    let paths = symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol.path.clone()))
        .collect::<HashMap<_, _>>();
    let mut coupled = HashMap::<String, HashSet<String>>::new();
    for edge in verified_analysis_edges(graph, &symbols, &[])? {
        let (Some(source), Some(target)) = (
            paths.get(&edge.edge.from_occurrence),
            paths.get(&edge.edge.to_occurrence),
        ) else {
            return Err(verified_analysis_unavailable(
                "coupling",
                "a relation endpoint is absent from the admitted symbol census",
            ));
        };
        if source != target {
            let (key, value) = if fan_in {
                (target, source)
            } else {
                (source, target)
            };
            coupled
                .entry(key.clone())
                .or_default()
                .insert(value.clone());
        }
    }
    let mut results = coupled
        .into_iter()
        .map(|(path, related)| (path, related.len()))
        .collect::<Vec<_>>();
    results.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    results.truncate(limit);
    let items: Vec<Value> = results
        .iter()
        .map(|(file, count)| {
            json!({
                "file": file,
                "coupled_files": count,
            })
        })
        .collect();

    let output = json!({
        "direction": direction,
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}

/// Handles `tracedecay_inheritance_depth` tool calls.
pub(crate) async fn handle_inheritance_depth(
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
    let mut parents = HashMap::<SymbolOccurrenceId, Vec<SymbolOccurrenceId>>::new();
    for edge in verified_analysis_edges(graph, &symbols, &[RelationEdgeKindV1::Extends])? {
        parents
            .entry(edge.edge.from_occurrence)
            .or_default()
            .push(edge.edge.to_occurrence);
    }
    let mut memo = HashMap::<SymbolOccurrenceId, u64>::new();
    for symbol in &symbols {
        inheritance_depth(&symbol.occurrence, &parents, &mut HashSet::new(), &mut memo)?;
    }
    symbols.sort_by(|left, right| {
        memo.get(&right.occurrence)
            .copied()
            .unwrap_or(0)
            .cmp(&memo.get(&left.occurrence).copied().unwrap_or(0))
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let items: Vec<Value> = symbols
        .iter()
        .map(|symbol| {
            json!({
                "id": symbol.occurrence.as_str(),
                "name": symbol.metadata.simple_name,
                "kind": symbol.metadata.kind,
                "file": symbol.path,
                "line": symbol.metadata.start_line,
                "depth": memo.get(&symbol.occurrence).copied().unwrap_or(0),
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

fn inheritance_depth(
    occurrence: &SymbolOccurrenceId,
    parents: &HashMap<SymbolOccurrenceId, Vec<SymbolOccurrenceId>>,
    visiting: &mut HashSet<SymbolOccurrenceId>,
    memo: &mut HashMap<SymbolOccurrenceId, u64>,
) -> Result<u64> {
    if let Some(depth) = memo.get(occurrence) {
        return Ok(*depth);
    }
    if !visiting.insert(occurrence.clone()) {
        return Err(verified_analysis_unavailable(
            "inheritance-depth",
            "the admitted extends relation contains a cycle",
        ));
    }
    let mut depth = 0u64;
    if let Some(parent_occurrences) = parents.get(occurrence) {
        for parent in parent_occurrences {
            depth =
                depth.max(inheritance_depth(parent, parents, visiting, memo)?.saturating_add(1));
        }
    }
    visiting.remove(occurrence);
    memo.insert(occurrence.clone(), depth);
    Ok(depth)
}

/// Handles `tracedecay_distribution` tool calls.
pub(crate) async fn handle_distribution(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_distribution")?;
    let path_prefix = effective_path(&args, scope_prefix);
    let summary = args
        .get("summary")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let symbols = verified_analysis_symbols(graph, path_prefix)?;
    let output = if summary {
        let mut totals = HashMap::<String, u64>::new();
        for symbol in &symbols {
            *totals.entry(symbol.metadata.kind.clone()).or_default() += 1;
        }
        let mut sorted = totals.into_iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let items: Vec<Value> = sorted
            .iter()
            .map(|(kind, count)| json!({ "kind": kind, "count": count }))
            .collect();

        json!({
            "path_filter": path_prefix,
            "mode": "summary",
            "total_kinds": items.len(),
            "distribution": items,
        })
    } else {
        let file_limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(100u64, |v| v.clamp(1, 1000));
        let mut counts = HashMap::<String, HashMap<String, u64>>::new();
        for symbol in &symbols {
            *counts
                .entry(symbol.path.clone())
                .or_default()
                .entry(symbol.metadata.kind.clone())
                .or_default() += 1;
        }
        let total_files = u64::try_from(counts.len()).unwrap_or(u64::MAX);
        let mut by_file = counts.into_iter().collect::<Vec<_>>();
        by_file.sort_by(|left, right| {
            let left_count = left.1.values().copied().sum::<u64>();
            let right_count = right.1.values().copied().sum::<u64>();
            right_count
                .cmp(&left_count)
                .then_with(|| left.0.cmp(&right.0))
        });
        by_file.truncate(file_limit as usize);
        let items: Vec<Value> = by_file
            .iter()
            .map(|(file, counts)| {
                let mut kinds = counts.iter().collect::<Vec<_>>();
                kinds.sort_by(|left, right| left.0.cmp(right.0));
                let kinds = kinds
                    .into_iter()
                    .map(|(kind, count)| json!({ "kind": kind, "count": count }))
                    .collect::<Vec<_>>();
                json!({ "file": file, "kinds": kinds })
            })
            .collect();
        let returned = u64::try_from(items.len()).unwrap_or(u64::MAX);

        json!({
            "path_filter": path_prefix,
            "mode": "per_file",
            "file_count": items.len(),
            "total_file_count": total_files,
            "omitted_file_count": total_files.saturating_sub(returned),
            "files": items,
        })
    };

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}
