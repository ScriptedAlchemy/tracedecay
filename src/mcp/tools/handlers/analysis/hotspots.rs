//! `tracedecay_hotspots` — churn-weighted complexity ranking.

use super::*;

/// Handles `tracedecay_hotspots` tool calls.
pub(crate) async fn handle_hotspots(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    require_positive_limit(limit, "tracedecay_hotspots")?;

    let mut symbols = verified_analysis_symbols(graph, scope_prefix)?;
    let edges = verified_analysis_edges(graph, &symbols, &[])?;
    let mut incoming = HashMap::<SymbolOccurrenceId, u64>::new();
    let mut outgoing = HashMap::<SymbolOccurrenceId, u64>::new();
    for edge in edges {
        *outgoing.entry(edge.edge.from_occurrence).or_default() += 1;
        *incoming.entry(edge.edge.to_occurrence).or_default() += 1;
    }
    symbols.sort_by(|left, right| {
        let left_total = incoming
            .get(&left.occurrence)
            .copied()
            .unwrap_or(0)
            .saturating_add(outgoing.get(&left.occurrence).copied().unwrap_or(0));
        let right_total = incoming
            .get(&right.occurrence)
            .copied()
            .unwrap_or(0)
            .saturating_add(outgoing.get(&right.occurrence).copied().unwrap_or(0));
        right_total
            .cmp(&left_total)
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);
    let mut items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for symbol in symbols {
        let incoming = incoming.get(&symbol.occurrence).copied().unwrap_or(0);
        let outgoing = outgoing.get(&symbol.occurrence).copied().unwrap_or(0);
        touched.push(symbol.path.clone());
        items.push(json!({
            "id": symbol.occurrence.as_str(),
            "name": symbol.metadata.simple_name,
            "kind": symbol.metadata.kind,
            "file": symbol.path,
            "line": symbol.metadata.start_line,
            "incoming": incoming,
            "outgoing": outgoing,
            "total": incoming + outgoing,
        }));
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "hotspot_count": items.len(),
        "hotspots": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}
