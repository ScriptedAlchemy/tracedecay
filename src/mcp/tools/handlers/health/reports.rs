//! `tracedecay_gini`, `tracedecay_dependency_depth`, and `tracedecay_health`.

use super::*;
use tracedecay_domain::RelationEdgeKindV1;

const MAX_GINI_SYMBOLS: usize = 500_000;
const MAX_GINI_RELATIONS: usize = 2_000_000;

/// Handles `tracedecay_gini` tool calls.
pub(crate) async fn handle_gini(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let metric = args
        .get("metric")
        .and_then(|v| v.as_str())
        .unwrap_or("complexity");
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("file");
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    let named_values = verified_gini_values(graph, metric, scope, path_prefix)?;

    let values: Vec<f64> = named_values.iter().map(|(_, v)| *v).collect();
    let gini = gini_coefficient(&values);
    let interpretation = gini_label(gini);

    let total_items = named_values.len();
    let mut sorted = named_values;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(limit);

    let max_val = sorted.first().map_or(0.0, |(_, v)| *v);
    let outliers: Vec<Value> = sorted
        .iter()
        .map(|(name, val)| {
            let pct = if max_val > 0.0 {
                (val / max_val * 100.0).round()
            } else {
                0.0
            };
            json!({
                "name": name,
                "value": val,
                "pct_of_max": pct,
            })
        })
        .collect();

    let output = json!({
        "gini": (gini * 10000.0).round() / 10000.0,
        "interpretation": interpretation,
        "total_items": total_items,
        "metric": metric,
        "scope": scope,
        "outliers": outliers,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}

fn verified_gini_values(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    metric: &str,
    scope: &str,
    path_prefix: Option<&str>,
) -> Result<Vec<(String, f64)>> {
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
        if crate::path_scope::path_matches_scope(path, path_prefix) {
            symbols.push((symbol.occurrence, path.clone(), metadata.clone()));
        }
    }

    match (metric, scope) {
        ("fan_in" | "fan_out", "file") => {
            verified_gini_fan_values(graph, &symbols, metric == "fan_in")
        }
        ("lines", "file") => {
            let mut per_file = HashMap::<String, f64>::new();
            for (_, path, metadata) in symbols {
                *per_file.entry(path).or_default() += f64::from(metadata.line_span);
            }
            Ok(per_file.into_iter().collect())
        }
        ("members", _) => verified_gini_member_values(graph, &symbols),
        (_, "symbol") => Ok(symbols
            .into_iter()
            .filter(|(_, _, metadata)| matches!(metadata.kind.as_str(), "function" | "method"))
            .map(|(_, path, metadata)| {
                let value = metadata
                    .branches
                    .saturating_add(metadata.loops)
                    .saturating_add(metadata.max_nesting);
                (format!("{path}:{}", metadata.simple_name), f64::from(value))
            })
            .collect()),
        _ => {
            let mut per_file = HashMap::<String, f64>::new();
            for (_, path, metadata) in symbols {
                let value = metadata
                    .branches
                    .saturating_add(metadata.loops)
                    .saturating_add(metadata.max_nesting);
                *per_file.entry(path).or_default() += f64::from(value);
            }
            Ok(per_file.into_iter().collect())
        }
    }
}

fn verified_gini_fan_values(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
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
            paths.get(&edge.edge.from_occurrence),
            paths.get(&edge.edge.to_occurrence),
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
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
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
        if let Some((_, count)) = members.get_mut(&edge.edge.from_occurrence) {
            *count += 1.0;
        }
    }
    Ok(members.into_values().collect())
}

/// Handles `tracedecay_dependency_depth` tool calls.
pub(crate) async fn handle_dependency_depth(
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

    let adj = graph.build_file_adjacency(path_prefix).await?;

    let result = dependency_depth(&adj, limit);
    let score = depth_score(result.max_depth, result.ideal_depth);

    let chains: Vec<Value> = result
        .chains
        .iter()
        .map(|ch| {
            json!({
                "file": ch.file,
                "depth": ch.depth,
                "chain": ch.chain,
            })
        })
        .collect();

    let output = json!({
        "max_depth": result.max_depth,
        "ideal_depth": result.ideal_depth,
        "depth_score": (score * 10000.0).round() / 10000.0,
        "chains": chains,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}

/// Handles `tracedecay_health` tool calls.
pub(crate) async fn handle_health(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let details = args
        .get("details")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let snap = tracedecay_usecases::graph::health::compute_verified_health_snapshot(
        &graph.manager(),
        path_prefix,
    )
    .await?;

    let output = if details {
        let r4 = |x: f64| (x * 10000.0).round() / 10000.0;
        json!({
            "quality_signal": snap.quality_signal,
            "files_analyzed": snap.files_analyzed,
            "dimensions": {
                "acyclicity": {
                    "score": r4(snap.acyclicity),
                    "edges_in_cycles": snap.edges_in_cycles,
                    "source": "1 - edges_in_nontrivial_SCCs / total_edges",
                },
                "depth": {
                    "score": r4(snap.depth),
                    "max_chain": snap.max_chain,
                    "ideal_chain": snap.ideal_chain,
                    "source": "min(1, ideal_chain / max_chain), ideal = ceil(log2(file_count))",
                },
                "equality": {
                    "score": r4(snap.equality),
                    "gini": r4(snap.gini),
                    "interpretation": gini_label(snap.gini),
                    "source": "1 - gini(per_file_complexity)",
                },
                "redundancy": {
                    "score": r4(snap.redundancy),
                    "dead_count": snap.dead_count,
                    "total_fns": snap.total_fns,
                    "source": "1 - dead_fns / total_fns",
                },
                "modularity": {
                    "score": r4(snap.modularity),
                    "interpretation": modularity_label(snap.modularity),
                    "components_after_hub_removal": snap.modularity_components,
                    "source": "1 - 1/components_after_hub_removal",
                },
                "coverage_discipline": {
                    "score": r4(snap.coverage_discipline),
                    "skip_test_coverage_count": snap.skip_coverage_count,
                    "total_fns": snap.total_fns,
                    "source": "1 - skip_test_coverage_annotations / total_fns",
                },
            },
            "weights": {
                "note": "quality_signal is geometric mean × 10000",
            },
        })
    } else {
        json!({
            "quality_signal": snap.quality_signal,
            "files_analyzed": snap.files_analyzed,
        })
    };

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}
