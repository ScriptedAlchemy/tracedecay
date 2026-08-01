//! The scoped health snapshot — the one value every health surface is built on.
//!
//! This lives below the MCP handler tree so the tool handlers and the root
//! engine's downward ports can both read it without either depending on the
//! other. The computation is byte-for-byte the pre-move handler code.

use std::collections::{HashMap, HashSet};

use crate::errors::Result;
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

use super::{
    HealthDimensions, acyclicity_score, compute_composite_health, dependency_depth, depth_score,
    gini_coefficient, modularity_score,
};

pub(crate) struct HealthSnapshot {
    pub(crate) quality_signal: u32,
    pub(crate) files_analyzed: usize,
    pub(crate) acyclicity: f64,
    pub(crate) depth: f64,
    pub(crate) equality: f64,
    pub(crate) redundancy: f64,
    pub(crate) modularity: f64,
    pub(crate) coverage_discipline: f64,
    /// Raw signals retained for `details=true` (#82).
    pub(crate) gini: f64,
    pub(crate) edges_in_cycles: usize,
    pub(crate) total_edges: usize,
    pub(crate) max_chain: usize,
    pub(crate) ideal_chain: usize,
    pub(crate) complexity_files: usize,
    pub(crate) modularity_components: usize,
    pub(crate) dead_count: usize,
    pub(crate) total_fns: usize,
    pub(crate) skip_coverage_count: usize,
}

/// The six named dimensions of a snapshot, in their canonical report order.
pub(crate) fn session_dimension_values(snap: &HealthSnapshot) -> [(&'static str, f64); 6] {
    [
        ("acyclicity", snap.acyclicity),
        ("depth", snap.depth),
        ("equality", snap.equality),
        ("redundancy", snap.redundancy),
        ("modularity", snap.modularity),
        ("coverage_discipline", snap.coverage_discipline),
    ]
}

/// Computes all 5 health dimensions and the composite signal for a given scope.
pub(crate) async fn compute_health_snapshot(
    cg: &TraceDecay,
    path_prefix: Option<&str>,
) -> Result<HealthSnapshot> {
    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;
    let files_analyzed = adj.len();
    let total_edges = adj.values().map(HashSet::len).sum();

    let (acyclicity, edges_in_cycles) = acyclicity_score(&adj);
    let depth_result = dependency_depth(&adj, 1);
    let depth = depth_score(depth_result.max_depth, depth_result.ideal_depth);

    let all_nodes = cg.get_all_nodes().await?;
    let nodes: Vec<_> = all_nodes
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix))
        .collect();

    let mut per_file_complexity: HashMap<String, f64> = HashMap::new();
    for n in &nodes {
        let c = f64::from(n.branches) * 2.0
            + f64::from(n.loops) * 2.0
            + f64::from(n.max_nesting) * 3.0
            + f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *per_file_complexity
            .entry(n.file_path.clone())
            .or_insert(0.0) += c;
    }
    let complexity_values: Vec<f64> = per_file_complexity.values().copied().collect();
    let complexity_files = complexity_values.len();
    let gini = gini_coefficient(&complexity_values);
    let equality = (1.0 - gini).clamp(0.0, 1.0);

    let dead = cg
        .find_dead_code(&[NodeKind::Function, NodeKind::Method], false)
        .await?;
    let dead_in_scope = dead
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix));
    let dead_count = dead_in_scope.count();
    let total_fns = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .count();
    let redundancy = if total_fns == 0 {
        1.0
    } else {
        (1.0 - dead_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let (modularity, modularity_components) = modularity_score(&adj);

    // coverage_discipline: penalise overuse of skip-test-coverage annotations.
    let skip_coverage = cg.get_skip_test_coverage_node_ids().await?;
    let skipped_in_scope = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method) && skip_coverage.contains(&n.id)
        })
        .count();
    let coverage_discipline = if total_fns == 0 {
        1.0
    } else {
        (1.0 - skipped_in_scope as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let dims = HealthDimensions {
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
    };
    let quality_signal = compute_composite_health(&dims);

    Ok(HealthSnapshot {
        quality_signal,
        files_analyzed,
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
        gini,
        edges_in_cycles,
        total_edges,
        max_chain: depth_result.max_depth,
        ideal_chain: depth_result.ideal_depth,
        complexity_files,
        modularity_components,
        dead_count,
        total_fns,
        skip_coverage_count: skipped_in_scope,
    })
}
