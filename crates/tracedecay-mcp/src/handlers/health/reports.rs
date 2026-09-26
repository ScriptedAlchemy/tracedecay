//! `tracedecay_gini`, `tracedecay_dependency_depth`, and `tracedecay_health`.

use super::*;
use tracedecay_domain::RelationEdgeKindV1;

const MAX_GINI_SYMBOLS: usize = 500_000;
const MAX_GINI_RELATIONS: usize = 2_000_000;

#[hotpath::measure(label = "mcp.health.gini.total")]
pub async fn compute_gini(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: GiniSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_gini")?;
    let metric = request.metric.unwrap_or(GiniMetricV1::Complexity);
    let scope = request.scope.unwrap_or(GiniScopeV1::File);
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let (named_values, incomplete_complexity_symbols) = hotpath::measure_block!(
        "mcp.health.gini.graph",
        verified_gini_values(graph, metric, scope, path_prefix)?
    );

    let (gini, interpretation, total_items, outliers) =
        hotpath::measure_block!("mcp.health.gini.compute", {
            let values: Vec<f64> = named_values.iter().map(|(_, v)| *v).collect();
            let gini = gini_coefficient(&values);
            let interpretation = gini_label(gini);

            let total_items = named_values.len();
            let mut sorted = named_values;
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            sorted.truncate(limit);

            let max_val = sorted.first().map_or(0.0, |(_, v)| *v);
            let outliers: Vec<GiniOutlierV1> = sorted
                .into_iter()
                .map(|(name, value)| {
                    let pct_of_max = if max_val > 0.0 {
                        (value / max_val * 100.0).round()
                    } else {
                        0.0
                    };
                    GiniOutlierV1 {
                        name,
                        value,
                        pct_of_max,
                    }
                })
                .collect();
            (gini, interpretation, total_items, outliers)
        });

    Ok(graph_tool_completion(
        GraphToolResultV1::Gini(GiniResultV1 {
            gini: (gini * 10000.0).round() / 10000.0,
            interpretation: interpretation.to_owned(),
            total_items: total_items as u64,
            metric,
            scope,
            incomplete_complexity_symbols: incomplete_complexity_symbols as u64,
            outliers,
        }),
        Vec::new(),
    ))
}

/// Named metric values plus, for complexity metrics, the number of symbols
/// left out because their bounded complexity walk did not cover the body:
/// their counters are lower bounds, so they measure nothing here.
fn verified_gini_values(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    metric: GiniMetricV1,
    scope: GiniScopeV1,
    path_prefix: Option<&str>,
) -> Result<(Vec<(String, f64)>, usize)> {
    let page = graph.symbols_page(None, MAX_GINI_SYMBOLS)?;
    if page.has_more {
        return Err(TraceDecayError::project_route(
            "code-graph-budget-exhausted",
            false,
            "verified Gini symbol census exceeded its analytical budget",
        ));
    }
    let mut symbols = Vec::with_capacity(page.symbols.len());
    for symbol in page.symbols {
        let binding = symbol.binding.as_ref().ok_or_else(|| {
            TraceDecayError::project_route(
                "code-graph-corrupt",
                false,
                "verified Gini symbol is missing its file binding",
            )
        })?;
        let path = binding.logical_path.as_ref().ok_or_else(|| {
            TraceDecayError::project_route(
                "code-graph-corrupt",
                false,
                "verified Gini symbol is missing its logical file path",
            )
        })?;
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            TraceDecayError::project_route(
                "code-graph-corrupt",
                false,
                "verified Gini symbol is missing lineage metadata",
            )
        })?;
        if tracedecay_domain::path_matches_scope(path, path_prefix) {
            symbols.push((symbol.occurrence, path.clone(), metadata.clone()));
        }
    }

    match (metric, scope) {
        (GiniMetricV1::FanIn | GiniMetricV1::FanOut, GiniScopeV1::File) => {
            verified_gini_fan_values(graph, &symbols, metric == GiniMetricV1::FanIn)
                .map(|values| (values, 0))
        }
        (GiniMetricV1::Lines, GiniScopeV1::File) => {
            let mut per_file = HashMap::<String, f64>::new();
            for (_, path, metadata) in symbols {
                *per_file.entry(path).or_default() += f64::from(metadata.line_span);
            }
            Ok((per_file.into_iter().collect(), 0))
        }
        (GiniMetricV1::Members, _) => {
            verified_gini_member_values(graph, &symbols).map(|values| (values, 0))
        }
        (_, GiniScopeV1::Symbol) => {
            let mut incomplete = 0usize;
            let values = symbols
                .into_iter()
                .filter(|(_, _, metadata)| matches!(metadata.kind.as_str(), "function" | "method"))
                .filter_map(|(_, path, metadata)| {
                    let Some(complexity) = metadata.exact_complexity() else {
                        incomplete += 1;
                        return None;
                    };
                    let value = complexity
                        .branches
                        .saturating_add(complexity.loops)
                        .saturating_add(complexity.max_nesting);
                    Some((format!("{path}:{}", metadata.simple_name), f64::from(value)))
                })
                .collect();
            Ok((values, incomplete))
        }
        _ => {
            let mut incomplete = 0usize;
            let mut per_file = HashMap::<String, f64>::new();
            for (_, path, metadata) in symbols {
                let Some(complexity) = metadata.exact_complexity() else {
                    incomplete += 1;
                    per_file.entry(path).or_default();
                    continue;
                };
                let value = complexity
                    .branches
                    .saturating_add(complexity.loops)
                    .saturating_add(complexity.max_nesting);
                *per_file.entry(path).or_default() += f64::from(value);
            }
            Ok((per_file.into_iter().collect(), incomplete))
        }
    }
}

fn verified_gini_fan_values(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    symbols: &[(
        tracedecay_domain::SymbolOccurrenceId,
        String,
        tracedecay_code_index::lineage::LineageSymbolRecordV1,
    )],
    fan_in: bool,
) -> Result<Vec<(String, f64)>> {
    let paths = symbols
        .iter()
        .map(|(occurrence, path, _)| (occurrence.clone(), path.clone()))
        .collect::<HashMap<_, _>>();
    let occurrences = symbols
        .iter()
        .map(|(occurrence, _, _)| occurrence.clone())
        .collect::<Vec<_>>();
    let edges = graph.edges_among(&occurrences, &[], MAX_GINI_RELATIONS)?;
    let mut per_file = symbols
        .iter()
        .map(|(_, path, _)| (path.clone(), 0.0))
        .collect::<HashMap<_, _>>();
    for edge in edges {
        let (Some(source), Some(target)) = (
            paths.get(&edge.from_occurrence),
            paths.get(&edge.to_occurrence),
        ) else {
            return Err(TraceDecayError::project_route(
                "code-graph-corrupt",
                false,
                "verified Gini relation endpoint is missing from its symbol census",
            ));
        };
        if source != target {
            let key = if fan_in { target } else { source };
            *per_file.entry(key.clone()).or_default() += 1.0;
        }
    }
    Ok(per_file.into_iter().collect())
}

fn verified_gini_member_values(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    symbols: &[(
        tracedecay_domain::SymbolOccurrenceId,
        String,
        tracedecay_code_index::lineage::LineageSymbolRecordV1,
    )],
) -> Result<Vec<(String, f64)>> {
    let containers = symbols
        .iter()
        .filter(|(_, _, metadata)| matches!(metadata.kind.as_str(), "class" | "struct"))
        .map(|(occurrence, _, metadata)| (occurrence.clone(), (metadata.simple_name.clone(), 0.0)))
        .collect::<HashMap<_, _>>();
    if containers.is_empty() {
        return Ok(Vec::new());
    }
    let occurrences = symbols
        .iter()
        .map(|(occurrence, _, _)| occurrence.clone())
        .collect::<Vec<_>>();
    let edges = graph.edges_among(
        &occurrences,
        &[RelationEdgeKindV1::Contains],
        MAX_GINI_RELATIONS,
    )?;
    let mut members = containers;
    for edge in edges {
        if let Some((_, count)) = members.get_mut(&edge.from_occurrence) {
            *count += 1.0;
        }
    }
    Ok(members.into_values().collect())
}

#[hotpath::measure(label = "mcp.health.dependency_depth.total")]
pub async fn compute_dependency_depth(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: DependencyDepthSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_dependency_depth")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let adj = hotpath::future!(
        graph.build_file_adjacency(path_prefix),
        label = "mcp.health.dependency_depth.graph"
    )
    .await?;

    let result = hotpath::measure_block!("mcp.health.dependency_depth.compute", {
        let result = dependency_depth(&adj, limit);
        let score = depth_score(result.max_depth, result.ideal_depth);
        DependencyDepthResultV1 {
            max_depth: result.max_depth as u64,
            ideal_depth: result.ideal_depth as u64,
            depth_score: (score * 10000.0).round() / 10000.0,
            chains: result
                .chains
                .into_iter()
                .map(|chain| DependencyDepthChainV1 {
                    file: chain.file,
                    depth: chain.depth as u64,
                    chain: chain.chain,
                })
                .collect(),
        }
    });

    Ok(graph_tool_completion(
        GraphToolResultV1::DependencyDepth(result),
        Vec::new(),
    ))
}

#[hotpath::measure(label = "mcp.health.health.total")]
pub async fn compute_health(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: HealthSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_health")?;
    let path_prefix = request.path.as_deref().or(scope_prefix);

    let snap = hotpath::future!(
        graph.verified_health_snapshot(path_prefix),
        label = "mcp.health.health.graph"
    )
    .await?;

    let (dimensions, weights) = if request.details.unwrap_or(false) {
        let r4 = |x: f64| (x * 10000.0).round() / 10000.0;
        (
            Some(HealthDimensionsV1 {
                acyclicity: HealthAcyclicityV1 {
                    score: r4(snap.acyclicity),
                    edges_in_cycles: snap.edges_in_cycles as u64,
                    source: "1 - edges_in_nontrivial_SCCs / total_edges".to_owned(),
                },
                depth: HealthDepthV1 {
                    score: r4(snap.depth),
                    max_chain: snap.max_chain as u64,
                    ideal_chain: snap.ideal_chain as u64,
                    source: "min(1, ideal_chain / max_chain), ideal = ceil(log2(file_count))"
                        .to_owned(),
                },
                equality: HealthEqualityV1 {
                    score: r4(snap.equality),
                    gini: r4(snap.gini),
                    interpretation: gini_label(snap.gini).to_owned(),
                    incomplete_complexity_symbols: snap.incomplete_complexity_symbols as u64,
                    source: "1 - gini(per_file_complexity); symbols whose complexity walk hit its budget are excluded and counted".to_owned(),
                },
                redundancy: HealthRedundancyV1 {
                    score: r4(snap.redundancy),
                    dead_count: snap.dead_count as u64,
                    total_fns: snap.total_fns as u64,
                    source: "1 - dead_fns / total_fns".to_owned(),
                },
                modularity: HealthModularityV1 {
                    score: r4(snap.modularity),
                    interpretation: modularity_label(snap.modularity).to_owned(),
                    components_after_hub_removal: snap.modularity_components as u64,
                    source: "1 - 1/components_after_hub_removal".to_owned(),
                },
                coverage_discipline: HealthCoverageDisciplineV1 {
                    score: r4(snap.coverage_discipline),
                    skip_test_coverage_count: snap.skip_coverage_count as u64,
                    total_fns: snap.total_fns as u64,
                    source: "1 - skip_test_coverage_annotations / total_fns".to_owned(),
                },
            }),
            Some(HealthWeightsV1 {
                note: "quality_signal is geometric mean × 10000".to_owned(),
            }),
        )
    } else {
        (None, None)
    };

    Ok(graph_tool_completion(
        GraphToolResultV1::Health(HealthResultV1 {
            quality_signal: snap.quality_signal,
            files_analyzed: snap.files_analyzed as u64,
            dimensions,
            weights,
        }),
        Vec::new(),
    ))
}
