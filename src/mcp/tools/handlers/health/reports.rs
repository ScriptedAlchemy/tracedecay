//! `tracedecay_gini`, `tracedecay_dependency_depth`, and `tracedecay_health`.

use super::*;

/// Handles `tracedecay_gini` tool calls.
pub(crate) async fn handle_gini(
    cg: &TraceDecay,
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

    let all_nodes = cg.get_all_nodes().await?;
    let all_edges = if metric == "fan_in" || metric == "fan_out" {
        cg.get_all_edges().await?
    } else {
        vec![]
    };

    // Apply path filter
    let nodes: Vec<_> = all_nodes
        .into_iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix))
        .collect();

    let named_values: Vec<(String, f64)> = match (metric, scope) {
        ("complexity", "file") => {
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += c;
            }
            per_file.into_iter().collect()
        }
        ("lines", "file") => {
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let lines = f64::from(n.end_line.saturating_sub(n.start_line) + 1);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += lines;
            }
            per_file.into_iter().collect()
        }
        ("fan_in", "file") => {
            let node_to_file: HashMap<String, String> = nodes
                .iter()
                .map(|n| (n.id.clone(), n.file_path.clone()))
                .collect();
            let mut per_file: HashMap<String, f64> = HashMap::new();
            // Initialize all files
            for n in &nodes {
                per_file.entry(n.file_path.clone()).or_insert(0.0);
            }
            for e in &all_edges {
                if let (Some(src_file), Some(tgt_file)) =
                    (node_to_file.get(&e.source), node_to_file.get(&e.target))
                    && src_file != tgt_file
                {
                    *per_file.entry(tgt_file.clone()).or_insert(0.0) += 1.0;
                }
            }
            per_file.into_iter().collect()
        }
        ("fan_out", "file") => {
            let node_to_file: HashMap<String, String> = nodes
                .iter()
                .map(|n| (n.id.clone(), n.file_path.clone()))
                .collect();
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                per_file.entry(n.file_path.clone()).or_insert(0.0);
            }
            for e in &all_edges {
                if let (Some(src_file), Some(tgt_file)) =
                    (node_to_file.get(&e.source), node_to_file.get(&e.target))
                    && src_file != tgt_file
                {
                    *per_file.entry(src_file.clone()).or_insert(0.0) += 1.0;
                }
            }
            per_file.into_iter().collect()
        }
        ("members", _) => {
            // Count members of each Class/Struct via parent_id (v9+).
            let class_nodes: HashSet<String> = nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct))
                .map(|n| n.id.clone())
                .collect();
            let mut per_class: HashMap<String, (String, f64)> = nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct))
                .map(|n| (n.id.clone(), (n.name.clone(), 0.0)))
                .collect();
            for n in &nodes {
                if let Some(parent) = n.parent_id.as_deref()
                    && class_nodes.contains(parent)
                    && let Some(entry) = per_class.get_mut(parent)
                {
                    entry.1 += 1.0;
                }
            }
            per_class.into_values().collect()
        }
        (_, "symbol") => {
            // Per-function/method complexity
            nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
                .map(|n| {
                    let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                    (format!("{}:{}", n.file_path, n.name), c)
                })
                .collect()
        }
        _ => {
            // Default: file-level complexity
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += c;
            }
            per_file.into_iter().collect()
        }
    };

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

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_dependency_depth` tool calls.
pub(crate) async fn handle_dependency_depth(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;

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

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_health` tool calls.
pub(crate) async fn handle_health(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let details = args
        .get("details")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let snap = compute_health_snapshot(cg, path_prefix).await?;

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

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}
