//! `tracedecay_port_order` — dependency-first porting order (Kahn levels) with SCC cycle reporting.

use super::*;
use tracedecay_application::retrieval::{
    PortCycleAnchorV1, PortCycleFileV1, PortCycleSymbolV1, PortCycleV1, PortOrderLevelV1,
    PortOrderResultV1, PortOrderSurfaceRequestV1, PortOrderSymbolV1,
};
use tracedecay_domain::RelationEdgeKindV1;

use super::super::support::decode_primitive_request;

#[derive(Clone, Copy)]
struct PortOrderSymbol<'a> {
    id: &'a str,
    name: &'a str,
    kind: &'a str,
    file: &'a str,
    start_line: u32,
}

/// Handles `tracedecay_port_order` tool calls.
pub(crate) async fn handle_port_order(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: PortOrderSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_port_order")?;
    let kind_strs = request.kinds.as_ref().map_or_else(
        || {
            PORT_DEFAULT_KINDS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        },
        Clone::clone,
    );

    let limit = request.limit.map_or(50, |value| value.min(500) as usize);

    let kinds: Vec<NodeKind> = kind_strs
        .iter()
        .filter_map(|s| NodeKind::from_str(s))
        .collect();

    if kinds.is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: kinds must contain at least one supported node kind"
                .to_owned(),
        });
    }

    let summaries = symbols_in_dir(graph, &request.source_dir, &kinds)?;
    let nodes = summaries
        .iter()
        .map(|symbol| {
            let (metadata, file) = required_symbol_parts(symbol)?;
            Ok(PortOrderSymbol {
                id: symbol.occurrence.as_str(),
                name: &metadata.simple_name,
                kind: &metadata.kind,
                file,
                start_line: metadata.start_line.saturating_add(1),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let total_symbols = nodes.len();

    if nodes.is_empty() {
        let result = PortOrderResultV1 {
            source_dir: request.source_dir,
            total_symbols: 0,
            returned: 0,
            levels: Vec::new(),
            cycles: Vec::new(),
        };
        let output = serde_json::to_value(result)?;
        return Ok(generic_tool_result(
            Some(cg.project_root()),
            &args,
            &output,
            vec![],
        ));
    }

    let node_ids: Vec<&str> = nodes.iter().map(|node| node.id).collect();
    let node_map: HashMap<&str, &PortOrderSymbol<'_>> =
        nodes.iter().map(|node| (node.id, node)).collect();
    let id_set: HashSet<&str> = node_ids.iter().copied().collect();

    let occurrences = summaries
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let edges = graph.edges_among(
        &occurrences,
        &[
            RelationEdgeKindV1::Calls,
            RelationEdgeKindV1::Uses,
            RelationEdgeKindV1::Extends,
            RelationEdgeKindV1::Implements,
        ],
        INFO_RELATION_LIMIT,
    )?;

    // Build adjacency list and in-degree map for Kahn's algorithm.
    // Edge direction: source depends on target (source calls/uses target),
    // so in the dependency graph, source -> target means "source needs target".
    // For topological sort, we want nodes with in_degree 0 (nothing depends on
    // them internally, OR they have no dependencies). Actually, for porting
    // order we want leaves first = nodes that DON'T depend on other internal
    // nodes. So in-degree in the dependency DAG = number of things this node
    // depends on = outgoing edges in the call/uses graph.
    //
    // Reframe: dependency_graph[A] = {B, C} means A depends on B and C.
    // in_degree[A] = number of nodes A depends on.
    // Kahn's starts with in_degree 0 = nodes with no dependencies = safe to port first.
    let mut dep_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    // Initialize all nodes
    for id in &node_ids {
        dep_graph.entry(*id).or_default();
        in_degree.entry(*id).or_insert(0);
    }

    // reverse_dep_graph[B] = list of nodes that depend on B.
    // When B is sorted, we decrement in_degree for each of its reverse deps.
    let mut reverse_dep_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &node_ids {
        reverse_dep_graph.entry(*id).or_default();
    }

    for edge in &edges {
        let source = edge.edge.from_occurrence.as_str();
        let target = edge.edge.to_occurrence.as_str();
        if !id_set.contains(source) || !id_set.contains(target) {
            continue;
        }
        // Self-edges are common resolver artifacts for methods with generic
        // names (`push`, `new`, `clamp`, `num_rows`) where a call on another
        // receiver fuzzy-binds back to the current method. They also make a
        // single symbol unsortable in Kahn's algorithm, producing noisy
        // singleton cycles instead of useful porting order. Mutual cycles are
        // still reported below.
        if source == target {
            continue;
        }
        // source depends on target: add dependency source -> target
        dep_graph.entry(source).or_default().push(target);
        // reverse: target is depended on by source
        reverse_dep_graph.entry(target).or_default().push(source);
        *in_degree.entry(source).or_insert(0) += 1;
    }

    // Kahn's algorithm (BFS topological sort)
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    for (&id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id);
        }
    }

    let mut levels: Vec<Vec<&str>> = Vec::new();
    let mut sorted_set: HashSet<&str> = HashSet::new();
    let mut emitted = 0usize;

    while !queue.is_empty() && emitted < limit {
        let mut current_level: Vec<&str> = Vec::new();
        let level_size = queue.len();
        for _ in 0..level_size {
            // Safety: we checked queue is non-empty above and iterate exactly level_size times
            let Some(id) = queue.pop_front() else { break };
            if sorted_set.contains(id) {
                continue;
            }
            sorted_set.insert(id);
            current_level.push(id);
            emitted += 1;
            if emitted >= limit {
                break;
            }
        }

        // For each sorted node, decrement in-degree of nodes that depend on it.
        for &sorted_id in &current_level {
            if let Some(dependents) = reverse_dep_graph.get(sorted_id) {
                for &dep_id in dependents {
                    if sorted_set.contains(dep_id) {
                        continue;
                    }
                    let deg = in_degree.entry(dep_id).or_insert(0);
                    if *deg > 0 {
                        *deg -= 1;
                    }
                    if *deg == 0 {
                        queue.push_back(dep_id);
                    }
                }
            }
        }

        if !current_level.is_empty() {
            levels.push(current_level);
        }
    }

    // Detect cycles: any unsorted nodes form cycles.
    let cycle_node_ids: HashSet<&str> = node_ids
        .iter()
        .copied()
        .filter(|id| !sorted_set.contains(id))
        .collect();

    // Group cycles into SCCs so multiple disjoint mutually-recursive
    // groups don't collapse into one mega-cycle. Each non-trivial SCC
    // becomes its own entry with the files forming it surfaced — gives
    // the user a clear "break this cycle" target instead of a 200+
    // symbol blob.
    let mut cycle_adj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (&node_id, neighbors) in &dep_graph {
        if !cycle_node_ids.contains(node_id) {
            continue;
        }
        let kept: HashSet<&str> = neighbors
            .iter()
            .copied()
            .filter(|n| cycle_node_ids.contains(n))
            .collect();
        cycle_adj.insert(node_id, kept);
    }
    let sccs = crate::graph::scc::tarjan_scc(&cycle_adj);

    let mut cycles = Vec::<PortCycleV1>::new();
    for scc in sccs {
        if !crate::graph::scc::is_cyclic_scc(&scc, &cycle_adj) {
            continue;
        }
        let scc_set: HashSet<&str> = scc.iter().copied().collect();
        // Rank symbols within the SCC by in-cycle out-degree (how many
        // *other* SCC members this symbol depends on). The symbol with the
        // smallest out-degree is the leaf-most node inside the cycle and is
        // the natural starting point: porting it requires stubbing the
        // fewest peers. The symbol with the largest out-degree is the
        // "hub" — the best candidate to break the cycle by refactoring its
        // call sites.
        let mut ranked: Vec<(&str, usize, usize)> = scc
            .iter()
            .map(|id| {
                let out_in_cycle = cycle_adj.get(id).map_or(0, |neighbors| {
                    neighbors.iter().filter(|n| scc_set.contains(*n)).count()
                });
                // In-degree (within the cycle) — how many SCC members
                // depend on this symbol. High in-degree = "many callers
                // inside the cycle", which is another useful break-point
                // signal.
                let mut in_in_cycle = 0;
                for (&src, neighbors) in &cycle_adj {
                    if !scc_set.contains(src) || src == *id {
                        continue;
                    }
                    if neighbors.contains(id) {
                        in_in_cycle += 1;
                    }
                }
                (*id, out_in_cycle, in_in_cycle)
            })
            .collect();
        // Ascending by out-degree → entry-point first; ties broken by
        // descending in-degree (hub-iness) so the most-referenced "leaf"
        // surfaces just after the cleanest leaf.
        ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.cmp(&a.2)));

        let symbols = ranked
            .iter()
            .filter_map(|(id, out_deg, in_deg)| {
                let node = node_map.get(id)?;
                Some(PortCycleSymbolV1 {
                    name: node.name.to_owned(),
                    kind: node.kind.to_owned(),
                    file: node.file.to_owned(),
                    line: node.start_line,
                    in_cycle_out_degree: *out_deg,
                    in_cycle_in_degree: *in_deg,
                })
            })
            .collect();

        // Rank files by how many cycle members each contains — the file
        // with the most members is the best refactor target.
        let mut file_counts: HashMap<&str, usize> = HashMap::new();
        for id in &scc {
            if let Some(n) = node_map.get(id) {
                *file_counts.entry(n.file).or_insert(0) += 1;
            }
        }
        let mut files_ranked: Vec<(&str, usize)> = file_counts.into_iter().collect();
        files_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let files = files_ranked
            .iter()
            .map(|(path, count)| PortCycleFileV1 {
                file: (*path).to_owned(),
                members_in_cycle: *count,
            })
            .collect();

        let entry_point = ranked.first().and_then(|(id, _, _)| node_map.get(id));
        let hub = ranked
            .iter()
            .max_by_key(|(_, _out, in_deg)| *in_deg)
            .and_then(|(id, _, _)| node_map.get(id));

        cycles.push(PortCycleV1 {
            size: scc.len(),
            files,
            symbols,
            entry_point: entry_point.map(|node| PortCycleAnchorV1 {
                name: node.name.to_owned(),
                file: node.file.to_owned(),
                line: node.start_line,
                rationale: None,
            }),
            break_point_candidate: hub.map(|node| PortCycleAnchorV1 {
                name: node.name.to_owned(),
                file: node.file.to_owned(),
                line: node.start_line,
                rationale: Some(
                    "Highest in-cycle in-degree — refactoring its callers is the most effective way to fragment this SCC."
                        .to_owned(),
                ),
            }),
            note: "Mutual dependency — port together, starting at `entry_point` and refactoring `break_point_candidate` to split the cycle."
                .to_owned(),
        });
    }

    let result_levels = levels
        .iter()
        .enumerate()
        .map(|(i, level_ids)| {
            let description = if i == 0 {
                "No internal dependencies — port these first".to_string()
            } else {
                format!("Depends only on levels 0–{}", i - 1)
            };

            let symbols = level_ids
                .iter()
                .filter_map(|id| {
                    let node = node_map.get(id)?;
                    // Find what this node depends on (for depends_on field)
                    let depends_on = dep_graph
                        .get(id)
                        .map(|d| {
                            d.iter()
                                .filter_map(|dep_id| {
                                    node_map.get(dep_id).map(|node| node.name.to_owned())
                                })
                                .collect::<Vec<_>>()
                        })
                        .filter(|dependencies| !dependencies.is_empty());

                    Some(PortOrderSymbolV1 {
                        name: node.name.to_owned(),
                        kind: node.kind.to_owned(),
                        file: node.file.to_owned(),
                        line: node.start_line,
                        depends_on,
                    })
                })
                .collect();

            PortOrderLevelV1 {
                level: i,
                description,
                symbols,
            }
        })
        .collect();

    let touched_files = unique_file_paths(nodes.iter().map(|node| node.file));

    let result = PortOrderResultV1 {
        source_dir: request.source_dir,
        total_symbols,
        returned: emitted,
        levels: result_levels,
        cycles,
    };
    let output = serde_json::to_value(result)?;

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}
