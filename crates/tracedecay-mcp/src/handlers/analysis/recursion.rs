//! `tracedecay_recursion`, self-recursive and mutually recursive symbol detection.

use tracedecay_contracts::retrieval::{
    RecursionCycleV1, RecursionResultV1, RecursionSurfaceRequestV1, RecursionSymbolV1,
};

use super::*;

/// Detects cycles in the call graph using iterative DFS on the calls-only
/// edge subgraph. Each cycle is a vec of node IDs forming the loop.
#[hotpath::measure(future = true, label = "mcp.analysis.recursion.total")]
pub(super) async fn compute_recursion(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: RecursionSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_recursion")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    require_positive_limit(limit, "tracedecay_recursion")?;

    let (symbols, call_edges) = hotpath::measure_block!("mcp.analysis.recursion.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let call_edges = verified_analysis_edges(graph, &symbols, &[RelationEdgeKindV1::Calls])?;
        (symbols, call_edges)
    });
    let (cycles, symbol_by_id) = hotpath::measure_block!("mcp.analysis.recursion.compute", {
        let symbol_by_id = symbols
            .iter()
            .map(|symbol| (symbol.occurrence.as_str().to_string(), symbol))
            .collect::<HashMap<_, _>>();
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        for edge in call_edges {
            let src = edge.from_occurrence.as_str().to_string();
            let tgt = edge.to_occurrence.as_str().to_string();
            adj.entry(src).or_default().insert(tgt.clone());
            adj.entry(tgt).or_default();
        }

        // Collect only the cyclic SCCs, then sort smallest-first so we keep
        // shorter / more interesting cycles when the cap kicks in. We still need
        // every cyclic SCC enumerated before sorting (truncating early would bias
        // toward Tarjan emission order), but we cap the per-SCC path search.
        let mut cyclic_sccs: Vec<Vec<String>> = tracedecay_graph_query::scc::tarjan_scc(&adj)
            .into_iter()
            .filter(|scc| tracedecay_graph_query::scc::is_cyclic_scc(scc, &adj))
            .collect();
        cyclic_sccs.sort_by_key(Vec::len);

        let mut cycles: Vec<Vec<String>> = Vec::new();
        for mut scc in cyclic_sccs {
            if cycles.len() >= limit {
                break;
            }
            if let Some(path) = cycle_path_for_scc(&mut scc, &adj) {
                cycles.push(path);
            }
        }
        cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        cycles.truncate(limit);
        (cycles, symbol_by_id)
    });

    let mut result_cycles: Vec<RecursionCycleV1> = Vec::with_capacity(cycles.len());
    let mut touched: Vec<&str> = Vec::new();
    for cycle in &cycles {
        let mut chain: Vec<RecursionSymbolV1> = Vec::with_capacity(cycle.len());
        for node_id in cycle {
            let Some(symbol) = symbol_by_id.get(node_id) else {
                return Err(verified_analysis_unavailable(
                    "recursion",
                    "a call-cycle endpoint is absent from the admitted symbol census",
                ));
            };
            touched.push(symbol.path.as_str());
            chain.push(RecursionSymbolV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: symbol.metadata.simple_name.clone(),
                kind: symbol.metadata.kind.clone(),
                file: symbol.path.clone(),
                line: user_line(symbol.metadata.start_line),
            });
        }
        result_cycles.push(RecursionCycleV1 {
            length: cycle.len().saturating_sub(1) as u64,
            chain,
        });
    }
    let touched_files = unique_file_paths(touched.into_iter());

    Ok(graph_tool_completion(
        GraphToolResultV1::Recursion(RecursionResultV1 {
            cycle_count: result_cycles.len() as u64,
            cycles: result_cycles,
        }),
        touched_files,
    ))
}

fn cycle_path_for_scc(
    scc: &mut [String],
    adj: &HashMap<String, HashSet<String>>,
) -> Option<Vec<String>> {
    scc.sort();
    let scc_set: HashSet<&str> = scc.iter().map(std::string::String::as_str).collect();
    if scc.len() == 1 {
        let id = scc[0].clone();
        if adj
            .get(&id)
            .is_some_and(|neighbors| neighbors.contains(&id))
        {
            return Some(vec![id.clone(), id]);
        }
        return None;
    }

    for start in scc.iter() {
        // `path` and `seen` operate on borrowed ids from `scc_set`: the SCC
        // outlives this call, so we never need to allocate `String`s during
        // the DFS itself. The final result has to be `Vec<String>` because
        // it leaves the function, so we materialise once at the end.
        let start_ref: &str = start.as_str();
        let mut path: Vec<&str> = vec![start_ref];
        let mut seen: HashSet<&str> = HashSet::from([start_ref]);
        if dfs_cycle_path(start_ref, start_ref, &scc_set, adj, &mut path, &mut seen) {
            return Some(path.into_iter().map(str::to_string).collect());
        }
    }
    None
}

fn dfs_cycle_path<'a>(
    current: &'a str,
    start: &'a str,
    scc_set: &HashSet<&'a str>,
    adj: &'a HashMap<String, HashSet<String>>,
    path: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) -> bool {
    let Some(neighbors) = adj.get(current) else {
        return false;
    };
    let mut neighbors: Vec<&'a str> = neighbors
        .iter()
        .filter_map(|n| scc_set.get(n.as_str()).copied())
        .collect();
    neighbors.sort_unstable();

    for neighbor in neighbors {
        if neighbor == start && path.len() > 1 {
            path.push(start);
            return true;
        }
        if !seen.insert(neighbor) {
            continue;
        }
        path.push(neighbor);
        if dfs_cycle_path(neighbor, start, scc_set, adj, path, seen) {
            return true;
        }
        path.pop();
        seen.remove(neighbor);
    }
    false
}
