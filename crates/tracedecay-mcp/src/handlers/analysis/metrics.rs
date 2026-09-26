//! Ranking and distribution reports: `tracedecay_rank`, `tracedecay_largest`, `tracedecay_coupling`, `tracedecay_inheritance_depth`, `tracedecay_distribution`.

use tracedecay_contracts::retrieval::{
    CouplingDirectionV1, CouplingEntryV1, CouplingResultV1, CouplingSurfaceRequestV1,
    DistributionFileV1, DistributionKindCountV1, DistributionResultV1,
    DistributionSurfaceRequestV1, DistributionViewV1, InheritanceDepthEntryV1,
    InheritanceDepthResultV1, InheritanceDepthSurfaceRequestV1, LargestEntryV1, LargestResultV1,
    LargestSurfaceRequestV1, RankDirectionV1, RankEdgeKindV1, RankEntryV1, RankResultV1,
    RankSurfaceRequestV1,
};

use super::*;

#[hotpath::measure(future = true, label = "mcp.analysis.rank.total")]
pub(super) async fn compute_rank(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: RankSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_rank")?;
    let direction = request.direction.unwrap_or_default();
    let incoming = direction == RankDirectionV1::Incoming;
    let node_kind = request
        .node_kind
        .as_deref()
        .map(|kind| requested_node_kind("tracedecay_rank", kind))
        .transpose()?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let relation_kind = match request.edge_kind {
        RankEdgeKindV1::Contains => RelationEdgeKindV1::Contains,
        RankEdgeKindV1::Calls => RelationEdgeKindV1::Calls,
        RankEdgeKindV1::Uses => RelationEdgeKindV1::Uses,
        RankEdgeKindV1::Implements => RelationEdgeKindV1::Implements,
        RankEdgeKindV1::TypeOf => RelationEdgeKindV1::TypeOf,
        RankEdgeKindV1::Returns => RelationEdgeKindV1::Returns,
        RankEdgeKindV1::Extends => RelationEdgeKindV1::Extends,
        RankEdgeKindV1::Annotates => RelationEdgeKindV1::Annotates,
        RankEdgeKindV1::Receives => RelationEdgeKindV1::Receives,
        RankEdgeKindV1::DerivesMacro => {
            return Err(verified_analysis_unavailable(
                "rank",
                "the admitted graph generation does not publish derives_macro relations",
            ));
        }
    };
    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.rank.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[relation_kind])?;
        (symbols, edges)
    });
    let (symbols, counts) = hotpath::measure_block!("mcp.analysis.rank.compute", {
        let mut counts = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            let occurrence = if incoming {
                edge.to_occurrence
            } else {
                edge.from_occurrence
            };
            *counts.entry(occurrence).or_default() += 1;
        }
        if let Some(kind) = node_kind {
            symbols
                .retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
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
        (symbols, counts)
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let ranking: Vec<RankEntryV1> = symbols
        .into_iter()
        .map(|symbol| RankEntryV1 {
            count: counts.get(&symbol.occurrence).copied().unwrap_or(0),
            id: symbol.occurrence.as_str().to_owned(),
            name: symbol.metadata.simple_name,
            kind: symbol.metadata.kind,
            file: symbol.path,
            line: user_line(symbol.metadata.start_line),
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::Rank(RankResultV1 {
            edge_kind: request.edge_kind,
            direction,
            node_kind_filter: request.node_kind,
            result_count: ranking.len() as u64,
            ranking,
        }),
        touched_files,
    ))
}

#[hotpath::measure(future = true, label = "mcp.analysis.largest.total")]
pub(super) async fn compute_largest(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: LargestSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_largest")?;
    let node_kind = request
        .node_kind
        .as_deref()
        .map(|kind| requested_node_kind("tracedecay_largest", kind))
        .transpose()?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let mut symbols = hotpath::measure_block!(
        "mcp.analysis.largest.graph",
        verified_analysis_symbols(graph, path_prefix)?
    );
    let symbols = hotpath::measure_block!("mcp.analysis.largest.compute", {
        if let Some(kind) = node_kind {
            symbols
                .retain(|symbol| NodeKind::from_str(&symbol.metadata.kind).as_ref() == Some(&kind));
        }
        symbols.sort_by(|left, right| {
            right
                .metadata
                .line_span
                .cmp(&left.metadata.line_span)
                .then_with(|| left.occurrence.cmp(&right.occurrence))
        });
        symbols.truncate(limit);
        symbols
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let ranking: Vec<LargestEntryV1> = symbols
        .into_iter()
        .map(|symbol| LargestEntryV1 {
            start_line: user_line(symbol.metadata.start_line),
            end_line: user_line(symbol.end_line()),
            lines: symbol.metadata.line_span,
            id: symbol.occurrence.as_str().to_owned(),
            name: symbol.metadata.simple_name,
            kind: symbol.metadata.kind,
            file: symbol.path,
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::Largest(LargestResultV1 {
            node_kind_filter: request.node_kind,
            result_count: ranking.len() as u64,
            ranking,
        }),
        touched_files,
    ))
}

#[hotpath::measure(future = true, label = "mcp.analysis.coupling.total")]
pub(super) async fn compute_coupling(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: CouplingSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_coupling")?;
    let direction = request.direction.unwrap_or_default();
    let fan_in = direction == CouplingDirectionV1::FanIn;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let (symbols, edges) = hotpath::measure_block!("mcp.analysis.coupling.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[])?;
        (symbols, edges)
    });
    let results = hotpath::measure_block!("mcp.analysis.coupling.compute", {
        let paths = symbols
            .iter()
            .map(|symbol| (symbol.occurrence.clone(), symbol.path.clone()))
            .collect::<HashMap<_, _>>();
        let mut coupled = HashMap::<String, HashSet<String>>::new();
        for edge in edges {
            let (Some(source), Some(target)) = (
                paths.get(&edge.from_occurrence),
                paths.get(&edge.to_occurrence),
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
        results
    });
    let ranking: Vec<CouplingEntryV1> = results
        .into_iter()
        .map(|(file, coupled_files)| CouplingEntryV1 {
            file,
            coupled_files: coupled_files as u64,
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::Coupling(CouplingResultV1 {
            direction,
            result_count: ranking.len() as u64,
            ranking,
        }),
        Vec::new(),
    ))
}

#[hotpath::measure(future = true, label = "mcp.analysis.inheritance_depth.total")]
pub(super) async fn compute_inheritance_depth(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: InheritanceDepthSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_inheritance_depth")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.inheritance_depth.graph", {
        let symbols = verified_analysis_symbols(graph, path_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[RelationEdgeKindV1::Extends])?;
        (symbols, edges)
    });
    let (symbols, memo) = hotpath::measure_block!("mcp.analysis.inheritance_depth.compute", {
        let mut parents = HashMap::<SymbolOccurrenceId, Vec<SymbolOccurrenceId>>::new();
        let mut hierarchy_symbols = HashSet::new();
        for edge in edges {
            hierarchy_symbols.insert(edge.from_occurrence.clone());
            hierarchy_symbols.insert(edge.to_occurrence.clone());
            parents
                .entry(edge.from_occurrence)
                .or_default()
                .push(edge.to_occurrence);
        }
        symbols.retain(|symbol| hierarchy_symbols.contains(&symbol.occurrence));
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
        (symbols, memo)
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.path.as_str()));
    let ranking: Vec<InheritanceDepthEntryV1> = symbols
        .into_iter()
        .map(|symbol| InheritanceDepthEntryV1 {
            depth: memo.get(&symbol.occurrence).copied().unwrap_or(0),
            id: symbol.occurrence.as_str().to_owned(),
            name: symbol.metadata.simple_name,
            kind: symbol.metadata.kind,
            file: symbol.path,
            line: user_line(symbol.metadata.start_line),
        })
        .collect();
    Ok(graph_tool_completion(
        GraphToolResultV1::InheritanceDepth(InheritanceDepthResultV1 {
            result_count: ranking.len() as u64,
            ranking,
        }),
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

#[hotpath::measure(future = true, label = "mcp.analysis.distribution.total")]
pub(super) async fn compute_distribution(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: DistributionSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_distribution")?;
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let symbols = hotpath::measure_block!(
        "mcp.analysis.distribution.graph",
        verified_analysis_symbols(graph, path_prefix)?
    );
    let view = if request.summary.unwrap_or(false) {
        hotpath::measure_block!("mcp.analysis.distribution.compute", {
            let mut totals = HashMap::<String, u64>::new();
            for symbol in &symbols {
                *totals.entry(symbol.metadata.kind.clone()).or_default() += 1;
            }
            let mut sorted = totals.into_iter().collect::<Vec<_>>();
            sorted.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            let distribution = sorted
                .into_iter()
                .map(|(kind, count)| DistributionKindCountV1 { kind, count })
                .collect::<Vec<_>>();
            DistributionViewV1::Summary {
                total_kinds: distribution.len() as u64,
                distribution,
            }
        })
    } else {
        hotpath::measure_block!("mcp.analysis.distribution.compute", {
            let file_limit = request.limit.map_or(100, |v| v.clamp(1, 1000) as usize);
            let mut counts = HashMap::<String, HashMap<String, u64>>::new();
            for symbol in &symbols {
                *counts
                    .entry(symbol.path.clone())
                    .or_default()
                    .entry(symbol.metadata.kind.clone())
                    .or_default() += 1;
            }
            let total_file_count = counts.len() as u64;
            let mut by_file = counts.into_iter().collect::<Vec<_>>();
            by_file.sort_by(|left, right| {
                let left_count = left.1.values().copied().sum::<u64>();
                let right_count = right.1.values().copied().sum::<u64>();
                right_count
                    .cmp(&left_count)
                    .then_with(|| left.0.cmp(&right.0))
            });
            by_file.truncate(file_limit);
            let files: Vec<DistributionFileV1> = by_file
                .into_iter()
                .map(|(file, counts)| {
                    let mut kinds = counts
                        .into_iter()
                        .map(|(kind, count)| DistributionKindCountV1 { kind, count })
                        .collect::<Vec<_>>();
                    kinds.sort_by(|left, right| left.kind.cmp(&right.kind));
                    DistributionFileV1 { file, kinds }
                })
                .collect();
            let file_count = files.len() as u64;
            DistributionViewV1::PerFile {
                file_count,
                total_file_count,
                omitted_file_count: total_file_count.saturating_sub(file_count),
                files,
            }
        })
    };

    Ok(graph_tool_completion(
        GraphToolResultV1::Distribution(DistributionResultV1 {
            path_filter: path_prefix.map(str::to_owned),
            view,
        }),
        Vec::new(),
    ))
}
